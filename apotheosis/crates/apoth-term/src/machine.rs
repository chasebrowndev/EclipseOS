// SPDX-License-Identifier: AGPL-3.0-only
//! The terminal state machine: an `apoth_vt::Sink` driving two `apoth_grid::Screen`s.
//!
//! Replies and events are queued, never written; nothing here reads a clock or does I/O.

use std::sync::Arc;
use std::time::Instant;

use apoth_grid::{
    CellAttrs, Color, Glyph, Region, Screen, ScreenConfig, Semantic, Style, TabStops, flags,
};
use apoth_input::modes::bit;
use apoth_input::{InputMirror, InputModes, KittyState};
use apoth_unicode::{ClusterMeasure, Segmenter, WidthMethod};
use apoth_vt::{Params, Sink, StrEnd, StrHead, StrKind, StrRoute};

use crate::color;
use crate::config::{CursorStyle, Rgb, TermConfig};
use crate::events::{EventSlot, ReplyQueue};
use crate::modes::{self, Access, ModeState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Charset {
    Ascii,
    Dec,
    Uk,
}

impl Charset {
    pub(crate) fn map(self, c: char) -> char {
        match self {
            Charset::Ascii => c,
            Charset::Uk => {
                if c == '#' {
                    '\u{a3}'
                } else {
                    c
                }
            }
            Charset::Dec => {
                const T: [char; 31] = [
                    '\u{25c6}', '\u{2592}', '\u{2409}', '\u{240c}', '\u{240d}', '\u{240a}',
                    '\u{b0}', '\u{b1}', '\u{2424}', '\u{240b}', '\u{2518}', '\u{2510}', '\u{250c}',
                    '\u{2514}', '\u{253c}', '\u{23ba}', '\u{23bb}', '\u{2500}', '\u{23bc}',
                    '\u{23bd}', '\u{251c}', '\u{2524}', '\u{2534}', '\u{252c}', '\u{2502}',
                    '\u{2264}', '\u{2265}', '\u{3c0}', '\u{2260}', '\u{a3}', '\u{b7}',
                ];
                match c {
                    '\u{5f}' => '\u{a0}',
                    '\u{60}'..='\u{7e}' => T[(c as usize) - 0x60],
                    _ => c,
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Saved {
    pub y: usize,
    pub x: usize,
    pub wrap: bool,
    pub pen: Style,
    pub protected: bool,
    pub origin: bool,
    pub charsets: [Charset; 4],
    pub gl: usize,
}

pub(crate) struct Pane {
    pub grid: Screen,
    pub saved: Option<Saved>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StrMode {
    None,
    Title(u32),
    Palette,
    Dyn(u32),
    ResetPalette,
    ResetDyn(u32),
    Cwd,
    Link,
    Clip,
    Mark,
    Notify,
    Notify777,
    Notify99,
    Progress,
    UserVar,
    Decrqss,
    Tcap,
    /// Kitty graphics APC (body starts at `G`).
    Kitty,
    /// Sixel DCS: bytes stream into the decoder, not `sbuf`.
    Sixel,
}

pub(crate) struct Machine {
    pub cfg: TermConfig,
    pub panes: [Pane; 2],
    pub cur: usize,
    pub rows: usize,
    pub cols: usize,
    // cursor (shared by both screens)
    pub y: usize,
    pub x: usize,
    pub wrap: bool,
    // pen
    pub pen: Style,
    pub protected: bool,
    pub semantic: Semantic,
    pub link: Option<(String, String)>,
    // margins
    pub top: usize,
    pub bottom: usize,
    pub left: usize,
    pub right: usize,
    pub tabs: TabStops,
    pub charsets: [Charset; 4],
    pub gl: usize,
    pub ss: Option<usize>,
    // modes
    pub modes: u128,
    pub autowrap: bool,
    pub irm: bool,
    pub lnm: bool,
    pub origin: bool,
    pub lrmm: bool,
    pub wide_mode: bool,
    pub cursor_style: CursorStyle,
    // text state
    pub seg: Segmenter,
    pub measure: ClusterMeasure,
    pub last: Option<(usize, usize, u8)>,
    pub last_ascii: bool,
    pub last_char: Option<char>,
    // input mirror
    pub mirror: Arc<InputMirror>,
    pub published: InputModes,
    pub in_bits: u64,
    pub kitty: KittyState,
    // outputs
    pub replies: ReplyQueue,
    pub events: EventSlot,
    /// OSC 1337 user variables (T-11 §9), at most [`crate::USER_VARS_MAX`] names.
    pub user_vars: std::collections::BTreeMap<String, String>,
    // colours and titles
    pub palette: Box<[Rgb; 256]>,
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor_color: Rgb,
    pub title: String,
    pub icon: String,
    pub title_stack: Vec<(String, String)>,
    // time and strings
    pub sync: Option<Instant>,
    pub now: Option<Instant>,
    pub strm: StrMode,
    pub sbuf: Vec<u8>,
    // shell integration (T-14 §1, T-18 §4)
    pub blocks: crate::shell::Blocks,
    /// A primary OSC 133 prompt was seen since mode 2004 was last set.
    pub bp_prompted: bool,
    /// The last accepted OSC 7 URI (a block's cwd).
    pub cwd_uri: Option<String>,
    /// Images (T-15).
    pub gfx: Box<crate::graphics::Graphics>,
    /// Tests: print one character at a time, bypassing the batching path.
    #[cfg(test)]
    pub no_batch: bool,
}

pub(crate) fn mk_attrs<'a>(
    pen: Style,
    link: &'a Option<(String, String)>,
    protected: bool,
    semantic: Semantic,
) -> CellAttrs<'a> {
    CellAttrs {
        style: pen,
        link: link.as_ref().map(|(a, b)| (a.as_str(), b.as_str())),
        protected,
        semantic,
    }
}

impl Machine {
    pub(crate) fn new(cfg: TermConfig, mirror: Arc<InputMirror>) -> Machine {
        let main = Screen::new(
            cfg.rows,
            cfg.cols,
            ScreenConfig {
                history: true,
                limits: cfg.scrollback,
            },
        );
        let (rows, cols) = (main.rows(), main.cols());
        let alt = Screen::new(
            rows,
            cols,
            ScreenConfig {
                history: false,
                limits: cfg.scrollback,
            },
        );
        let modes = modes::default_word();
        let in_bits = InputModes::default().raw().0;
        let m = Machine {
            panes: [
                Pane {
                    grid: main,
                    saved: None,
                },
                Pane {
                    grid: alt,
                    saved: None,
                },
            ],
            cur: 0,
            rows,
            cols,
            y: 0,
            x: 0,
            wrap: false,
            pen: Style::DEFAULT,
            protected: false,
            semantic: Semantic::Output,
            link: None,
            top: 0,
            bottom: rows - 1,
            left: 0,
            right: cols - 1,
            tabs: TabStops::new(cols),
            charsets: [Charset::Ascii; 4],
            gl: 0,
            ss: None,
            modes,
            autowrap: true,
            irm: false,
            lnm: false,
            origin: false,
            lrmm: false,
            wide_mode: false,
            cursor_style: cfg.cursor,
            seg: Segmenter::new(),
            measure: ClusterMeasure::new(),
            last: None,
            last_ascii: false,
            last_char: None,
            published: InputModes::default(),
            mirror,
            in_bits,
            kitty: KittyState::default(),
            replies: ReplyQueue::default(),
            events: EventSlot::default(),
            user_vars: std::collections::BTreeMap::new(),
            palette: Box::new(color::default_palette(&cfg.ansi)),
            fg: cfg.fg,
            bg: cfg.bg,
            cursor_color: cfg.cursor_color,
            title: String::new(),
            icon: String::new(),
            title_stack: Vec::new(),
            sync: None,
            now: None,
            strm: StrMode::None,
            sbuf: Vec::new(),
            blocks: crate::shell::Blocks::default(),
            bp_prompted: false,
            cwd_uri: None,
            gfx: Box::new(crate::graphics::Graphics::new(&cfg.graphics)),
            #[cfg(test)]
            no_batch: false,
            cfg,
        };
        m.mirror.store(m.published);
        m
    }

    // ---- helpers -----------------------------------------------------------------------

    pub(crate) fn width_method(&self) -> WidthMethod {
        if self.wide_mode {
            WidthMethod::Unicode
        } else {
            self.cfg.widths.method
        }
    }

    pub(crate) fn reply(&mut self, bytes: Vec<u8>) {
        self.replies.push(bytes);
    }

    pub(crate) fn reply_str(&mut self, s: String) {
        self.replies.push(s.into_bytes());
    }

    pub(crate) fn cursor_moved(&mut self) {
        self.seg.reset();
        self.last = None;
        self.last_ascii = false;
    }

    pub(crate) fn set_in(&mut self, mask: u64, on: bool) {
        if on {
            self.in_bits |= mask;
        } else {
            self.in_bits &= !mask;
        }
    }

    /// Store the input mirror (Release) before the next byte is parsed.
    pub(crate) fn publish(&mut self) {
        let m = InputModes::from_raw(self.in_bits, self.kitty.current());
        if m != self.published {
            self.mirror.store(m);
            self.published = m;
        }
    }

    pub(crate) fn region(&self) -> Region {
        Region {
            top: self.top,
            bottom: self.bottom,
            left: if self.lrmm { self.left } else { 0 },
            right: if self.lrmm { self.right } else { self.cols - 1 },
        }
    }

    fn inside_lr(&self) -> bool {
        !self.lrmm || (self.x >= self.left && self.x <= self.right)
    }

    pub(crate) fn home(&mut self) {
        self.y = if self.origin { self.top } else { 0 };
        self.x = if self.origin && self.lrmm {
            self.left
        } else {
            0
        };
        self.wrap = false;
    }

    pub(crate) fn cr(&mut self) {
        self.x = if self.lrmm && self.x >= self.left {
            self.left
        } else {
            0
        };
        self.wrap = false;
    }

    pub(crate) fn index(&mut self) {
        self.wrap = false;
        if self.y == self.bottom {
            if self.inside_lr() {
                let (r, bg, c) = (self.region(), self.pen.bg, self.cur);
                self.panes[c].grid.scroll_up(r, 1, bg);
            }
        } else if self.y + 1 < self.rows {
            self.y += 1;
        }
        // Rows a command block reaches carry its id (T-14 §1).
        if self.cur == 0 {
            let id = self.blocks.current_id();
            if id != 0 {
                let y = self.y;
                self.panes[0].grid.set_row_block(y, id);
            }
        }
    }

    pub(crate) fn reverse_index(&mut self) {
        self.wrap = false;
        if self.y == self.top {
            if self.inside_lr() {
                let (r, bg, c) = (self.region(), self.pen.bg, self.cur);
                self.panes[c].grid.scroll_down(r, 1, bg);
            }
        } else if self.y > 0 {
            self.y -= 1;
        }
    }

    pub(crate) fn tab_fwd(&mut self, n: usize) {
        let lim = if self.lrmm && self.x <= self.right {
            self.right
        } else {
            self.cols - 1
        };
        self.x = self.tabs.next(self.x, n).min(lim);
        self.wrap = false;
    }

    // ---- C0 ----------------------------------------------------------------------------

    pub(crate) fn exec(&mut self, c: u8) {
        match c {
            0x07 => self.events.bell = self.events.bell.saturating_add(1),
            0x08 => {
                self.cursor_moved();
                let lim = if self.lrmm && self.x >= self.left {
                    self.left
                } else {
                    0
                };
                self.wrap = false;
                if self.x > lim {
                    self.x -= 1;
                }
            }
            0x09 => {
                self.cursor_moved();
                self.tab_fwd(1);
            }
            0x0A..=0x0C => {
                self.cursor_moved();
                self.index();
                if self.lnm {
                    self.cr();
                }
            }
            0x0D => {
                self.cursor_moved();
                self.cr();
            }
            0x0E => self.gl = 1,
            0x0F => self.gl = 0,
            // ENQ: the answerback is empty (T-18 §2). CAN/SUB and the rest do nothing.
            _ => {}
        }
    }

    // ---- printing ----------------------------------------------------------------------

    fn limit(&self) -> usize {
        if self.lrmm && self.x <= self.right {
            self.right + 1
        } else {
            self.cols
        }
    }

    fn wrap_line(&mut self) {
        let c = self.cur;
        let y = self.y;
        self.panes[c].grid.set_row_flag(y, flags::WRAPPED, true);
        self.wrap = false;
        self.cr();
        self.index();
        let y2 = self.y;
        self.panes[c].grid.set_row_flag(y2, flags::WRAP_CONT, true);
    }

    /// Resolve a pending wrap and make room for `w` cells. False: nothing to print.
    fn pre_print(&mut self, w: usize) -> bool {
        if self.wrap {
            if self.autowrap {
                self.wrap_line();
            } else {
                self.wrap = false;
            }
        }
        let lim = self.limit();
        if self.x + w <= lim {
            return true;
        }
        if w == 2 && self.autowrap && self.x < lim {
            // Wide character with one cell left: blank it (the grid has no SpacerHead
            // writer) and wrap.
            let (c, y, x) = (self.cur, self.y, self.x);
            self.panes[c]
                .grid
                .erase_cells(y, x, lim, Color::Default, false);
            self.wrap_line();
            let lim2 = self.limit();
            return self.x + w <= lim2;
        }
        false
    }

    fn advance(&mut self, w: usize) {
        let lim = self.limit();
        self.x += w;
        if self.x >= lim {
            self.x = lim - 1;
            self.wrap = self.autowrap;
        }
    }

    pub(crate) fn print_text(&mut self, text: &str) {
        let b = text.as_bytes();
        let mut i = 0;
        while i < b.len() {
            let fast = self.autowrap
                && !self.irm
                && self.ss.is_none()
                && self.charsets[self.gl] == Charset::Ascii
                && (!self.seg.in_cluster() || self.last_ascii);
            if fast {
                let mut j = i;
                while j < b.len() && (0x20..0x7f).contains(&b[j]) {
                    j += 1;
                }
                if j > i {
                    self.print_ascii(&b[i..j]);
                    i = j;
                    continue;
                }
            }
            if text[i..].is_empty() {
                break;
            }
            i += self.print_chars(text, i);
        }
    }

    /// Print the characters of `text[i..]` the general way (segmenter, width, wrap), batching
    /// the cells that land on one row into a single grid call. Returns the bytes consumed (at
    /// least one character). Same result as calling [`Machine::print_char`] for each
    /// character; stops early only to let the ASCII fast path take a long run.
    fn print_chars(&mut self, text: &str, i: usize) -> usize {
        const BATCH: usize = 32;
        let rest = &text[i..];
        let plain = self.autowrap
            && !self.irm
            && self.ss.is_none()
            && self.charsets[self.gl] == Charset::Ascii;
        #[cfg(test)]
        let plain = plain && !self.no_batch;
        if !plain {
            let Some(c) = rest.chars().next() else {
                return 0;
            };
            self.print_char(c);
            return c.len_utf8();
        }
        let method = self.width_method();
        let lim = self.limit();
        let mut buf = [Glyph { cp: 0, width: 1 }; BATCH];
        let (mut n, mut bx, mut x_first) = (0usize, self.x, self.x);
        let mut tail = ('\0', 0u8);
        let mut used = 0;
        for (k, ch) in rest.char_indices() {
            let last_ascii = if n > 0 {
                tail.0.is_ascii()
            } else {
                self.last_ascii
            };
            if (0x20..0x7f).contains(&(ch as u32))
                && (!self.seg.in_cluster() || last_ascii)
                && rest.as_bytes()[k..]
                    .iter()
                    .take(4)
                    .filter(|b| (0x20..0x7f).contains(*b))
                    .count()
                    == 4
            {
                break;
            }
            used = k + ch.len_utf8();
            let mut new_cluster = self.seg.push(ch);
            if !new_cluster
                && method == WidthMethod::Wcswidth
                && self.cfg.widths.scalar_width(ch) > 0
            {
                self.seg.reset();
                new_cluster = self.seg.push(ch);
            }
            if !new_cluster {
                self.flush_glyphs(&buf[..n], x_first, tail);
                n = 0;
                self.extend_cluster(ch);
                (bx, x_first) = (self.x, self.x);
                if self.wrap {
                    break;
                }
                continue;
            }
            self.measure.reset();
            self.measure.push(&self.cfg.widths, ch);
            let w = self.measure.width(method).min(2) as usize;
            if w == 0 {
                continue;
            }
            let free = n > 0 || !self.wrap;
            if free && n < BATCH && bx + w <= lim {
                buf[n] = Glyph {
                    cp: ch as u32,
                    width: w as u8,
                };
                n += 1;
                bx += w;
                tail = (ch, w as u8);
                continue;
            }
            self.flush_glyphs(&buf[..n], x_first, tail);
            n = 0;
            self.place_glyph(ch, w, false);
            (bx, x_first) = (self.x, self.x);
            if self.wrap {
                break;
            }
        }
        self.flush_glyphs(&buf[..n], x_first, tail);
        if used == 0 {
            // Never stall: the character the caller chose the general path for is printed.
            if let Some(c) = rest.chars().next() {
                self.print_char(c);
                return c.len_utf8();
            }
        }
        used
    }

    /// Store a batch of pre-measured glyphs that fit the current row from column `x`, and
    /// update the cursor and last-character state as printing them one by one would.
    fn flush_glyphs(&mut self, glyphs: &[Glyph], x: usize, tail: (char, u8)) {
        if glyphs.is_empty() {
            return;
        }
        let (c, y) = (self.cur, self.y);
        let total: usize = glyphs.iter().map(|g| usize::from(g.width)).sum();
        self.gfx_text_over(y, x, x + total);
        let attrs = mk_attrs(self.pen, &self.link, self.protected, self.semantic);
        self.panes[c].grid.print_run(y, x, glyphs, &attrs);
        self.last = Some((y, x + total - usize::from(tail.1), tail.1));
        self.last_char = Some(tail.0);
        self.last_ascii = tail.0.is_ascii();
        self.advance(total);
    }

    fn print_ascii(&mut self, mut s: &[u8]) {
        while !s.is_empty() {
            if !self.pre_print(1) {
                return;
            }
            let lim = self.limit();
            let n = s.len().min(lim - self.x);
            let (c, y, x) = (self.cur, self.y, self.x);
            self.gfx_text_over(y, x, x + n);
            let attrs = mk_attrs(self.pen, &self.link, self.protected, self.semantic);
            self.panes[c].grid.print_ascii(y, x, &s[..n], &attrs);
            self.last = Some((y, x + n - 1, 1));
            self.last_char = Some(char::from(s[n - 1]));
            self.advance(n);
            s = &s[n..];
        }
        if let Some(ch) = self.last_char {
            self.seg.reset();
            self.seg.push(ch);
        }
        self.last_ascii = true;
    }

    pub(crate) fn print_char(&mut self, c: char) {
        let g = self.ss.take().unwrap_or(self.gl);
        let mut ch = c;
        let mut mapped = false;
        if (c as u32) < 0x80 {
            let m = self.charsets[g].map(c);
            if m != c {
                ch = m;
                mapped = true;
            }
        }
        if mapped {
            self.seg.reset();
        }
        let mut new_cluster = self.seg.push(ch);
        if !new_cluster
            && self.width_method() == WidthMethod::Wcswidth
            && self.cfg.widths.scalar_width(ch) > 0
        {
            // Legacy per-scalar widths: a visible scalar never joins the previous cell.
            self.seg.reset();
            new_cluster = self.seg.push(ch);
        }
        if !new_cluster {
            self.extend_cluster(ch);
            return;
        }
        self.measure.reset();
        self.measure.push(&self.cfg.widths, ch);
        let w = self.measure.width(self.width_method()).min(2) as usize;
        if w == 0 {
            return;
        }
        self.place_glyph(ch, w, mapped);
    }

    /// Print one measured glyph at the cursor: resolve a pending wrap, make room, store it.
    fn place_glyph(&mut self, ch: char, w: usize, mapped: bool) {
        if !self.pre_print(w) {
            self.last = None;
            return;
        }
        let (c, y, x) = (self.cur, self.y, self.x);
        self.gfx_text_over(y, x, x + w);
        if self.irm {
            let right = if self.lrmm && x <= self.right {
                self.right
            } else {
                self.cols - 1
            };
            let bg = self.pen.bg;
            self.panes[c].grid.insert_chars(y, x, w, right, bg);
        }
        let attrs = mk_attrs(self.pen, &self.link, self.protected, self.semantic);
        self.panes[c].grid.print_run(
            y,
            x,
            &[Glyph {
                cp: ch as u32,
                width: w as u8,
            }],
            &attrs,
        );
        self.last = Some((y, x, w as u8));
        self.last_char = Some(ch);
        self.last_ascii = !mapped && ch.is_ascii();
        self.advance(w);
    }

    fn extend_cluster(&mut self, ch: char) {
        if self.last_ascii {
            // The fast paths do not feed the measure; seed it with the base.
            if let Some(base) = self.last_char {
                self.measure.reset();
                self.measure.push(&self.cfg.widths, base);
            }
        }
        self.last_ascii = false;
        let Some((y, x, w)) = self.last else {
            return;
        };
        self.measure.push(&self.cfg.widths, ch);
        let nw = self.measure.width(self.width_method()).min(2);
        let c = self.cur;
        if !self.panes[c].grid.append_cp(y, x, ch as u32) {
            return;
        }
        if nw == 1 && w == 2 {
            // VS15 narrows an emoji-presentation cluster that is the last thing printed.
            let c = self.cur;
            let at_end = self.y == y && (self.x == x + 2 || (self.wrap && self.x == x + 1));
            if at_end {
                let mut cps = Vec::new();
                self.panes[c].grid.cell_text(y, x, &mut cps);
                let attrs = mk_attrs(self.pen, &self.link, self.protected, self.semantic);
                if self.panes[c].grid.print_cluster(y, x, &cps, 1, &attrs) {
                    self.last = Some((y, x, 1));
                    self.wrap = false;
                    self.x = x + 1;
                }
            }
            return;
        }
        if nw == 2 && w == 1 {
            let lim = self.limit();
            if self.x == x + 1 && !self.wrap && x + 2 <= lim && self.y == y {
                let mut cps = Vec::new();
                self.panes[c].grid.cell_text(y, x, &mut cps);
                let attrs = mk_attrs(self.pen, &self.link, self.protected, self.semantic);
                if self.panes[c].grid.print_cluster(y, x, &cps, 2, &attrs) {
                    self.last = Some((y, x, 2));
                    self.advance(1);
                }
            }
        }
    }

    // ---- ESC ---------------------------------------------------------------------------

    pub(crate) fn save_cursor(&mut self) {
        let s = Saved {
            y: self.y,
            x: self.x,
            wrap: self.wrap,
            pen: self.pen,
            protected: self.protected,
            origin: self.origin,
            charsets: self.charsets,
            gl: self.gl,
        };
        let c = self.cur;
        self.panes[c].saved = Some(s);
    }

    pub(crate) fn restore_cursor(&mut self) {
        let c = self.cur;
        match self.panes[c].saved.clone() {
            Some(s) => {
                self.y = s.y.min(self.rows - 1);
                self.x = s.x.min(self.cols - 1);
                self.wrap = s.wrap;
                self.pen = s.pen;
                self.protected = s.protected;
                self.origin = s.origin;
                self.charsets = s.charsets;
                self.gl = s.gl;
                // DECOM: the restored position is margin-relative (T-03 §3.1, VTCORE-24 CPR),
                // so margins set since the save clamp it.
                if self.origin {
                    self.y = self.y.clamp(self.top, self.bottom);
                    if self.lrmm {
                        self.x = self.x.clamp(self.left, self.right);
                    }
                }
            }
            None => {
                self.origin = false;
                self.home();
                self.pen = Style::DEFAULT;
                self.charsets = [Charset::Ascii; 4];
                self.gl = 0;
            }
        }
    }

    pub(crate) fn esc_dispatch(&mut self, inter: &[u8], fin: u8) {
        self.cursor_moved();
        self.last_char = None;
        match (inter, fin) {
            ([], b'7') => self.save_cursor(),
            ([], b'8') => self.restore_cursor(),
            ([], b'D') => self.index(),
            ([], b'E') => {
                self.cr();
                self.index();
            }
            ([], b'M') => self.reverse_index(),
            ([], b'H') => {
                let x = self.x;
                self.tabs.set(x);
            }
            ([], b'c') => self.ris(),
            ([], b'=') => self.set_mode(false, 66, true),
            ([], b'>') => self.set_mode(false, 66, false),
            ([], b'N') => self.ss = Some(2),
            ([], b'O') => self.ss = Some(3),
            ([], b'n') => self.gl = 2,
            ([], b'o') => self.gl = 3,
            ([b'#'], b'8') => self.decaln(),
            ([i @ (b'(' | b')' | b'*' | b'+')], f) => {
                let slot = match i {
                    b'(' => 0,
                    b')' => 1,
                    b'*' => 2,
                    _ => 3,
                };
                match f {
                    b'B' => self.charsets[slot] = Charset::Ascii,
                    b'0' => self.charsets[slot] = Charset::Dec,
                    b'A' => self.charsets[slot] = Charset::Uk,
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn decaln(&mut self) {
        self.top = 0;
        self.bottom = self.rows - 1;
        self.left = 0;
        self.right = self.cols - 1;
        let g = vec![Glyph { cp: 69, width: 1 }; self.cols];
        let attrs = CellAttrs::default();
        let c = self.cur;
        for y in 0..self.rows {
            self.panes[c].grid.print_run(y, 0, &g, &attrs);
        }
        self.home();
    }

    pub(crate) fn ris(&mut self) {
        let mut cfg = self.cfg.clone();
        cfg.rows = self.rows;
        cfg.cols = self.cols;
        let mut fresh = Machine::new(cfg, self.mirror.clone());
        fresh.replies = std::mem::take(&mut self.replies);
        fresh.events = std::mem::take(&mut self.events);
        fresh.now = self.now;
        fresh.sbuf = std::mem::take(&mut self.sbuf);
        // Command records survive RIS (their ids stay unique; their rows may survive in
        // history).
        fresh.blocks = std::mem::take(&mut self.blocks);
        fresh.gfx.carry_from(&self.gfx);
        *self = fresh;
    }

    pub(crate) fn soft_reset(&mut self) {
        self.pen = Style::DEFAULT;
        self.protected = false;
        self.link = None;
        self.charsets = [Charset::Ascii; 4];
        self.gl = 0;
        self.ss = None;
        for (num, ansi, on) in [
            (4u16, true, false),
            (6, false, false),
            (7, false, true),
            (25, false, true),
            (66, false, false),
            (1, false, false),
            (69, false, false),
        ] {
            self.set_mode(ansi, num, on);
        }
        self.top = 0;
        self.bottom = self.rows - 1;
        self.left = 0;
        self.right = self.cols - 1;
        self.panes[0].saved = None;
        self.panes[1].saved = None;
        self.wrap = false;
    }

    // ---- modes -------------------------------------------------------------------------

    pub(crate) fn mode_state(&self, ansi: bool, num: u16) -> ModeState {
        let Some(i) = modes::index(ansi, num) else {
            return ModeState::Unknown;
        };
        match modes::MODES[i].access {
            Access::Unrecognised => ModeState::Unknown,
            Access::ReadOnlySet => ModeState::PermSet,
            Access::ReadOnlyReset => ModeState::PermReset,
            Access::Settable => {
                if !ansi && num == 2027 && self.cfg.widths.method == WidthMethod::Unicode {
                    return ModeState::PermSet;
                }
                let on = if !ansi && num == 2026 {
                    self.sync.is_some()
                } else if !ansi && matches!(num, 47 | 1047 | 1049) {
                    self.cur == 1
                } else {
                    self.modes >> i & 1 == 1
                };
                if on { ModeState::Set } else { ModeState::Reset }
            }
        }
    }

    pub(crate) fn mode_on(&self, ansi: bool, num: u16) -> bool {
        modes::index(ansi, num).is_some_and(|i| self.modes >> i & 1 == 1)
    }

    pub(crate) fn set_mode(&mut self, ansi: bool, num: u16, on: bool) {
        let Some(i) = modes::index(ansi, num) else {
            return;
        };
        let def = modes::MODES[i];
        if def.access != Access::Settable {
            return;
        }
        if !ansi && num == 2027 && self.cfg.widths.method == WidthMethod::Unicode {
            return;
        }
        if on {
            self.modes |= 1 << i;
        } else {
            self.modes &= !(1 << i);
        }
        if def.input_bit != 0 {
            self.set_in(def.input_bit, on);
        }
        if !ansi && num == 2004 && on {
            // T-18 §4: 2004 counts as in force only once a prompt mark follows it.
            self.bp_prompted = false;
        }
        match (ansi, num) {
            (true, 4) => self.irm = on,
            (true, 20) => self.lnm = on,
            (false, 6) => {
                self.origin = on;
                self.home();
            }
            (false, 7) => self.autowrap = on,
            (false, 12) => self.cursor_style.blink = on,
            (false, 47) => {
                if on {
                    self.enter_alt(false);
                } else {
                    self.leave_alt(false);
                }
            }
            (false, 1047) => {
                if on {
                    self.enter_alt(false);
                } else {
                    self.leave_alt(true);
                }
            }
            (false, 1048) => {
                if on {
                    self.save_cursor();
                } else {
                    self.restore_cursor();
                }
            }
            (false, 1049) => {
                if on {
                    self.save_cursor();
                    self.enter_alt(true);
                } else {
                    self.leave_alt(false);
                    self.restore_cursor();
                }
            }
            (false, 69) => {
                self.lrmm = on;
                if !on {
                    self.left = 0;
                    self.right = self.cols - 1;
                }
            }
            (false, 2026) => {
                if on {
                    if self.sync.is_none() {
                        self.sync = self.now;
                    }
                } else {
                    self.sync = None;
                }
            }
            (false, 2027) => self.wide_mode = on,
            (false, 2048) => {
                if on {
                    self.size_report();
                }
            }
            (false, 5) => {
                let c = self.cur;
                self.panes[c].grid.mark_full();
            }
            _ => {}
        }
        if def.input_bit != 0 {
            self.publish();
        }
    }

    fn enter_alt(&mut self, clear: bool) {
        if self.cur == 1 {
            return;
        }
        self.cur = 1;
        self.gfx_clear_alt();
        if clear {
            let (rows, bg) = (self.rows, Color::Default);
            self.panes[1].grid.erase_rows(0, rows - 1, bg, false);
        }
        self.panes[1].grid.mark_full();
        self.kitty.switch_screen(true);
        self.set_in(bit::ALT_SCREEN, true);
        self.cursor_moved();
        self.publish();
    }

    fn leave_alt(&mut self, clear_first: bool) {
        if self.cur == 0 {
            return;
        }
        if clear_first {
            let rows = self.rows;
            self.panes[1]
                .grid
                .erase_rows(0, rows - 1, Color::Default, false);
        }
        self.cur = 0;
        self.gfx_clear_alt();
        self.panes[0].grid.mark_full();
        self.kitty.switch_screen(false);
        self.set_in(bit::ALT_SCREEN, false);
        self.cursor_moved();
        self.publish();
    }

    // ---- sizes -------------------------------------------------------------------------

    pub(crate) fn size_report(&mut self) {
        let (r, c) = (self.rows, self.cols);
        let (cw, ch) = self.cfg.cell_px;
        self.reply_str(format!(
            "\x1b[48;{};{};{};{}t",
            r,
            c,
            r as u32 * ch,
            c as u32 * cw
        ));
    }

    pub(crate) fn resize(&mut self, rows: usize, cols: usize) -> (usize, usize) {
        let cy = self.y;
        let r0 = self.panes[0].grid.resize_keeping(rows, cols, cy);
        let r1 = self.panes[1].grid.resize_keeping(rows, cols, cy);
        let res = if self.cur == 0 { r0 } else { r1 };
        let (nr, nc) = (res.rows, res.cols);
        if nr == self.rows && nc == self.cols {
            return (nr, nc);
        }
        self.rows = nr;
        self.cols = nc;
        self.y = res.cursor_row.min(nr - 1);
        self.x = self.x.min(nc - 1);
        self.wrap = false;
        self.top = 0;
        self.bottom = nr - 1;
        self.left = 0;
        self.right = nc - 1;
        self.tabs.resize(nc);
        for p in &mut self.panes {
            if let Some(s) = &mut p.saved {
                s.y = s.y.min(nr - 1);
                s.x = s.x.min(nc - 1);
            }
        }
        self.sync = None;
        self.cursor_moved();
        if self.mode_on(false, 2048) {
            self.size_report();
        }
        (nr, nc)
    }

    pub(crate) fn expire_sync(&mut self, now: Instant) {
        if let Some(s) = self.sync
            && now.duration_since(s) >= self.cfg.sync_timeout_clamped()
        {
            self.sync = None;
        }
    }
}

impl Sink for Machine {
    fn print_run(&mut self, text: &str) {
        self.print_text(text);
    }
    fn execute(&mut self, c0: u8) {
        self.exec(c0);
    }
    fn esc(&mut self, inter: &[u8], fin: u8) {
        self.esc_dispatch(inter, fin);
    }
    fn csi(&mut self, p: &Params, inter: &[u8], private: u8, fin: u8) {
        self.csi_dispatch(p, inter, private, fin);
    }
    fn string_start(&mut self, kind: StrKind, head: &StrHead) -> StrRoute {
        self.str_start(kind, head)
    }
    fn string_put(&mut self, chunk: &[u8]) {
        if self.strm == StrMode::Sixel {
            self.gfx_sixel_put(chunk);
        } else if self.strm != StrMode::None {
            self.sbuf.extend_from_slice(chunk);
        }
    }
    fn string_end(&mut self, end: StrEnd) {
        self.str_end(end);
    }
}
