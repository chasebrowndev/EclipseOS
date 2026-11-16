// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-secret-prompt`'s Terminal target, instance side (T-22 §3; ADR 0053 amendment of
//! 2026-11-15).
//!
//! On Abyss a password a pane's program asks for goes to `ec-secret-prompt terminal`
//! instead of the in-window sheet. The instance spawns the prompt itself, so it is the
//! prompt's parent (the prompt checks that before it calls anything "verified"). It writes
//! one [`Context`] to the prompt's stdin and reads the answer from its stdout, both on a
//! thread of their own. The secret is kept in one fixed buffer, moved into a [`Secret`],
//! and the buffer is wiped. Nothing here logs the context or the secret. Ids, sizes and
//! outcomes only.
//!
//! The kernel facts (ancestry, exe link, uids, cmdline, cwd) are read on that thread from
//! `/proc`, never on the main loop.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use apoth_proto::askpass::{MAX_SECRET, wipe};
use apoth_proto::secret_prompt::{
    Context, MAX_ANCESTRY, MAX_ARGV, MAX_TEXT, Reply, Unverified, VERSION, Verified, Via, cut,
    decode_reply,
};

use crate::askpass::{Answer, Secret};

/// The prompt's binary name, looked up on `PATH`.
pub(crate) const PROGRAM: &str = "ec-secret-prompt";

static NEXT: AtomicU64 = AtomicU64::new(1);

