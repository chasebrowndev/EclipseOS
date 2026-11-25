// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-ctl progress` — report work to the progress widget over its feed
//! socket (`$XDG_RUNTIME_DIR/eclipse/progress.sock`, served by `ec-services`).
//!
//! This is not the abyss control socket and carries no authority: the feed
//! is per-user and the wire contract lives in `ec_ipc::progress`.

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    process::ExitCode,
};

use ec_ipc::progress::{socket_path, Request};
use serde_json::Value;

pub const USAGE: &str = "\
ec-ctl progress — show work in the progress widget

  progress start <id> [--label L] [--total BYTES]
  progress set <id> <value> [--eta SECS]
                               0.4 or 40% is a fraction; 400 is bytes (needs --total);
                               --eta is your own estimate of the seconds left
  progress done <id>
  progress abort <id>          the work failed or was cancelled; not kept as a typical run
  progress list
";

pub fn run(args: &[String]) -> ExitCode {
    if args.is_empty() || matches!(args[0].as_str(), "-h" | "--help" | "help") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    match parse(args).and_then(send) {
        Ok(Reply::Ok) => ExitCode::SUCCESS,
        Ok(Reply::Jobs(text)) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("ec-ctl: {e}");
            ExitCode::FAILURE
        }
    }
}

enum Reply {
    Ok,
    Jobs(String),
}

fn parse(args: &[String]) -> Result<Request, String> {
    let a = |i: usize| args.get(i).map(String::as_str);
    let id = |i: usize| {
        a(i).map(str::to_owned)
            .ok_or_else(|| format!("missing id\n{USAGE}"))
    };
    match a(0) {
        Some("start") => {
            let id = id(1)?;
            let mut label = None;
            let mut total = None;
            let mut i = 2;
            while i < args.len() {
                let value = |i: usize| a(i + 1).ok_or_else(|| format!("{} needs a value", args[i]));
                match args[i].as_str() {
                    "--label" => {
                        label = Some(value(i)?.to_owned());
                        i += 2;
                    }
                    "--total" => {
                        let raw = value(i)?;
                        total = Some(
                            raw.parse::<u64>()
                                .map_err(|_| format!("--total expects bytes, got {raw:?}"))?,
                        );
                        i += 2;
                    }
                    other => return Err(format!("unknown option {other:?}")),
                }
            }
            Ok(Request::Start {
                label: label.unwrap_or_else(|| id.clone()),
                id,
                total,
            })
        }
        Some("set") => {
            let id = id(1)?;
            let raw = a(2).ok_or_else(|| format!("set needs a value\n{USAGE}"))?;
            let mut eta = None;
            let mut i = 3;
            while i < args.len() {
                match args[i].as_str() {
                    "--eta" => {
                        let v = a(i + 1).ok_or("--eta needs a value")?;
                        eta = Some(
                            v.parse::<u64>()
                                .map_err(|_| format!("--eta expects whole seconds, got {v:?}"))?,
                        );
                        i += 2;
                    }
                    other => return Err(format!("unknown option {other:?}")),
                }
            }
            if let Some(pct) = raw.strip_suffix('%') {
                let v: f32 = pct.parse().map_err(|_| format!("bad percentage {raw:?}"))?;
                Ok(Request::Fraction {
                    id,
                    fraction: v / 100.0,
                    eta,
                })
            } else if raw.contains('.') {
                let v: f32 = raw.parse().map_err(|_| format!("bad fraction {raw:?}"))?;
                Ok(Request::Fraction { id, fraction: v, eta })
            } else {
                let bytes: u64 = raw
                    .parse()
                    .map_err(|_| format!("expected a fraction (0.4, 40%) or bytes (400), got {raw:?}"))?;
                Ok(Request::Bytes { id, bytes, eta })
            }
        }
        Some("done") => Ok(Request::Done { id: id(1)? }),
        Some("abort") => Ok(Request::Abort { id: id(1)? }),
        Some("list") => Ok(Request::List),
        _ => Err(USAGE.to_owned()),
    }
}

fn send(req: Request) -> Result<Reply, String> {
    let line = req.encode()?;
    let path = socket_path().ok_or("XDG_RUNTIME_DIR is not set")?;
    let mut stream = UnixStream::connect(&path).map_err(|e| {
        format!(
            "{}: {e}\nis the progress widget running in this session?",
            path.display()
        )
    })?;
    stream
        .write_all(format!("{line}\n").as_bytes())
        .map_err(|e| e.to_string())?;
    let mut buf = String::new();
    if BufReader::new(stream)
        .read_line(&mut buf)
        .map_err(|e| e.to_string())?
        == 0
    {
        return Err("the progress feed closed the connection without replying".into());
    }
    let v: Value = serde_json::from_str(&buf).map_err(|e| e.to_string())?;
    if v.get("ok").and_then(Value::as_bool) != Some(true) {
        let why = v.get("error").and_then(Value::as_str).unwrap_or("refused");
        return Err(why.to_owned());
    }
    match req {
        Request::List => Ok(Reply::Jobs(list_text(&v))),
        _ => Ok(Reply::Ok),
    }
}

