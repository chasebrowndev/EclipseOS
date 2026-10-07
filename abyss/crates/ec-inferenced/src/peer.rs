// SPDX-License-Identifier: AGPL-3.0-only
//! Who may connect: `ec-agentd`, and nothing else.
//!
//! The same rule as brokerd's `classify` (`ec-brokerd/src/main.rs`): the
//! kernel's word for the peer's executable, only `/usr/bin` unless the
//! `dev-peers` build feature is on, and nothing under `agents.slice` whatever
//! it runs. A build feature, not an environment variable, because any
//! same-uid process can set a user unit's environment.

use std::path::Path;

/// Pure core of [`classify`], over the already-read `exe` and cgroup text.
pub fn allowed(exe: &Path, cgroup: &str, dev_peers: bool) -> bool {
    if under_agents(cgroup) {
        return false;
    }
    if !dev_peers && exe.parent() != Some(Path::new("/usr/bin")) {
        return false;
    }
    exe.file_name().and_then(|n| n.to_str()) == Some("ec-agentd")
}

/// Whether the process with `pid` is an acceptable peer.
pub fn classify(pid: i32) -> bool {
    let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap_or_default();
    let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe")) else {
        return false;
    };
    allowed(&exe, &cgroup, cfg!(feature = "dev-peers"))
}

/// Whether `/proc/<pid>/cgroup` (cgroup v2) places the process under
/// `agents.slice`. An unreadable or unexpected file counts as yes.
fn under_agents(cgroup: &str) -> bool {
    let mut lines = cgroup.lines();
    let (Some(line), None) = (lines.next(), lines.next()) else {
        return true;
    };
    line.strip_prefix("0::")
        .is_none_or(|path| path.split('/').any(|c| c == "agents.slice"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/ec-agentd.service\n";
    const AGENT: &str =
        "0::/user.slice/user-1000.slice/user@1000.service/agents.slice/agents-x.slice/s.scope\n";

    #[test]
    fn only_agentd_from_usr_bin() {
        assert!(allowed(Path::new("/usr/bin/ec-agentd"), USER, false));
        assert!(!allowed(Path::new("/usr/bin/ec-ctl"), USER, false));
        assert!(!allowed(Path::new("/usr/bin/ec-brokerd"), USER, false));
        assert!(!allowed(Path::new("/usr/bin/sh"), USER, false));
    }

    #[test]
    fn strict_build_refuses_other_directories() {
        assert!(!allowed(Path::new("/tmp/ec-agentd"), USER, false));
        assert!(!allowed(Path::new("/home/u/target/debug/ec-agentd"), USER, false));
    }

    #[test]
    fn dev_build_accepts_the_name_from_anywhere() {
        assert!(allowed(Path::new("/home/u/target/debug/ec-agentd"), USER, true));
        assert!(!allowed(Path::new("/home/u/target/debug/ec-other"), USER, true));
    }

    #[test]
    fn nothing_under_agents_slice_is_a_peer() {
        assert!(!allowed(Path::new("/usr/bin/ec-agentd"), AGENT, false));
        assert!(!allowed(Path::new("/usr/bin/ec-agentd"), AGENT, true));
        assert!(!allowed(Path::new("/usr/bin/ec-agentd"), "", true));
        assert!(!allowed(Path::new("/usr/bin/ec-agentd"), "0::/a\n0::/b\n", true));
    }
}
