// SPDX-License-Identifier: AGPL-3.0-only
//! One pane: the shared state, the messages that cross threads, and the io thread
//! (T-00 §4.2). The io thread reads the PTY, feeds `Term` under a short lock, collects
//! replies and events, and never writes the PTY and never touches Wayland or wgpu.

use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use apoth_input::InputMirror;
use apoth_pty::ExitStatus;
use apoth_term::Term;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::Errno;

use crate::hook::DumpHook;
use crate::workers::Spawner;

/// Bytes fed per lock hold and per read slice (T-07 §1: slices of at most 64 KiB or 1 ms).
const SLICE_BYTES: usize = 64 * 1024;
const SLICE_TIME: Duration = Duration::from_millis(1);

/// Wake byte: stop the io thread.
pub(crate) const WAKE_STOP: u8 = 1;
/// Wake byte: a resize request waits in [`PaneShared::request_resize`]'s mailbox.
pub(crate) const WAKE_RESIZE: u8 = 2;
/// Wake byte: a worker finished an image decode (T-15 §5).
const WAKE_IMAGE: u8 = 3;
/// Wake byte: the user answered a `graphics-file` question ([`PaneShared::post_graphics_file`]).
pub(crate) const WAKE_GRAPHICS_FILE: u8 = 4;
/// Wake byte: a media player has events for the term (T-16 §2).
const WAKE_MEDIA: u8 = 5;
/// Wake byte: the user clicked a media placement ([`PaneShared::post_media_toggle`]).
pub(crate) const WAKE_MEDIA_CLICK: u8 = 6;
/// Wake byte: a host-side terminal operation waits ([`PaneShared::post_term_op`]).
pub(crate) const WAKE_TERM_OP: u8 = 7;

/// A host-side operation on the pane's term (`pane.reset`, `pane.clear-screen`,
/// `scrollback.clear`), applied by the io thread under its own lock hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TermOp {
    Reset,
    ClearScreen,
    ClearScrollback,
}

impl TermOp {
    /// Run the operation on `t` (the same code the escape sequences reach).
    pub(crate) fn apply(self, t: &mut Term) -> apoth_term::Wake {
        match self {
            TermOp::Reset => t.reset_wake(),
            TermOp::ClearScreen => t.clear_screen(),
            TermOp::ClearScrollback => t.clear_scrollback(),
        }
    }
}

/// Upper bound on the yields the io thread spends letting waiting takers go first.
const MAX_HANDOFF_YIELDS: u32 = 2_000;

/// A resize for the io thread to apply under its own lock hold (main never takes `term`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ResizeReq {
    pub(crate) rows: usize,
    pub(crate) cols: usize,
    pub(crate) cell: (u32, u32),
}

/// State shared by main, the io thread and the render thread.
///
/// Lock discipline (T-07 §1): render takes `term` through [`PaneShared::lock`] (a taker);
/// the io thread takes it through [`PaneShared::lock_io`], which first yields while any taker
/// is waiting, so the io thread cannot re-acquire the lock ahead of a waiter. Main never
/// takes it.
pub(crate) struct PaneShared {
    term: Mutex<Term>,
    /// Takers blocked on, or just acquiring, `term`.
    waiters: AtomicUsize,
    resize: Mutex<Option<ResizeReq>>,
    /// Answers to `graphics-file` questions (token, allow), for the io thread.
    graphics_file: Mutex<Vec<(u64, bool)>>,
    /// Media placements the user clicked (play/pause toggles, by key), for the io thread.
    media_clicks: Mutex<Vec<u64>>,
    /// Host-side term operations, for the io thread.
    term_ops: Mutex<Vec<TermOp>>,
    /// The media placements of the frame render last drew, with that frame's top row
    /// (absolute): main hit-tests a press against them at press time (T-08 §4).
    media_shown: Mutex<(usize, Vec<apoth_term::MediaPlacement>)>,
    /// Lock-free mode mirror: main encodes keys from this and never locks `term`.
    pub(crate) mirror: Arc<InputMirror>,
    /// Set by io on output; cleared by render when it wakes (edge-triggered wake).
    pub(crate) dirty: AtomicBool,
    /// Mode 2026 presentation hold.
    pub(crate) hold: AtomicBool,
    /// Window and pane have keyboard focus.
    pub(crate) focused: AtomicBool,
    /// Nanoseconds (process clock) of the last local key press; 0 = never.
    pub(crate) last_key_ns: AtomicU64,
    /// The sequence policy's deny word (`apoth_term::deny`), stored by main; io installs it
    /// before each feed (T-18 §3: a denied class has no effect and no reply).
    pub(crate) deny: AtomicU32,
    /// The term is at an OSC 133 prompt (T-18 §4 paste rules (a) and (e)); stored by io.
    pub(crate) at_prompt: AtomicBool,
    /// Mode 2004 is set and a prompt mark followed it (T-18 §4 "in force"); stored by io.
    pub(crate) bracketed_ok: AtomicBool,
    /// An OSC 133 command is running (`C` seen, no `D` yet); stored by io. One of the
    /// "the user just ran it" tests of the password prompt (T-17 §5).
    pub(crate) running: AtomicBool,
}

impl PaneShared {
    pub(crate) fn new(term: Term) -> PaneShared {
        let mirror = term.input_mirror();
        PaneShared {
            term: Mutex::new(term),
            waiters: AtomicUsize::new(0),
            resize: Mutex::new(None),
            graphics_file: Mutex::new(Vec::new()),
            media_clicks: Mutex::new(Vec::new()),
            term_ops: Mutex::new(Vec::new()),
            media_shown: Mutex::new((0, Vec::new())),
            mirror,
            dirty: AtomicBool::new(false),
            hold: AtomicBool::new(false),
            focused: AtomicBool::new(false),
            last_key_ns: AtomicU64::new(0),
            deny: AtomicU32::new(0),
            at_prompt: AtomicBool::new(false),
            bracketed_ok: AtomicBool::new(false),
            running: AtomicBool::new(false),
        }
    }

