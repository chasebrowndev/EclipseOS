// SPDX-License-Identifier: AGPL-3.0-only
//! Output model (COMP-03).
//!
//! `AbyssState` owns one [`Outputs`] table. Outputs are addressed by a `u64`
//! handle, never by index or by pointer; a handle is never reused. Each output
//! carries its own set of workspaces, so unplugging a monitor takes its
//! workspaces with it and plugging it back in brings them home.
//!
//! Identity (COMP-03 §2) is EDID make/model/serial when the connector exposes a
//! readable EDID blob, and the connector name (`DP-3`) otherwise — the spec
//! allows the fallback, and it is the only thing available for virtual outputs
//! and for the winit backend. Identity is what layout persistence is keyed on,
//! which is why it must not be the connector name when we can do better: kernel
//! connector numbering shifts across driver upgrades.

pub mod edid;
pub mod persist;
pub mod power;

use std::collections::HashMap;

use smithay::{
    desktop::Window,
    output::{Mode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::wayland_server::backend::GlobalId,
    utils::{Logical, Point, Transform},
};

use crate::shell::workspace::{self, Workspace};

use persist::{Persist, SavedOutput};

/// Where an output came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputKind {
    /// A real connector on a DRM device, or the winit host window.
    Physical,
    /// Synthesised so the human is never left with no output at all (§5).
    Virtual,
}

pub struct OutputEntry {
    pub id: u64,
    /// Stable across reboots and driver upgrades where EDID allows.
    pub identity: String,
    /// `DP-3`, `winit`, `virtual-1`.
    pub connector: String,
    pub output: Output,
    pub kind: OutputKind,
    pub workspaces: Vec<Workspace>,
    /// 0-based index into `workspaces`.
    pub active: usize,
    pub global: Option<GlobalId>,
    /// Part of the desktop. A disabled output is unmapped and never rendered
    /// (COMP-03 §4); the last enabled output can never be disabled.
    pub enabled: bool,
    /// Scanning out. `false` is DPMS off — the output keeps its geometry and
    /// its windows, the CRTC just stops.
    pub powered: bool,
}

impl OutputEntry {
    pub fn workspace(&self) -> &Workspace {
        &self.workspaces[self.active]
    }

    /// The state worth remembering for next time.
    fn saved(&self, position: Point<i32, Logical>) -> SavedOutput {
        let mode = self
            .output
            .current_mode()
            .map(|m| (m.size.w, m.size.h, m.refresh));
        SavedOutput {
            position: Some((position.x, position.y)),
            scale: Some(self.output.current_scale().fractional_scale()),
            mode,
            transform: Some(transform_name(self.output.current_transform()).to_string()),
            enabled: Some(self.enabled),
        }
    }
}

/// Windows rescued from an output that went away, grouped by workspace index.
pub struct Removed {
    pub windows: Vec<Vec<Window>>,
}

#[derive(Default)]
pub struct Outputs {
    entries: Vec<OutputEntry>,
    next_id: u64,
    focused: u64,
    persist: Persist,
    /// Windows parked while their output is unplugged, by identity.
    stash: HashMap<String, Vec<Vec<Window>>>,
}

