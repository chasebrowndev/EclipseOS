// SPDX-License-Identifier: AGPL-3.0-only

//! Screenshot and ScreenCast frontends. The daemon owns the Request/Session
//! objects, tokens and identity; every decision and frame comes from the
//! `ScreenCastBackend` (lane B). With the stub backend all steps answer
//! response 2.

use std::sync::Arc;

use ecl_portal_core::options::Options;
use ecl_portal_core::request::{Reply, Response};
use ecl_portal_core::screencast::Call;
use ecl_portal_core::val::Val;
use zbus::message::Header;
use zbus::{Connection, OwnedFd, OwnedObjectPath};

use crate::convert::{to_options, WireDict};
use crate::error::PortalError;
use crate::req::{self, sender_of, Outcome};
use crate::session;
use crate::state::{SessionKind, Shared};

pub struct ScreenshotSvc(pub Arc<Shared>);

#[zbus::interface(name = "org.freedesktop.portal.Screenshot")]
impl ScreenshotSvc {
    async fn screenshot(
        &self,
        parent_window: String,
        options: WireDict,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<OwnedObjectPath, PortalError> {
        let opts = to_options(&options);
        let o2 = opts.clone();
        req::start(&self.0, conn, &hdr, &opts, move |ctx| async move {
            let call = Call {
                requester: ctx.requester,
                session: String::new(),
                parent_window,
                options: o2,
            };
            Outcome::Reply(ctx.shared.backend.screenshot(call).await)
        })
        .await
    }

    async fn pick_color(
        &self,
        parent_window: String,
        options: WireDict,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<OwnedObjectPath, PortalError> {
        let opts = to_options(&options);
        let o2 = opts.clone();
        req::start(&self.0, conn, &hdr, &opts, move |ctx| async move {
            let call = Call {
                requester: ctx.requester,
                session: String::new(),
                parent_window,
                options: o2,
            };
            Outcome::Reply(ctx.shared.backend.pick_color(call).await)
        })
        .await
    }

    #[zbus(property, name = "version")]
    async fn version(&self) -> u32 {
        2
    }
}

pub struct ScreenCastSvc(pub Arc<Shared>);

impl ScreenCastSvc {
    /// `session` must be a live ScreenCast session created by the caller.
    fn check_session(&self, hdr: &Header<'_>, session: &str) -> Result<(), PortalError> {
        let sender = sender_of(hdr)?;
        let table = crate::state::lock(&self.0.sessions);
        let ok = table.owner(session) == Some(sender.as_str())
            && table.get(session) == Some(&SessionKind::ScreenCast);
        if ok {
            Ok(())
        } else {
            Err(PortalError::NotFound("no such session".into()))
        }
    }
}

async fn step(
    ctx: req::ReqCtx,
    session: String,
    parent_window: String,
    options: Options,
    start: bool,
) -> Outcome {
    let call = Call {
        requester: ctx.requester,
        session,
        parent_window,
        options,
    };
    let reply = if start {
        ctx.shared.backend.start(call).await
    } else {
        ctx.shared.backend.select_sources(call).await
    };
    Outcome::Reply(reply)
}

#[zbus::interface(name = "org.freedesktop.portal.ScreenCast")]
impl ScreenCastSvc {
    async fn create_session(
        &self,
        options: WireDict,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<OwnedObjectPath, PortalError> {
        let opts = to_options(&options);
        let o2 = opts.clone();
        req::start(&self.0, conn, &hdr, &opts, move |ctx| async move {
            let path = match session::create(
                &ctx.shared,
                &ctx.conn,
                &ctx.sender,
                &o2,
                SessionKind::ScreenCast,
            )
            .await
            {
                Ok(p) => p,
                Err(_) => return Outcome::Reply(Reply::other()),
            };
            let call = Call {
                requester: ctx.requester.clone(),
                session: path.clone(),
                parent_window: String::new(),
                options: o2,
            };
            let reply = ctx.shared.backend.create_session(call).await;
            if reply.response != Response::Success {
                session::close(&ctx.shared, &ctx.conn, &path).await;
                return Outcome::Reply(Reply::other());
            }
            let mut results = reply.results;
            results.insert("session_handle", Val::Str(path));
            Outcome::Reply(Reply::ok(results))
        })
        .await
    }

    async fn select_sources(
        &self,
        session_handle: OwnedObjectPath,
        options: WireDict,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<OwnedObjectPath, PortalError> {
        let session = session_handle.as_str().to_owned();
        self.check_session(&hdr, &session)?;
        let opts = to_options(&options);
        let o2 = opts.clone();
        req::start(&self.0, conn, &hdr, &opts, move |ctx| {
            step(ctx, session, String::new(), o2, false)
        })
        .await
    }

    async fn start(
        &self,
        session_handle: OwnedObjectPath,
        parent_window: String,
        options: WireDict,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<OwnedObjectPath, PortalError> {
        let session = session_handle.as_str().to_owned();
        self.check_session(&hdr, &session)?;
        let opts = to_options(&options);
        let o2 = opts.clone();
        req::start(&self.0, conn, &hdr, &opts, move |ctx| {
            step(ctx, session, parent_window, o2, true)
        })
        .await
    }

    async fn open_pipe_wire_remote(
        &self,
        session_handle: OwnedObjectPath,
        _options: WireDict,
        #[zbus(header)] hdr: Header<'_>,
    ) -> Result<OwnedFd, PortalError> {
        self.check_session(&hdr, session_handle.as_str())?;
        let fd = self
            .0
            .backend
            .open_pipewire_remote(session_handle.as_str())
            .await
            .map_err(|e| PortalError::Failed(e.to_string()))?;
        Ok(OwnedFd::from(fd))
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
