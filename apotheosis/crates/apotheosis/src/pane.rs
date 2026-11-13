// SPDX-License-Identifier: AGPL-3.0-only
//! One pane: the shared state, the messages that cross threads, and the io thread
//! (T-00 §4.2). The io thread reads the PTY, feeds `Term` under a short lock, collects
//! replies and events, and never writes the PTY and never touches Wayland or wgpu.

use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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

/// Bytes fed per lock hold and per read slice (T-07 §1: slices of at most 64 KiB or 1 ms).
const SLICE_BYTES: usize = 64 * 1024;
const SLICE_TIME: Duration = Duration::from_millis(1);

/// Wake byte: stop the io thread.
pub(crate) const WAKE_STOP: u8 = 1;
/// Wake byte: a resize request waits in [`PaneShared::request_resize`]'s mailbox.
pub(crate) const WAKE_RESIZE: u8 = 2;

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
}

impl PaneShared {
    pub(crate) fn new(term: Term) -> PaneShared {
        let mirror = term.input_mirror();
        PaneShared {
            term: Mutex::new(term),
            waiters: AtomicUsize::new(0),
            resize: Mutex::new(None),
            mirror,
            dirty: AtomicBool::new(false),
            hold: AtomicBool::new(false),
            focused: AtomicBool::new(false),
            last_key_ns: AtomicU64::new(0),
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
    pub(crate) bell: u32,
}

/// Messages to the main loop.
pub(crate) enum MainMsg {
    /// One reply unit for the PTY, in arrival order.
    Reply(Vec<u8>),
    Events(PaneEvents),
    /// The io thread saw end of output and exits.
    Eof,
    /// The render thread could not start or recover (a clear message, no content).
    RenderFailed(String),
    /// The render thread's cell size at the current scale changed.
    CellSize((u32, u32)),
    /// The io thread applied a resize: the grid size the term now has (rows, cols).
    Resized((usize, usize)),
    /// `Pty::close` finished.
    Closed(Option<ExitStatus>),
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
    Stop,
}

/// Handles of a running io thread.
pub(crate) struct IoThread {
    pub(crate) join: JoinHandle<()>,
    /// Write one byte to stop the thread.
    pub(crate) wake: OwnedFd,
}

pub(crate) struct IoArgs {
    pub(crate) shared: Arc<PaneShared>,
    pub(crate) master: OwnedFd,
    pub(crate) wake_rx: OwnedFd,
    pub(crate) to_main: calloop::channel::Sender<MainMsg>,
    pub(crate) to_render: mpsc::Sender<RenderMsg>,
    pub(crate) hook: Option<DumpHook>,
}

pub(crate) fn spawn_io(a: IoArgs, wake: OwnedFd) -> std::io::Result<IoThread> {
    let join = std::thread::Builder::new()
        .name("apoth-io".into())
        .spawn(move || io_main(a))?;
    Ok(IoThread { join, wake })
}

fn io_main(a: IoArgs) {
    let IoArgs {
        shared,
        master,
        wake_rx,
        to_main,
        to_render,
        hook,
    } = a;
    let mut buf = vec![0u8; SLICE_BYTES];
    if let Some(h) = &hook {
        h.write(&shared.lock_io());
    }
    let mut eof = false;
    while !eof {
        let deadline = if shared.hold.load(Ordering::Acquire) {
            shared.lock_io().next_deadline()
        } else {
            None
        };
        let timeout = deadline.map(|d| {
            let left = d.saturating_duration_since(Instant::now());
            Timespec {
                tv_sec: i64::try_from(left.as_secs()).unwrap_or(i64::MAX),
                tv_nsec: i64::from(left.subsec_nanos()),
            }
        });
        let (readable, stop) = {
            let mut fds = [
                PollFd::new(&master, PollFlags::IN),
                PollFd::new(&wake_rx, PollFlags::IN),
            ];
            match poll(&mut fds, timeout.as_ref()) {
                Ok(_) => (!fds[0].revents().is_empty(), !fds[1].revents().is_empty()),
                Err(Errno::INTR) => continue,
                Err(e) => {
                    tracing::error!(errno = e.raw_os_error(), "io poll failed");
                    break;
                }
            }
        };
        if stop {
            let mut b = [0u8; 16];
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
                let _ = to_main.send(MainMsg::Resized(dims));
                if !shared.dirty.swap(true, Ordering::AcqRel) {
                    let _ = to_render.send(RenderMsg::Poke);
                }
            }
            if !readable {
                continue;
            }
        }
        if !readable {
            // Timeout: a mode 2026 hold may have expired.
            let wake = shared.lock_io().tick(Instant::now());
            publish(&shared, &to_main, &to_render, wake, Vec::new(), None);
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
                    feed(&shared, &to_main, &to_render, &buf[..n]);
                    if total >= SLICE_BYTES || started.elapsed() >= SLICE_TIME {
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
    let _ = to_main.send(MainMsg::Eof);
}

fn feed(
    shared: &PaneShared,
    to_main: &calloop::channel::Sender<MainMsg>,
    to_render: &mpsc::Sender<RenderMsg>,
    bytes: &[u8],
) {
    let mut replies = Vec::new();
    let mut events = None;
    let wake = {
        let mut t = shared.lock_io();
        let wake = t.feed(bytes, Instant::now());
        while let Some(u) = t.replies().pop() {
            replies.push(u);
        }
        if wake.events {
            let slot = t.events();
            events = Some(PaneEvents {
                title: slot.title.take(),
                bell: slot.bell,
            });
            slot.clear();
        }
        wake
    };
    publish(shared, to_main, to_render, wake, replies, events);
}

fn publish(
    shared: &PaneShared,
    to_main: &calloop::channel::Sender<MainMsg>,
    to_render: &mpsc::Sender<RenderMsg>,
    wake: apoth_term::Wake,
    replies: Vec<Vec<u8>>,
    events: Option<PaneEvents>,
) {
    shared.hold.store(wake.hold, Ordering::Release);
    for r in replies {
        let _ = to_main.send(MainMsg::Reply(r));
    }
    if let Some(e) = events {
        let _ = to_main.send(MainMsg::Events(e));
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
        publish(&shared, &mtx, &tx, w, Vec::new(), None);
        publish(&shared, &mtx, &tx, w, Vec::new(), None);
        assert!(matches!(rx.try_recv(), Ok(RenderMsg::Poke)));
        assert!(rx.try_recv().is_err());
    }
}
