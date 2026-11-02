// SPDX-License-Identifier: AGPL-3.0-only

//! FileChooser: `org.freedesktop.portal.FileChooser` (frontend) and
//! `org.freedesktop.impl.portal.FileChooser` (backend, kept for upstream
//! frontends), both driving Fog's picker (`ec-fog-ui --picker`, see
//! `ecl_portal_core::filechooser::PICKER_BIN`).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use ecl_portal_core::filechooser::{
    picker_outcome, reply, response, save_files_names, save_files_targets, uri_to_path,
    ChooserRequest, PICKER_BIN,
};
use ecl_portal_core::options::Options;
use ecl_portal_core::request::{Reply, Response, Results};
use tokio::io::AsyncReadExt;
use tokio::net::UnixStream;
use zbus::message::Header;
use zbus::{Connection, ObjectPath, OwnedObjectPath, OwnedVariant};

use crate::convert::{results_map, to_options, WireDict};
use crate::error::PortalError;
use crate::req::{self, Outcome};
use crate::spawn::spawn_with_fd;
use crate::state::Shared;

/// Run the picker for `req`. Dropping the future kills the picker.
pub async fn run_picker(req: &ChooserRequest) -> (Response, Vec<String>) {
    let spawned = spawn_with_fd(PICKER_BIN, |fd| req.picker_args(fd));
    let (mut child, mine) = match spawned {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("cannot start {PICKER_BIN}: {e}");
            return (Response::Other, Vec::new());
        }
    };
    let Ok(mut sock) = UnixStream::from_std(mine) else {
        return (Response::Other, Vec::new());
    };
    let reader = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = sock.read_to_end(&mut buf).await;
        buf
    });
    let status = child.wait().await;
    let out = tokio::time::timeout(Duration::from_secs(2), reader)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    picker_outcome(status.ok().and_then(|s| s.code()), &out)
}

/// `SaveFiles`: pick a folder, then place each requested name under it.
async fn save_files(title: &str, o: &Options) -> Reply {
    let Some(names) = save_files_names(o) else {
        return Reply::other();
    };
    let (code, folders) = run_picker(&ChooserRequest::folder_for_save_files(title, o)).await;
    if code != Response::Success {
        return Reply {
            response: code,
            results: Results::new(),
        };
    }
    let Some(folder) = folders.first().and_then(|u| uri_to_path(u)) else {
        return Reply::cancelled();
    };
    match save_files_targets(&folder, &names) {
        Some(t) => reply(response(&t)),
        None => Reply::other(),
    }
}

pub struct FileChooserSvc(pub Arc<Shared>);

#[zbus::interface(name = "org.freedesktop.portal.FileChooser")]
impl FileChooserSvc {
    async fn open_file(
        &self,
        _parent_window: &str,
        title: String,
        options: WireDict,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<OwnedObjectPath, PortalError> {
        let o = to_options(&options);
        let cr = ChooserRequest::from_options(&title, false, &o);
        req::start(&self.0, conn, &hdr, &o, move |_| async move {
            Outcome::Reply(reply(run_picker(&cr).await))
        })
        .await
    }

    async fn save_file(
        &self,
        _parent_window: &str,
        title: String,
        options: WireDict,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<OwnedObjectPath, PortalError> {
        let o = to_options(&options);
        let cr = ChooserRequest::from_options(&title, true, &o);
        req::start(&self.0, conn, &hdr, &o, move |_| async move {
            Outcome::Reply(reply(run_picker(&cr).await))
        })
        .await
    }

    async fn save_files(
        &self,
        _parent_window: &str,
        title: String,
        options: WireDict,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<OwnedObjectPath, PortalError> {
        let o = to_options(&options);
        let o2 = o.clone();
        req::start(&self.0, conn, &hdr, &o, move |_| async move {
            Outcome::Reply(save_files(&title, &o2).await)
        })
        .await
    }

    #[zbus(property, name = "version")]
    async fn version(&self) -> u32 {
        4
    }
}

/// Backend flavour (`org.freedesktop.impl.portal.FileChooser`): answers
/// directly, with no Request object (Close is not supported here).
pub struct ImplFileChooser;

fn wire(r: Reply) -> (u32, WireDict) {
    (r.response.code(), results_map(&r.results))
}

#[zbus::interface(name = "org.freedesktop.impl.portal.FileChooser")]
impl ImplFileChooser {
    async fn open_file(
        &self,
        _handle: ObjectPath<'_>,
        _app_id: &str,
        _parent_window: &str,
        title: &str,
        options: HashMap<String, OwnedVariant>,
    ) -> (u32, WireDict) {
        let o = to_options(&options);
        wire(reply(
            run_picker(&ChooserRequest::from_options(title, false, &o)).await,
        ))
    }

    async fn save_file(
        &self,
        _handle: ObjectPath<'_>,
        _app_id: &str,
        _parent_window: &str,
        title: &str,
        options: HashMap<String, OwnedVariant>,
    ) -> (u32, WireDict) {
        let o = to_options(&options);
        wire(reply(
            run_picker(&ChooserRequest::from_options(title, true, &o)).await,
        ))
    }

    async fn save_files(
        &self,
        _handle: ObjectPath<'_>,
        _app_id: &str,
        _parent_window: &str,
        title: &str,
        options: HashMap<String, OwnedVariant>,
    ) -> (u32, WireDict) {
        wire(save_files(title, &to_options(&options)).await)
    }
}
