// SPDX-License-Identifier: AGPL-3.0-only
//! The session record (A-08 §5.4, Appendix F-21, F-23).
//!
//! `$XDG_STATE_HOME/eclipse/sessions/<task_id>/record.jsonl` holds the
//! model-visible history of a task as it passed through agentd, in order, one
//! JSON entry per line. Directory 0700, file 0600, append-only, outside every
//! sandbox. Retention is the transcript's (A-08 §11); `delete_session` removes
//! it with the conversation.
//!
//! What is recorded today: every MCP request and response on the task's
//! socket (tool calls and their results, `initialize`, `tools/list`) and every
//! conversation message, as [`entry`] builds them. An `inference.complete`
//! call and its answer are MCP traffic like the rest, so a model's reply is in
//! the record; what an agent keeps in its own context beyond that is not
//! (docs/KNOWNBUGS.md AGENTD-01).
//!
//! `secret`-class content never reaches the record (A-08 §5.4, S-05 §2). It
//! cannot reach agentd at all today: agents see only what the compositor's
//! policy lets through at `private` or below. When classified content starts
//! to flow through agentd, the filter belongs in [`Sessions::append`]'s
//! caller, before the entry is built, never after.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// The marker `session.restore` ends with (F-23): after it every handle and
/// revision from the restored session is invalid.
pub const BOUNDARY_TEXT: &str = "The environment changed: every handle and revision from the restored session is invalid. Re-observe before acting.";

/// A record stops growing here: a looping agent must not fill the disk.
pub const MAX_RECORD_BYTES: u64 = 32 * 1024 * 1024;

/// One record entry. `kind` is `mcp_request`, `mcp_response`, `message` or
/// `boundary`.
pub fn entry(kind: &str, time_ms: u64, body: Value) -> Value {
    let mut v = json!({"kind": kind, "time_ms": time_ms});
    if let (Some(o), Some(b)) = (v.as_object_mut(), body.as_object()) {
        for (k, x) in b {
            o.insert(k.clone(), x.clone());
        }
    }
    v
}

pub fn boundary() -> Value {
    json!({"kind": "boundary", "text": BOUNDARY_TEXT})
}

pub struct Sessions {
    root: PathBuf,
}

impl Sessions {
    pub fn new(state_dir: &Path) -> Sessions {
        Sessions {
            root: state_dir.join("sessions"),
        }
    }

    fn dir(&self, task: &str) -> PathBuf {
        self.root.join(task)
    }

    fn file(&self, task: &str) -> PathBuf {
        self.dir(task).join("record.jsonl")
    }

    /// Appends one entry. `Ok(false)` when the record is full and the entry
    /// was dropped. `task` is a validated ULID.
    pub fn append(&self, task: &str, e: &Value) -> std::io::Result<bool> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(self.dir(task))?;
        let mut f = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(self.file(task))?;
        if f.metadata()?.len() >= MAX_RECORD_BYTES {
            return Ok(false);
        }
        let mut line = e.to_string();
        line.push('\n');
        f.write_all(line.as_bytes())?;
        Ok(true)
    }

    /// The entries in order, or `None` when the task has no record.
    pub fn read(&self, task: &str) -> Option<Vec<Value>> {
        let f = fs::File::open(self.file(task)).ok()?;
        Some(
            BufReader::new(f)
                .lines()
                .map_while(Result::ok)
                .filter_map(|l| serde_json::from_str(&l).ok())
                .collect(),
        )
    }

    pub fn exists(&self, task: &str) -> bool {
        self.file(task).is_file()
    }

    pub fn delete(&self, task: &str) {
        let _ = fs::remove_dir_all(self.dir(task));
    }

    /// Permission bits of the record directory (`""`) or its file, for tests.
    pub fn mode_of(&self, task: &str, file: &str) -> Option<u32> {
        let p = if file.is_empty() {
            self.dir(task)
        } else {
            self.dir(task).join(file)
        };
        fs::metadata(p).ok().map(|m| m.permissions().mode() & 0o777)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_only_private_and_ordered() {
        let tmp = crate::scratch_dir("sess");
        let s = Sessions::new(&tmp);
        assert!(!s.exists("T") && s.read("T").is_none());
        for i in 0..3 {
            assert!(s.append("T", &entry("message", i, json!({"n": i}))).unwrap());
        }
        let r = s.read("T").unwrap();
        assert_eq!(
            r.iter().map(|e| e["n"].as_u64().unwrap()).collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert_eq!(r[0]["kind"], "message");
        assert_eq!(s.mode_of("T", ""), Some(0o700));
        assert_eq!(s.mode_of("T", "record.jsonl"), Some(0o600));
        s.delete("T");
        assert!(!s.exists("T"));
    }

    #[test]
    fn a_full_record_drops_entries() {
        let tmp = crate::scratch_dir("sess-full");
        let s = Sessions::new(&tmp);
        s.append("T", &json!({"k": 1})).unwrap();
        let f = fs::OpenOptions::new().append(true).open(s.file("T")).unwrap();
        f.set_len(MAX_RECORD_BYTES).unwrap();
        assert!(!s.append("T", &json!({"k": 2})).unwrap());
    }
}
