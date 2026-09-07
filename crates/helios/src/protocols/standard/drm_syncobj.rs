// SPDX-License-Identifier: AGPL-3.0-only
//! `wp_linux_drm_syncobj_v1` (COMP-06 §1, COMP-02 §3): explicit sync.
//!
//! COMP-02 §3 mandates explicit sync with no implicit-sync fallback path in
//! the compositor. Clients hand us an acquire timeline point per commit; we
//! turn it into a calloop-backed blocker so the surface transaction only
//! applies once the GPU work behind the buffer has actually landed, and the
//! compositor never stalls waiting on it. Release points are signalled by
//! smithay when the last reference to the buffer is dropped.
//!
//! The global only exists when the render device supports syncobj eventfds;
//! see [`crate::backend::drm`].

use smithay::{
    delegate_drm_syncobj,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    wayland::{
        compositor::{add_blocker, with_states},
        drm_syncobj::{DrmSyncobjCachedState, DrmSyncobjHandler, DrmSyncobjState},
    },
};

use crate::state::HeliosState;
use smithay::reexports::wayland_server::Resource as _;
use smithay::wayland::compositor::CompositorHandler as _;

impl DrmSyncobjHandler for HeliosState {
    fn drm_syncobj_state(&mut self) -> Option<&mut DrmSyncobjState> {
        self.syncobj_state.as_mut()
    }
}

delegate_drm_syncobj!(HeliosState);

/// Pre-commit hook: if this commit carries an acquire point, block the
/// transaction on it instead of waiting inline.
///
/// A client that never signals its acquire point holds only its own surface
/// back — the compositor keeps compositing the previous buffer (COMP-02 §3).
pub fn block_on_acquire_point(state: &mut HeliosState, surface: &WlSurface) {
    let acquire = with_states(surface, |states| {
        states
            .cached_state
            .get::<DrmSyncobjCachedState>()
            .pending()
            .acquire_point
            .clone()
    });
    let Some(acquire) = acquire else { return };
    let (blocker, source) = match acquire.generate_blocker() {
        Ok(pair) => pair,
        Err(err) => {
            tracing::warn!(?err, "generating syncobj blocker; committing unblocked");
            return;
        }
    };
    let Some(client) = surface.client() else { return };
    let inserted = state.loop_handle.insert_source(source, move |_, _, state| {
        let dh = state.display_handle.clone();
        state.client_compositor_state(&client).blocker_cleared(state, &dh);
        Ok(())
    });
    match inserted {
        Ok(_) => add_blocker(surface, blocker),
        Err(err) => tracing::warn!(?err, "inserting syncobj source; committing unblocked"),
    }
}
