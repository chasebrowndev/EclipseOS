// SPDX-License-Identifier: AGPL-3.0-only
//! `apoth proxy [--session NAME] [--no-start]` (T-22 §5): the remote end of a mux domain.
//! A view runs it over ssh stdio; it copies bytes between its stdin/stdout and the local
//! session daemon's socket, so the view speaks the T-21 records to the daemon with no
//! listener and no port. It reads nothing it relays (no parsing, no logging).
//!
//! The daemon sees the proxy as its peer and applies the T-21 §3 attach rules to it. With
//! no daemon running, the proxy starts one (`apotheosis --server`) unless `--no-start` (a
//! reconnect must not open a fresh session where the old one ended). The remote binary is
//! never installed or downloaded here (T-22 §5).

use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::ctl::{ERROR, OK, USAGE};

/// No session daemon (and none could be started).
const NO_SERVER: i32 = 4;

fn valid_session(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && !s.starts_with('.')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// `(session, start)` from the arguments.
pub(crate) fn parse(args: &[String]) -> Result<(String, bool), String> {
    let mut session = "default".to_owned();
    let mut start = true;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--session" => session = it.next().ok_or("--session needs a value")?.clone(),
            "--no-start" => start = false,
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    if !valid_session(&session) {
        return Err("session names are 1-64 of A-Z a-z 0-9 . _ -".into());
    }
    Ok((session, start))
}

/// The `apotheosis` binary next to this one, else by `PATH`.
fn apotheosis_exe() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("apotheosis")))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("apotheosis"))
}

fn start_server(session: &str, path: &std::path::Path) -> Result<UnixStream, String> {
    use std::os::unix::process::CommandExt;
    let mut child = std::process::Command::new(apotheosis_exe())
        .args(["--server", "--session", session])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("no session server, and apotheosis cannot be started: {e}"))?;
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        if let Ok(s) = UnixStream::connect(path) {
            // The server outlives the proxy; leave it be (it is reaped by init once we exit).
            drop(child);
            return Ok(s);
        }
        if let Ok(Some(_)) = child.try_wait() {
            return Err("the session server did not start".into());
        }
        if Instant::now() > until {
            return Err("the session server did not start in time".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub(crate) fn run(args: &[String]) -> i32 {
    let (session, start) = match parse(args) {
        Ok(v) => v,
        Err(m) => {
            eprintln!("apoth proxy: {m}");
            return USAGE;
        }
    };
    let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    else {
        eprintln!("apoth proxy: XDG_RUNTIME_DIR is not set");
        return NO_SERVER;
    };
    let path = dir.join(format!("apotheosis/server-{session}.sock"));
    let sock = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(_) if start => match start_server(&session, &path) {
            Ok(s) => s,
            Err(m) => {
                eprintln!("apoth proxy: {m}");
                return NO_SERVER;
            }
        },
        Err(_) => {
            eprintln!("apoth proxy: no session server {session:?}");
            return NO_SERVER;
        }
    };
    match pump(io::stdin(), io::stdout().lock(), sock) {
        Ok(()) => OK,
        Err(_) => ERROR,
    }
}

/// Copy `input` → `sock` on a thread and `sock` → `output` here, until the socket ends.
/// End of `input` shuts the socket's write side, so the daemon sees the view leave.
pub(crate) fn pump(
    mut input: impl Read + Send,
    mut output: impl Write,
    sock: UnixStream,
) -> io::Result<()> {
    let mut up = sock.try_clone()?;
    let mut down = sock;
    std::thread::scope(|s| {
        s.spawn(move || {
            let mut b = vec![0u8; 64 * 1024];
            loop {
                match input.read(&mut b) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if up.write_all(&b[..n]).is_err() {
                            break;
                        }
                    }
                }
            }
            let _ = up.shutdown(Shutdown::Write);
        });
        let mut b = vec![0u8; 64 * 1024];
        let r = loop {
            match down.read(&mut b) {
                Ok(0) => break Ok(()),
                Ok(n) => {
                    if let Err(e) = output.write_all(&b[..n]).and_then(|()| output.flush()) {
                        break Err(e);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => break Err(e),
            }
        };
        // The scope would wait on the stdin reader; nothing more reaches the daemon.
        let _ = down.shutdown(Shutdown::Both);
        if r.is_ok() {
            // The view's side stays open until our stdout closes: end now.
            let _ = output.flush();
            std::process::exit(OK);
        }
        r
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn arguments() {
        assert_eq!(parse(&a(&[])), Ok(("default".into(), true)));
        assert_eq!(
            parse(&a(&["--session", "w", "--no-start"])),
            Ok(("w".into(), false))
        );
        assert!(parse(&a(&["--session"])).is_err());
        assert!(parse(&a(&["--session", "../x"])).is_err());
        assert!(parse(&a(&["x"])).is_err());
    }
}