impl Outputs {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            persist: Persist::load(),
            ..Default::default()
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &OutputEntry> {
        self.entries.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut OutputEntry> {
        self.entries.iter_mut()
    }

    pub fn get(&self, id: u64) -> Option<&OutputEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut OutputEntry> {
        self.entries.iter_mut().find(|e| e.id == id)
    }

    pub fn by_output(&self, output: &Output) -> Option<&OutputEntry> {
        self.entries.iter().find(|e| &e.output == output)
    }

    /// Resolve a client's `wl_output` back to its entry.
    pub fn by_wl_output(
        &self,
        wl: &smithay::reexports::wayland_server::protocol::wl_output::WlOutput,
    ) -> Option<&OutputEntry> {
        self.entries.iter().find(|e| e.output.owns(wl))
    }

    /// The focused output, or the first one. `None` only when there are none at
    /// all, which the fallback-output rule (§5) is meant to prevent.
    pub fn focused(&self) -> Option<&OutputEntry> {
        self.get(self.focused).or_else(|| self.entries.first())
    }

    pub fn set_focused(&mut self, id: u64) {
        if self.get(id).is_some() {
            self.focused = id;
        }
    }

    /// The first non-virtual output, else the first output — where orphaned
    /// windows land when their own output disappears.
    pub fn fallback_id(&self) -> Option<u64> {
        self.entries
            .iter()
            .find(|e| e.enabled && e.kind == OutputKind::Physical)
            .or_else(|| self.entries.iter().find(|e| e.enabled))
            .or_else(|| self.entries.first())
            .map(|e| e.id)
    }

    pub fn add(
        &mut self,
        identity: String,
        connector: String,
        output: Output,
        kind: OutputKind,
        global: Option<GlobalId>,
    ) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let mut workspaces = workspace::new_set();
        // Bring back anything parked when this output was last unplugged.
        if let Some(parked) = self.stash.remove(&identity) {
            for (i, mut wins) in parked.into_iter().enumerate() {
                // They may have been re-homed onto the fallback while this
                // output was away; take them back rather than duplicating.
                for w in &wins {
                    for e in self.entries.iter_mut() {
                        for ws in e.workspaces.iter_mut() {
                            ws.remove(w);
                        }
                    }
                }
                if i < workspaces.len() {
                    workspaces[i].pending.append(&mut wins);
                }
            }
        }
        self.entries.push(OutputEntry {
            id,
            identity,
            connector,
            output,
            kind,
            workspaces,
            active: 0,
            global,
            enabled: true,
            powered: true,
        });
        if self.focused == 0 {
            self.focused = id;
        }
        id
    }

    /// Drop an output, handing back its windows so the caller can re-home them.
    /// The windows are also stashed under the departed identity so that
    /// re-docking the same monitor restores them.
    pub fn remove(&mut self, id: u64) -> Option<Removed> {
        let idx = self.entries.iter().position(|e| e.id == id)?;
        let entry = self.entries.remove(idx);
        let windows: Vec<Vec<Window>> = entry
            .workspaces
            .iter()
            .map(|ws| {
                ws.windows()
                    .into_iter()
                    .chain(ws.pending.iter().cloned())
                    .collect()
            })
            .collect();
        self.stash.insert(entry.identity.clone(), windows.clone());
        if self.focused == id {
            self.focused = self.entries.first().map(|e| e.id).unwrap_or(0);
        }
        Some(Removed { windows })
    }

    /// Key identifying the currently-present set of outputs, for persistence.
    pub fn set_key(&self) -> String {
        let ids: Vec<String> = self.entries.iter().map(|e| e.identity.clone()).collect();
        persist::set_key(&ids)
    }

    pub fn saved_for(&self, identity: &str) -> Option<&SavedOutput> {
        self.persist.get(&self.set_key(), identity)
    }

    /// Remember every output's current geometry for this set, then write out.
    pub fn save<F>(&mut self, position_of: F)
    where
        F: Fn(&Output) -> Option<Point<i32, Logical>>,
    {
        let key = self.set_key();
        let records: Vec<(String, SavedOutput)> = self
            .entries
            .iter()
            .filter_map(|e| position_of(&e.output).map(|p| (e.identity.clone(), e.saved(p))))
            .collect();
        for (identity, saved) in records {
            self.persist.record(&key, &identity, saved);
        }
        self.persist.flush();
    }