    /// The taker's lock (render): announces itself so the io thread yields to it.
    pub(crate) fn lock(&self) -> MutexGuard<'_, Term> {
        self.waiters.fetch_add(1, Ordering::AcqRel);
        let g = self
            .term
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.waiters.fetch_sub(1, Ordering::AcqRel);
        g
    }

    /// The io thread's lock: yields (bounded) while a taker is waiting, then locks.
    pub(crate) fn lock_io(&self) -> MutexGuard<'_, Term> {
        let mut n = 0;
        while self.waiters.load(Ordering::Acquire) > 0 && n < MAX_HANDOFF_YIELDS {
            std::thread::yield_now();
            n += 1;
        }
        self.term
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Leave a resize for the io thread (the latest request wins); the caller then writes
    /// [`WAKE_RESIZE`] to the io wake pipe.
    pub(crate) fn request_resize(&self, r: ResizeReq) {
        *self
            .resize
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(r);
    }

    fn take_resize(&self) -> Option<ResizeReq> {
        self.resize
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// Leave the answer to a `graphics-file` question for the io thread; the caller then
    /// writes [`WAKE_GRAPHICS_FILE`] to the io wake pipe.
    pub(crate) fn post_graphics_file(&self, token: u64, allow: bool) {
        self.graphics_file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((token, allow));
    }

    /// Leave a play/pause toggle of media placement `key` for the io thread (a click with
    /// mouse reporting off, T-08 §4); the caller then writes [`WAKE_MEDIA_CLICK`] to the io
    /// wake pipe. Main never takes the term lock; the io thread resolves the toggle against
    /// the placement's state when it runs.
    pub(crate) fn post_media_toggle(&self, key: u64) {
        let mut q = self
            .media_clicks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Bounded: a click storm faster than the io thread is a few toggles, not a queue.
        if q.len() < 16 {
            q.push(key);
        }
    }

    /// Leave a host-side term operation for the io thread; the caller then writes
    /// [`WAKE_TERM_OP`] to the io wake pipe. Bounded: a runaway sender is a few operations.
    pub(crate) fn post_term_op(&self, op: TermOp) {
        let mut q = self
            .term_ops
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if q.len() < 8 {
            q.push(op);
        }
    }

    fn take_term_ops(&self) -> Vec<TermOp> {
        std::mem::take(
            &mut *self
                .term_ops
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Render: the media placements it just drew, `top` being the frame's first row.
    pub(crate) fn set_media_shown(&self, top: usize, places: &[apoth_term::MediaPlacement]) {
        let mut s = self
            .media_shown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        s.0 = top;
        s.1.clear();
        s.1.extend_from_slice(places);
    }

    /// The drawn media placement at viewport cell `(row, col)`, if any (its key).
    pub(crate) fn media_shown_at(&self, row: usize, col: usize) -> Option<u64> {
        let s = self
            .media_shown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::view::media_at(&s.1, s.0 + row, col)
    }

    /// The drawn placements with the viewport row of their top (clamped to 0 for a box
    /// cut by the frame's first row), front last. For the keyboard and accessibility
    /// targets (T-08 §5, T-20 §5); main never takes the term lock.
    pub(crate) fn media_shown_list(&self) -> Vec<(usize, apoth_term::MediaPlacement)> {
        let s = self
            .media_shown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        s.1.iter()
            .filter(|p| p.row + p.rows as usize > s.0)
            .map(|p| (p.row.saturating_sub(s.0), p.clone()))
            .collect()
    }

    pub(crate) fn take_media_toggles(&self) -> Vec<u64> {
        std::mem::take(
            &mut *self
                .media_clicks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    fn take_graphics_file(&self) -> Vec<(u64, bool)> {
        std::mem::take(
            &mut *self
                .graphics_file
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
}

/// Monotonic nanoseconds since the first call in the process (never 0).
pub(crate) fn now_ns() -> u64 {
    static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let o = ORIGIN.get_or_init(Instant::now);
    u64::try_from(o.elapsed().as_nanos()).unwrap_or(u64::MAX - 1) + 1
}

/// Events drained from the pane's slot. Title text is user-visible window content; it is
/// forwarded to the compositor and never logged.
#[derive(Default)]
pub(crate) struct PaneEvents {
    pub(crate) title: Option<String>,
    /// OSC 7 URI (`file://host/path`); user data, never logged.
    pub(crate) cwd: Option<String>,
    pub(crate) bell: u32,
    /// OSC 52 writes the term allowed (program-origin content, never logged).
    pub(crate) clipboard: Vec<apoth_term::ClipboardSet>,
    /// An OSC 52 read request (selection letters); main answers under the policy.
    pub(crate) clip_read: Option<String>,
    /// OSC 9 / 777 / 99 notifications (program content: shown, never logged).
    pub(crate) notifications: Vec<apoth_term::Notification>,
    /// OSC 9;4 progress.
    pub(crate) progress: Option<apoth_term::Progress>,
    /// OSC 133 `A` marks seen (a prompt started).
    pub(crate) prompts: u32,
    /// OSC 1337 SetUserVar changes, in order.
    pub(crate) vars: Vec<apoth_term::UserVar>,
    /// Command blocks that started or finished, resolved while the term was locked.
    pub(crate) commands: Vec<CommandInfo>,
    /// A `graphics-file` question (T-15 §5, T-18 §3): the token main answers with
    /// [`PaneShared::post_graphics_file`]. The pane's output waits until then.
    pub(crate) graphics_file: Option<u64>,
    /// A media placement plays muted: no sound server took its audio (T-16 §3).
    pub(crate) audio_unavailable: bool,
}

/// A command block's state change with what rules match on (T-11 §5). The command line
/// and directory are user data: matched and templated, never logged.
#[derive(Debug, Clone, Default)]
pub(crate) struct CommandInfo {
    pub(crate) finished: bool,
    pub(crate) cmdline: Option<String>,
    pub(crate) exit: Option<i32>,
    pub(crate) duration_ms: Option<u64>,
    pub(crate) cwd: Option<String>,
}

/// A pane's id in the mux session (`apoth_mux::PaneId`).
pub(crate) type PaneKey = u64;

/// Messages to the main loop. Pane messages carry the pane's id: one io thread per pane.
pub(crate) enum MainMsg {
    /// One reply unit for the pane's PTY, in arrival order.
    Reply(PaneKey, Vec<u8>),
    /// A control-socket reply computed on a worker, for client `u64`.
    Ctl(u64, apoth_proto::Reply),
    /// Accessibility: an adapter request or a finished pane snapshot.
    A11y(crate::app::A11yMsg),
    Events(PaneKey, PaneEvents),
    /// The pane's io thread saw end of output and exits.
    Eof(PaneKey),
    /// The render thread could not start or recover (a clear message, no content).
    RenderFailed(String),
    /// The render thread's cell size at the current scale changed.
    CellSize((u32, u32)),
    /// The io thread applied a resize: the grid size the term now has (rows, cols).
    Resized(PaneKey, (usize, usize)),
    /// `Pty::close` finished for the pane.
    Closed(PaneKey, Option<ExitStatus>),
    /// Text the view took from the selection, to offer on the seat (never logged).
    Copied(crate::view::Copied),
    /// Navigation results from the pane's view (search progress, hints, links); content is
    /// never logged.
    Nav(PaneKey, crate::view::NavMsg),
    /// The pane's cursor cell (row, column) changed: the IME cursor rectangle follows it.
    Cursor(PaneKey, (usize, usize)),
    /// The Abyss detection thread: a theme, or the connection lost (T-17 §1).
    Abyss(apoth_eclipse::Abyss),
    /// The settings portal's reduce-motion preference (T-08 §2).
    ReducedMotion(bool),
    /// A worker finished a config load (T-10 §4): main diffs and applies it.
    /// A media window's threads stopped: its surface can be destroyed.
    MediaStopped(u64),
    /// A media window's placements need these rows and columns (spare row included).
    MediaBox(u64, (usize, usize)),
    /// "Open with" worker results (T-14 §11).
    Open(crate::app::OpenMsg),
    /// The session daemon's stream (T-21 §3).
    Remote(crate::attach::Event),
    /// The askpass broker: a sheet to show or withdraw (T-22 §3; never carries a secret).
    Askpass(crate::askpass::Event),
    /// The pane's PTY entered (`true`) or left the credential state, ECHO off and ICANON
    /// on (ADR 0115), sampled by its io thread on output-idle edges.
    Credential(PaneKey, bool),
    /// An `ec-secret-prompt` ended (T-22 §3). The answer is `Some` for a pane prompt (main
    /// types it); an askpass prompt's answer already went to its client.
    Prompt(u64, Option<crate::askpass::Answer>),
    Loaded(
        Box<(
            apoth_config::reload::Reloader,
            apoth_config::reload::Outcome,
        )>,
    ),
}

/// Messages to the render thread.
pub(crate) enum RenderMsg {
    /// Something changed: drain pacer events, check the dirty flag, key stamp and focus.
    Poke,
    /// Hot reload: new render settings (and a new font configuration when `font.*` changed).
    Update(Box<crate::render::RenderUpdate>),
    /// The overlay bar (error bar or restart notice) changed; `None` removes it.
    Overlay(Option<Box<crate::ui::Overlay>>),
    /// The shown tab's panes and the interface chrome around them (T-13, T-08 §4).
    Layout(Box<crate::render::WindowLayout>),
    /// A selection, scroll or preedit command for a pane's view (applied under its lock).
    View(PaneKey, crate::view::ViewCmd),
    Stop,
}

/// Handles of a running io thread.
pub(crate) struct IoThread {
    pub(crate) join: JoinHandle<()>,
    /// Write one byte to stop the thread.
    pub(crate) wake: OwnedFd,
}

pub(crate) struct IoArgs {
    pub(crate) id: PaneKey,
    pub(crate) shared: Arc<PaneShared>,
    pub(crate) master: OwnedFd,
    pub(crate) wake_rx: OwnedFd,
    pub(crate) to_main: calloop::channel::Sender<MainMsg>,
    pub(crate) to_render: mpsc::Sender<RenderMsg>,
    pub(crate) hook: Option<DumpHook>,
    /// Image decodes run here (T-15 §5); a refused job runs inline.
    pub(crate) workers: Spawner,
    /// A session-daemon pane (T-21 §3): `master` is the view's packet socket from the
    /// attach demultiplexer ([`crate::attach`]), not a PTY, and restored terms arrive here.
    pub(crate) remote: Option<Mailbox>,
}

/// Where the attach demultiplexer leaves a pane's restored term for its io thread.
pub(crate) type Mailbox = Arc<Mutex<Option<Term>>>;

pub(crate) fn spawn_io(a: IoArgs, wake: OwnedFd) -> std::io::Result<IoThread> {
    // Workers wake the io thread through their own copy of the pipe's write end.
    let poke = Arc::new(wake.try_clone()?);
    let join = std::thread::Builder::new()
        .name(format!("apoth-io-{}", a.id))
        .spawn(move || io_main(a, poke))?;
    Ok(IoThread { join, wake })
}

/// The io thread's view of its term between lock holds.
#[derive(Clone, Copy, Default)]
struct IoState {
    /// An image decode or a `graphics-file` question holds the output (T-15 §5): the PTY
    /// is not read until it resolves, so the shell sees back-pressure, not loss.
    blocked: bool,
    /// The term's next timer (mode 2026 hold, half-received image transfers).
    deadline: Option<Instant>,
    /// An `apoth icat` session relies on the process group rule: re-check it every
    /// [`GROUP_RECHECK`] even without output (T-16 §1.1).
    group_watch: bool,
}

/// How often an open process-group-bound image session is re-checked while the pane is
/// quiet. Only while such a session is open.
const GROUP_RECHECK: Duration = Duration::from_millis(250);

/// Watch `pid` of an image session for exit: it must be in the pane's session (the pty's
/// session leader), so a program cannot tie its placements to an unrelated process.
fn watch_pid(pid: i32, master: &OwnedFd) -> Option<OwnedFd> {
    use rustix::process::{Pid, PidfdFlags, getsid, pidfd_open};
    let pid = Pid::from_raw(pid)?;
    let sid = rustix::termios::tcgetsid(master).ok()?;
    if getsid(Some(pid)).ok()? != sid {
        return None;
    }
    pidfd_open(pid, PidfdFlags::NONBLOCK).ok()
}

/// What the io thread needs to run a locked step and publish its results.
struct Io<'a> {
    id: PaneKey,
    shared: &'a PaneShared,
    to_main: &'a calloop::channel::Sender<MainMsg>,
    to_render: &'a mpsc::Sender<RenderMsg>,
    workers: &'a Spawner,
    done_tx: &'a mpsc::Sender<apoth_term::apoth_image::kitty::DecodeDone>,
    poke: &'a Arc<OwnedFd>,
    /// The pty master: its foreground process group owns `apoth icat` sessions.
    master: &'a OwnedFd,
    /// Pidfds of `apoth icat` sessions opened with `p=` (session id, pidfd).
    watch: std::cell::RefCell<Vec<(u64, OwnedFd)>>,
    /// The pane's media players.
    media: std::cell::RefCell<crate::mediahost::MediaHost>,
}

fn io_main(a: IoArgs, poke: Arc<OwnedFd>) {
    let IoArgs {
        id,
        shared,
        master,
        wake_rx,
        to_main,
        to_render,
        hook,
        workers,
        remote,
    } = a;
    let (done_tx, done_rx) = mpsc::channel();
    let io = Io {
        id,
        shared: &shared,
        to_main: &to_main,
        to_render: &to_render,
        workers: &workers,
        done_tx: &done_tx,
        poke: &poke,
        master: &master,
        watch: std::cell::RefCell::new(Vec::new()),
        media: std::cell::RefCell::new(crate::mediahost::MediaHost::new(
            crate::mediahost::pipe_poke(Arc::clone(&poke), WAKE_MEDIA),
        )),
    };
    // A remote packet is a tag byte plus up to one slice.
    let mut buf = vec![0u8; SLICE_BYTES + 16];
    if let Some(h) = &hook {
        h.write(&shared.lock_io());
    }
    let mut st = IoState::default();
    let mut eof = false;
    // The credential predicate (ADR 0115), local PTYs only: sampled when output goes idle
    // and once more a moment later (a program may print its prompt before it turns echo
    // off). Only a change is sent.
    let mut cred = Credential::default();
    while !eof {
        let deadline = if st.group_watch {
            let recheck = Instant::now() + GROUP_RECHECK;
            Some(st.deadline.map_or(recheck, |d| d.min(recheck)))
        } else {
            st.deadline
        };
        let deadline = match (deadline, cred.recheck) {
            (Some(d), Some(r)) => Some(d.min(r)),
            (d, r) => d.or(r),
        };
        let timeout = deadline.map(|d| {
            let left = d.saturating_duration_since(Instant::now());
            Timespec {
                tv_sec: i64::try_from(left.as_secs()).unwrap_or(i64::MAX),
                tv_nsec: i64::from(left.subsec_nanos()),
            }
        });
        let (readable, woken) = {
            // While blocked the PTY is left out of the poll (a hang-up would spin it).
            let mut fds = [
                PollFd::new(&wake_rx, PollFlags::IN),
                PollFd::new(&master, PollFlags::IN),
            ];
            let n = if st.blocked { 1 } else { 2 };
            let watch = io.watch.borrow();
            let r = if watch.is_empty() {
                poll(&mut fds[..n], timeout.as_ref())
                    .map(|_| (fds[0].revents(), fds[1].revents(), None))
            } else {
                // An image session's command is watched: its pidfd turns readable on exit.
                let mut all: Vec<PollFd<'_>> = fds[..n].to_vec();
                all.extend(watch.iter().map(|(_, fd)| PollFd::new(fd, PollFlags::IN)));
                poll(&mut all, timeout.as_ref()).map(|_| {
                    let gone = all[n..]
                        .iter()
                        .position(|f| !f.revents().is_empty())
                        .map(|i| watch[i].0);
                    (
                        all[0].revents(),
                        all.get(1).map_or(PollFlags::empty(), |f| f.revents()),
                        gone,
                    )
                })
            };
            drop(watch);
            match r {
                Ok((w, m, gone)) => {
                    if let Some(sid) = gone {
                        // Killed without `a=e`: its placements go now, not at the next output.
                        st = io.step(|t| apoth_term::Wake {
                            dirty: t.end_image_session(sid),
                            ..apoth_term::Wake::default()
                        });
                    }
                    (n == 2 && !m.is_empty(), !w.is_empty())
                }
                Err(Errno::INTR) => continue,
                Err(e) => {
                    tracing::error!(errno = e.raw_os_error(), "io poll failed");
                    break;
                }
            }
        };
        if woken {
            let mut b = [0u8; 64];
            let n = rustix::io::read(&wake_rx, &mut b).unwrap_or(0);
            if b[..n].contains(&WAKE_STOP) {
                break;
            }
            if let Some(r) = shared.take_resize() {
                let dims = {
                    let mut t = shared.lock_io();
                    t.set_cell_px(r.cell.0, r.cell.1);
                    t.resize(r.rows, r.cols)
                };
                let _ = to_main.send(MainMsg::Resized(id, dims));
                if !shared.dirty.swap(true, Ordering::AcqRel) {
                    let _ = to_render.send(RenderMsg::Poke);
                }
            }
            // Mailboxes, whatever bytes arrived: a stale token is ignored by the term.
            let mut ran = false;
            while let Ok(done) = done_rx.try_recv() {
                st = io.step(|t| t.image_done(done, Instant::now()));
                ran = true;
            }
            for (token, allow) in shared.take_graphics_file() {
                st = io.step(|t| t.graphics_file_decision(token, allow, Instant::now()));
                ran = true;
            }
            for op in shared.take_term_ops() {
                st = io.step(|t| op.apply(t));
                ran = true;
            }
            let clicks = shared.take_media_toggles();
            if !clicks.is_empty() {
                st = io.step(|t| media_toggles(t, &clicks));
            }
            let evs = io
                .media
                .borrow_mut()
                .drain(shared.focused.load(Ordering::Acquire));
            if !evs.is_empty() {
                st = io.step(|t| {
                    let now = Instant::now();
                    let mut w = apoth_term::Wake::default();
                    for (key, ev) in evs {
                        let x = t.media_event(key, ev, now);
                        w.dirty |= x.dirty;
                        w.replies |= x.replies;
                        w.events |= x.events;
                        w.jobs |= x.jobs;
                        w.media |= x.media;
                        w.hold = x.hold;
                    }
                    w
                });
            }
            if let Some(h) = &hook
                && ran
            {
                // The held output just ran.
                h.write(&shared.lock_io());
            }
            if !readable {
                continue;
            }
        }
        if !readable {
            if cred.recheck.is_some_and(|r| Instant::now() >= r) {
                cred.recheck = None;
                cred.sample(&master, id, &to_main);
            }
            // Timeout: a mode 2026 hold or a half-received image transfer may have expired.
            st = io.step(|t| t.tick(Instant::now()));
            continue;
        }
        let started = Instant::now();
        let mut total = 0usize;
        loop {
            match rustix::io::read(&master, &mut buf) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(n) if remote.is_some() => {
                    total += n;
                    st = io.remote_packet(&buf[..n], remote.as_ref().expect("checked"));
                    if total >= SLICE_BYTES || started.elapsed() >= SLICE_TIME {
                        break;
                    }
                }
                Ok(n) => {
                    total += n;
                    let bytes = &buf[..n];
                    st = io.step(|t| t.feed(bytes, Instant::now()));
                    if st.blocked || total >= SLICE_BYTES || started.elapsed() >= SLICE_TIME {
                        // The next `feed` yields to any waiting taker (`lock_io`).
                        break;
                    }
                }
                Err(Errno::AGAIN) => break,
                Err(Errno::INTR) => {}
                Err(_) => {
                    // EIO: the slave closed; everything before it was already read.
                    eof = true;
                    break;
                }
            }
        }
        if let Some(h) = &hook
            && total > 0
        {
            h.write(&shared.lock_io());
        }
        if total > 0 && remote.is_none() && !eof {
            cred.sample(&master, id, &to_main);
            cred.recheck = Some(Instant::now() + CREDENTIAL_RECHECK);
        }
    }
    let _ = to_main.send(MainMsg::Eof(id));
}

/// The second look after output goes idle.
const CREDENTIAL_RECHECK: Duration = Duration::from_millis(50);

/// The io thread's view of the credential predicate.
#[derive(Default)]
struct Credential {
    on: bool,
    recheck: Option<Instant>,
}

impl Credential {
    /// Read the line discipline and report a change. No allocation; one syscall.
    fn sample(
        &mut self,
        master: &OwnedFd,
        id: PaneKey,
        to_main: &calloop::channel::Sender<MainMsg>,
    ) {
        let Ok(t) = rustix::termios::tcgetattr(master) else {
            return;
        };
        let lm = t.local_modes;
        let on = apoth_pty::TermiosState {
            echo: lm.contains(rustix::termios::LocalModes::ECHO),
            icanon: lm.contains(rustix::termios::LocalModes::ICANON),
        }
        .is_credential();
        if on != self.on {
            self.on = on;
            let _ = to_main.send(MainMsg::Credential(id, on));
        }
    }
}

impl Io<'_> {
    /// One packet from the attach demultiplexer: output, a resize at its place in the
    /// stream, or a fresh snapshot to swap in (T-21 §3). The term the view holds then
    /// matches the server's at every point of the stream.
    fn remote_packet(&self, pkt: &[u8], mailbox: &Mailbox) -> IoState {
        match pkt.split_first() {
            Some((&crate::attach::PKT_OUTPUT, bytes)) => {
                self.step(|t| t.feed(bytes, Instant::now()))
            }
            Some((&crate::attach::PKT_RESIZE, b)) if b.len() == 4 => {
                let rows = usize::from(u16::from_le_bytes([b[0], b[1]]));
                let cols = usize::from(u16::from_le_bytes([b[2], b[3]]));
                let dims = self.shared.lock_io().resize(rows, cols);
                let _ = self.to_main.send(MainMsg::Resized(self.id, dims));
                self.step(|_| apoth_term::Wake {
                    dirty: true,
                    ..apoth_term::Wake::default()
                })
            }
            Some((&crate::attach::PKT_SNAPSHOT, _)) => {
                let Some(mut new) = mailbox
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                else {
                    return self.step(|_| apoth_term::Wake::default());
                };
                new.set_input_mirror(Arc::clone(&self.shared.mirror));
                // The server's sequence policy: the view must refuse exactly what it refused.
                self.shared.deny.store(new.deny_bits(), Ordering::Release);
                // The title the program set before this view attached shows on the tab.
                let title = new.title().to_owned();
                if !title.is_empty() {
                    new.events().title = Some(title);
                }
                let dims = {
                    let mut t = self.shared.lock_io();
                    *t = new;
                    (t.rows(), t.cols())
                };
                let _ = self.to_main.send(MainMsg::Resized(self.id, dims));
                self.step(|_| apoth_term::Wake {
                    dirty: true,
                    events: true,
                    ..apoth_term::Wake::default()
                })
            }
            _ => self.step(|_| apoth_term::Wake::default()),
        }
    }

    /// One locked step on the term (feed, tick, a decode result, a consent answer): installs
    /// the deny word, runs `f`, collects replies, events and image jobs, then publishes and
    /// hands the jobs to the workers outside the lock.
    fn step(&self, f: impl FnOnce(&mut Term) -> apoth_term::Wake) -> IoState {
        let shared = self.shared;
        let mut replies = Vec::new();
        let mut events = None;
        let mut jobs = Vec::new();
        let media;
        let (wake, st) = {
            let mut t = shared.lock_io();
            let deny = shared.deny.load(Ordering::Acquire);
            if t.deny_bits() != deny {
                t.set_deny(deny);
            }
            let mut wake = f(&mut t);
            // `apoth icat` sessions (T-16 §1.1): a session with `p=` is watched through a
            // pidfd; the others bind to the foreground group and end when it loses the
            // terminal (killed without cleaning up). Only while a session is open.
            let mut group_watch = false;
            if t.has_image_sessions() {
                let mut watch = self.watch.borrow_mut();
                for (sid, pid) in t.take_image_session_pids() {
                    match watch_pid(pid, self.master) {
                        Some(fd) => watch.push((sid, fd)),
                        None => t.unwatch_image_session(sid),
                    }
                }
                watch.retain(|(sid, _)| t.has_image_session(*sid));
                if t.has_group_image_sessions()
                    && let Ok(fg) = rustix::termios::tcgetpgrp(self.master)
                    && t.bind_image_sessions(fg.as_raw_nonzero().get())
                {
                    wake.dirty = true;
                }
                group_watch = t.has_group_image_sessions();
            } else {
                self.watch.borrow_mut().clear();
            }
            shared.at_prompt.store(t.at_prompt(), Ordering::Release);
            shared.running.store(
                t.blocks()
                    .last()
                    .is_some_and(|b| b.state == apoth_term::shell::BlockState::Running),
                Ordering::Release,
            );
            shared
                .bracketed_ok
                .store(t.bracketed_in_force(), Ordering::Release);
            while let Some(u) = t.replies().pop() {
                replies.push(u);
            }
            if wake.events {
                events = Some(take_events(&mut t));
            }
            if wake.jobs {
                jobs = t.take_image_jobs();
            }
            media = t.take_media_requests();
            let st = IoState {
                blocked: t.blocked(),
                deadline: t.next_deadline(),
                group_watch,
            };
            (wake, st)
        };
        publish(
            self.id,
            shared,
            self.to_main,
            self.to_render,
            wake,
            replies,
            events,
        );
        if !media.is_empty() {
            self.media
                .borrow_mut()
                .requests(media, shared.focused.load(Ordering::Acquire));
        }
        for job in jobs {
            let token = job.token;
            let (tx, poke) = (self.done_tx.clone(), Arc::clone(self.poke));
            let run = Box::new(move || {
                let mut dec = crate::mediahost::image_decoder();
                let _ = tx.send(job.run_with(&mut *dec));
                let _ = rustix::io::write(&*poke, &[WAKE_IMAGE]);
            });
            let Err(run) = self.workers.submit(run) else {
                continue;
            };
            // A full queue or no worker: a thread of its own, never this one (T-15 §5). A
            // pane has one transmission outstanding at a time, so these are bounded by panes.
            if std::thread::Builder::new()
                .name("apoth-decode".into())
                .spawn(run)
                .is_err()
            {
                tracing::warn!("cannot start an image decode thread");
                use apoth_term::apoth_image::{ErrCode, kitty};
                let _ = self.done_tx.send(kitty::DecodeDone {
                    token,
                    outcome: kitty::Outcome::Failed(ErrCode::Enospc),
                });
                let _ = rustix::io::write(&**self.poke, &[WAKE_IMAGE]);
            }
        }
        st
    }
}

/// The control a click on a placement means: pause while it plays, play otherwise (an
/// ended placement restarts, an opening one starts when ready).
pub(crate) fn toggle_ctl(p: &apoth_term::MediaPlacement) -> apoth_term::MediaCtl {
    match p.state {
        apoth_term::MediaPlay::Playing => apoth_term::MediaCtl::Pause,
        _ => apoth_term::MediaCtl::Play,
    }
}

/// What a keyboard media action asks for (T-08 §5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MediaWant {
    Toggle,
    Play,
    Pause,
}

/// The placement a keyboard media action applies to, when it changes anything: the one
/// under the pointer (`hovered`), else the frontmost drawn one; `None` when there is none or
/// it is already in the wanted state (`Play` on a playing one, `Pause` on one not playing).
pub(crate) fn media_key_target(
    shown: &[(usize, apoth_term::MediaPlacement)],
    hovered: Option<u64>,
    want: MediaWant,
) -> Option<u64> {
    let p = hovered
        .and_then(|k| shown.iter().find(|(_, p)| p.key == k))
        .or_else(|| shown.last())
        .map(|(_, p)| p)?;
    let playing = p.state == apoth_term::MediaPlay::Playing;
    match want {
        MediaWant::Toggle => Some(p.key),
        MediaWant::Play => (!playing).then_some(p.key),
        MediaWant::Pause => playing.then_some(p.key),
    }
}

/// Apply clicked toggles (by key) to the term; a key whose placement went is dropped.
pub(crate) fn media_toggles(t: &mut Term, keys: &[u64]) -> apoth_term::Wake {
    let mut w = apoth_term::Wake::default();
    for &key in keys {
        let Some(ctl) = t
            .media_placements()
            .iter()
            .find(|p| p.key == key)
            .map(toggle_ctl)
        else {
            continue;
        };
        let x = t.media_control(key, ctl);
        w.dirty |= x.dirty;
        w.media |= x.media;
        w.hold = x.hold;
    }
    w
}

pub(crate) fn take_events(t: &mut Term) -> PaneEvents {
    let slot = t.events();
    let mut e = PaneEvents {
        title: slot.title.take(),
        cwd: slot.cwd.take(),
        bell: slot.bell,
        clipboard: slot.clipboard.drain(..).collect(),
        clip_read: slot.clip_read.take(),
        notifications: slot.notifications.drain(..).collect(),
        progress: slot.progress.take(),
        prompts: slot.marks.iter().filter(|m| m.kind == 'A').count() as u32,
        vars: slot.vars.drain(..).collect(),
        commands: Vec::new(),
        graphics_file: slot.graphics_file.take(),
        audio_unavailable: slot.audio_unavailable,
    };
    let changes: Vec<apoth_term::CommandEvent> = slot.commands.drain(..).collect();
    slot.clear();
    e.commands = changes
        .into_iter()
        .map(|c| {
            let b = t.blocks().get(c.block);
            CommandInfo {
                finished: c.finished,
                cmdline: b.and_then(|b| b.cmdline.clone()),
                exit: b.and_then(|b| b.exit),
                duration_ms: b.and_then(|b| {
                    let d = b.ended?.checked_duration_since(b.started?)?;
                    u64::try_from(d.as_millis()).ok()
                }),
                cwd: b.and_then(|b| b.cwd.clone()),
            }
        })
        .collect();
    e
}

fn publish(
    id: PaneKey,
    shared: &PaneShared,
    to_main: &calloop::channel::Sender<MainMsg>,
    to_render: &mpsc::Sender<RenderMsg>,
    wake: apoth_term::Wake,
    replies: Vec<Vec<u8>>,
    events: Option<PaneEvents>,
) {
    shared.hold.store(wake.hold, Ordering::Release);
    for r in replies {
        let _ = to_main.send(MainMsg::Reply(id, r));
    }
    if let Some(e) = events {
        let _ = to_main.send(MainMsg::Events(id, e));
    }
    // Edge-triggered: notify render only on the clean to dirty transition.
    if (wake.dirty || !wake.hold) && !shared.dirty.swap(true, Ordering::AcqRel) {
        let _ = to_render.send(RenderMsg::Poke);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apoth_term::TermConfig;

    /// A press is hit-tested against the placements render last drew, offset by that
    /// frame's top row: no hover report is needed before the click (T-08 §4).
    #[test]
    fn a_press_hits_the_drawn_placement() {
        let shared = PaneShared::new(Term::new(TermConfig::default()));
        assert_eq!(shared.media_shown_at(0, 0), None);
        let place = apoth_term::MediaPlacement {
            key: 7,
            id: 1,
            row: 12,
            col: 3,
            cols: 4,
            rows: 2,
            state: apoth_term::MediaPlay::Playing,
            pos_ms: 0,
            duration_ms: None,
            video: None,
        };
        shared.set_media_shown(10, std::slice::from_ref(&place));
        assert_eq!(shared.media_shown_at(2, 3), Some(7));
        assert_eq!(shared.media_shown_at(3, 6), Some(7));
        assert_eq!(shared.media_shown_at(1, 3), None);
        assert_eq!(shared.media_shown_at(4, 3), None);
        assert_eq!(shared.media_shown_at(2, 7), None);
        // Scrolled back one row: the box sits a row lower in the viewport.
        shared.set_media_shown(9, &[place]);
        assert_eq!(shared.media_shown_at(2, 3), None);
        assert_eq!(shared.media_shown_at(3, 3), Some(7));
        shared.set_media_shown(9, &[]);
        assert_eq!(shared.media_shown_at(3, 3), None);
    }

    fn placed(key: u64, row: usize, state: apoth_term::MediaPlay) -> apoth_term::MediaPlacement {
        apoth_term::MediaPlacement {
            key,
            id: 1,
            row,
            col: 0,
            cols: 4,
            rows: 2,
            state,
            pos_ms: 0,
            duration_ms: None,
            video: None,
        }
    }

    /// The drawn list keeps boxes the frame's first row cuts (row clamped) and drops those
    /// wholly above it (T-08 §5, T-20 §5 targets).
    #[test]
    fn drawn_list_is_in_viewport_rows() {
        use apoth_term::MediaPlay::Playing;
        let shared = PaneShared::new(Term::new(TermConfig::default()));
        shared.set_media_shown(
            10,
            &[
                placed(1, 7, Playing),
                placed(2, 9, Playing),
                placed(3, 14, Playing),
            ],
        );
        let l = shared.media_shown_list();
        let rows: Vec<(u64, usize)> = l.iter().map(|(r, p)| (p.key, *r)).collect();
        assert_eq!(rows, [(2, 0), (3, 4)]);
    }

    /// A keyboard action takes the hovered placement, else the frontmost; play and pause
    /// leave one already in that state alone.
    #[test]
    fn keyboard_media_target() {
        use apoth_term::MediaPlay::{Ended, Paused, Playing};
        let shown = [(0, placed(1, 0, Playing)), (3, placed(2, 3, Paused))];
        assert_eq!(media_key_target(&shown, None, MediaWant::Toggle), Some(2));
        assert_eq!(
            media_key_target(&shown, Some(1), MediaWant::Toggle),
            Some(1)
        );
        // A stale hover (that placement went) falls back to the frontmost.
        assert_eq!(
            media_key_target(&shown, Some(9), MediaWant::Toggle),
            Some(2)
        );
        assert_eq!(media_key_target(&shown, None, MediaWant::Play), Some(2));
        assert_eq!(media_key_target(&shown, None, MediaWant::Pause), None);
        assert_eq!(media_key_target(&shown, Some(1), MediaWant::Pause), Some(1));
        assert_eq!(media_key_target(&shown, Some(1), MediaWant::Play), None);
        let ended = [(0, placed(4, 0, Ended))];
        assert_eq!(media_key_target(&ended, None, MediaWant::Play), Some(4));
        assert_eq!(media_key_target(&[], None, MediaWant::Toggle), None);
    }

    /// A term with one playing media placement (key, 4x2 cells at the origin).
    fn playing() -> (Term, u64) {
        let now = std::time::Instant::now();
        let mut t = Term::new(TermConfig {
            rows: 10,
            cols: 40,
            ..TermConfig::default()
        });
        t.feed(b"\x1b_Ma=o,s=f,t=1,i=2,c=4,r=2\x1b\\", now);
        let key = match t.take_media_requests().as_slice() {
            [apoth_term::MediaRequest::Open { key, .. }] => *key,
            other => panic!("{other:?}"),
        };
        let ready = apoth_term::MediaEvent::Ready {
            width: 32,
            height: 32,
            duration_ms: 0,
            video_w: 32,
            video_h: 32,
            dar: None,
        };
        t.media_event(key, ready, now);
        (t, key)
    }

    #[test]
    fn a_click_toggles_play_and_pause() {
        use apoth_term::{MediaCtl, MediaPlay, MediaRequest};
        let (mut t, key) = playing();
        assert_eq!(toggle_ctl(&t.media_placements()[0]), MediaCtl::Pause);
        let w = media_toggles(&mut t, &[key]);
        assert!(w.dirty && w.media);
        assert_eq!(t.media_placements()[0].state, MediaPlay::Paused);
        assert_eq!(
            t.take_media_requests(),
            vec![MediaRequest::Control {
                key,
                ctl: MediaCtl::Pause
            }]
        );
        assert_eq!(toggle_ctl(&t.media_placements()[0]), MediaCtl::Play);
        media_toggles(&mut t, &[key]);
        assert_eq!(t.media_placements()[0].state, MediaPlay::Playing);
        // A key whose placement went is dropped.
        let w = media_toggles(&mut t, &[key + 9]);
        assert!(!w.dirty && !w.media);
    }

    #[test]
    fn now_is_monotonic_and_nonzero() {
        let a = now_ns();
        let b = now_ns();
        assert!(a > 0 && b >= a);
    }

    #[test]
    fn io_yields_to_a_waiting_taker() {
        let shared = Arc::new(PaneShared::new(Term::new(TermConfig::default())));
        let order = Arc::new(Mutex::new(Vec::new()));
        let held = shared.lock();
        let taker = {
            let (s, o) = (Arc::clone(&shared), Arc::clone(&order));
            std::thread::spawn(move || {
                let _g = s.lock();
                o.lock().unwrap().push("taker");
            })
        };
        while shared.waiters.load(Ordering::Acquire) == 0 {
            std::thread::yield_now();
        }
        let io = {
            let (s, o) = (Arc::clone(&shared), Arc::clone(&order));
            std::thread::spawn(move || {
                let _g = s.lock_io();
                o.lock().unwrap().push("io");
            })
        };
        std::thread::sleep(Duration::from_millis(30));
        drop(held);
        taker.join().unwrap();
        io.join().unwrap();
        assert_eq!(*order.lock().unwrap(), ["taker", "io"]);
    }

    #[test]
    fn term_ops_mailbox_is_bounded_and_drains_in_order() {
        let shared = PaneShared::new(Term::new(TermConfig::default()));
        shared.post_term_op(TermOp::ClearScreen);
        shared.post_term_op(TermOp::Reset);
        for _ in 0..20 {
            shared.post_term_op(TermOp::ClearScrollback);
        }
        let ops = shared.take_term_ops();
        assert_eq!(ops.len(), 8);
        assert_eq!(ops[..2], [TermOp::ClearScreen, TermOp::Reset]);
        assert!(shared.take_term_ops().is_empty());
    }

    #[test]
    fn term_ops_reach_the_term() {
        let mut t = Term::new(TermConfig {
            rows: 3,
            cols: 10,
            ..TermConfig::default()
        });
        let now = Instant::now();
        for i in 0..8 {
            t.feed(format!("l{i}\r\n").as_bytes(), now);
        }
        assert!(t.primary().history_rows() > 0);
        assert!(TermOp::ClearScreen.apply(&mut t).dirty);
        assert!(t.primary().history_rows() > 0);
        assert!(TermOp::ClearScrollback.apply(&mut t).dirty);
        assert_eq!(t.primary().history_rows(), 0);
        t.feed(b"\x1b[?2004h", now);
        let on = |t: &Term| {
            t.input_mirror()
                .load()
                .has(apoth_input::modes::bit::BRACKETED_PASTE)
        };
        assert!(on(&t));
        TermOp::Reset.apply(&mut t);
        assert!(!on(&t));
    }

    #[test]
    fn resize_mailbox_keeps_the_latest() {
        let shared = PaneShared::new(Term::new(TermConfig::default()));
        let r = |rows| ResizeReq {
            rows,
            cols: 80,
            cell: (8, 16),
        };
        shared.request_resize(r(10));
        shared.request_resize(r(20));
        assert_eq!(shared.take_resize(), Some(r(20)));
        assert_eq!(shared.take_resize(), None);
    }

    #[test]
    fn dirty_edge_pokes_once() {
        let shared = PaneShared::new(Term::new(TermConfig::default()));
        let (tx, rx) = mpsc::channel();
        let (mtx, _mrx) = calloop::channel::channel::<MainMsg>();
        let w = apoth_term::Wake {
            dirty: true,
            ..Default::default()
        };
        publish(1, &shared, &mtx, &tx, w, Vec::new(), None);
        publish(1, &shared, &mtx, &tx, w, Vec::new(), None);
        assert!(matches!(rx.try_recv(), Ok(RenderMsg::Poke)));
        assert!(rx.try_recv().is_err());
    }
}
