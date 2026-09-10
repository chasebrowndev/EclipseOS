// SPDX-License-Identifier: AGPL-3.0-only
//! The control-socket authorisation gate (COMP-13 §2).
//!
//! Every request crosses this before any state is read or written. There is
//! no ambient authority here: a method that is not in [`TABLE`] is denied,
//! and a peer that is not the session owner is denied. The decision is a
//! ratchet — [`Decision::tighten`] can turn `Allow` into `Deny` and never
//! the other way round — so adding a rule can only ever remove access.

use crate::config::Config;

/// What a method is allowed to touch. Ordering is not significance: each
/// class carries its own rule, applied in [`check`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Reads compositor state. No mutation.
    Query,
    /// Mutates window/workspace/output state the human already controls
    /// with a keybind.
    Command,
    /// Mirrors the emergency panel (agent lifecycle, grant revocation).
    /// COMP-13 §2.1 requires the socket's owner uid explicitly.
    Privileged,
    /// Synthesises human input (COMP-13 §2.2). Off unless the config says on.
    ScriptedInput,
}

/// One row of the method table. A method with no row does not exist.
#[derive(Debug, Clone, Copy)]
pub struct Entry {
    pub method: &'static str,
    pub kind: Kind,
    /// False while the underlying subsystem has not landed yet. The gate
    /// still runs; dispatch answers with "not implemented" rather than
    /// pretending the call succeeded.
    pub implemented: bool,
}

const fn e(method: &'static str, kind: Kind, implemented: bool) -> Entry {
    Entry {
        method,
        kind,
        implemented,
    }
}

/// The complete set of methods the control socket answers (COMP-13 §2.1).
pub const TABLE: &[Entry] = &[
    // Queries.
    e("get_workspaces", Kind::Query, true),
    e("get_outputs", Kind::Query, true),
    e("get_windows", Kind::Query, true),
    e("get_focused", Kind::Query, true),
    e("get_metrics", Kind::Query, true),
    e("dump_state", Kind::Query, true),
    e("subscribe", Kind::Query, true),
    e("unsubscribe", Kind::Query, true),
    // Commands.
    e("focus_window", Kind::Command, true),
    e("close_window", Kind::Command, true),
    e("move_to_workspace", Kind::Command, true),
    e("set_floating", Kind::Command, true),
    e("switch_workspace", Kind::Command, true),
    e("reload_config", Kind::Command, true),
    e("resize", Kind::Command, true),
    e("move_workspace_to_output", Kind::Command, true),
    e("set_output", Kind::Command, true),
    // Agent lifecycle: the protocol itself is Phase 2 (COMP-08).
    e("get_agents", Kind::Privileged, false),
    e("pause_agent", Kind::Privileged, false),
    e("resume_agent", Kind::Privileged, false),
    e("terminate_agent", Kind::Privileged, false),
    e("revoke_grants", Kind::Privileged, false),
    // Scripted input: the gate is real, the injection path is not (COMP-04).
    e("type_text", Kind::ScriptedInput, false),
    e("click_at", Kind::ScriptedInput, false),
];

pub fn lookup(method: &str) -> Option<&'static Entry> {
    TABLE.iter().find(|e| e.method == method)
}

