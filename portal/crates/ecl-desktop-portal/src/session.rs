// SPDX-License-Identifier: AGPL-3.0-only

//! `org.freedesktop.portal.Session` objects (ScreenCast, Inhibit monitors).

use std::collections::HashMap;
use std::sync::Arc;

use ecl_portal_core::options::Options;
use ecl_portal_core::request::session_path;
use zbus::message::Header;
use zbus::{Connection, OwnedVariant};

use crate::error::PortalError;
use crate::state::{lock, SessionKind, Shared};

pub struct SessionObj {
    shared: Arc<Shared>,
    path: String,
    owner: String,
}

#[zbus::interface(name = "org.freedesktop.portal.Session")]
impl SessionObj {
    async fn close(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), PortalError> {
        if hdr.sender().map(|s| s.as_str()) != Some(self.owner.as_str()) {
            return Err(PortalError::NotAllowed(
                "only the creating caller may close a session".into(),
            ));
        }
        close(&self.shared, conn, &self.path).await;
        Ok(())
    }

    #[zbus(property, name = "version")]
    async fn version(&self) -> u32 {
        1
    }
}

/// Create and export a session for `sender`; returns its path.
pub async fn create(
    shared: &Arc<Shared>,
    conn: &Connection,
    sender: &str,
    opts: &Options,
    kind: SessionKind,
) -> Result<String, PortalError> {
    let token = match opts.session_token()? {
        Some(t) => t.to_owned(),
        None => ecl_portal_core::request::generated_token(shared.next_id()),
    };
    let path = session_path(sender, &token)?;
    lock(&shared.sessions)
        .insert(path.clone(), sender, kind)
        .map_err(|_| PortalError::InvalidArgument("session token already in use".into()))?;
    let obj = SessionObj {
        shared: shared.clone(),
        path: path.clone(),
        owner: sender.to_owned(),
    };
    match conn.object_server().at(path.as_str(), obj).await {
        Ok(None) => Ok(path),
        other => {
            lock(&shared.sessions).remove(&path);
            match other {
                Err(e) => Err(e.into()),
                _ => Err(PortalError::InvalidArgument("session path exists".into())),
            }
        }
    }
}

/// Tear a session down: table entry, object, `Closed` signal, backend state.
/// Idempotent.
pub async fn close(shared: &Arc<Shared>, conn: &Connection, path: &str) {
    let owner = lock(&shared.sessions).owner(path).map(str::to_owned);
    let kind = lock(&shared.sessions).remove(path);
    let (Some(owner), Some(kind)) = (owner, kind) else {
        return;
    };
    let _ = conn.object_server().remove::<SessionObj, _>(path).await;
    let details: HashMap<String, OwnedVariant> = HashMap::new();
    let _ = conn
        .emit_signal(
            Some(owner.as_str()),
            path,
            "org.freedesktop.portal.Session",
            "Closed",
            &(details,),
        )
        .await;
    if kind == SessionKind::ScreenCast {
        shared.backend.close_session(path).await;
    }
}
