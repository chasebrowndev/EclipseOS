// SPDX-License-Identifier: AGPL-3.0-only
//! `apoth doctor`: one line per check, `ok`/`warn`/`fail`. The spec (T-12 §5) only calls it a
//! capability and environment report, so this is the minimal diagnostic: exit 0 unless a check
//! fails (exit 1). Nothing user-authored is printed; the journal tail is log lines only.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use apoth_config::fs::RealFs;
use serde_json::{Value, json};

use crate::ctl::{self, Client, ERROR, OK};

const SHORT: Duration = Duration::from_secs(2);
const LINE_MAX: usize = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    Ok,
    Warn,
    Fail,
}

impl Status {
    fn word(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Fail => "fail",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Check {
    pub(crate) name: String,
    pub(crate) status: Status,
    pub(crate) detail: String,
}

fn check(name: &str, status: Status, detail: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        status,
        detail: detail.into(),
    }
}

/// The pid in a `<pid>.sock` file name; other names are not control sockets.
pub(crate) fn socket_pid(name: &str) -> Option<u32> {
    let stem = name.strip_suffix(".sock")?;
    if stem.is_empty() || !stem.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    stem.parse().ok()
}

/// 1 when any check failed.
pub(crate) fn exit_code(checks: &[Check]) -> i32 {
    if checks.iter().any(|c| c.status == Status::Fail) {
        ERROR
    } else {
        OK
    }
}

pub(crate) fn render_text(checks: &[Check], journal: &[String]) -> String {
    let mut s = String::new();
    for c in checks {
        s.push_str(&format!(
            "{:<4} {}: {}\n",
            c.status.word(),
            c.name,
            c.detail
        ));
    }
    if !journal.is_empty() {
        s.push_str("recent warnings and errors:\n");
        for l in journal {
            s.push_str("  ");
            s.push_str(l);
            s.push('\n');
        }
    }
    s
}

pub(crate) fn render_json(checks: &[Check], journal: &[String]) -> Value {
    json!({
        "ok": exit_code(checks) == OK,
        "checks": checks.iter().map(|c| json!({
            "name": c.name, "status": c.status.word(), "detail": c.detail,
        })).collect::<Vec<_>>(),
        "journal": journal,
    })
}

/// Keep at most `n` last non-empty lines, each cut to [`LINE_MAX`] bytes on a char boundary.
pub(crate) fn tail_lines(text: &str, n: usize) -> Vec<String> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|l| {
            let mut end = l.len().min(LINE_MAX);
            while !l.is_char_boundary(end) {
                end -= 1;
            }
            l[..end].to_owned()
        })
        .collect()
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

fn hello(stream: UnixStream) -> Result<(), String> {
    let mut c = Client::new(stream).map_err(|e| e.to_string())?;
    let _ = c.stream().set_read_timeout(Some(SHORT));
    let _ = c.stream().set_write_timeout(Some(SHORT));
    let h = apoth_proto::Params::Hello(apoth_proto::params::Hello {
        proto: apoth_proto::message::PROTO_VERSION,
        client: "apoth".into(),
        want: Vec::new(),
    });
    c.call(h).map(|_| ()).map_err(|e| e.message)
}

fn sockets(dir: &Path) -> Vec<Check> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return vec![check(
            "sockets",
            Status::Warn,
            format!("{} does not exist (no instance has run)", dir.display()),
        )];
    };
    let mut names: Vec<String> = rd
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| socket_pid(n).is_some())
        .collect();
    names.sort();
    if names.is_empty() {
        return vec![check(
            "sockets",
            Status::Ok,
            "no sockets; no instance running",
        )];
    }
    names
        .iter()
        .map(|n| {
            let name = format!("socket {n}");
            let pid = socket_pid(n).unwrap_or(0);
            if !Path::new(&format!("/proc/{pid}")).exists() {
                return check(&name, Status::Warn, format!("stale: pid {pid} is gone"));
            }
            match UnixStream::connect(dir.join(n)) {
                Err(e) => check(&name, Status::Warn, format!("stale: not connectable ({e})")),
                Ok(s) => match hello(s) {
                    Ok(()) => check(&name, Status::Ok, format!("pid {pid} answers hello")),
                    Err(m) => check(
                        &name,
                        Status::Fail,
                        format!("pid {pid} live, no hello: {m}"),
                    ),
                },
            }
        })
        .collect()
}

fn wayland(runtime: Option<&Path>) -> Check {
    let Some(d) = std::env::var_os("WAYLAND_DISPLAY").filter(|d| !d.is_empty()) else {
        return check("wayland", Status::Warn, "WAYLAND_DISPLAY is not set");
    };
    let mut p = PathBuf::from(&d);
    if !p.is_absolute() {
        match runtime {
            Some(r) => p = r.join(p),
            None => return check("wayland", Status::Warn, "cannot resolve the socket"),
        }
    }
    match UnixStream::connect(&p) {
        Ok(_) => check(
            "wayland",
            Status::Ok,
            format!("{} connectable", p.display()),
        ),
        Err(e) => check("wayland", Status::Fail, format!("{}: {e}", p.display())),
    }
}

