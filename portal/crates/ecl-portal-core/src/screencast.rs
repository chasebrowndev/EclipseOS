// SPDX-License-Identifier: AGPL-3.0-only

//! The boundary between the daemon's D-Bus frontend and the ScreenCast /
//! Screenshot implementation (`ecl-portal-screencast`, lane B).
//!
//! The daemon owns Request/Session objects, identity and the object paths; a
//! backend only answers "what is the reply" for each step. Methods return
//! boxed futures so the trait is object-safe without a macro, and the daemon
//! drops the future when the request is closed (cancel by drop).

use std::future::Future;
use std::os::fd::OwnedFd;
use std::pin::Pin;
use std::sync::Arc;

use ecl_portal_proto::Requester;

use crate::options::Options;
use crate::request::Reply;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// `AvailableSourceTypes` bit flags.
pub const SOURCE_MONITOR: u32 = 1;
pub const SOURCE_WINDOW: u32 = 2;
/// `AvailableCursorModes` bit flags.
pub const CURSOR_HIDDEN: u32 = 1;
pub const CURSOR_EMBEDDED: u32 = 2;
pub const CURSOR_METADATA: u32 = 4;

/// What the backend is told about a call. `session` is the daemon-minted
/// session object path (empty for Screenshot). Everything in `options` is the
/// caller's claim.
#[derive(Debug, Clone)]
pub struct Call {
    pub requester: Requester,
    pub session: String,
    pub parent_window: String,
    pub options: Options,
}

#[derive(Debug, thiserror::Error)]
pub enum ScreenCastError {
    #[error("screencast is not available: {0}")]
    Unavailable(String),
    #[error("no such session")]
    NoSession,
}

pub trait ScreenCastBackend: Send + Sync {
    fn available_source_types(&self) -> u32;
    fn available_cursor_modes(&self) -> u32;

    /// `CreateSession`: set up state for `call.session`.
    fn create_session(&self, call: Call) -> BoxFuture<'_, Reply>;
    fn select_sources(&self, call: Call) -> BoxFuture<'_, Reply>;
    /// `Start`: may show the source chooser and the abyss confirm (ADR 0085).
    fn start(&self, call: Call) -> BoxFuture<'_, Reply>;
    /// A PipeWire remote socket for `session`.
    fn open_pipewire_remote(
        &self,
        session: &str,
    ) -> BoxFuture<'_, Result<OwnedFd, ScreenCastError>>;
    /// Session closed by the caller, a peer exit, or the daemon.
    fn close_session(&self, session: &str) -> BoxFuture<'_, ()>;

    fn screenshot(&self, call: Call) -> BoxFuture<'_, Reply>;
    fn pick_color(&self, call: Call) -> BoxFuture<'_, Reply>;
}

/// Stand-in until lane B lands: every step answers response 2, no sources are
/// advertised, and `OpenPipeWireRemote` fails with a clear error.
#[derive(Debug, Default, Clone, Copy)]
pub struct StubBackend;

impl StubBackend {
    pub fn shared() -> Arc<dyn ScreenCastBackend> {
        Arc::new(StubBackend)
    }
}

fn ended() -> BoxFuture<'static, Reply> {
    Box::pin(async { Reply::other() })
}

impl ScreenCastBackend for StubBackend {
    fn available_source_types(&self) -> u32 {
        0
    }

    fn available_cursor_modes(&self) -> u32 {
        0
    }

    fn create_session(&self, _: Call) -> BoxFuture<'_, Reply> {
        ended()
    }

    fn select_sources(&self, _: Call) -> BoxFuture<'_, Reply> {
        ended()
    }

    fn start(&self, _: Call) -> BoxFuture<'_, Reply> {
        ended()
    }

    fn open_pipewire_remote(&self, _: &str) -> BoxFuture<'_, Result<OwnedFd, ScreenCastError>> {
        Box::pin(async {
            Err(ScreenCastError::Unavailable(
                "ecl-portal-screencast is not wired in".into(),
            ))
        })
    }

    fn close_session(&self, _: &str) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    fn screenshot(&self, _: Call) -> BoxFuture<'_, Reply> {
        ended()
    }

    fn pick_color(&self, _: Call) -> BoxFuture<'_, Reply> {
        ended()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::host;
    use crate::request::Response;

    fn call() -> Call {
        Call {
            requester: host(),
            session: String::new(),
            parent_window: String::new(),
            options: Options::new(),
        }
    }

    // A tiny executor: the stub futures are immediately ready.
    fn block_on<F: Future>(f: F) -> F::Output {
        use std::task::{Context, Poll, Waker};
        let mut cx = Context::from_waker(Waker::noop());
        let mut f = Box::pin(f);
        match f.as_mut().poll(&mut cx) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("stub future was not ready"),
        }
    }

    #[test]
    fn stub_answers_response_two() {
        let b = StubBackend::shared();
        for r in [
            block_on(b.create_session(call())),
            block_on(b.select_sources(call())),
            block_on(b.start(call())),
            block_on(b.screenshot(call())),
            block_on(b.pick_color(call())),
        ] {
            assert_eq!(r.response, Response::Other);
            assert!(r.results.is_empty());
        }
        assert_eq!(b.available_source_types(), 0);
        assert!(block_on(b.open_pipewire_remote("/s")).is_err());
    }
}
