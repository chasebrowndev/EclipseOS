// SPDX-License-Identifier: AGPL-3.0-only
//! `eclipse-ctl` — thin CLI over the helios control socket (COMP-13 §2.3).
//!
//! It holds no authority: it opens `$XDG_RUNTIME_DIR/eclipse/helios.sock` and
//! writes JSON-RPC. The compositor decides what is allowed; anything this
//! tool can do, a shell one-liner can do too. Keep it that way.

use std::{
    io::{BufRead, BufReader, IsTerminal, Write},
    os::unix::net::UnixStream,
    process::ExitCode,
};

use serde_json::{json, Value};

const USAGE: &str = "\
eclipse-ctl — control the helios compositor

  eclipse-ctl outputs [--all]         list outputs (--all includes virtual)
  eclipse-ctl workspaces              list workspaces
  eclipse-ctl windows                 list windows
  eclipse-ctl focused                 the focused window, per seat
  eclipse-ctl metrics                 frame timing and counters
  eclipse-ctl dump                    everything, as JSON
  eclipse-ctl watch [EVENT...]        stream events until interrupted
  eclipse-ctl focus HANDLE            focus a window
  eclipse-ctl close HANDLE            ask a window to close
  eclipse-ctl float HANDLE on|off     set a window floating or tiled
  eclipse-ctl workspace N             switch to workspace N
  eclipse-ctl move HANDLE N           move a window to workspace N
  eclipse-ctl reload                  re-read the config
  eclipse-ctl call METHOD [JSON]      raw JSON-RPC, for anything not above

Options:
  --json    print the raw result even for the table commands
  --socket PATH
";

fn main() -> ExitCode {
    // Restore the default SIGPIPE so `eclipse-ctl windows | head` exits quietly
    // instead of panicking on a closed stdout.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut socket = None;
    let mut raw = false;
    let mut all = false;
    args.retain(|a| match a.as_str() {
        "--json" => {
            raw = true;
            false
        }
        "--all" => {
            all = true;
            false
        }
        _ => true,
    });
    if let Some(i) = args.iter().position(|a| a == "--socket") {
        if i + 1 >= args.len() {
            eprintln!("--socket needs a path");
            return ExitCode::FAILURE;
        }
        socket = Some(args.remove(i + 1));
        args.remove(i);
    }
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" || args[0] == "help" {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    let (method, params, table) = match parse(&args, all) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("eclipse-ctl: {e}");
            return ExitCode::FAILURE;
        }
    };

    let path = socket.unwrap_or_else(default_socket);
    let stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("eclipse-ctl: {path}: {e}");
            eprintln!("is helios running, and is this the same session?");
            return ExitCode::FAILURE;
        }
    };

    if method == "subscribe" {
        return watch(stream, params);
    }
    match call(stream, &method, params) {
        Ok(result) => {
            match table.filter(|_| !raw) {
                Some(t) => print_table(t, &result),
                None => println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default()),
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("eclipse-ctl: {e}");
            ExitCode::FAILURE
        }
    }
}

fn default_socket() -> String {
    let base = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".into());
    format!("{base}/eclipse/helios.sock")
}

/// Which columns a listing prints. Anything not here falls back to JSON.
#[derive(Clone, Copy)]
enum Table {
    Outputs,
    Workspaces,
    Windows,
}

type Parsed = (String, Value, Option<Table>);

fn parse(args: &[String], all: bool) -> Result<Parsed, String> {
    let a = |i: usize| args.get(i).map(String::as_str).unwrap_or_default();
    let num = |i: usize| -> Result<u64, String> {
        a(i).parse::<u64>()
            .map_err(|_| format!("expected a number, got {:?}", a(i)))
    };
    Ok(match a(0) {
        "outputs" => ("get_outputs".into(), json!({"all": all}), Some(Table::Outputs)),
        "workspaces" => ("get_workspaces".into(), Value::Null, Some(Table::Workspaces)),
        "windows" => ("get_windows".into(), Value::Null, Some(Table::Windows)),
        "focused" => ("get_focused".into(), Value::Null, None),
        "metrics" => ("get_metrics".into(), Value::Null, None),
        "dump" => ("dump_state".into(), Value::Null, None),
        "agents" => ("get_agents".into(), Value::Null, None),
        "reload" => ("reload_config".into(), Value::Null, None),
        "watch" => {
            let events: Vec<Value> = args[1..].iter().map(|s| Value::String(s.clone())).collect();
            let params = if events.is_empty() {
                Value::Null
            } else {
                json!({"events": events})
            };
            ("subscribe".into(), params, None)
        }
        "focus" => ("focus_window".into(), json!({"handle": num(1)?}), None),
        "close" => ("close_window".into(), json!({"handle": num(1)?}), None),
        "float" => {
            let on = match a(2) {
                "on" | "true" | "yes" => true,
                "off" | "false" | "no" => false,
                other => return Err(format!("expected on|off, got {other:?}")),
            };
            (
                "set_floating".into(),
                json!({"handle": num(1)?, "floating": on}),
                None,
            )
        }
        "workspace" => ("switch_workspace".into(), json!({"workspace": num(1)?}), None),
        "move" => (
            "move_to_workspace".into(),
            json!({"handle": num(1)?, "workspace": num(2)?}),
            None,
        ),
        "pause" | "resume" | "terminate" => {
            // COMP-13 §2.3 spells these as `eclipse-ctl pause agent:research-7`.
            // The compositor has no agents yet and answers "not implemented";
            // the CLI surface exists so the shape does not change later.
            let id = a(1);
            if id.is_empty() {
                return Err("expected an agent id".into());
            }
            (format!("{}_agent", a(0)), json!({"agent": id}), None)
        }
        "call" => {
            let method = a(1);
            if method.is_empty() {
                return Err("expected a method name".into());
            }
            let params = match args.get(2) {
                None => Value::Null,
                Some(s) => serde_json::from_str(s).map_err(|e| format!("params: {e}"))?,
            };
            (method.to_string(), params, None)
        }
        other => return Err(format!("unknown command {other:?}; try --help")),
    })
}

