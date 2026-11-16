// SPDX-License-Identifier: AGPL-3.0-only
//! The Terminal target (ADR 0053 amendment of 2026-11-15; wire: T-22 §3 of
//! `docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`).
//!
//! `ec-secret-prompt terminal` is spawned by Apotheosis for a password a program in one of
//! its panes asks for (`sudo -A`, `apoth ssh`, or a getpass-style read the terminal
//! noticed). The request comes on stdin as one JSON object; the answer goes out on stdout
//! as one status byte and the secret, and Apotheosis relays it to the program. Nothing
//! is taken from argv (world-readable in `/proc`).
//!
//! The request has two blocks: facts Apotheosis read from the kernel, and what the program
//! says about itself. The facts are shown as **verified** only when this process's parent
//! is a live Apotheosis ([`parent_is_apotheosis`]). Any same-uid process can spawn this
//! binary, and the check is what stops it passing off its own "facts". Otherwise they are
//! shown as unverified with [`UNVERIFIED_NOTE`].
//!
//! The request carries the program's argv. It is never logged, stored or audited, and
//! `Debug` here prints counts only.

use std::fmt;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Deserialize;

use crate::wipe;

/// Largest request on stdin (the wire's budget).
pub const MAX_CONTEXT: usize = 64 * 1024;
/// Largest answer: the askpass wire's secret budget.
pub const MAX_SECRET: usize = 1024;
/// Status byte: answered, the secret follows.
const ANSWERED: u8 = 0;
/// Status byte: cancelled.
const CANCELLED: u8 = 1;
/// The installed Apotheosis, the only parent whose facts count as verified.
pub const APOTHEOSIS: &str = "/usr/bin/apotheosis";
/// Shown with the facts when the parent check failed.
pub const UNVERIFIED_NOTE: &str =
    "These details did not come from Apotheosis. Anything here could be made up.";

/// How the request reached Apotheosis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Via {
    /// `sudo -A` or `apoth ssh` asked through the armed askpass channel.
    Askpass,
    /// The focused pane started a password read; the answer is typed into the pane.
    Pty,
}

/// The facts block, as Apotheosis sent it.
#[derive(Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Facts {
    /// Window title.
    pub window: String,
    /// Tab title.
    pub tab: String,
    /// Pane title.
    pub pane: String,
    /// The pane's id (`None`: a mux link with no pane).
    pub pane_id: Option<u64>,
    /// The requester is in the pane's foreground process group.
    pub in_foreground: bool,
    /// Pids from the requester up to the pane's process.
    pub ancestry: Vec<u32>,
    /// The requesting process.
    pub pid: u32,
    /// Its executable (`None`: unreadable, as for setuid `sudo`).
    pub exe: Option<String>,
    /// Real uid.
    pub uid: u32,
    /// Effective uid (0 for `sudo`).
    pub euid: u32,
}

/// What the program says about itself. Always shown as unverified.
#[derive(Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claims {
    /// Its command line.
    pub argv: Vec<String>,
    /// Its working directory.
    pub cwd: Option<String>,
    /// The prompt it printed or passed (program text).
    pub prompt: String,
    /// The host an `apoth ssh` named.
    pub host: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    v: u32,
    via: Via,
    verified: Facts,
    unverified: Claims,
}

/// The view model of a Terminal-target window: everything the frontend draws for it.
#[derive(Clone, PartialEq, Eq)]
pub struct TerminalAsk {
    pub via: Via,
    /// The parent check passed: draw [`TerminalAsk::facts`] as the verified block.
    /// When false, draw them inside the unverified block with [`UNVERIFIED_NOTE`].
    pub verified: bool,
    pub facts: Facts,
    pub claims: Claims,
}

impl fmt::Debug for TerminalAsk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TerminalAsk")
            .field("via", &self.via)
            .field("verified", &self.verified)
            .field("pane_id", &self.facts.pane_id)
            .field("argc", &self.claims.argv.len())
            .finish_non_exhaustive()
    }
}

fn base(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

impl TerminalAsk {
    /// Parse what Apotheosis wrote. `verified` comes from [`parent_is_apotheosis`], never
    /// from the request.
    pub fn parse(b: &[u8], verified: bool) -> Result<TerminalAsk, String> {
        if b.len() > MAX_CONTEXT {
            return Err("the request is too large".to_owned());
        }
        let w: Wire = serde_json::from_slice(b).map_err(|_| "the request is malformed".to_owned())?;
        if w.v != 1 {
            return Err("the request is from another version".to_owned());
        }
        Ok(TerminalAsk {
            via: w.via,
            verified,
            facts: w.verified,
            claims: w.unverified,
        })
    }

    /// Read the request from stdin (to EOF, bounded) and run the parent check.
    pub fn from_stdin() -> Result<TerminalAsk, String> {
        let mut b = Vec::new();
        std::io::stdin()
            .lock()
            .take(MAX_CONTEXT as u64 + 1)
            .read_to_end(&mut b)
            .map_err(|_| "no request on stdin".to_owned())?;
        TerminalAsk::parse(&b, parent_is_apotheosis())
    }

    /// The name to put in the heading: the verified executable's file name, else the
    /// program's own argv[0] (unverified), else a plain phrase.
    pub fn program(&self) -> &str {
        if let (true, Some(e)) = (self.verified, &self.facts.exe) {
            return base(e);
        }
        self.claims
            .argv
            .first()
            .map(|a| base(a))
            .filter(|a| !a.is_empty())
            .unwrap_or("A program")
    }

    /// The program runs with more rights than the user (setuid `sudo`): worth a line.
    pub fn elevated(&self) -> bool {
        self.facts.euid == 0 && self.facts.uid != 0
    }
}

/// The parent is a live Apotheosis: a pidfd on `getppid()`, its `exe` link the installed
/// binary (debug builds: also an `apotheosis` beside this binary), and still alive with
/// the same pid once the link was read. While our parent lives its pid is its own, and
/// if it dies we are reparented and `getppid()` changes.
pub fn parent_is_apotheosis() -> bool {
    use rustix::event::{poll, PollFd, PollFlags, Timespec};
    use rustix::process::{pidfd_open, Pid, PidfdFlags};
    let ppid = std::os::unix::process::parent_id();
    let Some(pid) = i32::try_from(ppid).ok().and_then(Pid::from_raw) else {
        return false;
    };
    let Ok(fd) = pidfd_open(pid, PidfdFlags::empty()) else {
        return false;
    };
    let exe = std::fs::read_link(format!("/proc/{ppid}/exe"));
    let zero = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let mut fds = [PollFd::new(&fd, PollFlags::IN)];
    let exited = matches!(poll(&mut fds, Some(&zero)), Ok(n) if n > 0);
    !exited && std::os::unix::process::parent_id() == ppid && exe.is_ok_and(|e| expected_parent(&e))
}

/// `exe` names the installed Apotheosis (or, in debug builds, the one beside this binary).
/// An upgraded-while-running binary reads as `<path> (deleted)`, which still counts: only
/// root can have put a file at the installed path.
fn expected_parent(exe: &Path) -> bool {
    let s = exe.to_string_lossy();
    let s = s.strip_suffix(" (deleted)").unwrap_or(&s);
    if s == APOTHEOSIS {
        return true;
    }
    #[cfg(debug_assertions)]
    if let Some(sib) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("apotheosis")))
    {
        return Path::new(s) == sib;
    }
    false
}

