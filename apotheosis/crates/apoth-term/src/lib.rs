// SPDX-License-Identifier: AGPL-3.0-only
//! Deterministic terminal: feed(bytes) -> grid state, PTY replies and events.
//!
//! Governing spec: T-03 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`).
//!
//! `Term::feed` does no I/O and reads no clock: the caller passes `now`. The same bytes
//! in any chunking, with the same `now` sequence, give the same state, replies and events.
//!
//! Decisions that fill spec gaps (reported to the orchestrator):
//! * `Wake` is a plain struct of four flags; the events are the typed [`EventSlot`], which
//!   `Term` owns and the io thread drains; `Term` also creates the `InputMirror` and hands
//!   out an `Arc` of it ([`Term::input_mirror`]).
//! * Configuration arrives in [`TermConfig`] at construction.
//! * The title stack is 10 deep and ignores a push when full.
//! * OSC 52 read (query) never replies from the term: it raises `EventSlot::clip_read` and
//!   main answers under the sequence policy (T-18 §3).
//! * XTGETTCAP (`DCS + q`), DECRQCRA outside `esctest` and XTSAVE are consumed.
//! * OSC 777 `notify;title;body` and OSC 99 (`p=title|body` payloads, optional `e=1` base64) push
//!   a notification event; other OSC 99 payload kinds and chunked assembly are consumed.
//! * A wide character at the last column blanks that cell and wraps (the grid has no
//!   `SpacerHead` writer); with autowrap off it is dropped.
//! * Hyperlinks are not carried across `ED 3` (grid has no history-clear call yet).
#![forbid(unsafe_code)]

mod color;
pub mod config;
mod csi;
pub mod events;
pub mod fuzz;
pub mod graphics;
mod machine;
pub mod modes;
mod osc;
mod sgr;
pub mod shell;
#[cfg(test)]
mod tests_batch;

use std::sync::Arc;
use std::time::Instant;

use apoth_grid::{Screen, SnapRow, Tag, Wide};
use apoth_input::InputMirror;
use apoth_vt::Parser;

pub use color::XTERM_ANSI16;

/// OSC 1337 user variables per pane (T-11 §9): names, name bytes, value bytes.
pub const USER_VARS_MAX: usize = 64;
pub const USER_VAR_NAME_MAX: usize = 64;
pub const USER_VAR_VALUE_MAX: usize = 1024;
/// The image crate this terminal speaks (decode jobs, store types).
pub use apoth_image;
pub use config::{CursorStyle, Rgb, TermConfig, deny};
pub use events::{
    ClipboardSet, CommandEvent, EventSlot, Mark, Notification, Progress, ReplyQueue, UserVar,
};
pub use graphics::{GraphicsConfig, ImageDraw, PlacementInfo};
pub use modes::{Access, ModeDef, ModeState};

/// What `feed` tells the io thread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Wake {
    /// The grid may have changed.
    pub dirty: bool,
    /// Replies are pending.
    pub replies: bool,
    /// Events are pending.
    pub events: bool,
    /// A synchronized-output presentation hold is in force.
    pub hold: bool,
    /// Image decode jobs are waiting for [`Term::take_image_jobs`].
    pub jobs: bool,
}

/// Cursor state for the renderer and tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorInfo {
    /// Active-area row.
    pub y: usize,
    /// Column.
    pub x: usize,
    /// A wrap is pending (the cursor sits after the last column).
    pub wrap_pending: bool,
    /// DECTCEM.
    pub visible: bool,
    /// Shape and blink.
    pub style: CursorStyle,
}

/// The terminal.
pub struct Term {
    parser: Parser,
    m: machine::Machine,
    /// Bytes that arrived while an image decode or consent held the stream.
    held: Vec<u8>,
}

impl std::fmt::Debug for Term {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Term")
            .field("rows", &self.m.rows)
            .field("cols", &self.m.cols)
            .finish_non_exhaustive()
    }
}

impl Term {
    /// A blank terminal.
    pub fn new(cfg: TermConfig) -> Term {
        let mirror = Arc::new(InputMirror::default());
        Term {
            parser: Parser::new(),
            m: machine::Machine::new(cfg, mirror),
            held: Vec::new(),
        }
    }

