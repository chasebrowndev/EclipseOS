// SPDX-License-Identifier: AGPL-3.0-only
//! The mux snapshot of a terminal (T-21 §3, ADR 0123): everything later output depends on,
//! so a view that restores it and then feeds the same bytes as the server ends in the same
//! state, by construction.
//!
//! A [`TermImage`] is the machine state (a versioned, little-endian record: cursor, pen,
//! margins, tab stops, charsets, modes, kitty keyboard stacks, segmenter carry, colours,
//! titles, user variables, command blocks, saved cursors, the two screens' counters) plus
//! each screen's pages as bytes ([`apoth_grid::Screen::from_image`]).
//!
//! A snapshot is taken only between sequences ([`Term::at_ground`]): parser, string and
//! held-byte state are then empty, so they need no encoding. Not carried yet (M35 scope,
//! reported): images and media placements (T-21 §3 "images by reference"); the server
//! denies those classes, so neither side ever holds any. Transient outputs (replies,
//! events) are not state. Instants are carried as ages relative to the `now` of each side.
//!
//! [`Term::restore`] reads data from another process: every length, index and string is
//! bounded and checked, and each page goes through `Page::from_bytes`.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use apoth_grid::{Limits, PageSpec, Screen, ScreenMeta, Semantic, Style, TabStops};
use apoth_input::{InputModes, KittyFlagStack, KittyState};
use apoth_unicode::{ClusterMeasure, Segmenter};

use crate::config::{CursorStyle, Rgb, TermConfig};
use crate::machine::{Charset, Saved};
use crate::shell::{BLOCKS_CAP, Block, BlockState, Blocks, CMDLINE_MAX};
use crate::{Term, USER_VAR_NAME_MAX, USER_VAR_VALUE_MAX, USER_VARS_MAX};

/// Snapshot format version. A different version is refused, never guessed.
pub const SNAPSHOT_VERSION: u16 = 1;
const MAGIC: &[u8; 4] = b"APTS";
/// Longest title, icon name or working-directory URI carried.
pub const SNAP_STR_MAX: usize = 64 * 1024;
/// Longest machine-state record accepted (everything but the pages).
pub const SNAP_STATE_MAX: usize = 4 << 20;
/// Most pages per screen accepted.
pub const SNAP_PAGES_MAX: usize = 1 << 16;
/// Deepest title stack carried.
const TITLE_STACK_MAX: usize = 64;

/// A terminal as data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TermImage {
    /// The machine-state record.
    pub state: Vec<u8>,
    /// Pages of the primary and alternate screens, oldest first.
    pub pages: [Vec<Vec<u8>>; 2],
}

impl TermImage {
    /// Bytes in the image.
    #[must_use]
    pub fn len(&self) -> usize {
        self.state.len() + self.pages.iter().flatten().map(Vec::len).sum::<usize>()
    }

    /// Never true for a real image.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Why a snapshot could not be taken or restored. Carries no content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapError {
    /// A sequence, string or held output is in flight: take it at the next ground point.
    NotIdle,
    /// Another format version.
    Version(u16),
    /// The record is truncated, oversized or inconsistent.
    Corrupt,
    /// A screen image was refused.
    Screen(apoth_grid::ImageError),
}

