// SPDX-License-Identifier: AGPL-3.0-only

//! The `org.freedesktop.portal.Request` object and the driver that runs one
//! portal call to its `Response`.
//!
//! Lifecycle (D-08 §3): export the Request at its predictable path, return the
//! handle, resolve identity, run the work, emit `Response` to the caller, drop
//! the object. `Request.Close`, the caller vanishing, or daemon shutdown cancel
//! the work by dropping its future (so child panes and pickers die with it)
//! and emit nothing.

use std::future::Future;
use std::sync::Arc;

use ecl_portal_core::options::Options;
use ecl_portal_core::request::{generated_token, request_path, Reply};
use ecl_portal_proto::Requester;
use tokio::sync::oneshot;
use zbus::message::Header;
use zbus::{Connection, OwnedObjectPath};

use crate::convert::{self, results_map};
use crate::error::PortalError;
use crate::ident;
use crate::state::{lock, Shared};

pub const REQUEST_IFACE: &str = "org.freedesktop.portal.Request";

pub struct RequestObj {
    shared: Arc<Shared>,
    path: String,
    owner: String,
}

#[zbus::interface(name = "org.freedesktop.portal.Request")]
impl RequestObj {
    async fn close(&self, #[zbus(header)] hdr: Header<'_>) -> Result<(), PortalError> {
        // Only the creator may cancel its request.
        if hdr.sender().map(|s| s.as_str()) != Some(self.owner.as_str()) {
            return Err(PortalError::NotAllowed(
                "only the caller that created a request may close it".into(),
            ));
        }
        self.shared.cancel_request(&self.path);
        Ok(())
    }
}

/// What a piece of work ends in.
pub enum Outcome {
    /// Emit this `Response`.
    Reply(Reply),
    /// No `Response`: keep `guard` alive until the request is closed (Inhibit).
    Hold(Box<dyn Send>),
}

/// Context handed to the work closure.
pub struct ReqCtx {
    pub requester: Requester,
    pub sender: String,
    pub conn: Connection,
    pub shared: Arc<Shared>,
}

pub fn sender_of(hdr: &Header<'_>) -> Result<String, PortalError> {
    hdr.sender()
        .map(|s| s.to_string())
        .ok_or_else(|| PortalError::Failed("call has no sender".into()))
}

/// Register a Request for the caller and run `work` in the background.
/// Returns the request handle to give back from the D-Bus method.
pub async fn start<F, Fut>(
    shared: &Arc<Shared>,
    conn: &Connection,
    hdr: &Header<'_>,
    opts: &Options,
    work: F,
) -> Result<OwnedObjectPath, PortalError>
where
    F: FnOnce(ReqCtx) -> Fut + Send + 'static,
    Fut: Future<Output = Outcome> + Send + 'static,
{
    let sender = sender_of(hdr)?;
    let token = match opts.handle_token()? {
        Some(t) => t.to_owned(),
        None => generated_token(shared.next_id()),
    };
    let path = request_path(&sender, &token)?;
    let (tx, rx) = oneshot::channel();
    lock(&shared.requests)
        .insert(path.clone(), &sender, tx)
        .map_err(|_| PortalError::InvalidArgument("handle_token already in use".into()))?;
    let obj = RequestObj {
        shared: shared.clone(),
        path: path.clone(),
        owner: sender.clone(),
    };
    match conn.object_server().at(path.as_str(), obj).await {
        Ok(None) => {}
        Ok(Some(_)) => {
            lock(&shared.requests).remove(&path);
            return Err(PortalError::InvalidArgument("request path exists".into()));
        }
        Err(e) => {
            lock(&shared.requests).remove(&path);
            return Err(e.into());
        }
    }
    let handle = convert::object_path(&path)?;
    tokio::spawn(drive(shared.clone(), conn.clone(), sender, path, rx, work));
    Ok(handle)
}

async fn drive<F, Fut>(
    shared: Arc<Shared>,
    conn: Connection,
    sender: String,
    path: String,
    mut rx: oneshot::Receiver<()>,
    work: F,
) where
    F: FnOnce(ReqCtx) -> Fut,
    Fut: Future<Output = Outcome>,
{
    let job = async {
        let requester = ident::requester(&shared, &conn, &sender).await;
        work(ReqCtx {
            requester,
            sender: sender.clone(),
            conn: conn.clone(),
            shared: shared.clone(),
        })
        .await
    };
    let outcome = tokio::select! {
        o = job => Some(o),
        _ = &mut rx => None,
    };
    match outcome {
        Some(Outcome::Reply(reply)) => {
            let body = (reply.response.code(), results_map(&reply.results));
            if let Err(e) = conn
                .emit_signal(
                    Some(sender.as_str()),
                    path.as_str(),
                    REQUEST_IFACE,
                    "Response",
                    &body,
                )
                .await
            {
                tracing::warn!("cannot emit Response: {e}");
            }
        }
        Some(Outcome::Hold(guard)) => {
            let _ = (&mut rx).await;
            drop(guard);
        }
        None => {}
    }
    lock(&shared.requests).remove(&path);
    let _ = conn
        .object_server()
        .remove::<RequestObj, _>(path.as_str())
        .await;
}
