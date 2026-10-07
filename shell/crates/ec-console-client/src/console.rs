// SPDX-License-Identifier: AGPL-3.0-only
//! Client for agentd's console socket (A-08 §7, console-plan C5).
//!
//! `$XDG_RUNTIME_DIR/eclipse/console.sock`, line-delimited JSON-RPC 2.0. No
//! async runtime: one reader thread demultiplexes replies (to the waiting
//! caller) from notifications (to [`Console::events`]), so a GUI can call from
//! its update function and drain events from a subscription.
//!
//! The console carries no authority (A-08 §1). There is deliberately no
//! `create_task`, `unpause_task`, `resume_session` or `answer_prompt` here:
//! the server does not have them either.
//!
//! Replies are parsed by hand from `serde_json::Value`, tolerantly: a missing
//! field takes a default and an unknown one is ignored, so a server that adds a
//! field does not break a console that predates it.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{channel, Receiver, Sender},
        Arc, Mutex,
    },
    time::Duration,
};

use serde_json::{json, Value};

/// How long a request waits for its reply.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// `$XDG_RUNTIME_DIR/eclipse/console.sock`.
pub fn default_socket() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")?;
    Some(PathBuf::from(base).join("eclipse").join("console.sock"))
}

#[derive(Debug)]
pub enum Error {
    Connect(String),
    Io(std::io::Error),
    /// The server answered with a JSON-RPC error.
    Rpc {
        code: i64,
        message: String,
        data: Option<Value>,
    },
    /// The connection ended, or the reader thread is gone.
    Disconnected,
    /// No reply within [`CALL_TIMEOUT`].
    Timeout,
    /// A reply that is not the shape the contract promises.
    Protocol(String),
}

/// Server error codes (agentd's console.sock). Matched by code, never by text.
pub mod code {
    /// A task id that does not exist or is not the owner's: the same answer
    /// for both (C5), so a console cannot probe for other tasks.
    pub const NOT_FOUND: i64 = -32002;
    pub const UNAVAILABLE: i64 = -32003;
    pub const RATE_LIMITED: i64 = -32004;
    pub const CLOSED: i64 = -32006;
    pub const DENIED: i64 = -32000;
}

impl Error {
    fn code(&self) -> Option<i64> {
        match self {
            Error::Rpc { code, .. } => Some(*code),
            _ => None,
        }
    }

    pub fn is_not_found(&self) -> bool {
        self.code() == Some(code::NOT_FOUND)
    }

    /// Rate limited (`show_decisions` is 1/s).
    pub fn is_rate_limited(&self) -> bool {
        self.code() == Some(code::RATE_LIMITED)
    }

    /// policyd or the compositor is not reachable.
    pub fn is_unavailable(&self) -> bool {
        self.code() == Some(code::UNAVAILABLE)
    }

    /// The task's conversation is closed to new posts.
    pub fn is_closed(&self) -> bool {
        self.code() == Some(code::CLOSED)
    }

    pub fn is_denied(&self) -> bool {
        self.code() == Some(code::DENIED)
    }

