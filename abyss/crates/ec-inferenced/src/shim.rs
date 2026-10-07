// SPDX-License-Identifier: AGPL-3.0-only
//! The tool shim: how Claude's tool calls become the agent's `tool_use`
//! blocks (ADR 0076).
//!
//! `claude` is started with every built-in tool off and one MCP server,
//! `eclipse`, that is this binary run as `ec-inferenced --tool-shim <sock>`.
//! The shim is a stdio MCP server whose tool list is the *agent's* tool list
//! (the request's `tools`), so the only tools the model can call are the
//! agent's own, which agentd then runs through policy and consent like any
//! other. A call blocks inside the shim until the router hands back the
//! agent's `tool_result`.
//!
//! Two halves live here:
//!
//! * [`Hub`] + [`spawn_accept`]: the router side. One listener per session,
//!   accepting exactly one connection from our uid; after that the socket
//!   file is gone and nothing else can connect.
//! * [`run`]: the shim process (`--tool-shim`).
//!
//! Wire between them: `ec-inference-wire` frames (4-byte length + JSON).
//! shim -> router: `{"hello":true}`, then `{"call":{"id","name","input"}}`.
//! router -> shim: `{"tools":[...]}` once, then
//! `{"result":{"id","text","is_error"}}` per call.

use ec_inference_wire::{read_frame, write_frame, Value};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// The MCP server name in `mcp.json`; `claude` prefixes every tool with
/// `mcp__<server>__`.
pub const SERVER_NAME: &str = "eclipse";
pub const TOOL_PREFIX: &str = "mcp__eclipse__";

/// A poisoned lock only means another thread panicked; the data here is plain
/// maps and flags, so carry on rather than take the daemon down.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The agent's answer to one tool call.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    pub text: String,
    pub is_error: bool,
}

#[derive(Default)]
struct HubState {
    /// The accepted shim connection (write half).
    writer: Option<UnixStream>,
    /// `tool_use` ids the shim has asked about and nobody has answered yet.
    called: HashSet<String>,
    /// Results that arrived before the shim's call for them.
    early: HashMap<String, ToolResult>,
}

/// The router's side of the shim connection: matches calls with results in
/// whichever order they happen.
#[derive(Default)]
pub struct Hub {
    state: Mutex<HubState>,
}

impl Hub {
    pub fn new() -> Arc<Hub> {
        Arc::new(Hub::default())
    }

    fn attach(&self, w: UnixStream) {
        lock(&self.state).writer = Some(w);
    }

    fn detach(&self) {
        lock(&self.state).writer = None;
    }

    fn send(writer: &mut Option<UnixStream>, id: &str, r: &ToolResult) {
        if let Some(w) = writer {
            // A write error means the shim is gone; the session is ending.
            let _ = write_frame(
                w,
                &json!({"result": {"id": id, "text": r.text, "is_error": r.is_error}}),
            );
        }
    }

    /// The shim announced a call for `id`: answer it now if the result is
    /// already here, else remember the call.
    pub fn on_call(&self, id: &str) {
        let mut guard = lock(&self.state);
        let s = &mut *guard;
        if let Some(r) = s.early.remove(id) {
            Hub::send(&mut s.writer, id, &r);
        } else {
            s.called.insert(id.to_owned());
        }
    }

    /// The agent's result for `id`: forward it to the waiting call, or keep
    /// it until the call arrives.
    pub fn deliver(&self, id: &str, r: ToolResult) {
        let mut guard = lock(&self.state);
        let s = &mut *guard;
        if s.called.remove(id) {
            Hub::send(&mut s.writer, id, &r);
        } else {
            s.early.insert(id.to_owned(), r);
        }
    }

    #[cfg(test)]
    fn pending(&self) -> (usize, usize) {
        let s = lock(&self.state);
        (s.called.len(), s.early.len())
    }
}

/// Whether `conn` is a process of our own uid (`SO_PEERCRED`).
fn peer_is(conn: &UnixStream, uid: rustix::process::Uid) -> bool {
    matches!(rustix::net::sockopt::socket_peercred(conn), Ok(c) if c.uid == uid)
}

/// Accept exactly one connection from `uid` on `listener`, serve it, then
/// stop. Runs on its own thread. `stop` ends the wait for a shim that never
/// comes (the session was killed).
pub fn spawn_accept(
    listener: UnixListener,
    sock: PathBuf,
    uid: rustix::process::Uid,
    tools: Value,
    hub: Arc<Hub>,
    stop: Arc<AtomicBool>,
) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    std::thread::Builder::new()
        .name("cc-shim".into())
        .spawn(move || accept_one(listener, &sock, uid, &tools, &hub, &stop))?;
    Ok(())
}

