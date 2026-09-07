// SPDX-License-Identifier: AGPL-3.0-only
//! `zwp_idle_inhibit_manager_v1` (COMP-03 §7).
//!
//! An inhibitor only counts while its surface is mapped; the check lives in
//! [`crate::input::idle`], which runs it on every tick.

use smithay::{
    delegate_idle_inhibit, reexports::wayland_server::protocol::wl_surface::WlSurface,
    wayland::idle_inhibit::IdleInhibitHandler,
};

use crate::state::HeliosState;

impl IdleInhibitHandler for HeliosState {
    fn inhibit(&mut self, surface: WlSurface) {
        tracing::debug!("idle inhibitor created");
        self.idle.add_inhibitor(surface);
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        tracing::debug!("idle inhibitor destroyed");
        self.idle.remove_inhibitor(&surface);
    }
}

delegate_idle_inhibit!(HeliosState);
