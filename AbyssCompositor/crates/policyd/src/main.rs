// SPDX-License-Identifier: AGPL-3.0-only
//! The `policyd` entry point.
//!
//! Opens the state directory, replays the journal, then serves
//! `policyd.sock` (F-05, COMP-01 §6): each connection from the session user
//! is offered the grant verifying key and held open, so abyss sees the link
//! drop when `policyd` does. The daemon itself lives in the library next to
//! this file.

use policyd::tasks;

use std::fs;
use std::io::Read;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

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
    let store = match tasks::TaskStore::open(&dir, key) {
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
    match serve(&offer) {
        Ok(never) => match never {},
        Err(e) => {
            eprintln!("policyd: cannot serve policyd.sock: {e}");
            std::process::ExitCode::FAILURE
        }
    }
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

fn serve(offer: &[u8]) -> std::io::Result<std::convert::Infallible> {
    use rustix::net::{self, SocketAddrUnix, SocketFlags};
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
        std::thread::spawn(move || hold(conn, &offer));
    }
}

/// Offers the key, then holds the connection until the peer hangs up. The
/// peer sends nothing yet; anything it does send is ignored.
fn hold(conn: OwnedFd, offer: &[u8]) {
    use rustix::net::{recv, send, RecvFlags, SendFlags};
    if send(&conn, offer, SendFlags::NOSIGNAL).is_err() {
        return;
    }
    let mut buf = [0u8; 64];
    loop {
        match recv(&conn, &mut buf, RecvFlags::empty()) {
            Ok((0, _)) => return,
            Ok(_) | Err(rustix::io::Errno::INTR) => continue,
            Err(_) => return,
        }
    }
}