fn accept_one(
    listener: UnixListener,
    sock: &Path,
    uid: rustix::process::Uid,
    tools: &Value,
    hub: &Hub,
    stop: &AtomicBool,
) {
    let conn = loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        match listener.accept() {
            // A connection from another uid is dropped unread, and the wait
            // continues for the real shim.
            Ok((c, _)) if peer_is(&c, uid) => break c,
            Ok(_) => continue,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => return,
        }
    };
    // One connection per session: nothing may connect after this.
    drop(listener);
    let _ = std::fs::remove_file(sock);
    let _ = conn.set_nonblocking(false);
    let Ok(mut writer) = conn.try_clone() else {
        return;
    };
    let mut reader = conn;
    match read_frame(&mut reader) {
        Ok(Some(v)) if v.get("hello").is_some() => {}
        _ => return,
    }
    if write_frame(&mut writer, &json!({"tools": tools})).is_err() {
        return;
    }
    hub.attach(writer);
    while let Ok(Some(v)) = read_frame(&mut reader) {
        if let Some(id) = v.pointer("/call/id").and_then(Value::as_str) {
            hub.on_call(id);
        }
    }
    hub.detach();
}

// ---------------------------------------------------------------------------
// The shim process.

#[derive(Default)]
struct Pending {
    closed: bool,
    map: HashMap<String, Sender<ToolResult>>,
}

type Out = Arc<Mutex<io::Stdout>>;

fn respond(out: &Out, v: &Value) {
    let mut o = lock(out);
    if let Ok(mut s) = serde_json::to_string(v) {
        s.push('\n');
        let _ = o.write_all(s.as_bytes());
        let _ = o.flush();
    }
}

fn reply(out: &Out, id: Value, result: Value) {
    respond(out, &json!({"jsonrpc": "2.0", "id": id, "result": result}));
}

fn tool_error(out: &Out, id: Value, text: &str) {
    reply(
        out,
        id,
        json!({"content": [{"type": "text", "text": text}], "isError": true}),
    );
}

/// `ec-inferenced --tool-shim <sock>`: the stdio MCP server `claude` runs.
/// Returns the process exit code.
pub fn run(sock: &str) -> i32 {
    let mut conn = match UnixStream::connect(sock) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ec-inferenced tool-shim: cannot connect: {e}");
            return 1;
        }
    };
    if write_frame(&mut conn, &json!({"hello": true})).is_err() {
        return 1;
    }
    let tools = match read_frame(&mut conn) {
        Ok(Some(v)) => match v.get("tools") {
            Some(t) if t.is_array() => t.clone(),
            _ => return 1,
        },
        _ => return 1,
    };
    let Ok(mut rd) = conn.try_clone() else {
        return 1;
    };
    let wr = Arc::new(Mutex::new(conn));
    let pending = Arc::new(Mutex::new(Pending::default()));

    // Router -> shim: results, matched to the blocked call by id. When the
    // router goes away every blocked call is failed so `claude` is not left
    // waiting on a tool that can no longer answer.
    {
        let pending = pending.clone();
        let spawned = std::thread::Builder::new().name("shim-rx".into()).spawn(move || {
            while let Ok(Some(v)) = read_frame(&mut rd) {
                let Some(r) = v.get("result") else { continue };
                let Some(id) = r.get("id").and_then(Value::as_str) else {
                    continue;
                };
                let res = ToolResult {
                    text: r.get("text").and_then(Value::as_str).unwrap_or("").to_owned(),
                    is_error: r.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                };
                if let Some(tx) = lock(&pending).map.remove(id) {
                    let _ = tx.send(res);
                }
            }
            let mut p = lock(&pending);
            p.closed = true;
            for (_, tx) in p.map.drain() {
                let _ = tx.send(ToolResult {
                    text: "the router closed the session".into(),
                    is_error: true,
                });
            }
        });
        if spawned.is_err() {
            return 1;
        }
    }

    let out: Out = Arc::new(Mutex::new(io::stdout()));
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = msg.get("id").cloned();
        match (msg.get("method").and_then(Value::as_str), id) {
            (Some("initialize"), Some(id)) => {
                let version = msg
                    .pointer("/params/protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or("2024-11-05");
                reply(
                    &out,
                    id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION")},
                    }),
                );
            }
            (Some("ping"), Some(id)) => reply(&out, id, json!({})),
            (Some("tools/list"), Some(id)) => reply(&out, id, json!({"tools": tools})),
            (Some("tools/call"), Some(id)) => {
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                let (wr, pending, out) = (wr.clone(), pending.clone(), out.clone());
                // One thread per call: `claude` may have several in flight,
                // and each blocks until the agent has run its tool.
                let spawned = std::thread::Builder::new()
                    .name("shim-call".into())
                    .spawn(move || call(id, &params, &wr, &pending, &out));
                if spawned.is_err() {
                    eprintln!("ec-inferenced tool-shim: cannot spawn a call thread");
                }
            }
            (Some(_), Some(id)) => respond(
                &out,
                &json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "method not found"}}),
            ),
            // Notifications (`notifications/initialized`, ...) and responses.
            _ => {}
        }
    }
    0
}

