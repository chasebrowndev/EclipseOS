// SPDX-License-Identifier: Apache-2.0
//! Control-socket wire types (newline-delimited JSON).
//!
//! Governing spec: T-12 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`).
//!
//! Types and framing only: no I/O loop, no peer identity, no policy. The server (M29)
//! reads lines through [`LineDecoder`], decodes with [`Request::decode`], gates on
//! [`Params::classify`] / [`METHODS`], and answers with [`Reply`] and [`WireEvent`] lines.
//!
//! Strictness (T-12 §1): requests reject unknown methods and unknown fields at every
//! level; envelopes of replies and events are strict too, result payloads are not.
//! There is deliberately no method that answers a consent or sheet prompt (T-11 §1);
//! `action.run` of such a name is `unknown-action`.
#![forbid(unsafe_code)]

pub mod caps;
pub mod error;
pub mod event;
pub mod framing;
pub mod fuzz;
pub mod limits;
pub mod message;
pub mod method;
pub mod params;
pub mod results;

pub use caps::{Cap, Class};
pub use error::{
    EXIT_DENIED, EXIT_ERROR, EXIT_NO_INSTANCE, EXIT_OK, EXIT_USAGE, ErrorCode, WireError,
};
pub use event::WireEvent;
pub use framing::{LineDecoder, check_line, encode_line};
pub use fuzz::fuzz_parse;
pub use limits::MAX_LINE_BYTES;
pub use message::{
    DecodeError, PROTO_OLDEST, PROTO_VERSION, Reply, Request, ServerMessage, negotiate,
};
pub use method::{METHODS, Method, MethodInfo, injects_input};
pub use params::Params;

/// Environment variables each pane exports (T-12 §3).
pub const ENV_SOCKET: &str = "APOTHEOSIS_SOCKET";
/// See [`ENV_SOCKET`].
pub const ENV_PANE: &str = "APOTHEOSIS_PANE";
/// See [`ENV_SOCKET`].
pub const ENV_WINDOW: &str = "APOTHEOSIS_WINDOW";
/// See [`ENV_SOCKET`].
pub const ENV_VERSION: &str = "APOTHEOSIS_VERSION";

/// The target used when a request names none and the client is inside a pane (T-12 §3).
pub const DEFAULT_TARGET: &str = "self";

/// Features this crate's server side offers in `hello` (gap: vocabulary is a proposal).
pub const FEATURES: &[&str] = &["events", "launch-wait", "pane-text-sgr"];
