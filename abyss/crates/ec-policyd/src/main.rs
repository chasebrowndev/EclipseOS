// SPDX-License-Identifier: AGPL-3.0-only
//! The `policyd` entry point.
//!
//! Opens the state directory, replays the journal, then serves
//! `policyd.sock` (F-05, COMP-01 §6): each connection from the session user
//! is offered the grant verifying key and held open, so abyss sees the link
//! drop when `policyd` does. What the peer then sends is its audit stream
//! (COMP-12 §1), one emission per packet, interleaved with link messages
//! (`ec_policy_eval::link::ToPolicyd`) that the main thread answers on the
//! same connection. The daemon itself lives in the library next to this file.
//!
//! Link-message answers, as built so far:
//! - `revoke` and `terminate` cancel the principal's live task, which revokes
//!   its grants in one record, and answer `revoked`.
//! - `audit_tail` answers from the store's in-memory tail.
//! - `mint` issues exactly the scope the prompt showed, on the principal's
//!   live task (`TaskStore::mint_from_prompt`), or answers `mint_refused`.
//! - `defer` runs `TaskStore::defer_check`: `deny` when the principal has no
//!   task that may act, otherwise `prompt` (COMP-11 §5).
//! - `open_task` and `close_task` are the human's (A-04 §3): `ec-policyd task
//!   open|close`, a client on this same socket. A close tells every peer
//!   `revoked`, so abyss drops the grants at once.
//!
//! The store has one owner, the main thread. Connection threads decode and
//! queue; a full queue stops a connection thread reading, which fills the
//! socket, which is the backpressure abyss stalls the agent on.

use ec_policy_eval::audit::{Emission, MAX_EMISSION};
use ec_policy_eval::link::{self, FromPolicyd, ToPolicyd};
use ec_policyd::tasks;
use std::sync::{Arc, Mutex, Weak};

use std::fs;
use std::io::Read;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender};

/// Emissions decoded but not yet appended, across every connection.
const QUEUE: usize = 1024;

/// How often the policy directories are checked for a change.
const WATCH: std::time::Duration = std::time::Duration::from_secs(2);

/// The signed table every connection is given, and the connections to push
/// a new one to. Shared by the connection threads and the watcher.
#[derive(Default)]
struct Push {
    /// The encoded `FromPolicyd::Table` message, once a compile has succeeded.
    table: Option<Vec<u8>>,
    version: u64,
    conns: Vec<Weak<OwnedFd>>,
}

