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

pub mod calibrate;
pub mod edid;
pub mod overscan;
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
    /// Effective overscan compensation (COMP-03 §2). Purely a render and input
    /// transform: the logical size stays the full mode size, so layout, layer
    /// shell and window placement never see it. Non-zero costs direct scanout
    /// on this output.
    pub overscan: overscan::Overscan,
    /// Set while the calibration overlay owns this output's input.
    pub calibrating: Option<Calibration>,
    /// Stable, human-facing display number (ADR 0049). Assigned by connection
    /// order the first time this output is seen this run, unless a config
    /// `output { number N; }` override applies. Never reassigned on removal
    /// of another output — a gap persists until restart. Purely positional
    /// bookkeeping: never derived from or fed back into `identity`.
    pub number: u8,
}

/// Live state of the on-screen overscan calibration (COMP-03 §2). The overlay
/// is compositor-drawn — it has to be, since its corner markers live in the
/// framebuffer margin that no client can ever address.
#[derive(Debug, Clone)]
pub struct Calibration {
    /// Value to restore on `Esc`.
    pub original: overscan::Overscan,
    /// Last time a key was handled; the overlay self-cancels after
    /// [`CALIBRATION_TIMEOUT`] so a TV left mid-calibration recovers on its own.
    pub last_input: std::time::Instant,
}

/// How long the calibration overlay waits before reverting itself.
pub const CALIBRATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

impl OutputEntry {
    pub fn workspace(&self) -> &Workspace {
        &self.workspaces[self.active]
    }

    /// The state worth remembering for next time.
    ///
    /// A scale that only matches what `auto_scale` would derive anyway is not
    /// worth remembering, and remembering it does harm: the saved layout beats
    /// the derived default, so a machine that ran abyss once would be pinned
    /// to whatever the default was that day and could never pick up a better
    /// one. Only a scale that departs from the derived value is a decision.
    fn saved(&self, position: Point<i32, Logical>) -> SavedOutput {
        let mode = self
            .output
            .current_mode()
            .map(|m| (m.size.w, m.size.h, m.refresh));
        let scale = self.output.current_scale().fractional_scale();
        let phys = self.output.physical_properties().size;
        let px = mode_size(&self.output);
        let derived = auto_scale((phys.w, phys.h), (px.w, px.h)).unwrap_or(1.0);
        SavedOutput {
            position: Some((position.x, position.y)),
            scale: (scale != derived).then_some(scale),
            mode,
            transform: Some(transform_name(self.output.current_transform()).to_string()),
            enabled: Some(self.enabled),
        }
    }
}

/// Sentinel `Outputs::focused` value meaning "no output is focused". Handles
/// start at 1 and are never reused, so 0 can never name a real output.
pub const NO_OUTPUT: u64 = 0;

/// Windows rescued from an output that went away, grouped by workspace index.
pub struct Removed {
    pub windows: Vec<Vec<Window>>,
}

#[derive(Default)]
pub struct Outputs {
    entries: Vec<OutputEntry>,
    next_id: u64,
    /// Next display number (ADR 0049) to hand out by connection order.
    next_number: u8,
    focused: u64,
    persist: Persist,
    /// Windows parked while their output is unplugged, by identity.
    stash: HashMap<String, Vec<Vec<Window>>>,
}

