// SPDX-License-Identifier: AGPL-3.0-only
//! Seats (T-09 §6): keyboard, pointer, touch, text-input-v3, clipboard and primary
//! selection. State is per seat. Nothing here logs key, text or clipboard content.
#![allow(unreachable_pub)] // `pub` fields of crate-private structs

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::{Arc, Mutex};

use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager::WlDataDeviceManager;
use wayland_client::protocol::wl_data_offer::{self, WlDataOffer};
use wayland_client::protocol::wl_data_source::{self, WlDataSource};
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::protocol::wl_touch::{self, WlTouch};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum, delegate_noop, event_created_child};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::WpCursorShapeDeviceV1;
use wayland_protocols::wp::primary_selection::zv1::client::zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1;
use wayland_protocols::wp::primary_selection::zv1::client::zwp_primary_selection_device_v1::{
    self, ZwpPrimarySelectionDeviceV1,
};
use wayland_protocols::wp::primary_selection::zv1::client::zwp_primary_selection_offer_v1::{
    self, ZwpPrimarySelectionOfferV1,
};
use wayland_protocols::wp::primary_selection::zv1::client::zwp_primary_selection_source_v1::{
    self, ZwpPrimarySelectionSourceV1,
};
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3;
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::{
    self, ChangeCause, ContentHint, ContentPurpose, ZwpTextInputV3,
};

use crate::clock::Stamp;
use crate::cursor::{CursorShape, CursorStack};
use crate::event::{
    DndEvent, ImeEvent, InputKind, Keymap, KeymapFormat, ScrollFrame, SeatId, Secret,
    SelectionEvent, SelectionKind, WindowId, WlEvent,
};
use crate::gesture::{AxisAccum, Gesture, TouchTracker};
use crate::mime::{self, Payload, Want};
use crate::repeat::{RepeatInfo, Repeater, Tick};
use crate::state::{WlError, WlState};
use crate::transfer::Transfer;
use crate::window::{Rect, ResizeEdge};

/// Largest keymap accepted (the compositor sends text of tens of KiB).
const MAX_KEYMAP: usize = 4 * 1024 * 1024;

/// Per-frame scroll accumulation for one pointer (pure; unit tested below).
#[derive(Debug)]
pub struct ScrollState {
    x: AxisAccum,
    y: AxisAccum,
    px: (f64, f64),
    v120: (i32, i32),
    stop: (bool, bool),
}

impl ScrollState {
    /// New with a wheel multiplier.
    pub fn new(multiplier: f64) -> ScrollState {
        ScrollState {
            x: AxisAccum::new(multiplier),
            y: AxisAccum::new(multiplier),
            px: (0.0, 0.0),
            v120: (0, 0),
            stop: (false, false),
        }
    }

    /// Change the multiplier.
    pub fn set_multiplier(&mut self, m: f64) {
        self.x.set_multiplier(m);
        self.y.set_multiplier(m);
    }

    /// `wl_pointer.axis` (pixels).
    pub fn axis(&mut self, horizontal: bool, value: f64) {
        if horizontal {
            self.px.0 += value;
        } else {
            self.px.1 += value;
        }
    }

    /// `axis_value120` (or `axis_discrete * 120`).
    pub fn value120(&mut self, horizontal: bool, v: i32) {
        if horizontal {
            self.v120.0 = self.v120.0.saturating_add(v);
        } else {
            self.v120.1 = self.v120.1.saturating_add(v);
        }
    }

    /// `axis_stop`.
    pub fn stop(&mut self, horizontal: bool) {
        if horizontal {
            self.stop.0 = true;
        } else {
            self.stop.1 = true;
        }
    }

    /// `wl_pointer.frame`: turn what was gathered into one [`ScrollFrame`]. Where a
    /// source gave `value120`, lines come from it and its pixel value is not double counted.
    pub fn finish(&mut self) -> ScrollFrame {
        let mut f = ScrollFrame::default();
        let (px, v120, stop) = (self.px, self.v120, self.stop);
        self.px = (0.0, 0.0);
        self.v120 = (0, 0);
        self.stop = (false, false);

        if v120.0 != 0 {
            f.lines_x = self.x.push_value120(v120.0);
        } else if px.0 != 0.0 {
            f.lines_x = self.x.push_px(px.0);
            f.px_x = px.0;
        }
        if v120.1 != 0 {
            f.lines_y = self.y.push_value120(v120.1);
        } else if px.1 != 0.0 {
            f.lines_y = self.y.push_px(px.1);
            f.px_y = px.1;
        }
        if stop.0 {
            self.x.stop();
            f.stop_x = true;
        }
        if stop.1 {
            self.y.stop();
            f.stop_y = true;
        }
        f
    }
}

/// Offer metadata: MIME types announced by `offer` events.
#[derive(Debug, Default)]
pub(crate) struct OfferData {
    mimes: Mutex<Vec<String>>,
}

impl OfferData {
    fn push(&self, m: String) {
        if let Ok(mut v) = self.mimes.lock() {
            v.push(m);
        }
    }
    fn list(&self) -> Vec<String> {
        self.mimes.lock().map(|v| v.clone()).unwrap_or_default()
    }
}

/// User data of our selection sources.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SourceData {
    pub seat: SeatId,
    pub kind: SelectionKind,
}

pub(crate) enum OfferProxy {
    Data(WlDataOffer),
    Primary(ZwpPrimarySelectionOfferV1),
}

impl OfferProxy {
    fn destroy(&self) {
        match self {
            OfferProxy::Data(o) => o.destroy(),
            OfferProxy::Primary(o) => o.destroy(),
        }
    }
    fn receive(&self, mime: String, fd: BorrowedFd<'_>) {
        match self {
            OfferProxy::Data(o) => o.receive(mime, fd),
            OfferProxy::Primary(o) => o.receive(mime, fd),
        }
    }
    fn mimes(&self) -> Vec<String> {
        match self {
            OfferProxy::Data(o) => o
                .data::<OfferData>()
                .map(OfferData::list)
                .unwrap_or_default(),
            OfferProxy::Primary(o) => o
                .data::<OfferData>()
                .map(OfferData::list)
                .unwrap_or_default(),
        }
    }
}

pub(crate) enum OwnedSource {
    Data(WlDataSource, Arc<[u8]>),
    Primary(ZwpPrimarySelectionSourceV1, Arc<[u8]>),
}