    /// `data.reason`: for a denial, policyd's reason code.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Error::Rpc { data: Some(d), .. } => d.get("reason").and_then(Value::as_str),
            _ => None,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Connect(m) => write!(f, "cannot reach agentd: {m}"),
            Error::Io(e) => write!(f, "{e}"),
            Error::Rpc { code, message, .. } => write!(f, "{message} (code {code})"),
            Error::Disconnected => write!(f, "agentd connection closed"),
            Error::Timeout => write!(f, "agentd did not answer in time"),
            Error::Protocol(m) => write!(f, "protocol error: {m}"),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

// ------------------------------------------------------------------ values

/// A message id: an integer, per task.
pub type MsgId = u64;

/// Provenance summary of a message (A-08 §6): `min_trust` over the chain and
/// the head source. Agent text is untrusted regardless.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Trust {
    pub min_trust: String,
    pub head: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub task_id: String,
    pub msg_id: MsgId,
    pub time_ms: u64,
    /// `say` | `ask` | `human` | `context` (`conversation.v1`).
    pub kind: String,
    pub text: String,
    pub reply_to: Option<MsgId>,
    pub trust: Trust,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Package {
    pub id: String,
    pub name: String,
    pub publisher: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub task_id: String,
    pub package: String,
    pub version: String,
    pub statement: String,
    /// `active` | `paused` | `draining` | `closed`.
    pub state: String,
    pub reason: String,
    /// Absolute epoch milliseconds.
    pub deadline_ms: u64,
    pub started_ms: u64,
    pub closed_ms: Option<u64>,
    pub continuation: String,
    pub min_trust: String,
    pub depth: u64,
    pub awaiting_reply: bool,
    pub pending_decisions: u64,
    pub parent: Option<String>,
    /// The subtree, present on `get_task`.
    pub children: Vec<Task>,
    /// The whole object, for fields this crate does not name (counters).
    pub raw: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub task_id: String,
    pub package: String,
    pub statement: String,
    pub reason: String,
    pub closed_ms: u64,
    pub min_trust: String,
}

/// `list_tasks` result.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskList {
    pub tasks: Vec<Task>,
    /// False while agentd cannot reach policyd; the console says so.
    pub available: bool,
    pub decisions_pending: u32,
}

/// `conversation_read` result.
#[derive(Debug, Clone, PartialEq)]
pub struct Conversation {
    pub messages: Vec<Message>,
    pub closed: bool,
    pub awaiting_reply: bool,
}

/// `conversation_post` result.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Posted {
    pub msg_id: MsgId,
    pub time_ms: u64,
}

/// `subscribe` result.
#[derive(Debug, Clone, PartialEq)]
pub struct Subscribed {
    pub events: Vec<String>,
    pub available: bool,
    pub decisions_pending: u32,
}

/// `cancel_task` mode (A-04 §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelMode {
    Drain,
    Immediate,
}

impl CancelMode {
    fn as_str(self) -> &'static str {
        match self {
            CancelMode::Drain => "drain",
            CancelMode::Immediate => "immediate",
        }
    }
}

fn s(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(x)) => x.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn u(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}

fn task_id_of(v: &Value) -> String {
    let id = s(v, "task_id");
    if id.is_empty() {
        s(v, "id")
    } else {
        id
    }
}

fn trust_of(v: &Value) -> Trust {
    match v.get("trust") {
        Some(t) => Trust {
            min_trust: s(t, "min_trust"),
            head: s(t, "head"),
        },
        None => Trust::default(),
    }
}

fn message_of(v: &Value, fallback_task: &str) -> Message {
    let tid = task_id_of(v);
    Message {
        task_id: if tid.is_empty() {
            fallback_task.to_owned()
        } else {
            tid
        },
        msg_id: u(v, "msg_id"),
        time_ms: u(v, "time_ms"),
        kind: s(v, "kind"),
        text: s(v, "text"),
        reply_to: v.get("reply_to").and_then(Value::as_u64),
        trust: trust_of(v),
    }
}

impl Task {
    pub fn from_json(v: &Value) -> Task {
        let children = ["children", "subtree"]
            .iter()
            .find_map(|k| v.get(*k).and_then(Value::as_array))
            .map(|a| a.iter().map(Task::from_json).collect())
            .unwrap_or_default();
        let parent = v
            .get("parent")
            .or_else(|| v.get("parent_id"))
            .and_then(Value::as_str)
            .filter(|p| !p.is_empty())
            .map(str::to_owned);
        Task {
            task_id: task_id_of(v),
            package: s(v, "package"),
            version: s(v, "version"),
            statement: s(v, "statement"),
            state: s(v, "state"),
            reason: s(v, "reason"),
            deadline_ms: u(v, "deadline_ms"),
            started_ms: u(v, "started_ms"),
            closed_ms: v.get("closed_ms").and_then(Value::as_u64),
            continuation: s(v, "continuation"),
            min_trust: s(v, "min_trust"),
            depth: u(v, "depth"),
            awaiting_reply: v.get("awaiting_reply").and_then(Value::as_bool).unwrap_or(false),
            pending_decisions: u(v, "pending_decisions"),
            parent,
            children,
            raw: v.clone(),
        }
    }
}