impl std::fmt::Display for SnapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SnapError::NotIdle => f.write_str("the terminal is inside a sequence"),
            SnapError::Version(v) => write!(f, "snapshot version {v} is not supported"),
            SnapError::Corrupt => f.write_str("the snapshot is corrupt"),
            SnapError::Screen(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for SnapError {}

// ---- encoding ---------------------------------------------------------------------------

struct W(Vec<u8>);

impl W {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn bool(&mut self, v: bool) {
        self.0.push(u8::from(v));
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn usize(&mut self, v: usize) {
        self.u64(v as u64);
    }
    fn bytes(&mut self, b: &[u8]) {
        self.u32(b.len() as u32);
        self.0.extend_from_slice(b);
    }
    fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }
    fn opt_str(&mut self, s: Option<&str>) {
        self.bool(s.is_some());
        if let Some(s) = s {
            self.str(s);
        }
    }
    fn rgb(&mut self, c: Rgb) {
        self.0.extend_from_slice(&[c.0, c.1, c.2]);
    }
    fn age(&mut self, t: Option<Instant>, now: Instant) {
        self.bool(t.is_some());
        if let Some(t) = t {
            let ms = now.saturating_duration_since(t).as_millis();
            self.u64(u64::try_from(ms).unwrap_or(u64::MAX));
        }
    }
    fn pin(&mut self, p: Option<apoth_grid::Pin>) {
        self.bool(p.is_some());
        if let Some(p) = p {
            self.u64(p.serial);
            self.u16(p.slot);
            self.u16(p.col);
            self.u16(p.wrap_width);
        }
    }
}

struct R<'a> {
    b: &'a [u8],
}

type Res<T> = Result<T, SnapError>;

impl R<'_> {
    fn take(&mut self, n: usize) -> Res<&[u8]> {
        if self.b.len() < n {
            return Err(SnapError::Corrupt);
        }
        let (a, rest) = self.b.split_at(n);
        self.b = rest;
        Ok(a)
    }
    fn u8(&mut self) -> Res<u8> {
        Ok(self.take(1)?[0])
    }
    fn bool(&mut self) -> Res<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(SnapError::Corrupt),
        }
    }
    fn u16(&mut self) -> Res<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Res<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Res<u64> {
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_le_bytes(a))
    }
    /// A size or index, at most `max`.
    fn usize(&mut self, max: usize) -> Res<usize> {
        let v = usize::try_from(self.u64()?).map_err(|_| SnapError::Corrupt)?;
        if v > max {
            return Err(SnapError::Corrupt);
        }
        Ok(v)
    }
    fn bytes(&mut self, max: usize) -> Res<&[u8]> {
        let n = self.u32()? as usize;
        if n > max {
            return Err(SnapError::Corrupt);
        }
        self.take(n)
    }
    fn str(&mut self, max: usize) -> Res<String> {
        String::from_utf8(self.bytes(max)?.to_vec()).map_err(|_| SnapError::Corrupt)
    }
    fn opt_str(&mut self, max: usize) -> Res<Option<String>> {
        Ok(if self.bool()? {
            Some(self.str(max)?)
        } else {
            None
        })
    }
    fn rgb(&mut self) -> Res<Rgb> {
        let b = self.take(3)?;
        Ok((b[0], b[1], b[2]))
    }
    fn age(&mut self, now: Instant) -> Res<Option<Instant>> {
        if !self.bool()? {
            return Ok(None);
        }
        let ms = self.u64()?;
        Ok(Some(
            now.checked_sub(Duration::from_millis(ms)).unwrap_or(now),
        ))
    }
    fn pin(&mut self) -> Res<Option<apoth_grid::Pin>> {
        if !self.bool()? {
            return Ok(None);
        }
        Ok(Some(apoth_grid::Pin {
            serial: self.u64()?,
            slot: self.u16()?,
            col: self.u16()?,
            wrap_width: self.u16()?,
        }))
    }
}

fn charset_byte(c: Charset) -> u8 {
    match c {
        Charset::Ascii => 0,
        Charset::Dec => 1,
        Charset::Uk => 2,
    }
}

fn charset_of(b: u8) -> Res<Charset> {
    Ok(match b {
        0 => Charset::Ascii,
        1 => Charset::Dec,
        2 => Charset::Uk,
        _ => return Err(SnapError::Corrupt),
    })
}

fn semantic_of(b: u8) -> Res<Semantic> {
    Ok(match b {
        0 => Semantic::Output,
        1 => Semantic::Input,
        2 => Semantic::Prompt,
        _ => return Err(SnapError::Corrupt),
    })
}

fn block_state_byte(s: BlockState) -> u8 {
    match s {
        BlockState::Prompt => 0,
        BlockState::Input => 1,
        BlockState::Running => 2,
        BlockState::Done => 3,
    }
}

