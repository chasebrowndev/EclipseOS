// SPDX-License-Identifier: Apache-2.0
//! The reference agent: a deterministic, mechanical MCP client (console-plan
//! C6, A-08 §6). No model. It exists so the console wave can be exercised end
//! to end, and as the smallest example of an agent that only converses.
//!
//! Wire: line-delimited JSON-RPC 2.0, MCP `initialize` then `tools/call` for
//! `task.say {text}`, `task.ask {question}` and `task.inbox {since?}`.

use std::{
    io::{BufRead, BufReader, Read, Write},
    time::Duration,
};

use serde_json::{json, Value};

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// The server closed the connection.
    Closed,
    /// The server answered with a JSON-RPC error.
    Rpc(String),
    /// Something arrived that is not the protocol.
    Protocol(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Closed => write!(f, "the task socket closed"),
            Error::Rpc(m) => write!(f, "server error: {m}"),
            Error::Protocol(m) => write!(f, "protocol error: {m}"),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// One MCP session over a duplex byte stream.
pub struct Client<R: Read, W: Write> {
    r: BufReader<R>,
    w: W,
    next_id: u64,
}

impl<R: Read, W: Write> Client<R, W> {
    pub fn new(r: R, w: W) -> Self {
        Self {
            r: BufReader::new(r),
            w,
            next_id: 0,
        }
    }

    fn send(&mut self, v: &Value) -> Result<(), Error> {
        let mut line = v.to_string();
        line.push('\n');
        self.w.write_all(line.as_bytes())?;
        self.w.flush()?;
        Ok(())
    }

    /// Send a request and wait for its response, skipping any notification or
    /// unrelated line the server interleaves.
    fn request(&mut self, method: &str, params: Value) -> Result<Value, Error> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        loop {
            let mut line = String::new();
            if self.r.read_line(&mut line)? == 0 {
                return Err(Error::Closed);
            }
            if line.trim().is_empty() {
                continue;
            }
            let msg: Value =
                serde_json::from_str(&line).map_err(|e| Error::Protocol(format!("not JSON: {e}")))?;
            if msg.get("id") != Some(&json!(id)) {
                continue;
            }
            if let Some(err) = msg.get("error") {
                let m = err.get("message").and_then(Value::as_str).unwrap_or("error");
                return Err(Error::Rpc(m.to_owned()));
            }
            return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// MCP handshake. Returns `instructions`, the task statement.
    pub fn initialize(&mut self) -> Result<String, Error> {
        let result = self.request(
            "initialize",
            json!({
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "ec-ref-agent", "version": env!("CARGO_PKG_VERSION")},
            }),
        )?;
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
        Ok(result
            .get("instructions")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned())
    }

    fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value, Error> {
        let result = self.request("tools/call", json!({"name": name, "arguments": arguments}))?;
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            let text = result
                .pointer("/content/0/text")
                .and_then(Value::as_str)
                .unwrap_or("tool error");
            return Err(Error::Rpc(text.to_owned()));
        }
        Ok(result)
    }

    pub fn say(&mut self, text: &str) -> Result<(), Error> {
        self.call_tool("task.say", json!({"text": text})).map(drop)
    }

    pub fn ask(&mut self, question: &str) -> Result<(), Error> {
        self.call_tool("task.ask", json!({"question": question}))
            .map(drop)
    }

    /// `task.inbox`. The payload `{messages, closed}` is accepted as the bare
    /// result, as `structuredContent`, or as JSON text in the first content
    /// block, since MCP servers differ in where they put it.
    pub fn inbox(&mut self, since: Option<&Value>) -> Result<Inbox, Error> {
        let args = match since {
            Some(s) => json!({"since": s}),
            None => json!({}),
        };
        let result = self.call_tool("task.inbox", args)?;
        let payload = if result.get("messages").is_some() {
            result
        } else if let Some(s) = result
            .get("structuredContent")
            .filter(|s| s.get("messages").is_some())
        {
            s.clone()
        } else {
            let text = result
                .pointer("/content/0/text")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::Protocol("task.inbox result has no payload".into()))?;
            serde_json::from_str(text).map_err(|e| Error::Protocol(format!("task.inbox text: {e}")))?
        };
        Ok(Inbox {
            messages: payload
                .get("messages")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            closed: payload.get("closed").and_then(Value::as_bool).unwrap_or(false),
            cursor: payload.get("cursor").filter(|c| !c.is_null()).cloned(),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Inbox {
    pub messages: Vec<Value>,
    pub closed: bool,
    /// The next `since`, when the server hands one out.
    pub cursor: Option<Value>,
}

/// The acknowledgement said once at start, quoting the statement.
pub fn acknowledgement(statement: &str) -> String {
    format!("Acknowledged. My task: \"{statement}\"")
}

/// The reply to one human message.
pub fn reply_to(text: &str) -> String {
    format!("You said: \"{text}\". Noted.")
}

/// The follow-up question a human question earns (exercises `awaiting_reply`).
pub fn followup_question(text: &str) -> String {
    format!("You asked \"{text}\" - can you tell me more?")
}

/// Whether a human message is a question: it ends in `?`.
pub fn is_question(text: &str) -> bool {
    text.trim_end().ends_with('?')
}

/// Run the reference behaviour over `sock` until the inbox reports closed.
pub fn run(sock: std::os::unix::net::UnixStream, poll: Duration) -> Result<(), Error> {
    let w = sock.try_clone()?;
    serve(Client::new(sock, w), poll)
}

/// The behaviour itself, over any transport.
pub fn serve<R: Read, W: Write>(mut c: Client<R, W>, poll: Duration) -> Result<(), Error> {
    let statement = c.initialize()?;
    c.say(&acknowledgement(&statement))?;

    let mut since: Option<Value> = None;
    loop {
        let inbox = c.inbox(since.as_ref())?;
        // The server's cursor is authoritative; the last msg_id is the fallback.
        if let Some(c) = &inbox.cursor {
            since = Some(c.clone());
        }
        for m in &inbox.messages {
            if inbox.cursor.is_none() {
                if let Some(id) = m.get("msg_id") {
                    since = Some(id.clone());
                }
            }
            // Only the human's words get a reply: not our own echoes, not the
            // `context` message of a resumed task.
            if m.get("kind").and_then(Value::as_str) != Some("human") {
                continue;
            }
            let text = m.get("text").and_then(Value::as_str).unwrap_or_default();
            c.say(&reply_to(text))?;
            if is_question(text) {
                c.ask(&followup_question(text))?;
            }
        }
        if inbox.closed {
            return Ok(());
        }
        std::thread::sleep(poll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::net::UnixStream, thread};

    /// Scripted fake server: answers `initialize`, records every tool call, and
    /// serves the queued inbox batches in order (the last one with closed).
    fn fake(server: UnixStream, batches: Vec<Value>) -> thread::JoinHandle<Vec<(String, Value)>> {
        thread::spawn(move || {
            let mut r = BufReader::new(server.try_clone().expect("clone"));
            let mut w = server;
            let mut calls = Vec::new();
            let mut batches = batches.into_iter();
            let mut line = String::new();
            loop {
                line.clear();
                if r.read_line(&mut line).expect("read") == 0 {
                    return calls;
                }
                let req: Value = serde_json::from_str(&line).expect("json");
                let Some(id) = req.get("id").cloned() else {
                    continue; // notification
                };
                let result = match req["method"].as_str().expect("method") {
                    "initialize" => json!({"protocolVersion": "2025-03-26", "capabilities": {},
                        "serverInfo": {"name": "fake", "version": "0"},
                        "instructions": "sort the pile"}),
                    "tools/call" => {
                        let name = req["params"]["name"].as_str().expect("name").to_owned();
                        let args = req["params"]["arguments"].clone();
                        calls.push((name.clone(), args));
                        if name == "task.inbox" {
                            let b = batches.next().expect("more polls than scripted");
                            json!({"content": [{"type": "text", "text": b.to_string()}]})
                        } else {
                            json!({"content": [{"type": "text", "text": "ok"}]})
                        }
                    }
                    other => panic!("unexpected {other}"),
                };
                // A stray notification first: the client must skip it.
                writeln!(w, "{}", json!({"jsonrpc": "2.0", "method": "notifications/x"})).expect("w");
                writeln!(w, "{}", json!({"jsonrpc": "2.0", "id": id, "result": result})).expect("w");
            }
        })
    }

    fn drive(batches: Vec<Value>) -> Vec<(String, Value)> {
        let (client, server) = UnixStream::pair().expect("pair");
        let h = fake(server, batches);
        run(client, Duration::from_millis(1)).expect("run");
        h.join().expect("join")
    }

    #[test]
    fn acknowledges_replies_asks_and_exits_on_close() {
        let calls = drive(vec![
            json!({"messages": [], "closed": false}),
            json!({"messages": [
                {"msg_id": "a", "kind": "say", "text": "mine"},
                {"msg_id": "b", "kind": "human", "text": "hello"},
                {"msg_id": "c", "kind": "human", "text": "ready?  "},
            ], "closed": false}),
            json!({"messages": [], "closed": true}),
        ]);
        let names: Vec<&str> = calls.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "task.say",
                "task.inbox",
                "task.inbox",
                "task.say",
                "task.say",
                "task.ask",
                "task.inbox"
            ]
        );
        assert_eq!(
            calls[0].1["text"],
            json!("Acknowledged. My task: \"sort the pile\"")
        );
        assert_eq!(calls[1].1, json!({}));
        assert_eq!(calls[2].1, json!({}));
        assert_eq!(calls[3].1["text"], json!("You said: \"hello\". Noted."));
        assert_eq!(calls[4].1["text"], json!("You said: \"ready?  \". Noted."));
        assert!(calls[5].1["question"].as_str().expect("q").contains("ready?"));
        // `since` carries the last msg_id seen, and own messages are not echoed.
        assert_eq!(calls[6].1, json!({"since": "c"}));
    }

    #[test]
    fn the_servers_cursor_becomes_the_next_since() {
        let calls = drive(vec![
            json!({"messages": [{"msg_id": 5, "kind": "human", "text": "hi"}], "closed": false, "cursor": 7}),
            json!({"messages": [], "closed": true, "cursor": 7}),
        ]);
        let last = calls.last().expect("calls");
        assert_eq!(last.0, "task.inbox");
        assert_eq!(last.1, json!({"since": 7}));
    }

    #[test]
    fn closed_with_messages_still_replies_first() {
        let calls = drive(vec![
            json!({"messages": [{"msg_id": 1, "kind": "human", "text": "bye"}],
            "closed": true}),
        ]);
        let names: Vec<&str> = calls.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["task.say", "task.inbox", "task.say"]);
    }

    #[test]
    fn accepts_structured_content_and_bare_results() {
        let (client, server) = UnixStream::pair().expect("pair");
        let h = thread::spawn(move || {
            let mut r = BufReader::new(server.try_clone().expect("clone"));
            let mut w = server;
            let mut line = String::new();
            let payloads = [
                json!({"structuredContent": {"messages": [], "closed": false}}),
                json!({"messages": [], "closed": true}),
            ];
            for p in payloads {
                line.clear();
                r.read_line(&mut line).expect("read");
                let id = serde_json::from_str::<Value>(&line).expect("json")["id"].clone();
                writeln!(w, "{}", json!({"jsonrpc": "2.0", "id": id, "result": p})).expect("w");
            }
        });
        let w = client.try_clone().expect("clone");
        let mut c = Client::new(client, w);
        assert!(!c.inbox(None).expect("a").closed);
        assert!(c.inbox(None).expect("b").closed);
        h.join().expect("join");
    }

    #[test]
    fn a_closed_socket_is_an_error_not_a_hang() {
        let (client, server) = UnixStream::pair().expect("pair");
        drop(server);
        assert!(run(client, Duration::from_millis(1)).is_err());
    }

    #[test]
    fn question_detection() {
        assert!(is_question("why?"));
        assert!(is_question("why? \n"));
        assert!(!is_question("why? no"));
        assert!(!is_question(""));
    }
}
