// SPDX-License-Identifier: AGPL-3.0-only

//! `org.freedesktop.portal.OpenURI`: open a URI/file/directory with the user's
//! default handler (mimeapps.list), launched directly (no `xdg-open`).

use std::os::fd::AsRawFd;
use std::sync::Arc;

use ecl_portal_core::filechooser::path_to_uri;
use ecl_portal_core::openuri::{scheme_mime, scheme_of};
use ecl_portal_core::request::Reply;
use zbus::message::Header;
use zbus::{Connection, OwnedFd, OwnedObjectPath};

use crate::apps;
use crate::convert::{to_options, WireDict};
use crate::error::PortalError;
use crate::launch;
use crate::req::{self, Outcome};
use crate::state::Shared;

pub struct OpenUriSvc(pub Arc<Shared>);

/// Absolute path behind a received fd (`/proc/self/fd/N`).
fn fd_path(fd: &OwnedFd) -> Result<String, PortalError> {
    let p = std::fs::read_link(format!("/proc/self/fd/{}", fd.as_raw_fd()))
        .map_err(|e| PortalError::InvalidArgument(format!("bad fd: {e}")))?;
    let s = p
        .to_str()
        .ok_or_else(|| PortalError::InvalidArgument("non-UTF-8 path".into()))?
        .to_owned();
    if !s.starts_with('/') {
        return Err(PortalError::InvalidArgument("fd is not a file".into()));
    }
    Ok(s)
}

fn file_candidates(path: &str) -> Vec<String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    apps::mime_of_name(name)
        .map(|m| apps::mime_apps().candidates(&m))
        .unwrap_or_default()
}

fn outcome(started: bool) -> Outcome {
    Outcome::Reply(if started {
        Reply::ok(Default::default())
    } else {
        Reply::other()
    })
}

#[zbus::interface(name = "org.freedesktop.portal.OpenURI")]
impl OpenUriSvc {
    #[zbus(name = "OpenURI")]
    async fn open_uri(
        &self,
        _parent_window: &str,
        uri: String,
        options: WireDict,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<OwnedObjectPath, PortalError> {
        let scheme = scheme_of(&uri)
            .ok_or_else(|| PortalError::InvalidArgument("not an absolute URI".into()))?
            .to_ascii_lowercase();
        let opts = to_options(&options);
        let n = self.0.next_id();
        req::start(&self.0, conn, &hdr, &opts, move |_| async move {
            let (cands, path) = if scheme == "file" {
                match ecl_portal_core::filechooser::uri_to_path(&uri)
                    .and_then(|p| String::from_utf8(p).ok())
                {
                    Some(p) => (file_candidates(&p), Some(p)),
                    None => return outcome(false),
                }
            } else {
                (apps::mime_apps().candidates(&scheme_mime(&scheme)), None)
            };
            outcome(launch::open(&cands, &uri, path.as_deref(), n).await)
        })
        .await
    }

    async fn open_file(
        &self,
        _parent_window: &str,
        fd: OwnedFd,
        options: WireDict,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<OwnedObjectPath, PortalError> {
        let path = fd_path(&fd)?;
        let opts = to_options(&options);
        let n = self.0.next_id();
        req::start(&self.0, conn, &hdr, &opts, move |_| async move {
            let _keep = fd;
            let uri = path_to_uri(path.as_bytes());
            outcome(launch::open(&file_candidates(&path), &uri, Some(&path), n).await)
        })
        .await
    }

    async fn open_directory(
        &self,
        _parent_window: &str,
        fd: OwnedFd,
        options: WireDict,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<OwnedObjectPath, PortalError> {
        let path = fd_path(&fd)?;
        let opts = to_options(&options);
        let n = self.0.next_id();
        req::start(&self.0, conn, &hdr, &opts, move |_| async move {
            let _keep = fd;
            let uri = path_to_uri(path.as_bytes());
            let cands = apps::mime_apps().candidates("inode/directory");
            outcome(launch::open(&cands, &uri, Some(&path), n).await)
        })
        .await
    }

    async fn scheme_supported(&self, scheme: &str, _options: WireDict) -> bool {
        scheme_of(&format!("{scheme}:x")).is_some()
            && !apps::mime_apps()
                .candidates(&scheme_mime(scheme))
                .is_empty()
    }

    #[zbus(property, name = "version")]
    async fn version(&self) -> u32 {
        5
    }
}
