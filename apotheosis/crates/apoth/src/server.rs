// SPDX-License-Identifier: AGPL-3.0-only
//! The session daemon from the command line (T-21 §3): `apoth server ls` lists the running
//! sessions (ids, sizes and view counts: `observe`, no content), `apoth detach [SESSION]`
//! detaches every view of a session (the panes keep running), `apoth attach [SESSION]`
//! opens a window on it (runs `apotheosis --attach`). T-12 §5 does not name these yet
//! (reported).

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use apoth_proto::mux::{
    Decoded, MUX_MAJOR, MUX_MINOR, PaneEntry, RecordDecoder, ServerMsg, ViewMsg,
};

use crate::ctl::{ERROR, OK, USAGE};

/// No session daemon answered.
const NO_SERVER: i32 = 4;
const TIMEOUT: Duration = Duration::from_secs(5);

fn dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .map(|p| p.join("apotheosis"))
}

/// `(session, socket)` for every `server-<session>.sock`, by name.
pub(crate) fn sessions(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<(String, PathBuf)> = rd
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let s = name.strip_prefix("server-")?.strip_suffix(".sock")?;
            (!s.is_empty()).then(|| (s.to_owned(), e.path()))
        })
        .collect();
    v.sort();
    v
}

struct Conn {
    s: UnixStream,
    dec: RecordDecoder,
}

impl Conn {
    /// Connect and say hello; `Err` carries the user-facing reason.
    fn open(path: &Path) -> Result<Conn, String> {
        let s = UnixStream::connect(path).map_err(|e| e.to_string())?;
        s.set_read_timeout(Some(TIMEOUT))
            .map_err(|e| e.to_string())?;
        s.set_write_timeout(Some(TIMEOUT))
            .map_err(|e| e.to_string())?;
        let mut c = Conn {
            s,
            dec: RecordDecoder::new(),
        };
        c.send(&ViewMsg::Hello {
            major: MUX_MAJOR,
            minor: MUX_MINOR,
            caps: Vec::new(),
            client: "apoth".into(),
        })?;
        match c.recv()? {
            ServerMsg::Welcome { .. } => Ok(c),
            ServerMsg::Refused { reason } => Err(reason),
            _ => Err("unexpected answer".into()),
        }
    }

    fn send(&mut self, m: &ViewMsg) -> Result<(), String> {
        self.s.write_all(&m.encode()).map_err(|e| e.to_string())
    }

