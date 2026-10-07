// SPDX-License-Identifier: AGPL-3.0-only
//! The link to the inference router, `ec-inferenced` (I-02, ADR 0076).
//!
//! One satellite thread owns the connection to
//! `$XDG_RUNTIME_DIR/eclipse/inferenced.sock`. The core hands it
//! [`ToRouter`] messages by channel and never touches the socket, so a slow or
//! absent router cannot stall the core loop. The thread connects lazily on the
//! first request, reconnects after a failure, and keeps one connection for
//! every task. A reader thread per connection turns replies into
//! [`Msg::Inference`]; when the connection ends it fails whatever was still in
//! flight on it, so no call waits on a router that is gone.
//!
//! What a call is for (task, MCP request id, socket) lives in the core, keyed
//! by the request id this module only carries. Nothing here logs request or
//! reply content.

use std::collections::HashSet;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};

use ec_inference_wire::{read_frame, write_frame, Failure, Reply, ToRouter, Value};
use serde_json::json;

use crate::core::Msg;
use crate::net::{our_uid, peer_cred};

/// The largest `max_tokens` an agent may ask for, and the default.
pub const MAX_TOKENS_CAP: u32 = 64_000;
pub const MAX_TOKENS_DEFAULT: u32 = 16_000;

pub const DOWN_MESSAGE: &str = "the inference router (ec-inferenced) is not running";

pub fn failure(kind: &str, message: &str) -> Failure {
    Failure {
        kind: kind.to_owned(),
        message: message.to_owned(),
    }
}

/// The checked arguments of one `inference.complete`.
#[derive(Debug, PartialEq)]
pub struct Args {
    pub system: String,
    pub messages: Value,
    pub tools: Value,
    pub max_tokens: u32,
}

/// Validates `arguments`. `backend` and `model` are refused outright: they
/// come from the manifest, and an agent that names them is confused or
/// probing, so it hears about it rather than being silently overridden.
pub fn parse_args(a: &Value) -> Result<Args, &'static str> {
    let o = a.as_object().ok_or("arguments must be an object")?;
    if o.contains_key("model") || o.contains_key("backend") {
        return Err("model and backend come from the package manifest, not the call");
    }
    let system = match o.get("system") {
        None => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(_) => return Err("system must be a string"),
    };
    let messages = match o.get("messages") {
        Some(m)
            if m.as_array()
                .is_some_and(|v| !v.is_empty() && v.iter().all(Value::is_object)) =>
        {
            m.clone()
        }
        _ => return Err("messages must be a non-empty array of message objects"),
    };
    let tools = match o.get("tools") {
        None => json!([]),
        Some(t) if t.as_array().is_some_and(|v| v.iter().all(Value::is_object)) => t.clone(),
        Some(_) => return Err("tools must be an array of tool definitions"),
    };
    let max_tokens = match o.get("max_tokens") {
        None => MAX_TOKENS_DEFAULT,
        Some(v) => v
            .as_u64()
            .filter(|n| (1..=u64::from(MAX_TOKENS_CAP)).contains(n))
            .map(|n| n as u32)
            .ok_or("max_tokens must be an integer from 1 to 64000")?,
    };
    Ok(Args {
        system,
        messages,
        tools,
        max_tokens,
    })
}

/// The MCP tool result for a finished call: success carries the Messages-API
/// answer as a JSON string, failure carries `{kind, message}`.
pub fn tool_result(r: &Result<ec_inference_wire::Completion, Failure>) -> Value {
    let (text, is_error) = match r {
        Ok(c) => (
            json!({"content": c.content, "stop_reason": c.stop_reason, "model": c.model}),
            false,
        ),
        Err(f) => (json!({"kind": f.kind, "message": f.message}), true),
    };
    json!({"content": [{"type": "text", "text": text.to_string()}], "isError": is_error})
}

/// In-flight request ids on one connection, and whether it has ended. Shared
/// by the writer (this module's thread) and that connection's reader; the core
/// never sees it.
#[derive(Default)]
struct Shared {
    ids: HashSet<u64>,
    dead: bool,
}

struct Conn {
    w: UnixStream,
    shared: Arc<Mutex<Shared>>,
}

impl Conn {
    fn is_dead(&self) -> bool {
        self.shared.lock().map_or(true, |s| s.dead)
    }