impl Outputs {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            next_number: 1,
            persist: Persist::load(),
            ..Default::default()
        }
    }

    /// Display number (ADR 0049) already in use by some other output, so a
    /// config override never collides with the connection-order default.
    fn number_taken(&self, number: u8) -> bool {
        self.entries.iter().any(|e| e.number == number)
    }

    /// Apply an explicit config override for an output's display number
    /// (ADR 0049), skipping it (with a warning) if already taken.
    pub fn set_number(&mut self, id: u64, number: u8) {
        if self.number_taken(number) {
            tracing::warn!(id, number, "output number already in use, keeping default");
            return;
        }
        if let Some(entry) = self.get_mut(id) {
            entry.number = number;
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

    /// The focused output, and only that. `None` means nothing is focused —
    /// either no outputs exist at all, or the focused one just went away and
    /// [`Outputs::repair_focus`] found nothing to move to.
    ///
    /// There is deliberately no fall-back to the first entry (ADR 0042): the
    /// pointer-focus decision compares "output under the pointer" against
    /// "output of the focused window", and a silent fall-back makes that
    /// comparison report a same-output match that never happened. The id is
    /// repaired explicitly on removal instead, so a dangling id cannot exist
    /// for a caller to paper over.
    pub fn focused(&self) -> Option<&OutputEntry> {
        self.get(self.focused)
    }

    /// The focused output's id, or `None`. Sentinel-free view of `self.focused`.
    pub fn focused_id(&self) -> Option<u64> {
        self.focused().map(|e| e.id)
    }

    /// Focus an output by id. Returns true when the focused id actually
    /// changed, so callers on the pointer-motion hot path can skip building an
    /// event payload on the overwhelmingly common no-op.
    pub fn set_focused(&mut self, id: u64) -> bool {
        if self.get(id).is_some() && self.focused != id {
            self.focused = id;
            return true;
        }
        false
    }

    /// Re-point `focused` at a live output when it no longer resolves to one.
    ///
    /// Returns `Some(new)` when the id actually moved — `Some(None)` meaning
    /// "nothing is focused any more" — and `None` when it was already fine, so
    /// the caller emits the `output` event exactly on a real transition, the
    /// same contract as [`Outputs::set_focused`].
    ///
    /// The replacement is the lowest surviving id: deterministic, and the
    /// oldest output is the one most likely to be the human's primary.
    #[allow(clippy::option_option)]
    pub fn repair_focus(&mut self) -> Option<Option<u64>> {
        if self.get(self.focused).is_some() {
            return None;
        }
        let next = self.entries.iter().map(|e| e.id).min();
        self.focused = next.unwrap_or(NO_OUTPUT);
        Some(next)
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
        // Two panels can advertise byte-identical EDIDs (a TV with two inputs
        // does exactly that). Identity has to stay unique or they share one
        // persisted layout and one window stash, and the second one's saved
        // mode gets applied to the first — an unsatisfiable modeset that the
        // kernel rejects on every frame, forever.
        let identity = if self.entries.iter().any(|e| e.identity == identity) {
            format!("{identity} ({connector})")
        } else {
            identity
        };
        let id = self.next_id;
        self.next_id += 1;
        // ADR 0049: connection-order default. `next_number` only ever
        // increases, so a mid-session disconnect leaves a gap rather than
        // renumbering the outputs still attached.
        let number = self.next_number;
        self.next_number = self.next_number.saturating_add(1);
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
            number,
            identity,
            connector,
            output,
            kind,
            workspaces,
            active: 0,
            global,
            enabled: true,
            powered: true,
            overscan: overscan::Overscan::default(),
            calibrating: None,
        });
        if self.focused == NO_OUTPUT {
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
                ws.all_windows()
                    .into_iter()
                    .chain(ws.pending.iter().cloned())
                    .collect()
            })
            .collect();
        self.stash.insert(entry.identity.clone(), windows.clone());
        // Focus repair is *not* done here: it has an observable side effect
        // (the `output` IPC event) and this method is pure state. `unregister`
        // calls `repair_focus` and emits. See ADR 0042.
        Some(Removed { windows })
    }

    /// Overscan remembered for a panel, keyed by identity alone — see
    /// [`persist`]'s module docs for why this one field is not set-keyed.
    pub fn saved_overscan(&self, identity: &str) -> Option<overscan::Overscan> {
        self.persist.overscan(identity)
    }

    /// Remember calibrated overscan for a panel and write it out.
    pub fn save_overscan(&mut self, identity: &str, value: overscan::Overscan) {
        self.persist.record_overscan(identity, value);
        self.persist.flush();
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
    pub overscan: Option<overscan::Overscan>,
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

    // Refused before anything is applied, so the failure cannot leave the
    // `Output` describing a mode the scanout surface is not actually in.
    if let Some(m) = change.mode {
        if !crate::backend::set_output_mode(state, id, m) {
            return Err("backend refused that mode");
        }
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
    if let Some(value) = change.overscan {
        set_overscan(state, id, value, true);
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

/// The scale to use for a panel nobody has configured (COMP-03 §1).
///
/// A HiDPI laptop at scale 1 is a desktop nobody can read, and "edit a config
/// file first" is not an answer for a machine that is meant to install without
/// a terminal — the owner's Framework 13 (2880x1920 on a 13.5" panel, ~257
/// DPI) came up at 1.0 and was unusable. So the default is derived from the
/// panel instead of assumed.
///
/// The thresholds are integer steps at 192 and 384 DPI (2x and 4x of the
/// traditional 96), which is the same shape GNOME and KDE use and lands the
/// common cases where people expect: 1080p/24" and 4K/27" stay at 1, a modern
/// laptop panel goes to 2. Fractional scales are deliberately not guessed —
/// they cost a blit and a round of client rescaling, so they stay something a
/// human asks for.
///
/// Returns `None` when the EDID gives no usable physical size (0x0 is common
/// on projectors and virtual connectors) or when the numbers imply something
/// absurd, in which case the caller keeps scale 1.
pub fn auto_scale(physical_mm: (i32, i32), mode_px: (i32, i32)) -> Option<f64> {
    let (mm_w, px_w) = (physical_mm.0, mode_px.0);
    if mm_w <= 0 || px_w <= 0 {
        return None;
    }
    // Under ~100mm wide is not a panel anyone is sitting in front of; it is a
    // connector reporting nonsense, and dividing by it invents a huge DPI.
    if mm_w < 100 {
        return None;
    }
    let dpi = f64::from(px_w) * 25.4 / f64::from(mm_w);
    let scale = match dpi {
        d if d >= 384.0 => 3.0,
        d if d >= 192.0 => 2.0,
        _ => return None,
    };
    // A scaled desktop smaller than 1024 logical pixels wide is worse than a
    // small one: panels stop fitting. Refuse rather than produce that.
    if f64::from(px_w) / scale < 1024.0 {
        return None;
    }
    Some(scale)
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

/// Re-apply every output's config rule to the outputs that already exist.
///
/// Called on config reload (COMP-13): `apply_settings` otherwise runs only
/// when an output is added, so editing `output "eDP-1" { scale 2.0 }` and
/// reloading changed nothing until the connector was replugged or the session
/// restarted. Rules are declarative and `apply_settings` is idempotent, so
/// running it again over the live set is the whole fix.
///
/// One asymmetry is deliberate: an output already disabled by an earlier
/// config is not re-enabled here, because `apply_settings` treats "enabled"
/// as the absence of a refusal rather than a state to drive.
pub fn reapply_settings(state: &mut crate::state::AbyssState) {
    let ids: Vec<u64> = state.outputs.iter().map(|e| e.id).collect();
    for id in ids {
        apply_settings(state, id);
    }
}

/// Config first, then the remembered layout for this exact output set.
fn apply_settings(state: &mut crate::state::AbyssState, id: u64) {
    let Some(entry) = state.outputs.get(id) else {
        return;
    };
    let output = entry.output.clone();
    let ident = entry.identity.clone();
    let rule = state.config.output_rule(&entry.connector, &entry.identity);
    let saved = state.outputs.saved_for(&entry.identity).cloned();

    if let Some(number) = rule.number {
        state.outputs.set_number(id, number);
    }

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
    // Config, then what this output was last set to, then the panel's own
    // DPI. The derived value is last so it never argues with a human.
    let scale = rule
        .scale
        .or(saved.as_ref().and_then(|s| s.scale))
        .or_else(|| {
            let phys = output.physical_properties().size;
            let px = mode_size(&output);
            let auto = auto_scale((phys.w, phys.h), (px.w, px.h));
            if let Some(s) = auto {
                tracing::info!(id, scale = s, "scale derived from panel DPI");
            }
            auto
        })
        .map(scale_from);
    let transform = rule
        .transform
        .as_deref()
        .or(saved.as_ref().and_then(|s| s.transform.as_deref()))
        .and_then(parse_transform);
    // The backend has to accept the mode first: changing only the `Output`
    // leaves the scanout surface on its old timing, and every atomic commit
    // then asks the primary plane to scale, which no plane here can do.
    let mode = match mode {
        Some(m) if !crate::backend::set_output_mode(state, id, m) => {
            tracing::warn!(
                id,
                w = m.size.w,
                h = m.size.h,
                "backend refused mode, keeping current"
            );
            None
        }
        other => other,
    };
    if mode.is_some() || scale.is_some() || transform.is_some() {
        output.change_current_state(mode, transform, scale, None);
        if let Some(m) = mode {
            output.set_preferred(m);
        }
    }

    // Config wins over persisted calibration, same precedence as every other
    // field above: `abyss.kdl` is hand-written and is the human's stated
    // intent, `outputs.kdl` is only what we last observed.
    let value = rule
        .overscan
        .or_else(|| state.outputs.saved_overscan(&ident))
        .unwrap_or_default();
    if let Some(entry) = state.outputs.get_mut(id) {
        entry.overscan = value.clamped(mode_size(&output));
    }
}

/// Physical size of an output's current mode — the framebuffer the overscan
/// inset is measured against.
pub fn mode_size(output: &Output) -> smithay::utils::Size<i32, smithay::utils::Physical> {
    output
        .current_mode()
        .map(|m| (m.size.w, m.size.h).into())
        .unwrap_or_else(|| (0, 0).into())
}

/// Overscan currently in effect for an output.
pub fn overscan_of(state: &crate::state::AbyssState, id: u64) -> overscan::Overscan {
    state.outputs.get(id).map(|e| e.overscan).unwrap_or_default()
}

/// The one place overscan changes. `persist` is false while calibrating — the
/// preview must be live on screen without writing a file on every arrow key.
///
/// A hand-written `overscan` in `abyss.kdl` outranks the state file, so when
/// one is present this reports that the new value will not survive a restart
/// rather than silently taking effect and then vanishing.
pub fn set_overscan(
    state: &mut crate::state::AbyssState,
    id: u64,
    value: overscan::Overscan,
    persist: bool,
) -> bool {
    let Some(entry) = state.outputs.get(id) else {
        return false;
    };
    let (output, identity, connector) = (
        entry.output.clone(),
        entry.identity.clone(),
        entry.connector.clone(),
    );
    let value = value.clamped(mode_size(&output));

    let overridden = state.config.output_rule(&connector, &identity).overscan.is_some();

    if let Some(entry) = state.outputs.get_mut(id) {
        entry.overscan = value;
    }
    if persist && !overridden {
        state.outputs.save_overscan(&identity, value);
    }

    crate::backend::damage_all(state);
    crate::ipc::emit(
        state,
        "output",
        serde_json::json!({
            "change": "overscan",
            "id": id,
            "top": value.top,
            "bottom": value.bottom,
            "left": value.left,
            "right": value.right,
            "persisted": persist && !overridden,
        }),
    );
    !overridden
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
            let last = target.workspaces.len().saturating_sub(1);
            if !target.workspaces.is_empty() {
                for (i, wins) in removed.windows.into_iter().enumerate() {
                    let slot = i.min(last);
                    target.workspaces[slot].pending.extend(wins);
                }
            }
        }
    }
    // The focused id must never dangle: ADR 0042's pointer-focus decision
    // compares it against the output under the pointer, and an id naming a
    // departed output makes that comparison lie. Repair it explicitly and emit
    // the same `output` event the pointer and keyboard focus paths emit, so
    // hyperion's per-output fold does not keep folding against a ghost.
    if let Some(next) = state.outputs.repair_focus() {
        let name = next
            .and_then(|id| state.outputs.get(id))
            .map(|e| e.connector.clone());
        crate::ipc::emit(
            state,
            "output",
            serde_json::json!({ "focused": next, "name": name }),
        );
        tracing::info!(?next, "focused output repaired after removal");
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

    /// The cases the thresholds exist to get right, named by the hardware
    /// they came from rather than by the numbers.
    #[test]
    fn auto_scale_matches_the_panels_it_was_written_for() {
        // Framework 13, 2880x1920 on 13.5" (~257 DPI) — the panel that
        // prompted this: unreadable at 1.0.
        assert_eq!(auto_scale((285, 190), (2880, 1920)), Some(2.0));
        // 1080p 24" (~92 DPI) and 4K 27" (~163 DPI) both stay at 1: neither is
        // a 2x panel, and guessing a fractional scale is not this function's
        // job.
        assert_eq!(auto_scale((531, 299), (1920, 1080)), None);
        assert_eq!(auto_scale((597, 336), (3840, 2160)), None);
        // 5K 27" (~218 DPI) is over the line.
        assert_eq!(auto_scale((597, 336), (5120, 2880)), Some(2.0));
    }

    #[test]
    fn auto_scale_refuses_nonsense_edid() {
        // Projectors and virtual connectors report 0x0.
        assert_eq!(auto_scale((0, 0), (1920, 1080)), None);
        assert_eq!(auto_scale((-1, -1), (1920, 1080)), None);
        assert_eq!(auto_scale((285, 190), (0, 0)), None);
        // A "panel" 5cm wide is a lie; without the guard this reports 975 DPI.
        assert_eq!(auto_scale((50, 30), (1920, 1080)), None);
        // High DPI but tiny: scaling it would leave under 1024 logical px.
        assert_eq!(auto_scale((110, 70), (1280, 800)), None);
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

    /// Two-plus outputs with no DRM and no display: `virtual_output` is enough
    /// for everything the focus bookkeeping touches.
    fn outputs_with(n: usize) -> (Outputs, Vec<u64>) {
        let mut outputs = Outputs {
            next_id: 1,
            ..Default::default()
        };
        let ids = (0..n)
            .map(|i| {
                let name = format!("virtual-{i}");
                let (output, _) = virtual_output(&name, (800, 600));
                outputs.add(name.clone(), name, output, OutputKind::Virtual, None)
            })
            .collect();
        (outputs, ids)
    }

    #[test]
    fn removing_the_focused_output_repairs_the_id() {
        let (mut outputs, ids) = outputs_with(3);
        assert!(outputs.set_focused(ids[1]));
        outputs.remove(ids[1]);
        // Dangling until repaired — that is exactly the state `focused()` must
        // report honestly rather than papering over.
        assert_eq!(outputs.focused_id(), None);
        assert_eq!(outputs.repair_focus(), Some(Some(ids[0])));
        assert_eq!(outputs.focused_id(), Some(ids[0]));
    }

    #[test]
    fn removing_another_output_leaves_focus_alone() {
        let (mut outputs, ids) = outputs_with(3);
        assert!(outputs.set_focused(ids[2]));
        outputs.remove(ids[0]);
        assert_eq!(outputs.focused_id(), Some(ids[2]));
        // Nothing to repair, so nothing is emitted.
        assert_eq!(outputs.repair_focus(), None);
        assert_eq!(outputs.focused_id(), Some(ids[2]));
    }

    #[test]
    fn removing_the_last_output_leaves_nothing_focused() {
        let (mut outputs, ids) = outputs_with(1);
        assert_eq!(outputs.focused_id(), Some(ids[0]));
        outputs.remove(ids[0]);
        assert_eq!(outputs.repair_focus(), Some(None));
        assert_eq!(outputs.focused_id(), None);
        assert!(outputs.focused().is_none());
        assert_eq!(outputs.focused, NO_OUTPUT);
        // And the next output to arrive takes focus, as `add` promises.
        let (output, _) = virtual_output("virtual-x", (800, 600));
        let id = outputs.add("x".into(), "x".into(), output, OutputKind::Virtual, None);
        assert_eq!(outputs.focused_id(), Some(id));
    }

    #[test]
    fn scales() {
        assert!(matches!(scale_from(2.0), Scale::Integer(2)));
        assert!(matches!(scale_from(1.5), Scale::Fractional(_)));
    }
}
