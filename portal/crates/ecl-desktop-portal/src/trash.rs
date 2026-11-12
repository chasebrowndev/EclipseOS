// SPDX-License-Identifier: AGPL-3.0-only

//! `org.freedesktop.portal.Trash`: explicitly unsupported for now. The trash
//! operation belongs to fogd (its Trash job); this daemon has no ambient
//! authority to move user files and does not reimplement the trash spec.

use zbus::OwnedFd;

use crate::error::PortalError;

pub struct TrashSvc;

#[zbus::interface(name = "org.freedesktop.portal.Trash")]
impl TrashSvc {
    async fn trash_file(&self, _fd: OwnedFd) -> Result<u32, PortalError> {
        Err(PortalError::NotSupported(
            "Trash is not supported: the fogd trash job is not wired into the portal yet".into(),
        ))
    }

    #[zbus(property, name = "version")]
    async fn version(&self) -> u32 {
        1
    }
}
