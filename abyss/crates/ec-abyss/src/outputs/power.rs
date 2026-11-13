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
    if !enabled && !state.outputs.other_enabled(id) {
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
        // Focus must not stay on an output that is off: new windows are placed
        // on the focused output and would join a workspace nobody can see.
        if state.outputs.focused_id() == Some(id) {
            let next = state
                .outputs
                .iter()
                .find(|e| e.enabled && e.id != id)
                .map(|e| e.id);
            if let Some(next) = next {
                state.outputs.set_focused(next);
            }
        }
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
    crate::shell::maximize::note_departing(state, id);
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

/// Lid state the compositor remembers (COMP-01 §4.1, ADR 0108). Plain data:
/// the lid is read from libinput switch events, which also report the initial
/// state at startup; nothing re-reads it after resume, so a lid opened while
/// asleep is corrected by the open event libinput delivers on wake.
#[derive(Debug)]
pub struct LidState {
    pub closed: bool,
    /// A suspend was requested for this closure; cleared when the lid opens.
    pub suspend_sent: bool,
    /// False while the VT is switched away (set from the backend hook).
    pub session_active: bool,
    /// When the session last became active; the grace window counts from here
    /// (or from startup when it never has).
    pub activated: Option<std::time::Instant>,
    /// The pending removal debounce timer; at most one.
    pub timer: Option<smithay::reexports::calloop::RegistrationToken>,
}

impl Default for LidState {
    fn default() -> Self {
        Self {
            closed: false,
            suspend_sent: false,
            session_active: true,
            activated: None,
            timer: None,
        }
    }
}

/// A lid close this soon after startup or session activation is the initial or
/// replayed switch state, read before docked externals have come up; it never
/// suspends (§4.1).
const STARTUP_GRACE: std::time::Duration = std::time::Duration::from_secs(20);

/// How long "no other output" must persist before a removal suspends; output
/// ids churn for a few seconds around resume and replug.
const REMOVAL_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(3);

/// Pure: is `since_anchor` (time since startup or last activation) inside the
/// grace window.
pub fn in_grace(since_anchor: std::time::Duration) -> bool {
    since_anchor < STARTUP_GRACE
}

/// Backend hook: the session became active (true) or was paused (false).
pub fn session_state(state: &mut AbyssState, active: bool) {
    state.lid.session_active = active;
    if active {
        state.lid.activated = Some(std::time::Instant::now());
    }
}

/// What a lid event does to the internal panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LidDecision {
    DisableInternal,
    EnableInternal,
    Suspend,
    Nothing,
}

/// Pure decision for a lid event: `action` is the output's `lid-close` value.
pub fn lid_action(action: &str, closed: bool, other_enabled: bool) -> LidDecision {
    match (action, closed) {
        ("ignore", _) => LidDecision::Nothing,
        ("suspend", true) => LidDecision::Suspend,
        ("suspend", false) => LidDecision::Nothing,
        (_, true) if other_enabled => LidDecision::DisableInternal,
        (_, true) => LidDecision::Suspend,
        (_, false) => LidDecision::EnableInternal,
    }
}

/// Whether a removal leaving only the internal panel suspends instead of
/// lighting it: lid closed, panel disabled, `off` action, nothing else
/// enabled, and the session is on screen. Re-evaluated when the debounce fires.
pub fn suspend_on_removal(
    action: &str,
    closed: bool,
    internal_disabled: bool,
    other_enabled: bool,
    session_active: bool,
) -> bool {
    closed && internal_disabled && !other_enabled && session_active && !matches!(action, "ignore" | "suspend")
}

fn lid_action_for(state: &AbyssState, id: u64) -> String {
    state
        .outputs
        .get(id)
        .map(|e| state.config.output_rule(&e.connector, &e.identity))
        .and_then(|r| r.lid_close)
        .unwrap_or_else(|| "off".to_string())
}

fn request_suspend(state: &mut AbyssState) {
    if state.lid.suspend_sent {
        return;
    }
    state.lid.suspend_sent = true;
    #[cfg(not(test))]
    crate::shell::spawn_with(&[], "systemctl suspend");
}

/// Evaluate the removal rule against the state as it is now.
fn removal_due(state: &AbyssState) -> bool {
    let Some(id) = internal_output(state) else {
        return false;
    };
    let Some(entry) = state.outputs.get(id) else {
        return false;
    };
    suspend_on_removal(
        &lid_action_for(state, id),
        state.lid.closed,
        !entry.enabled,
        state.outputs.other_enabled(id),
        state.lid.session_active,
    )
}

