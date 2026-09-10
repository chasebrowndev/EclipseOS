// SPDX-License-Identifier: AGPL-3.0-only
use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    delegate_compositor, delegate_output,
    reexports::wayland_server::{protocol::wl_surface::WlSurface, Client},
    wayland::compositor::{
        get_parent, is_sync_subsurface, CompositorClientState, CompositorHandler, CompositorState,
    },
};

use smithay::wayland::output::OutputHandler;

use crate::state::{AbyssState, ClientState};

impl CompositorHandler for AbyssState {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        // XWayland connects as a client of its own, carrying smithay's data
        // rather than ours.
        if let Some(state) = client.get_data::<smithay::xwayland::XWaylandClientData>() {
            return &state.compositor_state;
        }
        &client
            .get_data::<ClientState>()
            .expect("every client carries one of the two client datas")
            .compositor_state
    }

    #[cfg(feature = "drm")]
    fn new_surface(&mut self, surface: &WlSurface) {
        // Explicit sync: acquire points gate the transaction, never the loop.
        let id =
            smithay::wayland::compositor::add_pre_commit_hook::<Self, _>(surface, |state, _dh, surface| {
                super::drm_syncobj::block_on_acquire_point(state, surface);
            });
        let _ = id;
    }

    fn destroyed(&mut self, surface: &WlSurface) {
        // A touch point whose target dies must still be lifted: the client is
        // owed an `up` for every id it saw come down (COMP-04 §6).
        if !self.touch_points.is_empty() {
            let time = self.start_time.elapsed().as_millis() as u32;
            self.release_touch_on(surface, time);
        }
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            let window = self
                .space
                .elements()
                .find(|w| crate::shell::window_surface(w).as_ref() == Some(&root))
                .cloned();
            if let Some(window) = window {
                window.on_commit();
                // The commit may have grown the surface tree past the root's
                // own bounds, which moves the window geometry origin the space
                // pins the element by.
                crate::shell::reanchor(self, &window);
            }
        }
        crate::shell::handle_commit(self, surface);
    }
}

impl OutputHandler for AbyssState {}

delegate_compositor!(AbyssState);
delegate_output!(AbyssState);
