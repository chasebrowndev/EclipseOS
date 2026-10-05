// SPDX-License-Identifier: AGPL-3.0-only
//! The control-socket authorisation gate (COMP-13 §2).
//!
//! Every request crosses this before any state is read or written. There is
//! no ambient authority here: a method that is not in [`TABLE`] is denied,
//! and a peer that is not the session owner is denied. The decision is a
//! ratchet — [`Decision::tighten`] can turn `Allow` into `Deny` and never
//! the other way round — so adding a rule can only ever remove access.

use crate::hooks::{Hook, HookSet};
use ec_abyss_config::Config;

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
    e("set_minimized", Kind::Command, true),
    // Where the taskbar drew a window's chip, so minimize can aim at it
    // (ADR 0073). Render-only: it moves nothing and grants nothing.
    e("set_window_chip_rect", Kind::Command, true),
    e("switch_workspace", Kind::Command, true),
    e("reload_config", Kind::Command, true),
    e("resize", Kind::Command, true),
    e("move_workspace_to_output", Kind::Command, true),
    e("set_output", Kind::Command, true),
    e("calibrate_output", Kind::Command, true),
    // Annotation overlays (COMP-18 §3). Command and not Privileged: the pass
    // is untrusted by construction, carries no phrase, and a caller can affect
    // nothing but the glyphs inside a panel the compositor places.
    e("annotation_create", Kind::Command, true),
    e("annotation_update", Kind::Command, true),
    e("annotation_destroy", Kind::Command, true),
    e("annotation_clear", Kind::Command, true),
    // Idle inhibit on behalf of a D-Bus client (ADR 0051). Command and not
    // Privileged: it can only keep the screen on, which any client can already
    // do with `zwp_idle_inhibit_v1`, and it lapses with the connection.
    e("set_idle_inhibit", Kind::Command, true),
    // Open the bar's start menu (`launcher.style "menu"`). Command and not
    // Privileged: it is the eclipse button / Super+R, a thing the human already
    // does, and it only rebroadcasts on the `launcher` event stream; the
    // compositor holds no launcher state and the bar decides what opens.
    e("open_launcher", Kind::Command, true),
    // Config read/write (COMP-13 §1.4). `set_config_value` is Command and not
    // Privileged on purpose: it edits the same keys a human edits in a text
    // editor, and the file it may touch is decided by `CONFIG_FILES`, not by
    // the method's kind. Making it Privileged would suggest the kind is what
    // protects `policy.kdl`; it is not.
    e("get_config", Kind::Query, true),
    e("validate_config", Kind::Query, true),
    e("set_config_value", Kind::Command, true),
    e("set_config_values", Kind::Command, true),
    // Collection entries (`bar { widget … }`, ADR 0065): the same authority as
    // `set_config_value`, by the same reasoning, and the same per-file check.
    e("set_config_collection", Kind::Command, true),
    // Re-queue a withheld command widget's approval prompt (ADR 0067). Command:
    // it carries no answer and can only show the owner a prompt again.
    e("review_widget", Kind::Command, true),
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

/// Which config file an operation names (COMP-13 §1.3). Not a path: the
/// gate decides about *roles*, and the search path decides about paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFile {
    Abyss,
    Policy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

/// One (file, access) permission, as data, for the same reason [`TABLE`] is
/// data: a reviewer reads four lines instead of tracing branches.
#[derive(Debug, Clone, Copy)]
pub struct FileEntry {
    pub file: ConfigFile,
    pub access: Access,
    pub allowed: bool,
}

const fn f(file: ConfigFile, access: Access, allowed: bool) -> FileEntry {
    FileEntry {
        file,
        access,
        allowed,
    }
}

/// The control socket's authority over the config files.
///
/// `policy.kdl` is closed in **both** directions and is not config-toggleable:
/// a socket peer cannot read the security surface and cannot write it. Read is
/// closed too because the policy viewer reads the file from disk under the
/// human's own uid — it needs no socket capability, so granting one would be
/// authority nothing asked for.
pub const CONFIG_FILES: &[FileEntry] = &[
    f(ConfigFile::Abyss, Access::Read, true),
    f(ConfigFile::Abyss, Access::Write, true),
    f(ConfigFile::Policy, Access::Read, false),
    f(ConfigFile::Policy, Access::Write, false),
];

/// Second gate, inside the handler, before any file is opened or any `Config`
/// field is read. Tightens onto the outer [`check`] result (the ratchet), so
/// this can only ever remove access the method table already granted.
pub fn check_config_file(outer: Decision, file: ConfigFile, access: Access) -> Decision {
    let mut d = outer;
    let allowed = CONFIG_FILES
        .iter()
        .find(|e| e.file == file && e.access == access)
        .is_some_and(|e| e.allowed);
    d.tighten(
        !allowed,
        "the control socket has no such authority over that config file",
    );
    d
}