impl OwnedSource {
    fn destroy(&self) {
        match self {
            OwnedSource::Data(s, _) => s.destroy(),
            OwnedSource::Primary(s, _) => s.destroy(),
        }
    }
    fn data(&self) -> Arc<[u8]> {
        match self {
            OwnedSource::Data(_, d) | OwnedSource::Primary(_, d) => d.clone(),
        }
    }
    fn is(&self, id: wayland_client::backend::ObjectId) -> bool {
        match self {
            OwnedSource::Data(s, _) => s.id() == id,
            OwnedSource::Primary(s, _) => s.id() == id,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct ImeState {
    pub focus: Option<WindowId>,
    pub enabled: bool,
    pub cursor_rect: Option<Rect>,
    pub has_preedit: bool,
    pub pending_preedit: Option<(Option<String>, i32, i32)>,
    pub pending_commit: Option<String>,
    pub pending_delete: (u32, u32),
}

impl ImeState {
    /// Drop every not-yet-`done` field (text-input focus changes).
    pub fn clear_pending(&mut self) {
        self.pending_preedit = None;
        self.pending_commit = None;
        self.pending_delete = (0, 0);
    }
}

/// Protocol limit of `set_surrounding_text` (bytes).
pub(crate) const MAX_SURROUNDING: usize = 4000;

fn floor_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Fit surrounding text into the protocol limit: at most [`MAX_SURROUNDING`] bytes, cut on
/// UTF-8 boundaries around the cursor, with cursor and anchor re-based, clamped into the
/// text and moved onto boundaries.
pub(crate) fn clamp_surrounding(text: &str, cursor: i32, anchor: i32) -> (String, i32, i32) {
    let off = |v: i32| usize::try_from(v).unwrap_or(0).min(text.len());
    let (c, a) = (off(cursor), off(anchor));
    let (start, end) = if text.len() <= MAX_SURROUNDING {
        (0, text.len())
    } else {
        let start = ceil_boundary(text, c.saturating_sub(MAX_SURROUNDING / 2));
        let end = floor_boundary(text, (start + MAX_SURROUNDING).min(text.len()));
        (start, end)
    };
    let rebase = |v: usize| floor_boundary(&text[start..end], v.clamp(start, end) - start) as i32;
    (text[start..end].to_string(), rebase(c), rebase(a))
}

/// A drag in progress over one of our windows (at most one per seat).
pub(crate) struct DndSession {
    pub offer: WlDataOffer,
    pub window: WindowId,
    pub pos: (f64, f64),
    /// The type we accepted and what it carries; `None` = refused.
    pub accepted: Option<(String, Payload)>,
    pub dropped: bool,
}

pub(crate) struct SeatState {
    pub id: SeatId,
    pub seat: WlSeat,
    pub keyboard: Option<WlKeyboard>,
    pub pointer: Option<WlPointer>,
    pub touch_dev: Option<WlTouch>,
    pub cursor_device: Option<WpCursorShapeDeviceV1>,
    pub text_input: Option<ZwpTextInputV3>,
    pub data_device: Option<WlDataDevice>,
    pub primary_device: Option<ZwpPrimarySelectionDeviceV1>,
    pub kb_focus: Option<WindowId>,
    pub held: Vec<u32>,
    pub repeater: Repeater,
    pub ptr_focus: Option<WindowId>,
    pub enter_serial: u32,
    /// Latest input serial: used for selection ownership, `move`, `resize`, menus.
    pub last_serial: u32,
    pub cursor: CursorStack,
    pub cursor_hidden: bool,
    pub scroll: ScrollState,
    pub touch: TouchTracker,
    pub ime: ImeState,
    pub clip_offer: Option<OfferProxy>,
    pub primary_offer: Option<OfferProxy>,
    pub clip_source: Option<OwnedSource>,
    pub primary_source: Option<OwnedSource>,
    pub dnd: Option<DndSession>,
    /// input-timestamps objects: keyboard, pointer, touch.
    pub ts: [Option<wayland_protocols::wp::input_timestamps::zv1::client::zwp_input_timestamps_v1::ZwpInputTimestampsV1>; 3],
}

impl SeatState {
    pub(crate) fn window_destroyed(&mut self, w: WindowId) {
        if self.kb_focus == Some(w) {
            self.kb_focus = None;
            self.repeater.cancel();
            self.held.clear();
        }
        if self.ptr_focus == Some(w) {
            self.ptr_focus = None;
        }
        if self.ime.focus == Some(w) {
            self.ime.focus = None;
        }
        if let Some(d) = self.dnd.take_if(|d| d.window == w) {
            d.offer.destroy();
        }
    }
}

pub(crate) fn bind_seat(state: &mut WlState, registry: &WlRegistry, name: u32, version: u32) {
    let id = SeatId(name);
    let qh = state.qh.clone();
    let seat = registry.bind::<WlSeat, _, _>(name, version.min(9), &qh, id);
    let data_device = state
        .globals
        .data_manager
        .as_ref()
        .map(|m| m.get_data_device(&seat, &qh, id));
    let primary_device = state
        .globals
        .primary
        .as_ref()
        .map(|m| m.get_device(&seat, &qh, id));
    let text_input = state
        .globals
        .text_input
        .as_ref()
        .map(|m| m.get_text_input(&seat, &qh, id));
    let mut repeater = Repeater::new();
    repeater.set_overrides(state.cfg.repeat_rate, state.cfg.repeat_delay_ms);
    state.seats.push(SeatState {
        id,
        seat,
        keyboard: None,
        pointer: None,
        touch_dev: None,
        cursor_device: None,
        text_input,
        data_device,
        primary_device,
        kb_focus: None,
        held: Vec::new(),
        repeater,
        ptr_focus: None,
        enter_serial: 0,
        last_serial: 0,
        cursor: CursorStack::new(CursorShape::Default),
        cursor_hidden: false,
        scroll: ScrollState::new(state.cfg.axis_multiplier),
        touch: TouchTracker::new(),
        ime: ImeState::default(),
        clip_offer: None,
        primary_offer: None,
        clip_source: None,
        primary_source: None,
        dnd: None,
        ts: [None, None, None],
    });
}

pub(crate) fn remove_seat(state: &mut WlState, name: u32) {
    if let Some(i) = state.seats.iter().position(|s| s.id.0 == name) {
        let s = state.seats.remove(i);
        if let Some(src) = &s.clip_source {
            src.destroy();
        }
        if let Some(src) = &s.primary_source {
            src.destroy();
        }
        if let Some(d) = &s.dnd {
            d.offer.destroy();
        }
        state.rearm_timer();
    }
}

fn read_keymap(fd: OwnedFd, size: u32) -> Vec<u8> {
    let size = (size as usize).min(MAX_KEYMAP);
    let mut buf = vec![0u8; size];
    let mut off = 0;
    while off < size {
        match rustix::io::pread(&fd, &mut buf[off..], off as u64) {
            Ok(0) => break,
            Ok(n) => off += n,
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => break,
        }
    }
    buf.truncate(off);
    while buf.last() == Some(&0) {
        buf.pop();
    }
    buf
}

fn window_of(surface: &wayland_client::protocol::wl_surface::WlSurface) -> Option<WindowId> {
    surface.data::<WindowId>().copied()
}

/// Errors from clipboard and primary-selection operations.
#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    /// No such seat.
    #[error("no such seat")]
    NoSeat,
    /// The compositor does not offer the needed manager (primary selection disabled, ...).
    #[error("selection protocol unavailable")]
    Unavailable,
    /// Nobody owns the selection.
    #[error("no selection")]
    NoSelection,
    /// The offer has no MIME type we can read.
    #[error("no usable MIME type offered")]
    NoUsableMime,
    /// Pipe setup failed.
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
}

/// Why a cursor could not be set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CursorError {
    /// No such seat.
    #[error("no such seat")]
    NoSeat,
    /// The pointer has no focus on our surface.
    #[error("pointer not over a window")]
    NoFocus,
    /// cursor-shape-v1 is absent: the caller must draw a themed cursor via shm.
    #[error("cursor-shape-v1 unavailable; themed cursor required")]
    NeedsThemedCursor,
}

impl WlState {
    pub(crate) fn seat_index(&self, id: SeatId) -> Option<usize> {
        self.seats.iter().position(|s| s.id == id)
    }

