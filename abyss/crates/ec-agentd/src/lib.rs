// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-agentd`: the agent daemon (A-01, A-08 §5-§13, A-02, A-03).
//!
//! * the console backend: `console.sock`, the task model and the per-task
//!   conversation store (A-08 §6, §7, §11);
//! * the agent host: one MCP socket per task and the launcher (A-02, A-01 §4);
//! * the `policyd` link that provisions tasks and takes pause and cancel
//!   (C2 in docs/internal/console-plan.md);
//! * [`admit`], the M11 admission client, kept as the future per-task bridge.
//!
//! Not TCB, and no authority: agentd cannot create a task, and the only
//! `ToPolicyd` messages in this crate are pause, cancel and exited (a test
//! greps the source to hold that line).

pub mod admit;
pub mod core;
pub mod human;
pub mod launcher;
pub mod link;
pub mod net;
pub mod packages;
pub mod rpc;
pub mod sanitize;
pub mod store;

use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::time::Duration;

use ec_policy_eval::Ulid;

use crate::core::{Core, Msg, Surface};

/// Transcript retention (A-08 §11): 30 days, owner-configurable.
pub const RETENTION_DAYS: u64 = 30;

#[derive(Clone)]
pub struct Config {
    /// `$XDG_RUNTIME_DIR/eclipse`.
    pub runtime_dir: PathBuf,
    /// `$XDG_STATE_HOME/eclipse`.
    pub state_dir: PathBuf,
    pub policyd_socket: PathBuf,
    /// abyss's human control socket, for `show_decisions` and the
    /// `decisions_pending` relay. `None` disables both.
    pub human_socket: Option<PathBuf>,
    /// `(root, publisher)` in lookup order.
    pub package_roots: Vec<(PathBuf, String)>,
    pub retention_ms: u64,
    /// Start agent processes on Provision. Off in tests that play the agent.
    pub launch: bool,
    /// The console peer rule (A-08 §7).
    pub console_policy: net::PeerPolicy,
    /// Milliseconds since the epoch; replaceable in tests.
    pub clock: Arc<dyn Fn() -> u64 + Send + Sync>,
}

pub fn system_clock() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl Config {
    pub fn from_env() -> Result<Config, String> {
        let run = std::env::var_os("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR is not set")?;
        let run = PathBuf::from(run).join("eclipse");
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let state = match std::env::var_os("XDG_STATE_HOME") {
            Some(s) if !s.is_empty() => PathBuf::from(s),
            _ => home.clone().ok_or("HOME is not set")?.join(".local/state"),
        };
        let data = match std::env::var_os("XDG_DATA_HOME") {
            Some(s) if !s.is_empty() => PathBuf::from(s),
            _ => home.ok_or("HOME is not set")?.join(".local/share"),
        };
        let policyd_socket = std::env::var_os("ECLIPSE_POLICYD_SOCKET")
            .map(PathBuf::from)
            .unwrap_or_else(|| run.join("policyd.sock"));
        Ok(Config {
            human_socket: Some(run.join("abyss.sock")),
            runtime_dir: run,
            state_dir: state.join("eclipse"),
            policyd_socket,
            package_roots: vec![
                (data.join("eclipse/agents"), "local".to_owned()),
                (PathBuf::from("/usr/share/eclipse/agents"), "eclipse".to_owned()),
            ],
            retention_ms: RETENTION_DAYS * 24 * 3600 * 1000,
            launch: true,
            console_policy: net::console_policy(),
            clock: Arc::new(system_clock),
        })
    }
}

/// A running daemon: the core thread and its listeners.
pub struct Daemon {
    pub tx: Sender<Msg>,
    pub console_socket: PathBuf,
    stop: Arc<AtomicBool>,
    core: Option<std::thread::JoinHandle<()>>,
}

fn private_dir(p: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(p)?;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700))
}

fn bind_console(path: &Path) -> std::io::Result<UnixListener> {
    if path.exists() {
        if std::os::unix::net::UnixStream::connect(path).is_ok() {
            return Err(std::io::Error::other("another agentd is serving console.sock"));
        }
        std::fs::remove_file(path)?;
    }
    let l = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(l)
}

pub fn start(cfg: Config) -> std::io::Result<Daemon> {
    private_dir(&cfg.runtime_dir)?;
    private_dir(&cfg.runtime_dir.join("agents"))?;
    private_dir(&cfg.state_dir)?;
    let console_socket = cfg.runtime_dir.join("console.sock");
    let listener = bind_console(&console_socket)?;
    let (tx, rx) = channel::<Msg>();
    let stop = Arc::new(AtomicBool::new(false));
    let ids = Arc::new(AtomicU64::new(1));
    net::serve(
        listener,
        Surface::Console,
        cfg.console_policy.clone(),
        tx.clone(),
        stop.clone(),
        ids.clone(),
    );
    link::spawn(cfg.policyd_socket.clone(), tx.clone(), stop.clone());
    if let Some(h) = &cfg.human_socket {
        human::spawn_relay(h.clone(), tx.clone(), stop.clone());
    }
    {
        let (tx, stop) = (tx.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(500));
                if tx.send(Msg::Tick).is_err() {
                    return;
                }
            }
        });
    }
    let core = Core::new(cfg, tx.clone(), ids);
    let handle = std::thread::spawn(move || core.run(rx));
    Ok(Daemon {
        tx,
        console_socket,
        stop,
        core: Some(handle),
    })
}

impl Daemon {
    /// Stops the listeners and the core, which stops every agent it started.
    pub fn shutdown(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Msg::Shutdown);
        if let Some(h) = self.core.take() {
            let _ = h.join();
        }
        let _ = std::fs::remove_file(&self.console_socket);
    }

    /// Blocks until the core ends.
    pub fn join(mut self) {
        if let Some(h) = self.core.take() {
            let _ = h.join();
        }
    }
}

/// Decodes the 26-character Crockford form `Ulid::to_text` writes.
pub fn ulid_from_text(s: &str) -> Option<Ulid> {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let b = s.as_bytes();
    if b.len() != 26 {
        return None;
    }
    let mut n: u128 = 0;
    for (i, c) in b.iter().enumerate() {
        let v = ALPHABET.iter().position(|a| a == c)? as u128;
        if i == 0 && v > 7 {
            return None;
        }
        n = (n << 5) | v;
    }
    Some(Ulid(n.to_be_bytes()))
}

/// A fresh scratch directory under the system temp dir, for tests.
#[doc(hidden)]
pub fn scratch_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    // Unix socket paths are short; keep this short too.
    let p = std::env::temp_dir().join(format!(
        "ea-{}-{}-{tag}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("scratch dir");
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ulid_text_round_trips() {
        let u = Ulid::from_parts(1_700_000_000_000, [9, 8, 7, 6, 5, 4, 3, 2, 1, 0]);
        assert_eq!(ulid_from_text(&u.to_text()), Some(u));
        assert_eq!(ulid_from_text("short"), None);
        assert_eq!(ulid_from_text(&"Z".repeat(26)), None);
        assert_eq!(ulid_from_text("../../../../etc/passwdxx"), None);
    }
}
