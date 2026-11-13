// SPDX-License-Identifier: AGPL-3.0-only
//! The screen: an ordered list of pages, the active area at its tail (T-02 §1, §3, §4).
//!
//! Coordinates in this API are *active-area* coordinates `(y, x)` with `y = 0` the top
//! visible row. Nothing here stores raw row indices across calls: durable references are
//! tracked pins ([`crate::pin`]).
//!
//! Scroll direction convention used everywhere (including `Damage::Scroll::delta`):
//! `delta > 0` means content moved *up* by `delta` rows (row `y + delta` became row `y`),
//! `delta < 0` means content moved down.

use std::sync::atomic::{AtomicU64, Ordering};

mod reflow;
pub use reflow::{ReflowOpts, Reflowed};

use crate::cell::{Cell, Semantic, Tag, Wide};
use crate::page::{
    Ceil, Full, LinkError, MAX_CLUSTER_CPS, PAGE_BYTES, Page, PageSpec, ROW_HEADER_BYTES,
    row_flags as flags,
};
use crate::pin::{Pin, PinId, PinKind, PinState, PinTable};
use crate::style::{Color, Style};

static NEXT_SCREEN_ID: AtomicU64 = AtomicU64::new(1);

/// Most cells (`rows x cols`) one screen may hold in its active area. Larger requests are
/// clamped by reducing the rows (T-18 §5: every allocation a hostile resize can drive has a
/// budget). 4 Mi cells is 32 MiB of cells.
pub const MAX_CELLS: usize = 1 << 22;

/// Free page buffers kept for reuse after eviction.
const POOL_KEEP: usize = 2;

/// Recycled page buffers (zeroed on release, so a fresh page never sees old content).
#[derive(Debug, Default)]
struct PagePool {
    free: Vec<Vec<u8>>,
    hits: u64,
    misses: u64,
}

impl PagePool {
    fn acquire(&mut self, spec: PageSpec, serial: u64) -> Page {
        let want = spec.total_bytes();
        if let Some(i) = self.free.iter().position(|b| b.len() == want) {
            self.hits += 1;
            let buf = self.free.swap_remove(i);
            return Page::with_buffer(spec, serial, buf);
        }
        self.misses += 1;
        Page::new(spec, serial)
    }

    fn release(&mut self, page: Page, standard_len: usize) {
        if page.spec().total_bytes() == standard_len && self.free.len() < POOL_KEEP {
            self.free.push(page.recycle());
        }
    }
}

/// Scrollback limits (T-02 §4). Whichever is hit first evicts whole head pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Logical bytes per pane, counted as page size x page count (includes the active area).
    pub bytes: usize,
    /// History lines kept; `None` = unlimited (the byte limit still applies).
    pub lines: Option<usize>,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            bytes: 64 << 20,
            lines: None,
        }
    }
}

/// Screen construction parameters.
#[derive(Clone, Copy, Debug)]
pub struct ScreenConfig {
    /// Primary screen (true) keeps history; alternate screen (false) is a fixed page set.
    pub history: bool,
    /// Initial limits.
    pub limits: Limits,
}

impl Default for ScreenConfig {
    fn default() -> Self {
        ScreenConfig {
            history: true,
            limits: Limits::default(),
        }
    }
}

/// A scroll region in active-area coordinates, all bounds inclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    /// First row.
    pub top: usize,
    /// Last row.
    pub bottom: usize,
    /// First column.
    pub left: usize,
    /// Last column.
    pub right: usize,
}

impl Region {
    /// The whole screen.
    #[must_use]
    pub fn full(rows: usize, cols: usize) -> Region {
        Region {
            top: 0,
            bottom: rows.saturating_sub(1),
            left: 0,
            right: cols.saturating_sub(1),
        }
    }
}

/// One pre-measured glyph to print. Widths are inputs (decided once, at print time, by
/// the caller; the grid never recomputes them).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glyph {
    /// Code point.
    pub cp: u32,
    /// Columns: 1 or 2. 0 is skipped (zero-width marks go through `append_cp`).
    pub width: u8,
}

/// Attributes applied to printed cells.
#[derive(Clone, Copy, Debug)]
pub struct CellAttrs<'a> {
    /// Style (interned per page).
    pub style: Style,
    /// Hyperlink `(id, uri)`.
    pub link: Option<(&'a str, &'a str)>,
    /// DECSCA.
    pub protected: bool,
    /// OSC 133 zone.
    pub semantic: Semantic,
}

impl Default for CellAttrs<'_> {
    fn default() -> Self {
        CellAttrs {
            style: Style::DEFAULT,
            link: None,
            protected: false,
            semantic: Semantic::Output,
        }
    }
}

/// Counters for tests, benches and the debug overlay. Sizes and counts only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Items (style, link, cluster tail) dropped because even a split page had no room.
    pub degraded: u64,
    /// Pages copied into a larger page because a side table was full.
    pub page_growths: u64,
    /// Pages split because a side table was at its ceiling.
    pub page_splits: u64,
    /// Head pages evicted.
    pub evicted_pages: u64,
    /// Pages opened from the pool.
    pub pool_hits: u64,
    /// Pages newly allocated.
    pub pool_misses: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ScrollRec {
    pub(crate) region: Region,
    pub(crate) delta: isize,
}

/// What [`Screen::resize`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resized {
    /// Visible rows after the (clamped) resize.
    pub rows: usize,
    /// Columns after the (clamped) resize.
    pub cols: usize,
    /// Where the row that was the cursor row is now (active-area row).
    pub cursor_row: usize,
    /// Rows that moved from the top of the active area into scrollback.
    pub pushed: usize,
}

/// Open a blank row at the end of `pages`, starting a page when the last is full.
fn open_row(
    pages: &mut Vec<Page>,
    pool: &mut PagePool,
    spec: PageSpec,
    serial: &mut u64,
    wrap_width: u16,
) -> usize {
    if pages.last().is_none_or(Page::is_full) {
        let mut p = pool.acquire(spec, *serial);
        *serial += 1;
        p.set_wrap_width(wrap_width);
        pages.push(p);
    }
    pages.last_mut().and_then(Page::push_row).unwrap_or(0)
}

/// A terminal screen (primary with history, or alternate without).
#[derive(Debug)]
pub struct Screen {
    pub(crate) id: u64,
    pub(crate) epoch: u64,
    pub(crate) rows: usize,
    pub(crate) cols: usize,
    history: bool,
    limits: Limits,
    /// Template for new pages.
    spec: PageSpec,
    ceil: Ceil,
    /// Test-only fixed page row capacity (from `with_page_cap`), kept across resizes.
    cap: Option<u16>,
    pub(crate) pages: Vec<Page>,
    pool: PagePool,
    next_serial: u64,
    seq: u64,
    total_rows: usize,
    bytes_total: usize,
    pins: PinTable,
    pub(crate) pending: Option<ScrollRec>,
    pub(crate) force_full: bool,
    pub(crate) cursor_dirty: bool,
    stats: Stats,
}

