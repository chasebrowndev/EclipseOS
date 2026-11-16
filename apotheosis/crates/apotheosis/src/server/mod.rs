// SPDX-License-Identifier: AGPL-3.0-only
//! The session daemon (T-21 §3, ADR 0123): `apotheosis --server` owns the PTYs, the
//! parsers and the grids of one session, so the panes outlive the GUI and the compositor.
//! Views attach over `$XDG_RUNTIME_DIR/apotheosis/server-<session>.sock` (T-12 §1 rules:
//! `0700` directory, `0600` socket, same uid) and speak the `apoth_proto::mux` records
//! (T-21 §4).
//!
//! One thread, one `poll` loop: the listener, every pane's master and pidfd, every view's
//! stream. A view first gets each granted pane as a snapshot taken at a ground point
//! (`Term::snapshot`), then the pane's PTY bytes in order with sequence numbers, so feeding
//! them to the restored term reproduces the server's state exactly. A view whose output
//! backs up past [`HIGH_WATER`] stops getting the tee; once its socket drains it is sent
//! fresh snapshots (T-21 §3 "slow view").
//!
//! The server answers every reply itself (DA, DSR, ...); views discard theirs. Sequences
//! that need a view to answer or show something the server cannot (clipboard reads,
//! graphics) are denied here and so in every view (the deny word rides in the snapshot).
//!
//! Lifetime (the tmux model, owner decision 2026-11-14): closing a window detaches it; the
//! server keeps running with no view attached and exits when its last pane exits, removing
//! its socket. `KillSession` (`apoth server kill`, the `session.kill` action) closes every
//! pane with the HUP, TERM, KILL escalation, so the server then ends. Nothing here logs pane content,
//! input, command lines or working directories.

pub(crate) mod auth;

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use apoth_proto::mux::{
    self, Decoded, MAX_CHUNK, MUX_MAJOR, MUX_MINOR, PaneEntry, RecordDecoder, ServerMsg, ViewMsg,
};
use apoth_pty::{EnvSpec, ExitStatus, Pty, PtySize, SpawnConfig, build_env, choose_term};
use apoth_term::{SnapError, Term};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::Errno;
use rustix::process::Signal;

use self::auth::Grant;
use crate::ctl::peer::{self, Peer, Refused};
use crate::settings::{Settings, resolve_shell};

/// The session a bare `--server` / `--attach` names.
pub(crate) const DEFAULT_SESSION: &str = "default";
/// Largest record a view may send (input chunks and spawns, never pages).
const VIEW_RECORD_LIMIT: usize = 4 << 20;
/// Bytes queued to one view before it is "slow" and stops getting the tee.
pub(crate) const HIGH_WATER: usize = 8 << 20;
/// Connections at once (views and listers).
const MAX_VIEWS: usize = 32;
/// A connection must say `Hello` within this.
const HELLO_WITHIN: Duration = Duration::from_secs(5);
/// How long a pending snapshot waits for the pane to reach a ground point before the
/// server cancels the sequence in flight (a CAN, fed and teed like output).
const GROUND_WAIT: Duration = Duration::from_secs(1);
/// A view's input is not read while a pane's PTY has this much unwritten (back-pressure
/// to the view's socket instead of dropping keys or paste).
const INPUT_BACKLOG: usize = 256 << 10;
/// PTY reads per pane per loop turn (fairness between panes and views).
const READS_PER_TURN: usize = 4;
/// Grace between the close signals (T-18 §7, SECURITY-28).
const CLOSE_STEP: Duration = Duration::from_secs(1);

/// How the server sizes a pane several views ask different sizes for (T-21 §3):
/// `sessions.resize` (`Settings::resize_policy`), default `latest`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResizePolicy {
    Smallest,
    Latest,
    Largest,
}

/// The size a pane takes: `asks` are the attached views' current requests, `latest` the
/// one just received.
#[must_use]
pub(crate) fn pick_size(
    policy: ResizePolicy,
    asks: &[(u16, u16)],
    latest: Option<(u16, u16)>,
) -> Option<(u16, u16)> {
    match policy {
        ResizePolicy::Latest => latest.or_else(|| asks.last().copied()),
        ResizePolicy::Smallest => asks
            .iter()
            .copied()
            .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1))),
        ResizePolicy::Largest => asks
            .iter()
            .copied()
            .reduce(|a, b| (a.0.max(b.0), a.1.max(b.1))),
    }
}

