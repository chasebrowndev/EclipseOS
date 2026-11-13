// SPDX-License-Identifier: AGPL-3.0-only
//! The calloop event source and the application handle (T-09 §2).
//!
//! The display fd is a calloop source using prepare-read / read / dispatch-pending. The loop
//! flushes before sleeping; on `EAGAIN` it waits for `POLLOUT`; on `EPIPE` it reads once more
//! so the protocol error is surfaced. Protocol errors are fatal and logged with interface
//! and code. `CLOCK_MONOTONIC` is read when the fd becomes readable, before dispatch, and
//! stamped on every input event of that dispatch.

use std::cell::RefCell;
use std::io;
use std::rc::Rc;

use calloop::generic::Generic;
use calloop::{EventSource, Interest, Mode, Poll, PostAction, Readiness, Token, TokenFactory};
use rustix::io::Errno;
use wayland_backend::client::{ReadEventsGuard, WaylandError};
use wayland_client::{Connection, DispatchError, EventQueue};

use crate::caps::{BlurPath, CapabilityTable};
use crate::clock::{Stamp, Timerfd};
use crate::cursor::CursorShape;
use crate::event::{FatalError, OutputInfo, SeatId, SelectionKind, WindowId, WlEvent};
use crate::mime::Want;
use crate::rawhandle::WlSurfaceHandle;
use crate::scale::Scale;
use crate::seat::{ClipboardError, CursorError};
use crate::state::{BlurRequest, WindowSpec, WlConfig, WlError, WlState};
use crate::transfer::Transfer;
use crate::window::{CsdMetrics, FrameKind, Rect, ResizeEdge, WindowStates, clamp_size};

/// The Wayland connection as a calloop event source. Events come out as [`WlEvent`]; insert
/// with `handle.insert_source(source, |event, _, data| ...)`.
pub struct WlSource {
    queue: EventQueue<WlState>,
    state: Rc<RefCell<WlState>>,
    conn_source: Generic<Connection>,
    timer_source: Generic<Rc<Timerfd>>,
    read_guard: Option<ReadEventsGuard>,
    fake_token: Option<Token>,
    stored_error: Result<(), io::Error>,
    stamp: Stamp,
    stamp_fresh: bool,
    write_blocked: bool,
}

impl std::fmt::Debug for WlSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WlSource")
            .field("write_blocked", &self.write_blocked)
            .finish_non_exhaustive()
    }
}

/// Connect to `$WAYLAND_DISPLAY`, bind the globals, and return the source plus its handle.
pub fn connect(cfg: WlConfig) -> Result<(WlSource, WlHandle), WlError> {
    let (conn, queue, state) = WlState::connect(cfg)?;
    let timer = state.timer.clone();
    let state = Rc::new(RefCell::new(state));
    let source = WlSource {
        queue,
        state: state.clone(),
        conn_source: Generic::new(conn, Interest::READ, Mode::Level),
        timer_source: Generic::new(timer, Interest::READ, Mode::Level),
        read_guard: None,
        fake_token: None,
        stored_error: Ok(()),
        stamp: Stamp::now(),
        stamp_fresh: false,
        write_blocked: false,
    };
    Ok((source, WlHandle { state }))
}

enum Flush {
    Ok,
    Blocked,
    Fatal(FatalError),
}

impl WlSource {
    fn conn(&self) -> &Connection {
        self.conn_source.get_ref()
    }

    fn fatal_from(p: &wayland_backend::protocol::ProtocolError) -> FatalError {
        FatalError {
            interface: p.object_interface.clone(),
            code: p.code,
            object_id: p.object_id,
            message: p.message.clone(),
        }
    }

