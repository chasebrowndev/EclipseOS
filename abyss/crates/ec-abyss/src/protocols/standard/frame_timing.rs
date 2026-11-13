// SPDX-License-Identifier: AGPL-3.0-only
//! `wp_fifo_v1` and `wp_commit_timing_v1` (COMP-06 §1, T-17 §8), both in
//! smithay's managed mode: smithay parks a blocker on a surface commit that
//! waits on a fifo barrier or a commit-timer timestamp, and the compositor's
//! one job is to release them.
//!
//! [`release`] runs after every presented frame, next to `send_frames`:
//!
//! - fifo: the barrier a latched commit set is signalled, so the commit that
//!   waits on it applies for the next frame (one content update per refresh);
//! - commit timing: every commit whose target is no later than the next
//!   presentation (now plus one refresh) is released. A target further out
//!   arms a one-shot timer that wakes the loop for a frame at that moment, so
//!   an idle output cannot sleep through a client's deadline.
//!
//! Released clients get `blocker_cleared` and, only when something was
//! released, one redraw so the commit applied there is not left waiting for
//! unrelated activity (the same reason `drm_syncobj` does it). No surface that
//! uses neither protocol is touched beyond a map lookup; the loop never blocks.

use std::time::Duration;

use smithay::{
    delegate_commit_timing, delegate_fifo,
    desktop::{layer_map_for_output, utils::with_surfaces_surface_tree, Window},
    output::Output,
    reexports::{
        calloop::timer::{TimeoutAction, Timer},
        wayland_server::{backend::ClientId, protocol::wl_surface::WlSurface, Client, Resource},
    },
    utils::{Monotonic, Time},
    wayland::{
        commit_timing::{CommitTimerBarrierStateUserData, Timestamp},
        compositor::{CompositorHandler, SurfaceData},
        fifo::FifoBarrierCachedState,
    },
};

use crate::state::AbyssState;

delegate_fifo!(AbyssState);
delegate_commit_timing!(AbyssState);

/// Refresh period used when an output has no mode (nested, headless).
const DEFAULT_PERIOD: Duration = Duration::from_nanos(16_666_667);

/// What one frame's sweep found.
#[derive(Default)]
struct Sweep {
    /// Clients with a surface that was released, one entry each.
    clients: Vec<Client>,
    /// Earliest commit-timer target still in the future.
    next: Option<Timestamp>,
}

impl Sweep {
    fn note_client(&mut self, surface: &WlSurface) {
        if let Some(c) = surface.client() {
            let id: ClientId = c.id();
            if !self.clients.iter().any(|k| k.id() == id) {
                self.clients.push(c);
            }
        }
    }

    fn surface(&mut self, surface: &WlSurface, data: &SurfaceData, until: Timestamp) {
        let barrier = data
            .cached_state
            .get::<FifoBarrierCachedState>()
            .current()
            .barrier
            .take();
        let mut released = false;
        if let Some(b) = barrier {
            b.signal();
            released = true;
        }
        if let Some(timer) = data.data_map.get::<CommitTimerBarrierStateUserData>() {
            let mut t = timer.lock().unwrap();
            released |= t.signal_until(until);
            if let Some(d) = t.next_deadline() {
                self.next = Some(self.next.map_or(d, |n| n.min(d)));
            }
        }
        if released {
            self.note_client(surface);
        }
    }
}

/// Release fifo barriers and due commit timers after `output` presented a frame.
pub fn release(state: &mut AbyssState, output: &Output) {
    let period = output
        .current_mode()
        .filter(|m| m.refresh > 0)
        .map_or(DEFAULT_PERIOD, |m| {
            Duration::from_nanos(1_000_000_000_000 / m.refresh as u64)
        });
    let now: Time<Monotonic> = state.clock.now();
    let until = Timestamp::from(now + period);

    let mut sweep = Sweep::default();
    for window in state.space.elements() {
        sweep_window(window, &mut sweep, until);
    }
    for layer in layer_map_for_output(output).layers() {
        with_surfaces_surface_tree(layer.wl_surface(), |s, d| sweep.surface(s, d, until));
    }

    if let Some(next) = sweep.next {
        arm_wakeup(state, now, next);
    }
    if sweep.clients.is_empty() {
        return;
    }
    let dh = state.display_handle.clone();
    for client in &sweep.clients {
        state.client_compositor_state(client).blocker_cleared(state, &dh);
    }
    crate::backend::damage_all(state);
}

fn sweep_window(window: &Window, sweep: &mut Sweep, until: Timestamp) {
    window.with_surfaces(|s, d| sweep.surface(s, d, until));
}

/// One-shot wake-up for the earliest pending commit timer.
fn arm_wakeup(state: &mut AbyssState, now: Time<Monotonic>, next: Timestamp) {
    if state.timing_wakeup_armed {
        return;
    }
    let target: Time<Monotonic> = next.into();
    let delay = Time::elapsed(&now, target).max(Duration::from_millis(1));
    state.timing_wakeup_armed = true;
    let armed = state
        .loop_handle
        .insert_source(Timer::from_duration(delay), |_, _, state| {
            state.timing_wakeup_armed = false;
            crate::backend::damage_all(state);
            TimeoutAction::Drop
        });
    if armed.is_err() {
        state.timing_wakeup_armed = false;
    }
}
