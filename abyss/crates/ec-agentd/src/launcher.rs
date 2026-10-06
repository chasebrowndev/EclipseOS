// SPDX-License-Identifier: AGPL-3.0-only
//! Starting and stopping an agent process (A-01 §4, VOL1 agent sandbox
//! baseline).
//!
//! Sandboxed (the only build the package ships):
//! `systemd-run --user --scope --slice=agents-<pkg>.slice` around `bwrap` with
//! new user/pid/ipc/uts/net/cgroup namespaces, a read-only root, tmpfs
//! `/home`, `/tmp` and `/run`, the package directory read-only at
//! `/opt/agent`, and the task's MCP socket as the only socket bound in.
//!
//! The `dev-unsandboxed` feature runs the entrypoint as a plain child for
//! headless tests and is never in the PKGBUILD.
//!
//! Not done here: Landlock, seccomp, per-grant filesystem binds, network
//! proxying and resource limits (docs/KNOWNBUGS.md).

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;

use crate::core::Msg;

/// A started agent. Stopped by pid and scope name, never by process name.
pub struct Running {
    pub pid: u32,
    scope: Option<String>,
}

pub struct Spec<'a> {
    pub task: &'a str,
    pub package: &'a str,
    pub pkg_dir: &'a Path,
    /// The manifest's `entrypoint` words, already validated.
    pub entrypoint: &'a [String],
    pub mcp_sock: &'a Path,
}

/// The slice name for a package: one level under `agents.slice`, so `-` in the
/// id (which systemd reads as nesting) becomes `_`.
#[cfg_attr(feature = "dev-unsandboxed", allow(dead_code))]
pub fn slice_name(package: &str) -> String {
    let id: String = package
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("agents-{id}.slice")
}

pub fn scope_name(task: &str) -> String {
    format!("eclipse-task-{task}.scope")
}

/// `rel` as the entrypoint path under `base`: only a first word with a `/` is
/// a package path; a bare word is a runtime command.
fn resolve(first: &str, base: &str) -> String {
    if first.contains('/') {
        format!(
            "{}/{}",
            base.trim_end_matches('/'),
            first.trim_start_matches("./")
        )
    } else {
        first.to_owned()
    }
}

#[cfg(not(feature = "dev-unsandboxed"))]
pub fn command(s: &Spec) -> Command {
    let sock = "/run/eclipse/mcp.sock";
    let slice = format!("--slice={}", slice_name(s.package));
    let unit = format!("--unit=eclipse-task-{}", s.task);
    let mut c = Command::new("systemd-run");
    c.args([
        "--user",
        "--scope",
        "--quiet",
        "--collect",
        slice.as_str(),
        unit.as_str(),
        "--",
        "bwrap",
        "--unshare-all",
        "--die-with-parent",
        "--new-session",
        "--clearenv",
        "--ro-bind",
        "/",
        "/",
        "--tmpfs",
        "/home",
        "--tmpfs",
        "/tmp",
        "--tmpfs",
        "/run",
        "--tmpfs",
        "/opt",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
    ]);
    c.arg("--ro-bind").arg(s.pkg_dir).arg("/opt/agent");
    c.arg("--bind").arg(s.mcp_sock).arg(sock);
    c.args(["--setenv", "ECLIPSE_MCP_SOCKET", sock]);
    c.args(["--setenv", "ECLIPSE_TASK_ID", s.task]);
    c.args(["--setenv", "PATH", "/usr/local/bin:/usr/bin:/bin"]);
    c.args(["--setenv", "HOME", "/tmp"]);
    c.args(["--chdir", "/opt/agent", "--"]);
    c.arg(resolve(&s.entrypoint[0], "/opt/agent"));
    c.args(&s.entrypoint[1..]);
    c
}

#[cfg(feature = "dev-unsandboxed")]
pub fn command(s: &Spec) -> Command {
    let first = resolve(&s.entrypoint[0], &s.pkg_dir.to_string_lossy());
    let mut c = Command::new(first);
    c.args(&s.entrypoint[1..]);
    c.current_dir(s.pkg_dir);
    c.env("ECLIPSE_MCP_SOCKET", s.mcp_sock);
    c.env("ECLIPSE_TASK_ID", s.task);
    c
}

/// Starts the agent; a thread reaps it and reports `Msg::ProcExit`.
pub fn start(s: &Spec, tx: Sender<Msg>) -> std::io::Result<Running> {
    if s.entrypoint.is_empty() {
        return Err(std::io::Error::other("empty entrypoint"));
    }
    let mut cmd = command(s);
    // Agent output is the agent's business, not the journal's.
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd.spawn()?;
    let pid = child.id();
    let task = s.task.to_owned();
    std::thread::spawn(move || {
        let status = match child.wait() {
            Ok(st) if st.success() => "completed",
            _ => "failed",
        };
        let _ = tx.send(Msg::ProcExit {
            task,
            status: status.to_owned(),
        });
    });
    Ok(Running {
        pid,
        scope: cfg!(not(feature = "dev-unsandboxed")).then(|| scope_name(s.task)),
    })
}

/// Stops the agent by pid, and its whole scope when it has one.
pub fn stop(r: &Running) {
    if let Some(scope) = &r.scope {
        let _ = Command::new("systemctl")
            .args(["--user", "stop", scope])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    if let Some(pid) = rustix::process::Pid::from_raw(r.pid as i32) {
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_is_one_level() {
        assert_eq!(slice_name("invoice-triage"), "agents-invoice_triage.slice");
    }

    #[test]
    fn package_paths_resolve_under_the_base() {
        assert_eq!(resolve("bin/run", "/opt/agent"), "/opt/agent/bin/run");
        assert_eq!(resolve("./run", "/opt/agent/"), "/opt/agent/run");
        assert_eq!(resolve("python", "/opt/agent"), "python");
    }

    #[cfg(not(feature = "dev-unsandboxed"))]
    #[test]
    fn sandbox_command_binds_only_the_mcp_socket() {
        let dir = Path::new("/home/u/.local/share/eclipse/agents/a/1");
        let sock = Path::new("/run/user/1000/eclipse/agents/T/mcp.sock");
        let ep = vec!["bin/run".to_owned(), "--x".to_owned()];
        let c = command(&Spec {
            task: "T",
            package: "a",
            pkg_dir: dir,
            entrypoint: &ep,
            mcp_sock: sock,
        });
        let args: Vec<String> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(c.get_program(), "systemd-run");
        assert!(args.contains(&"--unshare-all".to_owned()));
        assert!(args.contains(&"--slice=agents-a.slice".to_owned()));
        assert_eq!(args.iter().filter(|a| *a == "--bind").count(), 1);
        assert!(args
            .windows(2)
            .any(|w| w[0] == "/run/eclipse/mcp.sock" && w[1] == "--setenv"));
        assert!(args.contains(&"/opt/agent/bin/run".to_owned()));
    }
}