fn list_text(v: &Value) -> String {
    let jobs = v
        .get("jobs")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if jobs.is_empty() {
        return "no work in progress\n".into();
    }
    let mut out = String::new();
    for j in jobs {
        let id = j.get("id").and_then(Value::as_str).unwrap_or("?");
        let label = j.get("label").and_then(Value::as_str).unwrap_or("");
        let source = j.get("source").and_then(Value::as_str).unwrap_or("");
        let pct = match j.get("fraction").and_then(Value::as_f64) {
            Some(f) => format!("{:>3.0}%", f * 100.0),
            None => "  …".into(),
        };
        let eta = eta_text(j);
        out.push_str(&format!("{pct}  {id:<20} {label} ({source}){eta}\n"));
    }
    out
}

/// The ETA column. An estimate is never shown as exact: a measured rate is
/// `~`, a history estimate is `~` and says it is typical, and only a tool's
/// own figure is shown plain, labelled as the tool's.
fn eta_text(j: &Value) -> String {
    let Some(ms) = j.get("eta_ms").and_then(Value::as_u64) else {
        return String::new();
    };
    let t = fmt_duration(ms / 1000);
    match j.get("eta_basis").and_then(Value::as_str) {
        Some("reported") => format!("  eta {t} (reported)"),
        Some("rate") => format!("  ~{t} left (est.)"),
        Some("history") => format!("  ~{t} left (typical)"),
        _ => String::new(),
    }
}

fn fmt_duration(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m{:02}s", secs / 60, secs % 60),
        _ => format!("{}h{:02}m", secs / 3600, secs % 3600 / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(words: &[&str]) -> Result<Request, String> {
        let args: Vec<String> = words.iter().map(|s| s.to_string()).collect();
        parse(&args)
    }

    #[test]
    fn start_defaults_label_to_id() {
        assert_eq!(
            p(&["start", "b1", "--total", "100"]).unwrap(),
            Request::Start {
                id: "b1".into(),
                label: "b1".into(),
                total: Some(100)
            }
        );
    }

    #[test]
    fn set_reads_fraction_percent_or_bytes() {
        assert_eq!(
            p(&["set", "b1", "0.4"]).unwrap(),
            Request::Fraction {
                id: "b1".into(),
                fraction: 0.4,
                eta: None
            }
        );
        match p(&["set", "b1", "40%"]).unwrap() {
            Request::Fraction { fraction, .. } => assert!((fraction - 0.4).abs() < 1e-6),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            p(&["set", "b1", "400"]).unwrap(),
            Request::Bytes {
                id: "b1".into(),
                bytes: 400,
                eta: None
            }
        );
        assert!(p(&["set", "b1", "lots"]).is_err());
    }

    #[test]
    fn set_takes_an_optional_eta() {
        match p(&["set", "b1", "40%", "--eta", "90"]).unwrap() {
            Request::Fraction { eta, .. } => assert_eq!(eta, Some(90)),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            p(&["set", "b1", "400", "--eta", "5"]).unwrap(),
            Request::Bytes {
                id: "b1".into(),
                bytes: 400,
                eta: Some(5)
            }
        );
        assert!(p(&["set", "b1", "0.5", "--eta"]).is_err());
        assert!(p(&["set", "b1", "0.5", "--eta", "soon"]).is_err());
        assert!(p(&["set", "b1", "0.5", "--eta", "-1"]).is_err());
        assert!(p(&["set", "b1", "0.5", "--bogus", "1"]).is_err());
    }

    #[test]
    fn list_text_marks_every_eta_by_its_basis() {
        let v = serde_json::json!({"ok": true, "jobs": [
            {"id": "a", "label": "x", "source": "feed", "fraction": 0.5, "eta_ms": 90000, "eta_basis": "reported"},
            {"id": "b", "label": "y", "source": "feed", "fraction": 0.5, "eta_ms": 12000, "eta_basis": "rate"},
            {"id": "c", "label": "z", "source": "scan", "fraction": null, "eta_ms": 3700000, "eta_basis": "history"},
            {"id": "d", "label": "w", "source": "scan", "fraction": null, "eta_ms": null, "eta_basis": null}
        ]});
        let t = list_text(&v);
        assert!(t.contains("eta 1m30s (reported)"), "{t}");
        assert!(t.contains("~12s left (est.)"), "{t}");
        assert!(t.contains("~1h01m left (typical)"), "{t}");
        assert!(t.lines().last().unwrap().trim_end().ends_with("(scan)"));
    }

    #[test]
    fn rejects_unknown_options_and_verbs() {
        assert!(p(&["start", "b1", "--bogus"]).is_err());
        assert!(p(&["start", "b1", "--total", "x"]).is_err());
        assert!(p(&["frobnicate"]).is_err());
        assert!(p(&["done"]).is_err());
        assert!(p(&["abort"]).is_err());
        assert_eq!(p(&["abort", "b1"]).unwrap(), Request::Abort { id: "b1".into() });
    }

    #[test]
    fn list_text_shows_indeterminate_and_fraction() {
        let v = serde_json::json!({"ok": true, "jobs": [
            {"id": "a", "label": "cp x", "source": "scan", "fraction": 0.5},
            {"id": "b", "label": "tar y", "source": "feed", "fraction": null}
        ]});
        let t = list_text(&v);
        assert!(t.contains(" 50%  a"));
        assert!(t.contains("…  b"));
        assert_eq!(
            list_text(&serde_json::json!({"ok": true, "jobs": []})),
            "no work in progress\n"
        );
    }
}