/// A method (or one collection of it) that only exists while an add-on hook
/// is on (ADR 0066). In addition to [`TABLE`], never instead of it: the row
/// there still decides the gate kind and the uid.
#[derive(Debug, Clone, Copy)]
pub struct HookBinding {
    pub method: &'static str,
    /// For `set_config_collection`: which `collection` param the binding is
    /// about. `None` binds every call of the method.
    pub collection: Option<&'static str>,
    pub hook: Hook,
}

const fn hb(method: &'static str, collection: Option<&'static str>, hook: Hook) -> HookBinding {
    HookBinding {
        method,
        collection,
        hook,
    }
}

/// Every hook-bound method (ADR 0066 table). Reads stay open: `get_config`
/// is not here, and serves an empty `collections.widget` while
/// `taskbar-widgets` is off because the blocks are kept out of the live config.
pub const HOOKED: &[HookBinding] = &[
    hb("annotation_create", None, Hook::Annotations),
    hb("annotation_update", None, Hook::Annotations),
    hb("annotation_destroy", None, Hook::Annotations),
    hb("annotation_clear", None, Hook::Annotations),
    // Every `widget` write (upsert, remove, rename, move) is this one method.
    hb("set_config_collection", Some("widget"), Hook::TaskbarWidgets),
    // Command approval exists only with the taskbar add-on (ADR 0067).
    hb("review_widget", None, Hook::TaskbarWidgets),
    // Agent lifecycle exists only with the agent add-on (ADR 0069).
    hb("get_agents", None, Hook::Agents),
    hb("pause_agent", None, Hook::Agents),
    hb("resume_agent", None, Hook::Agents),
    hb("terminate_agent", None, Hook::Agents),
    hb("revoke_grants", None, Hook::Agents),
];