/// Where `ec-secret-prompt` is, if it is installed. Debug builds take
/// `APOTH_SECRET_PROMPT` first (the session tests' stand-in); release builds never read it.
pub(crate) fn program() -> Option<PathBuf> {
    #[cfg(debug_assertions)]
    if let Some(p) = std::env::var_os("APOTH_SECRET_PROMPT") {
        return Some(PathBuf::from(p));
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(PROGRAM))
        .find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// What main knows about the request; the thread adds the `/proc` facts.
pub(crate) struct Request {
    pub(crate) via: Via,
    pub(crate) window: String,
    pub(crate) tab: String,
    pub(crate) pane: String,
    pub(crate) pane_id: Option<u64>,
    /// The pane's foreground process group (`tcgetpgrp`), when there is a pane.
    pub(crate) fg_pgrp: Option<i32>,
    /// The requester (see [`Verified::pid`]).
    pub(crate) pid: i32,
    /// Where its ancestry walk ends: the armed process, or the pane's child.
    pub(crate) root: i32,
    /// Program text: the prompt it passed or printed.
    pub(crate) prompt: String,
    /// The host an `apoth ssh` named.
    pub(crate) host: Option<String>,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("via", &self.via)
            .field("pane_id", &self.pane_id)
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

/// A running prompt. Dropping this does not stop it; [`Running::kill`] does.
#[derive(Clone)]
pub(crate) struct Running {
    pub(crate) id: u64,
    child: Arc<Mutex<Option<Child>>>,
}

impl std::fmt::Debug for Running {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Running({})", self.id)
    }
}

impl Running {
    /// Close the prompt (the request was withdrawn). The child is not reaped until its
    /// thread takes it under the same lock, so the pid cannot have been reused.
    pub(crate) fn kill(&self) {
        if let Ok(mut c) = self.child.lock()
            && let Some(c) = c.as_mut()
        {
            let _ = c.kill();
        }
    }
}

/// Facts read from `/proc` for `pid`, ancestry walked up to `root`.
#[derive(Default)]
pub(crate) struct Facts {
    pub(crate) ancestry: Vec<u32>,
    pub(crate) pgrp: Option<i32>,
    pub(crate) exe: Option<String>,
    pub(crate) uid: u32,
    pub(crate) euid: u32,
    pub(crate) argv: Vec<String>,
    pub(crate) cwd: Option<String>,
}

/// `Uid:` line of `/proc/<pid>/status`: real and effective.
pub(crate) fn parse_uids(status: &str) -> Option<(u32, u32)> {
    let line = status.lines().find(|l| l.starts_with("Uid:"))?;
    let mut f = line["Uid:".len()..].split_whitespace();
    Some((f.next()?.parse().ok()?, f.next()?.parse().ok()?))
}

/// `/proc/<pid>/cmdline` as argv (lossy), within the budgets.
pub(crate) fn parse_cmdline(b: &[u8]) -> Vec<String> {
    let b = b.strip_suffix(&[0]).unwrap_or(b);
    if b.is_empty() {
        return Vec::new();
    }
    b.split(|&c| c == 0)
        .take(MAX_ARGV)
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect()
}

/// The pids from `pid` up to `root` (both included when `root` is reached).
pub(crate) fn ancestry(pid: i32, root: i32, ppid: impl Fn(i32) -> Option<i32>) -> Vec<u32> {
    let mut out = Vec::new();
    let mut cur = pid;
    while out.len() < MAX_ANCESTRY {
        out.push(u32::try_from(cur).unwrap_or(0));
        if cur == root {
            break;
        }
        match ppid(cur) {
            Some(p) if p > 1 => cur = p,
            _ => break,
        }
    }
    out
}

fn stat(pid: i32) -> Option<apoth_pty::StatInfo> {
    let b = std::fs::read(format!("/proc/{pid}/stat")).ok()?;
    apoth_pty::parse_stat(&b)
}

impl Facts {
    pub(crate) fn read(pid: i32, root: i32) -> Facts {
        let lossy = |p: PathBuf| cut(p.to_string_lossy().into_owned(), MAX_TEXT);
        let (uid, euid) = std::fs::read_to_string(format!("/proc/{pid}/status"))
            .ok()
            .and_then(|s| parse_uids(&s))
            .unwrap_or((u32::MAX, u32::MAX));
        Facts {
            ancestry: ancestry(pid, root, |p| stat(p).map(|s| s.ppid)),
            pgrp: stat(pid).map(|s| s.pgrp),
            exe: std::fs::read_link(format!("/proc/{pid}/exe"))
                .ok()
                .map(lossy),
            uid,
            euid,
            argv: std::fs::read(format!("/proc/{pid}/cmdline"))
                .map(|b| parse_cmdline(&b))
                .unwrap_or_default(),
            cwd: std::fs::read_link(format!("/proc/{pid}/cwd"))
                .ok()
                .map(lossy),
        }
    }
}

/// The context for `r` with `facts`.
pub(crate) fn context(r: Request, facts: Facts) -> Context {
    let in_foreground = matches!((r.fg_pgrp, facts.pgrp), (Some(a), Some(b)) if a == b);
    Context {
        v: VERSION,
        via: r.via,
        verified: Verified {
            window: r.window,
            tab: r.tab,
            pane: r.pane,
            pane_id: r.pane_id,
            in_foreground,
            ancestry: facts.ancestry,
            pid: u32::try_from(r.pid).unwrap_or(0),
            exe: facts.exe,
            uid: facts.uid,
            euid: facts.euid,
        },
        unverified: Unverified {
            argv: facts.argv,
            cwd: facts.cwd,
            prompt: r.prompt,
            host: r.host,
        },
    }
}

/// Start `program terminal` for `r`. `done` runs on the prompt's thread with the answer
/// (`Cancel` for a cancel, a close, a kill or anything malformed).
pub(crate) fn spawn(
    program: &Path,
    r: Request,
    done: impl FnOnce(u64, Answer) + Send + 'static,
) -> std::io::Result<Running> {
    let mut c = Command::new(program);
    c.arg("terminal")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // The prompt has no business with the armed channel.
        .env_remove(apoth_proto::askpass::ENV_SOCK)
        .env_remove(apoth_proto::askpass::ENV_TOKEN);
    let mut child = c.spawn()?;
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let child = Arc::new(Mutex::new(Some(child)));
    let running = Running {
        id,
        child: Arc::clone(&child),
    };
    let via = r.via;
    std::thread::Builder::new()
        .name("apoth-secret-prompt".into())
        .spawn(move || {
            let facts = Facts::read(r.pid, r.root);
            let bytes = context(r, facts).encode();
            if let Some(mut i) = stdin {
                let _ = i.write_all(&bytes);
            }
            drop(bytes);
            let answer = stdout.map_or(Answer::Cancel, read_answer);
            if let Ok(mut c) = child.lock()
                && let Some(mut c) = c.take()
            {
                let _ = c.kill();
                let _ = c.wait();
            }
            let answered = matches!(answer, Answer::Secret(_));
            tracing::info!(prompt = id, ?via, answered, "secret prompt ended");
            done(id, answer);
        })?;
    tracing::info!(prompt = id, ?via, "secret prompt started");
    Ok(running)
}

/// Read the prompt's stdout to EOF into one fixed buffer and decode it.
fn read_answer(mut out: impl Read) -> Answer {
    let mut buf = [0u8; 1 + MAX_SECRET + 1];
    let mut n = 0;
    while n < buf.len() {
        match out.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    let answer = match decode_reply(&buf[..n]) {
        Reply::Answered(s) => {
            let mut secret = Secret::new();
            if secret.push_bytes(s) {
                Answer::Secret(secret)
            } else {
                Answer::Cancel
            }
        }
        Reply::Cancelled => Answer::Cancel,
    };
    wipe(&mut buf);
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_parsers() {
        assert_eq!(
            parse_uids("Name:\tsudo\nUid:\t1000\t0\t0\t0\nGid:\t1\n"),
            Some((1000, 0))
        );
        assert_eq!(parse_uids("Name: x\n"), None);
        assert_eq!(
            parse_cmdline(b"sudo\0-A\0pacman\0"),
            vec!["sudo", "-A", "pacman"]
        );
        assert!(parse_cmdline(b"").is_empty());
        let parents = |p: i32| match p {
            40 => Some(30),
            30 => Some(20),
            20 => Some(1),
            _ => None,
        };
        assert_eq!(ancestry(40, 20, parents), vec![40, 30, 20]);
        assert_eq!(ancestry(40, 99, parents), vec![40, 30, 20]);
        assert_eq!(ancestry(5, 5, parents), vec![5]);
        assert_eq!(ancestry(3, 9, |_| Some(3)).len(), MAX_ANCESTRY);
    }

    #[test]
    fn own_facts_are_read_and_in_foreground_compares_groups() {
        let me = std::process::id() as i32;
        let f = Facts::read(me, me);
        assert_eq!(f.ancestry, vec![me as u32]);
        assert!(f.exe.is_some() && !f.argv.is_empty());
        assert_eq!(f.uid, rustix::process::getuid().as_raw());
        let pgrp = f.pgrp;
        let req = |fg| Request {
            via: Via::Pty,
            window: "w".into(),
            tab: "t".into(),
            pane: "p".into(),
            pane_id: Some(1),
            fg_pgrp: fg,
            pid: me,
            root: me,
            prompt: "Password: ".into(),
            host: None,
        };
        assert!(
            context(req(pgrp), Facts::read(me, me))
                .verified
                .in_foreground
        );
        assert!(
            !context(req(Some(-5)), Facts::read(me, me))
                .verified
                .in_foreground
        );
        assert!(!context(req(None), f).verified.in_foreground);
    }

    #[test]
    fn answers_are_decoded_and_bounded() {
        let a = read_answer(&b"\0hunter2"[..]);
        let Answer::Secret(s) = a else {
            panic!("expected a secret")
        };
        assert_eq!(s.bytes(), b"hunter2");
        assert!(matches!(read_answer(&b"\x01"[..]), Answer::Cancel));
        assert!(matches!(read_answer(&b""[..]), Answer::Cancel));
        let mut over = vec![0u8];
        over.extend(std::iter::repeat_n(b'x', MAX_SECRET + 5));
        assert!(matches!(read_answer(&over[..]), Answer::Cancel));
    }

    /// A stand-in prompt: reads the context, answers, and the answer reaches `done`.
    #[test]
    fn a_spawned_prompt_round_trips() {
        let dir = std::env::temp_dir().join(format!("apsp{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("prompt");
        let seen = dir.join("ctx");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n[ \"$1\" = terminal ] || exit 2\ncat > '{}'\nprintf '\\000pw'\n",
                seen.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let me = std::process::id() as i32;
        let (tx, rx) = std::sync::mpsc::channel();
        let r = Request {
            via: Via::Askpass,
            window: "w".into(),
            tab: "t".into(),
            pane: "p".into(),
            pane_id: None,
            fg_pgrp: None,
            pid: me,
            root: me,
            prompt: "pw:".into(),
            host: Some("me@box".into()),
        };
        spawn(&script, r, move |_, a| {
            let _ = tx.send(a);
        })
        .unwrap();
        let a = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        let Answer::Secret(s) = a else {
            panic!("expected a secret")
        };
        assert_eq!(s.bytes(), b"pw");
        let ctx = Context::decode(&std::fs::read(&seen).unwrap()).unwrap();
        assert_eq!(ctx.via, Via::Askpass);
        assert_eq!(ctx.unverified.host.as_deref(), Some("me@box"));
        assert_eq!(ctx.verified.pid, me as u32);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
