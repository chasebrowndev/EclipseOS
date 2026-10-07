// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-inferenced`: the inference router (I-02, ADR 0076).
//!
//! agentd forwards an agent's `inference.complete` here over
//! `$XDG_RUNTIME_DIR/eclipse/inferenced.sock` (frames: `ec-inference-wire`).
//! The router picks a backend by the request's `backend` field, which agentd
//! took from the package manifest and never from the agent.
//!
//! * [`api`]: `POST /v1/messages` with a key brokerd releases per request.
//! * [`claude_code`]: one warm, sandboxed `claude -p` per task; its tool calls
//!   come back as the agent's `tool_use` blocks through [`shim`].
//!
//! The network and brokerd sit behind the [`api::Http`] and
//! [`credential::Credentials`] traits so the server is testable end to end
//! without either.
//!
//! Invariants: the API key lives in a [`secret::ApiKey`] for exactly one HTTP
//! call, is never printed (Debug is redacted), and is zeroed on drop. Log
//! lines carry task, package, model, outcome and token counts, never content.

#![deny(unsafe_code)]

pub mod api;
pub mod claude_code;
pub mod credential;
pub mod peer;
pub mod router;
pub mod secret;
pub mod server;
pub mod shim;
