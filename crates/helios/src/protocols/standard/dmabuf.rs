// SPDX-License-Identifier: AGPL-3.0-only
//! `zwp_linux_dmabuf_v1` v5 (F-04 §2, COMP-02 §2): the expected client buffer
//! path. The global only exists when a render node does; per-surface feedback
//! promotes a full-screen surface to the output's scanout tranche.

use smithay::{
    backend::{allocator::dmabuf::Dmabuf, renderer::ImportDma},
    delegate_dmabuf,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    wayland::dmabuf::{DmabufFeedback, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
};

use crate::state::HeliosState;

impl DmabufHandler for HeliosState {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        self.dmabuf_state
            .as_mut()
            .expect("dmabuf global only exists once DmabufState is set")
    }

    fn new_surface_feedback(
        &mut self,
        _surface: &WlSurface,
        _global: &DmabufGlobal,
    ) -> Option<DmabufFeedback> {
        // Start every surface on the render tranche; the frame loop promotes a
        // scanout candidate once it actually covers an output.
        self.dmabuf_feedback.clone()
    }

    fn dmabuf_imported(&mut self, _global: &DmabufGlobal, dmabuf: Dmabuf, notifier: ImportNotifier) {
        #[allow(unused_mut)]
        let mut imported = false;
        #[cfg(feature = "drm")]
        if let Some(drm) = self.drm.as_mut() {
            imported = drm.renderer.import_dmabuf(&dmabuf, None).is_ok();
        }
        #[cfg(feature = "winit")]
        if !imported {
            if let Some(winit) = self.winit.as_mut() {
                imported = winit.renderer().import_dmabuf(&dmabuf, None).is_ok();
            }
        }
        if imported {
            let _ = notifier.successful::<HeliosState>();
        } else {
            tracing::warn!("rejecting client dmabuf: import failed");
            notifier.failed();
        }
    }
}

delegate_dmabuf!(HeliosState);
