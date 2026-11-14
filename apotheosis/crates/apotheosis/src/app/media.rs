// SPDX-License-Identifier: AGPL-3.0-only
//! Media windows (`apoth icat --ext`, T-16 §1.1, T-12 §2 `media.window-*`): a borderless
//! glass window holding one grid with no PTY, fed only by the control-socket connection that
//! opened it. The grid runs on a feeder thread (parse and image decode never touch the main
//! loop) and draws through its own render thread, as the main window does. The window
//! belongs to the connection: it closes when the connection does, on Escape or `q`, or when
//! the compositor closes it. Content is never logged: ids, sizes and counts only.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::Instant;

use apoth_proto::{ErrorCode, Reply, WireError};
use apoth_term::Term;
use apoth_wl::window::DecorationPref;
use apoth_wl::{ConfigureEvent, WindowId, WindowSpec};
use calloop::RegistrationToken;

use apoth_ui::mediawin::{self, MediaWinSpec};

use super::{App, build_fonts};
use crate::pane::{MainMsg, PaneShared, RenderMsg};
use crate::render::{
    ChromeModel, PaneView, RenderArgs, RenderSettings, TermDefaults, WindowLayout, pad_px,
};

/// Media windows open at once (all connections together).
const MAX_WINDOWS: usize = 8;
/// Bytes queued for a window's feeder before writes are refused.
const MAX_QUEUED: usize = 64 << 20;
/// How often a media window with a playing placement collects its frames.
const MEDIA_POLL: std::time::Duration = std::time::Duration::from_millis(8);
/// evdev codes that close a focused media window.
const KEY_ESC: u32 = 1;
const KEY_Q: u32 = 16;

#[derive(Default)]
pub(super) struct Media {
    windows: BTreeMap<u64, MediaWin>,
    next: u64,
    /// The media window with the keyboard focus.
    kb: Option<u64>,
    /// The media window under the pointer.
    ptr: Option<u64>,
}

struct MediaWin {
    /// The control-socket client that owns it.
    owner: u64,
    win: WindowId,
    /// Rows, columns.
    grid: (usize, usize),
    shared: Arc<PaneShared>,
    defaults: TermDefaults,
    feed: Option<mpsc::SyncSender<Vec<u8>>>,
    queued: Arc<std::sync::atomic::AtomicUsize>,
    feeder: Option<JoinHandle<()>>,
    render_tx: mpsc::Sender<RenderMsg>,
    render_rx: Option<mpsc::Receiver<RenderMsg>>,
    render: Option<JoinHandle<()>>,
    chan: Option<RegistrationToken>,
    /// `media.window-wait` replies owed: (client, request id).
    waits: Vec<(u64, u64)>,
    closing: bool,
    /// Pad (physical px) once running, the window's size in interface units, and the
    /// chrome it shows (focus rim, hover close and hint).
    pad: (i32, i32),
    size_ui: (f32, f32),
    chrome: MediaWinSpec,
}

/// The window's logical size for `grid` rows and columns of `cell`-px cells at `scale`. The
/// grid keeps one spare row below the picture (the cursor's move after a placement must not
/// scroll it): the window is cut above it.
fn media_size(
    grid: (usize, usize),
    cell: (u32, u32),
    settings: &crate::settings::Settings,
    scale: f64,
) -> (u32, u32) {
    let pad = (
        pad_px(settings.padding.0, scale),
        pad_px(settings.padding.1, scale),
    );
    let shown = grid.0.saturating_sub(1).max(1) as u32;
    let pw = grid.1 as u32 * cell.0 + 2 * pad.0;
    let ph = shown * cell.1 + 2 * pad.1;
    (
        (f64::from(pw) / scale).ceil() as u32,
        (f64::from(ph) / scale).ceil() as u32,
    )
}

fn err(code: ErrorCode, msg: &str) -> WireError {
    WireError::new(code, msg)
}