    /// Apply `bytes` to the screens. `now` is the caller's clock reading.
    ///
    /// While an image decode or a file-media consent is outstanding ([`Term::blocked`]) the
    /// bytes are held, unparsed, and run when [`Term::image_done`] or
    /// [`Term::graphics_file_decision`] releases the stream; the caller should stop reading
    /// the PTY meanwhile.
    pub fn feed(&mut self, bytes: &[u8], now: Instant) -> Wake {
        self.m.now = Some(now);
        self.m.expire_sync(now);
        if self.m.gfx.blocked() {
            self.held.extend_from_slice(bytes);
            return self.wake(false);
        }
        self.advance(bytes);
        self.m.gfx_collect();
        self.wake(!bytes.is_empty())
    }

    /// Parse `bytes`, stopping after the string terminator that raised an image barrier and
    /// holding the rest.
    fn advance(&mut self, bytes: &[u8]) {
        let n = self
            .parser
            .advance_until(bytes, &mut self.m, |m| m.gfx.blocked());
        if n < bytes.len() {
            self.held.extend_from_slice(&bytes[n..]);
        }
    }

    /// Run held bytes until the stream blocks again or they are gone.
    fn release(&mut self) {
        while !self.m.gfx.blocked() && !self.held.is_empty() {
            let held = std::mem::take(&mut self.held);
            self.advance(&held);
        }
        self.m.gfx_collect();
    }

    /// Timer check without bytes (the sync-output timeout, stale chunked transfers).
    pub fn tick(&mut self, now: Instant) -> Wake {
        self.m.now = Some(now);
        let before = self.m.sync.is_some();
        self.m.expire_sync(now);
        self.m.gfx.expire(now);
        self.wake(before && self.m.sync.is_none())
    }

    fn wake(&self, dirty: bool) -> Wake {
        Wake {
            dirty,
            replies: !self.m.replies.is_empty(),
            events: !self.m.events.is_empty(),
            hold: self.m.sync.is_some(),
            jobs: self.m.gfx.has_jobs(),
        }
    }