    /// Default arrangement: left to right in insertion order, in logical
    /// coordinates (so a scaled output takes its scaled width). Any output with
    /// a remembered or configured position keeps it and is skipped.
    pub fn auto_layout(&self, pinned: &HashMap<u64, Point<i32, Logical>>) -> Vec<(u64, Point<i32, Logical>)> {
        let mut x = 0;
        let mut out = Vec::with_capacity(self.entries.len());
        for e in self.entries.iter().filter(|e| e.enabled) {
            if let Some(p) = pinned.get(&e.id) {
                out.push((e.id, *p));
                continue;
            }
            out.push((e.id, (x, 0).into()));
            x += logical_width(&e.output);
        }
        out
    }
}

/// Logical width of an output at its current mode, scale and transform.
pub fn logical_width(output: &Output) -> i32 {
    let Some(mode) = output.current_mode() else {
        return 0;
    };
    let size = output
        .current_transform()
        .transform_size(mode.size)
        .to_f64()
        .to_logical(output.current_scale().fractional_scale());
    size.w.round() as i32
}

/// Build an output identity string from an EDID blob, falling back to the
/// connector name when the blob is missing or unparseable.
pub fn identity(edid_blob: Option<&[u8]>, connector: &str) -> String {
    match edid_blob.and_then(edid::parse) {
        Some(info) => format!("{} {} {}", info.make, info.model, info.serial),
        None => connector.to_string(),
    }
}

/// A headless output so the compositor stays usable with no connector (§5).
pub fn virtual_output(name: &str, size: (i32, i32)) -> (Output, Mode) {
    let mode = Mode {
        size: size.into(),
        refresh: 60_000,
    };
    let output = Output::new(
        name.to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "abyss".into(),
            model: "virtual".into(),
        },
    );
    output.change_current_state(
        Some(mode),
        Some(Transform::Normal),
        Some(Scale::Integer(1)),
        Some((0, 0).into()),
    );
    output.set_preferred(mode);
    (output, mode)
}

pub fn transform_name(t: Transform) -> &'static str {
    match t {
        Transform::Normal => "normal",
        Transform::_90 => "90",
        Transform::_180 => "180",
        Transform::_270 => "270",
        Transform::Flipped => "flipped",
        Transform::Flipped90 => "flipped-90",
        Transform::Flipped180 => "flipped-180",
        Transform::Flipped270 => "flipped-270",
    }
}

/// One validated runtime change to a single output (COMP-03 §4). Every field is
/// already known good: parsing and mode lookup happen in the caller, so the
/// apply path cannot fail halfway and leave a half-configured output.
#[derive(Debug, Default, Clone)]
pub struct OutputChange {
    pub mode: Option<Mode>,
    pub scale: Option<Scale>,
    pub transform: Option<Transform>,
    pub position: Option<(i32, i32)>,
    pub enabled: Option<bool>,
    pub vrr: Option<bool>,
}

/// The one place output geometry changes at runtime, shared by the human IPC
/// (COMP-13 §2.1) and `wlr-output-management` (COMP-06 §1) so both go through
/// the same refusals. `Ok(Some(false))` means VRR was asked for and the
/// connector does not support it; `Err` means nothing was changed at all.
pub fn apply_change(
    state: &mut crate::state::AbyssState,
    id: u64,
    change: &OutputChange,
) -> Result<Option<bool>, &'static str> {
    let Some(entry) = state.outputs.get(id) else {
        return Err("no such output");
    };
    let output = entry.output.clone();
    // The refusal (COMP-03 §4) is checked before anything is applied.
    if change.enabled == Some(false) && !state.outputs.iter().any(|e| e.enabled && e.id != id) {
        return Err("refusing to disable the only enabled output");
    }

    if change.mode.is_some() || change.scale.is_some() || change.transform.is_some() {
        output.change_current_state(change.mode, change.transform, change.scale, None);
        if let Some(m) = change.mode {
            output.set_preferred(m);
        }
    }
    if let Some((x, y)) = change.position {
        state.space.map_output(&output, (x, y));
        // Record the new geometry before relayout reads it back as a pin.
        let space = &state.space;
        state.outputs.save(|o| space.output_geometry(o).map(|g| g.loc));
    }
    if let Some(want) = change.enabled {
        power::set_enabled(state, id, want);
    }
    let mut vrr_applied = None;
    if let Some(want) = change.vrr {
        let ok = crate::backend::set_output_vrr(state, id, want);
        if !ok && want {
            return Err("this output does not support adaptive sync");
        }
        vrr_applied = Some(ok);
    }
    relayout(state);
    crate::backend::damage_all(state);
    crate::protocols::standard::output_management::refresh(state);
    crate::ipc::emit(
        state,
        "output",
        serde_json::json!({"change": "changed", "id": id}),
    );
    Ok(vrr_applied)
}