    fn flush(&mut self) -> Result<Flush, io::Error> {
        match self.queue.flush() {
            Ok(()) => Ok(Flush::Ok),
            Err(WaylandError::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => Ok(Flush::Blocked),
            Err(WaylandError::Io(e)) if e.kind() == io::ErrorKind::BrokenPipe => {
                // The compositor hung up, usually after a protocol error: read once more so
                // the error is surfaced.
                if let Some(g) = self.queue.prepare_read() {
                    let _ = g.read();
                }
                match self.conn().protocol_error() {
                    Some(p) => Ok(Flush::Fatal(Self::fatal_from(&p))),
                    None => Err(e),
                }
            }
            Err(WaylandError::Io(e)) => Err(e),
            Err(WaylandError::Protocol(p)) => Ok(Flush::Fatal(Self::fatal_from(&p))),
        }
    }

    fn deliver<F: FnMut(WlEvent, &mut ())>(&mut self, callback: &mut F) {
        // Handlers may queue more events through the handle; drain until quiet.
        for _ in 0..8 {
            let evs: Vec<WlEvent> = self.state.borrow_mut().events.drain(..).collect();
            if evs.is_empty() {
                break;
            }
            for ev in evs {
                callback(ev, &mut ());
            }
        }
    }

    fn die<F: FnMut(WlEvent, &mut ())>(
        &mut self,
        f: FatalError,
        callback: &mut F,
    ) -> calloop::Error {
        tracing::error!(interface = %f.interface, code = f.code, object = f.object_id, "wayland protocol error");
        callback(WlEvent::Fatal(f), &mut ());
        io::Error::from(Errno::PROTO).into()
    }
}

impl EventSource for WlSource {
    type Event = WlEvent;
    type Metadata = ();
    type Ret = ();
    type Error = calloop::Error;

    const NEEDS_EXTRA_LIFECYCLE_EVENTS: bool = true;

    fn process_events<F>(
        &mut self,
        _readiness: Readiness,
        _token: Token,
        mut callback: F,
    ) -> Result<PostAction, Self::Error>
    where
        F: FnMut(Self::Event, &mut Self::Metadata) -> Self::Ret,
    {
        self.read_guard = None;
        std::mem::replace(&mut self.stored_error, Ok(()))?;
        if !self.stamp_fresh {
            self.stamp = Stamp::now();
        }
        self.stamp_fresh = false;

        // Key repeat: due ticks are stamped now.
        self.timer_source.get_ref().drain();
        self.state.borrow_mut().tick_repeat(Stamp::now());

        let mut fatal: Option<FatalError> = None;
        loop {
            let r = {
                let mut st = self.state.borrow_mut();
                st.stamp = self.stamp;
                self.queue.dispatch_pending(&mut *st)
            };
            match r {
                Ok(0) => break,
                Ok(_) => {}
                Err(DispatchError::Backend(WaylandError::Protocol(p))) => {
                    fatal = Some(Self::fatal_from(&p));
                    break;
                }
                Err(DispatchError::Backend(WaylandError::Io(e))) => return Err(e.into()),
                Err(DispatchError::BadMessage {
                    interface,
                    sender_id,
                    opcode,
                }) => {
                    fatal = Some(FatalError {
                        interface: interface.to_string(),
                        code: u32::from(opcode),
                        object_id: sender_id.protocol_id(),
                        message: "bad message".into(),
                    });
                    break;
                }
            }
        }
        self.deliver(&mut callback);
        if let Some(f) = fatal {
            return Err(self.die(f, &mut callback));
        }

        // Flush; wait for POLLOUT only while the socket is full.
        let mut post = PostAction::Continue;
        match self.flush()? {
            Flush::Ok => {
                if self.write_blocked {
                    self.write_blocked = false;
                    self.conn_source.interest = Interest::READ;
                    post = PostAction::Reregister;
                }
            }
            Flush::Blocked => {
                if !self.write_blocked {
                    self.write_blocked = true;
                    self.conn_source.interest = Interest::BOTH;
                    post = PostAction::Reregister;
                }
            }
            Flush::Fatal(f) => return Err(self.die(f, &mut callback)),
        }
        Ok(post)
    }