    /// The earliest timer: the end of the sync-output hold, or a half-received chunked
    /// image going stale.
    pub fn next_deadline(&self) -> Option<Instant> {
        let sync = self.m.sync.map(|s| s + self.m.cfg.sync_timeout_clamped());
        match (sync, self.m.gfx.deadline()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    // ---- images (T-15) ------------------------------------------------------------------

    /// The stream is held for an image decode or a file-media consent.
    pub fn blocked(&self) -> bool {
        self.m.gfx.blocked()
    }

    /// Decode jobs for the worker pool (never run them on the io thread).
    pub fn take_image_jobs(&mut self) -> Vec<apoth_image::kitty::DecodeJob> {
        self.m.gfx.take_jobs()
    }

    /// A worker's answer: store and place the image, reply, then run the held bytes.
    pub fn image_done(&mut self, done: apoth_image::kitty::DecodeDone, now: Instant) -> Wake {
        self.m.now = Some(now);
        self.m.gfx_done(done);
        self.release();
        self.wake(true)
    }

    /// Main's answer to [`EventSlot::graphics_file`]; `allow` false has no effect and no
    /// reply. The held bytes run afterwards.
    pub fn graphics_file_decision(&mut self, token: u64, allow: bool, now: Instant) -> Wake {
        self.m.now = Some(now);
        self.m.gfx_decision(token, allow);
        self.release();
        self.wake(true)
    }

    /// Image quads of the current screen for a viewport whose top is absolute row
    /// `top_abs` (counted from the oldest retained row), `rows` tall, bottom to top, showing
    /// the animation frames current at `now`. Returns when an animation frame is due next
    /// (hand it to the frame scheduler; nothing else wakes for it).
    pub fn image_draws(
        &self,
        top_abs: usize,
        rows: usize,
        now: Instant,
        out: &mut Vec<ImageDraw>,
    ) -> Option<Instant> {
        self.m.gfx_draws(top_abs, rows, now, out)
    }

    /// Any `apoth icat` session is open (T-16 §1.1): the caller then compares the pty's
    /// foreground process group after each feed and calls [`Term::bind_image_sessions`].
    pub fn has_image_sessions(&self) -> bool {
        !self.m.gfx.sessions_empty()
    }

    /// Bind sessions opened since the last call to the foreground process group `fg`, and
    /// end (removing every placement) the bound sessions whose group is no longer `fg`.
    /// Returns true when a placement went (redraw).
    pub fn bind_image_sessions(&mut self, fg: i32) -> bool {
        let unbound: Vec<u64> = self.m.gfx.unbound_sessions().collect();
        for id in unbound {
            self.m.gfx.bind_session(id, fg);
        }
        let gone: Vec<u64> = self.m.gfx.sessions_not_in(fg).collect();
        let any = !gone.is_empty();
        for id in gone {
            self.m.session_end(id, true);
        }
        any
    }

    /// Any image is placed on either screen.
    pub fn has_images(&self) -> bool {
        self.m.gfx.has_places()
    }

    /// Placements of the current screen, oldest first (tests, fixtures).
    pub fn placements(&self) -> Vec<PlacementInfo> {
        self.m.gfx_placements()
    }

    /// The pane's image store (quota tests).
    pub fn image_store(&self) -> &apoth_image::store::ImageStore {
        self.m.gfx.store()
    }

    /// The graphics caps and quotas in force (fuzz invariants).
    pub fn image_store_limits(&self) -> &apoth_image::Limits {
        &self.m.cfg.graphics.limits
    }

    /// True while the renderer should keep the last completed frame (mode 2026).
    pub fn presentation_hold(&self) -> bool {
        self.m.sync.is_some()
    }

    /// Resize; returns the (clamped) size now in force.
    pub fn resize(&mut self, rows: usize, cols: usize) -> (usize, usize) {
        self.m.resize(rows, cols)
    }

    /// New physical cell size; reports mode 2048 when set, even if rows/cols are unchanged.
    pub fn set_cell_px(&mut self, w: u32, h: u32) {
        self.m.cfg.cell_px = (w, h);
        if self.m.mode_on(false, 2048) {
            self.m.size_report();
        }
    }

    /// Full reset (RIS).
    pub fn reset(&mut self) {
        self.m.ris();
        self.parser = Parser::new();
    }

    /// Visible rows.
    pub fn rows(&self) -> usize {
        self.m.rows
    }

    /// Columns.
    pub fn cols(&self) -> usize {
        self.m.cols
    }

    /// The active screen.
    pub fn grid(&self) -> &Screen {
        &self.m.panes[self.m.cur].grid
    }

    /// The active screen, mutably (renderer snapshots).
    pub fn grid_mut(&mut self) -> &mut Screen {
        let c = self.m.cur;
        &mut self.m.panes[c].grid
    }

    /// The primary screen.
    pub fn primary(&self) -> &Screen {
        &self.m.panes[0].grid
    }

    /// The alternate screen.
    pub fn alternate(&self) -> &Screen {
        &self.m.panes[1].grid
    }

    /// True while the alternate screen is shown.
    pub fn on_alternate(&self) -> bool {
        self.m.cur == 1
    }

    /// Cursor position and style.
    pub fn cursor(&self) -> CursorInfo {
        CursorInfo {
            y: self.m.y,
            x: self.m.x,
            wrap_pending: self.m.wrap,
            visible: self.m.mode_on(false, 25),
            style: self.m.cursor_style,
        }
    }

    /// DECRQM state of a mode.
    pub fn mode_state(&self, ansi: bool, num: u16) -> ModeState {
        self.m.mode_state(ansi, num)
    }

    /// Reply queue (main moves units out in arrival order).
    pub fn replies(&mut self) -> &mut ReplyQueue {
        &mut self.m.replies
    }

    /// Event slot.
    pub fn events(&mut self) -> &mut EventSlot {
        &mut self.m.events
    }

    /// Event slot, read-only.
    pub fn events_ref(&self) -> &EventSlot {
        &self.m.events
    }

    /// The lock-free mirror main loads with `Acquire` (T-00 §4.2).
    pub fn input_mirror(&self) -> Arc<InputMirror> {
        self.m.mirror.clone()
    }

    /// Command blocks of the primary screen (T-14 §1).
    pub fn blocks(&self) -> &shell::Blocks {
        &self.m.blocks
    }

    /// The cursor is in an OSC 133 prompt or input zone (T-18 §4 (a), (e)).
    pub fn at_prompt(&self) -> bool {
        self.m.blocks.at_prompt()
    }

    /// Bracketed paste is set and counts as in force: a primary prompt mark has been seen
    /// since it was last set (T-18 §4).
    pub fn bracketed_in_force(&self) -> bool {
        self.m.mode_on(false, 2004) && self.m.bp_prompted
    }

    /// Install the denied sequence classes ([`deny`] bits, T-18 §3). Called between feeds.
    pub fn set_deny(&mut self, bits: u32) {
        self.m.cfg.deny = bits;
    }

    /// Denied sequence classes now in force.
    pub fn deny_bits(&self) -> u32 {
        self.m.cfg.deny
    }

    /// Current title (sanitised).
    pub fn title(&self) -> &str {
        &self.m.title
    }

    /// Palette entry.
    pub fn palette(&self, i: u8) -> Rgb {
        self.m.palette[usize::from(i)]
    }

    /// Change the default colours (a theme change, T-17 §2). A colour the running program
    /// has not changed (it still equals the old default) follows; one it set (OSC 4/10-12)
    /// keeps its value. When the light/dark class changes and mode 2031 is set, the
    /// `CSI ? 997 ; 1|2 n` report is queued (T-10 §7).
    pub fn set_default_colors(
        &mut self,
        fg: Rgb,
        bg: Rgb,
        cursor: Rgb,
        ansi: [Rgb; 16],
        dark: bool,
    ) {
        let m = &mut self.m;
        let c = &m.cfg;
        if (c.fg, c.bg, c.cursor_color, c.ansi, c.dark) == (fg, bg, cursor, ansi, dark) {
            return;
        }
        let follow = |cur: &mut Rgb, old: Rgb, new: Rgb| {
            if *cur == old {
                *cur = new;
            }
        };
        follow(&mut m.fg, m.cfg.fg, fg);
        follow(&mut m.bg, m.cfg.bg, bg);
        follow(&mut m.cursor_color, m.cfg.cursor_color, cursor);
        for (i, new) in ansi.iter().enumerate() {
            follow(&mut m.palette[i], m.cfg.ansi[i], *new);
        }
        let flipped = m.cfg.dark != dark;
        m.cfg.fg = fg;
        m.cfg.bg = bg;
        m.cfg.cursor_color = cursor;
        m.cfg.ansi = ansi;
        m.cfg.dark = dark;
        m.events.colors_changed = true;
        if flipped && m.mode_on(false, 2031) {
            let v = if dark { 1 } else { 2 };
            m.reply_str(format!("\x1b[?997;{v}n"));
        }
    }

    /// Default foreground, background and cursor colours now in force.
    pub fn colors(&self) -> (Rgb, Rgb, Rgb) {
        (self.m.fg, self.m.bg, self.m.cursor_color)
    }

    /// OSC 1337 user variables, by name.
    pub fn user_vars(&self) -> &std::collections::BTreeMap<String, String> {
        &self.m.user_vars
    }

    /// Kitty keyboard flags of the active screen.
    pub fn kitty_flags(&self) -> u32 {
        self.m.kitty.current()
    }

    /// Text of visible row `y` (spacer cells skipped, blanks as spaces, not trimmed).
    pub fn row_text(&self, y: usize) -> String {
        row_text(self.grid(), y)
    }
}

/// Text of row `y` of `g`.
pub fn row_text(g: &Screen, y: usize) -> String {
    let mut s = String::new();
    let mut cps = Vec::new();
    for x in 0..g.cols() {
        let c = g.cell(y, x);
        match c.wide() {
            Wide::SpacerTail => continue,
            Wide::SpacerHead => {
                s.push(' ');
                continue;
            }
            _ => {}
        }
        match c.tag() {
            Tag::Scalar | Tag::Cluster => {
                g.cell_text(y, x, &mut cps);
                for &cp in &cps {
                    s.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                }
            }
            _ => s.push(' '),
        }
    }
    s
}

/// Text of row `abs` counted from the oldest retained row (scrollback excerpts).
pub fn abs_row_text(g: &Screen, abs: usize) -> Option<String> {
    let mut row = SnapRow::default();
    if !g.copy_abs_row(abs, &mut row) {
        return None;
    }
    let mut s = String::new();
    for c in &row.cells {
        match c.wide {
            Wide::SpacerTail => continue,
            Wide::SpacerHead => {
                s.push(' ');
                continue;
            }
            _ => {}
        }
        if c.cluster_len > 0 {
            for &cp in row.cluster(c) {
                s.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
            }
        } else if c.cp == 0 {
            s.push(' ');
        } else {
            s.push(char::from_u32(c.cp).unwrap_or('\u{fffd}'));
        }
    }
    Some(s)
}
