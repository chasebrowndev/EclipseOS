// SPDX-License-Identifier: AGPL-3.0-only
//! Frame pacing link to the render thread (T-07 §3, T-09 §2, RENDERING-55).
//!
//! [`crate::WlHandle`] is single-thread (it lives with the calloop loop). The render thread
//! gets a [`Pacer`] instead: `Send`, it issues `wl_surface.frame` and presentation feedback
//! requests itself (libwayland requests are thread-safe, the proxies are `Send + Sync`) and
//! receives the answers on a channel as [`PaceEvent`]s. The same `Frame` / `Presented` /
//! `Discarded` events still flow through the main [`crate::WlEvent`] stream; the pacer is an
//! additional consumer, never a replacement. The pacer also carries the surface size
//! (logical, buffer, scale) so the swapchain can be reconfigured without touching the loop.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll, Wake, Waker};
use std::time::{Duration, Instant};

use wayland_client::protocol::wl_callback::{self, WlCallback};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_region::WlRegion;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols::wp::presentation_time::client::wp_presentation::WpPresentation;
use wayland_protocols::wp::presentation_time::client::wp_presentation_feedback::{
    self, WpPresentationFeedback,
};
use wayland_protocols::xdg::shell::client::xdg_surface::XdgSurface;

use crate::clock::{Stamp, presentation_stamp};
use crate::dispatch::FeedbackData;
use crate::event::{PresentedEvent, WindowId};
use crate::scale::Scale;
use crate::window::Rect;

/// Feedback ids handed out by a pacer start here, so they never collide with the ids of
/// [`crate::WlHandle::request_presentation_feedback`].
const PACER_FEEDBACK_BASE: u64 = 1 << 63;

/// What the render thread is told.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PaceEvent {
    /// The frame callback fired: the compositor is ready for the next frame.
    Frame {
        /// Compositor time of the callback (ms, unspecified base).
        callback_ms: u32,
        /// Client stamp of the dispatch (CLOCK_MONOTONIC).
        stamp: Stamp,
    },
    /// `wp_presentation_feedback.presented`.
    Presented(PresentedEvent),
    /// The frame was never shown.
    Discarded {
        /// Feedback id.
        feedback: u64,
    },
    /// Surface geometry or scale changed (also sent once when the pacer is created).
    /// Reconfigure the swapchain to `buffer`; never derive it from `logical` yourself.
    Surface {
        /// Surface size, logical px (geometry plus CSD margin).
        logical: (u32, u32),
        /// Buffer size in physical px at `scale`: `round(logical * scale)` per axis.
        buffer: (u32, u32),
        /// Effective scale.
        scale: Scale,
        /// `xdg_toplevel` `suspended`: stop rendering (no frame callbacks will come).
        suspended: bool,
    },
}

/// Wakes the render thread when the main side has something for it.
type WakeFn = Arc<dyn Fn() + Send + Sync>;

/// Shared between the pacer and its sending half.
#[derive(Default)]
pub(crate) struct WakeCell(OnceLock<WakeFn>);

impl WakeCell {
    fn wake(&self) {
        if let Some(f) = self.0.get() {
            f();
        }
    }
}

/// The sending half, held by the window inside the loop.
#[derive(Clone)]
pub(crate) struct PaceTx(Sender<PaceEvent>, Arc<WakeCell>);

impl PaceTx {
    /// A dropped pacer makes this a no-op.
    pub(crate) fn send(&self, ev: PaceEvent) -> bool {
        let ok = self.0.send(ev).is_ok();
        if ok {
            self.1.wake();
        }
        ok
    }
}

/// Per-window double-buffered state the render thread commits with its frame (T-07 §3.1):
/// the configure ack, window geometry and input region. Main writes it; only the render
/// thread's pre-present hook applies it, and only when the swapchain matches `buffer`.
#[derive(Debug, Default)]
pub(crate) struct CommitSlot {
    inner: Mutex<SlotState>,
}

#[derive(Debug, Default)]
struct SlotState {
    /// Newest configure serial not yet acked.
    ack: Option<u32>,
    /// Buffer size the newest configure and scale call for.
    buffer: (u32, u32),
    geometry: Option<Rect>,
    /// `Some(None)` clears the input region.
    input: Option<Option<Rect>>,
}

/// What the hook applies for one commit.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Due {
    pub ack: Option<u32>,
    pub geometry: Option<Rect>,
    pub input: Option<Option<Rect>>,
}