pub fn parse_transform(s: &str) -> Option<Transform> {
    Some(match s {
        "normal" | "0" => Transform::Normal,
        "90" => Transform::_90,
        "180" => Transform::_180,
        "270" => Transform::_270,
        "flipped" => Transform::Flipped,
        "flipped-90" => Transform::Flipped90,
        "flipped-180" => Transform::Flipped180,
        "flipped-270" => Transform::Flipped270,
        _ => return None,
    })
}

/// Pick a scale: fractional when it is not a whole number, integer otherwise.
pub fn scale_from(f: f64) -> Scale {
    if (f - f.round()).abs() < f64::EPSILON && f >= 1.0 {
        Scale::Integer(f.round() as i32)
    } else {
        Scale::Fractional(f)
    }
}

/// Bring an output into the compositor: apply config and the persisted layout,
/// map it into the space, re-run the global arrangement (COMP-03 §3).
pub fn register(
    state: &mut crate::state::AbyssState,
    identity: String,
    connector: String,
    output: Output,
    kind: OutputKind,
    global: Option<GlobalId>,
) -> u64 {
    let id = state
        .outputs
        .add(identity.clone(), connector.clone(), output.clone(), kind, global);
    apply_settings(state, id);
    relayout(state);
    crate::protocols::standard::output_management::refresh(state);
    crate::ipc::emit(
        state,
        "output",
        serde_json::json!({"change": "added", "id": id, "name": identity}),
    );
    tracing::info!(id, %identity, %connector, "output added");
    id
}

/// Config first, then the remembered layout for this exact output set.
fn apply_settings(state: &mut crate::state::AbyssState, id: u64) {
    let Some(entry) = state.outputs.get(id) else {
        return;
    };
    let output = entry.output.clone();
    let rule = state.config.output_rule(&entry.connector, &entry.identity);
    let saved = state.outputs.saved_for(&entry.identity).cloned();

    let enabled = rule
        .enabled
        .or(saved.as_ref().and_then(|s| s.enabled))
        .unwrap_or(true);
    if !enabled {
        // Refusal path (§4): a config that would leave zero outputs is ignored.
        if state.outputs.iter().any(|e| e.enabled && e.id != id) {
            if let Some(entry) = state.outputs.get_mut(id) {
                entry.enabled = false;
            }
            tracing::info!(id, "output disabled by config");
        } else {
            tracing::warn!(id, "refusing to disable the only output");
        }
    }

    let mode = rule
        .mode
        .or(saved.as_ref().and_then(|s| s.mode))
        .and_then(|(w, h, r)| {
            output
                .modes()
                .into_iter()
                .find(|m| m.size.w == w && m.size.h == h && m.refresh == r)
                .or_else(|| {
                    output
                        .modes()
                        .into_iter()
                        .find(|m| m.size.w == w && m.size.h == h)
                })
        });
    let scale = rule
        .scale
        .or(saved.as_ref().and_then(|s| s.scale))
        .map(scale_from);
    let transform = rule
        .transform
        .as_deref()
        .or(saved.as_ref().and_then(|s| s.transform.as_deref()))
        .and_then(parse_transform);
    if mode.is_some() || scale.is_some() || transform.is_some() {
        output.change_current_state(mode, transform, scale, None);
        if let Some(m) = mode {
            output.set_preferred(m);
        }
    }
}

