// SPDX-License-Identifier: AGPL-3.0-only
//! The armed askpass channel, instance side (T-22 §3, SECURITY-30).
//!
//! Arming makes a 256-bit one-time token and a dedicated listening socket
//! `$XDG_RUNTIME_DIR/apotheosis/askpass-<random>.sock` (the T-12 §1 `0700` directory,
//! socket `0600`), separate from the JSON control socket. One thread per armed ssh process
//! owns the socket and the process's pidfd. A connection is accepted only when its uid is
//! ours, its process ancestry contains the armed process, its token matches, and no other
//! request is pending; anything else is answered "refused" (the client exits 3) and no sheet
//! is shown. When the armed process exits the socket is unlinked and the token wiped.
//!
//! The sheet's answer comes back over a channel and is written to the askpass client only;
//! secret buffers are wiped when dropped. Nothing here logs a token, prompt or secret: pids,
//! sizes and outcomes only.

use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apoth_proto::askpass::{
    self as wire, Kind, MAX_REQUEST, MAX_SECRET, Status, TOKEN_BYTES, decode_request, token_eq,
    wipe,
};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::process::{Pid, PidfdFlags, getuid, pidfd_open};

/// Ancestors walked from the askpass client looking for the armed process.
const MAX_ANCESTRY: usize = 64;
/// The client must send its whole request within this.
const REQUEST_WITHIN: Duration = Duration::from_secs(5);
/// Armed channels at once (one per live `apoth ssh` / mux link).
pub(crate) const MAX_ARMED: usize = 64;

static NEXT_ASK: AtomicU64 = AtomicU64::new(1);
static ARMED: AtomicU64 = AtomicU64::new(0);

/// A secret on its way to the askpass client. Wiped on drop; `Debug` shows the length.
pub(crate) struct Secret(Vec<u8>);

impl Secret {
    /// An empty secret with room for the whole budget, so typing never reallocates (a
    /// reallocation would leave an unwiped copy behind).
    pub(crate) fn new() -> Secret {
        Secret(Vec::with_capacity(MAX_SECRET))
    }

    /// Append typed text; false (and unchanged) past the budget.
    pub(crate) fn push_str(&mut self, s: &str) -> bool {
        if self.0.len() + s.len() > MAX_SECRET {
            return false;
        }
        self.0.extend_from_slice(s.as_bytes());
        true
    }

    /// Drop the last character.
    pub(crate) fn pop(&mut self) {
        let Ok(s) = std::str::from_utf8(&self.0) else {
            self.0.clear();
            return;
        };
        let cut = s.char_indices().last().map_or(0, |(i, _)| i);
        wipe(&mut self.0[cut..]);
        self.0.truncate(cut);
    }

    /// Characters typed (for the masked field).
    pub(crate) fn chars(&self) -> usize {
        std::str::from_utf8(&self.0).map_or(0, |s| s.chars().count())
    }

    fn bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        // Wipe the whole allocation, not only the live part.
        self.0.resize(self.0.capacity(), 0);
        wipe(&mut self.0);
        self.0.clear();
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secret({} bytes)", self.0.len())
    }
}

/// The sheet's answer.
#[derive(Debug)]
pub(crate) enum Answer {
    /// Submitted (for confirm / none prompts: allowed, empty).
    Secret(Secret),
    Cancel,
}

/// A request the sheet should show.
#[derive(Debug)]
pub(crate) struct Ask {
    pub(crate) id: u64,
    /// The armed `apoth ssh` destination (interface text, from the arming peer's request).
    pub(crate) host: String,
    pub(crate) kind: Kind,
    /// The remote's prompt: program text, sanitised by the sheet.
    pub(crate) prompt: String,
    pub(crate) answer: mpsc::Sender<Answer>,
}

/// What the broker tells main.
#[derive(Debug)]
pub(crate) enum Event {
    Ask(Ask),
    /// The request `id` ended without an answer (the client or the armed ssh went away).
    Withdrawn(u64),
}

/// Where events go (main's channel in the binary, a test's in tests).
pub(crate) type Sink = Box<dyn Fn(Event) + Send>;

fn random<const N: usize>() -> io::Result<[u8; N]> {
    let mut b = [0u8; N];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b)
}