impl CommitSlot {
    fn lock(&self) -> std::sync::MutexGuard<'_, SlotState> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn set_buffer(&self, b: (u32, u32)) {
        self.lock().buffer = b;
    }

    pub(crate) fn set_ack(&self, serial: u32) {
        self.lock().ack = Some(serial);
    }

    pub(crate) fn set_geometry(&self, r: Rect) {
        self.lock().geometry = Some(r);
    }

    pub(crate) fn set_input(&self, r: Option<Rect>) {
        self.lock().input = Some(r);
    }

    /// True while a configure waits for its ack.
    pub(crate) fn ack_pending(&self) -> bool {
        self.lock().ack.is_some()
    }

    /// Everything due for a commit whose swapchain is `size`; nothing while it differs from
    /// the configured buffer (the ack waits for the frame that matches).
    pub(crate) fn take_due(&self, size: (u32, u32)) -> Due {
        let mut s = self.lock();
        if s.buffer != size {
            return Due::default();
        }
        Due {
            ack: s.ack.take(),
            geometry: s.geometry.take(),
            input: s.input.take(),
        }
    }
}

/// What the render thread tells the pre-present hook about the frame being committed.
#[derive(Debug, Default)]
pub struct CommitCtl {
    frame: std::sync::atomic::AtomicBool,
    size: AtomicU64,
}

impl CommitCtl {
    /// Whether this frame wants a `wl_surface.frame` callback.
    pub fn set_frame(&self, want: bool) {
        self.frame.store(want, Ordering::Release);
    }

    /// The swapchain size (physical px) this frame is drawn at.
    pub fn set_size(&self, size: (u32, u32)) {
        self.size.store(
            u64::from(size.0) << 32 | u64::from(size.1),
            Ordering::Release,
        );
    }

    fn size(&self) -> (u32, u32) {
        let v = self.size.load(Ordering::Acquire);
        ((v >> 32) as u32, v as u32)
    }
}

/// State of the render-owned event queue: frame callbacks and presentation feedback for the
/// window surface are created on it, and main's queue never sees their events (T-07 §3.1).
#[derive(Default)]
struct PaceState {
    out: VecDeque<PaceEvent>,
    clock: Option<u32>,
}

delegate_noop!(PaceState: ignore WlRegion);

impl Dispatch<WlCallback, WindowId> for PaceState {
    fn event(
        state: &mut Self,
        _: &WlCallback,
        event: wl_callback::Event,
        _: &WindowId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { callback_data } = event {
            state.out.push_back(PaceEvent::Frame {
                callback_ms: callback_data,
                stamp: Stamp::now(),
            });
        }
    }
}

impl Dispatch<WpPresentationFeedback, FeedbackData> for PaceState {
    fn event(
        state: &mut Self,
        _: &WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        data: &FeedbackData,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wp_presentation_feedback::Event::Presented {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                refresh,
                seq_hi,
                seq_lo,
                flags,
            } => {
                state.out.push_back(PaceEvent::Presented(PresentedEvent {
                    window: data.window,
                    feedback: data.id,
                    time: presentation_stamp(state.clock, tv_sec_hi, tv_sec_lo, tv_nsec),
                    refresh_ns: refresh,
                    seq: crate::clock::join64(seq_hi, seq_lo),
                    flags: u32::from(flags),
                }));
            }
            wp_presentation_feedback::Event::Discarded => {
                state
                    .out
                    .push_back(PaceEvent::Discarded { feedback: data.id });
            }
            _ => {}
        }
    }
}

/// Request issuing for the window surface on the render queue; cloned into the hook.
#[derive(Clone)]
struct Requests {
    window: WindowId,
    surface: WlSurface,
    qh: QueueHandle<PaceState>,
    presentation: Option<WpPresentation>,
    next_feedback: Arc<AtomicU64>,
}

impl Requests {
    fn frame(&self) {
        self.surface.frame(&self.qh, self.window);
    }

    fn feedback(&self) -> Option<u64> {
        let p = self.presentation.as_ref()?;
        let id = self.next_feedback.fetch_add(1, Ordering::Relaxed);
        p.feedback(
            &self.surface,
            &self.qh,
            FeedbackData {
                window: self.window,
                id,
            },
        );
        Some(id)
    }
}

struct FnWake(WakeFn);

impl Wake for FnWake {
    fn wake(self: Arc<Self>) {
        (self.0)();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        (self.0)();
    }
}