    // ----- repeat timer ------------------------------------------------------------------

    pub(crate) fn rearm_timer(&self) {
        let next: Option<Stamp> = self
            .seats
            .iter()
            .filter_map(|s| s.repeater.next_deadline())
            .min();
        let r = match next {
            Some(at) => self.timer.arm_at(at),
            None => self.timer.disarm(),
        };
        if let Err(e) = r {
            tracing::debug!(error = %e, "repeat timer update failed");
        }
    }

    /// Emit repeats due at `now` (called when the timer fd fires).
    pub(crate) fn tick_repeat(&mut self, now: Stamp) {
        self.stamp = now;
        let ticks: Vec<(SeatId, Tick)> = self
            .seats
            .iter_mut()
            .filter_map(|s| s.repeater.poll(now).map(|t| (s.id, t)))
            .collect();
        for (seat, t) in ticks {
            for _ in 0..t.count {
                self.emit_input(
                    seat,
                    InputKind::Key {
                        key: t.key,
                        pressed: true,
                        repeat: true,
                        synthetic: false,
                        time_ms: 0,
                    },
                );
            }
        }
        self.rearm_timer();
    }

    // ----- cursor ------------------------------------------------------------------------

    /// Apply the current shape of a seat's stack to its pointer.
    pub(crate) fn apply_cursor(&mut self, seat: SeatId) -> Result<(), CursorError> {
        let si = self.seat_index(seat).ok_or(CursorError::NoSeat)?;
        let s = &self.seats[si];
        if s.ptr_focus.is_none() {
            return Err(CursorError::NoFocus);
        }
        if s.cursor_hidden {
            if let Some(p) = &s.pointer {
                p.set_cursor(s.enter_serial, None, 0, 0);
            }
            return Ok(());
        }
        match &s.cursor_device {
            Some(dev) => {
                dev.set_shape(s.enter_serial, s.cursor.current().to_wire());
                Ok(())
            }
            None => Err(CursorError::NeedsThemedCursor),
        }
    }

    pub(crate) fn cursor_stack_mut(&mut self, seat: SeatId) -> Option<&mut CursorStack> {
        let si = self.seat_index(seat)?;
        Some(&mut self.seats[si].cursor)
    }

    pub(crate) fn set_cursor_hidden(&mut self, seat: SeatId, hidden: bool) {
        if let Some(si) = self.seat_index(seat) {
            self.seats[si].cursor_hidden = hidden;
        }
    }

    // ----- selections --------------------------------------------------------------------

    /// Become the selection owner with UTF-8 text.
    pub(crate) fn set_selection(
        &mut self,
        seat: SeatId,
        kind: SelectionKind,
        data: Vec<u8>,
    ) -> Result<(), ClipboardError> {
        let si = self.seat_index(seat).ok_or(ClipboardError::NoSeat)?;
        let qh = self.qh.clone();
        let data: Arc<[u8]> = Arc::from(data);
        let udata = SourceData { seat, kind };
        let s = &mut self.seats[si];
        let serial = s.last_serial;
        match kind {
            SelectionKind::Clipboard => {
                let (m, dev) = self
                    .globals
                    .data_manager
                    .as_ref()
                    .zip(s.data_device.as_ref())
                    .ok_or(ClipboardError::Unavailable)?;
                let src = m.create_data_source(&qh, udata);
                for t in mime::OFFER {
                    src.offer(t.to_string());
                }
                dev.set_selection(Some(&src), serial);
                if let Some(old) = s.clip_source.replace(OwnedSource::Data(src, data)) {
                    old.destroy();
                }
            }
            SelectionKind::Primary => {
                let (m, dev) = self
                    .globals
                    .primary
                    .as_ref()
                    .zip(s.primary_device.as_ref())
                    .ok_or(ClipboardError::Unavailable)?;
                let src = m.create_source(&qh, udata);
                for t in mime::OFFER {
                    src.offer(t.to_string());
                }
                dev.set_selection(Some(&src), serial);
                if let Some(old) = s.primary_source.replace(OwnedSource::Primary(src, data)) {
                    old.destroy();
                }
            }
        }
        Ok(())
    }

    /// Give up ownership.
    pub(crate) fn clear_selection(
        &mut self,
        seat: SeatId,
        kind: SelectionKind,
    ) -> Result<(), ClipboardError> {
        let si = self.seat_index(seat).ok_or(ClipboardError::NoSeat)?;
        let s = &mut self.seats[si];
        let serial = s.last_serial;
        match kind {
            SelectionKind::Clipboard => {
                if let Some(d) = &s.data_device {
                    d.set_selection(None, serial);
                }
                if let Some(old) = s.clip_source.take() {
                    old.destroy();
                }
            }
            SelectionKind::Primary => {
                if let Some(d) = &s.primary_device {
                    d.set_selection(None, serial);
                }
                if let Some(old) = s.primary_source.take() {
                    old.destroy();
                }
            }
        }
        Ok(())
    }

