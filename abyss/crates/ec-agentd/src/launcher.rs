// SPDX-License-Identifier: AGPL-3.0-only
//! Starting and stopping an agent process (A-01 §4, VOL1 agent sandbox
//! baseline).
//!
//! Sandboxed (the only build the package ships):
//! `systemd-run --user --scope --slice=agents-<pkg>.slice` (with memory, task
//! and CPU limits) around `bwrap` with new user/pid/ipc/uts/net/cgroup
//! namespaces, all capabilities dropped, a read-only root, tmpfs `/home`,
//! `/tmp` and `/run`, the package directory read-only at `/opt/agent`, the
//! manifest's `fs.read` paths bound read-only, and the task's MCP socket as
//! the only socket bound in. There is no network: `--unshare-net` with no
//! egress proxy (S-09, milestone 20), so a package asking for `net.egress` is
//! refused before launch.
//!
//! Inside bwrap the entrypoint is not run directly: agentd re-execs itself as
//! `--sandbox-init` ([`crate::sandbox`]), which sets `no_new_privs`, applies
//! the Landlock allow-list and the seccomp deny-list, and only then execs the
//! agent.
//!
//! The `dev-unsandboxed` feature runs the entrypoint as a plain child for
//! headless tests and is never in the PKGBUILD.
//!
//! Not done here: a network egress proxy (docs/KNOWNBUGS.md AGENTD-01).

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;

use crate::core::Msg;
use crate::sandbox::Decl;
#[cfg(not(feature = "dev-unsandboxed"))]
use crate::sandbox::FsPolicy;

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
    /// The validated `sandbox { }` declaration.
    pub decl: &'a Decl,
    /// agentd's own executable, bound into the sandbox as the init helper.
    pub init_exe: &'a Path,
}

/// Where the helper finds its config and itself, inside the sandbox.
#[cfg_attr(feature = "dev-unsandboxed", allow(dead_code))]
const IN_CFG: &str = "/run/eclipse/sandbox.json";
#[cfg_attr(feature = "dev-unsandboxed", allow(dead_code))]
const IN_INIT: &str = "/run/eclipse/ec-agentd";