/// An armed channel before its process is known (the ssh child needs the environment
/// before it exists; nothing is accepted until [`Prepared::start`]).
pub(crate) struct Prepared {
    listener: UnixListener,
    path: PathBuf,
    token: [u8; TOKEN_BYTES],
    host: String,
}

impl std::fmt::Debug for Prepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prepared").finish_non_exhaustive()
    }
}

impl Drop for Prepared {
    fn drop(&mut self) {
        wipe(&mut self.token);
    }
}

impl Prepared {
    /// Make the token and bind the socket under `dir` (the T-12 §1 directory).
    pub(crate) fn new(dir: &Path, host: &str) -> io::Result<Prepared> {
        if ARMED.load(Ordering::Acquire) >= MAX_ARMED as u64 {
            return Err(io::Error::other("too many armed askpass channels"));
        }
        let name = wire::token_hex(&{
            let mut t = [0u8; TOKEN_BYTES];
            t[..8].copy_from_slice(&random::<8>()?);
            t
        });
        let name = format!(
            "askpass-{}",
            std::str::from_utf8(&name[..16]).unwrap_or("0")
        );
        let (listener, path) = crate::ctl::path::bind(dir, &name)?;
        Ok(Prepared {
            listener,
            path,
            token: random::<TOKEN_BYTES>()?,
            host: host.chars().take(wire::MAX_HOST).collect(),
        })
    }

    pub(crate) fn socket(&self) -> &Path {
        &self.path
    }

    /// The token for the ssh child's environment (or the arming reply). Never log it.
    pub(crate) fn token_hex(&self) -> String {
        let h = wire::token_hex(&self.token);
        String::from_utf8_lossy(&h).into_owned()
    }

    /// Bind the channel to process `pid` and serve it on its own thread until that process
    /// exits.
    pub(crate) fn start(self, pid: i32, sink: Sink) -> io::Result<()> {
        let pidfd = pidfd_open(
            Pid::from_raw(pid).ok_or_else(|| io::Error::other("bad pid"))?,
            PidfdFlags::empty(),
        )?;
        ARMED.fetch_add(1, Ordering::AcqRel);
        let r = std::thread::Builder::new()
            .name("apoth-askpass".into())
            .spawn(move || {
                let me = self;
                serve(&me, pid, &pidfd, &sink);
                let _ = std::fs::remove_file(&me.path);
                ARMED.fetch_sub(1, Ordering::AcqRel);
                tracing::debug!(pid, "askpass disarmed");
            });
        if r.is_err() {
            ARMED.fetch_sub(1, Ordering::AcqRel);
        }
        r.map(|_| ())
    }
}

fn readable(fd: &impl std::os::fd::AsFd, ms: i64) -> bool {
    let ts = Timespec {
        tv_sec: ms / 1000,
        tv_nsec: (ms % 1000) * 1_000_000,
    };
    let mut fds = [PollFd::new(fd, PollFlags::IN)];
    matches!(poll(&mut fds, Some(&ts)), Ok(n) if n > 0)
}

/// `pid`'s ancestry (itself first) contains `armed`.
pub(crate) fn descends_from(pid: i32, armed: i32, stat: impl Fn(i32) -> Option<i32>) -> bool {
    let mut cur = pid;
    for _ in 0..MAX_ANCESTRY {
        if cur == armed {
            return true;
        }
        match stat(cur) {
            Some(pp) if pp > 1 => cur = pp,
            _ => return false,
        }
    }
    false
}

fn ppid(pid: i32) -> Option<i32> {
    let b = std::fs::read(format!("/proc/{pid}/stat")).ok()?;
    apoth_pty::parse_stat(&b).map(|s| s.ppid)
}

fn refuse(mut s: UnixStream, why: &'static str) {
    tracing::info!(why, "askpass request refused");
    let _ = s.write_all(&[Status::Refused as u8]);
}

fn serve(p: &Prepared, armed: i32, pidfd: &OwnedFd, sink: &Sink) {
    loop {
        let mut fds = [
            PollFd::new(&p.listener, PollFlags::IN),
            PollFd::new(pidfd, PollFlags::IN),
        ];
        match poll(&mut fds, None) {
            Ok(_) | Err(rustix::io::Errno::INTR) => {}
            Err(_) => return,
        }
        if !fds[1].revents().is_empty() {
            return;
        }
        if fds[0].revents().contains(PollFlags::IN) {
            while let Ok((s, _)) = p.listener.accept() {
                if !handle(p, armed, pidfd, s, sink) {
                    return;
                }
            }
        }
    }
}