    /// Whether we currently own a selection.
    pub(crate) fn owns_selection(&self, seat: SeatId, kind: SelectionKind) -> bool {
        self.seat_index(seat).is_some_and(|si| match kind {
            SelectionKind::Clipboard => self.seats[si].clip_source.is_some(),
            SelectionKind::Primary => self.seats[si].primary_source.is_some(),
        })
    }

    /// MIME types of the current foreign selection.
    pub(crate) fn selection_mimes(&self, seat: SeatId, kind: SelectionKind) -> Vec<String> {
        let Some(si) = self.seat_index(seat) else {
            return Vec::new();
        };
        let s = &self.seats[si];
        let o = match kind {
            SelectionKind::Clipboard => s.clip_offer.as_ref(),
            SelectionKind::Primary => s.primary_offer.as_ref(),
        };
        o.map(OfferProxy::mimes).unwrap_or_default()
    }

    /// Start reading the current foreign selection. Insert the returned transfer in the loop.
    pub(crate) fn receive_selection(
        &mut self,
        seat: SeatId,
        kind: SelectionKind,
        want: Want,
    ) -> Result<Transfer, ClipboardError> {
        let si = self.seat_index(seat).ok_or(ClipboardError::NoSeat)?;
        let s = &self.seats[si];
        let offer = match kind {
            SelectionKind::Clipboard => s.clip_offer.as_ref(),
            SelectionKind::Primary => s.primary_offer.as_ref(),
        }
        .ok_or(ClipboardError::NoSelection)?;
        let mimes = offer.mimes();
        let (chosen, payload) =
            mime::choose_receive(&mimes, want).ok_or(ClipboardError::NoUsableMime)?;
        let chosen = chosen.to_string();
        let (read_fd, write_fd) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)
            .map_err(std::io::Error::from)?;
        offer.receive(chosen, write_fd.as_fd());
        let _ = self.conn.flush();
        drop(write_fd);
        let id = self.next_transfer;
        self.next_transfer += 1;
        Ok(Transfer::receive(
            id,
            seat,
            kind,
            payload,
            read_fd,
            self.cfg.transfer_limits,
        )?)
    }

    fn handle_send(&mut self, data: SourceData, mime: String, fd: OwnedFd) {
        if !mime::is_offered(&mime) {
            return; // fd closes: the requester sees EOF
        }
        let Some(si) = self.seat_index(data.seat) else {
            return;
        };
        let s = &self.seats[si];
        let src = match data.kind {
            SelectionKind::Clipboard => s.clip_source.as_ref(),
            SelectionKind::Primary => s.primary_source.as_ref(),
        };
        let Some(bytes) = src.map(OwnedSource::data) else {
            return;
        };
        let id = self.next_transfer;
        self.next_transfer += 1;
        match Transfer::send(
            id,
            data.seat,
            data.kind,
            bytes,
            fd,
            self.cfg.transfer_limits,
        ) {
            Ok(t) => self.emit(WlEvent::Send(t)),
            Err(e) => tracing::warn!(error = %e, "selection send setup failed"),
        }
    }

    fn handle_cancelled(&mut self, data: SourceData, id: wayland_client::backend::ObjectId) {
        let Some(si) = self.seat_index(data.seat) else {
            return;
        };
        let slot = match data.kind {
            SelectionKind::Clipboard => &mut self.seats[si].clip_source,
            SelectionKind::Primary => &mut self.seats[si].primary_source,
        };
        if slot.as_ref().is_some_and(|s| s.is(id)) {
            if let Some(s) = slot.take() {
                s.destroy();
            }
            self.emit(WlEvent::Selection(SelectionEvent::Lost {
                seat: data.seat,
                kind: data.kind,
            }));
        }
    }

    fn set_foreign_offer(&mut self, seat: SeatId, kind: SelectionKind, offer: Option<OfferProxy>) {
        let Some(si) = self.seat_index(seat) else {
            if let Some(o) = offer {
                o.destroy();
            }
            return;
        };
        let slot = match kind {
            SelectionKind::Clipboard => &mut self.seats[si].clip_offer,
            SelectionKind::Primary => &mut self.seats[si].primary_offer,
        };
        if let Some(old) = slot.take() {
            old.destroy();
        }
        let ev = match &offer {
            Some(o) => SelectionEvent::Offered {
                seat,
                kind,
                mimes: o.mimes(),
            },
            None => SelectionEvent::Cleared { seat, kind },
        };
        *slot = offer;
        self.emit(WlEvent::Selection(ev));
    }

    // ----- drag and drop in (wl_data_device enter/motion/leave/drop) -------------------------

    fn dnd_enter(
        &mut self,
        seat: SeatId,
        serial: u32,
        surface: &wayland_client::protocol::wl_surface::WlSurface,
        x: f64,
        y: f64,
        offer: Option<WlDataOffer>,
    ) {
        let Some(offer) = offer else { return };
        let (Some(si), Some(window)) = (self.seat_index(seat), window_of(surface)) else {
            offer.destroy();
            return;
        };
        if let Some(old) = self.seats[si].dnd.take() {
            old.offer.destroy();
        }
        let mimes = offer
            .data::<OfferData>()
            .map(OfferData::list)
            .unwrap_or_default();
        let accepted =
            mime::choose_receive(&mimes, Want::FilesOrText).map(|(m, p)| (m.to_string(), p));
        // Accept the type (or refuse with None) so the compositor shows the right cursor.
        offer.accept(serial, accepted.as_ref().map(|a| a.0.clone()));
        if offer.version() >= 3 {
            use wayland_client::protocol::wl_data_device_manager::DndAction;
            if accepted.is_some() {
                offer.set_actions(DndAction::Copy, DndAction::Copy);
            } else {
                offer.set_actions(DndAction::None, DndAction::None);
            }
        }
        let payload = accepted.as_ref().map(|a| a.1);
        self.seats[si].dnd = Some(DndSession {
            offer,
            window,
            pos: (x, y),
            accepted,
            dropped: false,
        });
        self.emit(WlEvent::Dnd(DndEvent::Enter {
            seat,
            window,
            x,
            y,
            payload,
            mimes,
        }));
    }

    fn dnd_motion(&mut self, seat: SeatId, x: f64, y: f64) {
        let Some(si) = self.seat_index(seat) else {
            return;
        };
        let Some(d) = self.seats[si].dnd.as_mut().filter(|d| !d.dropped) else {
            return;
        };
        d.pos = (x, y);
        let window = d.window;
        self.emit(WlEvent::Dnd(DndEvent::Motion { seat, window, x, y }));
    }

