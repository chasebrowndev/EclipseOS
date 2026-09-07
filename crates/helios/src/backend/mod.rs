// SPDX-License-Identifier: AGPL-3.0-only
//! Backends (COMP-01 §3): `winit` for nested development, `drm` for
//! production (M2), `headless` for CI (M9).

#[cfg(feature = "drm")]
pub mod drm;
#[cfg(feature = "winit")]
pub mod winit;

use crate::state::HeliosState;

/// Mark every output as needing a fresh composite. Used whenever compositor
/// state changes outside the normal damage path (lock, unlock, DPMS).
pub fn damage_all(state: &mut HeliosState) {
    // winit drives its own continuous redraw, so it needs nothing here.
    #[cfg(feature = "drm")]
    if state.drm.is_some() {
        drm::schedule_render(state);
    }
    let _ = state;
}

/// Turn one output's scanout on or off (DPMS, COMP-03 §7). Geometry, windows
/// and workspaces are untouched — only the pixels stop.
pub fn set_output_power(state: &mut HeliosState, id: u64, on: bool) {
    #[cfg(feature = "drm")]
    drm::set_power(state, id, on);
    let _ = (state, id, on);
}
