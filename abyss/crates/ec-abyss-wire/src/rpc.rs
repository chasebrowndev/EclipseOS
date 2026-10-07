// SPDX-License-Identifier: AGPL-3.0-only
//! JSON-RPC error type and codes.

/// JSON-RPC error codes. The negative 32xxx range is the standard's; -32000
/// down is ours.
pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const DENIED: i32 = -32000;
pub const NOT_IMPLEMENTED: i32 = -32001;
pub const RATE_LIMITED: i32 = -32002;

pub struct RpcError {
    pub code: i32,
    pub message: String,
}

impl RpcError {
    pub fn invalid_params(m: &str) -> Self {
        Self {
            code: INVALID_PARAMS,
            message: m.to_owned(),
        }
    }
    pub fn denied(m: &str) -> Self {
        Self {
            code: DENIED,
            message: m.to_owned(),
        }
    }
    /// A rate-limited call (A-08 §7). The message carries the code name and
    /// the wait, so a script can back off without parsing a new field.
    pub fn rate_limited(retry_after_ms: u64) -> Self {
        Self {
            code: RATE_LIMITED,
            message: format!("rate_limited: retry_after_ms={retry_after_ms}"),
        }
    }
    pub fn not_implemented(m: &str) -> Self {
        Self {
            code: NOT_IMPLEMENTED,
            message: m.to_owned(),
        }
    }
}