    fn register(
        &mut self,
        poll: &mut Poll,
        token_factory: &mut TokenFactory,
    ) -> calloop::Result<()> {
        self.fake_token = Some(token_factory.token());
        self.conn_source.register(poll, token_factory)?;
        self.timer_source.register(poll, token_factory)
    }

    fn reregister(
        &mut self,
        poll: &mut Poll,
        token_factory: &mut TokenFactory,
    ) -> calloop::Result<()> {
        self.conn_source.reregister(poll, token_factory)?;
        self.timer_source.reregister(poll, token_factory)
    }

    fn unregister(&mut self, poll: &mut Poll) -> calloop::Result<()> {
        self.conn_source.unregister(poll)?;
        self.timer_source.unregister(poll)
    }

    fn before_sleep(&mut self) -> calloop::Result<Option<(Readiness, Token)>> {
        self.read_guard = None;
        let fake = self.fake_token.map(|t| (Readiness::EMPTY, t));

        // Events produced outside a dispatch (handle calls, connect) are delivered at once.
        if !self.state.borrow().events.is_empty() {
            return Ok(fake);
        }
        match self.flush()? {
            Flush::Ok => {}
            // First time the socket is full: wake once so `process_events` switches to POLLOUT.
            Flush::Blocked if !self.write_blocked => return Ok(fake),
            Flush::Blocked => {}
            Flush::Fatal(_) => return Ok(fake),
        }
        self.read_guard = self.queue.prepare_read();
        match self.read_guard {
            Some(_) => Ok(None),
            // Events are already queued: dispatch them instead of sleeping.
            None => Ok(fake),
        }
    }

    fn before_handle_events(&mut self, events: calloop::EventIterator<'_>) {
        // The client stamp: read when the fd became readable, before the read and dispatch.
        self.stamp = Stamp::now();
        self.stamp_fresh = true;
        // The guard must not be held while process_events runs: the application may use the
        // socket (the Vulkan WSI does when creating a surface).
        let guard = self.read_guard.take();
        if events.count() > 0
            && let Some(Err(WaylandError::Io(err))) = guard.map(ReadEventsGuard::read)
            && err.kind() != io::ErrorKind::WouldBlock
        {
            self.stored_error = Err(err);
        }
    }
}

/// The application's handle to the client: windows, seat actions, clipboard, IME. Cheap to
/// clone; main-thread only. Do not call it from inside an event callback of another handle
/// method (callbacks run with no borrow held, so calling from a callback is fine).
#[derive(Clone)]
pub struct WlHandle {
    state: Rc<RefCell<WlState>>,
}

impl std::fmt::Debug for WlHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WlHandle")
    }
}

impl WlHandle {
    fn with<R>(&self, f: impl FnOnce(&mut WlState) -> R) -> R {
        f(&mut self.state.borrow_mut())
    }

    fn win<R>(&self, id: WindowId, f: impl FnOnce(&mut WlState, usize) -> R) -> Option<R> {
        let mut st = self.state.borrow_mut();
        let wi = st.win_index(id)?;
        Some(f(&mut st, wi))
    }

    // ----- windows -----

    /// Create a toplevel (no buffer attached; the first configure arrives as an event).
    pub fn create_window(&self, spec: WindowSpec) -> WindowId {
        self.with(|s| s.create_window(spec))
    }

    /// Destroy a window: roles, then `wl_surface`. Drop the swapchain first.
    pub fn destroy_window(&self, id: WindowId) {
        self.with(|s| s.destroy_window(id));
    }

    /// `xdg_toplevel.set_title`.
    pub fn set_title(&self, id: WindowId, title: &str) {
        self.win(id, |s, wi| {
            s.windows[wi]
                .toplevel
                .set_title(crate::window::clamp_title(title))
        });
    }

