// SPDX-License-Identifier: AGPL-3.0-only
//! `eclipse-ctl` — thin CLI over the abyss control socket (COMP-13 §2.3).
//!
//! It holds no authority: it opens `$XDG_RUNTIME_DIR/eclipse/abyss.sock` and
//! writes JSON-RPC. The compositor decides what is allowed; anything this
//! tool can do, a shell one-liner can do too. Keep it that way.

use std::{
    io::{BufRead, BufReader, IsTerminal, Write},
    os::unix::net::UnixStream,
    process::ExitCode,
};

use serde_json::{json, Value};

mod migrate;

const USAGE: &str = "\
eclipse-ctl — control the abyss compositor

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
  eclipse-ctl output ID overscan SPEC set overscan: N, or top=N,left=N,...
  eclipse-ctl output ID calibrate [commit|cancel]
                                      drive the on-screen overscan calibration
  eclipse-ctl reload                  re-read the config
  eclipse-ctl config list [--changed] every setting, its value and its file
  eclipse-ctl config describe PATH    one setting: type, range, default, doc
  eclipse-ctl config get PATH         one setting's value, bare
  eclipse-ctl config set PATH VALUE   write a setting (abyss.kdl only)
  eclipse-ctl config validate         check a config file without applying it
  eclipse-ctl config migrate          split a legacy abyss.kdl into two files
  eclipse-ctl call METHOD [JSON]      raw JSON-RPC, for anything not above

