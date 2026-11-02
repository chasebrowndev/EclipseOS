// SPDX-License-Identifier: AGPL-3.0-only

//! D-Bus errors under `org.freedesktop.portal.Error.*`.

use ecl_portal_store::StoreError;

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.portal.Error")]
pub enum PortalError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Failed(String),
    InvalidArgument(String),
    NotFound(String),
    Exists(String),
    NotAllowed(String),
    NotSupported(String),
}

impl From<StoreError> for PortalError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::InvalidName => PortalError::InvalidArgument(e.to_string()),
            StoreError::NotFound => PortalError::NotFound(e.to_string()),
            StoreError::Exists => PortalError::Exists(e.to_string()),
            StoreError::Io(_) | StoreError::Encode(_) => PortalError::Failed(e.to_string()),
        }
    }
}

impl From<ecl_portal_core::request::TokenError> for PortalError {
    fn from(e: ecl_portal_core::request::TokenError) -> Self {
        PortalError::InvalidArgument(e.to_string())
    }
}
