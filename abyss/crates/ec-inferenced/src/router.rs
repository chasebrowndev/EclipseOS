// SPDX-License-Identifier: AGPL-3.0-only
//! Backend dispatch.

use crate::api::{self, failure, Http};
use crate::claude_code::{ClaudeCode, ClaudeConfig};
use crate::credential::Credentials;
use ec_inference_wire::{Backend, Completion, Failure, Request};
use std::sync::Arc;

pub struct Router {
    http: Arc<dyn Http>,
    creds: Arc<dyn Credentials>,
    claude: ClaudeCode,
}

impl Router {
    pub fn new(http: Arc<dyn Http>, creds: Arc<dyn Credentials>) -> Router {
        Router::with_claude(http, creds, ClaudeConfig::from_env())
    }

    /// As [`Router::new`] with an explicit Claude Code configuration (tests).
    pub fn with_claude(http: Arc<dyn Http>, creds: Arc<dyn Credentials>, cfg: ClaudeConfig) -> Router {
        let claude = ClaudeCode::new(cfg, creds.clone());
        Router { http, creds, claude }
    }

    /// The Claude Code backend, for diagnostics and tests.
    pub fn claude(&self) -> &ClaudeCode {
        &self.claude
    }

    /// Serve one completion. Blocks for the whole model call; the server runs
    /// each on its own thread.
    pub fn complete(&self, req: &Request) -> Result<Completion, Failure> {
        if req.max_tokens == 0 {
            return Err(failure("bad_request", "max_tokens must be positive"));
        }
        match req.backend {
            Backend::Api => self.complete_api(req),
            // A warm `claude -p` session keyed by `req.task`, released in
            // `close`.
            Backend::ClaudeCode => self.claude.complete(req),
        }
    }

    fn complete_api(&self, req: &Request) -> Result<Completion, Failure> {
        // Held for this one call only; zeroed when it drops at scope end.
        let key = self.creds.api_key(&req.package)?;
        api::complete(&*self.http, &key, req)
    }

    /// The task closed: the API backend is stateless, so only the Claude Code
    /// session (if any) is dropped.
    pub fn close(&self, task: &str) {
        self.claude.close(task);
    }
}