fn two_mut<T>(v: &mut [T], a: usize, b: usize) -> (&mut T, &mut T) {
    debug_assert!(a != b);
    if a < b {
        let (l, r) = v.split_at_mut(b);
        (&mut l[a], &mut r[0])
    } else {
        let (l, r) = v.split_at_mut(a);
        (&mut r[0], &mut l[b])
    }
}

/// Scalars are stored in 21 bits and must be valid; anything else prints U+FFFD.
fn valid_scalar(cp: u32) -> u32 {
    if char::from_u32(cp).is_some() {
        cp
    } else {
        0xFFFD
    }
}

/// Store `cell`, taking the style and link references it will own.
fn put_acquired(page: &mut Page, pos: usize, x: usize, cell: Cell, sid: u16, link: Option<u16>) {
    page.style_acquire(sid, 1);
    if let Some(l) = link {
        page.link_acquire(l, 1);
    }
    page.put(pos, x, cell, link);
}

impl Screen {
    /// A blank screen of `rows x cols`. Each side is clamped to 1..=65535 and the area to
    /// [`MAX_CELLS`] (rows are reduced first); read the result back with `rows()`/`cols()`.
    #[must_use]
    pub fn new(rows: usize, cols: usize, cfg: ScreenConfig) -> Screen {
        Screen::with_page_cap(rows, cols, cfg, None)
    }

    pub(crate) fn with_page_cap(
        rows: usize,
        cols: usize,
        cfg: ScreenConfig,
        cap: Option<u16>,
    ) -> Screen {
        let cols = cols.clamp(1, u16::MAX as usize);
        let mut rows = rows.clamp(1, u16::MAX as usize);
        if rows * cols > MAX_CELLS {
            rows = (MAX_CELLS / cols).max(1);
        }
        let mut spec = match cap {
            Some(c) => PageSpec::with_rows(cols, usize::from(c)),
            None => PageSpec::standard(cols, PAGE_BYTES),
        };
        if !cfg.history && cap.is_none() {
            // Alternate screen: one fixed page set holding the whole active area.
            spec.rows_cap = spec.rows_cap.max(rows);
        }
        let mut s = Screen::build(rows, cols, cfg, spec, Ceil::MAX);
        s.cap = cap;
        s
    }

    /// Test constructor: explicit page template (including side-table capacities) and
    /// ceilings, so the grow and split paths are reachable with small data.
    #[cfg(test)]
    pub(crate) fn with_spec(
        rows: usize,
        cols: usize,
        cfg: ScreenConfig,
        spec: PageSpec,
        ceil: Ceil,
    ) -> Screen {
        Screen::build(rows, cols, cfg, spec, ceil)
    }

    fn build(rows: usize, cols: usize, cfg: ScreenConfig, spec: PageSpec, ceil: Ceil) -> Screen {
        let mut s = Screen {
            id: NEXT_SCREEN_ID.fetch_add(1, Ordering::Relaxed),
            epoch: 0,
            rows,
            cols,
            history: cfg.history,
            limits: cfg.limits,
            spec,
            ceil,
            cap: None,
            pages: Vec::new(),
            pool: PagePool::default(),
            next_serial: 1,
            seq: 0,
            total_rows: 0,
            bytes_total: 0,
            pins: PinTable::default(),
            pending: None,
            force_full: true,
            cursor_dirty: false,
            stats: Stats::default(),
        };
        for _ in 0..rows {
            s.append_row(Color::Default);
        }
        s
    }

    /// Visible rows.
    #[must_use]
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Columns.
    #[must_use]
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// Rows currently held above the active area.
    #[must_use]
    pub fn history_rows(&self) -> usize {
        self.total_rows - self.rows
    }

    /// Pages in the list.
    #[must_use]
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Logical bytes in use (page size x pages).
    #[must_use]
    pub fn bytes_in_use(&self) -> usize {
        self.bytes_total
    }

    /// Counters.
    #[must_use]
    pub fn stats(&self) -> Stats {
        Stats {
            pool_hits: self.pool.hits,
            pool_misses: self.pool.misses,
            ..self.stats
        }
    }

    /// Current limits.
    #[must_use]
    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Change limits at runtime; evicts immediately if now over.
    pub fn set_limits(&mut self, limits: Limits) {
        self.limits = limits;
        self.enforce_limits();
    }

    /// Whether this screen keeps history.
    #[must_use]
    pub fn has_history(&self) -> bool {
        self.history
    }

    /// Force the next `snapshot_dirty` to be `Full` (palette change, alt-screen switch...).
    pub fn mark_full(&mut self) {
        self.force_full = true;
        self.pending = None;
    }

    /// Record that only the cursor moved or changed look.
    pub fn mark_cursor_dirty(&mut self) {
        self.cursor_dirty = true;
    }