    /// Ask for an xdg-activation token for a child launch; it arrives as
    /// `WlEvent::ActivationToken`. `false` when the compositor lacks xdg-activation.
    pub fn request_activation_token(&self, id: WindowId) -> bool {
        self.with(|s| s.request_activation_token(id))
    }

    /// Urgency: ask the compositor to draw attention to the window (xdg-activation).
    pub fn request_attention(&self, id: WindowId) -> bool {
        self.with(|s| s.request_attention(id))
    }

    /// Activate the window with a token handed to us by whoever launched us.
    pub fn activate_with_token(&self, id: WindowId, token: &str) -> bool {
        self.with(|s| s.activate_with_token(id, token.to_string()))
    }

    /// Ring the system bell (xdg-system-bell); `false` when unavailable.
    pub fn ring_bell(&self, id: Option<WindowId>) -> bool {
        self.with(|s| s.ring_bell(id))
    }

    /// Per-window icon by XDG icon name (`None` clears); `false` when unavailable.
    pub fn set_icon_name(&self, id: WindowId, name: Option<&str>) -> bool {
        self.win(id, |s, wi| s.set_icon_name(wi, name))
            .unwrap_or(false)
    }

    /// Stable tag and description for compositor window rules; `false` when unavailable.
    pub fn set_toplevel_tag(&self, id: WindowId, tag: &str, description: &str) -> bool {
        self.win(id, |s, wi| s.set_toplevel_tag(wi, tag, description))
            .unwrap_or(false)
    }

    /// Content type hint (`video` while a media placement plays fullscreen-pane).
    pub fn set_content_type(&self, id: WindowId, ty: crate::ContentType) -> bool {
        self.win(id, |s, wi| s.set_content_type(wi, ty))
            .unwrap_or(false)
    }

    /// Inhibit or release screen idle for the window (`media.inhibit-idle`).
    pub fn set_idle_inhibit(&self, id: WindowId, on: bool) -> bool {
        self.win(id, |s, wi| s.set_idle_inhibit(wi, on))
            .unwrap_or(false)
    }

    /// Attach a solid XRGB8888 buffer of the given buffer size and commit: maps a window
    /// that has no GPU surface (tests, software fallback). Never on a WSI-owned surface.
    pub fn attach_solid_buffer(
        &self,
        id: WindowId,
        size: (u32, u32),
        xrgb: u32,
    ) -> Result<(), WlError> {
        self.with(|s| s.attach_solid(id, size.0, size.1, xrgb))
    }

    /// Minimum size in logical px (`None` clears).
    pub fn set_min_size(&self, id: WindowId, size: Option<(u32, u32)>) {
        let (w, h) = clamp_size(size.unwrap_or((0, 0)));
        self.win(id, |s, wi| {
            s.windows[wi].toplevel.set_min_size(w as i32, h as i32)
        });
    }

    /// Maximum size in logical px (`None` clears).
    pub fn set_max_size(&self, id: WindowId, size: Option<(u32, u32)>) {
        let (w, h) = clamp_size(size.unwrap_or((0, 0)));
        self.win(id, |s, wi| {
            s.windows[wi].toplevel.set_max_size(w as i32, h as i32)
        });
    }

    /// Maximize or restore.
    pub fn set_maximized(&self, id: WindowId, on: bool) {
        self.win(id, |s, wi| {
            let t = &s.windows[wi].toplevel;
            if on {
                t.set_maximized()
            } else {
                t.unset_maximized()
            }
        });
    }

    /// Fullscreen or restore (on the current output).
    pub fn set_fullscreen(&self, id: WindowId, on: bool) {
        self.win(id, |s, wi| {
            let t = &s.windows[wi].toplevel;
            if on {
                t.set_fullscreen(None)
            } else {
                t.unset_fullscreen()
            }
        });
    }

    /// Ask for a `wl_surface.frame` callback; it fires after the next commit.
    pub fn request_frame(&self, id: WindowId) {
        self.win(id, |s, wi| {
            s.windows[wi].surface.frame(&s.qh, id);
        });
    }