/// The feeder: bytes in arrival order through the term, image decodes inline (this thread is
/// the window's own), then one poke for the render thread.
fn feeder(
    shared: Arc<PaneShared>,
    rx: mpsc::Receiver<Vec<u8>>,
    queued: Arc<std::sync::atomic::AtomicUsize>,
    poke: mpsc::Sender<RenderMsg>,
) {
    let mut deadline: Option<Instant> = None;
    // `apoth play --ext` (T-16 §2): players here report by polling while any plays (the
    // feed channel cannot carry a poke without keeping itself open).
    let mut host = crate::mediahost::MediaHost::new(Arc::new(|| {}));
    loop {
        if !host.is_empty() {
            let poll = Instant::now() + MEDIA_POLL;
            deadline = Some(deadline.map_or(poll, |d| d.min(poll)));
        }
        let msg = match deadline {
            Some(d) => match rx.recv_timeout(d.saturating_duration_since(Instant::now())) {
                Ok(b) => Some(b),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            },
            None => match rx.recv() {
                Ok(b) => Some(b),
                Err(_) => return,
            },
        };
        // The window was opened by an explicit command: its audio counts as focused.
        let evs = host.drain(true);
        let mut jobs = {
            let mut t = shared.lock_io();
            let now = Instant::now();
            for (key, ev) in evs {
                t.media_event(key, ev, now);
            }
            match &msg {
                Some(b) => {
                    queued.fetch_sub(b.len(), Ordering::AcqRel);
                    t.feed(b, Instant::now());
                }
                None => {
                    t.tick(Instant::now());
                }
            }
            t.take_image_jobs()
        };
        while !jobs.is_empty() {
            let mut next = Vec::new();
            for job in jobs {
                let done = job.run_with(&mut *crate::mediahost::image_decoder());
                let mut t = shared.lock_io();
                t.image_done(done, Instant::now());
                next.extend(t.take_image_jobs());
            }
            jobs = next;
        }
        {
            let mut t = shared.lock_io();
            // Nothing answers: replies and events have no reader here.
            while t.replies().pop().is_some() {}
            let _ = crate::pane::take_events(&mut t);
            deadline = t.next_deadline();
            let reqs = t.take_media_requests();
            drop(t);
            if !reqs.is_empty() {
                host.requests(reqs, true);
            }
        }
        if !shared.dirty.swap(true, Ordering::AcqRel) {
            let _ = poke.send(RenderMsg::Poke);
        }
    }
}