fn block_state_of(b: u8) -> Res<BlockState> {
    Ok(match b {
        0 => BlockState::Prompt,
        1 => BlockState::Input,
        2 => BlockState::Running,
        3 => BlockState::Done,
        _ => return Err(SnapError::Corrupt),
    })
}

fn put_meta(w: &mut W, m: &ScreenMeta, pages: usize) {
    w.usize(m.rows);
    w.usize(m.cols);
    w.bool(m.history);
    w.usize(m.limits.bytes);
    w.bool(m.limits.lines.is_some());
    w.usize(m.limits.lines.unwrap_or(0));
    let s = &m.spec;
    for v in [
        s.cols,
        s.rows_cap,
        s.style_cap,
        s.chunk_cap,
        s.link_cap,
        s.lmap_cap,
        s.str_cap,
    ] {
        w.usize(v);
    }
    w.u64(m.seq);
    w.u64(m.next_serial);
    w.usize(pages);
}

fn get_meta(r: &mut R<'_>) -> Res<(ScreenMeta, usize)> {
    const BIG: usize = u32::MAX as usize;
    let rows = r.usize(u16::MAX as usize)?;
    let cols = r.usize(u16::MAX as usize)?;
    let history = r.bool()?;
    let bytes = r.usize(usize::MAX)?;
    let has_lines = r.bool()?;
    let lines = r.usize(usize::MAX)?;
    let mut f = [0usize; 7];
    for v in &mut f {
        *v = r.usize(BIG)?;
    }
    let spec = PageSpec {
        cols: f[0],
        rows_cap: f[1],
        style_cap: f[2],
        chunk_cap: f[3],
        link_cap: f[4],
        lmap_cap: f[5],
        str_cap: f[6],
    };
    let meta = ScreenMeta {
        rows,
        cols,
        history,
        limits: Limits {
            bytes,
            lines: has_lines.then_some(lines),
        },
        spec,
        seq: r.u64()?,
        next_serial: r.u64()?,
    };
    let pages = r.usize(SNAP_PAGES_MAX)?;
    Ok((meta, pages))
}

fn put_saved(w: &mut W, s: Option<&Saved>) {
    w.bool(s.is_some());
    let Some(s) = s else { return };
    w.usize(s.y);
    w.usize(s.x);
    w.bool(s.wrap);
    w.0.extend_from_slice(&s.pen.to_bytes());
    w.bool(s.protected);
    w.bool(s.origin);
    for c in s.charsets {
        w.u8(charset_byte(c));
    }
    w.u8(s.gl as u8);
}

fn get_saved(r: &mut R<'_>, rows: usize, cols: usize) -> Res<Option<Saved>> {
    if !r.bool()? {
        return Ok(None);
    }
    let y = r.usize(rows - 1)?;
    let x = r.usize(cols - 1)?;
    let wrap = r.bool()?;
    let pen = style(r)?;
    let protected = r.bool()?;
    let origin = r.bool()?;
    let mut charsets = [Charset::Ascii; 4];
    for c in &mut charsets {
        *c = charset_of(r.u8()?)?;
    }
    let gl = usize::from(r.u8()?);
    if gl > 3 {
        return Err(SnapError::Corrupt);
    }
    Ok(Some(Saved {
        y,
        x,
        wrap,
        pen,
        protected,
        origin,
        charsets,
        gl,
    }))
}

fn style(r: &mut R<'_>) -> Res<Style> {
    let mut b = [0u8; 16];
    b.copy_from_slice(r.take(16)?);
    Ok(Style::from_bytes(&b))
}

impl Term {
    /// No sequence, string, partial code point or held output is in flight: a snapshot
    /// taken now needs no parser state (T-21 §3).
    #[must_use]
    pub fn at_ground(&self) -> bool {
        self.parser.is_idle()
            && self.held.is_empty()
            && self.m.strm == crate::machine::StrMode::None
            && !self.m.gfx.blocked()
    }