Options:
  --json          print the raw result even for the table commands
  --file abyss|policy   which config file a config verb is about
  --changed       config list: only settings that differ from the default
  --dry-run       config set/migrate: say what would happen, write nothing
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
    let mut flags = Flags::default();
    args.retain(|a| match a.as_str() {
        "--json" => {
            raw = true;
            false
        }
        "--all" => {
            flags.all = true;
            false
        }
        "--changed" => {
            flags.changed = true;
            false
        }
        "--dry-run" => {
            flags.dry_run = true;
            false
        }
        _ => true,
    });
    if let Some(i) = args.iter().position(|a| a == "--file") {
        if i + 1 >= args.len() {
            eprintln!("--file needs abyss or policy");
            return ExitCode::FAILURE;
        }
        flags.file = Some(args.remove(i + 1));
        args.remove(i);
    }
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

    // `config migrate` never touches the socket: it edits the files in place,
    // and one of them is `policy.kdl`, which the socket may never write
    // (COMP-13 §1.3). It must also work with the compositor stopped.
    if args[0] == "config" && args.get(1).map(String::as_str) == Some("migrate") {
        return match migrate::run(flags.dry_run) {
            Ok(report) => {
                print!("{report}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("eclipse-ctl: {e}");
                ExitCode::FAILURE
            }
        };
    }

    let (method, params, table) = match parse(&args, &flags) {
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
            eprintln!("is abyss running, and is this the same session?");
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
    format!("{base}/eclipse/abyss.sock")
}

/// Command-line flags that survive into `parse`. Grouped rather than passed
/// one by one so adding a verb's flag does not re-thread every call site.
#[derive(Default)]
struct Flags {
    all: bool,
    changed: bool,
    dry_run: bool,
    file: Option<String>,
}

/// Which columns a listing prints. Anything not here falls back to JSON.
#[derive(Clone, Copy)]
enum Table {
    Outputs,
    Workspaces,
    Windows,
    /// `config list`. `changed` drops rows still at their default, which is
    /// what "show me what I have actually configured" means.
    Config {
        changed: bool,
    },
    ConfigKey,
    ConfigValue,
    ConfigCheck,
}

type Parsed = (String, Value, Option<Table>);

fn parse(args: &[String], flags: &Flags) -> Result<Parsed, String> {
    let a = |i: usize| args.get(i).map(String::as_str).unwrap_or_default();
    let num = |i: usize| -> Result<u64, String> {
        a(i).parse::<u64>()
            .map_err(|_| format!("expected a number, got {:?}", a(i)))
    };
    Ok(match a(0) {
        "outputs" => (
            "get_outputs".into(),
            json!({"all": flags.all}),
            Some(Table::Outputs),
        ),
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
        "output" => {
            let id = num(1)?;
            match a(2) {
                "overscan" => (
                    "set_output".into(),
                    json!({"output": id, "overscan": overscan_spec(a(3))?}),
                    None,
                ),
                "calibrate" => {
                    let action = match a(3) {
                        "" | "start" => "start",
                        "commit" => "commit",
                        "cancel" => "cancel",
                        other => return Err(format!("expected start|commit|cancel, got {other:?}")),
                    };
                    (
                        "calibrate_output".into(),
                        json!({"output": id, "action": action}),
                        None,
                    )
                }
                other => return Err(format!("unknown output subcommand {other:?}")),
            }
        }
        "config" => {
            let path = a(2);
            let need_path = |verb: &str| -> Result<String, String> {
                if path.is_empty() {
                    Err(format!("config {verb} needs a setting path"))
                } else {
                    Ok(path.to_string())
                }
            };
            let file = || match flags.file.as_deref() {
                None => Value::Null,
                Some(f) => json!(f),
            };
            match a(1) {
                "" | "list" => {
                    let mut params = json!({"schema": false});
                    if let Value::String(f) = file() {
                        params["file"] = json!(f);
                    }
                    (
                        "get_config".into(),
                        params,
                        Some(Table::Config {
                            changed: flags.changed,
                        }),
                    )
                }
                "describe" => (
                    "get_config".into(),
                    json!({"schema": true, "path": need_path("describe")?}),
                    Some(Table::ConfigKey),
                ),
                "get" => (
                    "get_config".into(),
                    json!({"schema": false, "path": need_path("get")?}),
                    Some(Table::ConfigValue),
                ),
                "set" => {
                    let path = need_path("set")?;
                    let raw = args.get(3).ok_or("config set needs a value")?;
                    (
                        "set_config_value".into(),
                        json!({
                            "path": path,
                            // Sent as a JSON string unless it is plainly a
                            // number or a bool; the compositor coerces per the
                            // key's type, which is the only place that knows it.
                            "value": scalar(raw),
                            "dry_run": flags.dry_run,
                        }),
                        None,
                    )
                }
                "validate" => {
                    // `config validate [FILE]` — the positional is the same
                    // abyss|policy name `--file` takes.
                    let named = if path.is_empty() { file() } else { json!(path) };
                    let mut params = json!({});
                    if let Value::String(f) = named {
                        params["file"] = json!(f);
                    }
                    ("validate_config".into(), params, Some(Table::ConfigCheck))
                }
                other => return Err(format!("unknown config subcommand {other:?}")),
            }
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

/// `48` (uniform) or `top=20,bottom=20,left=40` (per edge, any subset).
/// Kept here rather than in the compositor so the wire stays plain JSON.
fn overscan_spec(raw: &str) -> Result<Value, String> {
    if raw.is_empty() {
        return Err("expected a pixel count, or top=N,bottom=N,left=N,right=N".into());
    }
    if let Ok(n) = raw.parse::<i64>() {
        return Ok(json!(n));
    }
    let mut obj = serde_json::Map::new();
    for part in raw.split(',') {
        let (edge, px) = part
            .split_once('=')
            .ok_or_else(|| format!("expected edge=px, got {part:?}"))?;
        if !matches!(edge, "top" | "bottom" | "left" | "right") {
            return Err(format!("unknown edge {edge:?}"));
        }
        let px: i64 = px
            .parse()
            .map_err(|_| format!("{edge}: expected a number, got {px:?}"))?;
        obj.insert(edge.into(), json!(px));
    }
    Ok(Value::Object(obj))
}

/// A `config set` value, as JSON. Bare `true`/`false` and numbers go over the
/// wire as themselves; everything else is a string. The compositor coerces
/// against the key's declared type either way — this only decides what a shell
/// word looks like before it gets there, so `set … 0.5` is a float and
/// `set … "0.5"` is not something the shell can express differently.
fn scalar(raw: &str) -> Value {
    match raw {
        "true" | "false" => json!(raw == "true"),
        _ => match raw.parse::<i64>() {
            Ok(n) => json!(n),
            Err(_) => match raw.parse::<f64>() {
                Ok(f) => json!(f),
                Err(_) => json!(raw),
            },
        },
    }
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

/// The one-line form of a value: strings bare, lists space-joined, null as `-`.
fn scalar_str(v: &Value) -> String {
    match v {
        Value::Null => "-".into(),
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().map(scalar_str).collect::<Vec<_>>().join(" "),
        other => other.to_string(),
    }
}

fn print_config_list(rows: &[Value], changed: bool) {
    println!("{:<34} {:<7} {:<20} FLAGS", "SETTING", "FILE", "VALUE");
    for r in rows {
        if changed && r.get("source").is_none_or(Value::is_null) {
            continue;
        }
        let mut flags = Vec::new();
        if r["readable"] == Value::Bool(false) {
            flags.push("unreadable");
        }
        if r["writable"] == Value::Bool(false) {
            flags.push("read-only");
        }
        if !r.get("source").is_none_or(Value::is_null) {
            flags.push("set");
        }
        println!(
            "{:<34} {:<7} {:<20} {}",
            s(r, "path"),
            s(r, "file"),
            scalar_str(r.get("value").unwrap_or(&Value::Null)),
            flags.join(",")
        );
    }
}

fn print_config_key(rows: &[Value]) {
    let Some(r) = rows.first() else {
        eprintln!("no such setting");
        return;
    };
    println!("{}", s(r, "path"));
    println!("  file      {}", s(r, "file"));
    println!("  type      {}", s(r, "type"));
    if !r.get("constraints").is_none_or(Value::is_null) {
        println!("  allows    {}", r["constraints"]);
    }
    println!(
        "  value     {}",
        scalar_str(r.get("value").unwrap_or(&Value::Null))
    );
    println!(
        "  default   {}",
        scalar_str(r.get("default").unwrap_or(&Value::Null))
    );
    println!("  source    {}", s(r, "source"));
    println!("  reload    {}", s(r, "reload"));
    println!(
        "  writable  {}",
        if r["writable"] == Value::Bool(true) {
            "yes"
        } else {
            "no"
        }
    );
    println!("  {}", s(r, "doc"));
}

fn print_table(table: Table, result: &Value) {
    // The config verbs answer with an object, not a bare array.
    match table {
        Table::Config { changed } => {
            return print_config_list(result["keys"].as_array().unwrap_or(&Vec::new()), changed)
        }
        Table::ConfigKey => return print_config_key(result["keys"].as_array().unwrap_or(&Vec::new())),
        Table::ConfigValue => {
            let empty = Vec::new();
            let rows = result["keys"].as_array().unwrap_or(&empty);
            match rows.first() {
                Some(r) => println!("{}", scalar_str(r.get("value").unwrap_or(&Value::Null))),
                None => eprintln!("no such setting"),
            }
            return;
        }
        Table::ConfigCheck => {
            if result["valid"] == Value::Bool(true) {
                println!("ok");
                return;
            }
            for e in result["errors"].as_array().unwrap_or(&Vec::new()) {
                println!(
                    "{}:{}:{}: {}",
                    s(e, "file"),
                    s(e, "line"),
                    s(e, "col"),
                    s(e, "message")
                );
            }
            return;
        }
        _ => {}
    }
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
        Table::Config { .. } | Table::ConfigKey | Table::ConfigValue | Table::ConfigCheck => {
            unreachable!("handled above")
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
