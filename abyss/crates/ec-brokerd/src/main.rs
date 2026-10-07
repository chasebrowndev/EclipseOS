// SPDX-License-Identifier: AGPL-3.0-only
//! The `brokerd` entry point (S-08 §2).
//!
//! Hardens the process first and refuses to run if it cannot (dumpable off,
//! no core, no new privs), then serves `brokerd.sock`: SEQPACKET, one
//! canonical-CBOR message per packet (see `wire`). Connection threads only
//! decode a peer's class and queue packets; the main thread owns the broker
//! and answers, as `policyd` does, so there is one writer and no lock around
//! the secret store.
//!
//! The daemon starts **locked**. A compositor connection that drops locks it
//! again: a crashed or restarted session must unlock afresh (S-08 §2).

use ec_brokerd::audit::{AuditSink, SinkError};
use ec_brokerd::broker::Broker;
use ec_brokerd::gate::Peer;
use ec_brokerd::hygiene::{self, LockedBuf};
use ec_brokerd::sealer::{KdfParams, PassphraseSealer};
use ec_brokerd::wire::{Secret, MAX_MESSAGE};
use ec_policy_eval::audit::Emission;
use std::fs;
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::mpsc::{self, SyncSender};
use std::sync::Arc;
use zeroize::Zeroize;

const QUEUE: usize = 256;