/// Compile the policy and, on success, replace the pushed table and send it
/// to every live connection. A failed compile keeps the previous table
/// (S-02 §4) and says why.
fn recompile(push: &Mutex<Push>, key: &ed25519_dalek::SigningKey, dirs: &[PathBuf]) {
    let compiled = ec_policyd::policy::load(dirs).and_then(|f| ec_policyd::policy::compile(&f));
    let compiled = match compiled {
        Ok(c) => c,
        Err(e) => {
            eprintln!("policyd: policy not compiled, previous table stays: {e}");
            return;
        }
    };
    for n in &compiled.not_enforced {
        eprintln!("policyd: {n}");
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let Ok(mut p) = push.lock() else { return };
    // Strictly increasing within a run and, by the clock, across restarts:
    // abyss refuses a table that does not move forward (COMP-11 §2).
    let version = now_ms.max(p.version + 1);
    let table = ec_policyd::policy::table(compiled, version);
    let bytes = ec_policy_eval::table::encode(&table);
    if bytes.len() + 128 > link::MAX_MESSAGE {
        eprintln!("policyd: compiled table too large to push, previous table stays");
        return;
    }
    let sig = ec_policy_eval::table::sign(key, &bytes);
    let msg = FromPolicyd::Table { table: bytes, sig }.encode();
    p.version = version;
    p.table = Some(msg.clone());
    p.conns.retain(|c| c.strong_count() > 0);
    for c in p.conns.iter().filter_map(Weak::upgrade) {
        use rustix::net::{send, SendFlags};
        let _ = send(&*c, &msg, SendFlags::NOSIGNAL);
    }
    eprintln!(
        "policyd: table {version} pushed ({} deny, {} prompt, {} defer, {} allow)",
        table.rules.deny.len(),
        table.rules.prompt.len(),
        table.rules.defer.len(),
        table.rules.allow.len()
    );
}

fn watch(push: Arc<Mutex<Push>>, key: ed25519_dalek::SigningKey, dirs: Vec<PathBuf>) {
    let mut last = ec_policyd::policy::stamp(&dirs);
    loop {
        std::thread::sleep(WATCH);
        let now = ec_policyd::policy::stamp(&dirs);
        if now != last {
            last = now;
            recompile(&push, &key, &dirs);
        }
    }
}

/// What a connection thread hands the main thread.
enum Incoming {
    Emission(Emission),
    /// A link message, and the connection to answer it on.
    Message(ToPolicyd, Arc<OwnedFd>),
}

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
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("task") {
        return cli::task(&args[1..]);
    }
    let dir = state_dir();
    let key = match load_key(&dir) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("policyd: cannot load the issuing key: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let offer = ec_policy_eval::link::encode_key_offer(&key.verifying_key());
    // The table is compiled before anything is served, so the first
    // connection is given it right after the key (COMP-11 §2).
    let push = Arc::new(Mutex::new(Push::default()));
    let dirs = ec_policyd::policy::sources();
    recompile(&push, &key, &dirs);
    {
        let (push, key) = (push.clone(), key.clone());
        std::thread::spawn(move || watch(push, key, dirs));
    }
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
    let serving = push.clone();
    std::thread::spawn(move || {
        if let Err(e) = serve(listener, &offer, tx, serving) {
            eprintln!("policyd: policyd.sock: {e}");
            std::process::exit(1);
        }
    });
    write(&mut store, rx, &push)
}

/// Appends every queued emission. A record that cannot be written ends the
/// daemon: abyss sees the link drop and pauses every agent (COMP-01 §6),
/// which is the fail-closed answer to an audit log that has stopped taking
/// records. Continuing would let agents act unjournalled.
fn write(store: &mut tasks::TaskStore, rx: Receiver<Incoming>, push: &Mutex<Push>) -> std::process::ExitCode {
    for incoming in rx {
        match incoming {
            Incoming::Emission(e) => {
                if let Err(err) = store.record(e) {
                    eprintln!("policyd: cannot append an audit record: {err}");
                    return std::process::ExitCode::FAILURE;
                }
            }
            Incoming::Message(m, conn) => match answer(store, m, now_ms()) {
                Ok(Answer::To(reply)) => {
                    use rustix::net::{send, SendFlags};
                    // A peer that cannot take the answer has hung up; its
                    // connection thread sees that and ends.
                    let _ = send(&*conn, &reply.encode(), SendFlags::NOSIGNAL);
                }
                Ok(Answer::All(m)) => broadcast(push, &m.encode()),
                Ok(Answer::Nothing) => {}
                Err(err) => {
                    eprintln!("policyd: cannot journal a link request: {err}");
                    return std::process::ExitCode::FAILURE;
                }
            },
        }
    }
    std::process::ExitCode::FAILURE
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Sends `msg` to every live connection: abyss and any CLI on the socket.
fn broadcast(push: &Mutex<Push>, msg: &[u8]) {
    use rustix::net::{send, SendFlags};
    let Ok(mut p) = push.lock() else { return };
    p.conns.retain(|w| w.strong_count() > 0);
    for c in p.conns.iter().filter_map(Weak::upgrade) {
        let _ = send(&*c, msg, SendFlags::NOSIGNAL);
    }
}

/// Where an answer goes.
#[derive(Debug, PartialEq)]
enum Answer {
    /// Back to the connection that asked.
    To(FromPolicyd),
    /// To every connection: a change every holder of a grant must see.
    All(FromPolicyd),
    Nothing,
}

/// Whether a task error is a policy "no" (answered) rather than a journal
/// failure (fatal).
fn refusal(e: &tasks::TaskError) -> bool {
    !matches!(e, tasks::TaskError::Store(_))
}

/// The main thread's answer to one link message. An error is a journal
/// failure, which ends the daemon like a failed emission does; a policy
/// refusal is an answer.
fn answer(store: &mut tasks::TaskStore, m: ToPolicyd, now_ms: u64) -> Result<Answer, tasks::TaskError> {
    Ok(match m {
        ToPolicyd::Revoke { principal } | ToPolicyd::Terminate { principal } => {
            store.cancel_principal(&principal)?;
            Answer::To(FromPolicyd::Revoked { principal })
        }
        ToPolicyd::CloseTask { principal } => {
            if !store.cancel_principal(&principal)? {
                return Ok(Answer::Nothing);
            }
            Answer::All(FromPolicyd::Revoked { principal })
        }
        ToPolicyd::AuditTail { req, principal, n } => Answer::To(FromPolicyd::AuditRecords {
            req,
            records: store.tail(&principal, n as usize),
        }),
        ToPolicyd::Mint {
            req,
            principal,
            scope,
            unattended,
        } => match store.mint_from_prompt(&principal, &scope, unattended, now_ms) {
            Ok(grant) => Answer::To(FromPolicyd::Minted { req, grant }),
            Err(e) if refusal(&e) => {
                eprintln!("policyd: mint for {principal} refused: {e}");
                Answer::To(FromPolicyd::MintRefused { req })
            }
            Err(e) => return Err(e),
        },
        ToPolicyd::Defer { req, principal, .. } => Answer::To(FromPolicyd::DeferAnswer {
            req,
            answer: store.defer_check(&principal),
        }),
        ToPolicyd::OpenTask {
            req,
            principal,
            statement,
            deadline_ms,
            scope,
        } => match store.open_for_human(&principal, &statement, deadline_ms, &scope, now_ms) {
            Ok(grant) => Answer::To(FromPolicyd::TaskOpened { req, grant }),
            Err(e) if refusal(&e) => Answer::To(FromPolicyd::TaskRefused {
                req,
                reason: e.to_string(),
            }),
            Err(e) => return Err(e),
        },
    })
}

/// `$ECLIPSE_POLICYD_SOCKET`, else `$XDG_RUNTIME_DIR/eclipse/policyd.sock`.
mod cli;

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

fn serve(
    listener: OwnedFd,
    offer: &[u8],
    tx: SyncSender<Incoming>,
    push: Arc<Mutex<Push>>,
) -> std::io::Result<()> {
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
        let push = push.clone();
        std::thread::spawn(move || hold(conn, &offer, &tx, &push));
    }
}

/// Offers the key, then queues every emission the peer sends until it hangs
/// up. A packet that is oversized, malformed, or of a kind the compositor
/// does not emit (COMP-12 §2) ends the connection: a source whose stream
/// cannot be trusted is cut off, not partly believed.
fn hold(conn: OwnedFd, offer: &[u8], tx: &SyncSender<Incoming>, push: &Mutex<Push>) {
    use rustix::net::{recv, send, RecvFlags, SendFlags};
    if send(&conn, offer, SendFlags::NOSIGNAL).is_err() {
        return;
    }
    let conn = Arc::new(conn);
    // Registered and given the current table under one lock, so a recompile
    // can neither miss this connection nor send it an older table second.
    if let Ok(mut p) = push.lock() {
        p.conns.push(Arc::downgrade(&conn));
        if let Some(t) = &p.table {
            if send(&*conn, t, SendFlags::NOSIGNAL).is_err() {
                return;
            }
        }
    }
    let mut buf = vec![0u8; MAX_EMISSION];
    loop {
        // With TRUNC the second length is the packet's, not what fit.
        let n = match recv(&*conn, &mut buf[..], RecvFlags::TRUNC) {
            Ok((_, 0)) => return,
            Ok((_, len)) if len > MAX_EMISSION => {
                eprintln!("policyd: oversized audit emission; dropping the peer");
                return;
            }
            Ok((n, _)) => n,
            Err(rustix::io::Errno::INTR) => continue,
            Err(_) => return,
        };
        let incoming = match accept(&buf[..n]) {
            Some(Accepted::Message(m)) => Incoming::Message(m, conn.clone()),
            Some(Accepted::Emission(e)) => Incoming::Emission(e),
            None => {
                eprintln!("policyd: malformed audit emission or link message; dropping the peer");
                return;
            }
        };
        if tx.send(incoming).is_err() {
            return;
        }
    }
}

/// One packet from the peer, decoded.
#[derive(Debug, PartialEq)]
enum Accepted {
    Emission(Emission),
    Message(ToPolicyd),
}

/// An emission `policyd` will store from this peer, or a link message.
fn accept(packet: &[u8]) -> Option<Accepted> {
    if link::is_message(packet) {
        return ToPolicyd::decode(packet).ok().map(Accepted::Message);
    }
    Emission::decode(packet)
        .ok()
        .filter(|e| e.kind.from_compositor())
        .map(Accepted::Emission)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_policy_eval::audit::Kind;
    use ec_policy_eval::cbor::enc;

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
        assert_eq!(accept(&e.encode()), Some(Accepted::Emission(e)));
    }

    #[test]
    fn a_kind_policyd_owns_is_refused_from_the_socket() {
        for k in [Kind::Grant, Kind::Revoke, Kind::Task, Kind::Anchor] {
            assert_eq!(accept(&emission(k).encode()), None, "{k:?}");
        }
    }

    #[test]
    fn a_link_message_is_accepted_and_a_malformed_one_refused() {
        let m = ToPolicyd::Terminate {
            principal: "agent:a".into(),
        };
        assert_eq!(accept(&m.encode()), Some(Accepted::Message(m)));
        let mut bad = ToPolicyd::Terminate {
            principal: "agent:a".into(),
        }
        .encode();
        bad.push(0);
        assert_eq!(accept(&bad), None);
    }

    #[test]
    fn garbage_is_refused() {
        assert_eq!(accept(&[0xff, 0x00]), None);
        assert_eq!(accept(&[]), None);
    }
}