/// A session name: 1-64 of `[A-Za-z0-9._-]`, not starting with a dot.
#[must_use]
pub(crate) fn valid_session(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && !s.starts_with('.')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// `$XDG_RUNTIME_DIR/apotheosis/server-<session>.sock`.
#[must_use]
pub(crate) fn socket_path(session: &str) -> Option<PathBuf> {
    let rt = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    crate::ctl::path::socket_dir(rt.as_deref()).map(|d| d.join(format!("server-{session}.sock")))
}

/// What `--server` was started with.
#[derive(Debug, Default)]
pub(crate) struct Options {
    pub(crate) session: String,
    pub(crate) command: Option<Vec<String>>,
    pub(crate) cwd: Option<PathBuf>,
}

/// Run the daemon until its last pane exits. Returns the process exit code.
pub(crate) fn run(settings: Settings, interpreters: Vec<String>, opts: Options) -> i32 {
    if !valid_session(&opts.session) {
        eprintln!("apotheosis: session names are 1-64 of A-Z a-z 0-9 . _ -");
        return 64;
    }
    // Leave the launching terminal's session: its hang-up must not end ours.
    let _ = rustix::process::setsid();
    let rt = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    let Some(dir) = crate::ctl::path::socket_dir(rt.as_deref()) else {
        eprintln!("apotheosis: XDG_RUNTIME_DIR is not set; the session server needs it");
        return 1;
    };
    let (listener, path) = match crate::ctl::path::bind(&dir, &format!("server-{}", opts.session)) {
        Ok(x) => x,
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
            eprintln!(
                "apotheosis: a server for session {:?} is already running",
                opts.session
            );
            return 1;
        }
        Err(e) => {
            eprintln!("apotheosis: cannot create the session socket: {e}");
            return 1;
        }
    };
    let mut s = Server::new(settings, interpreters, listener, opts.session);
    let (rows, cols) = s.settings.initial_size;
    let size = (
        rows.min(u16::MAX as usize) as u16,
        cols.min(u16::MAX as usize) as u16,
    );
    if let Err(e) = s.spawn(opts.command.unwrap_or_default(), opts.cwd, size) {
        eprintln!("apotheosis: {e}");
        let _ = std::fs::remove_file(&path);
        return 1;
    }
    tracing::info!(session = %s.session, "session server started");
    s.run();
    let _ = std::fs::remove_file(&path);
    tracing::info!(session = %s.session, "session server stopped: no panes left");
    0
}

/// One pane of the session.
struct SPane {
    id: u64,
    pty: Pty,
    term: Term,
    /// Sequence number of the next `Tee` / `Resized` record.
    seq: u64,
    rows: u16,
    cols: u16,
    cell: (u32, u32),
    /// Views waiting for this pane's snapshot.
    pending: Vec<u64>,
    /// When the first pending snapshot found the pane mid-sequence.
    not_ground: Option<Instant>,
    /// Size requests of attached views, oldest first.
    asks: Vec<(u64, u16, u16)>,
    /// Close escalation: when the last signal went, and which (0 HUP, 1 TERM, 2 KILL).
    closing: Option<(Instant, u8)>,
    /// The master hung up or read failed: no more output.
    eof: bool,
    /// The child was reaped.
    status: Option<ExitStatus>,
}

/// One connection.
struct View {
    id: u64,
    stream: UnixStream,
    dec: RecordDecoder,
    out: Vec<u8>,
    off: usize,
    peer: Peer,
    since: Instant,
    hello: bool,
    attached: bool,
    grant: Grant,
    /// Backed up: no tee until `out` drains, then fresh snapshots.
    stale: bool,
    /// Refused or detached: nothing more is read; closed once `out` drains.
    closing: bool,
    dead: bool,
}

impl View {
    fn queued(&self) -> usize {
        self.out.len() - self.off
    }

    fn push(&mut self, rec: &[u8]) {
        if self.off > 0 && self.off == self.out.len() {
            self.out.clear();
            self.off = 0;
        } else if self.off > 1 << 20 {
            self.out.drain(..self.off);
            self.off = 0;
        }
        self.out.extend_from_slice(rec);
    }

    fn send(&mut self, m: &ServerMsg) {
        self.push(&m.encode());
    }

    fn refuse(&mut self, reason: &str) {
        self.send(&ServerMsg::Refused {
            reason: reason.to_owned(),
        });
        self.closing = true;
    }

    /// Receives this pane's content.
    fn sees(&self) -> bool {
        self.attached && !self.closing && !self.dead && !matches!(self.grant, Grant::None(_))
    }

    fn flush(&mut self) {
        while self.off < self.out.len() {
            match self.stream.write(&self.out[self.off..]) {
                Ok(0) => {
                    self.dead = true;
                    return;
                }
                Ok(n) => self.off += n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => {
                    self.dead = true;
                    return;
                }
            }
        }
        self.out.clear();
        self.off = 0;
    }
}