/// A list that may arrive bare or under `key` (the contract names the items,
/// not the envelope).
fn list<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.as_array()
        .or_else(|| v.get(key).and_then(Value::as_array))
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

// ------------------------------------------------------------------ events

/// Notifications after `subscribe` (C5).
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    TaskStarted {
        task_id: String,
        package: String,
        statement: String,
        deadline_ms: u64,
    },
    TaskState {
        task_id: String,
        state: String,
        reason: String,
    },
    Message(Message),
    AwaitingReply {
        task_id: String,
        awaiting: bool,
    },
    DecisionsPending {
        count: u32,
    },
    TaskClosed {
        task_id: String,
        reason: String,
    },
    /// An event name this crate does not know.
    Unknown {
        name: String,
        data: Value,
    },
    /// The connection ended. Last event on the channel.
    Disconnected,
}

impl Event {
    /// Parse one notification `{method, params}`. Two framings are accepted:
    /// `method: "event"` with `params: {event, data}` (as abyss's socket does)
    /// and `method: <event name>` with `params: <payload>`.
    pub fn from_notification(msg: &Value) -> Option<Event> {
        let method = msg.get("method")?.as_str()?;
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let (name, data) = if method == "event" {
            (
                params.get("event")?.as_str()?.to_owned(),
                params.get("data").cloned().unwrap_or(Value::Null),
            )
        } else {
            (method.to_owned(), params)
        };
        Some(match name.as_str() {
            "task_started" => Event::TaskStarted {
                task_id: task_id_of(&data),
                package: s(&data, "package"),
                statement: s(&data, "statement"),
                deadline_ms: u(&data, "deadline_ms"),
            },
            "task_state" => Event::TaskState {
                task_id: task_id_of(&data),
                state: s(&data, "state"),
                reason: s(&data, "reason"),
            },
            "message" => Event::Message(message_of(&data, "")),
            "awaiting_reply" => Event::AwaitingReply {
                task_id: task_id_of(&data),
                awaiting: data.get("awaiting").and_then(Value::as_bool).unwrap_or(false),
            },
            "decisions_pending" => Event::DecisionsPending {
                count: u32::try_from(u(&data, "count")).unwrap_or(u32::MAX),
            },
            "task_closed" => Event::TaskClosed {
                task_id: task_id_of(&data),
                reason: s(&data, "reason"),
            },
            _ => Event::Unknown { name, data },
        })
    }
}

// ------------------------------------------------------------------ client

type Reply = std::result::Result<Value, Error>;
type Pending = Arc<Mutex<HashMap<u64, Sender<Reply>>>>;

pub struct Console {
    writer: Mutex<UnixStream>,
    pending: Pending,
    next_id: AtomicU64,
    events: Receiver<Event>,
}

impl Console {
    pub fn connect() -> Result<Console> {
        let path = default_socket().ok_or_else(|| Error::Connect("XDG_RUNTIME_DIR is not set".into()))?;
        Console::connect_to(&path)
    }

    pub fn connect_to(path: &Path) -> Result<Console> {
        let sock =
            UnixStream::connect(path).map_err(|e| Error::Connect(format!("{}: {e}", path.display())))?;
        Console::from_stream(sock)
    }

    /// Wrap an already-connected stream (a socketpair in tests).
    pub fn from_stream(sock: UnixStream) -> Result<Console> {
        let reader = sock.try_clone()?;
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (tx, events) = channel();
        let p = Arc::clone(&pending);
        std::thread::Builder::new()
            .name("console-reader".into())
            .spawn(move || read_loop(reader, &p, &tx))?;
        Ok(Console {
            writer: Mutex::new(sock),
            pending,
            next_id: AtomicU64::new(1),
            events,
        })
    }