/// Hook check, tightened onto the outer [`check`] (the ratchet). A binding
/// with a `collection` applies unless the request names a *different*
/// collection as a string: a missing or malformed `collection` is treated as
/// the bound one, so a bad request can never slip past the hook.
pub fn check_hook(outer: Decision, hooks: HookSet, method: &str, params: &serde_json::Value) -> Decision {
    let mut d = outer;
    let named = params.get("collection").and_then(serde_json::Value::as_str);
    for b in HOOKED.iter().filter(|b| b.method == method) {
        let applies = match (b.collection, named) {
            (Some(c), Some(n)) => c == n,
            _ => true,
        };
        d.tighten(applies && !hooks.is_on(b.hook), b.hook.off_reason());
    }
    d
}

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
            comm: Some("ec-ctl".into()),
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

    /// COMP-18 §3: the four annotation methods exist, are Commands, and are
    /// closed to anyone but the owner. Anything adjacent that is not in the
    /// table does not exist -- the pass has no other door.
    #[test]
    fn annotation_methods_are_owner_only_commands() {
        let cfg = Config::default();
        let other = Peer { uid: 1001, ..owner() };
        for method in [
            "annotation_create",
            "annotation_update",
            "annotation_destroy",
            "annotation_clear",
        ] {
            let entry = TABLE.iter().find(|e| e.method == method).expect(method);
            assert_eq!(entry.kind, Kind::Command, "{method}");
            assert!(entry.implemented, "{method}");
            assert_eq!(check(&owner(), 1000, &cfg, method), Decision::Allow);
            assert!(matches!(check(&other, 1000, &cfg, method), Decision::Deny(_)));
        }
        for absent in ["annotation_list", "annotation_read", "annotation_get"] {
            assert!(
                matches!(check(&owner(), 1000, &cfg, absent), Decision::Deny(_)),
                "{absent} must not exist"
            );
        }
    }

    #[test]
    fn set_idle_inhibit_is_an_owner_only_command() {
        let cfg = Config::default();
        let other = Peer { uid: 1001, ..owner() };
        let entry = TABLE
            .iter()
            .find(|e| e.method == "set_idle_inhibit")
            .expect("row");
        assert_eq!(entry.kind, Kind::Command);
        assert!(entry.implemented);
        assert_eq!(check(&owner(), 1000, &cfg, "set_idle_inhibit"), Decision::Allow);
        assert!(matches!(
            check(&other, 1000, &cfg, "set_idle_inhibit"),
            Decision::Deny(_)
        ));
    }

    #[test]
    fn open_launcher_is_an_owner_only_command() {
        let cfg = Config::default();
        let other = Peer { uid: 1001, ..owner() };
        let entry = TABLE.iter().find(|e| e.method == "open_launcher").expect("row");
        assert_eq!(entry.kind, Kind::Command);
        assert!(entry.implemented);
        assert_eq!(check(&owner(), 1000, &cfg, "open_launcher"), Decision::Allow);
        assert!(matches!(
            check(&other, 1000, &cfg, "open_launcher"),
            Decision::Deny(_)
        ));
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

    /// COMP-13 §1.3: `policy.kdl` is closed both ways, unconditionally. This
    /// is a constant assertion, not a behaviour test — if someone adds a
    /// config toggle for it, this fails.
    #[test]
    fn policy_is_never_reachable_over_the_socket() {
        for e in CONFIG_FILES {
            if e.file == ConfigFile::Policy {
                assert!(!e.allowed, "{:?} {:?} is open", e.file, e.access);
            }
        }
        for access in [Access::Read, Access::Write] {
            assert!(matches!(
                check_config_file(Decision::Allow, ConfigFile::Policy, access),
                Decision::Deny(_)
            ));
        }
    }

    /// The inner check is a ratchet: it never turns a denial into an allow.
    #[test]
    fn the_file_check_only_tightens() {
        let denied = Decision::Deny("peer uid is not the session owner");
        assert_eq!(
            check_config_file(denied, ConfigFile::Abyss, Access::Write),
            denied
        );
        assert_eq!(
            check_config_file(Decision::Allow, ConfigFile::Abyss, Access::Write),
            Decision::Allow
        );
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

    fn hooked_params(b: &HookBinding) -> serde_json::Value {
        match b.collection {
            Some(c) => serde_json::json!({ "collection": c, "op": "remove", "name": "w" }),
            None => serde_json::Value::Null,
        }
    }

    /// ADR 0066: every hook-bound method is refused with its hook off, naming
    /// the hook, and passes the hook check with it on.
    #[test]
    fn hook_bound_methods_follow_their_hook() {
        let cfg = Config::default();
        for b in HOOKED {
            assert!(lookup(b.method).is_some(), "{} has no gate row", b.method);
            let params = hooked_params(b);
            let outer = check(&owner(), 1000, &cfg, b.method);
            assert_eq!(outer, Decision::Allow, "{}", b.method);
            let off = check_hook(outer, HookSet::default(), b.method, &params);
            let Decision::Deny(why) = off else {
                panic!("{} allowed with `{}` off", b.method, b.hook.name())
            };
            assert_eq!(why, format!("add-on hook `{}` is off", b.hook.name()));
            // Every other hook on is not enough.
            let mut others = HookSet::default();
            for h in Hook::ALL.into_iter().filter(|h| *h != b.hook) {
                others.insert(h);
            }
            assert!(matches!(
                check_hook(outer, others, b.method, &params),
                Decision::Deny(_)
            ));
            let mut on = HookSet::default();
            on.insert(b.hook);
            assert_eq!(
                check_hook(outer, on, b.method, &params),
                Decision::Allow,
                "{}",
                b.method
            );
            // The hook never rescues a denial the table made.
            let other = Peer { uid: 1001, ..owner() };
            let foreign = check(&other, 1000, &cfg, b.method);
            assert!(matches!(
                check_hook(foreign, on, b.method, &params),
                Decision::Deny(_)
            ));
        }
    }

    /// A new `annotation_*` row cannot land without its hook binding, and a
    /// `widget` write with a missing or malformed collection is still bound.
    #[test]
    fn hook_bindings_cover_the_surface() {
        for e in TABLE.iter().filter(|e| e.method.starts_with("annotation_")) {
            assert!(
                HOOKED
                    .iter()
                    .any(|b| b.method == e.method && b.hook == Hook::Annotations),
                "{} is not bound to `annotations`",
                e.method
            );
        }
        // Every agent-lifecycle (Privileged) row needs the agent add-on.
        for e in TABLE.iter().filter(|e| e.kind == Kind::Privileged) {
            assert!(
                HOOKED
                    .iter()
                    .any(|b| b.method == e.method && b.hook == Hook::Agents),
                "{} is not bound to `agents`",
                e.method
            );
        }
        for params in [
            serde_json::Value::Null,
            serde_json::json!({}),
            serde_json::json!({"collection": 7}),
        ] {
            assert!(matches!(
                check_hook(
                    Decision::Allow,
                    HookSet::default(),
                    "set_config_collection",
                    &params
                ),
                Decision::Deny(_)
            ));
        }
        // Reads and unbound methods pass the hook check untouched.
        for m in ["get_config", "set_config_value", "get_windows"] {
            assert_eq!(
                check_hook(Decision::Allow, HookSet::default(), m, &serde_json::Value::Null),
                Decision::Allow
            );
        }
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