    fn dnd_leave(&mut self, seat: SeatId) {
        let Some(si) = self.seat_index(seat) else {
            return;
        };
        // After `drop` the offer lives on for the transfer; `leave` then is not expected.
        if self.seats[si].dnd.as_ref().is_none_or(|d| d.dropped) {
            return;
        }
        if let Some(d) = self.seats[si].dnd.take() {
            d.offer.destroy();
            self.emit(WlEvent::Dnd(DndEvent::Leave {
                seat,
                window: d.window,
            }));
        }
    }

    fn dnd_drop(&mut self, seat: SeatId) {
        let Some(si) = self.seat_index(seat) else {
            return;
        };
        let Some(d) = self.seats[si].dnd.as_mut() else {
            return;
        };
        match d.accepted.as_ref().map(|a| a.1) {
            Some(payload) => {
                d.dropped = true;
                let (window, (x, y)) = (d.window, d.pos);
                self.emit(WlEvent::Dnd(DndEvent::Dropped {
                    seat,
                    window,
                    x,
                    y,
                    payload,
                }));
            }
            None => {
                // Nothing readable was offered: end the session.
                if let Some(d) = self.seats[si].dnd.take() {
                    d.offer.destroy();
                    self.emit(WlEvent::Dnd(DndEvent::Leave {
                        seat,
                        window: d.window,
                    }));
                }
            }
        }
    }

    /// Start reading a dropped payload. Insert the transfer in the loop; the done event
    /// carries `kind == Clipboard` for drops, so match by [`Transfer::id`]. Call
    /// [`WlState::finish_drop`] when it ends.
    pub(crate) fn receive_drop(&mut self, seat: SeatId) -> Result<Transfer, ClipboardError> {
        let si = self.seat_index(seat).ok_or(ClipboardError::NoSeat)?;
        let d = self.seats[si]
            .dnd
            .as_ref()
            .filter(|d| d.dropped)
            .ok_or(ClipboardError::NoSelection)?;
        let (mime, payload) = d.accepted.clone().ok_or(ClipboardError::NoUsableMime)?;
        let (read_fd, write_fd) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)
            .map_err(std::io::Error::from)?;
        d.offer.receive(mime, write_fd.as_fd());
        let _ = self.conn.flush();
        drop(write_fd);
        let id = self.next_transfer;
        self.next_transfer += 1;
        Ok(Transfer::receive(
            id,
            seat,
            SelectionKind::Clipboard,
            payload,
            read_fd,
            self.cfg.transfer_limits,
        )?)
    }

    /// End the drop session: `finish` (v3, when `ok`) and destroy the offer.
    pub(crate) fn finish_drop(&mut self, seat: SeatId, ok: bool) {
        let Some(si) = self.seat_index(seat) else {
            return;
        };
        if let Some(d) = self.seats[si].dnd.take_if(|d| d.dropped) {
            if ok && d.offer.version() >= 3 {
                d.offer.finish();
            }
            d.offer.destroy();
        }
    }

    // ----- IME (text-input-v3) -------------------------------------------------------------

    /// Enable the text input for a seat and send the cursor rectangle if known.
    pub(crate) fn ime_enable(&mut self, seat: SeatId) {
        let Some(si) = self.seat_index(seat) else {
            return;
        };
        let s = &mut self.seats[si];
        let Some(ti) = &s.text_input else { return };
        ti.enable();
        ti.set_content_type(ContentHint::empty(), ContentPurpose::Terminal);
        if let Some(r) = s.ime.cursor_rect {
            ti.set_cursor_rectangle(r.x, r.y, r.w, r.h);
        }
        ti.commit();
        s.ime.enabled = true;
    }

    /// Disable the text input.
    pub(crate) fn ime_disable(&mut self, seat: SeatId) {
        let Some(si) = self.seat_index(seat) else {
            return;
        };
        let s = &mut self.seats[si];
        if let Some(ti) = &s.text_input {
            ti.disable();
            ti.commit();
        }
        s.ime.enabled = false;
        s.ime.has_preedit = false;
    }

    /// Report the cursor rectangle (surface-local logical px); call whenever the cursor moves.
    pub(crate) fn ime_set_cursor_rect(&mut self, seat: SeatId, r: Rect) {
        let Some(si) = self.seat_index(seat) else {
            return;
        };
        let s = &mut self.seats[si];
        if s.ime.cursor_rect == Some(r) {
            return;
        }
        s.ime.cursor_rect = Some(r);
        if let (true, Some(ti)) = (s.ime.enabled, &s.text_input) {
            ti.set_cursor_rectangle(r.x, r.y, r.w, r.h);
            ti.commit();
        }
    }

    /// Report surrounding text (at most 4000 bytes, protocol limit) and cursor/anchor offsets.
    pub(crate) fn ime_set_surrounding(
        &mut self,
        seat: SeatId,
        text: String,
        cursor: i32,
        anchor: i32,
    ) {
        let Some(si) = self.seat_index(seat) else {
            return;
        };
        let s = &mut self.seats[si];
        if let (true, Some(ti)) = (s.ime.enabled, &s.text_input) {
            let (text, cursor, anchor) = clamp_surrounding(&text, cursor, anchor);
            ti.set_surrounding_text(text, cursor, anchor);
            ti.set_text_change_cause(ChangeCause::Other);
            ti.commit();
        }
    }

    // ----- CSD actions ----------------------------------------------------------------------

    fn csd_serial(&self, window: WindowId, seat: SeatId) -> Result<(usize, usize, u32), WlError> {
        let wi = self.win_index(window).ok_or(WlError::UnknownWindow)?;
        let si = self.seat_index(seat).ok_or(WlError::UnknownSeat)?;
        Ok((wi, si, self.seats[si].last_serial))
    }

    /// Title drag: `xdg_toplevel.move`.
    pub(crate) fn begin_move(&mut self, window: WindowId, seat: SeatId) -> Result<(), WlError> {
        let (wi, si, serial) = self.csd_serial(window, seat)?;
        self.windows[wi]
            .toplevel
            ._move(&self.seats[si].seat, serial);
        Ok(())
    }

    /// Edge drag: `xdg_toplevel.resize`.
    pub(crate) fn begin_resize(
        &mut self,
        window: WindowId,
        seat: SeatId,
        edge: ResizeEdge,
    ) -> Result<(), WlError> {
        let (wi, si, serial) = self.csd_serial(window, seat)?;
        self.windows[wi]
            .toplevel
            .resize(&self.seats[si].seat, serial, edge.to_wire());
        Ok(())
    }

    /// Right-click on the title: `xdg_toplevel.show_window_menu` at surface-local (x, y).
    pub(crate) fn show_window_menu(
        &mut self,
        window: WindowId,
        seat: SeatId,
        x: i32,
        y: i32,
    ) -> Result<(), WlError> {
        let (wi, si, serial) = self.csd_serial(window, seat)?;
        self.windows[wi]
            .toplevel
            .show_window_menu(&self.seats[si].seat, serial, x, y);
        Ok(())
    }
}