/// What a connection thread hands the main thread.
enum Msg {
    Packet(Peer, Secret, Arc<OwnedFd>),
    /// A peer hung up. A compositor going away locks the broker.
    Closed(Peer),
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `$ECLIPSE_BROKERD_DIR`, else `~/.local/state/eclipse/brokerd/secrets`.
///
/// S-08 §2 names `/var/lib/eclipse/secrets/`. brokerd runs as a *user*
/// service next to policyd (it is unlocked per session, and the session user
/// is who it serves), so the store lives in that user's state directory. This
/// is a recorded deviation: see docs/KNOWNBUGS.md.
fn store_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("ECLIPSE_BROKERD_DIR") {
        return PathBuf::from(d);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(".local/state/eclipse/brokerd/secrets")
}

fn runtime_dir() -> std::io::Result<PathBuf> {
    let run =
        std::env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| std::io::Error::other("XDG_RUNTIME_DIR unset"))?;
    let dir = PathBuf::from(run).join("eclipse");
    fs::create_dir_all(&dir)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

fn seqpacket() -> rustix::io::Result<OwnedFd> {
    use rustix::net::{socket_with, AddressFamily, SocketFlags, SocketType};
    socket_with(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )
}

/// Who a connecting process is, from the kernel's word for its executable.
///
/// The same rule as policyd's peers (`ec-policyd/src/peer.rs`): only binaries
/// installed under `/usr/bin` are recognised, so a same-uid process running a
/// copy of `ec-abyss` from `/tmp` is not the compositor, and nothing under
/// `agents.slice` is recognised whatever it runs. The `dev-peers` feature
/// accepts the same names from any directory, for a dev tree; it is a build
/// choice, not an environment variable, because any same-uid process can set
/// a user unit's environment.
fn classify(pid: i32) -> Option<Peer> {
    let cgroup = fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap_or_default();
    if under_agents(&cgroup) {
        return None;
    }
    let exe = fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    if !cfg!(feature = "dev-peers") && exe.parent()? != std::path::Path::new("/usr/bin") {
        return None;
    }
    match exe.file_name()?.to_str()? {
        "ec-abyss" => Some(Peer::Compositor),
        "ec-egress-proxy" => Some(Peer::Proxy),
        "ec-agentd" => Some(Peer::Agentd),
        "ec-secret" | "ec-ctl" => Some(Peer::Owner),
        _ => None,
    }
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

/// The audit stream to `policyd.sock` (S-04 §4).
///
/// Journal-before-release means a record that cannot be sent is a refused
/// use: `emit` fails, and the broker denies. The key offer and table pushes
/// policyd sends to every peer are drained, unread, before each emission so
/// they cannot back up the socket.
///
/// policyd stores `Kind::Secret` only from a peer it identifies as brokerd
/// (`ec-policyd/src/peer.rs`).
struct PolicydSink {
    path: PathBuf,
    conn: Option<OwnedFd>,
}

impl PolicydSink {
    fn connect(&self) -> Option<OwnedFd> {
        use rustix::net::{self, SocketAddrUnix};
        let addr = SocketAddrUnix::new(&self.path).ok()?;
        let fd = seqpacket().ok()?;
        net::connect(&fd, &addr).ok()?;
        Some(fd)
    }

    fn drain(fd: &OwnedFd) {
        use rustix::net::{recv, RecvFlags};
        let mut sink = vec![0u8; 64 * 1024];
        while let Ok((n, _)) = recv(fd, &mut sink[..], RecvFlags::DONTWAIT) {
            if n == 0 {
                break;
            }
        }
    }

    fn send_on(fd: &OwnedFd, bytes: &[u8]) -> bool {
        use rustix::net::{send, SendFlags};
        Self::drain(fd);
        send(fd, bytes, SendFlags::NOSIGNAL).is_ok()
    }
}

impl AuditSink for PolicydSink {
    fn emit(&mut self, e: Emission) -> Result<(), SinkError> {
        let bytes = e.encode();
        if let Some(fd) = &self.conn {
            if Self::send_on(fd, &bytes) {
                return Ok(());
            }
        }
        // Stale or absent: one fresh connection, one retry.
        self.conn = self.connect();
        match &self.conn {
            Some(fd) if Self::send_on(fd, &bytes) => Ok(()),
            _ => Err(SinkError),
        }
    }
}

fn listen(path: &std::path::Path) -> std::io::Result<OwnedFd> {
    use rustix::net::{self, SocketAddrUnix};
    let addr = SocketAddrUnix::new(path)?;
    if path.exists() {
        if net::connect(&seqpacket()?, &addr).is_ok() {
            return Err(std::io::Error::other("another brokerd is serving"));
        }
        fs::remove_file(path)?;
    }
    let l = seqpacket()?;
    net::bind(&l, &addr)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    net::listen(&l, 4)?;
    Ok(l)
}

fn serve(listener: OwnedFd, tx: SyncSender<Msg>) -> std::io::Result<()> {
    use rustix::net::{self, SocketFlags};
    let me = rustix::process::getuid();
    loop {
        let conn = match net::accept_with(&listener, SocketFlags::CLOEXEC) {
            Ok(c) => c,
            Err(rustix::io::Errno::INTR) => continue,
            Err(e) => return Err(e.into()),
        };
        let peer = match net::sockopt::socket_peercred(&conn) {
            Ok(c) if c.uid == me => classify(c.pid.as_raw_nonzero().get()),
            _ => None,
        };
        // An unrecognised peer is dropped before a byte of it is parsed.
        let Some(peer) = peer else { continue };
        let tx = tx.clone();
        std::thread::spawn(move || hold(conn, peer, &tx));
    }
}

fn hold(conn: OwnedFd, peer: Peer, tx: &SyncSender<Msg>) {
    use rustix::net::{recv, RecvFlags};
    let conn = Arc::new(conn);
    let mut buf = vec![0u8; MAX_MESSAGE];
    loop {
        let got = recv(&*conn, &mut buf[..], RecvFlags::TRUNC);
        let n = match got {
            Ok((_, 0)) => break,
            Ok((_, len)) if len > MAX_MESSAGE => break,
            Ok((n, _)) => n,
            Err(rustix::io::Errno::INTR) => continue,
            Err(_) => break,
        };
        let packet = Secret::new(buf[..n].to_vec());
        // The receive buffer held plaintext (a passphrase, a value to add).
        buf[..n].zeroize();
        if tx.send(Msg::Packet(peer, packet, conn.clone())).is_err() {
            break;
        }
    }
    buf.zeroize();
    let _ = tx.send(Msg::Closed(peer));
}

/// Self-test target for the secrets suite: harden, hold a locked secret,
/// report the settings, and wait. The test then attacks this process the
/// way a sandboxed agent would.
fn hygiene_probe() -> std::process::ExitCode {
    use std::io::{BufRead, Write};
    if let Err(e) = hygiene::harden() {
        eprintln!("brokerd: cannot harden: {e}");
        return std::process::ExitCode::FAILURE;
    }
    let held = match LockedBuf::from_slice(b"HYGIENE-PROBE-SECRET-0123456789") {
        Ok(b) => b,
        Err(e) => {
            eprintln!("brokerd: cannot lock memory: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let (soft, hard) = hygiene::core_limit().unwrap_or((u64::MAX, u64::MAX));
    println!(
        "ready pid={} dumpable={} core_soft={soft} core_hard={hard} vmlck_kib={}",
        std::process::id(),
        hygiene::dumpable().unwrap_or(-1),
        hygiene::vm_locked_kib().unwrap_or(0),
    );
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    drop(held);
    std::process::ExitCode::SUCCESS
}

fn main() -> std::process::ExitCode {
    // As policyd's: tells the dev install whether this is a `dev-peers` build.
    if std::env::args().nth(1).as_deref() == Some("--build-info") {
        println!(
            "{}",
            if cfg!(feature = "dev-peers") {
                "dev-peers"
            } else {
                "strict-peers"
            }
        );
        return std::process::ExitCode::SUCCESS;
    }
    if std::env::args().nth(1).as_deref() == Some("--hygiene-probe") {
        return hygiene_probe();
    }
    if let Err(e) = hygiene::harden() {
        eprintln!("brokerd: cannot harden the process, refusing to hold secrets: {e}");
        return std::process::ExitCode::FAILURE;
    }
    let run = match runtime_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("brokerd: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let policyd = std::env::var_os("ECLIPSE_POLICYD_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| run.join("policyd.sock"));
    let dir = store_dir();
    if let Some(parent) = dir.parent() {
        if fs::create_dir_all(parent).is_err() {
            eprintln!("brokerd: cannot create {}", parent.display());
            return std::process::ExitCode::FAILURE;
        }
    }
    let sealer = PassphraseSealer {
        params: KdfParams::PRODUCTION,
    };
    let mut broker = Broker::new(
        dir,
        Box::new(sealer),
        Box::new(PolicydSink {
            path: policyd,
            conn: None,
        }),
    );
    let listener = match listen(&run.join("brokerd.sock")) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("brokerd: cannot serve brokerd.sock: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let (tx, rx) = mpsc::sync_channel(QUEUE);
    std::thread::spawn(move || {
        if let Err(e) = serve(listener, tx) {
            eprintln!("brokerd: brokerd.sock: {e}");
            std::process::exit(1);
        }
    });
    eprintln!(
        "brokerd: serving, {} ({}), locked",
        if broker.is_initialised() {
            "store present"
        } else {
            "no store yet"
        },
        broker.sealer_name()
    );
    for msg in rx {
        match msg {
            Msg::Packet(peer, packet, conn) => {
                use rustix::net::{send, SendFlags};
                let reply = broker.handle(peer, &packet, now());
                // A peer that cannot take its answer has hung up.
                let _ = send(&*conn, &reply, SendFlags::NOSIGNAL);
            }
            Msg::Closed(Peer::Compositor) => {
                let _ = broker.lock(Peer::Compositor);
            }
            Msg::Closed(_) => {}
        }
    }
    std::process::ExitCode::FAILURE
}