struct Server {
    settings: Settings,
    interpreters: Vec<String>,
    deny: u32,
    listener: UnixListener,
    session: String,
    allowed: Vec<PathBuf>,
    /// The installed `apoth`, pre-granted while it runs `apoth proxy` (T-22 §5).
    proxy: Option<PathBuf>,
    panes: Vec<SPane>,
    views: Vec<View>,
    next_pane: u64,
    next_view: u64,
    buf: Vec<u8>,
    /// `KillSession` arrived: every pane is closing, nothing new starts.
    killing: bool,
}

/// What a poll turn saw.
#[derive(Default)]
struct Ready {
    listener: bool,
    /// (pane index, master readable or hung up, master writable, pidfd readable)
    panes: Vec<(bool, bool, bool)>,
    /// (view index, readable or hung up, writable)
    views: Vec<(bool, bool)>,
}

fn timespec(d: Duration) -> Timespec {
    Timespec {
        tv_sec: i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        tv_nsec: i64::from(d.subsec_nanos()),
    }
}

/// The exit status as a number (a signal is 128 + its number, as shells report it).
fn status_code(s: Option<ExitStatus>) -> Option<i32> {
    match s? {
        ExitStatus::Code(c) => Some(c),
        ExitStatus::Signal(n) => Some(128 + n),
    }
}

impl Server {
    fn new(
        settings: Settings,
        interpreters: Vec<String>,
        listener: UnixListener,
        session: String,
    ) -> Server {
        // The view-side deny word (T-18 §3) plus what no view can answer through the server.
        let mut deny = settings.nav.policy.deny_word(false);
        deny |= apoth_term::deny::GRAPHICS_DIRECT
            | apoth_term::deny::GRAPHICS_FILE
            | apoth_term::deny::CLIPBOARD_READ;
        deny &= !apoth_term::deny::GRAPHICS_FILE_ASK;
        Server {
            settings,
            interpreters,
            deny,
            listener,
            session,
            allowed: auth::pre_granted(),
            proxy: auth::proxy_exe(),
            panes: Vec::new(),
            views: Vec::new(),
            next_pane: 1,
            next_view: 1,
            buf: vec![0; MAX_CHUNK],
            killing: false,
        }
    }

    /// Start a pane: `argv` empty = the configured shell. Nothing is logged but sizes.
    fn spawn(
        &mut self,
        argv: Vec<String>,
        cwd: Option<PathBuf>,
        (rows, cols): (u16, u16),
    ) -> Result<u64, String> {
        let rows = rows.max(1);
        let cols = cols.max(1);
        let id = self.next_pane;
        let cell = self.settings.term.cell_px;
        let mut tc = self.settings.term.clone();
        tc.rows = usize::from(rows);
        tc.cols = usize::from(cols);
        let mut term = Term::new(tc);
        term.set_deny(self.deny);
        let (argv, login): (Vec<OsString>, bool) = if argv.is_empty() {
            let mut v = vec![OsString::from(resolve_shell(
                self.settings.shell.program.as_deref(),
            ))];
            v.extend(self.settings.shell.args.iter().map(OsString::from));
            (v, self.settings.shell.login)
        } else {
            (argv.into_iter().map(OsString::from).collect(), false)
        };
        let term_name = if self.settings.term_name == "xterm-apotheosis" {
            choose_term(false, &apoth_pty::terminfo_dirs(&|k| std::env::var_os(k))).to_owned()
        } else {
            self.settings.term_name.clone()
        };
        // No APOTHEOSIS_SOCKET / _PANE / _WINDOW: those name a GUI instance's control
        // socket, and a server pane belongs to no instance (reported).
        let mut extra: Vec<(OsString, OsString)> = vec![(
            "APOTHEOSIS_VERSION".into(),
            env!("CARGO_PKG_VERSION").into(),
        )];
        extra.extend(
            self.settings
                .shell
                .env
                .iter()
                .map(|(k, v)| (OsString::from(k), OsString::from(v))),
        );
        let env = build_env(
            std::env::vars_os(),
            &EnvSpec {
                term: &term_name,
                version: env!("CARGO_PKG_VERSION"),
                extra,
            },
        );
        let pty = Pty::spawn(&SpawnConfig {
            argv,
            login,
            cwd: cwd.or_else(|| self.settings.shell.cwd.clone()),
            env,
            size: PtySize::from_cells(rows, cols, cell.0, cell.1),
            iutf8: true,
        })
        .map_err(|e| format!("cannot start the shell: {e}"))?;
        self.next_pane += 1;
        tracing::debug!(pane = id, rows, cols, "server pane started");
        self.panes.push(SPane {
            id,
            pty,
            term,
            seq: 0,
            rows,
            cols,
            cell,
            pending: Vec::new(),
            not_ground: None,
            asks: Vec::new(),
            closing: None,
            eof: false,
            status: None,
        });
        // Every attached view gets the new pane (grants are evaluated per pane, T-21 §3).
        let p = self.panes.last_mut().expect("just pushed");
        for v in &self.views {
            if v.sees() {
                p.pending.push(v.id);
            }
        }
        Ok(id)
    }

