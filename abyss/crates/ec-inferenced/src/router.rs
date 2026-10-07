// SPDX-License-Identifier: AGPL-3.0-only
//! Backend dispatch.

use crate::api::{self, failure, Http};
use crate::credential::Credentials;
use ec_inference_wire::{Backend, Completion, Failure, Request};
use std::sync::Arc;

pub struct Router {
    http: Arc<dyn Http>,
    creds: Arc<dyn Credentials>,
}

impl Router {
    pub fn new(http: Arc<dyn Http>, creds: Arc<dyn Credentials>) -> Router {
        Router { http, creds }
    }

    /// Serve one completion. Blocks for the whole model call; the server runs
    /// each on its own thread.
    pub fn complete(&self, req: &Request) -> Result<Completion, Failure> {
        if req.max_tokens == 0 {
            return Err(failure("bad_request", "max_tokens must be positive"));
        }
        match req.backend {
            Backend::Api => self.complete_api(req),
            // The Claude Code backend lands here: a warm `claude -p` session
            // keyed by `req.task`, released in `close`.
            Backend::ClaudeCode => Err(failure(
                "backend_unavailable",
                "the Claude Code backend is not built yet",
            )),
        }
    }

    fn complete_api(&self, req: &Request) -> Result<Completion, Failure> {
        // Held for this one call only; zeroed when it drops at scope end.
        let key = self.creds.api_key(&req.package)?;
        api::complete(&*self.http, &key, req)
    }

    /// The task closed. The API backend is stateless, so there is nothing to
    /// drop; a stateful backend releases its per-task session here.
    pub fn close(&self, _task: &str) {}
}
