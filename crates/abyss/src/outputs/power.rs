// SPDX-License-Identifier: AGPL-3.0-only
//! Output power and enablement (COMP-03 §4, §7).
//!
//! Two different things live here on purpose. *Power* is DPMS: the CRTC stops
//! scanning out, but the output keeps its place in the layout and its windows.
//! *Enabled* is membership of the desktop: a disabled output is unmapped and
//! its windows are re-homed. Both refuse to leave the human with nothing:
//! the last enabled output cannot be disabled, and powering every output off
//! is only ever done by the idle path, which powers them back on at the next
//! input event.

use smithay::desktop::Window;

use crate::state::AbyssState;

/// DPMS one output.
pub fn set_power(state: &mut AbyssState, id: u64, on: bool) {
    let Some(entry) = state.outputs.get_mut(id) else {
        return;
    };
    if entry.powered == on {
        return;
    }
    entry.powered = on;
    tracing::info!(id, on, "output power");
    crate::backend::set_output_power(state, id, on);
    crate::protocols::standard::output_power::broadcast(state, id, on);
}

/// DPMS every enabled output. Virtual outputs are never scanned out, so power
/// does not apply to them (§7).
pub fn set_all_power(state: &mut AbyssState, on: bool) {
    let ids: Vec<u64> = state
        .outputs
        .iter()
        .filter(|e| e.enabled && e.kind == crate::outputs::OutputKind::Physical)
        .map(|e| e.id)
        .collect();
    for id in ids {
        set_power(state, id, on);
    }
}

/// Add or remove an output from the desktop. Returns `false` when the request
/// was refused because it would have left no enabled output (§4).
pub fn set_enabled(state: &mut AbyssState, id: u64, enabled: bool) -> bool {
    let Some(entry) = state.outputs.get(id) else {
        return false;
    };
    if entry.enabled == enabled {
        return true;
    }
    if !enabled && !state.outputs.iter().any(|e| e.enabled && e.id != id) {
        tracing::warn!(id, "refusing to disable the only enabled output");
        return false;
    }
    if let Some(entry) = state.outputs.get_mut(id) {
        entry.enabled = enabled;
    }
    if enabled {
        set_power(state, id, true);
    } else {
        rehome(state, id);
        let output = state.outputs.get(id).map(|e| e.output.clone());
        if let Some(output) = output {
            state.space.unmap_output(&output);
        }
        set_power(state, id, false);
    }
    tracing::info!(id, enabled, "output enablement changed");
    crate::outputs::relayout(state);
    true
}

/// Move everything off a disabled output so no window is stranded.
fn rehome(state: &mut AbyssState, id: u64) {
    let Some(target) = state
        .outputs
        .iter()
        .find(|e| e.enabled && e.id != id)
        .map(|e| e.id)
    else {
        return;
    };
    let mut moved: Vec<Vec<Window>> = Vec::new();
    if let Some(entry) = state.outputs.get_mut(id) {
        for ws in entry.workspaces.iter_mut() {
            let mut wins = ws.windows();
            for w in &wins {
                ws.remove(w);
            }
            wins.append(&mut ws.pending);
            moved.push(wins);
        }
    }
    for w in moved.iter().flatten() {
        state.space.unmap_elem(w);
    }
    if let Some(target) = state.outputs.get_mut(target) {
        for (i, wins) in moved.into_iter().enumerate() {
            let slot = i.min(target.workspaces.len() - 1);
            target.workspaces[slot].pending.extend(wins);
        }
    }
}

/// The internal panel, if there is one: eDP/LVDS/DSI by connector name.
pub fn internal_output(state: &AbyssState) -> Option<u64> {
    state
        .outputs
        .iter()
        .find(|e| {
            let c = e.connector.to_ascii_lowercase();
            c.starts_with("edp") || c.starts_with("lvds") || c.starts_with("dsi")
        })
        .map(|e| e.id)
}

/// Laptop lid opened or closed (COMP-01 §4.1). With no second output the lid is
/// ignored — closing the last screen is the one thing we never do.
pub fn lid_switch(state: &mut AbyssState, closed: bool) {
    let Some(id) = internal_output(state) else {
        tracing::debug!(closed, "lid switch with no internal panel, ignored");
        return;
    };
    let action = state
        .outputs
        .get(id)
        .map(|e| state.config.output_rule(&e.connector, &e.identity))
        .and_then(|r| r.lid_close)
        .unwrap_or_else(|| "off".to_string());
    if action == "ignore" {
        tracing::info!(closed, "lid switch ignored by config");
        return;
    }
    if action == "suspend" {
        // Suspend is logind's call, not ours (COMP-01 §8): it owns the inhibitor
        // locks and the wake path, and re-implementing that over its D-Bus API
        // would only duplicate what systemctl already does correctly. Opening
        // the lid is handled by the resume path, so there is nothing to undo.
        if closed {
            tracing::info!("lid closed, suspending");
            crate::shell::spawn("systemctl suspend");
        }
        return;
    }
    if closed {
        if !set_enabled(state, id, false) {
            tracing::info!("lid closed with no other output, keeping the panel on");
        }
    } else {
        set_enabled(state, id, true);
    }
}