impl App {
    pub(super) fn media_open(
        &mut self,
        cid: u64,
        p: apoth_proto::params::MediaWindowOpen,
    ) -> Result<Option<serde_json::Value>, WireError> {
        if self.media.windows.len() >= MAX_WINDOWS {
            return Err(err(ErrorCode::BadRequest, "too many media windows"));
        }
        let grid = (p.rows as usize, p.cols as usize);
        let mut tc = self.settings.term.clone();
        tc.rows = grid.0;
        tc.cols = grid.1;
        tc.cell_px = self.cell;
        let term = Term::new(tc);
        let defaults = TermDefaults {
            fg: term.colors().0,
            bg: term.colors().1,
            cursor: term.colors().2,
            ansi: std::array::from_fn(|i| term.palette(i as u8)),
        };
        let shared = Arc::new(PaneShared::new(term));
        shared.deny.store(self.deny_word(), Ordering::Release);
        let (render_tx, render_rx) = mpsc::channel();
        let (feed, rx) = mpsc::sync_channel::<Vec<u8>>(256);
        // No cursor: the window shows images, not a prompt (counted like any write).
        const HIDE_CURSOR: &[u8] = b"\x1b[?25l";
        let queued = Arc::new(std::sync::atomic::AtomicUsize::new(HIDE_CURSOR.len()));
        let feeder = {
            let (s, q, poke) = (Arc::clone(&shared), Arc::clone(&queued), render_tx.clone());
            std::thread::Builder::new()
                .name("apoth-media".into())
                .spawn(move || feeder(s, rx, q, poke))
                .map_err(|_| err(ErrorCode::Internal, "cannot start the media window"))?
        };
        let _ = feed.send(HIDE_CURSOR.to_vec());
        let title = p.title.as_deref().map_or_else(
            || "Apotheosis media".to_owned(),
            |t| super::clip(t, 256).to_owned(),
        );
        let win = self.handle.create_window(WindowSpec {
            title,
            app_id: Some(self.app_id.clone()),
            decorations: DecorationPref::None,
            ..WindowSpec::default()
        });
        // A fixed size before the first commit, so the compositor floats the window rather
        // than tiling it (the main window's scale stands in until the first configure).
        let scale = self.scale();
        let (lw, lh) = media_size(grid, self.cell, &self.settings, scale);
        self.handle.set_min_size(win, Some((lw, lh)));
        self.handle.set_max_size(win, Some((lw, lh)));
        self.handle.flush();
        self.media.next += 1;
        let id = self.media.next;
        self.media.windows.insert(
            id,
            MediaWin {
                owner: cid,
                win,
                grid,
                shared,
                defaults,
                feed: Some(feed),
                queued,
                feeder: Some(feeder),
                render_tx,
                render_rx: Some(render_rx),
                render: None,
                chan: None,
                waits: Vec::new(),
                closing: false,
                pad: (0, 0),
                size_ui: (0.0, 0.0),
                chrome: MediaWinSpec::default(),
            },
        );
        tracing::debug!(id, rows = grid.0, cols = grid.1, "media window opened");
        self.write_mux_dump();
        Ok(Some(serde_json::json!({ "window": id })))
    }

    pub(super) fn media_write(
        &mut self,
        cid: u64,
        p: apoth_proto::params::MediaWindowWrite,
    ) -> Result<Option<serde_json::Value>, WireError> {
        use base64::Engine;
        let w = self
            .media
            .windows
            .get(&p.window)
            .filter(|w| w.owner == cid && !w.closing)
            .ok_or_else(|| err(ErrorCode::BadRequest, "no such media window"))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(p.data.as_bytes())
            .map_err(|_| err(ErrorCode::BadRequest, "data is not base64"))?;
        if w.queued.load(Ordering::Acquire).saturating_add(bytes.len()) > MAX_QUEUED {
            return Err(err(ErrorCode::Internal, "the media window is behind"));
        }
        let n = bytes.len();
        w.queued.fetch_add(n, Ordering::AcqRel);
        let sent = w.feed.as_ref().is_some_and(|f| f.try_send(bytes).is_ok());
        if !sent {
            w.queued.fetch_sub(n, Ordering::AcqRel);
            return Err(err(ErrorCode::Internal, "the media window is behind"));
        }
        Ok(Some(serde_json::Value::Null))
    }

    /// Deferred: answered with `{closed: true}` when the window closes.
    pub(super) fn media_wait(
        &mut self,
        cid: u64,
        rid: u64,
        p: apoth_proto::params::MediaWindowWait,
    ) -> Result<Option<serde_json::Value>, WireError> {
        let w = self
            .media
            .windows
            .get_mut(&p.window)
            .filter(|w| w.owner == cid)
            .ok_or_else(|| err(ErrorCode::BadRequest, "no such media window"))?;
        if w.closing {
            return Ok(Some(serde_json::json!({ "closed": true })));
        }
        w.waits.push((cid, rid));
        Ok(None)
    }

    /// The media window of a Wayland window id.
    pub(super) fn media_of(&self, win: WindowId) -> Option<u64> {
        self.media
            .windows
            .iter()
            .find(|(_, w)| w.win == win)
            .map(|(id, _)| *id)
    }

