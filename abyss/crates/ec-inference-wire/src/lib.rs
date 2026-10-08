// SPDX-License-Identifier: Apache-2.0
//! The contract between `ec-agentd` and `ec-inferenced`, the inference router
//! (I-02, ADR 0076).
//!
//! **Transport.** A unix stream socket at
//! `$XDG_RUNTIME_DIR/eclipse/inferenced.sock`. Every frame is a 4-byte
//! big-endian length followed by that many bytes of one JSON object. A frame
//! over [`MAX_FRAME`] is refused and the connection closed. One connection
//! carries any number of requests; replies carry the request's `id` and may
//! arrive in any order.
//!
//! **Shape.** `messages`, `tools` and `content` are the Anthropic Messages
//! API's own JSON, carried as [`Value`] and not re-modelled here: the router
//! passes them through to the API backend and translates them for the Claude
//! Code backend. `backend` and `model` come from the package manifest via
//! agentd, never from the agent.

use std::io::{self, Read, Write};

pub use serde_json::Value;

/// Largest frame either side accepts: generous for a long conversation, small
/// enough that a confused peer cannot make the other allocate without bound.
pub const MAX_FRAME: usize = 8 * 1024 * 1024;

/// Which backend serves a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// `POST /v1/messages` with the key brokerd releases for
    /// `api.anthropic.com`.
    Api,
    /// A warm, sandboxed `claude -p` session per task.
    ClaudeCode,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Api => "api",
            Backend::ClaudeCode => "claude-code",
        }
    }

    pub fn parse(s: &str) -> Option<Backend> {
        match s {
            "api" => Some(Backend::Api),
            "claude-code" => Some(Backend::ClaudeCode),
            _ => None,
        }
    }
}

/// agentd -> router.
#[derive(Debug, Clone, PartialEq)]
pub enum ToRouter {
    Complete(Request),
    /// The task closed: drop anything held for it (a warm session).
    Close {
        task: String,
    },
}

/// One `inference.complete`.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub id: u64,
    pub task: String,
    pub package: String,
    pub backend: Backend,
    pub model: String,
    pub system: String,
    /// Messages API `messages`: an array.
    pub messages: Value,
    /// Messages API `tools`: an array, possibly empty.
    pub tools: Value,
    pub max_tokens: u32,
    /// The owner's account this task runs under (ADR 0077), or empty for
    /// the default. Chosen by the human when the task was committed and
    /// carried here by agentd from policyd's provision, never from the
    /// agent. [`account_ok`] holds.
    pub account: String,
}

/// An account name (ADR 0077): empty (the default), or 1–32 of
/// `[A-Za-z0-9_-]`. The same rule as `ec_policy_eval::link::account_ok`.
pub fn account_ok(a: &str) -> bool {
    a.len() <= 32
        && a.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The brokerd secret that holds `base` for `account`: `base` itself for the
/// default account, `base.account` otherwise. `None` when `account` breaks
/// [`account_ok`]; `default` names the default account too.
pub fn secret_name(base: &str, account: &str) -> Option<String> {
    if !account_ok(account) {
        return None;
    }
    Some(match account {
        "" | "default" => base.to_owned(),
        a => format!("{base}.{a}"),
    })
}

/// router -> agentd.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub id: u64,
    pub result: Result<Completion, Failure>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Completion {
    /// Messages API `content`: an array of `text` / `tool_use` blocks.
    pub content: Value,
    pub stop_reason: String,
    /// The model that answered (a fallback may differ from the request's).
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// What the backend says the call cost, in US dollars, as Claude Code
    /// reports it (`result.total_cost_usd`; may be per-session cumulative, not
    /// per turn). `None` for the API backend and from an older router. Optional on the wire so old peers still parse;
    /// reserved for the I-02 audit record.
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    /// `no_credential`, `broker_locked`, `provider_error`, `rate_limited`,
    /// `backend_unavailable`, `bad_request`, `timeout`.
    pub kind: String,
    /// For the console, already free of any credential.
    pub message: String,
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k)?.as_str().map(str::to_owned)
}

