// SPDX-License-Identifier: AGPL-3.0-only
//! Who is on the other end of a `policyd.sock` connection (F-05, A-08 §1).
//!
//! The socket is `0600`, so every peer is the session uid. That is not enough:
//! a task statement renders as trusted compositor text in every prompt for
//! that task (A-04 §11), so a same-uid script that could send `create_task`
//! would hold exactly the authority A-08 §1 withholds from everything but the
//! commit slot. Each connection is therefore given a role once, at accept, from
//! what the kernel recorded about the peer, and a message outside that role
//! drops the connection.
//!
//! The role is the peer's executable, read through `/proc/<pid>/exe` for the
//! `SO_PEERCRED` pid: a binary in `/usr/bin`, which the session user cannot
//! write. That is enough because the role buys nothing the binary does not
//! itself enforce. A same-uid process that runs the real `/usr/bin/ec-abyss`
//! gets a real compositor, one that creates tasks only through its own commit
//! slot on physical input (COMP-19 §3), so it gains nothing. A modified copy
//! lives outside `/usr/bin` and is refused.
//!
//! ```text
//!   /usr/bin/ec-abyss    → Compositor   table, prompts, tasks, slots
//!   /usr/bin/ec-agentd   → Agentd       pause / cancel, channel audit
//!   /usr/bin/ec-brokerd  → Brokerd      secret audit only
//!   anything else, or any peer under agents.slice → refused at accept
//! ```
//!
//! Systemd units would be the F-05 answer, but the compositor cannot be one:
//! it must stay in its logind session scope to hold the seat. Agent-launched
//! processes are refused by cgroup whatever their binary (A-08 §7's rule for
//! the console socket, applied here too).
//!
//! The `dev-peers` feature accepts the same basenames from any directory, for
//! `cargo run` and the headless end-to-end tests. It is never enabled by the
//! package build (`packaging/pkg/eclipseos/PKGBUILD`).

use ec_policy_eval::audit::Kind;
use ec_policy_eval::link::ToPolicyd;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Compositor,
    Agentd,
    Brokerd,
}

/// Where a role's binary must live outside `dev-peers` builds.
const BIN_DIR: &str = "/usr/bin";

fn by_name(name: &str) -> Option<Role> {
    match name {
        "ec-abyss" => Some(Role::Compositor),
        "ec-agentd" => Some(Role::Agentd),
        "ec-brokerd" => Some(Role::Brokerd),
        _ => None,
    }
}

/// The role for a peer's resolved executable path. A `(deleted)` suffix (the
/// binary was replaced while it ran) is not the installed binary.
pub fn from_exe(exe: &Path, any_dir: bool) -> Option<Role> {
    let dir_ok = any_dir || exe.parent() == Some(Path::new(BIN_DIR));
    if !dir_ok {
        return None;
    }
    by_name(exe.file_name()?.to_str()?)
}

/// Whether `/proc/<pid>/cgroup` (cgroup v2) places the process under
/// `agents.slice`. An unreadable or unexpected file counts as yes.
pub fn under_agents(cgroup: &str) -> bool {
    let mut lines = cgroup.lines();
    let (Some(line), None) = (lines.next(), lines.next()) else {
        return true;
    };
    let Some(path) = line.strip_prefix("0::") else {
        return true;
    };
    path.split('/').any(|c| c == "agents.slice")
}

/// Classifies a connected peer, or `None` to refuse it.
pub fn classify(pid: i32) -> Option<Role> {
    let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap_or_default();
    if under_agents(&cgroup) {
        return None;
    }
    let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    from_exe(&exe, cfg!(feature = "dev-peers"))
}

impl Role {
    /// Whether this role may store an emission of `kind` (COMP-12 §2,
    /// S-04 §1): each source emits only what it witnesses.
    pub fn may_emit(self, kind: Kind) -> bool {
        match self {
            Role::Compositor => kind.from_compositor(),
            Role::Agentd => matches!(kind, Kind::Channel | Kind::Sandbox),
            Role::Brokerd => kind == Kind::Secret,
        }
    }

    /// Whether this role may send `m`.
    pub fn may_send(self, m: &ToPolicyd) -> bool {
        // Everything the link carries today is the compositor's, which is why
        // a message this match does not name is refused for the other roles
        // rather than allowed.
        matches!((self, m), (Role::Compositor, _))
    }

    /// Whether this role is pushed the signed table and the key. Only the
    /// enforcement point needs either.
    pub fn takes_table(self) -> bool {
        self == Role::Compositor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_come_from_installed_binaries_only() {
        let at = |p: &str| from_exe(Path::new(p), false);
        assert_eq!(at("/usr/bin/ec-abyss"), Some(Role::Compositor));
        assert_eq!(at("/usr/bin/ec-agentd"), Some(Role::Agentd));
        assert_eq!(at("/usr/bin/ec-brokerd"), Some(Role::Brokerd));
        assert_eq!(at("/home/u/ec-abyss"), None, "a copy outside /usr/bin");
        assert_eq!(at("/usr/bin/ec-abyss (deleted)"), None, "replaced while running");
        assert_eq!(at("/usr/bin/python3"), None);
        assert_eq!(at("/usr/bin/sub/ec-abyss"), None);
        assert_eq!(
            from_exe(Path::new("/home/u/EclipseOS/target/debug/ec-abyss"), true),
            Some(Role::Compositor),
            "dev-peers"
        );
    }

    #[test]
    fn agent_launched_peers_are_refused_by_cgroup() {
        assert!(under_agents(
            "0::/user.slice/user-1000.slice/user@1000.service/agents.slice/agent-x.slice/launch-1.scope\n"
        ));
        assert!(!under_agents("0::/user.slice/user-1000.slice/session-2.scope\n"));
        assert!(under_agents(""), "unreadable counts as agent");
        assert!(under_agents("1:name=x:/y\n"), "not cgroup v2");
    }

    #[test]
    fn each_role_emits_only_its_own_kinds() {
        for k in Kind::ALL {
            assert_eq!(Role::Compositor.may_emit(k), k.from_compositor(), "{k:?}");
        }
        assert!(Role::Brokerd.may_emit(Kind::Secret));
        assert!(!Role::Brokerd.may_emit(Kind::Request));
        assert!(!Role::Compositor.may_emit(Kind::Secret));
        assert!(Role::Agentd.may_emit(Kind::Channel));
        for k in [Kind::Task, Kind::Grant, Kind::Revoke, Kind::Anchor, Kind::Secret] {
            assert!(!Role::Agentd.may_emit(k), "{k:?}");
        }
    }

    #[test]
    fn only_the_compositor_sends_link_messages_today() {
        let m = ToPolicyd::Terminate {
            principal: "agent:a".into(),
        };
        assert!(Role::Compositor.may_send(&m));
        assert!(!Role::Agentd.may_send(&m));
        assert!(!Role::Brokerd.may_send(&m));
    }
}
