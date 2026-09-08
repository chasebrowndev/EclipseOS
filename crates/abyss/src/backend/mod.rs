// SPDX-License-Identifier: AGPL-3.0-only
//! Backends (COMP-01 §3): `winit` for nested development, `drm` for
//! production (M2), `headless` for CI (M9).

#[cfg(feature = "drm")]
pub mod drm;
#[cfg(feature = "winit")]
pub mod winit;

use crate::state::AbyssState;

/// Pixel format vocabulary. Re-exported so nothing outside `backend/` has to
/// name a `smithay::backend::` path (root invariant: backends live behind this
/// module).
pub use smithay::backend::allocator::Fourcc;

/// What every backend can do to itself.
///
/// Operations that need the whole compositor (damage, output power) stay as
/// free functions below: the backend is owned *by* [`AbyssState`], so it
/// cannot take a second mutable borrow of it. What is left is the per-backend
/// work — importing a client buffer into whichever renderer is live, and
/// handing the session back to another VT — and that is what this trait is.
pub trait Backend {
    /// Import a client dmabuf into this backend's renderer. `false` means the
    /// buffer is unusable and the client must be told so (COMP-02 §2).
    fn import_dmabuf(&mut self, buf: &smithay::backend::allocator::dmabuf::Dmabuf) -> bool;

    /// Switch to another virtual terminal. Only the DRM backend owns a
    /// session, so the default is to log and ignore.
    fn change_vt(&mut self, vt: i32) {
        tracing::debug!(vt, "VT switch ignored (backend owns no session)");
    }
}

/// Mark every output as needing a fresh composite. Used whenever compositor
/// state changes outside the normal damage path (lock, unlock, DPMS).
pub fn damage_all(state: &mut AbyssState) {
    #[cfg(feature = "drm")]
    if state.drm.is_some() {
        drm::schedule_render(state);
        return;
    }
    // The nested backend redraws only when the host hands it a frame callback,
    // and a host that has the abyss window occluded hands it none. Anything
    // queued behind this call — a screencopy `copy` in particular — would wait
    // forever on a frame that is not coming, so service it from an idle.
    #[cfg(feature = "winit")]
    if state.winit.is_some() {
        let _ = state.loop_handle.insert_idle(winit::service_captures);
    }
    let _ = state;
}

/// Turn one output's scanout on or off (DPMS, COMP-03 §7). Geometry, windows
/// and workspaces are untouched — only the pixels stop.
pub fn set_output_power(state: &mut AbyssState, id: u64, on: bool) {
    #[cfg(feature = "drm")]
    drm::set_power(state, id, on);
    let _ = (state, id, on);
}

/// Ask one output for adaptive sync (VRR, COMP-13 §2.1). `false` means the
/// backend cannot do it — no DRM device, unknown output, or a connector that
/// does not advertise support — and nothing was changed.
pub fn set_output_vrr(state: &mut AbyssState, id: u64, on: bool) -> bool {
    #[cfg(feature = "drm")]
    return drm::set_vrr(state, id, on);
    #[cfg(not(feature = "drm"))]
    {
        let _ = (state, id, on);
        false
    }
}