/// The connecting process, as the kernel reports it. Never trusted beyond
/// what `SO_PEERCRED` guarantees; `comm` is for the journal only and is
/// never an authorisation input.
#[derive(Debug, Clone)]
pub struct Peer {
    pub uid: u32,
    pub pid: i32,
    pub comm: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny(&'static str),
}

impl Decision {
    /// Ratchet: a later rule may only take access away.
    pub fn tighten(&mut self, deny_if: bool, why: &'static str) {
        if deny_if && matches!(self, Decision::Allow) {
            *self = Decision::Deny(why);
        }
    }
}

/// Decide whether `peer` may call `method`. Fail-closed in every direction:
/// an unknown method, a peer that is not the session owner, or a
/// scripted-input call with the toggle off are all denials, and nothing has
/// been read or written by the time this returns.
pub fn check(peer: &Peer, owner_uid: u32, config: &Config, method: &str) -> Decision {
    // No table row = the method does not exist = deny. This is the default,
    // not a fallback: the `else` branch below can never reach `Allow`.
    let Some(entry) = lookup(method) else {
        return Decision::Deny("unknown method");
    };

    let mut d = Decision::Allow;
    // Owner-only channel. The listener also refuses a foreign uid at accept
    // time; this is the second of the two checks on purpose.
    d.tighten(peer.uid != owner_uid, "peer uid is not the session owner");
    d.tighten(peer.pid <= 0, "peer credentials unavailable");
    d.tighten(
        entry.kind == Kind::ScriptedInput && !config.misc.scripted_input,
        "scripted input is disabled (misc { scripted-input })",
    );
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner() -> Peer {
        Peer {
            uid: 1000,
            pid: 42,
            comm: Some("eclipse-ctl".into()),
        }
    }

    #[test]
    fn owner_may_query_and_command() {
        let cfg = Config::default();
        assert_eq!(check(&owner(), 1000, &cfg, "get_workspaces"), Decision::Allow);
        assert_eq!(check(&owner(), 1000, &cfg, "focus_window"), Decision::Allow);
        assert_eq!(check(&owner(), 1000, &cfg, "get_agents"), Decision::Allow);
    }

    #[test]
    fn unknown_method_is_denied() {
        let cfg = Config::default();
        assert!(matches!(
            check(&owner(), 1000, &cfg, "get_tree"),
            Decision::Deny(_)
        ));
        assert!(matches!(check(&owner(), 1000, &cfg, ""), Decision::Deny(_)));
        // Not a prefix match, not a case-insensitive match.
        assert!(matches!(
            check(&owner(), 1000, &cfg, "GET_WORKSPACES"),
            Decision::Deny(_)
        ));
        assert!(matches!(
            check(&owner(), 1000, &cfg, "get_workspaces2"),
            Decision::Deny(_)
        ));
    }

    #[test]
    fn foreign_uid_is_denied_every_method() {
        let cfg = Config::default();
        let other = Peer { uid: 1001, ..owner() };
        for entry in TABLE {
            assert!(
                matches!(check(&other, 1000, &cfg, entry.method), Decision::Deny(_)),
                "{} allowed for a foreign uid",
                entry.method
            );
        }
    }

    #[test]
    fn missing_credentials_are_denied() {
        let cfg = Config::default();
        let anon = Peer { pid: 0, ..owner() };
        assert!(matches!(
            check(&anon, 1000, &cfg, "get_workspaces"),
            Decision::Deny(_)
        ));
    }

    #[test]
    fn scripted_input_follows_the_config_toggle() {
        let mut cfg = Config::default();
        assert!(!cfg.misc.scripted_input, "default must be off");
        assert!(matches!(
            check(&owner(), 1000, &cfg, "type_text"),
            Decision::Deny(_)
        ));
        cfg.misc.scripted_input = true;
        assert_eq!(check(&owner(), 1000, &cfg, "type_text"), Decision::Allow);
        // ... but the toggle never rescues a foreign uid.
        let other = Peer { uid: 1001, ..owner() };
        assert!(matches!(
            check(&other, 1000, &cfg, "type_text"),
            Decision::Deny(_)
        ));
    }

    #[test]
    fn tighten_never_grants() {
        let mut d = Decision::Deny("first");
        d.tighten(true, "second");
        assert_eq!(d, Decision::Deny("first"));
        d.tighten(false, "third");
        assert_eq!(d, Decision::Deny("first"));
        let mut a = Decision::Allow;
        a.tighten(false, "no");
        assert_eq!(a, Decision::Allow);
    }

    #[test]
    fn table_has_no_duplicate_methods() {
        for (i, a) in TABLE.iter().enumerate() {
            assert!(
                !TABLE[..i].iter().any(|b| b.method == a.method),
                "duplicate row for {}",
                a.method
            );
        }
    }

    #[test]
    fn phase_one_commands_are_implemented() {
        for m in ["resize", "move_workspace_to_output", "set_output"] {
            let row = TABLE.iter().find(|e| e.method == m).expect("row exists");
            assert_eq!(row.kind, Kind::Command);
            assert!(row.implemented, "{m} should be implemented");
        }
    }

    #[test]
    fn phase_two_rows_stay_unimplemented() {
        // Agent lifecycle (COMP-08) and scripted input (COMP-04) are not
        // Phase 1; the gate must keep answering "not implemented".
        let pending: Vec<&str> = TABLE
            .iter()
            .filter(|e| !e.implemented)
            .map(|e| e.method)
            .collect();
        assert_eq!(
            pending,
            vec![
                "get_agents",
                "pause_agent",
                "resume_agent",
                "terminate_agent",
                "revoke_grants",
                "type_text",
                "click_at",
            ]
        );
    }
}