    /// Notifications. The last one is [`Event::Disconnected`].
    pub fn events(&self) -> &Receiver<Event> {
        &self.events
    }

    /// One request, blocking until its reply.
    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = channel();
        self.pending
            .lock()
            .map_err(|_| Error::Disconnected)?
            .insert(id, tx);
        let mut line = request_line(id, method, params);
        line.push('\n');
        let sent = self
            .writer
            .lock()
            .map_err(|_| Error::Disconnected)
            .and_then(|mut w| {
                w.write_all(line.as_bytes())
                    .and_then(|()| w.flush())
                    .map_err(Error::Io)
            });
        if let Err(e) = sent {
            self.forget(id);
            return Err(e);
        }
        match rx.recv_timeout(CALL_TIMEOUT) {
            Ok(r) => r,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                self.forget(id);
                Err(Error::Timeout)
            }
            Err(_) => Err(Error::Disconnected),
        }
    }

    fn forget(&self, id: u64) {
        if let Ok(mut p) = self.pending.lock() {
            p.remove(&id);
        }
    }

    // ---- A-08 §7, one method each

    pub fn list_packages(&self) -> Result<Vec<Package>> {
        let r = self.call("list_packages", Value::Null)?;
        Ok(list(&r, "packages")
            .iter()
            .map(|p| Package {
                id: s(p, "id"),
                name: s(p, "name"),
                publisher: s(p, "publisher"),
                version: s(p, "version"),
            })
            .collect())
    }

    /// `state` filters server-side (`active`, `paused`, ...); `None` is all.
    pub fn list_tasks(&self, state: Option<&str>) -> Result<TaskList> {
        let r = self.call("list_tasks", list_tasks_params(state))?;
        Ok(TaskList {
            tasks: list(&r, "tasks").iter().map(Task::from_json).collect(),
            available: r.get("available").and_then(Value::as_bool).unwrap_or(true),
            decisions_pending: u32::try_from(u(&r, "decisions_pending")).unwrap_or(u32::MAX),
        })
    }

    pub fn get_task(&self, task_id: &str) -> Result<Task> {
        let r = self.call("get_task", json!({"task_id": task_id}))?;
        let t = r.get("task").unwrap_or(&r);
        Ok(Task::from_json(t))
    }

    pub fn conversation_read(&self, task_id: &str, since: Option<MsgId>) -> Result<Conversation> {
        let r = self.call("conversation_read", conversation_read_params(task_id, since))?;
        Ok(Conversation {
            messages: list(&r, "messages")
                .iter()
                .map(|m| message_of(m, task_id))
                .collect(),
            closed: r.get("closed").and_then(Value::as_bool).unwrap_or(false),
            awaiting_reply: r.get("awaiting_reply").and_then(Value::as_bool).unwrap_or(false),
        })
    }

    /// Post a human message (`standard` trust, A-08 §6).
    pub fn conversation_post(&self, task_id: &str, text: &str, reply_to: Option<MsgId>) -> Result<Posted> {
        let r = self.call(
            "conversation_post",
            conversation_post_params(task_id, text, reply_to),
        )?;
        Ok(Posted {
            msg_id: u(&r, "msg_id"),
            time_ms: u(&r, "time_ms"),
        })
    }

    pub fn pause_task(&self, task_id: &str) -> Result<()> {
        self.call("pause_task", json!({"task_id": task_id})).map(drop)
    }

    pub fn cancel_task(&self, task_id: &str, mode: CancelMode) -> Result<()> {
        self.call("cancel_task", json!({"task_id": task_id, "mode": mode.as_str()}))
            .map(drop)
    }

    /// Ask the compositor (via agentd) to open the decision queue. 1/s.
    pub fn show_decisions(&self) -> Result<()> {
        self.call("show_decisions", Value::Null).map(drop)
    }

    pub fn list_sessions(&self, since: Option<u64>) -> Result<Vec<Session>> {
        let params = match since {
            Some(s) => json!({"since": s}),
            None => Value::Null,
        };
        let r = self.call("list_sessions", params)?;
        Ok(list(&r, "sessions")
            .iter()
            .map(|v| Session {
                task_id: task_id_of(v),
                package: s(v, "package"),
                statement: s(v, "statement"),
                reason: s(v, "reason"),
                closed_ms: u(v, "closed_ms"),
                min_trust: s(v, "min_trust"),
            })
            .collect())
    }

    pub fn delete_session(&self, task_id: &str) -> Result<()> {
        self.call("delete_session", json!({"task_id": task_id})).map(drop)
    }

    /// Start the event stream; events arrive on [`Console::events`].
    pub fn subscribe(&self) -> Result<Subscribed> {
        let r = self.call("subscribe", Value::Null)?;
        Ok(Subscribed {
            events: r
                .get("events")
                .or_else(|| r.get("subscribed"))
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|e| e.as_str().map(str::to_owned)).collect())
                .unwrap_or_default(),
            available: r.get("available").and_then(Value::as_bool).unwrap_or(true),
            decisions_pending: u32::try_from(u(&r, "decisions_pending")).unwrap_or(u32::MAX),
        })
    }
}

