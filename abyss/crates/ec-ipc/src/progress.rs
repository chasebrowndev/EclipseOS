// SPDX-License-Identifier: AGPL-3.0-only
//! Wire contract for the progress feed (`ec-ctl progress …`).
//!
//! This is *not* the compositor's control socket. Progress is a userland
//! concept: the bar's progress widget owns the model, so the feed is a
//! separate per-user socket, `$XDG_RUNTIME_DIR/eclipse/progress.sock`, served
//! by `ec-services`. It is mode 0600 inside a 0700 directory and refuses any
//! peer whose uid is not ours. Nothing here touches `abyss`.
//!
//! One request per line, one reply per line:
//!
//! ```text
//! {"op":"start","id":"build","label":"cargo build","total":1000}
//! {"op":"set","id":"build","fraction":0.4}     or  {"op":"set","id":"build","bytes":400}
//! {"op":"done","id":"build"}
//! {"op":"list"}
//! ```
//!
//! Replies are `{"ok":true}`, `{"ok":true,"jobs":[…]}` or
//! `{"ok":false,"error":"…"}`. Validation happens in [`Request::validate`],
//! which both [`Request::encode`] (the client) and [`Request::decode`] (the
//! server) run, so a bad request is refused on either side.

use std::path::PathBuf;

use serde_json::{json, Value};

/// Longest accepted job id, in bytes.
pub const MAX_ID_LEN: usize = 64;
/// Longest accepted label, in bytes.
pub const MAX_LABEL_LEN: usize = 120;
/// Longest request line the server reads before dropping the peer.
pub const MAX_LINE: usize = 4096;

/// The feed socket's path, or `None` when neither `ECLIPSE_SOCKET_DIR` nor
/// `XDG_RUNTIME_DIR` is set. Same resolution as the control socket.
pub fn socket_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("ECLIPSE_SOCKET_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir).join("progress.sock"));
    }
    std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("eclipse/progress.sock"))
}

/// One feed request. Ids are `[A-Za-z0-9._:-]{1,64}`; labels are printable
/// text up to 120 bytes.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// Open (or restart) a job. `total` is in bytes when present and enables
    /// [`Request::Bytes`]; without it the job is indeterminate until a
    /// fraction arrives.
    Start {
        id: String,
        label: String,
        total: Option<u64>,
    },
    /// Report a fraction in `0.0..=1.0`.
    Fraction {
        id: String,
        fraction: f32,
    },
    /// Report bytes done. Needs a `total` at start.
    Bytes {
        id: String,
        bytes: u64,
    },
    Done {
        id: String,
    },
    List,
}

impl Request {
    /// Refuse anything the server would refuse. Returns the reason.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Start { id, label, .. } => {
                check_id(id)?;
                check_label(label)
            }
            Self::Fraction { id, fraction } => {
                check_id(id)?;
                if fraction.is_finite() && (0.0..=1.0).contains(fraction) {
                    Ok(())
                } else {
                    Err(format!("fraction must be between 0 and 1, got {fraction}"))
                }
            }
            Self::Bytes { id, .. } | Self::Done { id } => check_id(id),
            Self::List => Ok(()),
        }
    }

    /// The request as one JSON line, without the newline.
    pub fn encode(&self) -> Result<String, String> {
        self.validate()?;
        let v = match self {
            Self::Start { id, label, total } => {
                let mut v = json!({"op": "start", "id": id, "label": label});
                if let Some(t) = total {
                    v["total"] = json!(t);
                }
                v
            }
            Self::Fraction { id, fraction } => json!({"op": "set", "id": id, "fraction": fraction}),
            Self::Bytes { id, bytes } => json!({"op": "set", "id": id, "bytes": bytes}),
            Self::Done { id } => json!({"op": "done", "id": id}),
            Self::List => json!({"op": "list"}),
        };
        Ok(v.to_string())
    }

    /// Parse and validate one request line.
    pub fn decode(line: &str) -> Result<Self, String> {
        let v: Value = serde_json::from_str(line).map_err(|e| format!("bad json: {e}"))?;
        let op = v.get("op").and_then(Value::as_str).ok_or("missing op")?;
        let id = || {
            v.get("id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| "missing id".to_owned())
        };
        let req = match op {
            "start" => Self::Start {
                id: id()?,
                label: v
                    .get("label")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or("missing label")?,
                total: match v.get("total") {
                    None | Some(Value::Null) => None,
                    Some(t) => Some(t.as_u64().ok_or("total must be a whole number of bytes")?),
                },
            },
            "set" => {
                if let Some(f) = v.get("fraction") {
                    Self::Fraction {
                        id: id()?,
                        fraction: f.as_f64().ok_or("fraction must be a number")? as f32,
                    }
                } else if let Some(b) = v.get("bytes") {
                    Self::Bytes {
                        id: id()?,
                        bytes: b.as_u64().ok_or("bytes must be a whole number")?,
                    }
                } else {
                    return Err("set needs fraction or bytes".into());
                }
            }
            "done" => Self::Done { id: id()? },
            "list" => Self::List,
            other => return Err(format!("unknown op {other:?}")),
        };
        req.validate()?;
        Ok(req)
    }
}