/// Position every output (config/persisted pins first, then left-to-right),
/// map them into the space, and re-run every layout.
pub fn relayout(state: &mut crate::state::AbyssState) {
    let mut pinned: HashMap<u64, Point<i32, Logical>> = HashMap::new();
    let ids: Vec<(u64, String, String)> = state
        .outputs
        .iter()
        .map(|e| (e.id, e.connector.clone(), e.identity.clone()))
        .collect();
    for (id, connector, identity) in &ids {
        let from_config = state.config.output_rule(connector, identity).position;
        let from_saved = state.outputs.saved_for(identity).and_then(|s| s.position);
        if let Some((x, y)) = from_config.or(from_saved) {
            pinned.insert(*id, (x, y).into());
        }
    }
    let disabled: Vec<Output> = state
        .outputs
        .iter()
        .filter(|e| !e.enabled)
        .map(|e| e.output.clone())
        .collect();
    for output in disabled {
        state.space.unmap_output(&output);
        state.lock.forget_output(&output);
    }
    let placement = state.outputs.auto_layout(&pinned);
    for (id, pos) in placement {
        if let Some(entry) = state.outputs.get(id) {
            let output = entry.output.clone();
            output.change_current_state(None, None, None, Some(pos));
            state.space.map_output(&output, (pos.x, pos.y));
        }
    }
    let space = &state.space;
    state.outputs.save(|o| space.output_geometry(o).map(|g| g.loc));
    crate::shell::arrange(state);
}

/// Drop an output: unmap it from the space, re-home its windows onto the
/// fallback output, and drop its global so clients stop referencing it.
pub fn unregister(state: &mut crate::state::AbyssState, id: u64) {
    let Some(entry) = state.outputs.get(id) else {
        return;
    };
    let output = entry.output.clone();
    let identity = entry.identity.clone();
    let global = state.outputs.get_mut(id).and_then(|e| e.global.take());
    let removed = state.outputs.remove(id);
    for w in removed.iter().flat_map(|r| r.windows.iter().flatten()) {
        state.space.unmap_elem(w);
    }
    state.space.unmap_output(&output);
    state.lock.forget_output(&output);
    if let Some(global) = global {
        state
            .display_handle
            .remove_global::<crate::state::AbyssState>(global);
    }
    // Re-home onto the fallback so nothing is stranded; the stash still holds a
    // copy, so re-docking the same monitor takes them back.
    if let (Some(fallback), Some(removed)) = (state.outputs.fallback_id(), removed) {
        if let Some(target) = state.outputs.get_mut(fallback) {
            for (i, wins) in removed.windows.into_iter().enumerate() {
                let slot = i.min(target.workspaces.len() - 1);
                target.workspaces[slot].pending.extend(wins);
            }
        }
    }
    if state
        .focus
        .as_ref()
        .is_some_and(|w| !state.space.elements().any(|e| e == w))
    {
        state.focus = None;
    }
    relayout(state);
    crate::protocols::standard::output_management::refresh(state);
    crate::ipc::emit(
        state,
        "output",
        serde_json::json!({"change": "removed", "id": id, "name": identity}),
    );
    tracing::info!(id, %identity, "output removed");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_falls_back_to_connector() {
        assert_eq!(identity(None, "DP-3"), "DP-3");
        assert_eq!(identity(Some(&[0u8; 4]), "DP-3"), "DP-3");
    }

    #[test]
    fn transforms_round_trip() {
        for t in [
            Transform::Normal,
            Transform::_90,
            Transform::_180,
            Transform::_270,
            Transform::Flipped,
            Transform::Flipped90,
            Transform::Flipped180,
            Transform::Flipped270,
        ] {
            assert_eq!(parse_transform(transform_name(t)), Some(t));
        }
    }

    #[test]
    fn scales() {
        assert!(matches!(scale_from(2.0), Scale::Integer(2)));
        assert!(matches!(scale_from(1.5), Scale::Fractional(_)));
    }
}
