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

use crate::state::{ClientState, HeliosState};

impl CompositorHandler for HeliosState {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
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

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(window) = self
                .space
                .elements()
                .find(|w| w.toplevel().unwrap().wl_surface() == &root)
            {
                window.on_commit();
            }
        }
        crate::shell::handle_commit(self, surface);
    }
}

impl OutputHandler for HeliosState {}

delegate_compositor!(HeliosState);
delegate_output!(HeliosState);
