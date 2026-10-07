// SPDX-License-Identifier: AGPL-3.0-only
//! Line-delimited JSON-RPC 2.0 as COMP-13 §2 uses it, shared by the console
//! socket and the per-task MCP sockets.
//!
//! Codes: the standard `-32700..-32602`, `-32000` (refused) and `-32001`
//! (not implemented) are COMP-13's. `-32002..-32006` are agentd's own; each
//! error also carries `data.reason`, a stable word a client can match on.

use serde_json::{json, Value};

pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL: i32 = -32603;
pub const DENIED: i32 = -32000;
/// Unknown task id, or one that is not the owner's: indistinguishable (A-08 §7).
pub const NOT_FOUND: i32 = -32002;
/// `policyd` is not linked.
pub const UNAVAILABLE: i32 = -32003;
pub const RATE_LIMITED: i32 = -32004;
pub const QUOTA_EXCEEDED: i32 = -32005;
/// The task is closed to new posts.
pub const CLOSED: i32 = -32006;

#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i32,
    pub reason: String,
    pub message: String,
}

impl RpcError {
    pub fn new(code: i32, reason: &str, message: &str) -> Self {
        RpcError {
            code,
            reason: reason.to_owned(),
            message: message.to_owned(),
        }
    }
    pub fn invalid_params(message: &str) -> Self {
        Self::new(INVALID_PARAMS, "invalid_params", message)
    }
    pub fn not_found() -> Self {
        Self::new(NOT_FOUND, "not_found", "no such task")
    }
    pub fn unavailable() -> Self {
        Self::new(UNAVAILABLE, "unavailable", "policyd is not linked")
    }
    pub fn closed() -> Self {
        Self::new(CLOSED, "closed", "the task is closed")
    }
    pub fn to_json(&self) -> Value {
        json!({"code": self.code, "message": self.message, "data": {"reason": self.reason}})
    }
}

pub struct Request {
    /// `None` for a notification.
    pub id: Option<Value>,
    pub method: String,
    /// Always an object.
    pub params: Value,
}

pub enum Parsed {
    Req(Request),
    /// A malformed request: the reply to send.
    Reply(String),
}

pub fn ok(id: &Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

pub fn err(id: &Value, e: &RpcError) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": e.to_json()}).to_string()
}

pub fn parse(line: &str) -> Parsed {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return Parsed::Reply(err(
            &Value::Null,
            &RpcError::new(PARSE_ERROR, "parse_error", "not JSON"),
        ));
    };
    let bad = |m: &str| {
        Parsed::Reply(err(
            &Value::Null,
            &RpcError::new(INVALID_REQUEST, "invalid_request", m),
        ))
    };
    let Some(obj) = v.as_object() else {
        return bad("batches and non-objects are not supported");
    };
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return bad("jsonrpc must be \"2.0\"");
    }
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        return bad("method must be a string");
    };
    let id = match obj.get("id") {
        None => None,
        Some(i) if i.is_string() || i.is_number() || i.is_null() => Some(i.clone()),
        Some(_) => return bad("id must be a string, number or null"),
    };
    let params = match obj.get("params") {
        None => json!({}),
        Some(p) if p.is_object() => p.clone(),
        Some(_) => {
            let e = RpcError::invalid_params("params must be an object");
            return Parsed::Reply(err(id.as_ref().unwrap_or(&Value::Null), &e));
        }
    };
    Parsed::Req(Request {
        id,
        method: method.to_owned(),
        params,
    })
}

/// A required string parameter.
pub fn p_str<'a>(params: &'a Value, key: &str) -> Result<&'a str, RpcError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params(&format!("{key} must be a string")))
}

/// An optional unsigned parameter.
pub fn p_u64(params: &Value, key: &str) -> Result<Option<u64>, RpcError> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| RpcError::invalid_params(&format!("{key} must be an unsigned integer"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(p: Parsed) -> Value {
        match p {
            Parsed::Reply(s) => serde_json::from_str(&s).unwrap(),
            Parsed::Req(_) => panic!("expected a reply"),
        }
    }

    #[test]
    fn malformed_lines_get_standard_errors() {
        assert_eq!(reply(parse("{nope"))["error"]["code"], PARSE_ERROR);
        assert_eq!(reply(parse("[1]"))["error"]["code"], INVALID_REQUEST);
        assert_eq!(
            reply(parse(r#"{"jsonrpc":"1.0","method":"x"}"#))["error"]["code"],
            INVALID_REQUEST
        );
        assert_eq!(
            reply(parse(r#"{"jsonrpc":"2.0","method":5}"#))["error"]["code"],
            INVALID_REQUEST
        );
        let r = reply(parse(r#"{"jsonrpc":"2.0","id":4,"method":"x","params":[1]}"#));
        assert_eq!(r["error"]["code"], INVALID_PARAMS);
        assert_eq!(r["id"], 4);
    }

    #[test]
    fn notifications_have_no_id() {
        let Parsed::Req(r) = parse(r#"{"jsonrpc":"2.0","method":"x"}"#) else {
            panic!()
        };
        assert!(r.id.is_none());
        assert!(r.params.is_object());
    }
}