    /// The terminal as data. `now` is the caller's clock (ages of timed state are relative
    /// to it). [`SnapError::NotIdle`] between the bytes of a sequence.
    pub fn snapshot(&self, now: Instant) -> Result<TermImage, SnapError> {
        if !self.at_ground() {
            return Err(SnapError::NotIdle);
        }
        let m = &self.m;
        let mut w = W(Vec::with_capacity(4096));
        w.0.extend_from_slice(MAGIC);
        w.u16(SNAPSHOT_VERSION);
        w.usize(m.rows);
        w.usize(m.cols);
        w.u8(m.cur as u8);
        w.usize(m.y);
        w.usize(m.x);
        w.bool(m.wrap);
        w.0.extend_from_slice(&m.pen.to_bytes());
        w.bool(m.protected);
        w.u8(m.semantic as u8);
        w.bool(m.link.is_some());
        if let Some((id, uri)) = &m.link {
            w.str(id);
            w.str(uri);
        }
        for v in [m.top, m.bottom, m.left, m.right] {
            w.usize(v);
        }
        let mut tabs = vec![0u8; m.cols.div_ceil(8)];
        for x in 0..m.cols {
            if m.tabs.is_set(x) {
                tabs[x / 8] |= 1 << (x % 8);
            }
        }
        w.bytes(&tabs);
        for c in m.charsets {
            w.u8(charset_byte(c));
        }
        w.u8(m.gl as u8);
        w.bool(m.ss.is_some());
        w.u8(m.ss.unwrap_or(0) as u8);
        w.0.extend_from_slice(&m.modes.to_le_bytes());
        for b in [m.autowrap, m.irm, m.lnm, m.origin, m.lrmm, m.wide_mode] {
            w.bool(b);
        }
        w.u8(m.cursor_style.shape);
        w.bool(m.cursor_style.blink);
        w.u8(m.seg.to_raw());
        w.0.extend_from_slice(&m.measure.to_raw());
        w.bool(m.last.is_some());
        let (ly, lx, lw) = m.last.unwrap_or((0, 0, 0));
        w.usize(ly);
        w.usize(lx);
        w.u8(lw);
        w.bool(m.last_ascii);
        w.bool(m.last_char.is_some());
        w.u32(m.last_char.map_or(0, u32::from));
        let (bits, kitty) = m.published.raw();
        w.u64(bits);
        w.u32(kitty);
        w.u64(m.in_bits);
        let (km, ka, kalt) = m.kitty.parts();
        w.bytes(km.entries());
        w.bytes(ka.entries());
        w.bool(kalt);
        w.u32(m.user_vars.len() as u32);
        for (k, v) in &m.user_vars {
            w.str(k);
            w.str(v);
        }
        for c in m.palette.iter() {
            w.rgb(*c);
        }
        for c in [
            m.fg,
            m.bg,
            m.cursor_color,
            m.cfg.fg,
            m.cfg.bg,
            m.cfg.cursor_color,
        ] {
            w.rgb(c);
        }
        for c in m.cfg.ansi {
            w.rgb(c);
        }
        w.bool(m.cfg.dark);
        w.u32(m.cfg.deny);
        w.str(&m.title);
        w.str(&m.icon);
        w.u32(m.title_stack.len() as u32);
        for (t, i) in &m.title_stack {
            w.str(t);
            w.str(i);
        }
        w.age(m.sync, now);
        // Command blocks (T-14 §1). Command lines stay in memory: this record crosses a
        // socket to a view of the same user, never a file or a log.
        let (next_id, evicted) = m.blocks.counters();
        w.u32(next_id);
        w.u64(evicted);
        w.u32(m.blocks.len() as u32);
        for b in m.blocks.iter() {
            w.u32(b.id);
            w.u8(block_state_byte(b.state));
            for p in [b.prompt, b.input, b.output, b.end] {
                w.pin(p);
            }
            w.bool(b.exit.is_some());
            w.u32(b.exit.unwrap_or(0) as u32);
            w.age(b.started, now);
            w.age(b.ended, now);
            w.opt_str(b.cwd.as_deref());
            w.opt_str(b.cmdline.as_deref());
        }
        w.bool(m.bp_prompted);
        w.opt_str(m.cwd_uri.as_deref());
        let mut pages: [Vec<Vec<u8>>; 2] = [Vec::new(), Vec::new()];
        for (i, p) in m.panes.iter().enumerate() {
            put_saved(&mut w, p.saved.as_ref());
            let meta = p.grid.image_meta();
            put_meta(&mut w, &meta, p.grid.image_pages().len());
            pages[i] = p
                .grid
                .image_pages()
                .map(|pg| pg.as_bytes().to_vec())
                .collect();
        }
        Ok(TermImage { state: w.0, pages })
    }

