// SPDX-License-Identifier: AGPL-3.0-only
//! Capability decisions (T-12 §4): a pure function of who the peer is, what class a request
//! needs, and which pane it targets. Fail-closed: anything not granted here is asked or
//! denied, and an agent peer never gets more than `observe` from this table.

use std::path::Path;

use apoth_action::Class;

/// The facts about a peer the table reads (taken once at connect, T-12 §1).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PeerFacts {
    /// The pane the peer descends from, when it is one of ours.
    pub(crate) pane: Option<u64>,
    /// A remote client: the peer or a process between it and the pane is a remote-session
    /// program (ssh, mosh and the like).
    pub(crate) remote: bool,
    /// Runs under `agents.slice/`.
    pub(crate) agent: bool,
    /// The executable is an interpreter (never pre-granted).
    pub(crate) interpreter: bool,
}

/// What the table says about one class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Allowed.
    Grant,
    /// Needs the user's consent on the consent bar (T-11 §1).
    Ask,
    /// Never from this peer.
    Deny(&'static str),
}

/// The configuration the table reads.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Policy<'a> {
    /// `socket.grants`: executable path to granted class names.
    pub(crate) grants: &'a [(String, Vec<String>)],
    /// `socket.key-stream` in the target pane.
    pub(crate) key_stream: bool,
}

impl Policy<'_> {
    fn pre_granted(&self, exe: Option<&Path>, class: Class) -> bool {
        let Some(exe) = exe.and_then(Path::to_str) else {
            return false;
        };
        self.grants
            .iter()
            .any(|(p, cs)| p == exe && cs.iter().any(|c| c == class.name()))
    }
}

/// The verdict for `class` from `peer` (executable `exe`) on pane `target`.
#[must_use]
pub(crate) fn decide(
    peer: &PeerFacts,
    exe: Option<&Path>,
    class: Class,
    target: Option<u64>,
    policy: &Policy<'_>,
) -> Verdict {
    if peer.agent {
        // ADR 0116: agent-origin requests outside `observe` are denied until COMP-11
        // defines the policyd query.
        return if class == Class::Observe {
            Verdict::Grant
        } else {
            Verdict::Deny("agent clients are limited to observe")
        };
    }
    let own = peer.pane.is_some() && target == peer.pane;
    let pre = !peer.interpreter && policy.pre_granted(exe, class);
    match class {
        Class::Observe | Class::Layout | Class::Appearance => Verdict::Grant,
        Class::Spawn if peer.pane.is_some() && !peer.remote => Verdict::Grant,
        Class::ReadContent | Class::Input if own => Verdict::Grant,
        Class::Keys if policy.key_stream => Verdict::Grant,
        Class::Keys => Verdict::Deny("the key stream needs socket.key-stream"),
        _ if pre => Verdict::Grant,
        _ => Verdict::Ask,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Class::*;

    const ALL: [Class; 9] = Class::ALL;

    fn local(pane: Option<u64>) -> PeerFacts {
        PeerFacts {
            pane,
            ..PeerFacts::default()
        }
    }

    fn row(peer: &PeerFacts, target: Option<u64>, policy: &Policy<'_>) -> Vec<Verdict> {
        ALL.iter()
            .map(|&c| decide(peer, Some(Path::new("/usr/bin/tool")), c, target, policy))
            .collect()
    }

    fn short(v: &[Verdict]) -> String {
        v.iter()
            .map(|v| match v {
                Verdict::Grant => 'G',
                Verdict::Ask => 'A',
                Verdict::Deny(_) => 'D',
            })
            .collect()
    }

    #[test]
    fn class_order_is_the_spec_table() {
        let names: Vec<&str> = ALL.iter().map(|c| c.name()).collect();
        assert_eq!(
            names,
            [
                "observe",
                "layout",
                "appearance",
                "spawn",
                "read-content",
                "input",
                "clipboard",
                "keys",
                "config-write"
            ]
        );
    }

    #[test]
    fn capability_matrix() {
        let none = Policy::default();
        // Columns: observe layout appearance spawn read-content input clipboard keys
        // config-write.
        // A descendant of pane 3 acting on its own pane.
        assert_eq!(short(&row(&local(Some(3)), Some(3), &none)), "GGGGGGADA");
        // ... on another pane: content and input are asked.
        assert_eq!(short(&row(&local(Some(3)), Some(4), &none)), "GGGGAAADA");
        // An outside client (not a descendant).
        assert_eq!(short(&row(&local(None), Some(4), &none)), "GGGAAAADA");
        // A remote client inside a pane: spawn is asked; its own pane is still its own.
        let remote = PeerFacts {
            pane: Some(3),
            remote: true,
            ..PeerFacts::default()
        };
        assert_eq!(short(&row(&remote, Some(3), &none)), "GGGAGGADA");
        // An agent peer: observe only, everything else denied.
        let agent = PeerFacts {
            pane: Some(3),
            agent: true,
            ..PeerFacts::default()
        };
        assert_eq!(short(&row(&agent, Some(3), &none)), "GDDDDDDDD");
    }

    #[test]
    fn grants_are_by_executable_and_never_for_interpreters_or_keys() {
        let grants = vec![(
            "/usr/bin/tool".to_owned(),
            vec!["clipboard".to_owned(), "keys".to_owned()],
        )];
        let p = Policy {
            grants: &grants,
            key_stream: false,
        };
        let peer = local(None);
        let exe = Some(Path::new("/usr/bin/tool"));
        assert_eq!(decide(&peer, exe, Clipboard, None, &p), Verdict::Grant);
        assert!(matches!(
            decide(&peer, exe, Keys, None, &p),
            Verdict::Deny(_)
        ));
        let other = Some(Path::new("/usr/bin/other"));
        assert_eq!(decide(&peer, other, Clipboard, None, &p), Verdict::Ask);
        let interp = PeerFacts {
            interpreter: true,
            ..PeerFacts::default()
        };
        assert_eq!(decide(&interp, exe, Clipboard, None, &p), Verdict::Ask);
        let ks = Policy {
            grants: &[],
            key_stream: true,
        };
        assert_eq!(decide(&peer, exe, Keys, None, &ks), Verdict::Grant);
    }
}