fn call(id: Value, params: &Value, wr: &Mutex<UnixStream>, pending: &Mutex<Pending>, out: &Out) {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    // The id of the `tool_use` block on claude's stdout; it is what ties this
    // call to the agent's `tool_result`.
    let Some(tuid) = params
        .get("_meta")
        .and_then(|m| m.get("claudecode/toolUseId"))
        .and_then(Value::as_str)
    else {
        tool_error(out, id, "the tool call carried no tool use id");
        return;
    };
    let (tx, rx) = mpsc::channel();
    {
        let mut p = lock(pending);
        if p.closed {
            drop(p);
            tool_error(out, id, "the router closed the session");
            return;
        }
        p.map.insert(tuid.to_owned(), tx);
    }
    let sent = {
        let mut w = lock(wr);
        write_frame(
            &mut *w,
            &json!({"call": {"id": tuid, "name": name, "input": params.get("arguments").cloned().unwrap_or(Value::Null)}}),
        )
    };
    if sent.is_err() {
        lock(pending).map.remove(tuid);
        tool_error(out, id, "the router closed the session");
        return;
    }
    let r = rx.recv().unwrap_or(ToolResult {
        text: "the router closed the session".into(),
        is_error: true,
    });
    reply(
        out,
        id,
        json!({"content": [{"type": "text", "text": r.text}], "isError": r.is_error}),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(t: &str) -> ToolResult {
        ToolResult {
            text: t.into(),
            is_error: false,
        }
    }

    fn hub_with_pair() -> (Arc<Hub>, UnixStream) {
        let (a, b) = UnixStream::pair().unwrap();
        let hub = Hub::new();
        hub.attach(a);
        (hub, b)
    }

    fn frame(r: &mut UnixStream) -> Value {
        read_frame(r).unwrap().unwrap()
    }

    #[test]
    fn a_call_then_its_result() {
        let (hub, mut shim) = hub_with_pair();
        hub.on_call("toolu_1");
        assert_eq!(hub.pending(), (1, 0));
        hub.deliver("toolu_1", res("done"));
        let v = frame(&mut shim);
        assert_eq!(v["result"]["id"], "toolu_1");
        assert_eq!(v["result"]["text"], "done");
        assert_eq!(v["result"]["is_error"], false);
        assert_eq!(hub.pending(), (0, 0));
    }

    #[test]
    fn a_result_before_its_call_is_held() {
        let (hub, mut shim) = hub_with_pair();
        hub.deliver(
            "toolu_9",
            ToolResult {
                text: "early".into(),
                is_error: true,
            },
        );
        assert_eq!(hub.pending(), (0, 1));
        hub.on_call("toolu_9");
        let v = frame(&mut shim);
        assert_eq!(v["result"]["id"], "toolu_9");
        assert_eq!(v["result"]["text"], "early");
        assert_eq!(v["result"]["is_error"], true);
        assert_eq!(hub.pending(), (0, 0));
    }

    #[test]
    fn only_one_connection_is_accepted_and_the_socket_is_removed() {
        let dir = std::env::temp_dir().join(format!("ecshim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("shim.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let hub = Hub::new();
        let stop = Arc::new(AtomicBool::new(false));
        spawn_accept(
            listener,
            sock.clone(),
            rustix::process::getuid(),
            json!([{"name": "t"}]),
            hub.clone(),
            stop.clone(),
        )
        .unwrap();
        let mut c = UnixStream::connect(&sock).unwrap();
        write_frame(&mut c, &json!({"hello": true})).unwrap();
        assert_eq!(frame(&mut c)["tools"][0]["name"], "t");
        // The socket file is gone once the one connection is taken.
        for _ in 0..200 {
            if !sock.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!sock.exists());
        assert!(UnixStream::connect(&sock).is_err());
        write_frame(&mut c, &json!({"call": {"id": "a", "name": "t", "input": {}}})).unwrap();
        for _ in 0..200 {
            if hub.pending().0 == 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(hub.pending(), (1, 0));
        stop.store(true, Ordering::Release);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
