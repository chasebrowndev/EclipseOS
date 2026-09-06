// SPDX-License-Identifier: AGPL-3.0-only
//! `zwp_linux_dmabuf_v1` (F-04 §2): the expected client buffer path. Only
//! wired up on the DRM backend, where a GLES renderer exists to import into.

use smithay::{
    backend::{allocator::dmabuf::Dmabuf, renderer::ImportDma},
    delegate_dmabuf,
    wayland::dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
};

use crate::state::HeliosState;

impl DmabufHandler for HeliosState {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        self.dmabuf_state
            .as_mut()
            .expect("dmabuf global only exists once DmabufState is set")
    }

    fn dmabuf_imported(&mut self, _global: &DmabufGlobal, dmabuf: Dmabuf, notifier: ImportNotifier) {
        let imported = self
            .drm
            .as_mut()
            .map(|drm| drm.renderer.import_dmabuf(&dmabuf, None).is_ok())
            .unwrap_or(false);
        if imported {
            let _ = notifier.successful::<HeliosState>();
        } else {
            tracing::warn!("rejecting client dmabuf: import failed");
            notifier.failed();
        }
    }
}

delegate_dmabuf!(HeliosState);