/// Render-thread side of a window's frame pacing. `Send`; not `Sync` (one consumer).
///
/// Owns a dedicated `wl_event_queue`: the window surface's frame callbacks and presentation
/// feedback are created on it and dispatched by [`Pacer::try_recv`] on the render thread.
pub struct Pacer {
    req: Requests,
    queue: EventQueue<PaceState>,
    state: PaceState,
    conn: Connection,
    xdg_surface: XdgSurface,
    compositor: WlCompositor,
    slot: Arc<CommitSlot>,
    wake: Arc<WakeCell>,
    waker: Option<Waker>,
    rx: Receiver<PaceEvent>,
}

impl std::fmt::Debug for Pacer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pacer")
            .field("window", &self.req.window)
            .field("presentation", &self.req.presentation.is_some())
            .finish_non_exhaustive()
    }
}

/// Everything a pacer is built from.
pub(crate) struct PacerParts {
    pub window: WindowId,
    pub surface: WlSurface,
    pub conn: Connection,
    pub presentation: Option<WpPresentation>,
    pub clock: Option<u32>,
    pub xdg_surface: XdgSurface,
    pub compositor: WlCompositor,
    pub slot: Arc<CommitSlot>,
}

impl Pacer {
    pub(crate) fn new(p: PacerParts) -> (Pacer, PaceTx) {
        let (tx, rx) = mpsc::channel();
        let queue = p.conn.new_event_queue::<PaceState>();
        let wake = Arc::new(WakeCell::default());
        (
            Pacer {
                req: Requests {
                    window: p.window,
                    surface: p.surface,
                    qh: queue.handle(),
                    presentation: p.presentation,
                    next_feedback: Arc::new(AtomicU64::new(PACER_FEEDBACK_BASE)),
                },
                queue,
                state: PaceState {
                    out: VecDeque::new(),
                    clock: p.clock,
                },
                conn: p.conn,
                xdg_surface: p.xdg_surface,
                compositor: p.compositor,
                slot: p.slot,
                wake: Arc::clone(&wake),
                waker: None,
                rx,
            },
            PaceTx(tx, wake),
        )
    }

    /// Install the function that wakes the render thread when an event for it arrives
    /// (from main's channel or from the render queue). Call once, before the first
    /// [`Pacer::try_recv`]; it must not block.
    pub fn set_wake(&mut self, f: impl Fn() + Send + Sync + 'static) {
        let f: WakeFn = Arc::new(f);
        let _ = self.wake.0.set(Arc::clone(&f));
        self.waker = Some(Waker::from(Arc::new(FnWake(f))));
    }

    /// The window this pacer belongs to.
    pub fn window(&self) -> WindowId {
        self.req.window
    }

    /// Whether `wp_presentation` is bound (feedback requests return `Some`).
    pub fn has_presentation(&self) -> bool {
        self.req.presentation.is_some()
    }

    /// Ask for a frame callback; it fires after the next commit (the wgpu present). Normally
    /// the commit hook does this; calling it directly is for tests.
    pub fn request_frame(&self) {
        self.req.frame();
    }

    /// Ask for presentation feedback for the next commit. `None` without `wp_presentation`.
    /// The id matches `Presented` / `Discarded`.
    pub fn request_presentation_feedback(&mut self) -> Option<u64> {
        self.req.feedback()
    }

    /// True while a configure has not been acked yet (the next matching frame acks it).
    pub fn ack_pending(&self) -> bool {
        self.slot.ack_pending()
    }

    /// The hook for `SurfacePresenter::set_pre_present`. Runs on the render thread right
    /// before the present that commits the surface and sends, in this order: the pending
    /// `ack_configure` and window geometry and input region (only when `ctl`'s size is the
    /// configured buffer size), `wl_surface.frame` when `ctl` asks, then
    /// `wp_presentation.feedback`, then flushes. It runs only for a frame that is committed.
    pub fn commit_hook(&self, ctl: Arc<CommitCtl>) -> Box<dyn FnMut() + Send> {
        let req = self.req.clone();
        let slot = Arc::clone(&self.slot);
        let xdg = self.xdg_surface.clone();
        let comp = self.compositor.clone();
        let conn = self.conn.clone();
        Box::new(move || {
            let due = slot.take_due(ctl.size());
            if let Some(serial) = due.ack {
                xdg.ack_configure(serial);
            }
            if let Some(g) = due.geometry {
                xdg.set_window_geometry(g.x, g.y, g.w, g.h);
            }
            if let Some(i) = due.input {
                match i {
                    Some(r) => {
                        let region = comp.create_region(&req.qh, ());
                        region.add(r.x, r.y, r.w, r.h);
                        req.surface.set_input_region(Some(&region));
                        region.destroy();
                    }
                    None => req.surface.set_input_region(None),
                }
            }
            if ctl.frame.load(Ordering::Acquire) {
                req.frame();
            }
            let _ = req.feedback();
            let _ = conn.flush();
        })
    }

