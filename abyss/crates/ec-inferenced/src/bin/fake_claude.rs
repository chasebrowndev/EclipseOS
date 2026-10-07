// SPDX-License-Identifier: AGPL-3.0-only
//! A scripted stand-in for `claude -p --input-format stream-json
//! --output-format stream-json` (feature `fake-claude`, tests only).
//!
//! It parses the same arguments the router passes, starts the MCP server named
//! in `--mcp-config` (the real tool shim) and speaks MCP to it over stdio, and
//! emits the stream shapes the spike observed: `system/init` once per turn,
//! `assistant` events, `user` (`tool_result`) events, `rate_limit_event`
//! between messages, one `result` per turn.
//!
//! The script is the user turn's text (`pid` replies `pid=<pid>`; `env` replies
//! `token_len=<n>`, the length of `CLAUDE_CODE_OAUTH_TOKEN`, never the value;
//! `exit` dies without a result; `tool:a,b[:split][:slow]` calls tools `a` and
//! `b`, in one assistant event or one per call with `split`, and `slow` delays
//! the MCP call by 300 ms so the agent's result can arrive first, then replies
//! `done:<results joined by ,>`; anything else replies `got:<text>`).
//!
//! The system prompt can switch behaviour: `FAKE:builtin` lists a built-in tool
//! in `system/init`; `FAKE:429` fails every turn with HTTP 429.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

fn emit(v: &Value) {
    let mut o = std::io::stdout().lock();
    let _ = writeln!(o, "{v}");
    let _ = o.flush();
}

struct Mcp {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl Mcp {
    fn send(&mut self, v: &Value) {
        writeln!(self.stdin, "{v}").expect("shim stdin");
        self.stdin.flush().expect("shim flush");
    }

    fn read(&mut self) -> Value {
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("shim stdout");
        serde_json::from_str(&line).expect("shim json")
    }

    fn id(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.id();
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let v = self.read();
            if v["id"] == json!(id) {
                return v["result"].clone();
            }
        }
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let system = flag(&args, "--system-prompt").unwrap_or_default();
    let model = "fake-model";
    let cfg: Value = serde_json::from_str(
        &std::fs::read_to_string(flag(&args, "--mcp-config").expect("--mcp-config")).expect("mcp.json"),
    )
    .expect("mcp.json is JSON");
    let srv = &cfg["mcpServers"]["eclipse"];
    let mut child = Command::new(srv["command"].as_str().expect("command"))
        .args(
            srv["args"]
                .as_array()
                .expect("args")
                .iter()
                .map(|a| a.as_str().expect("arg").to_owned()),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the shim");
    let mut mcp = Mcp {
        stdin: child.stdin.take().expect("stdin"),
        stdout: BufReader::new(child.stdout.take().expect("stdout")),
        next: 0,
    };
    mcp.request(
        "initialize",
        json!({"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "fake-claude", "version": "0"}}),
    );
    mcp.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let listed = mcp.request("tools/list", json!({}));
    let mut tools: Vec<String> = listed["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|t| format!("mcp__eclipse__{}", t["name"].as_str().expect("name")))
        .collect();
    if system.contains("FAKE:builtin") {
        tools.insert(0, "Bash".into());
    }

    let mut tool_seq = 0u64;
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let text = v["message"]["content"].as_str().unwrap_or("").to_owned();
        emit(
            &json!({"type": "system", "subtype": "init", "model": model, "tools": tools, "session_id": "s"}),
        );
        emit(&json!({"type": "rate_limit_event", "rate_limit_info": {"status": "allowed"}}));
        if system.contains("FAKE:429") {
            emit(&json!({"type": "result", "subtype": "success", "is_error": true,
                "api_error_status": 429, "result": "rate limited"}));
            continue;
        }
        let reply = if text == "pid" {
            format!("pid={}", std::process::id())
        } else if text == "env" {
            let n = std::env::var("CLAUDE_CODE_OAUTH_TOKEN").map_or(0, |t| t.len());
            format!("token_len={n}")
        } else if text == "exit" {
            std::process::exit(3);
        } else if let Some(spec) = text.strip_prefix("tool:") {
            let mut parts = spec.split(':');
            let names: Vec<String> = parts.next().unwrap_or("").split(',').map(str::to_owned).collect();
            let opts: Vec<&str> = parts.collect();
            let (split, slow) = (opts.contains(&"split"), opts.contains(&"slow"));
            let mut calls = Vec::new();
            for n in &names {
                tool_seq += 1;
                calls.push((format!("toolu_{tool_seq}"), n.clone()));
            }
            let blocks: Vec<Value> = calls
                .iter()
                .map(|(id, n)| {
                    json!({"type": "tool_use", "id": id, "name": format!("mcp__eclipse__{n}"), "input": {"n": n}})
                })
                .collect();
            let msg = |content: Value| json!({"type": "assistant", "message": {"role": "assistant", "content": content}});
            emit(&msg(
                json!([{"type": "thinking", "thinking": "plan"}, {"type": "text", "text": "calling"}]),
            ));
            if split {
                for b in &blocks {
                    emit(&msg(json!([b])));
                }
            } else {
                emit(&msg(Value::Array(blocks)));
            }
            if slow {
                std::thread::sleep(Duration::from_millis(300));
            }
            let mut ids = Vec::new();
            for (id, n) in &calls {
                let rid = mcp.id();
                ids.push((rid, id.clone()));
                mcp.send(&json!({"jsonrpc": "2.0", "id": rid, "method": "tools/call",
                    "params": {"name": n, "arguments": {"n": n}, "_meta": {"claudecode/toolUseId": id}}}));
            }
            let mut results: Vec<(String, String)> = Vec::new();
            while results.len() < ids.len() {
                let r = mcp.read();
                if let Some((_, tuid)) = ids.iter().find(|(rid, _)| r["id"] == json!(rid)) {
                    let t = r["result"]["content"][0]["text"]
                        .as_str()
                        .unwrap_or("")
                        .to_owned();
                    results.push((tuid.clone(), t));
                }
            }
            let blocks: Vec<Value> = results
                .iter()
                .map(|(id, t)| json!({"type": "tool_result", "tool_use_id": id, "content": t}))
                .collect();
            emit(&json!({"type": "user", "message": {"role": "user", "content": blocks}}));
            // Join in call order, whatever order the answers came back in.
            let by_call: Vec<String> = calls
                .iter()
                .map(|(id, _)| {
                    results
                        .iter()
                        .find(|(r, _)| r == id)
                        .map(|(_, t)| t.clone())
                        .unwrap_or_default()
                })
                .collect();
            format!("done:{}", by_call.join(","))
        } else {
            format!("got:{text}")
        };
        emit(
            &json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": reply}]}}),
        );
        emit(&json!({"type": "rate_limit_event", "rate_limit_info": {"status": "allowed"}}));
        emit(
            &json!({"type": "result", "subtype": "success", "is_error": false, "result": reply,
            "stop_reason": "end_turn", "total_cost_usd": 0.0123,
            "usage": {"input_tokens": 10, "output_tokens": 4},
            "modelUsage": {}}),
        );
    }
    drop(mcp);
    let _ = child.wait();
}
