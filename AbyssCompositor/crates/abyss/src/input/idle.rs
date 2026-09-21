// SPDX-License-Identifier: AGPL-3.0-only
//! Idle tracking (COMP-03 §7).
//!
//! One second-resolution calloop timer owns the idle clock; the input hot path
//! only stamps an `Instant` and pokes `ext_idle_notify_v1`. Nothing about the
//! input itself is recorded — the tracker never sees a keysym or a button.

use std::time::{Duration, Instant};

use smithay::reexports::{
    calloop::{
        timer::{TimeoutAction, Timer},
        LoopHandle,
    },
    wayland_server::{protocol::wl_surface::WlSurface, Resource},
};

use crate::state::AbyssState;

/// How often the idle clock is examined. Timeouts are configured in seconds,
/// so this is as precise as it needs to be.
const TICK: Duration = Duration::from_secs(1);

pub struct IdleTracker {
    last_activity: Instant,
    /// Surfaces holding a `zwp_idle_inhibitor_v1`.
    inhibitors: Vec<WlSurface>,
    /// Control-socket connections holding an idle inhibit (`set_idle_inhibit`).
    /// Owned by the connection, like an annotation: the disconnect path is the
    /// whole lifetime story, so a crashed holder cannot pin the screen on.
    conn_inhibitors: Vec<u64>,
    /// Outputs are currently powered off by the DPMS timeout.
    dpms_off: bool,
    /// The lock command has already been run for this idle period.
    lock_spawned: bool,
    /// The session has been idle past `bar.idle-seconds`. A latch, not a
    /// derived value: the bar is told on the flip, and only on the flip.
    bar_idle: bool,
}

impl Default for IdleTracker {
    fn default() -> Self {
        Self {
            last_activity: Instant::now(),
            inhibitors: Vec::new(),
            conn_inhibitors: Vec::new(),
            dpms_off: false,
            lock_spawned: false,
            bar_idle: false,
        }
    }
}

impl IdleTracker {
    /// Read by `emit_output_state`, which has no other view of the clock.
    pub fn bar_idle(&self) -> bool {
        self.bar_idle
    }

    pub fn add_inhibitor(&mut self, surface: WlSurface) {
        self.inhibitors.push(surface);
    }

    /// A `windowrule "idle-inhibit"` inhibitor. Idempotent, because rules are
    /// re-evaluated on every title change and the window holds no object to
    /// destroy — the entry falls out when the surface dies or unmaps.
    pub fn add_rule_inhibitor(&mut self, surface: WlSurface) {
        if !self.inhibitors.contains(&surface) {
            self.inhibitors.push(surface);
        }
    }

    pub fn remove_inhibitor(&mut self, surface: &WlSurface) {
        self.inhibitors.retain(|s| s != surface);
    }

    /// Set or clear the idle inhibit held by a control-socket connection.
    /// Idempotent both ways: the caller states the level, not an edge.
    pub fn set_conn_inhibit(&mut self, conn: u64, inhibit: bool) {
        self.conn_inhibitors.retain(|c| *c != conn);
        if inhibit {
            self.conn_inhibitors.push(conn);
        }
    }

    /// A connection went away; whatever it held goes with it.
    pub fn clear_conn_inhibit(&mut self, conn: u64) {
        self.conn_inhibitors.retain(|c| *c != conn);
    }
}

/// Arm the idle clock.
///
/// Unconditional since `bar.fold-when-idle` joined the tick's consumers: that
/// key is `Live`, so the timer cannot be armed from whatever the config said at
/// startup — it would stay dormant for the rest of the session after a reload
/// turned the key on. One wakeup a second that mostly compares two `Instant`s
/// is cheaper than a second timer that has to be armed and disarmed correctly.
pub fn start(_state: &mut AbyssState, handle: &LoopHandle<'static, AbyssState>) {
    if let Err(err) = handle.insert_source(Timer::from_duration(TICK), |_, _, state| {
        tick(state);
        TimeoutAction::ToDuration(TICK)
    }) {
        tracing::warn!(%err, "arming the idle timer");
    }
}