    fn recv(&mut self) -> Result<ServerMsg, String> {
        let until = Instant::now() + TIMEOUT;
        loop {
            if let Some(r) = self.dec.next_record() {
                let rec = r.map_err(|_| "malformed answer".to_owned())?;
                match ServerMsg::decode(rec) {
                    Ok(Decoded::Msg(m)) => return Ok(m),
                    Ok(Decoded::Skipped(_)) => continue,
                    Err(_) => return Err("malformed answer".into()),
                }
            }
            if Instant::now() > until {
                return Err("no answer".into());
            }
            let mut b = [0u8; 4096];
            match self.s.read(&mut b) {
                Ok(0) => return Err("the server closed the connection".into()),
                Ok(n) => self.dec.feed(&b[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.to_string()),
            }
        }
    }
}

fn list(path: &Path) -> Result<Vec<PaneEntry>, String> {
    let mut c = Conn::open(path)?;
    c.send(&ViewMsg::List)?;
    loop {
        match c.recv()? {
            ServerMsg::Panes(p) => return Ok(p),
            ServerMsg::Refused { reason } => return Err(reason),
            _ => {}
        }
    }
}

/// One `apoth server ls` line.
pub(crate) fn render_ls(session: &str, panes: &[PaneEntry]) -> String {
    let views = panes.first().map_or(0, |p| p.views);
    let mut s = format!(
        "{session}\t{} pane{}\t{views} view{}",
        panes.len(),
        if panes.len() == 1 { "" } else { "s" },
        if views == 1 { "" } else { "s" },
    );
    for p in panes {
        s.push_str(&format!("\n  pane {}\t{}x{}", p.pane, p.cols, p.rows));
    }
    s.push('\n');
    s
}

fn session_arg(args: &[String]) -> Result<String, String> {
    match args {
        [] => Ok("default".into()),
        [s] if !s.starts_with('-') => Ok(s.clone()),
        _ => Err("expects at most one SESSION".into()),
    }
}

/// `apoth server ls`.
pub(crate) fn server(args: &[String]) -> i32 {
    if args != ["ls"] {
        eprintln!("apoth server: usage: apoth server ls");
        return USAGE;
    }
    let Some(dir) = dir() else {
        eprintln!("apoth server: XDG_RUNTIME_DIR is not set");
        return NO_SERVER;
    };
    let mut out = io::stdout().lock();
    let mut any = false;
    for (session, path) in sessions(&dir) {
        match list(&path) {
            Ok(panes) => {
                any = true;
                let _ = out.write_all(render_ls(&session, &panes).as_bytes());
            }
            // A stale socket of a server that is gone: not a session.
            Err(_) if UnixStream::connect(&path).is_err() => {}
            Err(e) => eprintln!("apoth server: {session}: {e}"),
        }
    }
    if any { OK } else { NO_SERVER }
}

/// `apoth detach [SESSION]`.
pub(crate) fn detach(args: &[String]) -> i32 {
    let session = match session_arg(args) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("apoth detach: {e}");
            return USAGE;
        }
    };
    let Some(path) = dir().map(|d| d.join(format!("server-{session}.sock"))) else {
        eprintln!("apoth detach: XDG_RUNTIME_DIR is not set");
        return NO_SERVER;
    };
    let mut c = match Conn::open(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("apoth detach: no session {session:?}: {e}");
            return NO_SERVER;
        }
    };
    match c.send(&ViewMsg::DetachAll) {
        Ok(()) => OK,
        Err(e) => {
            eprintln!("apoth detach: {e}");
            ERROR
        }
    }
}

/// `apoth attach [SESSION]`: becomes `apotheosis --attach --session SESSION`.
pub(crate) fn attach(args: &[String]) -> i32 {
    use std::os::unix::process::CommandExt;
    let session = match session_arg(args) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("apoth attach: {e}");
            return USAGE;
        }
    };
    // The interface installed next to this `apoth`, else the one on PATH.
    let gui = std::env::current_exe()
        .ok()
        .and_then(|p| Some(p.parent()?.join("apotheosis")))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("apotheosis"));
    let e = std::process::Command::new(gui)
        .args(["--attach", "--session", &session])
        .exec();
    eprintln!("apoth attach: cannot start apotheosis: {e}");
    ERROR
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_are_server_sockets_only() {
        let d = std::env::temp_dir().join(format!("apoth-srv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for n in [
            "server-b.sock",
            "server-a.sock",
            "123.sock",
            "server-.sock",
            "server-c.txt",
        ] {
            std::fs::write(d.join(n), b"").unwrap();
        }
        let s: Vec<String> = sessions(&d).into_iter().map(|(s, _)| s).collect();
        assert_eq!(s, ["a", "b"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ls_lines() {
        let p = |pane, views| PaneEntry {
            pane,
            rows: 24,
            cols: 80,
            alive: true,
            views,
        };
        assert_eq!(
            render_ls("default", &[p(1, 1), p(3, 1)]),
            "default\t2 panes\t1 view\n  pane 1\t80x24\n  pane 3\t80x24\n"
        );
        assert_eq!(
            render_ls("w", &[p(2, 0)]),
            "w\t1 pane\t0 views\n  pane 2\t80x24\n"
        );
        assert_eq!(session_arg(&[]), Ok("default".into()));
        assert_eq!(session_arg(&["w".into()]), Ok("w".into()));
        assert!(session_arg(&["-x".into()]).is_err());
        assert!(session_arg(&["a".into(), "b".into()]).is_err());
    }
}