fn binary() -> Check {
    let found = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|d| d.join("apotheosis"))
            .find(|p| p.is_file())
    });
    let Some(p) = found else {
        return check("binary", Status::Warn, "`apotheosis` not found on PATH");
    };
    let Ok(mut child) = Command::new(&p)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return check(
            "binary",
            Status::Warn,
            format!("{}: cannot run", p.display()),
        );
    };
    let t = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) => {
                let mut v = String::new();
                if let Some(mut o) = child.stdout.take() {
                    let _ = std::io::Read::read_to_string(&mut o, &mut v);
                }
                let v = tail_lines(&v, 1).pop().unwrap_or_default();
                let s = if st.success() {
                    Status::Ok
                } else {
                    Status::Warn
                };
                return check("binary", s, format!("{} {v}", p.display()));
            }
            Ok(None) if t.elapsed() < SHORT => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return check(
                    "binary",
                    Status::Warn,
                    format!("{}: --version timed out", p.display()),
                );
            }
        }
    }
}

fn config() -> Check {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return check("config", Status::Fail, "HOME is not set");
    };
    let xdg = env_path("XDG_CONFIG_HOME");
    let (e, w) = crate::config::diagnostic_counts(&RealFs, &home, xdg.as_deref());
    let s = match (e, w) {
        (0, 0) => Status::Ok,
        (0, _) => Status::Warn,
        _ => Status::Fail,
    };
    check(
        "config",
        s,
        format!("{e} error(s), {w} warning(s) (apoth config validate)"),
    )
}

fn journal() -> Vec<String> {
    let Ok(o) = Command::new("journalctl")
        .args(["--user", "-t", "apotheosis", "-p", "warning", "-n", "10"])
        .args(["--no-pager", "-o", "cat"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    tail_lines(&String::from_utf8_lossy(&o.stdout), 10)
}

pub(crate) fn run(o: &ctl::Opts) -> i32 {
    let mut checks = Vec::new();
    let runtime = env_path("XDG_RUNTIME_DIR");
    match &runtime {
        Some(r) => {
            checks.push(check("runtime dir", Status::Ok, r.display().to_string()));
            checks.extend(sockets(&r.join("apotheosis")));
        }
        None => checks.push(check(
            "runtime dir",
            Status::Fail,
            "XDG_RUNTIME_DIR is not set",
        )),
    }
    checks.push(wayland(runtime.as_deref()));
    checks.push(binary());
    checks.push(config());
    let j = journal();
    let code = exit_code(&checks);
    let text = if o.json {
        format!("{}\n", render_json(&checks, &j))
    } else {
        render_text(&checks, &j)
    };
    let _ = std::io::Write::write_all(&mut std::io::stdout(), text.as_bytes());
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_from_socket_name() {
        assert_eq!(socket_pid("123.sock"), Some(123));
        assert_eq!(socket_pid("server-a.sock"), None);
        assert_eq!(socket_pid("askpass-1.sock"), None);
        assert_eq!(socket_pid(".sock"), None);
        assert_eq!(socket_pid("12.txt"), None);
    }

    #[test]
    fn exit_code_is_one_only_for_fail() {
        let w = check("a", Status::Warn, "x");
        let f = check("b", Status::Fail, "y");
        assert_eq!(exit_code(&[]), OK);
        assert_eq!(exit_code(&[check("a", Status::Ok, ""), w]), OK);
        assert_eq!(exit_code(&[f]), ERROR);
    }

    #[test]
    fn render_golden() {
        let c = vec![
            check("runtime dir", Status::Ok, "/run/user/1"),
            check("wayland", Status::Warn, "WAYLAND_DISPLAY is not set"),
        ];
        let j = vec!["boom".to_owned()];
        assert_eq!(
            render_text(&c, &j),
            "ok   runtime dir: /run/user/1\nwarn wayland: WAYLAND_DISPLAY is not set\nrecent warnings and errors:\n  boom\n"
        );
        let v = render_json(&c, &j);
        assert_eq!(v["ok"], true);
        assert_eq!(v["checks"][1]["status"], "warn");
        assert_eq!(v["journal"][0], "boom");
    }

    #[test]
    fn tail_keeps_last_and_cuts() {
        assert_eq!(tail_lines("a\n\nb\nc\n", 2), vec!["b", "c"]);
        let long = "é".repeat(400);
        let t = tail_lines(&long, 1);
        assert!(t[0].len() <= LINE_MAX && t[0].chars().all(|c| c == 'é'));
    }
}