    /// Ask for presentation feedback for the next commit. `None` without `wp_presentation`.
    /// The returned id matches the `Presented` / `PresentationDiscarded` event.
    pub fn request_presentation_feedback(&self, id: WindowId) -> Option<u64> {
        self.win(id, |s, wi| {
            let p = s.globals.presentation.as_ref()?;
            let fid = s.next_feedback;
            s.next_feedback += 1;
            p.feedback(
                &s.windows[wi].surface,
                &s.qh,
                crate::dispatch::FeedbackData {
                    window: id,
                    id: fid,
                },
            );
            Some(fid)
        })
        .flatten()
    }

    /// Set the window geometry size when the compositor left it to us (configure size `None`).
    pub fn set_window_size(&self, id: WindowId, size: (u32, u32)) {
        self.win(id, |s, wi| {
            s.windows[wi].window_size = clamp_size(size);
            s.relayout(wi);
        });
    }

    /// Current scale of a window.
    pub fn scale(&self, id: WindowId) -> Option<Scale> {
        self.win(id, |s, wi| s.windows[wi].scale)
    }

    /// Surface size in logical px (geometry plus CSD shadow margin) and its buffer size at
    /// the current scale: what the swapchain must be.
    pub fn surface_size(&self, id: WindowId) -> Option<((u32, u32), (u32, u32))> {
        self.win(id, |s, wi| {
            let w = &s.windows[wi];
            (w.logical, w.scale.to_buffer_size(w.logical))
        })
    }

    /// Who draws the frame now.
    pub fn frame_kind(&self, id: WindowId) -> Option<FrameKind> {
        self.win(id, |s, wi| s.windows[wi].frame)
    }

    /// Last configured states.
    pub fn states(&self, id: WindowId) -> Option<WindowStates> {
        self.win(id, |s, wi| s.windows[wi].states)
    }

    /// Update CSD sizes (relayouts immediately).
    pub fn set_csd_metrics(&self, id: WindowId, m: CsdMetrics) {
        self.win(id, |s, wi| {
            s.windows[wi].csd_metrics = m;
            s.relayout(wi);
        });
    }

    /// The CSD layout, when we are drawing the frame.
    pub fn csd_layout(&self, id: WindowId) -> Option<crate::window::CsdLayout> {
        self.win(id, |s, wi| s.windows[wi].csd_layout).flatten()
    }

    // ----- blur -----

    /// Request a blur region (`None` removes it). Committed with the frame that draws it.
    pub fn set_blur(&self, id: WindowId, req: Option<BlurRequest>) {
        self.win(id, |s, wi| {
            s.windows[wi].blur_request = req;
            s.apply_blur_region(wi);
        });
    }

    /// Which blur path is active.
    pub fn blur_path(&self, id: WindowId) -> Option<BlurPath> {
        self.win(id, |s, wi| s.windows[wi].blur_path)
    }

    // ----- CSD actions -----

    /// Title drag.
    pub fn begin_move(&self, id: WindowId, seat: SeatId) -> Result<(), WlError> {
        self.with(|s| s.begin_move(id, seat))
    }

    /// Edge drag.
    pub fn begin_resize(
        &self,
        id: WindowId,
        seat: SeatId,
        edge: ResizeEdge,
    ) -> Result<(), WlError> {
        self.with(|s| s.begin_resize(id, seat, edge))
    }

    /// Window menu at surface-local (x, y).
    pub fn show_window_menu(
        &self,
        id: WindowId,
        seat: SeatId,
        x: i32,
        y: i32,
    ) -> Result<(), WlError> {
        self.with(|s| s.show_window_menu(id, seat, x, y))
    }

    // ----- cursor -----

    /// Set the base cursor shape (what the UI wants over the surface).
    pub fn set_cursor_base(&self, seat: SeatId, shape: CursorShape) -> Result<(), CursorError> {
        self.with(|s| {
            s.cursor_stack_mut(seat)
                .ok_or(CursorError::NoSeat)?
                .set_base(shape);
            s.apply_cursor(seat)
        })
    }

