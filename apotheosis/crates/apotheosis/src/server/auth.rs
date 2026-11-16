// SPDX-License-Identifier: AGPL-3.0-only
//! Who may attach to the session daemon (T-21 §3 "Attach transport and authority", ADR 0116).
//!
//! A view holds every pane's content and an input path, so `attach` needs `read-content`
//! and `input` for every pane it receives. A pure function of the peer facts taken once at
//! connect (T-12 §1), fail-closed:
//! - an agent peer is refused: the seat-policy query of T-12 §4.1 is not defined yet
//!   (COMP-11), and no reachable policy service means refuse;
//! - a peer that descends from one of our panes (including `apoth` run inside a pane) needs
//!   consent on an already-attached view's glass sheet; that sheet does not exist yet, so
//!   it is refused, which is what the spec prescribes with no view to ask on;
//! - only the GUI binary itself — the executable this server runs from, which is the path
//!   the package installs — started outside our panes is pre-granted, and the installed
//!   `apoth` (next to it) while it runs `apoth proxy` (its argv, read from `/proc` and
//!   confirmed against the pidfd): the remote end of a mux domain (T-22 §5, owner decision
//!   2026-11-14). The agent, pane-descendant and interpreter refusals come first for both.
//!
//! Lister requests (`List`) are `observe`: ids, sizes and liveness only, no content.
//! `DetachAll` changes no content and starts nothing (`layout`): any same-uid, non-agent
//! peer.

use std::path::{Path, PathBuf};

use crate::ctl::caps::PeerFacts;

/// What a peer may do with the panes of the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Grant {
    /// Snapshot, tee and input.
    Full,
    /// Snapshot and tee; input is dropped (`read-content` without `input`).
    #[allow(dead_code)] // reachable once consent can grant read-content alone
    ReadOnly,
    /// Omitted from the snapshot and tee.
    None(&'static str),
}

/// The server's own executable as `/proc/self/exe` names it, without the ` (deleted)`
/// suffix a replaced binary gets (an upgrade leaves the running server on the old inode).
pub(crate) fn own_exe() -> Option<PathBuf> {
    let p = std::fs::read_link("/proc/self/exe").ok()?;
    Some(strip_deleted(p))
}

/// The executables pre-granted `attach`: the server's own. Debug builds also take
/// `APOTH_TEST_ATTACH_EXE` from the server's environment, so the integration tests can
/// attach a protocol client (whoever sets the server's environment owns it already).
#[must_use]
pub(crate) fn pre_granted() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = own_exe().into_iter().collect();
    #[cfg(debug_assertions)]
    if let Some(p) = std::env::var_os("APOTH_TEST_ATTACH_EXE") {
        v.push(PathBuf::from(p));
    }
    v
}

pub(crate) fn strip_deleted(p: PathBuf) -> PathBuf {
    match p.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(s) => PathBuf::from(s),
        None => p,
    }
}

/// The `apoth` the package installs next to this server's binary (`apoth proxy` runs it).
pub(crate) fn proxy_exe() -> Option<PathBuf> {
    let p = own_exe()?.parent()?.join("apoth");
    p.is_file().then_some(p)
}

/// `argv` (NUL-separated, as `/proc/<pid>/cmdline`) is `apoth proxy …`.
#[must_use]
pub(crate) fn argv_is_proxy(cmdline: &[u8]) -> bool {
    let mut it = cmdline.split(|&b| b == 0);
    it.next().is_some_and(|a0| !a0.is_empty()) && it.next() == Some(b"proxy")
}

/// The peer `pid` runs `apoth proxy`: its argv says so and the pidfd shows the process
/// still alive after the read (so the pid was not recycled in between).
pub(crate) fn runs_proxy(pid: i32, pidfd: &impl std::os::fd::AsFd) -> bool {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    let Ok(cmd) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    let mut fds = [PollFd::new(pidfd, PollFlags::IN)];
    let zero = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let exited = matches!(poll(&mut fds, Some(&zero)), Ok(n) if n > 0);
    !exited && argv_is_proxy(&cmd)
}

/// The attach grant for one pane (the same rule for every pane today; it is evaluated per
/// pane, at attach and again for every pane added afterwards, T-21 §3). `proxy` is the
/// installed `apoth`; `runs_proxy` is asked only when the peer's executable is that one.
#[must_use]
pub(crate) fn attach_grant(
    peer: &PeerFacts,
    exe: Option<&Path>,
    allowed: &[PathBuf],
    proxy: Option<&Path>,
    runs_proxy: impl FnOnce() -> bool,
) -> Grant {
    if peer.agent {
        return Grant::None("agent sessions cannot attach: no policy service answers for them");
    }
    if peer.pane.is_some() {
        return Grant::None(
            "a program running inside a pane needs consent on an attached view to attach",
        );
    }
    if peer.interpreter {
        return Grant::None("interpreters are never pre-granted");
    }
    match exe.map(|e| strip_deleted(e.to_path_buf())) {
        Some(e) if allowed.contains(&e) => Grant::Full,
        Some(e) if proxy == Some(e.as_path()) && runs_proxy() => Grant::Full,
        _ => Grant::None("only the Apotheosis interface or apoth proxy may attach without consent"),
    }
}