delegate_noop!(WlState: ignore WlDataDeviceManager);
delegate_noop!(WlState: ignore ZwpPrimarySelectionDeviceManagerV1);
delegate_noop!(WlState: ignore ZwpTextInputManagerV3);

impl Dispatch<WlSeat, SeatId> for WlState {
    fn event(
        state: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        id: &SeatId,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_seat::Event::Capabilities { capabilities } = event else {
            return;
        };
        let WEnum::Value(caps) = capabilities else {
            return;
        };
        let Some(si) = state.seat_index(*id) else {
            return;
        };
        let cursor_manager = state.globals.cursor_shape.clone();
        let s = &mut state.seats[si];

        if caps.contains(wl_seat::Capability::Keyboard) {
            if s.keyboard.is_none() {
                s.keyboard = Some(seat.get_keyboard(qh, *id));
            }
        } else if let Some(k) = s.keyboard.take() {
            if k.version() >= 3 {
                k.release();
            }
            s.repeater.cancel();
            s.held.clear();
            s.kb_focus = None;
            if let Some(t) = s.ts[0].take() {
                t.destroy();
            }
        }

        if caps.contains(wl_seat::Capability::Pointer) {
            if s.pointer.is_none() {
                let p = seat.get_pointer(qh, *id);
                s.cursor_device = cursor_manager.as_ref().map(|m| m.get_pointer(&p, qh, ()));
                s.pointer = Some(p);
            }
        } else if let Some(p) = s.pointer.take() {
            if let Some(d) = s.cursor_device.take() {
                d.destroy();
            }
            if p.version() >= 3 {
                p.release();
            }
            s.ptr_focus = None;
            if let Some(t) = s.ts[1].take() {
                t.destroy();
            }
        }

        if caps.contains(wl_seat::Capability::Touch) {
            if s.touch_dev.is_none() {
                s.touch_dev = Some(seat.get_touch(qh, *id));
            }
        } else if let Some(t) = s.touch_dev.take() {
            if t.version() >= 3 {
                t.release();
            }
            s.touch.cancel();
            if let Some(t) = s.ts[2].take() {
                t.destroy();
            }
        }
        state.attach_timestamps(si);
        state.rearm_timer();
    }
}

impl Dispatch<WlKeyboard, SeatId> for WlState {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        id: &SeatId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let seat = *id;
        let Some(si) = state.seat_index(seat) else {
            return;
        };
        match event {
            wl_keyboard::Event::Keymap { format, fd, size } => {
                let (format, bytes) = match format {
                    WEnum::Value(wl_keyboard::KeymapFormat::XkbV1) => {
                        (KeymapFormat::XkbV1, read_keymap(fd, size))
                    }
                    WEnum::Value(wl_keyboard::KeymapFormat::NoKeymap) => {
                        (KeymapFormat::NoKeymap, Vec::new())
                    }
                    _ => return,
                };
                tracing::debug!(bytes = bytes.len(), "keymap received");
                state.emit_input(seat, InputKind::Keymap(Keymap { format, bytes }));
            }
            wl_keyboard::Event::Enter {
                serial,
                surface,
                keys,
            } => {
                let Some(window) = window_of(&surface) else {
                    return;
                };
                let held: Vec<u32> = keys
                    .chunks_exact(4)
                    .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                    .collect();
                let s = &mut state.seats[si];
                s.kb_focus = Some(window);
                s.held = held.clone();
                s.last_serial = serial;
                s.repeater.cancel();
                state.rearm_timer();
                state.emit_input(seat, InputKind::KeyboardEnter { window, held });
            }
            wl_keyboard::Event::Leave { serial, surface } => {
                let Some(window) = window_of(&surface) else {
                    return;
                };
                let s = &mut state.seats[si];
                s.last_serial = serial;
                s.kb_focus = None;
                s.repeater.cancel();
                let held = std::mem::take(&mut s.held);
                state.rearm_timer();
                // Release everything that was down (kitty flag 2 release events), reset
                // modifiers, then report the focus loss.
                for key in held {
                    state.emit_input(
                        seat,
                        InputKind::Key {
                            key,
                            pressed: false,
                            repeat: false,
                            synthetic: true,
                            time_ms: 0,
                        },
                    );
                }
                state.emit_input(seat, InputKind::ModifiersReset);
                state.emit_input(seat, InputKind::KeyboardLeave { window });
            }
            wl_keyboard::Event::Key {
                serial,
                time,
                key,
                state: ks,
            } => {
                let pressed = matches!(ks, WEnum::Value(wl_keyboard::KeyState::Pressed));
                let released = matches!(ks, WEnum::Value(wl_keyboard::KeyState::Released));
                if !pressed && !released {
                    return;
                }
                let repeats = (state.repeat_filter)(key);
                let stamp = state.stamp;
                let s = &mut state.seats[si];
                s.last_serial = serial;
                if pressed {
                    if !s.held.contains(&key) {
                        s.held.push(key);
                    }
                    if repeats {
                        s.repeater.press(key, stamp);
                    }
                } else {
                    s.held.retain(|k| *k != key);
                    s.repeater.release(key);
                }
                state.rearm_timer();
                state.emit_input(
                    seat,
                    InputKind::Key {
                        key,
                        pressed,
                        repeat: false,
                        synthetic: false,
                        time_ms: time,
                    },
                );
            }
            wl_keyboard::Event::Modifiers {
                serial,
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
            } => {
                state.seats[si].last_serial = serial;
                state.emit_input(
                    seat,
                    InputKind::Modifiers {
                        depressed: mods_depressed,
                        latched: mods_latched,
                        locked: mods_locked,
                        group,
                    },
                );
            }
            wl_keyboard::Event::RepeatInfo { rate, delay } => {
                state.seats[si].repeater.set_compositor(RepeatInfo {
                    rate_hz: rate.max(0) as u32,
                    delay_ms: delay.max(0) as u32,
                });
                state.emit_input(
                    seat,
                    InputKind::RepeatInfo {
                        rate_hz: rate,
                        delay_ms: delay,
                    },
                );
            }
            _ => {}
        }
    }
}