    fn bump(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    // ---- row addressing -------------------------------------------------------------

    pub(crate) fn loc(&self, y: usize) -> (usize, usize) {
        debug_assert!(y < self.rows);
        let mut rem = self.rows - 1 - y;
        for pi in (0..self.pages.len()).rev() {
            let l = self.pages[pi].used_rows();
            if rem < l {
                return (pi, l - 1 - rem);
            }
            rem -= l;
        }
        (0, 0)
    }

    /// `(page index, row position in that page)` of active-area row `y`.
    pub(crate) fn slot_of(&self, y: usize) -> (usize, usize) {
        self.loc(y)
    }

    fn page_logical_bytes(&self, p: &Page) -> usize {
        p.rows_cap() * (self.cols * 8 + ROW_HEADER_BYTES)
    }

    pub(crate) fn loc_abs(&self, abs: usize) -> Option<(usize, usize)> {
        let mut rem = abs;
        for (pi, p) in self.pages.iter().enumerate() {
            if rem < p.used_rows() {
                return Some((pi, rem));
            }
            rem -= p.used_rows();
        }
        None
    }

    // ---- reading ----------------------------------------------------------------------

    /// The raw cell at `(y, x)`.
    #[must_use]
    pub fn cell(&self, y: usize, x: usize) -> Cell {
        if y >= self.rows || x >= self.cols {
            return Cell::EMPTY;
        }
        let (pi, slot) = self.slot_of(y);
        self.pages[pi].cell(slot, x)
    }

    /// Hyperlink `(id, uri)` of the cell at `(y, x)`, if it carries one.
    #[must_use]
    pub fn link_at(&self, y: usize, x: usize) -> Option<(String, String)> {
        if y >= self.rows || x >= self.cols {
            return None;
        }
        let (pi, slot) = self.slot_of(y);
        let page = &self.pages[pi];
        let item = page.cell_link(slot, x)?;
        let (id, uri) = page.link_get(item);
        Some((id.to_string(), uri.to_string()))
    }

    /// Code points of a cluster cell (or the scalar) into `out`.
    pub fn cell_text(&self, y: usize, x: usize, out: &mut Vec<u32>) {
        out.clear();
        if y >= self.rows || x >= self.cols {
            return;
        }
        let (pi, slot) = self.slot_of(y);
        let c = self.pages[pi].cell(slot, x);
        match c.tag() {
            Tag::Scalar => out.push(c.payload()),
            Tag::Cluster => {
                let mut buf = ['\0'; MAX_CLUSTER_CPS];
                let n = self.pages[pi].cluster_read(c.payload(), &mut buf);
                out.extend(buf[..n].iter().map(|&ch| ch as u32));
            }
            _ => {}
        }
    }

    /// Mark the last cell of row `y` as a `SpacerHead` (a wide character wrapped to the next
    /// row, VTCORE-08). Only a blank last cell is replaced; reflow drops it when rejoining.
    pub fn set_spacer_head(&mut self, y: usize) {
        if y >= self.rows {
            return;
        }
        let x = self.cols - 1;
        let (pi, pos) = self.slot_of(y);
        if self.pages[pi].cell(pos, x) == Cell::EMPTY {
            self.pages[pi].put(pos, x, Cell::spacer_head(), None);
        }
    }

    /// Row flag bits (see [`flags`]).
    #[must_use]
    pub fn row_flags(&self, y: usize) -> u16 {
        if y >= self.rows {
            return 0;
        }
        let (pi, slot) = self.slot_of(y);
        self.pages[pi].row_flags(slot)
    }

    /// Shell-integration block id of a row.
    #[must_use]
    pub fn row_block(&self, y: usize) -> u32 {
        if y >= self.rows {
            return 0;
        }
        let (pi, slot) = self.slot_of(y);
        self.pages[pi].row_block(slot)
    }

    // ---- row metadata -----------------------------------------------------------------

    /// Set or clear one of `WRAPPED`, `WRAP_CONT`, `PROMPT_START`.
    pub fn set_row_flag(&mut self, y: usize, flag: u16, on: bool) {
        let allowed = flags::WRAPPED | flags::WRAP_CONT | flags::PROMPT_START;
        if y >= self.rows || flag & !allowed != 0 {
            return;
        }
        let (pi, slot) = self.slot_of(y);
        if (self.pages[pi].row_flags(slot) & flag == flag) == on {
            return;
        }
        let seq = self.bump();
        let p = &mut self.pages[pi];
        if on {
            p.or_row_flags(slot, flag);
        } else {
            p.clear_row_flags(slot, flag);
        }
        p.touch(slot, seq);
    }

    /// Set a row's shell-integration block id.
    pub fn set_row_block(&mut self, y: usize, block: u32) {
        if y >= self.rows {
            return;
        }
        let (pi, slot) = self.slot_of(y);
        if self.pages[pi].row_block(slot) == block {
            return;
        }
        let seq = self.bump();
        let p = &mut self.pages[pi];
        p.set_row_block(slot, block);
        p.set_row_seq(slot, seq);
    }

    // ---- printing ---------------------------------------------------------------------

    /// Run `f` against row `y`'s page, growing the page when a table is full and splitting
    /// it when a table is at its ceiling (T-02 §1.4). `Err` only when a split page still has
    /// no room; the caller then degrades the item and counts it.
    fn with_row<T>(
        &mut self,
        y: usize,
        mut f: impl FnMut(&mut Page, usize) -> Result<T, Full>,
    ) -> Result<T, Full> {
        let mut split = false;
        loop {
            let (pi, pos) = self.slot_of(y);
            match f(&mut self.pages[pi], pos) {
                Ok(v) => return Ok(v),
                Err(w) => {
                    if self.grow(pi, w) {
                        continue;
                    }
                    if !split && self.split_at(pi, pos) {
                        split = true;
                        continue;
                    }
                    return Err(w);
                }
            }
        }
    }

    /// Replace page `pi` by a copy with the `w` table doubled (up to the ceiling).
    fn grow(&mut self, pi: usize, w: Full) -> bool {
        if self.pages[pi].grow_in_place(w, &self.ceil) {
            self.stats.page_growths += 1;
            // A page that needed more starts its successors there: growing costs a copy of
            // the page tail. Bounded so one hot page cannot bloat every later one.
            if self.cap.is_none() {
                let spec = *self.pages[pi].spec();
                match w {
                    Full::Styles => {
                        self.spec.style_cap = self.spec.style_cap.max(spec.style_cap.min(8192))
                    }
                    Full::Chunks => {
                        self.spec.chunk_cap = self.spec.chunk_cap.max(spec.chunk_cap.min(4096))
                    }
                    _ => {}
                }
            }
            true
        } else {
            false
        }
    }

    /// Replace page `pi` by two fresh pages holding rows `..pos` and `pos..`, each
    /// re-interned from scratch (dead table items are dropped). Pins follow their rows.
    /// False, with nothing changed, if either half does not fit a grown fresh page.
    fn split_at(&mut self, pi: usize, pos: usize) -> bool {
        let total = self.pages[pi].used_rows();
        let ranges: &[(usize, usize)] = if pos == 0 {
            &[(0, total)]
        } else {
            &[(0, pos), (pos, total)]
        };
        let ceil = self.ceil;
        let cols = self.cols;
        let mut fresh: Vec<(Page, usize)> = Vec::with_capacity(2);
        for &(a, b) in ranges {
            let mut spec = self.spec;
            spec.rows_cap = spec.rows_cap.max(b - a);
            let mut q = Page::new(spec, self.next_serial + fresh.len() as u64);
            q.set_wrap_width(self.pages[pi].wrap_width());
            for k in a..b {
                let dst = q.push_row().unwrap_or(0);
                loop {
                    match q.import_row(dst, &self.pages[pi], k, 0, cols - 1, false) {
                        Ok(()) => break,
                        Err(w) => match q.grown_with(w, &ceil) {
                            Some(n) => q = n,
                            None => return false,
                        },
                    }
                }
            }
            fresh.push((q, a));
        }
        self.next_serial += fresh.len() as u64;
        let old = self.pages.remove(pi);
        self.total_rows -= old.used_rows();
        self.bytes_total -= self.page_logical_bytes(&old);
        for (q, a) in &fresh {
            for k in 0..q.used_rows() {
                self.pins.retarget(
                    (old.serial(), old.slot_at(a + k)),
                    (q.serial(), q.slot_at(k)),
                );
            }
        }
        let at = pi;
        for (i, (q, _)) in fresh.into_iter().enumerate() {
            self.total_rows += q.used_rows();
            self.bytes_total += self.page_logical_bytes(&q);
            self.pages.insert(at + i, q);
        }
        let std_len = self.spec.total_bytes();
        self.pool.release(old, std_len);
        self.stats.page_splits += 1;
        true
    }

    /// Intern what a print needs in row `y`'s page: the style, the hyperlink (with room for
    /// `link_cells` map entries) and, for a cluster, its arena chunks. Returns the stored
    /// style id, link item and cluster head. Items that cannot fit even a split page are
    /// dropped and counted in [`Stats::degraded`].
    fn intern_for(
        &mut self,
        y: usize,
        attrs: &CellAttrs<'_>,
        link_cells: usize,
        cluster: &[char],
    ) -> (u16, Option<u16>, Option<u32>) {
        let mut a = *attrs;
        let mut cl = cluster;
        loop {
            let r = self.with_row(y, |page, _| {
                let sid = page.style_intern(&a.style, 0)?;
                let link = match a.link {
                    Some((id, uri)) => match page.link_intern(id, uri, 0) {
                        Ok(l) => {
                            page.link_reserve(link_cells)?;
                            Some(l)
                        }
                        Err(LinkError::Rejected) => None,
                        Err(LinkError::Full(w)) => return Err(w),
                    },
                    None => None,
                };
                let head = if cl.len() > 1 {
                    Some(page.cluster_alloc(cl)?)
                } else {
                    None
                };
                Ok((sid, link, head))
            });
            match r {
                Ok(v) => return v,
                Err(w) => {
                    self.stats.degraded += 1;
                    match w {
                        Full::Styles => a.style = Style::DEFAULT,
                        Full::Chunks => cl = &cl[..1],
                        _ => a.link = None,
                    }
                }
            }
        }
    }

    /// The common print case: no link, so only the style needs interning, in the row's own
    /// page. `None` when that page has no room (the caller takes the growing path).
    #[inline]
    fn plain_slot(&mut self, y: usize, attrs: &CellAttrs<'_>) -> Option<(usize, usize, u16)> {
        if attrs.link.is_some() {
            return None;
        }
        let (pi, pos) = self.slot_of(y);
        let sid = self.pages[pi].style_intern(&attrs.style, 0).ok()?;
        Some((pi, pos, sid))
    }

    /// Print pre-measured glyphs starting at `(y, x)`. Stops before a glyph that does not
    /// fit in the row and returns how many glyphs were consumed; wrapping is the caller's
    /// (apoth-term) decision. Allocates nothing once the page tables have capacity.
    pub fn print_run(&mut self, y: usize, x: usize, run: &[Glyph], attrs: &CellAttrs<'_>) -> usize {
        if y >= self.rows || x >= self.cols {
            return 0;
        }
        let seq = self.bump();
        let cols = self.cols;
        let mut cells = 0;
        let mut cx = x;
        let (mut all_fit, mut no_zero) = (true, true);
        for g in run {
            if g.width == 0 {
                no_zero = false;
                continue;
            }
            let w = if g.width >= 2 { 2 } else { 1 };
            if cx + w > cols {
                all_fit = false;
                break;
            }
            cx += w;
            cells += w;
        }
        let (pi, pos, sid, link) = match self.plain_slot(y, attrs) {
            Some((pi, pos, sid)) => (pi, pos, sid, None),
            None => {
                let (sid, link, _) = self.intern_for(y, attrs, cells, &[]);
                let (pi, pos) = self.slot_of(y);
                (pi, pos, sid, link)
            }
        };
        let page = &mut self.pages[pi];
        let proto = Cell::EMPTY
            .with_style(sid)
            .with_protected(attrs.protected)
            .with_semantic(attrs.semantic)
            .with_link(link.is_some());
        if link.is_none() && cells == run.len() && run.iter().all(|g| g.width == 1) {
            page.put_narrow_run(pos, x, proto, run.len(), run.iter().map(|g| g.cp));
            page.touch(pos, seq);
            return run.len();
        }
        if link.is_none() && all_fit && no_zero {
            page.put_glyph_run(pos, x, proto, cells, run.iter().map(|g| (g.cp, g.width)));
            page.touch(pos, seq);
            return run.len();
        }
        let mut cx = x;
        let mut n = 0;
        for g in run {
            if g.width == 0 {
                n += 1;
                continue;
            }
            let w = if g.width >= 2 { 2 } else { 1 };
            if cx + w > cols {
                break;
            }
            let c = proto.with_tag(Tag::Scalar).with_payload(valid_scalar(g.cp));
            if w == 2 {
                put_acquired(page, pos, cx, c.with_wide(Wide::Wide), sid, link);
                put_acquired(
                    page,
                    pos,
                    cx + 1,
                    proto.with_wide(Wide::SpacerTail),
                    sid,
                    link,
                );
            } else {
                put_acquired(page, pos, cx, c, sid, link);
            }
            cx += w;
            n += 1;
        }
        page.touch(pos, seq);
        n
    }

    /// Print printable ASCII (0x20..0x7e) bytes as width-1 cells starting at `(y, x)`: the
    /// same as [`Screen::print_run`] on one-column glyphs, without building them. Returns the
    /// bytes consumed (fewer only when the row ends first). Allocates nothing once the page
    /// tables have capacity.
    pub fn print_ascii(&mut self, y: usize, x: usize, text: &[u8], attrs: &CellAttrs<'_>) -> usize {
        if y >= self.rows || x >= self.cols {
            return 0;
        }
        let n = text.len().min(self.cols - x);
        if n == 0 {
            return 0;
        }
        let seq = self.bump();
        let (pi, pos, sid, link) = match self.plain_slot(y, attrs) {
            Some((pi, pos, sid)) => (pi, pos, sid, None),
            None => {
                let (sid, link, _) = self.intern_for(y, attrs, n, &[]);
                let (pi, pos) = self.slot_of(y);
                (pi, pos, sid, link)
            }
        };
        let page = &mut self.pages[pi];
        let proto = Cell::EMPTY
            .with_style(sid)
            .with_protected(attrs.protected)
            .with_semantic(attrs.semantic)
            .with_link(link.is_some());
        let cps = text[..n].iter().map(|&b| u32::from(b));
        if link.is_none() {
            page.put_narrow_run(pos, x, proto, n, cps);
        } else {
            for (i, cp) in cps.enumerate() {
                let c = proto.with_tag(Tag::Scalar).with_payload(cp);
                put_acquired(page, pos, x + i, c, sid, link);
            }
        }
        page.touch(pos, seq);
        n
    }

    /// Print one grapheme cluster (several code points) of the given width. Returns false
    /// if it does not fit. A cluster over 64 code points is truncated.
    pub fn print_cluster(
        &mut self,
        y: usize,
        x: usize,
        cps: &[u32],
        width: u8,
        attrs: &CellAttrs<'_>,
    ) -> bool {
        if cps.is_empty() || y >= self.rows || x >= self.cols {
            return false;
        }
        let w = if width >= 2 { 2 } else { 1 };
        if x + w > self.cols {
            return false;
        }
        if cps.len() == 1 {
            return self.print_run(
                y,
                x,
                &[Glyph {
                    cp: cps[0],
                    width: w as u8,
                }],
                attrs,
            ) == 1;
        }
        let seq = self.bump();
        let mut chars = ['\0'; MAX_CLUSTER_CPS];
        let n = cps.len().min(MAX_CLUSTER_CPS);
        for (d, &c) in chars.iter_mut().zip(&cps[..n]) {
            *d = char::from_u32(valid_scalar(c)).unwrap_or('\u{FFFD}');
        }
        let (sid, link, head) = self.intern_for(y, attrs, w, &chars[..n]);
        let (pi, pos) = self.slot_of(y);
        let page = &mut self.pages[pi];
        let proto = Cell::EMPTY
            .with_style(sid)
            .with_protected(attrs.protected)
            .with_semantic(attrs.semantic)
            .with_link(link.is_some());
        let head = match head {
            Some(h) => proto.with_tag(Tag::Cluster).with_payload(h),
            None => proto
                .with_tag(Tag::Scalar)
                .with_payload(u32::from(chars[0])),
        };
        if w == 2 {
            put_acquired(page, pos, x, head.with_wide(Wide::Wide), sid, link);
            put_acquired(
                page,
                pos,
                x + 1,
                proto.with_wide(Wide::SpacerTail),
                sid,
                link,
            );
        } else {
            put_acquired(page, pos, x, head, sid, link);
        }
        page.touch(pos, seq);
        true
    }

    /// Append a code point to the cell at `(y, x)` (a combining mark or ZWJ continuation),
    /// turning a Scalar into a Cluster. A spacer-tail position resolves to its head. False
    /// if the cell is empty, or the cluster is at its 64 code-point cap.
    pub fn append_cp(&mut self, y: usize, x: usize, cp: u32) -> bool {
        if y >= self.rows || x >= self.cols {
            return false;
        }
        let seq = self.bump();
        let ch = char::from_u32(valid_scalar(cp)).unwrap_or('\u{FFFD}');
        let r = self.with_row(y, |page, pos| {
            let x = if x > 0 && page.cell(pos, x).wide() == Wide::SpacerTail {
                x - 1
            } else {
                x
            };
            let c = page.cell(pos, x);
            let ok = match c.tag() {
                Tag::Scalar => {
                    let first = char::from_u32(c.payload()).unwrap_or('\u{FFFD}');
                    page.make_cluster(pos, x, &[first, ch])?;
                    true
                }
                Tag::Cluster => page.cluster_append(c.payload(), ch)?,
                _ => false,
            };
            if ok {
                page.touch(pos, seq);
            }
            Ok(ok)
        });
        r.unwrap_or_else(|_| {
            self.stats.degraded += 1;
            false
        })
    }

    // ---- erase / insert / delete ------------------------------------------------------

    /// Erase columns `x0..x1` of one row with background `bg` (BCE). `selective` skips
    /// DECSCA-protected cells.
    pub fn erase_cells(&mut self, y: usize, x0: usize, x1: usize, bg: Color, selective: bool) {
        if y >= self.rows || x0 >= x1 {
            return;
        }
        let seq = self.bump();
        let (pi, slot) = self.slot_of(y);
        let cols = self.cols;
        let page = &mut self.pages[pi];
        page.erase_cells(slot, x0, x1.min(cols), Cell::bg_only(bg), selective, seq);
    }

    /// Erase whole rows `y0..=y1` and clear their wrap flags (unless `selective`).
    pub fn erase_rows(&mut self, y0: usize, y1: usize, bg: Color, selective: bool) {
        let y1 = y1.min(self.rows.saturating_sub(1));
        for y in y0..=y1 {
            self.erase_cells(y, 0, self.cols, bg, selective);
            if !selective {
                self.set_row_flag(y, flags::WRAPPED | flags::WRAP_CONT, false);
            }
        }
    }

    /// ICH: insert `n` blanks at `x`, shifting right up to column `right` (inclusive).
    pub fn insert_chars(&mut self, y: usize, x: usize, n: usize, right: usize, bg: Color) {
        if y >= self.rows || x > right || right >= self.cols || n == 0 {
            return;
        }
        let seq = self.bump();
        let (pi, slot) = self.slot_of(y);
        let page = &mut self.pages[pi];
        let n = n.min(right + 1 - x);
        page.insert_cells(slot, x, n, right, Cell::bg_only(bg), seq);
    }

    /// DCH: delete `n` cells at `x`, shifting left from column `right` (inclusive).
    pub fn delete_chars(&mut self, y: usize, x: usize, n: usize, right: usize, bg: Color) {
        if y >= self.rows || x > right || right >= self.cols || n == 0 {
            return;
        }
        let seq = self.bump();
        let (pi, slot) = self.slot_of(y);
        let page = &mut self.pages[pi];
        let n = n.min(right + 1 - x);
        page.delete_cells(slot, x, n, right, Cell::bg_only(bg), seq);
    }

    // ---- scrolling --------------------------------------------------------------------

    fn region_ok(&self, r: Region) -> bool {
        r.top <= r.bottom && r.bottom < self.rows && r.left <= r.right && r.right < self.cols
    }

    /// Scroll `region` up by `n` (SU, or LF at the bottom margin). On the primary screen a
    /// scroll of the whole screen appends to history and copies no cell.
    pub fn scroll_up(&mut self, region: Region, n: usize, bg: Color) {
        self.scroll_impl(region, n, true, true, bg);
    }

    /// Scroll `region` down by `n` (SD, RI at the top margin).
    pub fn scroll_down(&mut self, region: Region, n: usize, bg: Color) {
        self.scroll_impl(region, n, false, false, bg);
    }

    /// DL: delete `n` lines at the top of `region` (never feeds history).
    pub fn delete_lines(&mut self, region: Region, n: usize, bg: Color) {
        self.scroll_impl(region, n, true, false, bg);
    }

    /// IL: insert `n` lines at the top of `region`.
    pub fn insert_lines(&mut self, region: Region, n: usize, bg: Color) {
        self.scroll_impl(region, n, false, false, bg);
    }

    fn scroll_impl(&mut self, r: Region, n: usize, up: bool, allow_history: bool, bg: Color) {
        if !self.region_ok(r) || n == 0 {
            return;
        }
        let h = r.bottom - r.top + 1;
        let n = n.min(h);
        let full_w = r.left == 0 && r.right == self.cols - 1;
        if up && allow_history && self.history && full_w && r.top == 0 && r.bottom == self.rows - 1
        {
            for _ in 0..n {
                self.append_row(bg);
            }
            let seq = self.bump();
            self.bump_seq_rows(0, self.rows - 1, seq);
            self.record_scroll(r, n as isize);
            self.enforce_limits();
            return;
        }
        let seq = self.bump();
        if full_w {
            let (d0, d1) = if up {
                (r.top, r.top + n - 1)
            } else {
                (r.bottom + 1 - n, r.bottom)
            };
            for y in d0..=d1 {
                let (pi, pos) = self.slot_of(y);
                let (serial, slot) = (self.pages[pi].serial(), self.pages[pi].slot_at(pos));
                self.pins.garbage_row(serial, slot);
            }
            let (pt, post) = self.loc(r.top);
            let (pb, posb) = self.loc(r.bottom);
            if pt == pb && n == 1 && h > 1 {
                // One page holds the whole region: rotate the row handles, no cell moves.
                if up {
                    self.pages[pt].rotate_rows_up(post, posb, 1);
                } else {
                    self.pages[pt].rotate_rows_down(post, posb, 1);
                }
            } else if up {
                for y in r.top..r.bottom + 1 - n {
                    self.move_row(y + n, y, seq);
                }
            } else {
                for y in (r.top + n..=r.bottom).rev() {
                    self.move_row(y - n, y, seq);
                }
            }
            let (b0, b1) = if up {
                (r.bottom + 1 - n, r.bottom)
            } else {
                (r.top, r.top + n - 1)
            };
            for y in b0..=b1 {
                self.blank_row(y, bg, seq);
            }
            self.bump_seq_rows(r.top, r.bottom, seq);
            self.record_scroll(r, if up { n as isize } else { -(n as isize) });
        } else {
            if up {
                for y in r.top..r.bottom + 1 - n {
                    self.copy_row_span(y + n, y, r.left, r.right, seq);
                }
            } else {
                for y in (r.top + n..=r.bottom).rev() {
                    self.copy_row_span(y - n, y, r.left, r.right, seq);
                }
            }
            let (b0, b1) = if up {
                (r.bottom + 1 - n, r.bottom)
            } else {
                (r.top, r.top + n - 1)
            };
            for y in b0..=b1 {
                let (pi, slot) = self.slot_of(y);
                self.pages[pi].erase_cells(
                    slot,
                    r.left,
                    r.right + 1,
                    Cell::bg_only(bg),
                    false,
                    seq,
                );
            }
            // Partial-width scrolls are not recorded as Scroll damage: every row of the
            // region is simply dirty.
            for y in r.top..=r.bottom {
                let (pi, slot) = self.slot_of(y);
                self.pages[pi].touch(slot, seq);
            }
        }
    }

    /// Move row `ysrc` into `ydst` (the destination's old content is discarded). Within a
    /// page this rotates handles; across a page seam it copies the cells.
    fn move_row(&mut self, ysrc: usize, ydst: usize, seq: u64) {
        let cols = self.cols;
        let mut split = false;
        let mut lossy = false;
        loop {
            let (ps, poss) = self.loc(ysrc);
            let (pd, posd) = self.loc(ydst);
            if ps == pd {
                self.pages[ps].swap_positions(poss, posd);
                return;
            }
            let ss = self.pages[ps].slot_at(poss);
            let ds = self.pages[pd].slot_at(posd);
            let (src, dst) = two_mut(&mut self.pages, ps, pd);
            match dst.import_row(posd, src, poss, 0, cols - 1, lossy) {
                Ok(()) => {
                    dst.touch(posd, seq);
                    src.erase_cells(poss, 0, cols, Cell::EMPTY, false, seq);
                    let (from, to) = ((src.serial(), ss), (dst.serial(), ds));
                    self.pins.retarget(from, to);
                    return;
                }
                Err(w) => {
                    if self.grow(pd, w) {
                        continue;
                    }
                    if !split && self.split_at(pd, posd) {
                        split = true;
                        continue;
                    }
                    lossy = true;
                    self.stats.degraded += 1;
                }
            }
        }
    }

    fn copy_row_span(&mut self, ysrc: usize, ydst: usize, left: usize, right: usize, seq: u64) {
        let mut split = false;
        let mut lossy = false;
        loop {
            let (ps, poss) = self.loc(ysrc);
            let (pd, posd) = self.loc(ydst);
            let res = if ps == pd {
                self.pages[ps].move_cells_within(poss, posd, left, right, seq, lossy)
            } else {
                let (src, dst) = two_mut(&mut self.pages, ps, pd);
                let r = dst.import_row(posd, src, poss, left, right, lossy);
                if r.is_ok() {
                    dst.touch(posd, seq);
                    src.erase_cells(poss, left, right + 1, Cell::EMPTY, false, seq);
                }
                r
            };
            match res {
                Ok(()) => return,
                Err(w) => {
                    if self.grow(pd, w) {
                        continue;
                    }
                    if !split && self.split_at(pd, posd) {
                        split = true;
                        continue;
                    }
                    lossy = true;
                    self.stats.degraded += 1;
                }
            }
        }
    }

    fn blank_row(&mut self, y: usize, bg: Color, seq: u64) {
        let (pi, slot) = self.slot_of(y);
        let cols = self.cols;
        let page = &mut self.pages[pi];
        page.erase_cells(slot, 0, cols, Cell::bg_only(bg), false, seq);
        let keep = page.row_flags(slot) & flags::HAS_MASK;
        page.set_row_flags(slot, keep | flags::DIRTY);
        page.set_row_block(slot, 0);
        page.set_row_seq(slot, seq);
    }

    fn bump_seq_rows(&mut self, top: usize, bottom: usize, seq: u64) {
        // A row that changes position changed what is shown at both positions, so
        // `snapshot_since` consumers (which key on position) must see a new sequence.
        // One address lookup, then a run per page.
        let (mut pi, mut pos) = self.loc(top);
        let mut left = bottom + 1 - top;
        while left > 0 {
            let Some(page) = self.pages.get_mut(pi) else {
                return;
            };
            let used = page.used_rows();
            if pos < used {
                let take = (used - pos).min(left);
                page.set_row_seqs(pos, pos + take - 1, seq);
                left -= take;
            }
            pi += 1;
            pos = 0;
        }
    }

    fn record_scroll(&mut self, r: Region, delta: isize) {
        if self.force_full {
            return;
        }
        let h = r.bottom - r.top + 1;
        match self.pending {
            None => {
                if delta.unsigned_abs() >= h {
                    self.force_full = true;
                } else {
                    self.pending = Some(ScrollRec { region: r, delta });
                }
            }
            Some(p) if p.region == r => {
                let d = p.delta + delta;
                if d.unsigned_abs() >= h {
                    self.force_full = true;
                    self.pending = None;
                } else if d == 0 {
                    self.pending = None;
                } else {
                    self.pending = Some(ScrollRec {
                        region: r,
                        delta: d,
                    });
                }
            }
            Some(_) => {
                self.force_full = true;
                self.pending = None;
            }
        }
    }

    // ---- history growth and eviction --------------------------------------------------

    fn append_row(&mut self, bg: Color) {
        let seq = self.bump();
        let mut opened = false;
        if self.pages.last().is_none_or(Page::is_full) {
            self.adapt_style_cap();
            let serial = self.next_serial;
            self.next_serial += 1;
            let page = self.pool.acquire(self.spec, serial);
            self.bytes_total += self.page_logical_bytes(&page);
            self.pages.push(page);
            opened = true;
        }
        let cols = self.cols;
        let Some(page) = self.pages.last_mut() else {
            return;
        };
        let Some(slot) = page.push_row() else {
            return;
        };
        page.touch(slot, seq);
        if bg != Color::Default {
            page.erase_cells(slot, 0, cols, Cell::bg_only(bg), false, seq);
        }
        self.total_rows += 1;
        if opened {
            self.enforce_limits();
        }
    }

    /// Size the style table of the next page: a page that left most of its table idle lets
    /// the template shrink again (growth raises it, see `grow`).
    fn adapt_style_cap(&mut self) {
        if self.cap.is_some() {
            return;
        }
        let Some(last) = self.pages.last() else {
            return;
        };
        let (cap, used) = (last.spec().style_cap, last.style_items());
        let cur = self.spec.style_cap;
        if cur > PageSpec::DEFAULT_STYLE_CAP && cap >= cur && used * 8 < cur {
            self.spec.style_cap = (cur / 2).max(PageSpec::DEFAULT_STYLE_CAP);
        }
    }

    fn enforce_limits(&mut self) {
        while self.pages.len() > 1 {
            let head_len = self.pages[0].used_rows();
            if self.total_rows - head_len < self.rows {
                break;
            }
            let hist = self.total_rows - self.rows;
            let over_bytes = self.bytes_total > self.limits.bytes;
            let over_lines = self.limits.lines.is_some_and(|l| hist > l);
            if !(over_bytes || over_lines) {
                break;
            }
            let page = self.pages.remove(0);
            self.total_rows -= page.used_rows();
            self.bytes_total -= self.page_logical_bytes(&page);
            self.pins.garbage_page(page.serial());
            self.stats.evicted_pages += 1;
            let std_len = self.spec.total_bytes();
            self.pool.release(page, std_len);
        }
    }

    // ---- resize (no reflow; reflow is M12) ----------------------------------------------

    /// Resize with the cursor assumed on the bottom row: shrinking rows always pushes the
    /// top rows into scrollback. See [`Screen::resize_keeping`].
    pub fn resize(&mut self, rows: usize, cols: usize) -> Resized {
        let cy = self.rows - 1;
        self.resize_keeping(rows, cols, cy)
    }

    /// Resize to `rows x cols` without reflow (clamped like [`Screen::new`]); `cursor_row`
    /// is the active-area row that must stay visible.
    ///
    /// * Columns are truncated or padded with blanks; a wide character cut by the new right
    ///   edge is blanked (a stale `SpacerHead` is blanked when the row widens).
    /// * Growing rows appends blank rows at the bottom (nothing is pulled from history).
    /// * Shrinking rows first drops rows below the cursor; the rest leaves from the top into
    ///   scrollback on the primary screen (xterm style) or is discarded on the alternate
    ///   screen. Pins on discarded rows become garbage; all others follow their rows.
    /// * Every row is reported dirty (the next snapshot is `Full`, consumers reset).
    pub fn resize_keeping(&mut self, rows: usize, cols: usize, cursor_row: usize) -> Resized {
        let nc = cols.clamp(1, u16::MAX as usize);
        let mut nr = rows.clamp(1, u16::MAX as usize);
        if nr * nc > MAX_CELLS {
            nr = (MAX_CELLS / nc).max(1);
        }
        let (or, oc) = (self.rows, self.cols);
        let cy = cursor_row.min(or - 1);
        let d = or.saturating_sub(nr);
        let trim = d.min(or - 1 - cy);
        let drop_top = d - trim;
        let append = nr.saturating_sub(or);
        let out = Resized {
            rows: nr,
            cols: nc,
            cursor_row: cy - drop_top,
            pushed: if self.history { drop_top } else { 0 },
        };
        if nr == or && nc == oc {
            return out;
        }
        if nc == oc && trim == 0 && (self.history || drop_top == 0) {
            // Rows only, nothing to remove: no page is touched.
            for _ in 0..append {
                self.append_row(Color::Default);
            }
            self.rows = nr;
            self.enforce_limits();
        } else {
            self.rebuild(nr, nc, trim, drop_top, append);
        }
        self.epoch += 1;
        self.force_full = true;
        self.pending = None;
        let seq = self.bump();
        for y in 0..self.rows {
            let (pi, slot) = self.slot_of(y);
            self.pages[pi].touch(slot, seq);
        }
        out
    }

    /// Copy the retained rows into fresh pages of the new width (see `resize_keeping`).
    fn rebuild(&mut self, nr: usize, nc: usize, trim: usize, drop_top: usize, append: usize) {
        let (oc, history) = (self.cols, self.history);
        let mut spec = match self.cap {
            Some(c) => PageSpec::with_rows(nc, usize::from(c)),
            None => PageSpec::standard(nc, PAGE_BYTES),
        };
        if !history && self.cap.is_none() {
            spec.rows_cap = spec.rows_cap.max(nr);
        }
        let keep_start = if history { 0 } else { drop_top };
        let keep_end = self.total_rows - trim;
        let old = std::mem::take(&mut self.pages);
        let ceil = self.ceil;
        let mut serial = self.next_serial;
        let mut fresh: Vec<Page> = Vec::new();
        let mut degraded = 0;
        let copy_cols = nc.min(oc);
        let keep_flags = flags::WRAPPED | flags::WRAP_CONT | flags::PROMPT_START;
        let seq = self.bump();
        let mut abs = 0;
        self.pool.free.clear();
        for src in &old {
            for pos in 0..src.used_rows() {
                let a = abs + pos;
                if a < keep_start || a >= keep_end {
                    continue;
                }
                let dpos = open_row(
                    &mut fresh,
                    &mut self.pool,
                    spec,
                    &mut serial,
                    src.wrap_width(),
                );
                let di = fresh.len() - 1;
                let mut lossy = false;
                loop {
                    match fresh[di].import_row(dpos, src, pos, 0, copy_cols - 1, lossy) {
                        Ok(()) => break,
                        Err(w) => match fresh[di].grown_with(w, &ceil) {
                            Some(n) => fresh[di] = n,
                            None => {
                                lossy = true;
                                degraded += 1;
                            }
                        },
                    }
                }
                let dst = &mut fresh[di];
                if nc > oc && dst.cell(dpos, oc - 1).wide() == Wide::SpacerHead {
                    dst.fill_range(dpos, oc - 1, oc, Cell::EMPTY);
                }
                dst.or_row_flags(dpos, src.row_flags(pos) & keep_flags);
                dst.set_row_block(dpos, src.row_block(pos));
                dst.touch(dpos, seq);
            }
            abs += src.used_rows();
        }
        for _ in 0..append {
            let pos = open_row(&mut fresh, &mut self.pool, spec, &mut serial, 0);
            if let Some(p) = fresh.last_mut() {
                p.touch(pos, seq);
            }
        }
        // Pins: old absolute row -> new absolute row (kept rows are contiguous).
        self.pins.remap(|p| {
            let pi = old.iter().position(|pg| pg.serial() == p.serial)?;
            let pos = old[pi].pos_of(p.slot)?;
            let a = old[..pi].iter().map(Page::used_rows).sum::<usize>() + pos;
            if a < keep_start || a >= keep_end {
                return None;
            }
            let mut rem = a - keep_start;
            for q in &fresh {
                if rem < q.used_rows() {
                    return Some(Pin {
                        serial: q.serial(),
                        slot: q.slot_at(rem),
                        col: p.col.min((nc - 1) as u16),
                        wrap_width: q.wrap_width(),
                    });
                }
                rem -= q.used_rows();
            }
            None
        });
        self.next_serial = serial;
        self.stats.degraded += degraded;
        self.rows = nr;
        self.cols = nc;
        self.spec = spec;
        self.total_rows = fresh.iter().map(Page::used_rows).sum();
        self.pages = fresh;
        self.bytes_total = self.pages.iter().map(|p| self.page_logical_bytes(p)).sum();
        drop(old);
        self.enforce_limits();
    }

    // ---- pins -------------------------------------------------------------------------

    /// Track a pin at an active-area position.
    pub fn track_active(&mut self, y: usize, x: usize, kind: PinKind) -> Option<PinId> {
        if y >= self.rows || x >= self.cols {
            return None;
        }
        let (pi, slot) = self.slot_of(y);
        let p = &self.pages[pi];
        let pin = Pin {
            serial: p.serial(),
            slot: p.slot_at(slot),
            col: x as u16,
            wrap_width: p.wrap_width(),
        };
        self.pins.track(pin, kind)
    }

    /// Track a pin at row `abs` counted from the oldest retained row (history included).
    pub fn track_abs(&mut self, abs: usize, x: usize, kind: PinKind) -> Option<PinId> {
        if x >= self.cols {
            return None;
        }
        let (pi, pos) = self.loc_abs(abs)?;
        let p = &self.pages[pi];
        let pin = Pin {
            serial: p.serial(),
            slot: p.slot_at(pos),
            col: x as u16,
            wrap_width: p.wrap_width(),
        };
        self.pins.track(pin, kind)
    }

    /// The pin value for row `abs` (from the oldest retained row) and column `x`. The value
    /// stays valid across scroll and eviction checks; hand it to [`Screen::track_pin`] to keep
    /// it across reflow.
    #[must_use]
    pub fn pin_at(&self, abs: usize, x: usize) -> Option<Pin> {
        if x >= self.cols {
            return None;
        }
        let (pi, pos) = self.loc_abs(abs)?;
        let p = &self.pages[pi];
        Some(Pin {
            serial: p.serial(),
            slot: p.slot_at(pos),
            col: x as u16,
            wrap_width: p.wrap_width(),
        })
    }

    /// Track an existing pin value (for example a search match).
    pub fn track_pin(&mut self, pin: Pin, kind: PinKind) -> Option<PinId> {
        self.pin_abs(&pin)?;
        self.pins.track(pin, kind)
    }

    /// `(abs row, col)` of a pin value, if its row still exists.
    #[must_use]
    pub fn pin_abs(&self, p: &Pin) -> Option<(usize, usize)> {
        let pi = self.pages.iter().position(|pg| pg.serial() == p.serial)?;
        let pos = self.pages[pi].pos_of(p.slot)?;
        let before: usize = self.pages[..pi].iter().map(Page::used_rows).sum();
        Some((before + pos, p.col as usize))
    }

    /// Current state of a pin; `None` for an unknown or untracked id.
    #[must_use]
    pub fn pin_state(&self, id: PinId) -> Option<PinState> {
        self.pins.get(id)
    }

    /// `(abs row from the oldest retained row, col)` of a live pin.
    #[must_use]
    pub fn pin_pos(&self, id: PinId) -> Option<(usize, usize)> {
        let PinState::Live(p) = self.pins.get(id)? else {
            return None;
        };
        let pi = self.pages.iter().position(|pg| pg.serial() == p.serial)?;
        let pos = self.pages[pi].pos_of(p.slot)?;
        let before: usize = self.pages[..pi].iter().map(Page::used_rows).sum();
        Some((before + pos, p.col as usize))
    }

    /// Active-area row of a live pin, if it is currently in the active area.
    #[must_use]
    pub fn pin_active_row(&self, id: PinId) -> Option<usize> {
        let (abs, _) = self.pin_pos(id)?;
        abs.checked_sub(self.history_rows())
    }

    /// Move a live pin to another column of its row.
    pub fn set_pin_col(&mut self, id: PinId, col: usize) -> bool {
        col < self.cols && self.pins.set_col(id, col as u16)
    }

    /// Stop tracking a pin.
    pub fn untrack_pin(&mut self, id: PinId) -> bool {
        self.pins.untrack(id)
    }

    /// Drain `(pin, kind)` notifications for pins whose rows were evicted or discarded.
    pub fn take_garbage(&mut self, out: &mut Vec<(PinId, PinKind)>) {
        self.pins.take_garbage(out);
    }

    // ---- test support -----------------------------------------------------------------

    #[cfg(test)]
    pub(crate) fn pool_free(&self) -> usize {
        self.pool.free.len()
    }

    /// Structural invariants: every page audits, accounting matches, pins resolve.
    #[cfg(test)]
    pub(crate) fn check_invariants(&self) -> Result<(), String> {
        let mut total = 0;
        let mut bytes = 0;
        let mut serials = std::collections::HashSet::new();
        for p in &self.pages {
            p.audit()?;
            total += p.used_rows();
            bytes += self.page_logical_bytes(p);
            if !serials.insert(p.serial()) {
                return Err("duplicate page serial".into());
            }
        }
        if total != self.total_rows || bytes != self.bytes_total {
            return Err(format!(
                "accounting: rows {total}/{} bytes {bytes}/{}",
                self.total_rows, self.bytes_total
            ));
        }
        if self.total_rows < self.rows {
            return Err("fewer rows than the active area".into());
        }
        for (id, _) in self.pins.all_live() {
            if self.pin_pos(id).is_none() {
                return Err("live pin does not resolve".into());
            }
        }
        Ok(())
    }
}
