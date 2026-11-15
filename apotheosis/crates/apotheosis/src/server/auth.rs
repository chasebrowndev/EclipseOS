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
//!   the package installs — started outside our panes is pre-granted.
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

/// The attach grant for one pane (the same rule for every pane today; it is evaluated per
/// pane, at attach and again for every pane added afterwards, T-21 §3).
#[must_use]
pub(crate) fn attach_grant(peer: &PeerFacts, exe: Option<&Path>, allowed: &[PathBuf]) -> Grant {
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
        _ => Grant::None("only the Apotheosis interface may attach without consent"),
    }
}

/// `DetachAll` (`layout` class).
#[must_use]
pub(crate) fn may_detach_all(peer: &PeerFacts) -> bool {
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

    fn grant(f: &PeerFacts, exe: &str) -> Grant {
        attach_grant(f, Some(Path::new(exe)), &[PathBuf::from(GUI)])
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
            attach_grant(&facts(None, false, false), None, &[PathBuf::from(GUI)]),
            Grant::None(_)
        ));
        assert!(matches!(
            attach_grant(&facts(None, false, false), Some(Path::new(GUI)), &[]),
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