    /// OSC 22 push.
    pub fn push_cursor_shape(&self, seat: SeatId, shape: CursorShape) -> Result<(), CursorError> {
        self.with(|s| {
            s.cursor_stack_mut(seat)
                .ok_or(CursorError::NoSeat)?
                .push(shape);
            s.apply_cursor(seat)
        })
    }

    /// OSC 22 pop.
    pub fn pop_cursor_shape(&self, seat: SeatId) -> Result<(), CursorError> {
        self.with(|s| {
            s.cursor_stack_mut(seat).ok_or(CursorError::NoSeat)?.pop();
            s.apply_cursor(seat)
        })
    }

    /// Hide or show the pointer over our surface.
    pub fn set_cursor_hidden(&self, seat: SeatId, hidden: bool) -> Result<(), CursorError> {
        self.with(|s| {
            s.set_cursor_hidden(seat, hidden);
            s.apply_cursor(seat)
        })
    }

    // ----- selections -----

    /// Own the clipboard or primary selection with UTF-8 text.
    pub fn set_selection(
        &self,
        seat: SeatId,
        kind: SelectionKind,
        data: Vec<u8>,
    ) -> Result<(), ClipboardError> {
        self.with(|s| s.set_selection(seat, kind, data))
    }

    /// Release ownership.
    pub fn clear_selection(&self, seat: SeatId, kind: SelectionKind) -> Result<(), ClipboardError> {
        self.with(|s| s.clear_selection(seat, kind))
    }

    /// True while our source is the selection.
    pub fn owns_selection(&self, seat: SeatId, kind: SelectionKind) -> bool {
        self.with(|s| s.owns_selection(seat, kind))
    }

    /// MIME types of the current foreign selection (may be our own: check `owns_selection`).
    pub fn selection_mimes(&self, seat: SeatId, kind: SelectionKind) -> Vec<String> {
        self.with(|s| s.selection_mimes(seat, kind))
    }

    /// Start reading the selection; insert the transfer into the loop with
    /// [`Transfer::insert`].
    pub fn receive_selection(
        &self,
        seat: SeatId,
        kind: SelectionKind,
        want: Want,
    ) -> Result<Transfer, ClipboardError> {
        self.with(|s| s.receive_selection(seat, kind, want))
    }

    // ----- IME -----

    /// Enable text-input-v3 on a seat.
    pub fn ime_enable(&self, seat: SeatId) {
        self.with(|s| s.ime_enable(seat));
    }

    /// Disable text-input-v3.
    pub fn ime_disable(&self, seat: SeatId) {
        self.with(|s| s.ime_disable(seat));
    }

    /// Report the cursor rectangle whenever the cursor moves.
    pub fn ime_set_cursor_rect(&self, seat: SeatId, r: Rect) {
        self.with(|s| s.ime_set_cursor_rect(seat, r));
    }

    /// Report surrounding text (at most 4000 bytes) with cursor and anchor byte offsets.
    pub fn ime_set_surrounding(&self, seat: SeatId, text: String, cursor: i32, anchor: i32) {
        self.with(|s| s.ime_set_surrounding(seat, text, cursor, anchor));
    }

    // ----- keyboard / scroll configuration -----

    /// Install the repeat filter (`xkb_keymap_key_repeats` once the keymap is parsed).
    pub fn set_repeat_filter(&self, f: Box<dyn Fn(u32) -> bool>) {
        self.with(|s| s.repeat_filter = f);
    }

    /// `keyboard.repeat-rate` / `keyboard.repeat-delay` overrides (live).
    pub fn set_repeat_overrides(&self, rate_hz: Option<u32>, delay_ms: Option<u32>) {
        self.with(|s| {
            for seat in &mut s.seats {
                seat.repeater.set_overrides(rate_hz, delay_ms);
            }
        });
    }

