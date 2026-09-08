// SPDX-License-Identifier: AGPL-3.0-only
//! `zwp_linux_dmabuf_v1` v5 (F-04 §2, COMP-02 §2): the expected client buffer
//! path. The global only exists when a render node does; per-surface feedback
//! promotes a full-screen surface to the output's scanout tranche.

use smithay::{
    backend::allocator::dmabuf::Dmabuf,
    delegate_dmabuf,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    wayland::dmabuf::{DmabufFeedback, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
};

use crate::state::AbyssState;

impl DmabufHandler for AbyssState {
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
        let imported = self
            .backend_mut()
            .is_some_and(|backend| backend.import_dmabuf(&dmabuf));
        if imported {
            let _ = notifier.successful::<AbyssState>();
        } else {
            tracing::warn!("rejecting client dmabuf: import failed");
            notifier.failed();
        }
    }
}

delegate_dmabuf!(AbyssState);