    /// A configure for a media window: the first sizes it and starts its render thread.
    pub(super) fn media_configure(&mut self, id: u64, c: &ConfigureEvent) {
        let Some(w) = self.media.windows.get_mut(&id) else {
            return;
        };
        if w.render.is_some() || w.closing {
            let _ = w.render_tx.send(RenderMsg::Poke);
            return;
        }
        let Some(rx) = w.render_rx.take() else { return };
        let (win, grid) = (w.win, w.grid);
        let scale = self.handle.scale(win).map_or(1.0, |s| s.as_f64());
        let started = (|| -> Result<(), String> {
            let (fonts, m, _) = build_fonts(&self.settings, scale)?;
            let cell = (m.width as u32, m.height as u32);
            let pad = (
                pad_px(self.settings.padding.0, scale),
                pad_px(self.settings.padding.1, scale),
            );
            if c.size.is_none() {
                // The grid keeps one spare row below the picture (the cursor's move after
                // a placement must not scroll it): the window is cut above it.
                let (lw, lh) = media_size(grid, cell, &self.settings, scale);
                self.handle.set_window_size(win, (lw, lh));
                // A fixed size: the compositor floats it instead of tiling it.
                self.handle.set_min_size(win, Some((lw, lh)));
                self.handle.set_max_size(win, Some((lw, lh)));
            }
            let (_, buffer) = self
                .handle
                .surface_size(win)
                .ok_or("media window has no size")?;
            let surface = self
                .handle
                .surface_handle(win)
                .ok_or("media window has no surface")?;
            let pacer = self.handle.pacer(win).ok_or("media window has no pacer")?;
            // Its own channel: a render failure closes this window, never the terminal.
            let (to_main, chan_rx) = calloop::channel::channel::<MainMsg>();
            let token = self
                .lh
                .insert_source(chan_rx, move |ev, (), app: &mut App| {
                    if let calloop::channel::Event::Msg(MainMsg::RenderFailed(_)) = ev {
                        tracing::warn!(id, "a media window cannot render");
                        app.media_close(id);
                    }
                })
                .map_err(|e| e.error.to_string())?;
            let zoom = self.settings.glass.interface_zoom.max(0.01);
            let logical = self.handle.surface_size(win).map_or((0, 0), |(l, _)| l);
            let w = self.media.windows.get_mut(&id).ok_or("gone")?;
            w.pad = (pad.0 as i32, pad.1 as i32);
            w.size_ui = (logical.0 as f32 / zoom, logical.1 as f32 / zoom);
            w.chan = Some(token);
            {
                let mut t = w.shared.lock();
                t.set_cell_px(cell.0, cell.1);
            }
            let render = crate::render::spawn(RenderArgs {
                surface,
                pacer,
                fonts,
                settings: RenderSettings::from_settings(&self.settings, self.handle.blur_path(win)),
                scale,
                buffer,
                rx,
                poke: w.render_tx.clone(),
                to_main,
            })
            .map_err(|e| e.to_string())?;
            w.render = Some(render);
            Self::media_layout(id, w);
            Ok(())
        })();
        if let Err(why) = started {
            tracing::warn!(id, %why, "a media window could not start");
            self.media_close(id);
        }
    }

    /// Send the window's layout and chrome to its render thread.
    fn media_layout(id: u64, w: &MediaWin) {
        let view = PaneView {
            id,
            shared: Arc::clone(&w.shared),
            defaults: w.defaults.clone(),
            origin: w.pad,
            focused: false,
            first: false,
            floating: false,
        };
        let mut chrome = ChromeModel::empty();
        chrome.media = Some(w.chrome.clone());
        let _ = w.render_tx.send(RenderMsg::Layout(Box::new(WindowLayout {
            panes: vec![view],
            chrome,
        })));
    }

    /// Recompute the chrome state from the focus and pointer; resend when it changed.
    fn media_chrome(&mut self, id: u64, f: impl FnOnce(&mut MediaWin)) {
        let Some(w) = self.media.windows.get_mut(&id) else {
            return;
        };
        let before = w.chrome.clone();
        f(w);
        if w.chrome != before && w.render.is_some() && !w.closing {
            Self::media_layout(id, w);
        }
    }