/// Called when an output is removed, before the rescue path would light the
/// internal panel. Returns true when the panel must stay dark for now: a
/// one-shot timer re-checks the rule after [`REMOVAL_DEBOUNCE`] and either
/// suspends or lights the panel.
pub fn removal_suspend(state: &mut AbyssState) -> bool {
    if !removal_due(state) {
        return false;
    }
    if state.lid.timer.is_some() {
        return true;
    }
    use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
    let armed = state
        .loop_handle
        .insert_source(Timer::from_duration(REMOVAL_DEBOUNCE), |_, _, state| {
            state.lid.timer = None;
            if removal_due(state) {
                tracing::info!("lid closed and the last other output went away, suspending");
                request_suspend(state);
            } else {
                crate::outputs::ensure_one_enabled(state);
            }
            TimeoutAction::Drop
        });
    match armed {
        Ok(token) => {
            state.lid.timer = Some(token);
            true
        }
        Err(err) => {
            tracing::warn!(%err, "arming the lid removal timer");
            false
        }
    }
}

/// Laptop lid opened or closed (COMP-01 §4.1, ADR 0108). The compositor owns
/// the lid; logind ignores it and only performs the suspend.
pub fn lid_switch(state: &mut AbyssState, closed: bool) {
    state.lid.closed = closed;
    if !closed {
        state.lid.suspend_sent = false;
    }
    let Some(id) = internal_output(state) else {
        tracing::debug!(closed, "lid switch with no internal panel, ignored");
        return;
    };
    let action = lid_action_for(state, id);
    let other = state.outputs.other_enabled(id);
    match lid_action(&action, closed, other) {
        LidDecision::Nothing => tracing::info!(closed, action, "lid switch needs no action"),
        LidDecision::Suspend => {
            let anchor = state.lid.activated.unwrap_or(state.start_time);
            if action == "off" && in_grace(anchor.elapsed()) {
                tracing::info!("lid closed at startup with no other output, keeping the panel on");
            } else {
                if action == "off" {
                    tracing::info!("lid closed with no other output, suspending");
                } else {
                    tracing::info!("lid closed, suspending");
                }
                request_suspend(state);
            }
        }
        LidDecision::DisableInternal => {
            set_enabled(state, id, false);
        }
        LidDecision::EnableInternal => {
            set_enabled(state, id, true);
        }
    }
}

#[cfg(test)]
mod lid_tests {
    use super::*;

    #[test]
    fn decision_table() {
        use LidDecision::*;
        for (a, c, o, want) in [
            ("off", true, true, DisableInternal),
            ("off", true, false, Suspend),
            ("off", false, true, EnableInternal),
            ("off", false, false, EnableInternal),
            ("suspend", true, true, Suspend),
            ("suspend", true, false, Suspend),
            ("suspend", false, true, Nothing),
            ("ignore", true, true, Nothing),
            ("ignore", true, false, Nothing),
            ("ignore", false, true, Nothing),
        ] {
            assert_eq!(lid_action(a, c, o), want, "{a} closed={c} other={o}");
        }
    }

    #[test]
    fn removal_rule_rechecked_at_expiry() {
        // (action, closed, internal_disabled, other, active)
        assert!(suspend_on_removal("off", true, true, false, true));
        assert!(
            !suspend_on_removal("off", true, true, true, true),
            "external came back"
        );
        assert!(!suspend_on_removal("off", false, true, false, true), "lid opened");
        assert!(
            !suspend_on_removal("off", true, false, false, true),
            "panel already lit"
        );
        assert!(
            !suspend_on_removal("off", true, true, false, false),
            "session inactive"
        );
        assert!(!suspend_on_removal("ignore", true, true, false, true));
        assert!(!suspend_on_removal("suspend", true, true, false, true));
    }

    #[test]
    fn grace_window() {
        use std::time::Duration;
        assert!(in_grace(Duration::from_secs(0)));
        assert!(in_grace(Duration::from_secs(19)));
        assert!(!in_grace(Duration::from_secs(20)));
    }

    #[test]
    fn default_lid_state_is_active_and_open() {
        let l = LidState::default();
        assert!(l.session_active && !l.closed && l.activated.is_none() && l.timer.is_none());
    }
}