    fn pane_list(&self) -> Vec<PaneEntry> {
        let views = self.views.iter().filter(|v| v.sees()).count() as u32;
        self.panes
            .iter()
            .map(|p| PaneEntry {
                pane: p.id,
                rows: p.rows,
                cols: p.cols,
                alive: p.status.is_none(),
                views,
            })
            .collect()
    }

    fn run(&mut self) {
        while !self.panes.is_empty() {
            let ready = match self.poll() {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(errno = e.raw_os_error(), "server poll failed");
                    break;
                }
            };
            if ready.listener {
                self.accept();
            }
            for (i, &(r, w, x)) in ready.panes.iter().enumerate() {
                if w && let Err(e) = self.panes[i].pty.drain_out() {
                    tracing::debug!(error = %e, pane = self.panes[i].id, "pty write failed");
                }
                if r {
                    self.read_pane(i, READS_PER_TURN);
                }
                if x {
                    self.child_exited(i);
                }
            }
            for (i, &(r, w)) in ready.views.iter().enumerate() {
                if r {
                    self.read_view(i);
                }
                if w {
                    self.views[i].flush();
                }
            }
            self.timers();
            self.snapshots();
            self.reap();
        }
        // Let the last `Exited` records reach the views (bounded).
        let until = Instant::now() + Duration::from_millis(500);
        while Instant::now() < until && self.views.iter().any(|v| !v.dead && v.queued() > 0) {
            for v in &mut self.views {
                v.flush();
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn deadline(&self) -> Option<Instant> {
        let mut d: Option<Instant> = None;
        let mut min = |t: Instant| d = Some(d.map_or(t, |d| d.min(t)));
        for p in &self.panes {
            if let Some((at, stage)) = p.closing
                && stage < 2
            {
                min(at + CLOSE_STEP);
            }
            if let Some(t) = p.not_ground {
                min(t + GROUND_WAIT);
            }
            if let Some(t) = p.term.next_deadline() {
                min(t);
            }
        }
        for v in &self.views {
            if !v.hello && !v.closing {
                min(v.since + HELLO_WITHIN);
            }
        }
        d
    }

    fn poll(&mut self) -> Result<Ready, Errno> {
        let timeout = self
            .deadline()
            .map(|d| timespec(d.saturating_duration_since(Instant::now())));
        let backlog = self
            .panes
            .iter()
            .any(|p| p.pty.pending_out_bytes() > INPUT_BACKLOG);
        let mut fds: Vec<PollFd<'_>> =
            Vec::with_capacity(1 + 2 * self.panes.len() + self.views.len());
        fds.push(PollFd::new(&self.listener, PollFlags::IN));
        for p in &self.panes {
            let mut f = PollFlags::empty();
            if !p.eof {
                f |= PollFlags::IN;
            }
            if p.pty.wants_pollout() {
                f |= PollFlags::OUT;
            }
            fds.push(PollFd::from_borrowed_fd(p.pty.master_fd(), f));
            let x = if p.status.is_none() {
                PollFlags::IN
            } else {
                PollFlags::empty()
            };
            fds.push(PollFd::from_borrowed_fd(p.pty.pidfd(), x));
        }
        for v in &self.views {
            let mut f = PollFlags::empty();
            if !v.closing && !(backlog && v.attached) {
                f |= PollFlags::IN;
            }
            if v.queued() > 0 {
                f |= PollFlags::OUT;
            }
            fds.push(PollFd::new(&v.stream, f));
        }
        loop {
            match poll(&mut fds, timeout.as_ref()) {
                Ok(_) => break,
                Err(Errno::INTR) => {}
                Err(e) => return Err(e),
            }
        }
        let hup = PollFlags::HUP | PollFlags::ERR;
        let mut r = Ready {
            listener: fds[0].revents().contains(PollFlags::IN),
            ..Ready::default()
        };
        let mut k = 1;
        for p in &self.panes {
            let m = fds[k].revents();
            let x = fds[k + 1].revents();
            k += 2;
            r.panes.push((
                !p.eof && (m.contains(PollFlags::IN) || m.intersects(hup)),
                m.contains(PollFlags::OUT),
                !x.is_empty(),
            ));
        }
        for _ in &self.views {
            let f = fds[k].revents();
            k += 1;
            r.views.push((
                f.contains(PollFlags::IN) || f.intersects(hup),
                f.contains(PollFlags::OUT),
            ));
        }
        Ok(r)
    }

    fn accept(&mut self) {
        for _ in 0..16 {
            let stream = match self.listener.accept() {
                Ok((s, _)) => s,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
                Err(e) => {
                    tracing::debug!(error = %e, "accept failed");
                    return;
                }
            };
            if self.views.len() >= MAX_VIEWS {
                tracing::debug!("too many views; connection refused");
                continue;
            }
            let pids: Vec<(u64, i32)> = self.panes.iter().map(|p| (p.id, p.pty.pid())).collect();
            let peer = match peer::identify(&stream, &pids, &self.interpreters) {
                Ok(p) => p,
                Err(why) => {
                    let why = match why {
                        Refused::Uid => "another uid",
                        Refused::Gone => "gone or recycled pid",
                        Refused::Io(_) => "unreadable",
                    };
                    tracing::debug!(why, "view refused");
                    continue;
                }
            };
            if stream.set_nonblocking(true).is_err() {
                continue;
            }
            let id = self.next_view;
            self.next_view += 1;
            tracing::debug!(view = id, pid = peer.pid, "view connected");
            self.views.push(View {
                id,
                stream,
                dec: RecordDecoder::with_limit(VIEW_RECORD_LIMIT),
                out: Vec::new(),
                off: 0,
                peer,
                since: Instant::now(),
                hello: false,
                attached: false,
                grant: Grant::None("not attached"),
                stale: false,
                closing: false,
                dead: false,
            });
        }
    }

    /// Read up to `turns` slices of pane `i`'s output, feed and tee each.
    fn read_pane(&mut self, i: usize, turns: usize) {
        let mut buf = std::mem::take(&mut self.buf);
        for _ in 0..turns {
            let p = &mut self.panes[i];
            if p.eof {
                break;
            }
            match p.pty.read(&mut buf) {
                Ok(0) => p.eof = true,
                Ok(n) => self.output(i, &buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                // EIO: the slave closed; everything before it was read.
                Err(_) => p.eof = true,
            }
        }
        self.buf = buf;
    }

    /// Feed `bytes` to pane `i`, answer its replies, then tee them to the views.
    fn output(&mut self, i: usize, bytes: &[u8]) {
        let p = &mut self.panes[i];
        p.term.feed(bytes, Instant::now());
        Self::settle(p);
        let rec = ServerMsg::Tee {
            pane: p.id,
            seq: p.seq,
            bytes: bytes.to_vec(),
        }
        .encode();
        p.seq += 1;
        let (id, pending) = (p.id, p.pending.clone());
        self.tee(id, &pending, &rec);
    }

    /// After a feed or tick: replies to the PTY (the server is the one that answers),
    /// events dropped (the views raise their own from the same bytes).
    fn settle(p: &mut SPane) {
        let mut replies = Vec::new();
        while let Some(u) = p.term.replies().pop() {
            replies.push(u);
        }
        for u in replies {
            let _ = p.pty.send(u);
        }
        p.term.events().clear();
        let _ = p.term.take_image_jobs();
        let _ = p.term.take_media_requests();
    }

    /// Queue a record of `pane`'s stream to every view that follows it.
    fn tee(&mut self, pane: u64, pending: &[u64], rec: &[u8]) {
        for v in &mut self.views {
            if !v.sees() || v.stale || pending.contains(&v.id) {
                continue;
            }
            v.push(rec);
            if v.queued() > HIGH_WATER {
                tracing::debug!(view = v.id, pane, "view is slow; resync once it drains");
                v.stale = true;
            }
        }
    }

    fn child_exited(&mut self, i: usize) {
        let p = &mut self.panes[i];
        match p.pty.try_wait() {
            Ok(Some(s)) => p.status = Some(s),
            Ok(None) => return,
            Err(e) => {
                tracing::debug!(error = %e, pane = p.id, "wait failed");
                p.status = Some(ExitStatus::Code(-1));
            }
        }
        // The last output, bounded: a background holder of the slave can keep writing.
        self.read_pane(i, 64);
    }

    fn read_view(&mut self, i: usize) {
        let mut buf = std::mem::take(&mut self.buf);
        let mut msgs = Vec::new();
        // Records that arrived before the hang-up still count (input, then `Detach`).
        let mut gone = false;
        {
            let v = &mut self.views[i];
            for _ in 0..4 {
                if v.closing || v.dead || gone {
                    break;
                }
                match v.stream.read(&mut buf) {
                    Ok(0) => gone = true,
                    Ok(n) => v.dec.feed(&buf[..n]),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => gone = true,
                }
                while let Some(r) = v.dec.next_record() {
                    match r.map(ViewMsg::decode) {
                        Ok(Ok(Decoded::Msg(m))) => msgs.push(m),
                        Ok(Ok(Decoded::Skipped(_))) => {}
                        Ok(Err(_)) | Err(_) => {
                            gone = true;
                            break;
                        }
                    }
                }
            }
        }
        self.buf = buf;
        for m in msgs {
            if self.views[i].dead || self.views[i].closing {
                break;
            }
            self.on_msg(i, m);
        }
        if gone {
            tracing::debug!(view = self.views[i].id, "view gone");
            self.views[i].dead = true;
        }
    }

    fn pane_index(&self, id: u64) -> Option<usize> {
        self.panes
            .iter()
            .position(|p| p.id == id && p.status.is_none())
    }

    fn on_msg(&mut self, i: usize, m: ViewMsg) {
        let v = &mut self.views[i];
        if !v.hello {
            let ViewMsg::Hello { major, minor, .. } = m else {
                v.refuse("the stream must open with a hello");
                return;
            };
            if let Err(reason) = mux::check_version(major, minor) {
                tracing::info!(view = v.id, major, minor, "view refused: protocol version");
                v.refuse(&reason);
                return;
            }
            v.hello = true;
            let welcome = ServerMsg::Welcome {
                major: MUX_MAJOR,
                minor: MUX_MINOR,
                caps: Vec::new(),
                session: self.session.clone(),
                view: v.id,
            };
            v.send(&welcome);
            return;
        }
        let full = v.attached && v.grant == Grant::Full;
        match m {
            ViewMsg::Hello { .. } => v.refuse("hello twice"),
            ViewMsg::Attach => {
                if v.attached {
                    return;
                }
                let (pid, pidfd) = (v.peer.pid, &v.peer.pidfd);
                let grant = auth::attach_grant(
                    &v.peer.facts,
                    v.peer.exe.as_deref(),
                    &self.allowed,
                    self.proxy.as_deref(),
                    || auth::runs_proxy(pid, pidfd),
                );
                if let Grant::None(reason) = grant {
                    tracing::info!(view = v.id, pid = v.peer.pid, "attach refused");
                    v.refuse(reason);
                    return;
                }
                v.attached = true;
                v.grant = grant;
                tracing::info!(view = v.id, "view attached");
                let id = v.id;
                // `sessions.resize` is live: it applies from the next attach (T-21 §3).
                self.settings.resize_policy =
                    crate::settings::resize_policy_now(&self.settings.launch);
                let list = ServerMsg::Panes(self.pane_list());
                self.views[i].send(&list);
                for p in &mut self.panes {
                    if p.status.is_none() && !p.pending.contains(&id) {
                        p.pending.push(id);
                    }
                }
            }
            ViewMsg::List => {
                let list = ServerMsg::Panes(self.pane_list());
                self.views[i].send(&list);
            }
            ViewMsg::Input { pane, bytes } => {
                if !full {
                    return;
                }
                if let Some(k) = self.pane_index(pane)
                    && self.panes[k].closing.is_none()
                {
                    let _ = self.panes[k].pty.send(bytes);
                }
            }
            ViewMsg::Resize {
                pane,
                rows,
                cols,
                cell_w,
                cell_h,
            } => {
                if v.sees() {
                    let id = v.id;
                    self.resize(pane, id, (rows, cols), (cell_w, cell_h));
                }
            }
            ViewMsg::Spawn {
                token,
                argv,
                cwd,
                rows,
                cols,
            } => {
                if self.killing || !(v.sees() && auth::may_spawn(v.grant)) {
                    v.send(&ServerMsg::Spawned { token, pane: None });
                    return;
                }
                let id = v.id;
                let pane = match self.spawn(argv, cwd.map(PathBuf::from), (rows, cols)) {
                    Ok(p) => Some(p),
                    Err(e) => {
                        tracing::warn!(view = id, "spawn failed: {e}");
                        None
                    }
                };
                self.views[i].send(&ServerMsg::Spawned { token, pane });
            }
            ViewMsg::Close { pane } => {
                if full && let Some(k) = self.pane_index(pane) {
                    let p = &mut self.panes[k];
                    if p.closing.is_none() {
                        let _ = p.pty.signal_group(Signal::HUP);
                        p.closing = Some((Instant::now(), 0));
                    }
                }
            }
            ViewMsg::Resync { pane } => {
                if v.sees() {
                    let id = v.id;
                    if let Some(k) = self.pane_index(pane)
                        && !self.panes[k].pending.contains(&id)
                    {
                        self.panes[k].pending.push(id);
                    }
                }
            }
            ViewMsg::Detach => {
                tracing::info!(view = v.id, "view detached");
                v.dead = true;
            }
            ViewMsg::KillSession => {
                if !auth::may_kill_session(&v.peer.facts) {
                    v.refuse("agent sessions cannot kill a session");
                    return;
                }
                tracing::info!(view = v.id, panes = self.panes.len(), "session kill");
                self.killing = true;
                let now = Instant::now();
                for p in &mut self.panes {
                    if p.closing.is_none() && p.status.is_none() {
                        let _ = p.pty.signal_group(Signal::HUP);
                        p.closing = Some((now, 0));
                    }
                }
            }
            ViewMsg::DetachAll => {
                if !auth::may_detach_all(&v.peer.facts) {
                    v.refuse("agent sessions cannot detach views");
                    return;
                }
                let me = v.id;
                for w in &mut self.views {
                    if w.id != me && w.attached && !w.closing {
                        w.refuse("detached");
                    }
                }
                tracing::info!(view = me, "every view detached");
            }
        }
    }

    /// A view's size for a pane; the policy decides, and a change is a record in the
    /// pane's stream so every view resizes at the same point.
    fn resize(&mut self, pane: u64, view: u64, (rows, cols): (u16, u16), cell: (u16, u16)) {
        let Some(k) = self.pane_index(pane) else {
            return;
        };
        let p = &mut self.panes[k];
        let (rows, cols) = (rows.max(1), cols.max(1));
        p.asks.retain(|a| a.0 != view);
        p.asks.push((view, rows, cols));
        let asks: Vec<(u16, u16)> = p.asks.iter().map(|a| (a.1, a.2)).collect();
        let Some((r, c)) = pick_size(self.settings.resize_policy, &asks, Some((rows, cols))) else {
            return;
        };
        if cell.0 > 0 && cell.1 > 0 {
            let cell = (u32::from(cell.0), u32::from(cell.1));
            if cell != p.cell {
                p.cell = cell;
                p.term.set_cell_px(cell.0, cell.1);
            }
        }
        if (r, c) != (p.rows, p.cols) {
            let (tr, tc) = p.term.resize(usize::from(r), usize::from(c));
            p.rows = tr as u16;
            p.cols = tc as u16;
        } else {
            // Same size: only the pixel size may have changed, which is no record.
            let _ = p
                .pty
                .resize(PtySize::from_cells(p.rows, p.cols, p.cell.0, p.cell.1));
            return;
        }
        let _ = p
            .pty
            .resize(PtySize::from_cells(p.rows, p.cols, p.cell.0, p.cell.1));
        let rec = ServerMsg::Resized {
            pane: p.id,
            seq: p.seq,
            rows: p.rows,
            cols: p.cols,
        }
        .encode();
        p.seq += 1;
        let (id, pending) = (p.id, p.pending.clone());
        self.tee(id, &pending, &rec);
    }

    fn timers(&mut self) {
        let now = Instant::now();
        for p in &mut self.panes {
            if let Some((at, stage)) = p.closing
                && stage < 2
                && now >= at + CLOSE_STEP
                && p.status.is_none()
            {
                let sig = if stage == 0 {
                    Signal::TERM
                } else {
                    Signal::KILL
                };
                let _ = p.pty.signal_group(sig);
                p.closing = Some((now, stage + 1));
            }
            if p.term.next_deadline().is_some_and(|d| d <= now) {
                p.term.tick(now);
                Self::settle(p);
            }
        }
        for v in &mut self.views {
            if !v.hello && !v.closing && now >= v.since + HELLO_WITHIN {
                v.dead = true;
            }
        }
        // Mid-sequence too long: cancel it, in the stream, so every view cancels it too.
        let stuck: Vec<usize> = (0..self.panes.len())
            .filter(|&i| {
                let p = &self.panes[i];
                p.not_ground.is_some_and(|t| now >= t + GROUND_WAIT) && p.status.is_none()
            })
            .collect();
        for i in stuck {
            tracing::debug!(
                pane = self.panes[i].id,
                "snapshot waits on a sequence; cancelling it"
            );
            self.panes[i].not_ground = None;
            self.output(i, b"\x18");
        }
        // Slow views that drained get fresh snapshots of everything.
        for v in &mut self.views {
            if v.stale && v.queued() == 0 && v.sees() {
                v.stale = false;
                for p in &mut self.panes {
                    if p.status.is_none() && !p.pending.contains(&v.id) {
                        p.pending.push(v.id);
                    }
                }
            }
        }
    }

    /// Send every pending snapshot whose pane is at a ground point.
    fn snapshots(&mut self) {
        for k in 0..self.panes.len() {
            let p = &mut self.panes[k];
            p.pending
                .retain(|id| self.views.iter().any(|v| v.id == *id && v.sees()));
            if p.pending.is_empty() || p.status.is_some() {
                p.not_ground = None;
                continue;
            }
            let img = match p.term.snapshot(Instant::now()) {
                Ok(img) => img,
                Err(SnapError::NotIdle) => {
                    p.not_ground.get_or_insert_with(Instant::now);
                    continue;
                }
                Err(e) => {
                    tracing::warn!(pane = p.id, "snapshot failed: {e}");
                    let pending = std::mem::take(&mut p.pending);
                    for v in &mut self.views {
                        if pending.contains(&v.id) {
                            v.refuse("a pane could not be sent; attach again");
                        }
                    }
                    continue;
                }
            };
            p.not_ground = None;
            let pending = std::mem::take(&mut p.pending);
            let mut pages = Vec::new();
            for (screen, list) in img.pages.iter().enumerate() {
                for bytes in list {
                    pages.extend_from_slice(
                        &ServerMsg::SnapPage {
                            pane: p.id,
                            screen: screen as u8,
                            bytes: bytes.clone(),
                        }
                        .encode(),
                    );
                }
            }
            let end = ServerMsg::SnapEnd { pane: p.id }.encode();
            for v in &mut self.views {
                if !pending.contains(&v.id) {
                    continue;
                }
                v.send(&ServerMsg::SnapBegin {
                    pane: p.id,
                    seq: p.seq,
                    read_only: v.grant != Grant::Full,
                    state: img.state.clone(),
                });
                v.push(&pages);
                v.push(&end);
                v.flush();
            }
        }
    }

    /// Drop closed views and reaped panes.
    fn reap(&mut self) {
        for v in &mut self.views {
            if v.closing && !v.dead {
                v.flush();
                if v.queued() == 0 {
                    v.dead = true;
                }
            }
        }
        let gone: Vec<u64> = self.views.iter().filter(|v| v.dead).map(|v| v.id).collect();
        if !gone.is_empty() {
            self.views.retain(|v| !v.dead);
            for p in &mut self.panes {
                p.asks.retain(|a| !gone.contains(&a.0));
                p.pending.retain(|id| !gone.contains(id));
            }
        }
        let mut k = 0;
        while k < self.panes.len() {
            let p = &self.panes[k];
            if p.status.is_none() {
                k += 1;
                continue;
            }
            let rec = ServerMsg::Exited {
                pane: p.id,
                status: status_code(p.status),
            }
            .encode();
            tracing::debug!(pane = p.id, "server pane exited");
            for v in &mut self.views {
                if v.sees() {
                    v.push(&rec);
                    v.flush();
                }
            }
            self.panes.remove(k);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_policies() {
        let asks = [(24, 80), (50, 120), (30, 60)];
        assert_eq!(
            pick_size(ResizePolicy::Latest, &asks, Some((30, 60))),
            Some((30, 60))
        );
        assert_eq!(
            pick_size(ResizePolicy::Smallest, &asks, None),
            Some((24, 60))
        );
        assert_eq!(
            pick_size(ResizePolicy::Largest, &asks, None),
            Some((50, 120))
        );
        assert_eq!(pick_size(ResizePolicy::Smallest, &[], None), None);
        assert_eq!(
            pick_size(ResizePolicy::Latest, &[], Some((5, 6))),
            Some((5, 6))
        );
    }

    #[test]
    fn session_names() {
        for ok in ["default", "work", "a.b_c-1", &"x".repeat(64)] {
            assert!(valid_session(ok), "{ok}");
        }
        for bad in ["", ".hidden", "a/b", "a b", "../x", &"x".repeat(65), "é"] {
            assert!(!valid_session(bad), "{bad}");
        }
        assert!(socket_path("w").is_none_or(|p| p.ends_with("apotheosis/server-w.sock")));
    }

    #[test]
    fn exit_status_numbers() {
        assert_eq!(status_code(Some(ExitStatus::Code(3))), Some(3));
        assert_eq!(status_code(Some(ExitStatus::Signal(9))), Some(137));
        assert_eq!(status_code(None), None);
    }
}