    /// Wayland events for a media window. Returns false when `ev` is not one.
    pub(super) fn media_wl(&mut self, ev: &apoth_wl::WlEvent) -> bool {
        use apoth_wl::WlEvent as E;
        let win = match ev {
            E::Configure(c) => c.window,
            E::CloseRequested(w) => *w,
            E::ScaleChanged { window, .. }
            | E::Frame { window, .. }
            | E::PresentationDiscarded { window, .. }
            | E::BlurPathChanged { window, .. } => *window,
            E::Presented(p) => p.window,
            _ => return false,
        };
        let Some(id) = self.media_of(win) else {
            return false;
        };
        match ev {
            E::Configure(c) => self.media_configure(id, c),
            E::CloseRequested(_) => self.media_close(id),
            _ => {
                if let Some(w) = self.media.windows.get(&id) {
                    let _ = w.render_tx.send(RenderMsg::Poke);
                }
            }
        }
        true
    }

    /// Input routing: focus and pointer enter/leave for media windows, and every key or
    /// pointer event while one has them. Returns true when the event was a media window's.
    pub(super) fn media_input(&mut self, kind: &apoth_wl::InputKind) -> bool {
        use apoth_wl::InputKind as K;
        match kind {
            K::KeyboardEnter { window, .. } => {
                self.media.kb = self.media_of(*window);
                if let Some(id) = self.media.kb {
                    self.media_chrome(id, |w| w.chrome.focused = true);
                }
                self.media.kb.is_some()
            }
            K::KeyboardLeave { window } => {
                if let Some(id) = self.media_of(*window) {
                    self.media.kb = None;
                    self.media_chrome(id, |w| w.chrome.focused = false);
                    return true;
                }
                false
            }
            K::PointerEnter { window, x, y, .. } => {
                self.media.ptr = self.media_of(*window);
                if let Some(id) = self.media.ptr {
                    self.media_pointer(id, Some((*x, *y)));
                }
                self.media.ptr.is_some()
            }
            K::PointerLeave { window } => {
                if let Some(id) = self.media_of(*window) {
                    self.media.ptr = None;
                    self.media_pointer(id, None);
                    return true;
                }
                false
            }
            K::Key { key, pressed, .. } => {
                let Some(id) = self.media.kb else {
                    return false;
                };
                if *pressed && (*key == KEY_ESC || *key == KEY_Q) {
                    self.media_close(id);
                }
                true
            }
            K::PointerMotion { x, y } => {
                let Some(id) = self.media.ptr else {
                    return false;
                };
                self.media_pointer(id, Some((*x, *y)));
                true
            }
            K::PointerButton {
                button, pressed, ..
            } => {
                const BTN_LEFT: u32 = 0x110;
                let Some(id) = self.media.ptr else {
                    return false;
                };
                if *button == BTN_LEFT {
                    let over = self
                        .media
                        .windows
                        .get(&id)
                        .is_some_and(|w| w.chrome.close.hover);
                    if over && *pressed {
                        self.media_chrome(id, |w| w.chrome.close.pressed = true);
                    } else if !*pressed {
                        let was = self
                            .media
                            .windows
                            .get(&id)
                            .is_some_and(|w| w.chrome.close.pressed);
                        self.media_chrome(id, |w| w.chrome.close.pressed = false);
                        if was && over {
                            self.media_close(id);
                        }
                    }
                }
                true
            }
            K::Scroll(_) => self.media.ptr.is_some(),
            _ => false,
        }
    }