fn u(v: &Value, k: &str) -> Option<u64> {
    v.get(k)?.as_u64()
}

impl ToRouter {
    pub fn to_json(&self) -> Value {
        match self {
            ToRouter::Complete(r) => serde_json::json!({
                "type": "complete",
                "id": r.id,
                "task": r.task,
                "package": r.package,
                "backend": r.backend.as_str(),
                "model": r.model,
                "system": r.system,
                "messages": r.messages,
                "tools": r.tools,
                "max_tokens": r.max_tokens,
                "account": r.account,
            }),
            ToRouter::Close { task } => serde_json::json!({"type": "close", "task": task}),
        }
    }

    pub fn from_json(v: &Value) -> Option<ToRouter> {
        match v.get("type")?.as_str()? {
            "complete" => {
                let messages = v.get("messages")?.clone();
                let tools = v.get("tools").cloned().unwrap_or(Value::Array(Vec::new()));
                if !messages.is_array() || !tools.is_array() {
                    return None;
                }
                Some(ToRouter::Complete(Request {
                    id: u(v, "id")?,
                    task: s(v, "task")?,
                    package: s(v, "package")?,
                    backend: Backend::parse(v.get("backend")?.as_str()?)?,
                    model: s(v, "model")?,
                    system: s(v, "system").unwrap_or_default(),
                    messages,
                    tools,
                    max_tokens: u32::try_from(u(v, "max_tokens")?).ok()?,
                    account: s(v, "account").unwrap_or_default(),
                }))
                .filter(|m| matches!(m, ToRouter::Complete(r) if account_ok(&r.account)))
            }
            "close" => Some(ToRouter::Close { task: s(v, "task")? }),
            _ => None,
        }
    }
}

impl Reply {
    pub fn to_json(&self) -> Value {
        match &self.result {
            Ok(c) => {
                let mut ok = serde_json::json!({
                    "content": c.content,
                    "stop_reason": c.stop_reason,
                    "model": c.model,
                    "usage": {"input_tokens": c.input_tokens, "output_tokens": c.output_tokens},
                });
                if let Some(cost) = c.cost_usd {
                    ok["cost_usd"] = serde_json::json!(cost);
                }
                serde_json::json!({"id": self.id, "ok": ok})
            }
            Err(f) => serde_json::json!({
                "id": self.id,
                "error": {"kind": f.kind, "message": f.message},
            }),
        }
    }

    pub fn from_json(v: &Value) -> Option<Reply> {
        let id = u(v, "id")?;
        if let Some(ok) = v.get("ok") {
            let content = ok.get("content")?.clone();
            if !content.is_array() {
                return None;
            }
            let usage = ok.get("usage");
            return Some(Reply {
                id,
                result: Ok(Completion {
                    content,
                    stop_reason: s(ok, "stop_reason")?,
                    model: s(ok, "model").unwrap_or_default(),
                    input_tokens: usage.and_then(|x| u(x, "input_tokens")).unwrap_or(0),
                    output_tokens: usage.and_then(|x| u(x, "output_tokens")).unwrap_or(0),
                    cost_usd: ok.get("cost_usd").and_then(Value::as_f64),
                }),
            });
        }
        let e = v.get("error")?;
        Some(Reply {
            id,
            result: Err(Failure {
                kind: s(e, "kind")?,
                message: s(e, "message").unwrap_or_default(),
            }),
        })
    }
}

/// Write one frame.
pub fn write_frame(w: &mut impl Write, v: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(v).map_err(io::Error::other)?;
    if body.len() > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "frame too large"));
    }
    w.write_all(&(body.len() as u32).to_be_bytes())?;
    w.write_all(&body)?;
    w.flush()
}