    /// Lines per wheel detent.
    pub fn set_axis_multiplier(&self, m: f64) {
        self.with(|s| {
            for seat in &mut s.seats {
                seat.scroll.set_multiplier(m);
            }
        });
    }

    // ----- queries -----

    /// Known seats.
    pub fn seats(&self) -> Vec<SeatId> {
        self.with(|s| s.seats.iter().map(|x| x.id).collect())
    }

    /// Known outputs.
    pub fn outputs(&self) -> Vec<OutputInfo> {
        self.with(|s| {
            s.outputs
                .iter()
                .filter(|o| o.announced)
                .map(|o| o.info.clone())
                .collect()
        })
    }

    /// Running on Abyss?
    pub fn on_abyss(&self) -> bool {
        self.with(|s| s.on_abyss)
    }

    /// The protocol capability table (T-09 §3).
    pub fn capabilities(&self) -> CapabilityTable {
        self.with(|s| s.caps.clone())
    }

    /// Text for the `wayland-info` Action: the global table plus this window's resolved state.
    pub fn wayland_info(&self, id: Option<WindowId>) -> String {
        self.with(|s| {
            let mut out = String::new();
            if let Some(wi) = id.and_then(|i| s.win_index(i)) {
                let w = &s.windows[wi];
                out.push_str(&format!(
                    "window {}: scale {:.3} ({:?}), frame {:?}, blur {:?}, size {}x{}, wm_capabilities {:?}\n",
                    w.id.0,
                    w.scale.as_f64(),
                    w.scale_mode,
                    w.frame,
                    w.blur_path,
                    w.logical.0,
                    w.logical.1,
                    w.wm_capabilities
                ));
            }
            out.push_str(&format!(
                "presentation clock: {}\n",
                match s.presentation_clock {
                    Some(c) if c == crate::clock::CLOCK_MONOTONIC_ID => "CLOCK_MONOTONIC".to_string(),
                    Some(c) => format!("clock {c} (LAT-PHOTON unavailable)"),
                    None => "none".to_string(),
                }
            ));
            out.push_str(&s.caps.render());
            out
        })
    }

    /// Display and surface pointers for the GPU surface.
    pub fn surface_handle(&self, id: WindowId) -> Option<WlSurfaceHandle> {
        self.win(id, |s, wi| {
            WlSurfaceHandle::new(s.conn.clone(), s.windows[wi].surface.clone())
        })
    }

    /// Take the render thread's pacing link for a window: frame callbacks, presentation
    /// feedback and surface size changes arrive on it, and it requests frames itself. A
    /// second call replaces the first (the old pacer goes quiet). It starts with one
    /// `Surface` event describing the current size.
    pub fn pacer(&self, id: WindowId) -> Option<crate::pace::Pacer> {
        self.win(id, |s, wi| {
            let (pacer, tx) = crate::pace::Pacer::new(crate::pace::PacerParts {
                window: id,
                surface: s.windows[wi].surface.clone(),
                conn: s.conn.clone(),
                presentation: s.globals.presentation.clone(),
                clock: s.presentation_clock,
                xdg_surface: s.windows[wi].xdg_surface.clone(),
                compositor: s.globals.compositor.clone(),
                slot: std::sync::Arc::clone(&s.windows[wi].slot),
            });
            let w = &mut s.windows[wi];
            w.slot.set_buffer(w.scale.to_buffer_size(w.logical));
            tx.send(crate::pace::PaceEvent::Surface {
                logical: w.logical,
                buffer: w.scale.to_buffer_size(w.logical),
                scale: w.scale,
                suspended: !w.states.should_render(),
            });
            w.pace = Some(tx);
            pacer
        })
    }

    /// Flush pending requests now (the source also flushes before every sleep).
    pub fn flush(&self) {
        let _ = self.state.borrow().conn.flush();
    }
}