    /// The pointer entered, moved (interface units after the zoom) or left window `id`.
    fn media_pointer(&mut self, id: u64, at: Option<(f64, f64)>) {
        let zoom = self.settings.glass.interface_zoom.max(0.01);
        self.media_chrome(id, |w| {
            let Some((x, y)) = at else {
                w.chrome.hovered = false;
                w.chrome.close = apoth_ui::ControlState::default();
                return;
            };
            let area = apoth_ui::Rect::new(0.0, 0.0, w.size_ui.0, w.size_ui.1);
            let p = apoth_ui::Point::new(x as f32 / zoom, y as f32 / zoom);
            w.chrome.hovered = true;
            w.chrome.close.hover = mediawin::close_rect(area).contains(p);
        });
    }

    /// Close a media window: answer its waits, stop its threads off the loop, then destroy
    /// the Wayland window once the render thread has let go of the surface.
    pub(super) fn media_close(&mut self, id: u64) {
        let Some(w) = self.media.windows.get_mut(&id) else {
            return;
        };
        if w.closing {
            return;
        }
        w.closing = true;
        if self.media.kb == Some(id) {
            self.media.kb = None;
        }
        if self.media.ptr == Some(id) {
            self.media.ptr = None;
        }
        let waits = std::mem::take(&mut w.waits);
        w.feed = None;
        let _ = w.render_tx.send(RenderMsg::Stop);
        let (render, feeder) = (w.render.take(), w.feeder.take());
        if let Some(t) = w.chan.take() {
            self.lh.remove(t);
        }
        let tx = self.to_main.clone();
        let joined = std::thread::Builder::new()
            .name("apoth-media-close".into())
            .spawn(move || {
                if let Some(r) = render {
                    let _ = r.join();
                }
                if let Some(f) = feeder {
                    let _ = f.join();
                }
                let _ = tx.send(MainMsg::MediaStopped(id));
            });
        if joined.is_err() {
            self.media_stopped(id);
        }
        for (cid, rid) in waits {
            let r = Reply::ok(rid, &serde_json::json!({ "closed": true })).unwrap_or_else(|e| {
                Reply::Err {
                    id: Some(rid),
                    error: e,
                }
            });
            self.ctl_send(cid, &r);
        }
        tracing::debug!(id, "media window closed");
    }

    /// The window's threads are gone: the surface can go.
    pub(super) fn media_stopped(&mut self, id: u64) {
        if let Some(w) = self.media.windows.remove(&id) {
            self.handle.destroy_window(w.win);
            self.handle.flush();
        }
        self.write_mux_dump();
    }

    /// A control client went away: its windows close.
    pub(super) fn media_drop_client(&mut self, cid: u64) {
        let owned: Vec<u64> = self
            .media
            .windows
            .iter()
            .filter(|(_, w)| w.owner == cid)
            .map(|(id, _)| *id)
            .collect();
        for id in owned {
            if let Some(w) = self.media.windows.get_mut(&id) {
                // The owner cannot read a reply any more.
                w.waits.clear();
            }
            self.media_close(id);
        }
    }

    /// `media <id> rows <r> cols <c>` lines for the test dump.
    pub(super) fn media_dump(&self, s: &mut String) {
        use std::fmt::Write as _;
        for (id, w) in &self.media.windows {
            if !w.closing {
                let _ = writeln!(s, "media {id} rows {} cols {}", w.grid.0, w.grid.1);
            }
        }
    }

    /// Shutdown: stop every media window's threads (bounded) before the connection goes.
    pub(super) fn media_shutdown(&mut self) {
        let ids: Vec<u64> = self.media.windows.keys().copied().collect();
        for id in ids {
            let Some(mut w) = self.media.windows.remove(&id) else {
                continue;
            };
            w.feed = None;
            let _ = w.render_tx.send(RenderMsg::Stop);
            if let Some(r) = w.render.take() {
                let deadline = Instant::now() + std::time::Duration::from_secs(2);
                while !r.is_finished() && Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                if r.is_finished() {
                    let _ = r.join();
                }
            }
            self.handle.destroy_window(w.win);
        }
    }
}