/// One connection. False once the armed process is gone.
fn handle(p: &Prepared, armed: i32, pidfd: &OwnedFd, s: UnixStream, sink: &Sink) -> bool {
    let Ok(cred) = rustix::net::sockopt::socket_peercred(&s) else {
        return true;
    };
    if cred.uid != getuid() {
        refuse(s, "another uid");
        return true;
    }
    let peer = cred.pid.as_raw_nonzero().get();
    // Hold the peer by pidfd while its ancestry is read, so a recycled pid cannot pass.
    let Ok(peer_fd) = pidfd_open(cred.pid, PidfdFlags::empty()) else {
        refuse(s, "peer gone");
        return true;
    };
    let ok = descends_from(peer, armed, ppid) && !readable(&peer_fd, 0);
    if !ok {
        refuse(s, "not a descendant of the armed ssh");
        return true;
    }
    if s.set_nonblocking(false).is_err()
        || s.set_read_timeout(Some(Duration::from_millis(200)))
            .is_err()
    {
        return true;
    }
    let mut req = Vec::with_capacity(1024);
    let until = Instant::now() + REQUEST_WITHIN;
    let mut buf = [0u8; 4096];
    let mut s = s;
    loop {
        if Instant::now() > until || req.len() > MAX_REQUEST {
            refuse(s, "malformed request");
            return true;
        }
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => req.extend_from_slice(&buf[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return true,
        }
    }
    let Ok(r) = decode_request(&req) else {
        refuse(s, "malformed request");
        return true;
    };
    let mine = wire::token_hex(&p.token);
    if !token_eq(r.token, &mine) {
        refuse(s, "bad token");
        return true;
    }
    let id = NEXT_ASK.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel();
    tracing::info!(pid = peer, ask = id, "askpass request accepted");
    sink(Event::Ask(Ask {
        id,
        host: p.host.clone(),
        kind: r.kind,
        prompt: String::from_utf8_lossy(r.prompt).into_owned(),
        answer: tx,
    }));
    drop(req);
    // Wait for the sheet; refuse every other request meanwhile (one sheet at a time).
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Answer::Secret(secret)) => {
                let mut out = Vec::with_capacity(1 + MAX_SECRET);
                out.push(Status::Ok as u8);
                out.extend_from_slice(secret.bytes());
                drop(secret);
                let _ = s.write_all(&out);
                wipe(&mut out);
                tracing::info!(ask = id, "askpass answered");
                return true;
            }
            Ok(Answer::Cancel) => {
                let _ = s.write_all(&[Status::Cancelled as u8]);
                tracing::info!(ask = id, "askpass cancelled");
                return true;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = s.write_all(&[Status::Cancelled as u8]);
                return true;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if readable(pidfd, 0) {
            sink(Event::Withdrawn(id));
            return false;
        }
        // The client hung up (ssh gave up): the sheet goes.
        if readable(&peer_fd, 0) {
            sink(Event::Withdrawn(id));
            return true;
        }
        while let Ok((other, _)) = p.listener.accept() {
            refuse(other, "another request is pending");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("apask{tag}{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// The askpass client, as `apoth askpass` frames it.
    fn ask(sock: &Path, token: &[u8], prompt: &str) -> (Option<u8>, Vec<u8>) {
        let Ok(mut s) = UnixStream::connect(sock) else {
            return (None, Vec::new());
        };
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let _ = s.write_all(&wire::encode_request(
            Kind::Secret,
            token,
            prompt.as_bytes(),
        ));
        let _ = s.shutdown(std::net::Shutdown::Write);
        let mut b = Vec::new();
        let _ = s.read_to_end(&mut b);
        (b.first().copied(), b.get(1..).unwrap_or_default().to_vec())
    }

    fn sink() -> (Sink, mpsc::Receiver<Event>) {
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        (
            Box::new(move |e| {
                let _ = tx.lock().unwrap().send(e);
            }),
            rx,
        )
    }

    #[test]
    fn ancestry_walk() {
        let parents = |p: i32| match p {
            10 => Some(5),
            5 => Some(2),
            2 => Some(1),
            _ => None,
        };
        assert!(descends_from(10, 5, parents));
        assert!(descends_from(5, 5, parents));
        assert!(!descends_from(10, 7, parents));
        assert!(!descends_from(99, 5, parents));
        // A cycle cannot loop forever.
        assert!(!descends_from(3, 9, |_| Some(3)));
    }

    #[test]
    fn secret_buffer_is_bounded_and_edits_by_character() {
        let mut s = Secret::new();
        assert!(s.push_str("hé"));
        assert_eq!(s.chars(), 2);
        s.pop();
        assert_eq!(s.bytes(), b"h");
        assert!(!s.push_str(&"x".repeat(MAX_SECRET)));
        assert_eq!(format!("{s:?}"), "Secret(1 bytes)");
        let cap = s.0.capacity();
        assert!(s.push_str(&"x".repeat(MAX_SECRET - 1)));
        assert_eq!(s.0.capacity(), cap, "typing never reallocates");
    }

    #[test]
    fn round_trip_for_a_descendant_and_refusals_otherwise() {
        let d = dir("rt");
        // Armed for this test process: the test itself is a descendant (itself).
        let p = Prepared::new(&d, "me@box").unwrap();
        let sock = p.socket().to_owned();
        let token = p.token_hex();
        let md = std::fs::metadata(&sock).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(md.permissions().mode() & 0o777, 0o600);
        let (s, rx) = sink();
        p.start(std::process::id() as i32, s).unwrap();

        // No (or a wrong) token: refused, no sheet.
        let bad = "0".repeat(64);
        assert_eq!(
            ask(&sock, bad.as_bytes(), "pw:").0,
            Some(Status::Refused as u8)
        );
        assert_eq!(ask(&sock, b"short", "pw:").0, Some(Status::Refused as u8));
        assert!(rx.try_recv().is_err());

        // The right token: the sheet is asked, its answer reaches the client.
        let t2 = token.clone();
        let s2 = sock.clone();
        let client = std::thread::spawn(move || ask(&s2, t2.as_bytes(), "pw for me@box:"));
        let Event::Ask(a) = rx.recv_timeout(Duration::from_secs(10)).unwrap() else {
            panic!("expected an ask");
        };
        assert_eq!(a.host, "me@box");
        assert_eq!(a.prompt, "pw for me@box:");
        // A second request while one is pending is refused.
        assert_eq!(
            ask(&sock, token.as_bytes(), "again").0,
            Some(Status::Refused as u8)
        );
        let mut sec = Secret::new();
        sec.push_str("hunter2");
        a.answer.send(Answer::Secret(sec)).unwrap();
        let (st, body) = client.join().unwrap();
        assert_eq!(st, Some(Status::Ok as u8));
        assert_eq!(body, b"hunter2");

        // Cancel.
        let t3 = token.clone();
        let s3 = sock.clone();
        let client = std::thread::spawn(move || ask(&s3, t3.as_bytes(), "pw:"));
        let Event::Ask(a) = rx.recv_timeout(Duration::from_secs(10)).unwrap() else {
            panic!("expected an ask");
        };
        a.answer.send(Answer::Cancel).unwrap();
        assert_eq!(client.join().unwrap().0, Some(Status::Cancelled as u8));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_process_outside_the_armed_tree_is_refused_and_exit_disarms() {
        let d = dir("tree");
        let p = Prepared::new(&d, "h").unwrap();
        let sock = p.socket().to_owned();
        let token = p.token_hex();
        // Armed for a child; this test process is its parent, not a descendant.
        let mut child = Command::new("sleep")
            .arg("2")
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let (s, rx) = sink();
        p.start(child.id() as i32, s).unwrap();
        assert_eq!(
            ask(&sock, token.as_bytes(), "pw:").0,
            Some(Status::Refused as u8)
        );
        assert!(rx.try_recv().is_err());
        // The armed process exits: socket unlinked, token gone.
        let _ = child.kill();
        let _ = child.wait();
        let t0 = Instant::now();
        while sock.exists() {
            assert!(t0.elapsed() < Duration::from_secs(5), "socket not unlinked");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(ask(&sock, token.as_bytes(), "pw:").0, None);
        let _ = std::fs::remove_dir_all(&d);
    }
}