fn request(method: &str, params: Value) -> String {
    let mut req = json!({"jsonrpc": "2.0", "id": 1, "method": method});
    if !params.is_null() {
        req["params"] = params;
    }
    req.to_string()
}

fn call(mut stream: UnixStream, method: &str, params: Value) -> Result<Value, String> {
    let line = request(method, params);
    stream
        .write_all(format!("{line}\n").as_bytes())
        .map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(stream);
    let mut buf = String::new();
    if reader.read_line(&mut buf).map_err(|e| e.to_string())? == 0 {
        return Err("the compositor closed the connection without replying".into());
    }
    let v: Value = serde_json::from_str(&buf).map_err(|e| e.to_string())?;
    if let Some(err) = v.get("error") {
        let msg = err.get("message").and_then(Value::as_str).unwrap_or("error");
        let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
        return Err(format!("{msg} (code {code})"));
    }
    Ok(v.get("result").cloned().unwrap_or(Value::Null))
}

/// Subscribe and print events one JSON object per line, forever. Line-buffered
/// so `eclipse-ctl watch | while read` works.
fn watch(mut stream: UnixStream, params: Value) -> ExitCode {
    let line = request("subscribe", params);
    if let Err(e) = stream.write_all(format!("{line}\n").as_bytes()) {
        eprintln!("eclipse-ctl: {e}");
        return ExitCode::FAILURE;
    }
    let reader = BufReader::new(stream);
    let mut stdout = std::io::stdout();
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(err) = v.get("error") {
            eprintln!("eclipse-ctl: {err}");
            return ExitCode::FAILURE;
        }
        // The reply to `subscribe` itself is only interesting on a terminal.
        if v.get("result").is_some() {
            if std::io::stdout().is_terminal() {
                eprintln!("subscribed: {}", v["result"]);
            }
            continue;
        }
        let _ = writeln!(stdout, "{}", v.get("params").unwrap_or(&v));
        let _ = stdout.flush();
    }
    ExitCode::SUCCESS
}

fn s(v: &Value, key: &str) -> String {
    match v.get(key) {
        None | Some(Value::Null) => "-".into(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

fn print_table(table: Table, result: &Value) {
    let Some(rows) = result.as_array() else {
        println!("{result}");
        return;
    };
    match table {
        Table::Outputs => {
            println!("{:<4} {:<12} {:<11} {:<6} STATE", "ID", "NAME", "MODE", "SCALE");
            for r in rows {
                let mode = match r.get("mode") {
                    Some(m) if m.is_object() => {
                        format!("{}x{}", s(m, "width"), s(m, "height"))
                    }
                    _ => "-".into(),
                };
                let mut state = Vec::new();
                if r["enabled"] == Value::Bool(false) {
                    state.push("disabled");
                }
                if r["powered"] == Value::Bool(false) {
                    state.push("dpms-off");
                }
                if r["focused"] == Value::Bool(true) {
                    state.push("focused");
                }
                if r["virtual"] == Value::Bool(true) {
                    state.push("virtual");
                }
                println!(
                    "{:<4} {:<12} {:<11} {:<6} {}",
                    s(r, "id"),
                    s(r, "name"),
                    mode,
                    s(r, "scale"),
                    state.join(",")
                );
            }
        }
        Table::Workspaces => {
            println!("{:<4} {:<8} {:<8} ACTIVE", "WS", "OUTPUT", "WINDOWS");
            for r in rows {
                println!(
                    "{:<4} {:<8} {:<8} {}",
                    s(r, "index"),
                    s(r, "output_name"),
                    s(r, "windows"),
                    if r["active"] == Value::Bool(true) { "*" } else { "" }
                );
            }
        }
        Table::Windows => {
            println!(
                "{:<8} {:<4} {:<24} {:<8} TITLE",
                "HANDLE", "WS", "APP-ID", "TRUST"
            );
            for r in rows {
                println!(
                    "{:<8} {:<4} {:<24} {:<8} {}",
                    s(r, "handle"),
                    s(r, "workspace"),
                    s(r, "app_id"),
                    s(r, "trust"),
                    s(r, "title")
                );
            }
        }
    }
}
