// SPDX-License-Identifier: AGPL-3.0-only

//! Backend flavour of ScreenCast and Screenshot
//! (`org.freedesktop.impl.portal.*`), for when the upstream
//! `xdg-desktop-portal` frontend owns `org.freedesktop.portal.Desktop` and
//! routes these two interfaces to us (`portals.conf`). Same `CastBackend`
//! underneath; only the wire shape differs: no Request object, the frontend
//! passes the app id, and the session object lives on our connection.
//!
//! Only the frontend's own connection may call these: any other peer could
//! otherwise pick session paths and start a share without going through the
//! frontend's per-app bookkeeping. The chooser and abyss's confirm still gate
//! every capture either way.

use std::sync::Arc;

use ecl_portal_core::identity::{host, valid_app_id};
use ecl_portal_core::options::Options;
use ecl_portal_core::request::Reply;
use ecl_portal_core::screencast::Call;
use ecl_portal_proto::{IdentitySource, Requester};
use zbus::message::Header;
use zbus::names::BusName;
use zbus::{Connection, ObjectPath};

use crate::bus::FRONTEND_NAME;
use crate::convert::{results_map, to_options, WireDict};
use crate::state::Shared;

fn wire(r: Reply) -> (u32, WireDict) {
    (r.response.code(), results_map(&r.results))
}

fn denied() -> (u32, WireDict) {
    wire(Reply::other())
}

fn requester(app_id: &str) -> Requester {
    if app_id.is_empty() || !valid_app_id(app_id) {
        return host();
    }
    Requester {
        app_id: app_id.to_owned(),
        display_name: None,
        icon: None,
        source: IdentitySource::DesktopFile,
    }
}

/// Is `hdr`'s sender the current owner of the frontend name?
async fn from_frontend(conn: &Connection, hdr: &Header<'_>) -> bool {
    let Some(sender) = hdr.sender() else {
        return false;
    };
    let Ok(dbus) = zbus::fdo::DBusProxy::new(conn).await else {
        return false;
    };
    let Ok(name) = BusName::try_from(FRONTEND_NAME) else {
        return false;
    };
    match dbus.get_name_owner(name).await {
        Ok(owner) => owner.as_str() == sender.as_str(),
        Err(_) => false,
    }
}

fn call(app_id: &str, session: &str, parent_window: &str, options: Options) -> Call {
    Call {
        requester: requester(app_id),
        session: session.to_owned(),
        parent_window: parent_window.to_owned(),
        options,
    }
}

pub struct ImplScreenCast(pub Arc<Shared>);

pub struct ImplSession {
    shared: Arc<Shared>,
    path: String,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Session")]
impl ImplSession {
    async fn close(&self, #[zbus(connection)] conn: &Connection) {
        end_session(&self.shared, conn, &self.path).await;
    }

    #[zbus(property, name = "version")]
    async fn version(&self) -> u32 {
        1
    }
}

/// The grant behind a backend-flavour session was revoked: `Closed`, then
/// teardown.
pub async fn revoked(shared: &Arc<Shared>, conn: &Connection, path: &str) {
    let _ = conn
        .emit_signal(
            None::<&str>,
            path,
            "org.freedesktop.impl.portal.Session",
            "Closed",
            &(),
        )
        .await;
    end_session(shared, conn, path).await;
}

async fn end_session(shared: &Arc<Shared>, conn: &Connection, path: &str) {
    shared.backend.close_session(path).await;
    let _ = conn.object_server().remove::<ImplSession, _>(path).await;
}

#[zbus::interface(name = "org.freedesktop.impl.portal.ScreenCast")]
impl ImplScreenCast {
    async fn create_session(
        &self,
        _handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: &str,
        options: WireDict,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> (u32, WireDict) {
        if !from_frontend(conn, &hdr).await {
            return denied();
        }
        let path = session_handle.as_str().to_owned();
        let obj = ImplSession {
            shared: self.0.clone(),
            path: path.clone(),
        };
        if !matches!(conn.object_server().at(path.as_str(), obj).await, Ok(None)) {
            return denied();
        }
        let r = self
            .0
            .backend
            .create_session(call(app_id, &path, "", to_options(&options)))
            .await;
        if r.response != ecl_portal_core::request::Response::Success {
            end_session(&self.0, conn, &path).await;
            return denied();
        }
        wire(r)
    }

    async fn select_sources(
        &self,
        _handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: &str,
        options: WireDict,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> (u32, WireDict) {
        if !from_frontend(conn, &hdr).await {
            return denied();
        }
        wire(
            self.0
                .backend
                .select_sources(call(
                    app_id,
                    session_handle.as_str(),
                    "",
                    to_options(&options),
                ))
                .await,
        )
    }

    #[allow(clippy::too_many_arguments)] // fixed by the D-Bus method signature
    async fn start(
        &self,
        _handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: &str,
        parent_window: &str,
        options: WireDict,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> (u32, WireDict) {
        if !from_frontend(conn, &hdr).await {
            return denied();
        }
        wire(
            self.0
                .backend
                .start(call(
                    app_id,
                    session_handle.as_str(),
                    parent_window,
                    to_options(&options),
                ))
                .await,
        )
    }

    #[zbus(property, name = "AvailableSourceTypes")]
    async fn available_source_types(&self) -> u32 {
        self.0.backend.available_source_types()
    }

    #[zbus(property, name = "AvailableCursorModes")]
    async fn available_cursor_modes(&self) -> u32 {
        self.0.backend.available_cursor_modes()
    }

    #[zbus(property, name = "version")]
    async fn version(&self) -> u32 {
        4
    }
}

pub struct ImplScreenshot(pub Arc<Shared>);

#[zbus::interface(name = "org.freedesktop.impl.portal.Screenshot")]
impl ImplScreenshot {
    async fn screenshot(
        &self,
        _handle: ObjectPath<'_>,
        app_id: &str,
        parent_window: &str,
        options: WireDict,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> (u32, WireDict) {
        if !from_frontend(conn, &hdr).await {
            return denied();
        }
        wire(
            self.0
                .backend
                .screenshot(call(app_id, "", parent_window, to_options(&options)))
                .await,
        )
    }

    async fn pick_color(
        &self,
        _handle: ObjectPath<'_>,
        app_id: &str,
        parent_window: &str,
        options: WireDict,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> (u32, WireDict) {
        if !from_frontend(conn, &hdr).await {
            return denied();
        }
        wire(
            self.0
                .backend
                .pick_color(call(app_id, "", parent_window, to_options(&options)))
                .await,
        )
    }

    #[zbus(property, name = "version")]
    async fn version(&self) -> u32 {
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_ids_are_advisory_and_validated() {
        assert_eq!(requester("").app_id, "host");
        assert_eq!(requester("not an id!").app_id, "host");
        assert_eq!(requester("org.example.App").app_id, "org.example.App");
    }

    #[test]
    fn refusals_are_response_two_with_nothing_in_them() {
        let (code, map) = denied();
        assert_eq!(code, 2);
        assert!(map.is_empty());
    }
}
