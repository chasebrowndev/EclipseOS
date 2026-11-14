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
}

impl PaneShared {
    pub(crate) fn new(term: Term) -> PaneShared {
        let mirror = term.input_mirror();
        PaneShared {
            term: Mutex::new(term),
            waiters: AtomicUsize::new(0),
            resize: Mutex::new(None),
            graphics_file: Mutex::new(Vec::new()),
            mirror,
            dirty: AtomicBool::new(false),
            hold: AtomicBool::new(false),
            focused: AtomicBool::new(false),
            last_key_ns: AtomicU64::new(0),
            deny: AtomicU32::new(0),
            at_prompt: AtomicBool::new(false),
            bracketed_ok: AtomicBool::new(false),
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
    /// A worker finished a config load (T-10 §4): main diffs and applies it.
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
}

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
    };
    let mut buf = vec![0u8; SLICE_BYTES];
    if let Some(h) = &hook {
        h.write(&shared.lock_io());
    }
    let mut st = IoState::default();
    let mut eof = false;
    while !eof {
        let timeout = st.deadline.map(|d| {
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
            match poll(&mut fds[..n], timeout.as_ref()) {
                Ok(_) => (
                    n == 2 && !fds[1].revents().is_empty(),
                    !fds[0].revents().is_empty(),
                ),
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
    }
    let _ = to_main.send(MainMsg::Eof(id));
}

impl Io<'_> {
    /// One locked step on the term (feed, tick, a decode result, a consent answer): installs
    /// the deny word, runs `f`, collects replies, events and image jobs, then publishes and
    /// hands the jobs to the workers outside the lock.
    fn step(&self, f: impl FnOnce(&mut Term) -> apoth_term::Wake) -> IoState {
        let shared = self.shared;
        let mut replies = Vec::new();
        let mut events = None;
        let mut jobs = Vec::new();
        let (wake, st) = {
            let mut t = shared.lock_io();
            let deny = shared.deny.load(Ordering::Acquire);
            if t.deny_bits() != deny {
                t.set_deny(deny);
            }
            let wake = f(&mut t);
            shared.at_prompt.store(t.at_prompt(), Ordering::Release);
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
            let st = IoState {
                blocked: t.blocked(),
                deadline: t.next_deadline(),
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
        for job in jobs {
            let token = job.token;
            let (tx, poke) = (self.done_tx.clone(), Arc::clone(self.poke));
            let run = Box::new(move || {
                let _ = tx.send(job.run());
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

fn take_events(t: &mut Term) -> PaneEvents {
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