impl Drop for Console {
    /// The reader thread holds a second descriptor, so closing ours would not
    /// end the connection; shut it down explicitly (this also ends that thread).
    fn drop(&mut self) {
        if let Ok(w) = self.writer.lock() {
            let _ = w.shutdown(std::net::Shutdown::Both);
        }
    }
}

fn request_line(id: u64, method: &str, params: Value) -> String {
    let mut req = json!({"jsonrpc": "2.0", "id": id, "method": method});
    if !params.is_null() {
        req["params"] = params;
    }
    req.to_string()
}

fn list_tasks_params(state: Option<&str>) -> Value {
    match state {
        Some(s) => json!({"state": s}),
        None => Value::Null,
    }
}

fn conversation_read_params(task_id: &str, since: Option<MsgId>) -> Value {
    let mut p = json!({"task_id": task_id});
    if let Some(s) = since {
        p["since"] = json!(s);
    }
    p
}

fn conversation_post_params(task_id: &str, text: &str, reply_to: Option<MsgId>) -> Value {
    let mut p = json!({"task_id": task_id, "text": text});
    if let Some(r) = reply_to {
        p["reply_to"] = json!(r);
    }
    p
}

fn error_of(e: &Value) -> Error {
    Error::Rpc {
        code: e.get("code").and_then(Value::as_i64).unwrap_or(0),
        message: s(e, "message"),
        data: e.get("data").cloned(),
    }
}

fn read_loop(sock: UnixStream, pending: &Pending, events: &Sender<Event>) {
    let mut r = BufReader::new(sock);
    let mut line = String::new();
    loop {
        line.clear();
        match r.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let is_reply = msg.get("result").is_some() || msg.get("error").is_some();
        if let (true, Some(id)) = (is_reply, msg.get("id").and_then(Value::as_u64)) {
            let tx = pending.lock().ok().and_then(|mut p| p.remove(&id));
            if let Some(tx) = tx {
                let reply = match msg.get("error") {
                    Some(e) => Err(error_of(e)),
                    None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = tx.send(reply);
            }
        } else if let Some(ev) = Event::from_notification(&msg) {
            if events.send(ev).is_err() {
                // Nobody is listening; replies still matter, keep reading.
            }
        }
    }
    // Wake every caller still waiting, then say goodbye on the event channel.
    if let Ok(mut p) = pending.lock() {
        for (_, tx) in p.drain() {
            let _ = tx.send(Err(Error::Disconnected));
        }
    }
    let _ = events.send(Event::Disconnected);
}

#[cfg(test)]
#[path = "console_tests.rs"]
mod tests;
