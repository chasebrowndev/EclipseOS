// SPDX-License-Identifier: AGPL-3.0-only
//! The `policyd` entry point.
//!
//! Opens the state directory, replays the journal, then serves
//! `policyd.sock` (F-05, COMP-01 §6): each connection from the session user
//! is offered the grant verifying key and held open, so abyss sees the link
//! drop when `policyd` does. What the peer then sends is its audit stream
//! (COMP-12 §1), one emission per packet. The daemon itself lives in the
//! library next to this file.
//!
//! The store has one owner, the main thread. Connection threads decode and
//! queue; a full queue stops a connection thread reading, which fills the
//! socket, which is the backpressure abyss stalls the agent on.

use policy_eval::audit::{Emission, MAX_EMISSION};
use policyd::tasks;

use std::fs;
use std::io::Read;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender};

/// Emissions decoded but not yet appended, across every connection.
const QUEUE: usize = 1024;

/// Where the journal and the issuing key live.
///
/// `$ECLIPSE_STATE_DIR` exists so a test or a nested dev instance can run
/// against its own directory; nothing else in the daemon reads the environment.
fn state_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("ECLIPSE_STATE_DIR") {
        return PathBuf::from(d).join("policyd");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(".local/state/eclipse/policyd")
}

/// Loads the issuing key, generating one on first run.
///
/// The key never leaves this process and the file is `0600` in a `0700`
/// directory: a grant is only worth as much as the exclusivity of the thing
/// that signs it.
fn load_key(dir: &Path) -> std::io::Result<ed25519_dalek::SigningKey> {
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    let path = dir.join("issuer.key");
    let mut seed = [0u8; 32];
    match fs::OpenOptions::new().read(true).open(&path) {
        Ok(mut f) => {
            f.read_exact(&mut seed)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            getrandom::fill(&mut seed).map_err(std::io::Error::other)?;
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)?;
            use std::io::Write;
            f.write_all(&seed)?;
            f.sync_all()?;
        }
        Err(e) => return Err(e),
    }
    Ok(ed25519_dalek::SigningKey::from_bytes(&seed))
}

