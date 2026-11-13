// SPDX-License-Identifier: AGPL-3.0-only
//! `xdg_system_bell_v1` (COMP-06 §1, T-17 §8).
//!
//! A ring is a request for the human's attention, never an action on anything:
//! it marks the ringing window urgent (the same flag `xdg_activation` sets for
//! a refused focus steal, so a background bell cannot move focus) and emits a
//! `window` event, `change: "bell"`, for the shell to play or show. abyss makes
//! no sound itself. A surface-less ring belongs to the client as a whole; with
//! no window to name it is only counted in the trace log.

use std::time::{Duration, Instant};

use smithay::{
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    wayland::xdg_system_bell::XdgSystemBellHandler,
};

use crate::state::AbyssState;

/// A client ringing in a loop must not become an event flood on the socket.
const MIN_INTERVAL: Duration = Duration::from_millis(50);

/// Spaces rings out by [`MIN_INTERVAL`].
#[derive(Default)]
pub struct BellLimiter {
    last: Option<Instant>,
}

impl BellLimiter {
    /// True when a ring at `now` is not too close to the previous one.
    pub fn admit(&mut self, now: Instant) -> bool {
        if self.last.is_some_and(|l| now.duration_since(l) < MIN_INTERVAL) {
            return false;
        }
        self.last = Some(now);
        true
    }
}

impl XdgSystemBellHandler for AbyssState {
    fn ring(&mut self, surface: Option<WlSurface>) {
        if !self.bell_limit.admit(Instant::now()) {
            return;
        }
        let Some(window) = surface
            .as_ref()
            .and_then(|s| crate::shell::window_for_surface(self, s))
        else {
            tracing::debug!("system bell without a window");
            return;
        };
        crate::shell::mark_urgent(self, &window);
        let handle = self.ipc.handle_for(&window);
        crate::ipc::emit(
            self,
            "window",
            serde_json::json!({"change": "bell", "handle": handle}),
        );
    }
}

smithay::delegate_xdg_system_bell!(AbyssState);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rapid_rings_are_coalesced() {
        let mut s = BellLimiter::default();
        let t = Instant::now();
        assert!(s.admit(t));
        assert!(!s.admit(t + Duration::from_millis(10)));
        assert!(s.admit(t + Duration::from_millis(60)));
    }
}