/// `DetachAll` (`layout` class).
#[must_use]
pub(crate) fn may_detach_all(peer: &PeerFacts) -> bool {
    !peer.agent
}

/// `KillSession` (`layout` class, like `pane.close` for every pane, T-12 §4): any same-uid,
/// non-agent peer, as for `DetachAll` — the tmux model (`kill-session` from any client).
#[must_use]
pub(crate) fn may_kill_session(peer: &PeerFacts) -> bool {
    !peer.agent
}

/// `Spawn` from an attached view: the pre-granted interface is the user's own terminal
/// opening a tab (class `spawn`, T-12 §4); nothing else reaches it.
#[must_use]
pub(crate) fn may_spawn(grant: Grant) -> bool {
    grant == Grant::Full
}

#[cfg(test)]
mod tests {
    use super::*;

    const GUI: &str = "/usr/bin/apotheosis";

    fn facts(pane: Option<u64>, agent: bool, interpreter: bool) -> PeerFacts {
        PeerFacts {
            pane,
            remote: false,
            agent,
            interpreter,
        }
    }

    const APOTH: &str = "/usr/bin/apoth";

    fn grant(f: &PeerFacts, exe: &str) -> Grant {
        attach_grant(f, Some(Path::new(exe)), &[PathBuf::from(GUI)], None, || {
            false
        })
    }

    fn proxy(f: &PeerFacts, exe: &str, runs: bool) -> Grant {
        attach_grant(
            f,
            Some(Path::new(exe)),
            &[PathBuf::from(GUI)],
            Some(Path::new(APOTH)),
            || runs,
        )
    }

    #[test]
    fn apoth_is_pre_granted_only_while_it_runs_proxy() {
        let outside = facts(None, false, false);
        assert_eq!(proxy(&outside, APOTH, true), Grant::Full);
        // `apoth` doing anything else, or another binary claiming to be a proxy.
        assert!(matches!(proxy(&outside, APOTH, false), Grant::None(_)));
        assert!(matches!(
            proxy(&outside, "/tmp/apoth", true),
            Grant::None(_)
        ));
        // The refusals that come first: inside a server pane, agents, interpreters.
        assert!(matches!(
            proxy(&facts(Some(2), false, false), APOTH, true),
            Grant::None(_)
        ));
        assert!(matches!(
            proxy(&facts(None, true, false), APOTH, true),
            Grant::None(_)
        ));
        assert!(matches!(
            proxy(&facts(None, false, true), APOTH, true),
            Grant::None(_)
        ));
        assert!(argv_is_proxy(b"/usr/bin/apoth\0proxy\0--session\0t\0"));
        assert!(argv_is_proxy(b"apoth\0proxy"));
        assert!(!argv_is_proxy(b"/usr/bin/apoth\0ssh\0h\0"));
        assert!(!argv_is_proxy(b"/usr/bin/apoth\0proxyx\0"));
        assert!(!argv_is_proxy(b"\0proxy\0"));
        assert!(!argv_is_proxy(b""));
    }

    #[test]
    fn only_the_gui_binary_outside_panes_is_pre_granted() {
        assert_eq!(grant(&facts(None, false, false), GUI), Grant::Full);
        // `apoth` (or anything else) from outside the session asks: refused with no view.
        assert!(matches!(
            grant(&facts(None, false, false), "/usr/bin/apoth"),
            Grant::None(_)
        ));
        // The GUI binary started from inside one of our panes asks too.
        assert!(matches!(
            grant(&facts(Some(3), false, false), GUI),
            Grant::None(_)
        ));
        // `apoth` from a pane.
        assert!(matches!(
            grant(&facts(Some(3), false, false), "/usr/bin/apoth"),
            Grant::None(_)
        ));
        // An agent is refused whatever it runs.
        assert!(matches!(
            grant(&facts(None, true, false), GUI),
            Grant::None(_)
        ));
        // Unknown executables on either side refuse.
        assert!(matches!(
            attach_grant(
                &facts(None, false, false),
                None,
                &[PathBuf::from(GUI)],
                None,
                || true
            ),
            Grant::None(_)
        ));
        assert!(matches!(
            attach_grant(
                &facts(None, false, false),
                Some(Path::new(GUI)),
                &[],
                None,
                || true
            ),
            Grant::None(_)
        ));
        assert!(matches!(
            grant(&facts(None, false, true), GUI),
            Grant::None(_)
        ));
    }

    #[test]
    fn listing_detach_and_spawn_rules() {
        assert!(may_detach_all(&facts(Some(1), false, false)));
        assert!(!may_detach_all(&facts(None, true, false)));
        assert!(may_kill_session(&facts(Some(1), false, false)));
        assert!(may_kill_session(&facts(None, false, true)));
        assert!(!may_kill_session(&facts(None, true, false)));
        assert!(may_spawn(Grant::Full));
        assert!(!may_spawn(Grant::ReadOnly));
        assert!(!may_spawn(Grant::None("x")));
    }

    #[test]
    fn a_replaced_binary_keeps_its_path() {
        assert_eq!(
            strip_deleted(PathBuf::from("/usr/bin/apotheosis (deleted)")),
            PathBuf::from(GUI)
        );
        assert_eq!(strip_deleted(PathBuf::from(GUI)), PathBuf::from(GUI));
    }
}