fn main() -> std::process::ExitCode {
    let dir = state_dir();
    let key = match load_key(&dir) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("policyd: cannot load the issuing key: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let offer = policy_eval::link::encode_key_offer(&key.verifying_key());
    // A store that will not open is fatal, not a warning: without the journal
    // there is nothing to make a grant accountable to.
    let mut store = match tasks::TaskStore::open(&dir, key) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("policyd: cannot open the audit store: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let live = store.tasks().iter().filter(|t| t.state.is_live()).count();
    let grants = store.grants().iter().filter(|g| !g.revoked).count();
    eprintln!(
        "policyd: {} tasks replayed ({live} live), {grants} live grants",
        store.tasks().len()
    );
    let (tx, rx) = mpsc::sync_channel(QUEUE);
    let listener = match listen() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("policyd: cannot serve policyd.sock: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    std::thread::spawn(move || {
        if let Err(e) = serve(listener, &offer, tx) {
            eprintln!("policyd: policyd.sock: {e}");
            std::process::exit(1);
        }
    });
    write(&mut store, rx)
}

/// Appends every queued emission. A record that cannot be written ends the
/// daemon: abyss sees the link drop and pauses every agent (COMP-01 §6),
/// which is the fail-closed answer to an audit log that has stopped taking
/// records. Continuing would let agents act unjournalled.
fn write(store: &mut tasks::TaskStore, rx: Receiver<Emission>) -> std::process::ExitCode {
    for e in rx {
        if let Err(err) = store.record(e) {
            eprintln!("policyd: cannot append an audit record: {err}");
            return std::process::ExitCode::FAILURE;
        }
    }
    std::process::ExitCode::FAILURE
}

/// `$ECLIPSE_POLICYD_SOCKET`, else `$XDG_RUNTIME_DIR/eclipse/policyd.sock`.
fn socket_path() -> std::io::Result<PathBuf> {
    if let Some(p) = std::env::var_os("ECLIPSE_POLICYD_SOCKET") {
        return Ok(PathBuf::from(p));
    }
    let run =
        std::env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| std::io::Error::other("XDG_RUNTIME_DIR unset"))?;
    let dir = PathBuf::from(run).join("eclipse");
    fs::create_dir_all(&dir)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    Ok(dir.join("policyd.sock"))
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

fn listen() -> std::io::Result<OwnedFd> {
    use rustix::net::{self, SocketAddrUnix};
    let path = socket_path()?;
    let addr = SocketAddrUnix::new(&path)?;
    // A socket file someone still answers on is a second policyd: refuse to
    // steal it. One nobody answers on is left over from a crash.
    if path.exists() {
        if net::connect(seqpacket()?, &addr).is_ok() {
            return Err(std::io::Error::other("another policyd is serving"));
        }
        fs::remove_file(&path)?;
    }
    let listener = seqpacket()?;
    net::bind(&listener, &addr)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    net::listen(&listener, 4)?;
    Ok(listener)
}

fn serve(listener: OwnedFd, offer: &[u8], tx: SyncSender<Emission>) -> std::io::Result<()> {
    use rustix::net::{self, SocketFlags};
    let me = rustix::process::getuid();
    loop {
        let conn = match net::accept_with(&listener, SocketFlags::CLOEXEC) {
            Ok(c) => c,
            Err(rustix::io::Errno::INTR) => continue,
            Err(e) => return Err(e.into()),
        };
        // Only the session user is offered the key. The file mode already
        // says so; this is the kernel's word for it.
        match net::sockopt::socket_peercred(&conn) {
            Ok(c) if c.uid == me => {}
            _ => continue,
        }
        let offer = offer.to_vec();
        let tx = tx.clone();
        std::thread::spawn(move || hold(conn, &offer, &tx));
    }
}

/// Offers the key, then queues every emission the peer sends until it hangs
/// up. A packet that is oversized, malformed, or of a kind the compositor
/// does not emit (COMP-12 §2) ends the connection: a source whose stream
/// cannot be trusted is cut off, not partly believed.
fn hold(conn: OwnedFd, offer: &[u8], tx: &SyncSender<Emission>) {
    use rustix::net::{recv, send, RecvFlags, SendFlags};
    if send(&conn, offer, SendFlags::NOSIGNAL).is_err() {
        return;
    }
    let mut buf = vec![0u8; MAX_EMISSION];
    loop {
        // With TRUNC the second length is the packet's, not what fit.
        let n = match recv(&conn, &mut buf[..], RecvFlags::TRUNC) {
            Ok((_, 0)) => return,
            Ok((_, len)) if len > MAX_EMISSION => {
                eprintln!("policyd: oversized audit emission; dropping the peer");
                return;
            }
            Ok((n, _)) => n,
            Err(rustix::io::Errno::INTR) => continue,
            Err(_) => return,
        };
        match accept(&buf[..n]) {
            Some(e) => {
                if tx.send(e).is_err() {
                    return;
                }
            }
            None => {
                eprintln!("policyd: malformed audit emission; dropping the peer");
                return;
            }
        }
    }
}

/// An emission `policyd` will store from this peer.
fn accept(packet: &[u8]) -> Option<Emission> {
    Emission::decode(packet).ok().filter(|e| e.kind.from_compositor())
}

#[cfg(test)]
mod tests {
    use super::*;
    use policy_eval::audit::Kind;
    use policy_eval::cbor::enc;

    fn emission(kind: Kind) -> Emission {
        Emission {
            kind,
            principal: "agent:a".into(),
            grant_id: None,
            task_id: None,
            chain_id: None,
            req_id: Some(1),
            serial: None,
            body: enc(|w| w.null()),
        }
    }

    #[test]
    fn a_compositor_kind_is_accepted() {
        let e = emission(Kind::Request);
        assert_eq!(accept(&e.encode()), Some(e));
    }

    #[test]
    fn a_kind_policyd_owns_is_refused_from_the_socket() {
        for k in [Kind::Grant, Kind::Revoke, Kind::Task, Kind::Anchor] {
            assert_eq!(accept(&emission(k).encode()), None, "{k:?}");
        }
    }

    #[test]
    fn garbage_is_refused() {
        assert_eq!(accept(&[0xff, 0x00]), None);
        assert_eq!(accept(&[]), None);
    }
}