    /// A terminal from [`Term::snapshot`] data. `cfg` supplies what is local to this side
    /// (widths, limits, version, cell size); the image supplies all screen state, including
    /// the size, colours in force and the sequence policy word the server feeds under.
    pub fn restore(cfg: TermConfig, image: TermImage, now: Instant) -> Result<Term, SnapError> {
        if image.state.len() > SNAP_STATE_MAX {
            return Err(SnapError::Corrupt);
        }
        let mut r = R { b: &image.state };
        if r.take(4)? != MAGIC {
            return Err(SnapError::Corrupt);
        }
        let v = r.u16()?;
        if v != SNAPSHOT_VERSION {
            return Err(SnapError::Version(v));
        }
        let rows = r.usize(u16::MAX as usize)?;
        let cols = r.usize(u16::MAX as usize)?;
        if rows == 0 || cols == 0 {
            return Err(SnapError::Corrupt);
        }
        let mut t = Term::new(TermConfig {
            rows: 1,
            cols: 1,
            ..cfg
        });
        let m = &mut t.m;
        m.rows = rows;
        m.cols = cols;
        m.cur = usize::from(r.u8()?);
        if m.cur > 1 {
            return Err(SnapError::Corrupt);
        }
        m.y = r.usize(rows - 1)?;
        m.x = r.usize(cols - 1)?;
        m.wrap = r.bool()?;
        m.pen = style(&mut r)?;
        m.protected = r.bool()?;
        m.semantic = semantic_of(r.u8()?)?;
        m.link = if r.bool()? {
            let id = r.str(apoth_grid::MAX_LINK_ID)?;
            let uri = r.str(apoth_grid::MAX_URI)?;
            Some((id, uri))
        } else {
            None
        };
        m.top = r.usize(rows - 1)?;
        m.bottom = r.usize(rows - 1)?;
        m.left = r.usize(cols - 1)?;
        m.right = r.usize(cols - 1)?;
        if m.top > m.bottom || m.left > m.right {
            return Err(SnapError::Corrupt);
        }
        let tabs = r.bytes(cols.div_ceil(8))?;
        if tabs.len() != cols.div_ceil(8) {
            return Err(SnapError::Corrupt);
        }
        m.tabs = TabStops::new(cols);
        m.tabs.clear_all();
        for x in 0..cols {
            if tabs[x / 8] & (1 << (x % 8)) != 0 {
                m.tabs.set(x);
            }
        }
        for c in &mut m.charsets {
            *c = charset_of(r.u8()?)?;
        }
        m.gl = usize::from(r.u8()?);
        let has_ss = r.bool()?;
        let ss = usize::from(r.u8()?);
        if m.gl > 3 || ss > 3 {
            return Err(SnapError::Corrupt);
        }
        m.ss = has_ss.then_some(ss);
        let mut mb = [0u8; 16];
        mb.copy_from_slice(r.take(16)?);
        m.modes = u128::from_le_bytes(mb);
        m.autowrap = r.bool()?;
        m.irm = r.bool()?;
        m.lnm = r.bool()?;
        m.origin = r.bool()?;
        m.lrmm = r.bool()?;
        m.wide_mode = r.bool()?;
        m.cursor_style = CursorStyle {
            shape: r.u8()?,
            blink: r.bool()?,
        };
        if m.cursor_style.shape > 2 {
            return Err(SnapError::Corrupt);
        }
        m.seg = Segmenter::from_raw(r.u8()?).ok_or(SnapError::Corrupt)?;
        let mut mr = [0u8; 5];
        mr.copy_from_slice(r.take(5)?);
        m.measure = ClusterMeasure::from_raw(mr).ok_or(SnapError::Corrupt)?;
        let has_last = r.bool()?;
        let ly = r.usize(rows - 1)?;
        let lx = r.usize(cols - 1)?;
        let lw = r.u8()?;
        m.last = has_last.then_some((ly, lx, lw));
        m.last_ascii = r.bool()?;
        let has_char = r.bool()?;
        let ch = r.u32()?;
        m.last_char = if has_char {
            Some(char::from_u32(ch).ok_or(SnapError::Corrupt)?)
        } else {
            None
        };
        let bits = r.u64()?;
        let kitty = r.u32()?;
        m.published = InputModes::from_raw(bits, kitty);
        m.in_bits = r.u64()?;
        let km = KittyFlagStack::from_entries(r.bytes(64)?).ok_or(SnapError::Corrupt)?;
        let ka = KittyFlagStack::from_entries(r.bytes(64)?).ok_or(SnapError::Corrupt)?;
        m.kitty = KittyState::from_parts(km, ka, r.bool()?);
        let nvars = r.u32()? as usize;
        if nvars > USER_VARS_MAX {
            return Err(SnapError::Corrupt);
        }
        for _ in 0..nvars {
            let k = r.str(USER_VAR_NAME_MAX)?;
            let v = r.str(USER_VAR_VALUE_MAX)?;
            m.user_vars.insert(k, v);
        }
        for c in m.palette.iter_mut() {
            *c = r.rgb()?;
        }
        m.fg = r.rgb()?;
        m.bg = r.rgb()?;
        m.cursor_color = r.rgb()?;
        m.cfg.fg = r.rgb()?;
        m.cfg.bg = r.rgb()?;
        m.cfg.cursor_color = r.rgb()?;
        for c in &mut m.cfg.ansi {
            *c = r.rgb()?;
        }
        m.cfg.dark = r.bool()?;
        m.cfg.deny = r.u32()?;
        m.cfg.rows = rows;
        m.cfg.cols = cols;
        m.title = r.str(SNAP_STR_MAX)?;
        m.icon = r.str(SNAP_STR_MAX)?;
        let depth = r.u32()? as usize;
        if depth > TITLE_STACK_MAX {
            return Err(SnapError::Corrupt);
        }
        for _ in 0..depth {
            let a = r.str(SNAP_STR_MAX)?;
            let b = r.str(SNAP_STR_MAX)?;
            m.title_stack.push((a, b));
        }
        m.sync = r.age(now)?;
        let next_id = r.u32()?;
        let evicted = r.u64()?;
        let n = r.u32()? as usize;
        if n > BLOCKS_CAP || next_id == 0 {
            return Err(SnapError::Corrupt);
        }
        let mut list = VecDeque::with_capacity(n);
        for _ in 0..n {
            let id = r.u32()?;
            if id == 0 || id >= next_id {
                return Err(SnapError::Corrupt);
            }
            let state = block_state_of(r.u8()?)?;
            let (prompt, input, output, end) = (r.pin()?, r.pin()?, r.pin()?, r.pin()?);
            let has_exit = r.bool()?;
            let exit = r.u32()? as i32;
            list.push_back(Block {
                id,
                state,
                prompt,
                input,
                output,
                end,
                exit: has_exit.then_some(exit),
                started: r.age(now)?,
                ended: r.age(now)?,
                cwd: r.opt_str(SNAP_STR_MAX)?,
                cmdline: r.opt_str(CMDLINE_MAX)?,
            });
        }
        m.blocks = Blocks::from_parts(list, next_id, evicted);
        m.bp_prompted = r.bool()?;
        m.cwd_uri = r.opt_str(SNAP_STR_MAX)?;
        let [p0, p1] = image.pages;
        for (i, pages) in [p0, p1].into_iter().enumerate() {
            let saved = get_saved(&mut r, rows, cols)?;
            let (meta, count) = get_meta(&mut r)?;
            if count != pages.len() || meta.rows != rows || meta.cols != cols {
                return Err(SnapError::Corrupt);
            }
            if meta.history != (i == 0) {
                return Err(SnapError::Corrupt);
            }
            m.panes[i].grid = Screen::from_image(meta, pages).map_err(SnapError::Screen)?;
            m.panes[i].saved = saved;
        }
        if !r.b.is_empty() {
            return Err(SnapError::Corrupt);
        }
        m.mirror.store(m.published);
        m.panes[m.cur].grid.mark_full();
        Ok(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(rows: usize, cols: usize) -> Term {
        Term::new(TermConfig {
            rows,
            cols,
            ..TermConfig::default()
        })
    }

    /// Everything observable: the snapshot record (which encodes every carried field) and
    /// each screen's cells, styles, links and row flags, read through the grid's API. Page
    /// bytes are not compared raw: a recycled page buffer may hold stale bytes in rows it
    /// has not opened yet, which no reader sees.
    type Rows = Vec<(
        Vec<u32>,
        Vec<Option<Style>>,
        Option<u16>,
        Vec<Option<(String, String)>>,
    )>;

    fn state(t: &Term, now: Instant) -> (Vec<u8>, [Rows; 2]) {
        let img = t.snapshot(now).expect("idle");
        let screen = |g: &Screen| -> Rows {
            let mut out = Vec::new();
            for abs in 0..g.history_rows() + g.rows() {
                let (mut text, mut styles, mut links) = (Vec::new(), Vec::new(), Vec::new());
                for x in 0..g.cols() {
                    styles.push(g.cell_abs(abs, x, &mut text));
                    text.push(0);
                    links.push(g.link_at_abs(abs, x));
                }
                out.push((text, styles, g.row_flags_abs(abs), links));
            }
            out
        };
        (img.state, [screen(t.primary()), screen(t.alternate())])
    }

    const CORPUS: &[&[u8]] = &[
        b"hello \x1b[1;31mred\x1b[0m world\r\n",
        b"\x1b]0;a title\x07\x1b]8;;https://example.org/\x07link\x1b]8;;\x07\r\n",
        b"\x1b[?1049h\x1b[2J\x1b[5;10Halt screen\x1b[?1049l",
        b"\x1b[3;20r\x1b[?6h\x1b[2;3Horigin\x1b[?6l\x1b[r",
        b"\x1b(0lqqk\x1b(B \x1b[3g\x1bH\t*\r\n",
        b"e\xcc\x81 \xe4\xb8\xad \xf0\x9f\x91\x8d\xf0\x9f\x8f\xbd\r\n",
        b"\x1b]133;A\x07$ \x1b]133;B\x07ls\x1b]133;C\x07out\r\n\x1b]133;D;0\x07",
        b"\x1b[>1u\x1b[?2004h\x1b[?1000h\x1b7\x1b[10;10H\x1b8",
        b"\x1b]1337;SetUserVar=k=dg==\x07\x1b]4;1;rgb:12/34/56\x07\x1b[22t",
        b"\x1b[4:3m\x1b[58;2;1;2;3mcurly\x1b[m\x1b#8",
    ];

    fn fed(rows: usize, cols: usize, n: usize, now: Instant) -> Term {
        let mut t = term(rows, cols);
        for i in 0..n {
            t.feed(CORPUS[i % CORPUS.len()], now);
            if i % 7 == 6 {
                t.feed(&b"line\r\n".repeat(30), now);
            }
        }
        t
    }

    #[test]
    fn snapshot_round_trips() {
        let now = Instant::now();
        for n in [0, 3, 10, 40] {
            let t = fed(12, 40, n, now);
            let img = t.snapshot(now).unwrap();
            let u = Term::restore(TermConfig::default(), img.clone(), now).unwrap();
            assert_eq!(
                u.snapshot(now).unwrap(),
                img,
                "after {n} chunks: byte for byte"
            );
            assert_eq!(state(&u, now), state(&t, now));
            assert_eq!(u.title(), t.title());
            assert_eq!(u.cursor(), t.cursor());
            assert_eq!(u.on_alternate(), t.on_alternate());
            assert_eq!(u.kitty_flags(), t.kitty_flags());
            assert_eq!(u.input_mirror().load(), t.input_mirror().load());
            for y in 0..t.rows() {
                assert_eq!(u.row_text(y), t.row_text(y));
            }
        }
    }

    /// The tee rule (ADR 0123): restore anywhere, feed the same bytes to both, and the
    /// states are byte-identical.
    #[test]
    fn tee_after_restore_is_identical() {
        let now = Instant::now();
        let all: Vec<u8> = CORPUS.iter().flat_map(|c| c.iter().copied()).collect();
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        for _ in 0..40 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let mut server = term(10, 30);
            let feed_both = |s: &mut Term, bytes: &[u8]| {
                for chunk in bytes.chunks(1 + (seed as usize % 13)) {
                    s.feed(chunk, now);
                }
            };
            let cut = seed as usize % (all.len() * 3);
            let corpus = all.repeat(4);
            feed_both(&mut server, &corpus[..cut]);
            // Advance to a ground point, as the server does before it snapshots.
            let mut at = cut;
            while !server.at_ground() && at < corpus.len() {
                server.feed(&corpus[at..at + 1], now);
                at += 1;
            }
            let mut view =
                Term::restore(TermConfig::default(), server.snapshot(now).unwrap(), now).unwrap();
            feed_both(&mut server, &corpus[at..]);
            feed_both(&mut view, &corpus[at..]);
            server.resize(7, 22);
            view.resize(7, 22);
            feed_both(&mut server, CORPUS[0]);
            feed_both(&mut view, CORPUS[0]);
            assert_eq!(state(&view, now), state(&server, now), "cut at {cut}");
        }
    }

    #[test]
    fn mid_sequence_is_not_idle() {
        let now = Instant::now();
        let mut t = term(5, 10);
        t.feed(b"\x1b]0;half", now);
        assert_eq!(t.snapshot(now).err(), Some(SnapError::NotIdle));
        t.feed(b"\x07", now);
        assert!(t.snapshot(now).is_ok());
        t.feed(b"\x1b[", now);
        assert!(!t.at_ground());
        t.feed(b"m\xe4", now);
        assert!(!t.at_ground(), "a partial code point is in flight");
        t.feed(b"\xb8\xad", now);
        assert!(t.at_ground());
    }

    #[test]
    fn foreign_versions_and_corruption_are_refused() {
        let now = Instant::now();
        let img = fed(6, 20, 12, now).snapshot(now).unwrap();
        let mut v = img.clone();
        v.state[4] = 9;
        assert_eq!(
            Term::restore(TermConfig::default(), v, now).err(),
            Some(SnapError::Version(9))
        );
        let mut short = img.clone();
        short.state.truncate(short.state.len() - 1);
        assert!(Term::restore(TermConfig::default(), short, now).is_err());
        let mut long = img.clone();
        long.state.push(0);
        assert!(Term::restore(TermConfig::default(), long, now).is_err());
        let mut missing = img.clone();
        missing.pages[0].pop();
        assert!(Term::restore(TermConfig::default(), missing, now).is_err());
        // Any single flipped byte of the record is refused or restores a usable term.
        for i in 6..img.state.len() {
            let mut c = img.clone();
            c.state[i] ^= 0xa5;
            if let Ok(mut t) = Term::restore(TermConfig::default(), c, now) {
                t.feed(
                    b"\x1b[2J\x1b[Hok\r\n\x1b[10;10Hx\x1b[?1049h\x1b[r\x1b[L\x1b[M",
                    now,
                );
                t.resize(3, 4);
                t.feed(b"after", now);
            }
        }
    }
}