/// A reply that carries no data.
pub fn ok() -> Value {
    json!({"ok": true})
}

/// A refusal, with the reason a human can read.
pub fn err(reason: impl Into<String>) -> Value {
    json!({"ok": false, "error": reason.into()})
}

fn check_id(id: &str) -> Result<(), String> {
    let ok = !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "id must be 1-{MAX_ID_LEN} characters of [A-Za-z0-9._:-], got {id:?}"
        ))
    }
}

fn check_label(label: &str) -> Result<(), String> {
    if label.is_empty() || label.len() > MAX_LABEL_LEN {
        return Err(format!("label must be 1-{MAX_LABEL_LEN} bytes"));
    }
    if label.chars().any(char::is_control) {
        return Err("label must not contain control characters".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_request() {
        let cases = [
            Request::Start {
                id: "build.1".into(),
                label: "cargo build".into(),
                total: Some(1000),
            },
            Request::Start {
                id: "x".into(),
                label: "x".into(),
                total: None,
            },
            Request::Fraction {
                id: "x".into(),
                fraction: 0.25,
            },
            Request::Bytes {
                id: "x".into(),
                bytes: 7,
            },
            Request::Done { id: "x".into() },
            Request::List,
        ];
        for r in cases {
            let line = r.encode().unwrap();
            assert_eq!(Request::decode(&line).unwrap(), r, "{line}");
        }
    }

    #[test]
    fn refuses_bad_ids_labels_and_fractions() {
        assert!(Request::Done { id: "".into() }.encode().is_err());
        assert!(Request::Done { id: "a/b".into() }.encode().is_err());
        assert!(Request::Done {
            id: "a".repeat(MAX_ID_LEN + 1)
        }
        .encode()
        .is_err());
        assert!(Request::Start {
            id: "a".into(),
            label: "bad\nlabel".into(),
            total: None
        }
        .encode()
        .is_err());
        for f in [-0.1, 1.5, f32::NAN, f32::INFINITY] {
            assert!(Request::Fraction {
                id: "a".into(),
                fraction: f
            }
            .encode()
            .is_err());
        }
    }

    #[test]
    fn decode_refuses_malformed_lines() {
        for line in [
            "",
            "not json",
            "{}",
            r#"{"op":"nope"}"#,
            r#"{"op":"set","id":"a"}"#,
            r#"{"op":"start","id":"a"}"#,
            r#"{"op":"start","id":"a","label":"l","total":-1}"#,
            r#"{"op":"set","id":"a","fraction":2.0}"#,
        ] {
            assert!(Request::decode(line).is_err(), "{line}");
        }
    }
}