/// The sandbox config's path: beside the task's MCP socket.
pub fn cfg_path(mcp_sock: &Path) -> std::path::PathBuf {
    mcp_sock.with_file_name("sandbox.json")
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
        "-p",
        concat!("MemoryMax=", "2G"),
        "-p",
        concat!("TasksMax=", "256"),
        "-p",
        concat!("CPUQuota=", "200%"),
        "--",
        "bwrap",
        "--unshare-all",
        "--die-with-parent",
        "--new-session",
        "--cap-drop",
        "ALL",
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
    for p in &s.decl.fs_read {
        c.arg("--ro-bind").arg(p).arg(p);
    }
    c.arg("--ro-bind").arg(s.init_exe).arg(IN_INIT);
    c.arg("--ro-bind").arg(cfg_path(s.mcp_sock)).arg(IN_CFG);
    c.arg("--bind").arg(s.mcp_sock).arg(sock);
    c.args(["--setenv", "ECLIPSE_MCP_SOCKET", sock]);
    c.args(["--setenv", "ECLIPSE_TASK_ID", s.task]);
    c.args(["--setenv", "PATH", "/usr/local/bin:/usr/bin:/bin"]);
    c.args(["--setenv", "HOME", "/tmp"]);
    c.args(["--chdir", "/opt/agent", "--"]);
    c.args([IN_INIT, "--sandbox-init", IN_CFG, "--"]);
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
    #[cfg(not(feature = "dev-unsandboxed"))]
    {
        let cfg = FsPolicy::for_package(Path::new("/opt/agent"), s.decl).to_json();
        write_private(&cfg_path(s.mcp_sock), cfg.to_string().as_bytes())?;
    }
    let mut cmd = command(s);
    // Agent output is the agent's business, not the journal's. The one
    // exception is the sandbox helper's own stderr (and bwrap's), which is
    // how a failed sandbox is explained: the helper points the agent's
    // stderr at /dev/null before exec, so only launch errors reach the pipe.
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(if cfg!(feature = "dev-unsandboxed") {
            Stdio::null()
        } else {
            Stdio::piped()
        });
    let mut child = cmd.spawn()?;
    let pid = child.id();
    let task = s.task.to_owned();
    let mut launch_err = child.stderr.take();
    std::thread::spawn(move || {
        let status = match child.wait() {
            Ok(st) if st.success() => "completed",
            Ok(st) => {
                if let Some(e) = launch_err.take() {
                    use std::io::Read;
                    let mut buf = Vec::new();
                    let _ = e.take(2048).read_to_end(&mut buf);
                    let why = String::from_utf8_lossy(&buf);
                    if !why.trim().is_empty() {
                        eprintln!("ec-agentd: task {task}: launch failed ({st}): {}", why.trim());
                    }
                }
                "failed"
            }
            Err(_) => "failed",
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

#[cfg(not(feature = "dev-unsandboxed"))]
fn write_private(p: &Path, b: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(p)?
        .write_all(b)
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
        let decl = Decl {
            fs_read: vec!["/data/in".into()],
        };
        let c = command(&Spec {
            task: "T",
            package: "a",
            pkg_dir: dir,
            entrypoint: &ep,
            mcp_sock: sock,
            decl: &decl,
            init_exe: Path::new("/usr/bin/ec-agentd"),
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

    #[cfg(not(feature = "dev-unsandboxed"))]
    #[test]
    fn the_baseline_holds() {
        let ep = vec!["bin/run".to_owned()];
        let decl = Decl {
            fs_read: vec!["/data/in".into()],
        };
        let c = command(&Spec {
            task: "T",
            package: "a",
            pkg_dir: Path::new("/p"),
            entrypoint: &ep,
            mcp_sock: Path::new("/run/user/1000/eclipse/agents/T/mcp.sock"),
            decl: &decl,
            init_exe: Path::new("/usr/bin/ec-agentd"),
        });
        let a: Vec<String> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        let has = |w: &[&str]| a.windows(w.len()).any(|x| x.iter().zip(w).all(|(p, q)| p == q));
        // Scope limits.
        assert!(
            has(&["-p", "MemoryMax=2G"]) && has(&["-p", "TasksMax=256"]) && has(&["-p", "CPUQuota=200%"])
        );
        // Empty environment but the four variables; no way to a display or bus.
        assert!(a.contains(&"--clearenv".to_owned()) && has(&["--cap-drop", "ALL"]));
        let setenv: Vec<&str> = a
            .windows(3)
            .filter(|w| w[0] == "--setenv")
            .map(|w| w[1].as_str())
            .collect();
        assert_eq!(setenv, ["ECLIPSE_MCP_SOCKET", "ECLIPSE_TASK_ID", "PATH", "HOME"]);
        assert!(has(&["--ro-bind", "/", "/"]));
        for t in ["/home", "/tmp", "/run"] {
            assert!(has(&["--tmpfs", t]), "{t}");
        }
        // The only writable bind is the MCP socket; declared reads are ro.
        assert!(has(&["--ro-bind", "/data/in", "/data/in"]));
        assert_eq!(a.iter().filter(|x| *x == "--bind").count(), 1);
        assert!(!a.iter().any(|x| x.contains("WAYLAND")
            || x.contains(".ssh")
            || x.contains(".gnupg")
            || x.contains("bus")));
        // No network, and the agent runs behind the sandbox helper.
        assert!(a.contains(&"--unshare-all".to_owned()) && !a.contains(&"--share-net".to_owned()));
        assert!(has(&[
            "/run/eclipse/ec-agentd",
            "--sandbox-init",
            "/run/eclipse/sandbox.json",
            "--",
            "/opt/agent/bin/run"
        ]));
    }
}