    /// Flush queued requests to the compositor.
    pub fn flush(&self) {
        let _ = self.conn.flush();
    }

    /// Dispatch what the render queue has pending (the socket is read by main's loop, which
    /// distributes events to this queue) and register the wake-up for the next arrival.
    fn pump(&mut self) {
        let waker = self.waker.clone().unwrap_or_else(|| Waker::noop().clone());
        let mut cx = Context::from_waker(&waker);
        if let Poll::Ready(Err(e)) = self.queue.poll_dispatch_pending(&mut cx, &mut self.state) {
            tracing::warn!(error = %e, "render event queue failed");
        }
    }

    /// Next event if one is ready. Dispatches the render queue on the calling (render)
    /// thread.
    pub fn try_recv(&mut self) -> Option<PaceEvent> {
        self.pump();
        if let Some(e) = self.state.out.pop_front() {
            return Some(e);
        }
        self.rx.try_recv().ok()
    }

    /// Wait up to `timeout` for the next event (polls every millisecond). `None` on timeout.
    pub fn recv_timeout(&mut self, timeout: Duration) -> Option<PaceEvent> {
        let end = Instant::now() + timeout;
        loop {
            if let Some(e) = self.try_recv() {
                return Some(e);
            }
            if Instant::now() >= end {
                return None;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send<T: Send>() {}

    #[test]
    fn pacer_and_events_are_send() {
        assert_send::<Pacer>();
        assert_send::<PaceEvent>();
        assert_send::<crate::WlSurfaceHandle>();
    }

    #[test]
    fn channel_round_trip_and_drop() {
        let (tx, rx) = mpsc::channel();
        let tx = PaceTx(tx, Arc::new(WakeCell::default()));
        assert!(tx.send(PaceEvent::Discarded { feedback: 7 }));
        assert_eq!(rx.try_recv(), Ok(PaceEvent::Discarded { feedback: 7 }));
        drop(rx);
        assert!(!tx.send(PaceEvent::Discarded { feedback: 8 }));
    }

    #[test]
    fn slot_acks_only_the_matching_swapchain() {
        let slot = CommitSlot::default();
        slot.set_buffer((800, 600));
        slot.set_ack(7);
        slot.set_geometry(Rect {
            x: 1,
            y: 2,
            w: 3,
            h: 4,
        });
        assert!(slot.ack_pending());
        // A stale swapchain gets nothing and keeps the ack.
        assert_eq!(slot.take_due((640, 480)), Due::default());
        assert!(slot.ack_pending());
        let due = slot.take_due((800, 600));
        assert_eq!(due.ack, Some(7));
        assert!(due.geometry.is_some() && due.input.is_none());
        assert!(!slot.ack_pending());
        assert_eq!(slot.take_due((800, 600)), Due::default());
    }

    #[test]
    fn slot_newest_serial_wins() {
        let slot = CommitSlot::default();
        slot.set_buffer((10, 10));
        slot.set_ack(1);
        slot.set_ack(2);
        slot.set_input(None);
        let due = slot.take_due((10, 10));
        assert_eq!((due.ack, due.input), (Some(2), Some(None)));
    }

    /// T-07 §6 commit ownership, static half: once a window has a pacer, main's code has no
    /// `wl_surface.commit` (only window creation's role-assigning commit and the buffer-attach
    /// helper for windows without a GPU surface) and acks a configure only when no pacer
    /// exists; the render thread's hook is the other ack site.
    #[test]
    fn main_side_commit_and_ack_sites_are_the_known_ones() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut commits = Vec::new();
        let mut acks = Vec::new();
        for e in std::fs::read_dir(dir).unwrap() {
            let path = e.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&path).unwrap();
            let code = text.split("#[cfg(test)]").next().unwrap();
            for _ in code.matches("surface.commit()") {
                commits.push(name.clone());
            }
            for _ in code.matches(".ack_configure(") {
                acks.push(name.clone());
            }
        }
        commits.sort();
        acks.sort();
        assert_eq!(commits, ["shmbuf.rs", "state.rs"]);
        assert_eq!(acks, ["pace.rs", "state.rs"]);
    }

    #[test]
    fn commit_ctl_packs_size() {
        let c = CommitCtl::default();
        c.set_size((3840, 2160));
        assert_eq!(c.size(), (3840, 2160));
    }

    #[test]
    fn feedback_ids_do_not_collide_with_handle_ids() {
        assert!(PACER_FEEDBACK_BASE > u64::from(u32::MAX));
    }
}
