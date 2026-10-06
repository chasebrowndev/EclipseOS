// SPDX-License-Identifier: AGPL-3.0-only
//! The task model and its on-disk form (A-08 §6, §11).
//!
//! `$XDG_STATE_HOME/eclipse/conversations/<task_id>/` holds `meta.json` (the
//! task, rewritten atomically) and `messages.jsonl` (append-only, one
//! `conversation.v1` message per line). The directory is 0700, files 0600.
//! Nothing here is hashed or signed, so plain JSON is fine.

use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// A-08 §6.1 `kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Say,
    Ask,
    Human,
    Context,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Say => "say",
            Kind::Ask => "ask",
            Kind::Human => "human",
            Kind::Context => "context",
        }
    }
    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "say" => Kind::Say,
            "ask" => Kind::Ask,
            "human" => Kind::Human,
            "context" => Kind::Context,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub msg_id: u64,
    pub time_ms: u64,
    pub kind: Kind,
    pub text: String,
    pub reply_to: Option<u64>,
    /// Provenance summary (A-08 §6): the lowest trust on the poster's chain
    /// and the head source.
    pub min_trust: String,
    pub head: String,
}

impl Message {
    pub fn to_json(&self) -> Value {
        let mut v = json!({
            "schema": "conversation.v1",
            "msg_id": self.msg_id,
            "time_ms": self.time_ms,
            "kind": self.kind.as_str(),
            "text": self.text,
            "trust": {"min_trust": self.min_trust, "head": self.head},
        });
        if let Some(r) = self.reply_to {
            v["reply_to"] = json!(r);
        }
        v
    }