/// Only one answer ever leaves the process.
static REPLIED: AtomicBool = AtomicBool::new(false);

/// Write the secret to stdout for Apotheosis, then wipe it. `Err` is a sentence for the
/// window; it never contains the secret.
pub fn answer(secret: String) -> Result<(), String> {
    if REPLIED.swap(true, Ordering::AcqRel) {
        wipe(secret);
        return Err("Already answered.".to_owned());
    }
    if secret.len() > MAX_SECRET {
        wipe(secret);
        REPLIED.store(false, Ordering::Release);
        return Err("That is too long.".to_owned());
    }
    let mut out = Vec::with_capacity(1 + MAX_SECRET);
    out.push(ANSWERED);
    out.extend_from_slice(secret.as_bytes());
    wipe(secret);
    let mut stdout = std::io::stdout().lock();
    let sent = stdout.write_all(&out).and_then(|()| stdout.flush());
    out.resize(out.capacity(), 0);
    out.fill(0);
    std::hint::black_box(&mut out);
    sent.map_err(|_| "The terminal is gone.".to_owned())
}

/// Tell Apotheosis the human said no (EOF alone means the same).
pub fn cancel() {
    if REPLIED.swap(true, Ordering::AcqRel) {
        return;
    }
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(&[CANCELLED]).and_then(|()| stdout.flush());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same bytes as `apoth_proto::secret_prompt`'s golden test.
    const GOLDEN: &str = r#"{"v":1,"via":"askpass","verified":{"window":"apotheosis","tab":"zsh","pane":"sudo","pane_id":3,"in_foreground":true,"ancestry":[40,30,20],"pid":40,"exe":null,"uid":1000,"euid":0},"unverified":{"argv":["sudo","-A","pacman"],"cwd":"/home/u","prompt":"[sudo] password for u: ","host":null}}"#;

    #[test]
    fn parses_the_golden_request() {
        let t = TerminalAsk::parse(GOLDEN.as_bytes(), true).unwrap();
        assert_eq!(t.via, Via::Askpass);
        assert!(t.verified && t.facts.in_foreground && t.elevated());
        assert_eq!(t.facts.ancestry, vec![40, 30, 20]);
        assert_eq!(t.claims.prompt, "[sudo] password for u: ");
        // No verified exe (setuid): the name is the program's own, unverified.
        assert_eq!(t.program(), "sudo");
        let d = format!("{t:?}");
        assert!(!d.contains("pacman") && !d.contains("/home/u"), "{d}");
    }

    #[test]
    fn verified_comes_from_the_check_not_the_request() {
        let t = TerminalAsk::parse(GOLDEN.as_bytes(), false).unwrap();
        assert!(!t.verified);
        let mut with_exe = t.clone();
        with_exe.facts.exe = Some("/usr/bin/ssh".to_owned());
        assert_eq!(with_exe.program(), "sudo", "an unverified exe is not used");
        with_exe.verified = true;
        assert_eq!(with_exe.program(), "ssh");
    }

    #[test]
    fn refuses_malformed_other_versions_and_oversize() {
        assert!(TerminalAsk::parse(b"{", true).is_err());
        assert!(TerminalAsk::parse(GOLDEN.replace("\"v\":1", "\"v\":2").as_bytes(), true).is_err());
        assert!(TerminalAsk::parse(GOLDEN.replace("\"tab\"", "\"x\":1,\"tab\"").as_bytes(), true).is_err());
        assert!(TerminalAsk::parse(&vec![b' '; MAX_CONTEXT + 1], true).is_err());
    }

    #[test]
    fn the_parent_check_wants_apotheosis() {
        assert!(expected_parent(Path::new("/usr/bin/apotheosis")));
        assert!(expected_parent(Path::new("/usr/bin/apotheosis (deleted)")));
        assert!(!expected_parent(Path::new("/tmp/apotheosis")));
        assert!(!expected_parent(Path::new("/usr/bin/zsh")));
        // The test runner's parent is cargo, not Apotheosis.
        assert!(!parent_is_apotheosis());
    }
}
