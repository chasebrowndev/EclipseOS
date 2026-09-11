// SPDX-License-Identifier: AGPL-3.0-only
//! The control-socket connection, and the two failures every DE app renders
//! the same way: `DENIED (-32000)` and a config error.

use eclipse_ipc::{Client, Error, EventKind};
use serde_json::{json, Value};

/// A failure worth showing the user. Anything the app can retry silently does
/// not become one of these.
#[derive(Debug, Clone, PartialEq)]
pub enum Problem {
    /// `-32000`. The message carries `{"file","path","reason"}` for a config
    /// write; anything else is shown verbatim.
    Denied { path: Option<String>, reason: String },
    /// The `ConfigError` shape: a parse or validation failure with a position.
    Config {
        file: String,
        line: u64,
        col: u64,
        message: String,
    },
    /// The socket is gone. The client must be rebuilt.
    Disconnected(String),
    /// Everything else.
    Other(String),
}

impl Problem {
    /// Short line for the banner.
    pub fn headline(&self) -> String {
        match self {
            Problem::Denied { path: Some(p), .. } => format!("DENIED  {p}"),
            Problem::Denied { path: None, .. } => "DENIED".into(),
            Problem::Config { file, line, col, .. } if !file.is_empty() => {
                format!("{file}:{line}:{col}")
            }
            Problem::Config { .. } => "invalid value".into(),
            Problem::Disconnected(_) => "the compositor closed the socket".into(),
            Problem::Other(_) => "the compositor rejected that".into(),
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            Problem::Denied { reason, .. } => reason,
            Problem::Config { message, .. } => message,
            Problem::Disconnected(m) | Problem::Other(m) => m,
        }
    }

    /// `RpcError::denied` packs JSON into the message; a denial that does not
    /// is still a denial and is shown as one.
    fn from_denied(message: &str) -> Self {
        match serde_json::from_str::<Value>(message) {
            Ok(v) if v.is_object() => Problem::Denied {
                path: v.get("path").and_then(Value::as_str).map(str::to_owned),
                reason: v
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("edit requires the policy editor")
                    .to_owned(),
            },
            _ => Problem::Denied {
                path: None,
                reason: message.to_owned(),
            },
        }
    }

    /// The `ConfigError` object shape shared by `validate_config` and the
    /// `config-error` event.
    pub fn from_config_error(v: &Value) -> Self {
        Problem::Config {
            file: v
                .get("file")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            line: v.get("line").and_then(Value::as_u64).unwrap_or(0),
            col: v.get("col").and_then(Value::as_u64).unwrap_or(0),
            message: v
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("invalid")
                .to_owned(),
        }
    }

    fn from_error(e: Error) -> Self {
        match e {
            Error::Rpc {
                code: -32000,
                message,
            } => Self::from_denied(&message),
            Error::Rpc { message, .. } => Problem::Other(message),
            Error::Connect(m) => Problem::Disconnected(m),
            Error::Io(e) => Problem::Disconnected(e.to_string()),
            Error::Protocol(m) => Problem::Other(m),
        }
    }
}

/// The socket, plus whatever went wrong reaching it. The app stays up with no
/// compositor so the failure is visible rather than a window that never opens.
pub struct Conn {
    client: Option<Client>,
    pub problem: Option<Problem>,
}

impl Default for Conn {
    fn default() -> Self {
        Self::new()
    }
}

impl Conn {
    pub fn new() -> Self {
        let mut conn = Conn {
            client: None,
            problem: None,
        };
        conn.reconnect();
        conn
    }

    pub fn reconnect(&mut self) {
        match eclipse_ipc::Client::connect() {
            Ok(mut client) => {
                // Output hot-plug (COMP-03) and config errors from someone
                // else editing the file under us both land as events.
                let _ = client.subscribe(&[EventKind::Output, EventKind::ConfigError]);
                self.client = Some(client);
                self.problem = None;
            }
            Err(e) => {
                self.client = None;
                self.problem = Some(Problem::from_error(e));
            }
        }
    }

    pub fn is_connected(&self) -> bool {
        self.client.is_some()
    }

    /// One blocking call. A dropped socket clears the client so the next
    /// interaction reconnects instead of failing forever.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, Problem> {
        let Some(client) = self.client.as_mut() else {
            return Err(self
                .problem
                .clone()
                .unwrap_or_else(|| Problem::Disconnected("not connected".into())));
        };
        match client.call(method, params) {
            Ok(v) => Ok(v),
            Err(e) => {
                let problem = Problem::from_error(e);
                if matches!(problem, Problem::Disconnected(_)) {
                    self.client = None;
                }
                Err(problem)
            }
        }
    }

    /// Drain whatever the compositor has pushed since the last poll.
    pub fn drain_events(&mut self) -> Vec<eclipse_ipc::Event> {
        let mut out = Vec::new();
        let Some(client) = self.client.as_mut() else {
            return out;
        };
        loop {
            match client.poll_event() {
                Ok(Some(ev)) => out.push(ev),
                Ok(None) => break,
                Err(e) => {
                    let problem = Problem::from_error(e);
                    if matches!(problem, Problem::Disconnected(_)) {
                        self.client = None;
                    }
                    self.problem = Some(problem);
                    break;
                }
            }
        }
        out
    }

    /// Every schema row for `abyss.kdl` *and* `policy.kdl` — the policy rows
    /// come back unreadable but present, and this app shows them locked rather
    /// than pretending the setting does not exist.
    pub fn load_schema(&mut self) -> Result<Vec<crate::schema::Row>, Problem> {
        let reply = self.call("get_config", json!({ "schema": true }))?;
        Ok(crate::schema::parse_keys(&reply))
    }

    /// Check a value without writing it. `valid: false` carries the same error
    /// objects the config file itself produces.
    pub fn validate(&mut self, path: &str, value: Value) -> Result<(), Problem> {
        let reply = self.call(
            "validate_config",
            json!({ "file": "abyss", "path": path, "value": value }),
        )?;
        if reply.get("valid").and_then(Value::as_bool) == Some(true) {
            return Ok(());
        }
        let first = reply
            .get("errors")
            .and_then(Value::as_array)
            .and_then(|e| e.first());
        Err(match first {
            Some(v) => Problem::from_config_error(v),
            None => Problem::Other("the value was rejected".into()),
        })
    }

    /// Write one scalar. Returns whether the change needs a restart to apply.
    pub fn set(&mut self, path: &str, value: Value) -> Result<bool, Problem> {
        let reply = self.call("set_config_value", json!({ "path": path, "value": value }))?;
        Ok(reply
            .get("restart_required")
            .and_then(Value::as_bool)
            .unwrap_or(false))
    }
}