impl Dispatch<WlPointer, SeatId> for WlState {
    fn event(
        state: &mut Self,
        pointer: &WlPointer,
        event: wl_pointer::Event,
        id: &SeatId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let seat = *id;
        let Some(si) = state.seat_index(seat) else {
            return;
        };
        let horizontal = |a: WEnum<wl_pointer::Axis>| {
            matches!(a, WEnum::Value(wl_pointer::Axis::HorizontalScroll))
        };
        // Pointers older than v5 have no `frame`: each axis event is its own frame.
        let legacy = pointer.version() < 5;
        let mut flush = false;
        match event {
            wl_pointer::Event::Enter {
                serial,
                surface,
                surface_x,
                surface_y,
            } => {
                let Some(window) = window_of(&surface) else {
                    return;
                };
                let s = &mut state.seats[si];
                s.ptr_focus = Some(window);
                s.enter_serial = serial;
                s.last_serial = serial;
                state.emit_input(
                    seat,
                    InputKind::PointerEnter {
                        window,
                        x: surface_x,
                        y: surface_y,
                    },
                );
                let _ = state.apply_cursor(seat);
            }
            wl_pointer::Event::Leave { serial, surface } => {
                let Some(window) = window_of(&surface) else {
                    return;
                };
                let s = &mut state.seats[si];
                s.ptr_focus = None;
                s.last_serial = serial;
                state.emit_input(seat, InputKind::PointerLeave { window });
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                state.emit_input(
                    seat,
                    InputKind::PointerMotion {
                        x: surface_x,
                        y: surface_y,
                    },
                );
            }
            wl_pointer::Event::Button {
                serial,
                time,
                button,
                state: bs,
            } => {
                state.seats[si].last_serial = serial;
                let pressed = matches!(bs, WEnum::Value(wl_pointer::ButtonState::Pressed));
                state.emit_input(
                    seat,
                    InputKind::PointerButton {
                        button,
                        pressed,
                        time_ms: time,
                    },
                );
            }
            wl_pointer::Event::Axis { axis, value, .. } => {
                state.seats[si].scroll.axis(horizontal(axis), value);
                flush = legacy;
            }
            wl_pointer::Event::AxisValue120 { axis, value120 } => {
                state.seats[si].scroll.value120(horizontal(axis), value120);
            }
            wl_pointer::Event::AxisDiscrete { axis, discrete } if pointer.version() < 8 => {
                state.seats[si]
                    .scroll
                    .value120(horizontal(axis), discrete.saturating_mul(120));
            }
            wl_pointer::Event::AxisStop { axis, .. } => {
                state.seats[si].scroll.stop(horizontal(axis));
                flush = legacy;
            }
            wl_pointer::Event::Frame => flush = true,
            _ => {}
        }
        if flush {
            let f = state.seats[si].scroll.finish();
            if !f.is_empty() {
                state.emit_input(seat, InputKind::Scroll(f));
            }
        }
    }
}

