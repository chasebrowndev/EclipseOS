// SPDX-License-Identifier: AGPL-3.0-only

//! `org.freedesktop.portal.Inhibit`, forwarded to `org.freedesktop.ScreenSaver`.
//! An Inhibit request has no `Response`: the inhibition lasts until the
//! Request is closed (or the caller exits), then `UnInhibit` is sent.

use std::sync::Arc;

use ecl_portal_core::request::{Reply, Results};
use ecl_portal_core::val::Val;
use zbus::message::Header;
use zbus::{Connection, OwnedObjectPath};

use crate::convert::{to_options, WireDict};
use crate::error::PortalError;
use crate::req::{self, Outcome};
use crate::session;
use crate::state::{SessionKind, Shared};

const DEST: &str = "org.freedesktop.ScreenSaver";
const PATH: &str = "/org/freedesktop/ScreenSaver";

/// Inhibit flags.
const FLAG_IDLE: u32 = 4;

pub struct InhibitSvc(pub Arc<Shared>);

/// Sends `UnInhibit` when dropped.
struct Cookie {
    conn: Connection,
    cookie: u32,
}

impl Drop for Cookie {
    fn drop(&mut self) {
        let conn = self.conn.clone();
        let cookie = self.cookie;
        tokio::spawn(async move {
            let _ = conn
                .call_method(Some(DEST), PATH, Some(DEST), "UnInhibit", &(cookie,))
                .await;
        });
    }
}

#[zbus::interface(name = "org.freedesktop.portal.Inhibit")]
impl InhibitSvc {
    async fn inhibit(
        &self,
        _window: &str,
        flags: u32,
        options: WireDict,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<OwnedObjectPath, PortalError> {
        let opts = to_options(&options);
        let reason = opts.str("reason").unwrap_or("").to_owned();
        req::start(&self.0, conn, &hdr, &opts, move |ctx| async move {
            // Only idle maps onto ScreenSaver; logout/suspend/switch inhibits
            // have no counterpart here and are accepted as no-ops.
            if flags & FLAG_IDLE == 0 {
                return Outcome::Hold(Box::new(()));
            }
            let app = ctx
                .requester
                .display_name
                .clone()
                .unwrap_or_else(|| ctx.requester.app_id.clone());
            let cookie: Option<u32> = ctx
                .conn
                .call_method(Some(DEST), PATH, Some(DEST), "Inhibit", &(app, reason))
                .await
                .ok()
                .and_then(|m| m.body().deserialize().ok());
            match cookie {
                Some(cookie) => Outcome::Hold(Box::new(Cookie {
                    conn: ctx.conn.clone(),
                    cookie,
                })),
                // No ScreenSaver service: the inhibit cannot be honoured.
                None => Outcome::Reply(Reply::other()),
            }
        })
        .await
    }

    async fn create_monitor(
        &self,
        _window: &str,
        options: WireDict,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<OwnedObjectPath, PortalError> {
        let opts = to_options(&options);
        let sopts = opts.clone();
        req::start(&self.0, conn, &hdr, &opts, move |ctx| async move {
            match session::create(
                &ctx.shared,
                &ctx.conn,
                &ctx.sender,
                &sopts,
                SessionKind::InhibitMonitor,
            )
            .await
            {
                Ok(path) => Outcome::Reply(Reply::ok(
                    Results::new().with("session_handle", Val::Str(path)),
                )),
                Err(_) => Outcome::Reply(Reply::other()),
            }
        })
        .await
    }

    async fn query_end_response(&self, _session_handle: OwnedObjectPath) {}

    #[zbus(property, name = "version")]
    async fn version(&self) -> u32 {
        3
    }
}