    pub fn from_json(v: &Value) -> Option<Message> {
        Some(Message {
            msg_id: v.get("msg_id")?.as_u64()?,
            time_ms: v.get("time_ms")?.as_u64()?,
            kind: Kind::parse(v.get("kind")?.as_str()?)?,
            text: v.get("text")?.as_str()?.to_owned(),
            reply_to: v.get("reply_to").and_then(Value::as_u64),
            min_trust: v.get("trust")?.get("min_trust")?.as_str()?.to_owned(),
            head: v.get("trust")?.get("head")?.as_str()?.to_owned(),
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Counters {
    pub messages_posted: u64,
    pub bytes_posted: u64,
    pub human_posts: u64,
    pub quota_exceeded: u64,
}

#[derive(Debug, Clone)]
pub struct Task {
    pub id: String,
    pub principal: String,
    pub package: String,
    pub version: String,
    pub statement: String,
    /// Absolute deadline, epoch ms, as policyd provisioned it.
    pub deadline_ms: u64,
    pub continuation: String,
    /// `active`, `paused`, `draining` or `closed` (A-04 §4).
    pub state: String,
    pub reason: String,
    pub awaiting_reply: bool,
    pub started_ms: u64,
    pub closed_ms: Option<u64>,
    pub min_trust: String,
    /// A drain cancel was acknowledged: the agent's inbox reports closed.
    pub drain_requested: bool,
    pub counters: Counters,
    pub msgs: Vec<Message>,
    pub next_msg: u64,
    /// (time_ms, bytes) of agent posts in the last minute, for A-03 §7.
    pub window: VecDeque<(u64, u64)>,
}

impl Task {
    pub fn new(id: &str, now: u64) -> Task {
        Task {
            id: id.to_owned(),
            principal: String::new(),
            package: String::new(),
            version: String::new(),
            statement: String::new(),
            deadline_ms: 0,
            continuation: String::new(),
            state: "active".into(),
            reason: String::new(),
            awaiting_reply: false,
            started_ms: now,
            closed_ms: None,
            min_trust: "standard".into(),
            drain_requested: false,
            counters: Counters::default(),
            msgs: Vec::new(),
            next_msg: 1,
            window: VecDeque::new(),
        }
    }

    pub fn is_closed(&self) -> bool {
        self.state == "closed"
    }

    /// What the agent's inbox calls closed: closed, or a drain in progress.
    pub fn inbox_closed(&self) -> bool {
        self.is_closed() || self.state == "draining" || self.drain_requested
    }

    /// The task as the console sees it (A-08 §7). No grant contents.
    pub fn to_json(&self) -> Value {
        json!({
            "task_id": self.id,
            "package": self.package,
            "version": self.version,
            "statement": self.statement,
            "state": self.state,
            "reason": self.reason,
            "deadline_ms": self.deadline_ms,
            "started_ms": self.started_ms,
            "closed_ms": self.closed_ms,
            "depth": 0,
            "continuation": self.continuation,
            "awaiting_reply": self.awaiting_reply,
            "pending_decisions": 0,
            "min_trust": self.min_trust,
            "counters": {
                "messages_posted": self.counters.messages_posted,
                "bytes_posted": self.counters.bytes_posted,
                "human_posts": self.counters.human_posts,
                "quota_exceeded": self.counters.quota_exceeded,
                "messages": self.msgs.len(),
            },
        })
    }

    fn meta_json(&self) -> Value {
        let mut v = self.to_json();
        v["principal"] = json!(self.principal);
        v["drain_requested"] = json!(self.drain_requested);
        v["next_msg"] = json!(self.next_msg);
        v
    }

    fn from_meta(v: &Value) -> Option<Task> {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        let u = |k: &str| v.get(k).and_then(Value::as_u64);
        let c = v.get("counters")?;
        let cu = |k: &str| c.get(k).and_then(Value::as_u64).unwrap_or(0);
        let mut t = Task::new(&s("task_id")?, u("started_ms")?);
        t.principal = s("principal")?;
        t.package = s("package")?;
        t.version = s("version")?;
        t.statement = s("statement")?;
        t.deadline_ms = u("deadline_ms").unwrap_or(0);
        t.continuation = s("continuation").unwrap_or_default();
        t.state = s("state")?;
        t.reason = s("reason").unwrap_or_default();
        t.awaiting_reply = v.get("awaiting_reply").and_then(Value::as_bool).unwrap_or(false);
        t.closed_ms = u("closed_ms");
        t.min_trust = s("min_trust").unwrap_or_else(|| "standard".into());
        t.drain_requested = v.get("drain_requested").and_then(Value::as_bool).unwrap_or(false);
        t.next_msg = u("next_msg").unwrap_or(1);
        t.counters = Counters {
            messages_posted: cu("messages_posted"),
            bytes_posted: cu("bytes_posted"),
            human_posts: cu("human_posts"),
            quota_exceeded: cu("quota_exceeded"),
        };
        Some(t)
    }
}

/// The conversation store under `$XDG_STATE_HOME/eclipse/conversations/`.
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(state_dir: &Path) -> Store {
        Store {
            root: state_dir.join("conversations"),
        }
    }

    fn dir(&self, task: &str) -> PathBuf {
        self.root.join(task)
    }

    /// `task` must already be a validated ULID; this never sees console input.
    pub fn ensure(&self, task: &str) -> std::io::Result<()> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(self.dir(task))
    }

    pub fn write_meta(&self, t: &Task) -> std::io::Result<()> {
        self.ensure(&t.id)?;
        let dir = self.dir(&t.id);
        let tmp = dir.join("meta.json.tmp");
        {
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(t.meta_json().to_string().as_bytes())?;
            f.sync_data()?;
        }
        fs::rename(tmp, dir.join("meta.json"))
    }

    pub fn append(&self, task: &str, m: &Message) -> std::io::Result<()> {
        self.ensure(task)?;
        let mut f = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(self.dir(task).join("messages.jsonl"))?;
        let mut line = m.to_json().to_string();
        line.push('\n');
        f.write_all(line.as_bytes())
    }

    pub fn delete(&self, task: &str) {
        let _ = fs::remove_dir_all(self.dir(task));
    }

    /// Every task on disk with its transcript. Unreadable entries are skipped.
    pub fn load_all(&self) -> Vec<Task> {
        let mut out = Vec::new();
        let Ok(rd) = fs::read_dir(&self.root) else {
            return out;
        };
        for e in rd.flatten() {
            let dir = e.path();
            let Ok(meta) = fs::read_to_string(dir.join("meta.json")) else {
                continue;
            };
            let Some(mut t) = serde_json::from_str::<Value>(&meta)
                .ok()
                .and_then(|v| Task::from_meta(&v))
            else {
                continue;
            };
            if let Ok(log) = fs::read_to_string(dir.join("messages.jsonl")) {
                for line in log.lines() {
                    if let Some(m) = serde_json::from_str::<Value>(line)
                        .ok()
                        .and_then(|v| Message::from_json(&v))
                    {
                        t.next_msg = t.next_msg.max(m.msg_id + 1);
                        t.msgs.push(m);
                    }
                }
            }
            out.push(t);
        }
        out
    }

    /// Whether the store's files are private (used by tests).
    pub fn mode_of(&self, task: &str, file: &str) -> Option<u32> {
        let p = if file.is_empty() {
            self.dir(task)
        } else {
            self.dir(task).join(file)
        };
        fs::metadata(p).ok().map(|m| m.permissions().mode() & 0o777)
    }
}