    /// Writes one message; false when the connection is unusable (it is then
    /// shut down, and its reader fails everything still in flight).
    fn send(&mut self, m: &ToRouter) -> bool {
        if let ToRouter::Complete(r) = m {
            let Ok(mut s) = self.shared.lock() else {
                return false;
            };
            if s.dead {
                return false;
            }
            s.ids.insert(r.id);
        }
        if write_frame(&mut self.w, &m.to_json()).is_ok() {
            return true;
        }
        if let ToRouter::Complete(r) = m {
            if let Ok(mut s) = self.shared.lock() {
                s.ids.remove(&r.id);
            }
        }
        let _ = self.w.shutdown(std::net::Shutdown::Both);
        false
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        // Unblocks the reader so its thread ends with ours.
        let _ = self.w.shutdown(std::net::Shutdown::Both);
    }
}

fn connect(path: &Path, core: &Sender<Msg>) -> Option<Conn> {
    let s = UnixStream::connect(path).ok()?;
    // The conversation goes to whoever answers here: only our own user.
    if peer_cred(&s).is_none_or(|(uid, _)| uid != our_uid()) {
        return None;
    }
    let mut r = s.try_clone().ok()?;
    let shared = Arc::new(Mutex::new(Shared::default()));
    let (sh, core) = (shared.clone(), core.clone());
    std::thread::spawn(move || {
        // A frame that does not parse is skipped: it names no call we could
        // fail, and the router's own timeout answers that call.
        while let Ok(Some(v)) = read_frame(&mut r) {
            let Some(reply) = Reply::from_json(&v) else {
                continue;
            };
            let ours = sh.lock().is_ok_and(|mut s| s.ids.remove(&reply.id));
            if ours
                && core
                    .send(Msg::Inference {
                        id: reply.id,
                        result: reply.result,
                    })
                    .is_err()
            {
                return;
            }
        }
        let gone = match sh.lock() {
            Ok(mut s) => {
                s.dead = true;
                std::mem::take(&mut s.ids)
            }
            Err(_) => HashSet::new(),
        };
        for id in gone {
            let _ = core.send(Msg::Inference {
                id,
                result: Err(failure(
                    "backend_unavailable",
                    "the inference router (ec-inferenced) went away",
                )),
            });
        }
    });
    Some(Conn { w: s, shared })
}

/// Starts the satellite. The returned sender is the core's only handle; when
/// it is dropped the thread and its connection end.
pub fn spawn(path: PathBuf, core: Sender<Msg>) -> Sender<ToRouter> {
    let (tx, rx) = channel::<ToRouter>();
    std::thread::spawn(move || {
        let mut conn: Option<Conn> = None;
        for cmd in rx {
            if conn.as_ref().is_some_and(Conn::is_dead) {
                conn = None;
            }
            match cmd {
                ToRouter::Complete(r) => {
                    let id = r.id;
                    if conn.is_none() {
                        conn = connect(&path, &core);
                    }
                    let sent = match conn.as_mut() {
                        Some(c) => c.send(&ToRouter::Complete(r)),
                        None => false,
                    };
                    if !sent {
                        conn = None;
                        let _ = core.send(Msg::Inference {
                            id,
                            result: Err(failure("backend_unavailable", DOWN_MESSAGE)),
                        });
                    }
                }
                // Nothing is held for a task on a router we are not connected
                // to, so a close never dials one.
                close @ ToRouter::Close { .. } => {
                    if conn.as_mut().is_some_and(|c| !c.send(&close)) {
                        conn = None;
                    }
                }
            }
        }
    });
    tx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_validated() {
        let ok = parse_args(&json!({"messages": [{"role": "user", "content": "hi"}]})).unwrap();
        assert_eq!(ok.max_tokens, MAX_TOKENS_DEFAULT);
        assert_eq!(ok.system, "");
        assert_eq!(ok.tools, json!([]));
        let m = json!([{"role": "user", "content": "hi"}]);
        for bad in [
            json!("x"),
            json!({}),
            json!({"messages": []}),
            json!({"messages": "hi"}),
            json!({"messages": ["hi"]}),
            json!({"messages": m, "system": 3}),
            json!({"messages": m, "tools": {}}),
            json!({"messages": m, "tools": ["x"]}),
            json!({"messages": m, "max_tokens": 0}),
            json!({"messages": m, "max_tokens": 64001}),
            json!({"messages": m, "max_tokens": -1}),
            json!({"messages": m, "max_tokens": "9"}),
            json!({"messages": m, "model": "x"}),
            json!({"messages": m, "backend": "api"}),
        ] {
            assert!(parse_args(&bad).is_err(), "{bad}");
        }
        let top =
            parse_args(&json!({"messages": m, "max_tokens": 64000, "system": "s", "tools": [{"name": "t"}]}));
        assert_eq!(top.unwrap().max_tokens, 64000);
    }
}