impl Dispatch<WlTouch, SeatId> for WlState {
    fn event(
        state: &mut Self,
        _: &WlTouch,
        event: wl_touch::Event,
        id: &SeatId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let seat = *id;
        let Some(si) = state.seat_index(seat) else {
            return;
        };
        match event {
            wl_touch::Event::Down {
                serial,
                time,
                surface,
                id: tid,
                x,
                y,
            } => {
                let Some(window) = window_of(&surface) else {
                    return;
                };
                let s = &mut state.seats[si];
                s.last_serial = serial;
                s.touch.down(tid, x, y, time);
                state.emit_input(
                    seat,
                    InputKind::TouchDown {
                        window,
                        id: tid,
                        x,
                        y,
                    },
                );
            }
            wl_touch::Event::Motion { id: tid, x, y, .. } => {
                let g = state.seats[si].touch.motion(tid, x, y);
                state.emit_input(seat, InputKind::TouchMotion { id: tid, x, y });
                if let Some(g) = g {
                    state.emit_input(seat, InputKind::Gesture(g));
                }
            }
            wl_touch::Event::Up {
                serial,
                time,
                id: tid,
            } => {
                state.seats[si].last_serial = serial;
                let g: Option<Gesture> = state.seats[si].touch.up(tid, time);
                state.emit_input(seat, InputKind::TouchUp { id: tid });
                if let Some(g) = g {
                    state.emit_input(seat, InputKind::Gesture(g));
                }
            }
            wl_touch::Event::Cancel => {
                state.seats[si].touch.cancel();
                state.emit_input(seat, InputKind::TouchCancel);
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpTextInputV3, SeatId> for WlState {
    fn event(
        state: &mut Self,
        _: &ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        id: &SeatId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let seat = *id;
        let Some(si) = state.seat_index(seat) else {
            return;
        };
        let ime = &mut state.seats[si].ime;
        match event {
            zwp_text_input_v3::Event::Enter { surface } => {
                let Some(window) = window_of(&surface) else {
                    return;
                };
                ime.focus = Some(window);
                ime.clear_pending();
                state.emit_input(seat, InputKind::Ime(ImeEvent::Enter { window }));
            }
            zwp_text_input_v3::Event::Leave { surface } => {
                let Some(window) = window_of(&surface) else {
                    return;
                };
                ime.focus = None;
                ime.has_preedit = false;
                ime.clear_pending();
                state.emit_input(seat, InputKind::Ime(ImeEvent::Leave { window }));
            }
            zwp_text_input_v3::Event::PreeditString {
                text,
                cursor_begin,
                cursor_end,
            } => {
                ime.pending_preedit = Some((text, cursor_begin, cursor_end));
            }
            zwp_text_input_v3::Event::CommitString { text } => ime.pending_commit = text,
            zwp_text_input_v3::Event::DeleteSurroundingText {
                before_length,
                after_length,
            } => {
                ime.pending_delete = (before_length, after_length);
            }
            zwp_text_input_v3::Event::Done { .. } => {
                // Protocol order: delete, commit, then the new preedit (which replaces the old).
                let delete = std::mem::take(&mut ime.pending_delete);
                let commit = ime.pending_commit.take();
                let preedit = ime.pending_preedit.take();
                let had_preedit = ime.has_preedit;
                ime.has_preedit = preedit.as_ref().is_some_and(|p| p.0.is_some());
                if delete != (0, 0) {
                    state.emit_input(
                        seat,
                        InputKind::Ime(ImeEvent::DeleteSurrounding {
                            before: delete.0,
                            after: delete.1,
                        }),
                    );
                }
                if let Some(c) = commit.filter(|c| !c.is_empty()) {
                    state.emit_input(seat, InputKind::Ime(ImeEvent::Commit(Secret(c))));
                }
                if preedit.is_some() || had_preedit {
                    let (text, cursor_begin, cursor_end) = preedit.unwrap_or((None, 0, 0));
                    state.emit_input(
                        seat,
                        InputKind::Ime(ImeEvent::Preedit {
                            text: text.filter(|t| !t.is_empty()).map(Secret),
                            cursor_begin,
                            cursor_end,
                        }),
                    );
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlDataDevice, SeatId> for WlState {
    event_created_child!(WlState, WlDataDevice, [
        0 => (WlDataOffer, OfferData::default())
    ]);

    fn event(
        state: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        id: &SeatId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_device::Event::Selection { id: offer } => {
                state.set_foreign_offer(*id, SelectionKind::Clipboard, offer.map(OfferProxy::Data));
            }
            wl_data_device::Event::Enter {
                serial,
                surface,
                x,
                y,
                id: offer,
            } => state.dnd_enter(*id, serial, &surface, x, y, offer),
            wl_data_device::Event::Motion { x, y, .. } => state.dnd_motion(*id, x, y),
            wl_data_device::Event::Leave => state.dnd_leave(*id),
            wl_data_device::Event::Drop => state.dnd_drop(*id),
            _ => {}
        }
    }
}

impl Dispatch<WlDataOffer, OfferData> for WlState {
    fn event(
        _: &mut Self,
        _: &WlDataOffer,
        event: wl_data_offer::Event,
        data: &OfferData,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = event {
            data.push(mime_type);
        }
    }
}

impl Dispatch<WlDataSource, SourceData> for WlState {
    fn event(
        state: &mut Self,
        src: &WlDataSource,
        event: wl_data_source::Event,
        data: &SourceData,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_source::Event::Send { mime_type, fd } => {
                state.handle_send(*data, mime_type, fd)
            }
            wl_data_source::Event::Cancelled => state.handle_cancelled(*data, src.id()),
            _ => {}
        }
    }
}

impl Dispatch<ZwpPrimarySelectionDeviceV1, SeatId> for WlState {
    event_created_child!(WlState, ZwpPrimarySelectionDeviceV1, [
        0 => (ZwpPrimarySelectionOfferV1, OfferData::default())
    ]);

    fn event(
        state: &mut Self,
        _: &ZwpPrimarySelectionDeviceV1,
        event: zwp_primary_selection_device_v1::Event,
        id: &SeatId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_primary_selection_device_v1::Event::Selection { id: offer } = event {
            state.set_foreign_offer(*id, SelectionKind::Primary, offer.map(OfferProxy::Primary));
        }
    }
}

impl Dispatch<ZwpPrimarySelectionOfferV1, OfferData> for WlState {
    fn event(
        _: &mut Self,
        _: &ZwpPrimarySelectionOfferV1,
        event: zwp_primary_selection_offer_v1::Event,
        data: &OfferData,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_primary_selection_offer_v1::Event::Offer { mime_type } = event {
            data.push(mime_type);
        }
    }
}

impl Dispatch<ZwpPrimarySelectionSourceV1, SourceData> for WlState {
    fn event(
        state: &mut Self,
        src: &ZwpPrimarySelectionSourceV1,
        event: zwp_primary_selection_source_v1::Event,
        data: &SourceData,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_primary_selection_source_v1::Event::Send { mime_type, fd } => {
                state.handle_send(*data, mime_type, fd)
            }
            zwp_primary_selection_source_v1::Event::Cancelled => {
                state.handle_cancelled(*data, src.id())
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surrounding_is_clamped_on_boundaries() {
        // 3-byte chars: 2000 of them is 6000 bytes.
        let t = "€".repeat(2000);
        let (out, c, a) = clamp_surrounding(&t, 3000, i32::MAX);
        assert!(out.len() <= MAX_SURROUNDING);
        assert!(c >= 0 && (c as usize) <= out.len() && out.is_char_boundary(c as usize));
        assert!(a >= 0 && (a as usize) <= out.len() && out.is_char_boundary(a as usize));
        let (out, c, _) = clamp_surrounding(&t, 4000, -5);
        assert!(out.is_char_boundary(c as usize));
        let (out, c, a) = clamp_surrounding("héllo", 99, -1);
        assert_eq!((out.as_str(), c, a), ("héllo", 6, 0));
        assert_eq!(clamp_surrounding("é", 1, 1).1, 0);
    }

    #[test]
    fn scroll_value120_saturates() {
        let mut s = ScrollState::new(1.0);
        s.value120(false, i32::MAX);
        s.value120(false, i32::MAX);
        assert_eq!(s.v120.1, i32::MAX);
    }

    #[test]
    fn ime_pending_cleared() {
        let mut i = ImeState {
            pending_commit: Some("x".into()),
            pending_delete: (1, 2),
            pending_preedit: Some((None, 0, 0)),
            ..Default::default()
        };
        i.clear_pending();
        assert!(i.pending_commit.is_none() && i.pending_preedit.is_none());
        assert_eq!(i.pending_delete, (0, 0));
    }

    #[test]
    fn wheel_frames_use_value120_not_pixels() {
        let mut s = ScrollState::new(1.0);
        // a wheel notch: axis px (10) + value120 (120) in one frame -> one line, no px echo
        s.axis(false, 10.0);
        s.value120(false, 120);
        let f = s.finish();
        assert_eq!((f.lines_y, f.px_y), (1, 0.0));
        assert!(!f.stop_y);
    }

    #[test]
    fn touchpad_frames_use_pixels_and_stop_ends_kinetics() {
        let mut s = ScrollState::new(1.0);
        s.axis(false, 4.0);
        let f = s.finish();
        assert_eq!((f.lines_y, f.px_y), (0, 4.0));
        s.axis(false, 7.0);
        let f = s.finish();
        assert_eq!(f.lines_y, 1); // 0.4 + 0.7 = 1.1 lines
        s.axis(false, 4.0);
        s.finish();
        s.stop(false);
        let f = s.finish();
        assert!(f.stop_y);
        // the remainder was dropped by axis_stop
        s.axis(false, 5.0);
        assert_eq!(s.finish().lines_y, 0);
    }

    #[test]
    fn axes_are_independent_and_empty_frames_are_empty() {
        let mut s = ScrollState::new(2.0);
        assert!(s.finish().is_empty());
        s.value120(true, 120);
        s.value120(false, -120);
        let f = s.finish();
        assert_eq!((f.lines_x, f.lines_y), (2, -2));
        s.set_multiplier(1.0);
        s.value120(false, 120);
        assert_eq!(s.finish().lines_y, 1);
    }
}