/// Called from the input path for every human event.
pub fn on_activity(state: &mut AbyssState) {
    state.idle.last_activity = Instant::now();
    state.idle.lock_spawned = false;
    if state.idle.bar_idle {
        // The unfold-on-any-input half of the bar's fold policy. Immediate and
        // unconditional: the human is already reaching for the bar.
        state.idle.bar_idle = false;
        crate::shell::focus::emit_focused_output_state(state);
    }
    let seat = state.seat.clone();
    state.idle_notifier.notify_activity(&seat);
    if state.idle.dpms_off {
        state.idle.dpms_off = false;
        tracing::info!("idle ended, powering outputs on");
        crate::outputs::power::set_all_power(state, true);
    }
}

/// True while a client holds an inhibitor on a surface that is actually mapped
/// (dead or unmapped inhibitors are ignored, as the protocol requires), or a
/// control-socket connection holds one on behalf of a client with no Wayland
/// surface to hang it on -- the `org.freedesktop.ScreenSaver` bridge.
fn inhibited(state: &mut AbyssState) -> bool {
    if !state.idle.conn_inhibitors.is_empty() {
        return true;
    }
    state.idle.inhibitors.retain(|s| s.is_alive());
    let inhibitors = std::mem::take(&mut state.idle.inhibitors);
    let held = inhibitors.iter().any(|s| {
        state
            .space
            .elements()
            .any(|w| w.toplevel().is_some_and(|t| t.wl_surface() == s))
    });
    state.idle.inhibitors = inhibitors;
    held
}

fn tick(state: &mut AbyssState) {
    let held = inhibited(state);
    state.idle_notifier.set_is_inhibited(held);
    if held {
        // An inhibitor freezes the clock rather than only suppressing the
        // notification, so DPMS and the locker are held off too.
        state.idle.last_activity = Instant::now();
        return;
    }
    let elapsed = state.idle.last_activity.elapsed();

    let want_bar_idle = state.config.bar.fold_when_idle
        && elapsed >= Duration::from_secs(state.config.bar.idle_seconds as u64);
    if want_bar_idle != state.idle.bar_idle {
        state.idle.bar_idle = want_bar_idle;
        crate::shell::focus::emit_focused_output_state(state);
    }

    if let Some(secs) = state.config.idle.dpms_timeout {
        if !state.idle.dpms_off && elapsed >= Duration::from_secs(secs) {
            state.idle.dpms_off = true;
            tracing::info!(secs, "idle dpms timeout, powering outputs off");
            crate::outputs::power::set_all_power(state, false);
        }
    }

    if let Some(secs) = state.config.idle.lock_timeout {
        if !state.idle.lock_spawned && elapsed >= Duration::from_secs(secs) {
            state.idle.lock_spawned = true;
            match state.config.idle.lock_command.clone() {
                Some(cmd) => {
                    tracing::info!(secs, "idle lock timeout, spawning locker");
                    crate::shell::spawn(&cmd);
                }
                // Self-locking without a locker would strand the human: only an
                // ext_session_lock_v1 client can unlock (ADR 0024).
                None => tracing::warn!("idle lock timeout with no lock-command configured"),
            }
        }
    }
}

/// One tick of the idle clock, for tests that need the `bar_idle` latch to flip
/// without waiting on calloop.
#[cfg(test)]
pub fn tick_for_test(state: &mut AbyssState) {
    tick(state);
}

#[cfg(test)]
mod tests {
    use super::IdleTracker;

    #[test]
    fn a_connection_holds_one_inhibit_at_a_time_and_releases_it() {
        let mut idle = IdleTracker::default();
        idle.set_conn_inhibit(7, true);
        idle.set_conn_inhibit(7, true);
        assert_eq!(idle.conn_inhibitors, [7], "setting the level twice is one hold");
        idle.set_conn_inhibit(9, true);
        idle.set_conn_inhibit(7, false);
        assert_eq!(idle.conn_inhibitors, [9]);
        idle.clear_conn_inhibit(9);
        assert!(
            idle.conn_inhibitors.is_empty(),
            "a dead connection releases its hold"
        );
    }
}