/// Read one frame. `Ok(None)` at a clean end of stream.
pub fn read_frame(r: &mut impl Read) -> io::Result<Option<Value>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let n = u32::from_be_bytes(len) as usize;
    if n > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut body = vec![0u8; n];
    r.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn req() -> Request {
        Request {
            id: 7,
            task: "01TASK".into(),
            package: "ec-claude-agent".into(),
            backend: Backend::ClaudeCode,
            model: "claude-opus-5-5".into(),
            system: "be brief".into(),
            messages: json!([{"role": "user", "content": "hi"}]),
            tools: json!([]),
            max_tokens: 16000,
            account: "work".into(),
        }
    }

    #[test]
    fn account_names_map_to_secret_names() {
        assert_eq!(
            secret_name("claude-code-token", "").as_deref(),
            Some("claude-code-token")
        );
        assert_eq!(
            secret_name("claude-code-token", "default").as_deref(),
            Some("claude-code-token")
        );
        assert_eq!(
            secret_name("claude-code-token", "work").as_deref(),
            Some("claude-code-token.work")
        );
        assert_eq!(secret_name("claude-code-token", "a.b"), None);
        let mut r = req();
        r.account = "../x".into();
        assert_eq!(ToRouter::from_json(&ToRouter::Complete(r).to_json()), None);
    }

    #[test]
    fn requests_and_replies_round_trip_through_frames() {
        let mut buf = Vec::new();
        let r = ToRouter::Complete(req());
        write_frame(&mut buf, &r.to_json()).unwrap();
        let close = ToRouter::Close {
            task: "01TASK".into(),
        };
        write_frame(&mut buf, &close.to_json()).unwrap();
        let mut rd = &buf[..];
        assert_eq!(
            ToRouter::from_json(&read_frame(&mut rd).unwrap().unwrap()),
            Some(r)
        );
        assert_eq!(
            ToRouter::from_json(&read_frame(&mut rd).unwrap().unwrap()),
            Some(close)
        );
        assert_eq!(read_frame(&mut rd).unwrap(), None);

        for reply in [
            Reply {
                id: 7,
                result: Ok(Completion {
                    content: json!([{"type": "text", "text": "hello"}]),
                    stop_reason: "end_turn".into(),
                    model: "claude-opus-5-5".into(),
                    input_tokens: 3,
                    output_tokens: 1,
                    cost_usd: Some(0.0123),
                }),
            },
            Reply {
                id: 8,
                result: Err(Failure {
                    kind: "broker_locked".into(),
                    message: "unlock with ec-secret unlock".into(),
                }),
            },
        ] {
            assert_eq!(Reply::from_json(&reply.to_json()), Some(reply));
        }
    }

    #[test]
    fn cost_is_optional_on_the_wire() {
        let mut r = Reply {
            id: 1,
            result: Ok(Completion {
                content: json!([]),
                stop_reason: "end_turn".into(),
                model: "m".into(),
                input_tokens: 1,
                output_tokens: 2,
                cost_usd: None,
            }),
        };
        // None is not written at all, and an old peer's reply (no field)
        // parses to None.
        assert!(r.to_json()["ok"].get("cost_usd").is_none());
        assert_eq!(Reply::from_json(&r.to_json()), Some(r.clone()));
        r.result.as_mut().unwrap().cost_usd = Some(0.5);
        assert_eq!(r.to_json()["ok"]["cost_usd"], json!(0.5));
        assert_eq!(Reply::from_json(&r.to_json()), Some(r));
    }

    #[test]
    fn malformed_requests_are_refused() {
        let mut v = ToRouter::Complete(req()).to_json();
        v["backend"] = json!("gpt");
        assert_eq!(ToRouter::from_json(&v), None);
        let mut v = ToRouter::Complete(req()).to_json();
        v["messages"] = json!("not an array");
        assert_eq!(ToRouter::from_json(&v), None);
        assert_eq!(ToRouter::from_json(&json!({"type": "nope"})), None);
    }

    #[test]
    fn an_oversized_frame_is_refused_before_it_is_read() {
        let mut buf = ((MAX_FRAME + 1) as u32).to_be_bytes().to_vec();
        buf.extend_from_slice(b"{}");
        assert!(read_frame(&mut &buf[..]).is_err());
    }
}
