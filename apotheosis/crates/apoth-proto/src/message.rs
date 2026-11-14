// SPDX-License-Identifier: Apache-2.0
//! Envelopes: request, reply, event (T-12 §1). Replies and events are distinct types, so
//! "events never interleave inside a reply" is expressible: a reply is one whole line and
//! an event is another.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{ErrorCode, WireError};
use crate::event::WireEvent;
use crate::framing::{check_line, encode_line};
use crate::method::Method;
use crate::params::Params;

/// Current wire protocol version (gap: the spec gives `proto` no type).
pub const PROTO_VERSION: u32 = 1;
/// Oldest version this crate still speaks (REMOTE-05 asks for it to be declared).
pub const PROTO_OLDEST: u32 = 1;

/// Pick the version for a connection: the lower of ours and the client's, or
/// `unsupported-version` when the client is older than [`PROTO_OLDEST`] or `0`.
pub fn negotiate(client_proto: u32) -> Result<u32, WireError> {
    if client_proto < PROTO_OLDEST {
        return Err(WireError::new(
            ErrorCode::UnsupportedVersion,
            format!("oldest supported protocol is {PROTO_OLDEST}"),
        ));
    }
    Ok(client_proto.min(PROTO_VERSION))
}

/// A decode failure with the request id when it could be recovered, so the server can
/// still answer the right id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError {
    /// Request id, if the line had a usable one.
    pub id: Option<u64>,
    /// The error to send.
    pub error: WireError,
}

impl DecodeError {
    fn new(id: Option<u64>, error: WireError) -> Self {
        Self { id, error }
    }

    /// The error reply for this failure.
    pub fn reply(&self) -> Reply {
        Reply::Err {
            id: self.id,
            error: self.error.clone(),
        }
    }
}

/// A request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// Client-chosen id, echoed in the reply.
    pub id: u64,
    /// The method and its params.
    pub params: Params,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRequest {
    id: u64,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

impl Request {
    /// Decode one line (without newline). Strict: unknown methods and unknown fields at
    /// any level are errors; the line limit is enforced here too.
    pub fn decode(line: &[u8]) -> Result<Request, DecodeError> {
        check_line(line).map_err(|e| DecodeError::new(None, e))?;
        let v: Value = serde_json::from_slice(line)
            .map_err(|_| DecodeError::new(None, WireError::bad_request("not valid JSON")))?;
        let id = v.get("id").and_then(Value::as_u64);
        let fail = |e: WireError| DecodeError::new(id, e);
        if !v.is_object() {
            return Err(fail(WireError::bad_request("request is not an object")));
        }
        let raw: RawRequest = serde_json::from_value(v)
            .map_err(|e| fail(WireError::bad_request(format!("request: {e}"))))?;
        let Some(method) = Method::parse(&raw.method) else {
            return Err(fail(WireError::new(
                ErrorCode::UnknownMethod,
                "unknown method",
            )));
        };
        let params = match raw.params {
            None => Value::Object(Map::new()),
            Some(p @ Value::Object(_)) => p,
            Some(_) => return Err(fail(WireError::bad_request("params is not an object"))),
        };
        let params = Params::decode(method, params).map_err(fail)?;
        Ok(Request { id: raw.id, params })
    }

    /// The method.
    pub fn method(&self) -> Method {
        self.params.method()
    }

    /// The JSON object.
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("id".into(), Value::from(self.id));
        m.insert("method".into(), Value::from(self.method().name()));
        m.insert("params".into(), self.params.to_value());
        Value::Object(m)
    }

    /// Encode as a line with trailing newline.
    pub fn to_line(&self) -> Result<Vec<u8>, WireError> {
        encode_line(&self.to_value())
    }
}

/// A reply: exactly one per request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// `{"id":n,"ok":…}`.
    Ok {
        /// Request id.
        id: u64,
        /// Result.
        ok: Value,
    },
    /// `{"id":n,"error":{…}}`; `id` is `null` when the request id was unrecoverable.
    Err {
        /// Request id, if known.
        id: Option<u64>,
        /// Error.
        error: WireError,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOk {
    id: u64,
    ok: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawErr {
    id: Option<u64>,
    error: WireError,
}

impl Reply {
    /// A success reply from any serialisable result.
    pub fn ok<T: Serialize>(id: u64, result: &T) -> Result<Reply, WireError> {
        let ok = serde_json::to_value(result)
            .map_err(|_| WireError::new(ErrorCode::Internal, "result not serialisable"))?;
        Ok(Reply::Ok { id, ok })
    }

    /// An error reply.
    pub fn error(id: Option<u64>, code: ErrorCode, message: impl Into<String>) -> Reply {
        Reply::Err {
            id,
            error: WireError::new(code, message),
        }
    }

    /// The JSON object.
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        match self {
            Reply::Ok { id, ok } => {
                m.insert("id".into(), Value::from(*id));
                m.insert("ok".into(), ok.clone());
            }
            Reply::Err { id, error } => {
                m.insert("id".into(), id.map_or(Value::Null, Value::from));
                m.insert(
                    "error".into(),
                    serde_json::to_value(error).unwrap_or(Value::Null),
                );
            }
        }
        Value::Object(m)
    }

    /// Encode as a line with trailing newline.
    pub fn to_line(&self) -> Result<Vec<u8>, WireError> {
        encode_line(&self.to_value())
    }
}

/// Anything the server writes: a reply line or an event line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerMessage {
    /// A reply.
    Reply(Reply),
    /// An event.
    Event(WireEvent),
}

impl ServerMessage {
    /// Decode one server line (client side). Envelopes are strict; `ok` payloads are not
    /// inspected.
    pub fn decode(line: &[u8]) -> Result<ServerMessage, WireError> {
        check_line(line)?;
        let v: Value =
            serde_json::from_slice(line).map_err(|_| WireError::bad_request("not valid JSON"))?;
        let Some(obj) = v.as_object() else {
            return Err(WireError::bad_request("message is not an object"));
        };
        let (ev, ok, er) = (
            obj.contains_key("event"),
            obj.contains_key("ok"),
            obj.contains_key("error"),
        );
        match (ev, ok, er) {
            (true, false, false) => WireEvent::from_value(v).map(ServerMessage::Event),
            (false, true, false) => {
                let r: RawOk = serde_json::from_value(v)
                    .map_err(|e| WireError::bad_request(format!("reply: {e}")))?;
                Ok(ServerMessage::Reply(Reply::Ok { id: r.id, ok: r.ok }))
            }
            (false, false, true) => {
                let r: RawErr = serde_json::from_value(v)
                    .map_err(|e| WireError::bad_request(format!("reply: {e}")))?;
                Ok(ServerMessage::Reply(Reply::Err {
                    id: r.id,
                    error: r.error,
                }))
            }
            _ => Err(WireError::bad_request(
                "not exactly one of event, ok, error",
            )),
        }
    }

    /// Encode as a line with trailing newline.
    pub fn to_line(&self) -> Result<Vec<u8>, WireError> {
        match self {
            ServerMessage::Reply(r) => r.to_line(),
            ServerMessage::Event(e) => encode_line(e),
        }
    }
}
