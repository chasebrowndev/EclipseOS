// SPDX-License-Identifier: AGPL-3.0-only
//! A page: one contiguous zeroed byte buffer holding row headers, cells, a style table, a
//! grapheme arena, a hyperlink table and a string arena (T-02 §1.1).
//!
//! Every internal reference is a page-local offset or index, never a pointer, so a page can
//! be moved, serialised ([`Page::as_bytes`], [`Page::from_bytes`]) or sent as bytes. All
//! access goes through typed accessors over the byte buffer; there is no `unsafe`.
//!
//! Layout, all little-endian and 8-aligned per region:
//!
//! ```text
//! header (64 B) | row headers (24 B each) | cells (8 B each) | style items (32 B each)
//! | style index (u16 slots, power of two) | cluster chunks (20 B each)
//! | link index (u16 slots) | link items (16 B each) | link map (8 B each)
//! | link-map index (u32 slots) | string arena
//! ```
//!
//! Row header: `cell_off u32 | flags u16 | high_water u16 | block u32 | pad u32 | seq u64`.

use crate::cell::{Cell, Tag, Wide};
use crate::style::Style;

/// Bytes in one row header.
pub const ROW_HEADER_BYTES: usize = 24;
/// Maximum scalars in one stored cluster (UNICODE-07).
pub const MAX_CLUSTER_SCALARS: usize = 64;
/// Maximum hyperlink URI length in bytes.
pub const MAX_URI: usize = 2083;
/// Maximum hyperlink id length in bytes.
pub const MAX_LINK_ID: usize = 250;
/// Largest style table (ids are 16-bit and 0 is the default style).
pub const MAX_STYLE_CAP: usize = 65_535;
/// Smallest row capacity of any page; very wide grids make oversize pages.
pub const MIN_PAGE_ROWS: usize = 32;
/// Standard page size in bytes (T-02 §1.1; fixed by the M3 bench).
pub const PAGE_BYTES: usize = 512 * 1024;
/// Alias used by the screen: scalars in one stored cluster.
pub const MAX_CLUSTER_CPS: usize = MAX_CLUSTER_SCALARS;

const HEADER_BYTES: usize = 64;
const STYLE_ITEM_BYTES: usize = 32;
const CHUNK_BYTES: usize = 20;
const LINK_ITEM_BYTES: usize = 16;
const LMAP_ENTRY_BYTES: usize = 8;
const MAGIC: u32 = 0x4150_4750; // "APGP"
const MAX_CHUNK_CAP: usize = 1 << 21;
const MAX_LINK_CAP: usize = 65_535;
const MAX_LMAP_CAP: usize = 1 << 20;
const MAX_STR_CAP: usize = 1 << 24;

const H_SERIAL: usize = 8;
const H_COLS: usize = 16;
const H_ROWS_CAP: usize = 18;
const H_USED: usize = 20;
const H_WRAP: usize = 22;
const H_STYLE_CAP: usize = 24;
const H_STYLE_USED: usize = 26;
const H_CHUNK_CAP: usize = 28;
const H_CHUNK_BUMP: usize = 32;
const H_CHUNK_FREE: usize = 36;
const H_LINK_CAP: usize = 40;
const H_LINK_USED: usize = 42;
const H_LMAP_CAP: usize = 44;
const H_LMAP_LEN: usize = 48;
const H_STR_CAP: usize = 52;
const H_STR_USED: usize = 56;
const H_CHUNK_FREE_COUNT: usize = 60;

/// Row flag bits (T-02 §1.3). Flags may be false-positive, never false-negative.
pub mod row_flags {
    /// The row soft-wraps into the next one.
    pub const WRAPPED: u16 = 1;
    /// The row continues the previous one.
    pub const WRAP_CONTINUATION: u16 = 1 << 1;
    /// Some cell may be a cluster.
    pub const HAS_CLUSTER: u16 = 1 << 2;
    /// Some cell may have a non-default style.
    pub const HAS_STYLE: u16 = 1 << 3;
    /// Some cell may carry a hyperlink.
    pub const HAS_LINK: u16 = 1 << 4;
    /// Some cell may be a multicell member.
    pub const HAS_MULTICELL: u16 = 1 << 5;
    /// A shell prompt starts on this row.
    pub const PROMPT_START: u16 = 1 << 6;
    /// Changed since the renderer's last `snapshot_dirty`.
    pub const DIRTY: u16 = 1 << 7;
    /// Flags that make erase and reset leave the fast path.
    pub const HEAVY: u16 = HAS_CLUSTER | HAS_STYLE | HAS_LINK | HAS_MULTICELL;
    /// Short name of [`WRAP_CONTINUATION`].
    pub const WRAP_CONT: u16 = WRAP_CONTINUATION;
    /// The "may hold resources" bits ([`HEAVY`]).
    pub const HAS_MASK: u16 = HEAVY;
}
use row_flags as rf;

/// Largest size each growable table of a page may reach. The screen splits a page when a
/// table is at its ceiling (T-02 §1.4); tests lower the ceilings to reach that path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ceil {
    /// Style items.
    pub styles: usize,
    /// Cluster chunks.
    pub chunks: usize,
    /// Hyperlink items.
    pub links: usize,
    /// Hyperlink map entries.
    pub lmap: usize,
    /// String arena bytes.
    pub strs: usize,
}

impl Ceil {
    /// The format limits.
    pub const MAX: Ceil = Ceil {
        styles: MAX_STYLE_CAP,
        chunks: MAX_CHUNK_CAP,
        links: MAX_LINK_CAP,
        lmap: MAX_LMAP_CAP,
        strs: MAX_STR_CAP,
    };
}

/// Which page table ran out of room; the caller grows the page and retries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Full {
    /// Style items.
    Styles,
    /// Cluster chunks.
    Chunks,
    /// Hyperlink items.
    Links,
    /// Hyperlink map entries.
    LinkMap,
    /// String arena.
    Strings,
}

/// Why a hyperlink could not be interned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkError {
    /// A page table is full.
    Full(Full),
    /// The URI or id exceeds the size limit, or the URI is empty.
    Rejected,
}

/// Why bytes are not a valid page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageError {
    /// Wrong magic or a length that does not match the header.
    Corrupt,
}

/// Capacities of a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageSpec {
    /// Columns per row.
    pub cols: usize,
    /// Row slots.
    pub rows_cap: usize,
    /// Style items (ids `1..style_cap`).
    pub style_cap: usize,
    /// Cluster chunks.
    pub chunk_cap: usize,
    /// Hyperlink items.
    pub link_cap: usize,
    /// Hyperlink map entries.
    pub lmap_cap: usize,
    /// String arena bytes.
    pub str_cap: usize,
}

impl PageSpec {
    /// Starting capacities of the side tables; they grow on demand.
    pub const DEFAULT_STYLE_CAP: usize = 128;

    /// A page of about `page_bytes` for `cols` columns (oversize when the grid is very wide).
    pub fn standard(cols: usize, page_bytes: usize) -> PageSpec {
        let mut s = PageSpec::with_rows(cols, 1);
        let fixed = Layout::of(&s).total - (ROW_HEADER_BYTES + s.cols * 8);
        let per_row = ROW_HEADER_BYTES + s.cols * 8;
        s.rows_cap = (page_bytes.saturating_sub(fixed) / per_row).clamp(MIN_PAGE_ROWS, 65_535);
        s
    }

    /// A page with exactly `rows_cap` row slots and default side tables.
    pub fn with_rows(cols: usize, rows_cap: usize) -> PageSpec {
        PageSpec {
            cols: cols.clamp(1, 65_535),
            rows_cap: rows_cap.clamp(1, 65_535),
            style_cap: Self::DEFAULT_STYLE_CAP,
            chunk_cap: 512,
            link_cap: 32,
            lmap_cap: 64,
            str_cap: 8192,
        }
    }

    /// Total bytes of a page with this spec.
    pub fn total_bytes(&self) -> usize {
        Layout::of(self).total
    }
}

fn align8(v: usize) -> usize {
    (v + 7) & !7
}

#[derive(Clone, Copy, Debug)]
struct Layout {
    rows: usize,
    cells: usize,
    styles: usize,
    sindex: usize,
    index_slots: usize,
    chunks: usize,
    links: usize,
    lkidx: usize,
    lkidx_slots: usize,
    lmap: usize,
    lidx: usize,
    lidx_slots: usize,
    strs: usize,
    total: usize,
}

impl Layout {
    fn of(s: &PageSpec) -> Layout {
        let mut o = HEADER_BYTES;
        let rows = o;
        o += align8(s.rows_cap * ROW_HEADER_BYTES);
        let cells = o;
        o += s.rows_cap * s.cols * 8;
        let styles = o;
        o += s.style_cap * STYLE_ITEM_BYTES;
        let sindex = o;
        let index_slots = (2 * s.style_cap).next_power_of_two();
        o += align8(index_slots * 2);
        let chunks = o;
        o += align8(s.chunk_cap * CHUNK_BYTES);
        let links = o;
        o += s.link_cap * LINK_ITEM_BYTES;
        let lkidx = o;
        let lkidx_slots = (2 * s.link_cap).next_power_of_two();
        o += align8(lkidx_slots * 2);
        let lmap = o;
        o += s.lmap_cap * LMAP_ENTRY_BYTES;
        let lidx = o;
        let lidx_slots = (2 * s.lmap_cap).next_power_of_two();
        o += lidx_slots * 4;
        let strs = o;
        o += align8(s.str_cap);
        Layout {
            rows,
            cells,
            styles,
            sindex,
            index_slots,
            chunks,
            links,
            lkidx,
            lkidx_slots,
            lmap,
            lidx,
            lidx_slots,
            strs,
            total: o,
        }
    }
}

fn scalar_wide_bits(w: Wide) -> u64 {
    Cell::EMPTY.with_wide(w).0
}

/// A page. See the module documentation.
pub struct Page {
    buf: Vec<u8>,
    spec: PageSpec,
    lay: Layout,
    /// The style interned last and its id: a repeat hit skips the hash probe. Cleared when
    /// dead items are reclaimed (ids may then be reused).
    last_style: Option<(Style, u16)>,
}

impl core::fmt::Debug for Page {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Page")
            .field("serial", &self.serial())
            .field("spec", &self.spec)
            .field("used_rows", &self.used_rows())
            .finish()
    }
}

impl Page {
    /// A fresh page over `buf`, which must be `spec.total_bytes()` long and zeroed (or come from
    /// [`Page::recycle`]: only the cell area may hold old bytes).
    pub fn with_buffer(spec: PageSpec, serial: u64, buf: Vec<u8>) -> Page {
        let lay = Layout::of(&spec);
        assert_eq!(buf.len(), lay.total, "page buffer has the wrong length");
        let mut p = Page {
            buf,
            spec,
            lay,
            last_style: None,
        };
        p.wr32(0, MAGIC);
        p.wr64(H_SERIAL, serial);
        p.wr16(H_COLS, spec.cols as u16);
        p.wr16(H_ROWS_CAP, spec.rows_cap as u16);
        p.wr16(H_STYLE_CAP, spec.style_cap as u16);
        p.wr16(H_STYLE_USED, 1);
        p.wr32(H_CHUNK_CAP, spec.chunk_cap as u32);
        p.wr16(H_LINK_CAP, spec.link_cap as u16);
        p.wr32(H_LMAP_CAP, spec.lmap_cap as u32);
        p.wr32(H_STR_CAP, spec.str_cap as u32);
        p
    }

    /// A fresh page with its own zeroed allocation.
    pub fn new(spec: PageSpec, serial: u64) -> Page {
        Page::with_buffer(spec, serial, vec![0u8; spec.total_bytes()])
    }

    /// Reconstruct a page from its serialised bytes.
    pub fn from_bytes(buf: Vec<u8>) -> Result<Page, PageError> {
        if buf.len() < HEADER_BYTES {
            return Err(PageError::Corrupt);
        }
        let r16 = |o: usize| usize::from(u16::from_le_bytes([buf[o], buf[o + 1]]));
        let r32 =
            |o: usize| u32::from_le_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]) as usize;
        if r32(0) != MAGIC as usize {
            return Err(PageError::Corrupt);
        }
        let spec = PageSpec {
            cols: r16(H_COLS),
            rows_cap: r16(H_ROWS_CAP),
            style_cap: r16(H_STYLE_CAP),
            chunk_cap: r32(H_CHUNK_CAP),
            link_cap: r16(H_LINK_CAP),
            lmap_cap: r32(H_LMAP_CAP),
            str_cap: r32(H_STR_CAP),
        };
        if spec.cols == 0 || spec.rows_cap == 0 || spec.style_cap == 0 {
            return Err(PageError::Corrupt);
        }
        if spec.style_cap > MAX_STYLE_CAP
            || spec.chunk_cap > MAX_CHUNK_CAP
            || spec.link_cap > MAX_LINK_CAP
            || spec.lmap_cap > MAX_LMAP_CAP
            || spec.str_cap > MAX_STR_CAP
        {
            return Err(PageError::Corrupt);
        }
        let lay = Layout::of(&spec);
        if lay.total != buf.len() {
            return Err(PageError::Corrupt);
        }
        let mut page = Page {
            buf,
            spec,
            lay,
            last_style: None,
        };
        page.validate()?;
        // The hash indexes are derived data; never trust them from outside (a full or
        // cyclic probe table would hang a lookup).
        page.rebuild_index();
        page.rebuild_link_index();
        page.rebuild_lidx();
        page.audit().map_err(|_| PageError::Corrupt)?;
        Ok(page)
    }

    /// Structural validation of untrusted bytes: every counter within its capacity and every
    /// row's cell offset a distinct, aligned slot inside the cells region. After this no
    /// accessor can index out of range; the cell contents are then checked by [`Page::audit`].
    fn validate(&self) -> Result<(), PageError> {
        let s = &self.spec;
        let used = self.used_rows();
        let ok = used <= s.rows_cap
            && (1..=s.style_cap).contains(&usize::from(self.rd16(H_STYLE_USED)))
            && self.rd32(H_CHUNK_BUMP) as usize <= s.chunk_cap
            && self.rd32(H_CHUNK_FREE) as usize <= self.rd32(H_CHUNK_BUMP) as usize
            && self.rd32(H_CHUNK_FREE_COUNT) <= self.rd32(H_CHUNK_BUMP)
            && usize::from(self.rd16(H_LINK_USED)) <= s.link_cap
            && self.rd32(H_LMAP_LEN) as usize <= s.lmap_cap
            && self.rd32(H_STR_USED) as usize <= s.str_cap;
        if !ok {
            return Err(PageError::Corrupt);
        }
        let row_bytes = s.cols * 8;
        let mut seen = vec![false; s.rows_cap];
        for r in 0..used {
            let off = self.cell_off(r);
            if off < self.lay.cells || !(off - self.lay.cells).is_multiple_of(row_bytes) {
                return Err(PageError::Corrupt);
            }
            let slot = (off - self.lay.cells) / row_bytes;
            if slot >= s.rows_cap || seen[slot] || self.row_high_water(r) > s.cols {
                return Err(PageError::Corrupt);
            }
            seen[slot] = true;
        }
        // The cluster free list must be an acyclic chain of in-range chunks.
        let (mut fh, mut walked) = (self.rd32(H_CHUNK_FREE) as usize, 0u32);
        while fh != 0 {
            walked += 1;
            if fh > self.rd32(H_CHUNK_BUMP) as usize || walked > self.rd32(H_CHUNK_FREE_COUNT) {
                return Err(PageError::Corrupt);
            }
            fh = self.rd32(self.ck(fh - 1) + 16) as usize;
        }
        if walked != self.rd32(H_CHUNK_FREE_COUNT) {
            return Err(PageError::Corrupt);
        }
        // Link map entries must name a real slot, column and link item.
        let links = self.link_items();
        for e in 0..self.rd32(H_LMAP_LEN) as usize {
            let o = self.lm(e);
            let (slot, col, link) = (self.rd16(o), self.rd16(o + 2), self.rd16(o + 4));
            if usize::from(slot) >= s.rows_cap
                || usize::from(col) >= s.cols
                || usize::from(link) >= links
            {
                return Err(PageError::Corrupt);
            }
        }
        Ok(())
    }

    /// The serialised page.
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    /// Give up the allocation (for pooling).
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    /// Give up the allocation for reuse by [`Page::with_buffer`]: everything but the cells is
    /// zeroed; [`Page::push_row`] clears a row's cells when it opens the row, so the (large)
    /// cell area is cleared only as far as it is used again.
    pub fn recycle(mut self) -> Vec<u8> {
        let (cells, styles) = (self.lay.cells, self.lay.styles);
        self.buf[..cells].fill(0);
        self.buf[styles..].fill(0);
        self.buf
    }

    /// Capacities.
    pub fn spec(&self) -> &PageSpec {
        &self.spec
    }

    /// Columns per row.
    pub fn cols(&self) -> usize {
        self.spec.cols
    }

    /// Page serial.
    pub fn serial(&self) -> u64 {
        self.rd64(H_SERIAL)
    }

    /// Rows in use.
    pub fn used_rows(&self) -> usize {
        usize::from(self.rd16(H_USED))
    }

    /// Row slots.
    pub fn rows_cap(&self) -> usize {
        self.spec.rows_cap
    }

    /// The `wrap_width` tag reserved for lazy reflow (T-02 §3); 0 means "current".
    pub fn wrap_width(&self) -> u16 {
        self.rd16(H_WRAP)
    }

    /// Set the `wrap_width` tag.
    pub fn set_wrap_width(&mut self, w: u16) {
        self.wr16(H_WRAP, w);
    }

    // ---- raw accessors -------------------------------------------------------------

    #[inline]
    fn rd16(&self, o: usize) -> u16 {
        u16::from_le_bytes([self.buf[o], self.buf[o + 1]])
    }

    #[inline]
    fn wr16(&mut self, o: usize, v: u16) {
        self.buf[o..o + 2].copy_from_slice(&v.to_le_bytes());
    }

    #[inline]
    fn rd32(&self, o: usize) -> u32 {
        u32::from_le_bytes([
            self.buf[o],
            self.buf[o + 1],
            self.buf[o + 2],
            self.buf[o + 3],
        ])
    }

    #[inline]
    fn wr32(&mut self, o: usize, v: u32) {
        self.buf[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }

    #[inline]
    fn rd64(&self, o: usize) -> u64 {
        let mut b = [0u8; 8];
        b.copy_from_slice(&self.buf[o..o + 8]);
        u64::from_le_bytes(b)
    }

    #[inline]
    fn wr64(&mut self, o: usize, v: u64) {
        self.buf[o..o + 8].copy_from_slice(&v.to_le_bytes());
    }

    // ---- rows ----------------------------------------------------------------------

    #[inline]
    fn rh(&self, r: usize) -> usize {
        self.lay.rows + r * ROW_HEADER_BYTES
    }

    #[inline]
    fn cell_off(&self, r: usize) -> usize {
        self.rd32(self.rh(r)) as usize
    }

    /// The storage slot of row `r`'s cells (stable across row-handle rotation).
    fn slot(&self, r: usize) -> usize {
        (self.cell_off(r) - self.lay.cells) / (self.spec.cols * 8)
    }

    /// Open a new blank row at the end; `None` when the page has no free row slot.
    pub fn push_row(&mut self) -> Option<usize> {
        let used = self.used_rows();
        if used >= self.spec.rows_cap {
            return None;
        }
        let rh = self.rh(used);
        let off = self.lay.cells + used * self.spec.cols * 8;
        self.buf[off..off + self.spec.cols * 8].fill(0);
        self.wr32(rh, off as u32);
        self.wr16(rh + 4, 0);
        self.wr16(rh + 6, 0);
        self.wr32(rh + 8, 0);
        self.wr64(rh + 16, 0);
        self.wr16(H_USED, (used + 1) as u16);
        Some(used)
    }

    /// Row flags.
    pub fn row_flags(&self, r: usize) -> u16 {
        self.rd16(self.rh(r) + 4)
    }

    /// Replace the row flags.
    pub fn set_row_flags(&mut self, r: usize, f: u16) {
        let o = self.rh(r) + 4;
        self.wr16(o, f);
    }

    /// Set flag bits.
    pub fn or_row_flags(&mut self, r: usize, f: u16) {
        let v = self.row_flags(r) | f;
        self.set_row_flags(r, v);
    }

    /// Clear flag bits.
    pub fn clear_row_flags(&mut self, r: usize, f: u16) {
        let v = self.row_flags(r) & !f;
        self.set_row_flags(r, v);
    }

    /// Occupancy high-water column: no cell at or beyond it is non-blank.
    pub fn row_high_water(&self, r: usize) -> usize {
        usize::from(self.rd16(self.rh(r) + 6))
    }

    fn set_high_water(&mut self, r: usize, v: usize) {
        let o = self.rh(r) + 6;
        self.wr16(o, v as u16);
    }

    /// The row's block id (T-14); 0 means none.
    pub fn row_block(&self, r: usize) -> u32 {
        self.rd32(self.rh(r) + 8)
    }

    /// Set the row's block id.
    pub fn set_row_block(&mut self, r: usize, id: u32) {
        let o = self.rh(r) + 8;
        self.wr32(o, id);
    }

    /// The row's change sequence number.
    pub fn row_seq(&self, r: usize) -> u64 {
        self.rd64(self.rh(r) + 16)
    }

    /// Set the row's change sequence number.
    pub fn set_row_seq(&mut self, r: usize, seq: u64) {
        let o = self.rh(r) + 16;
        self.wr64(o, seq);
    }

    /// Stamp `seq` and mark the row dirty.
    pub fn touch(&mut self, r: usize, seq: u64) {
        self.set_row_seq(r, seq);
        self.or_row_flags(r, rf::DIRTY);
    }

    /// Stamp `seq` on the rows at positions `a..=b` (no dirty flag).
    pub fn set_row_seqs(&mut self, a: usize, b: usize, seq: u64) {
        let (s, e) = (self.rh(a), self.rh(b + 1));
        let bytes = seq.to_le_bytes();
        for row in self.buf[s..e].chunks_exact_mut(ROW_HEADER_BYTES) {
            row[16..24].copy_from_slice(&bytes);
        }
    }

    /// Rotate the row handles `a..=b` left by `k` (content moves up). Cells do not move.
    pub fn rotate_rows_up(&mut self, a: usize, b: usize, k: usize) {
        let s = self.rh(a);
        let e = self.rh(b + 1);
        self.buf[s..e].rotate_left(k * ROW_HEADER_BYTES);
    }

    /// Rotate the row handles `a..=b` right by `k` (content moves down).
    pub fn rotate_rows_down(&mut self, a: usize, b: usize, k: usize) {
        let s = self.rh(a);
        let e = self.rh(b + 1);
        self.buf[s..e].rotate_right(k * ROW_HEADER_BYTES);
    }

    // ---- cells ---------------------------------------------------------------------

    /// The cell at `(r, c)`.
    #[inline]
    pub fn cell(&self, r: usize, c: usize) -> Cell {
        Cell(self.rd64(self.cell_off(r) + c * 8))
    }

    #[inline]
    fn set_raw(&mut self, r: usize, c: usize, cell: Cell) {
        let o = self.cell_off(r) + c * 8;
        self.wr64(o, cell.0);
    }

    /// Release the resources (style reference, cluster chunks, link) held by `(r, c)`
    /// without changing the cell word.
    fn release_cell(&mut self, r: usize, c: usize) {
        let old = self.cell(r, c);
        if old.style_id() != 0 {
            self.style_release(old.style_id());
        }
        if old.tag() == Tag::Cluster {
            self.cluster_free(old.cluster_index());
        }
        if old.has_link() {
            self.lmap_remove(r, c);
        }
    }

    /// Blank `(r, c)` and release its resources.
    fn blank(&mut self, r: usize, c: usize) {
        self.release_cell(r, c);
        self.set_raw(r, c, Cell::EMPTY);
    }

    /// Replace the cell at `(r, c)`. Releases the old cell's resources and repairs the other
    /// half of a wide character it overwrites (O(1), the row's wrap flags untouched). `link`
    /// is a link item whose reference the caller has already acquired for this cell; the
    /// caller must have called [`Page::link_reserve`] first.
    pub fn put(&mut self, r: usize, c: usize, new: Cell, link: Option<u16>) {
        let old = self.cell(r, c);
        match old.wide() {
            Wide::Wide if c + 1 < self.spec.cols => self.blank(r, c + 1),
            Wide::SpacerTail if c > 0 => self.blank(r, c - 1),
            _ => {}
        }
        self.release_cell(r, c);
        self.set_raw(r, c, new);
        if let Some(l) = link {
            self.lmap_add(r, c, l);
        }
        let mut f = 0;
        if new.style_id() != 0 {
            f |= rf::HAS_STYLE;
        }
        if new.tag() == Tag::Cluster {
            f |= rf::HAS_CLUSTER;
        }
        if link.is_some() {
            f |= rf::HAS_LINK;
        }
        if new.is_multicell() {
            f |= rf::HAS_MULTICELL;
        }
        if f != 0 {
            self.or_row_flags(r, f);
        }
        if new.tag() != Tag::Empty || new.wide() != Wide::Narrow {
            let hw = self.row_high_water(r);
            if c + 1 > hw {
                self.set_high_water(r, c + 1);
            }
        }
    }

    /// Store `n` width-1 scalars (`cps`, clonable) with the template `proto` (style, protection and
    /// semantic bits, no link) into `(r, c0..)`. The caller has checked that the run fits the
    /// row and that every glyph has width 1; the style references (one per glyph) are acquired
    /// here. Cells that hold nothing needing release or repair take a single word store; the
    /// rest go through [`Page::put`].
    pub fn put_narrow_run<I>(&mut self, r: usize, c0: usize, proto: Cell, n: usize, cps: I)
    where
        I: Iterator<Item = u32> + Clone,
    {
        if n == 0 {
            return;
        }
        self.style_acquire(proto.style_id(), n as u32);
        let base = self.cell_off(r) + c0 * 8;
        let cells = &mut self.buf[base..base + n * 8];
        let mut seen = 0u64;
        for chunk in cells.chunks_exact(8) {
            let mut raw = [0u8; 8];
            raw.copy_from_slice(chunk);
            seen |= u64::from_le_bytes(raw);
        }
        let scalar = proto.with_tag(Tag::Scalar).0;
        let valid = |cp: u32| {
            u64::from(if char::from_u32(cp).is_some() {
                cp
            } else {
                0xFFFD
            })
        };
        if seen & Cell::NOT_PLAIN == 0 {
            for (chunk, cp) in cells.chunks_exact_mut(8).zip(cps) {
                chunk.copy_from_slice(&(scalar | valid(cp)).to_le_bytes());
            }
        } else {
            for (i, cp) in cps.enumerate() {
                let c = Cell(scalar | valid(cp));
                self.put(r, c0 + i, c, None);
            }
        }
        let mut f = rf::DIRTY;
        if proto.style_id() != 0 {
            f |= rf::HAS_STYLE;
        }
        self.or_row_flags(r, f);
        if c0 + n > self.row_high_water(r) {
            self.set_high_water(r, c0 + n);
        }
    }

    /// As [`Page::put_narrow_run`] for glyphs of one or two columns, `(scalar, width)` pairs
    /// covering exactly `cells` columns from `c0`; a width-2 glyph stores its wide cell and a
    /// spacer tail.
    pub fn put_glyph_run<I>(&mut self, r: usize, c0: usize, proto: Cell, cells: usize, glyphs: I)
    where
        I: Iterator<Item = (u32, u8)> + Clone,
    {
        if cells == 0 {
            return;
        }
        self.style_acquire(proto.style_id(), cells as u32);
        let base = self.cell_off(r) + c0 * 8;
        let span = &mut self.buf[base..base + cells * 8];
        let mut seen = 0u64;
        for chunk in span.chunks_exact(8) {
            let mut raw = [0u8; 8];
            raw.copy_from_slice(chunk);
            seen |= u64::from_le_bytes(raw);
        }
        let scalar = proto.with_tag(Tag::Scalar).0;
        let valid = |cp: u32| {
            u64::from(if char::from_u32(cp).is_some() {
                cp
            } else {
                0xFFFD
            })
        };
        let wide = scalar_wide_bits(Wide::Wide);
        let tail = proto.with_wide(Wide::SpacerTail).0.to_le_bytes();
        if seen & Cell::NOT_PLAIN == 0 {
            let mut k = 0;
            for (cp, w) in glyphs {
                if w >= 2 {
                    span[k * 8..k * 8 + 8]
                        .copy_from_slice(&(scalar | wide | valid(cp)).to_le_bytes());
                    span[k * 8 + 8..k * 8 + 16].copy_from_slice(&tail);
                    k += 2;
                } else {
                    span[k * 8..k * 8 + 8].copy_from_slice(&(scalar | valid(cp)).to_le_bytes());
                    k += 1;
                }
            }
        } else {
            let mut k = 0;
            for (cp, w) in glyphs {
                if w >= 2 {
                    let c = Cell(scalar | wide | valid(cp));
                    self.put(r, c0 + k, c, None);
                    self.put(r, c0 + k + 1, proto.with_wide(Wide::SpacerTail), None);
                    k += 2;
                } else {
                    self.put(r, c0 + k, Cell(scalar | valid(cp)), None);
                    k += 1;
                }
            }
        }
        let mut f = rf::DIRTY;
        if proto.style_id() != 0 {
            f |= rf::HAS_STYLE;
        }
        self.or_row_flags(r, f);
        if c0 + cells > self.row_high_water(r) {
            self.set_high_water(r, c0 + cells);
        }
    }

    /// Fix the wide characters cut by the boundaries of an erase of `x0..x1` (O(1)).
    pub fn repair_range(&mut self, r: usize, x0: usize, x1: usize) {
        let cols = self.spec.cols;
        if x0 > 0 && x0 < cols && self.cell(r, x0).wide() == Wide::SpacerTail {
            self.blank(r, x0 - 1);
        }
        if x1 > 0 && x1 < cols && self.cell(r, x1 - 1).wide() == Wide::Wide {
            self.blank(r, x1);
        }
    }

    /// Fill `x0..x1` with `fill` (no seam repair; see [`Page::repair_range`]). Rows with no
    /// style, cluster or link flag take the fast path with no per-cell release.
    pub fn fill_range(&mut self, r: usize, x0: usize, x1: usize, fill: Cell) {
        let x1 = x1.min(self.spec.cols);
        if x0 >= x1 {
            return;
        }
        let mut heavy = self.row_flags(r) & rf::HEAVY != 0;
        let base = self.cell_off(r);
        if heavy {
            // A flagged row often has nothing to release in this span: check in one pass.
            let mut seen = 0u64;
            for chunk in self.buf[base + x0 * 8..base + x1 * 8].chunks_exact(8) {
                let mut raw = [0u8; 8];
                raw.copy_from_slice(chunk);
                seen |= u64::from_le_bytes(raw);
            }
            heavy = seen & Cell::NOT_PLAIN != 0;
        }
        if heavy {
            for c in x0..x1 {
                self.release_cell(r, c);
            }
        }
        let bytes = fill.0.to_le_bytes();
        for chunk in self.buf[base + x0 * 8..base + x1 * 8].chunks_exact_mut(8) {
            chunk.copy_from_slice(&bytes);
        }
        if fill != Cell::EMPTY && x1 > self.row_high_water(r) {
            self.set_high_water(r, x1);
        }
    }

    /// After a whole-row fill: forget the heavy flags and reset the high-water column.
    pub fn reset_row_after_clear(&mut self, r: usize, fill: Cell) {
        self.clear_row_flags(r, rf::HEAVY);
        let hw = if fill == Cell::EMPTY {
            0
        } else {
            self.spec.cols
        };
        self.set_high_water(r, hw);
    }

    /// Move the cell at `from` to `to` in row `r` without releasing it; `to` must already
    /// be blank-raw. The vacated cell becomes empty.
    fn move_cell(&mut self, r: usize, from: usize, to: usize) {
        let cell = self.cell(r, from);
        if cell.has_link() {
            let slot = self.slot(r) as u16;
            if let Some(e) = self.lmap_find(slot, from as u16) {
                self.lmap_rekey(e, to as u16);
            }
        }
        self.set_raw(r, to, cell);
        self.set_raw(r, from, Cell::EMPTY);
        if to + 1 > self.row_high_water(r) {
            self.set_high_water(r, to + 1);
        }
    }

    /// ICH: shift `x..right` right by `n`, filling the opened cells with `fill`; cells pushed
    /// past `right` are dropped.
    pub fn shift_right(&mut self, r: usize, x: usize, right: usize, n: usize, fill: Cell) {
        let right = right.min(self.spec.cols);
        if x >= right {
            return;
        }
        let n = n.min(right - x);
        for c in (right - n)..right {
            self.blank(r, c);
        }
        let mut c = right - n;
        while c > x {
            c -= 1;
            self.move_cell(r, c, c + n);
        }
        self.fill_raw(r, x, x + n, fill);
        self.fixup_seams(r, x.saturating_sub(1), right);
    }

    /// DCH: delete `n` cells at `x`, shifting `x+n..right` left and filling the freed cells
    /// at the right margin with `fill`.
    pub fn shift_left(&mut self, r: usize, x: usize, right: usize, n: usize, fill: Cell) {
        let right = right.min(self.spec.cols);
        if x >= right {
            return;
        }
        let n = n.min(right - x);
        // Cut wide characters at both edges of the deleted span before anything moves, so a
        // head whose tail is deleted cannot adopt the tail of the cell shifted into its place.
        self.repair_range(r, x, x + n);
        for c in x..x + n {
            self.blank(r, c);
        }
        for c in (x + n)..right {
            self.move_cell(r, c, c - n);
        }
        self.fill_raw(r, right - n, right, fill);
        self.fixup_seams(r, x.saturating_sub(1), right);
    }

    fn fill_raw(&mut self, r: usize, x0: usize, x1: usize, fill: Cell) {
        for c in x0..x1 {
            self.set_raw(r, c, fill);
        }
        if fill != Cell::EMPTY && x1 > self.row_high_water(r) {
            self.set_high_water(r, x1);
        }
    }

    /// Blank any half-wide character in `lo..=hi` whose partner is missing.
    pub fn fixup_seams(&mut self, r: usize, lo: usize, hi: usize) {
        let cols = self.spec.cols;
        let hi = hi.min(cols.saturating_sub(1));
        for c in lo..=hi {
            match self.cell(r, c).wide() {
                Wide::SpacerTail if c == 0 || self.cell(r, c - 1).wide() != Wide::Wide => {
                    self.blank(r, c);
                }
                Wide::Wide if c + 1 >= cols || self.cell(r, c + 1).wide() != Wide::SpacerTail => {
                    self.blank(r, c);
                }
                _ => {}
            }
        }
    }

    // ---- row positions and bulk operations (used by the screen) --------------------

    /// Rows in use.
    pub fn len(&self) -> usize {
        self.used_rows()
    }

    /// True when no row is in use.
    pub fn is_empty(&self) -> bool {
        self.used_rows() == 0
    }

    /// True when no row slot is free.
    pub fn is_full(&self) -> bool {
        self.used_rows() >= self.spec.rows_cap
    }

    /// The storage slot of the row at position `pos` (what a pin records).
    pub fn slot_at(&self, pos: usize) -> u16 {
        self.slot(pos) as u16
    }

    /// The position currently holding storage slot `slot`.
    pub fn pos_of(&self, slot: u16) -> Option<usize> {
        (0..self.used_rows()).find(|&p| self.slot(p) == usize::from(slot))
    }

    /// Swap the row handles at positions `a` and `b`; cells do not move.
    pub fn swap_positions(&mut self, a: usize, b: usize) {
        if a == b {
            return;
        }
        let (x, y) = (self.rh(a), self.rh(b));
        for i in 0..ROW_HEADER_BYTES {
            self.buf.swap(x + i, y + i);
        }
    }

    /// Erase `x0..x1` of row `r` with `fill`, repairing wide characters cut at the edges.
    /// `selective` spares DECSCA-protected cells. Stamps `seq` and marks the row dirty.
    pub fn erase_cells(
        &mut self,
        r: usize,
        x0: usize,
        x1: usize,
        fill: Cell,
        selective: bool,
        seq: u64,
    ) {
        let cols = self.spec.cols;
        let x1 = x1.min(cols);
        if x0 >= x1 {
            return;
        }
        if selective {
            for c in x0..x1 {
                if !self.cell(r, c).is_protected() {
                    self.put(r, c, fill, None);
                }
            }
        } else {
            self.repair_range(r, x0, x1);
            self.fill_range(r, x0, x1, fill);
            if x0 == 0 && x1 == cols {
                self.reset_row_after_clear(r, fill);
            }
        }
        self.touch(r, seq);
    }

    /// ICH: open `n` cells at `x`, shifting `x..=right` right; the opened cells take `fill`.
    pub fn insert_cells(
        &mut self,
        r: usize,
        x: usize,
        n: usize,
        right: usize,
        fill: Cell,
        seq: u64,
    ) {
        self.shift_right(r, x, right + 1, n, fill);
        self.touch(r, seq);
    }

    /// DCH: delete `n` cells at `x`, shifting left to column `right` inclusive; the freed
    /// cells at the margin take `fill`.
    pub fn delete_cells(
        &mut self,
        r: usize,
        x: usize,
        n: usize,
        right: usize,
        fill: Cell,
        seq: u64,
    ) {
        self.shift_left(r, x, right + 1, n, fill);
        self.touch(r, seq);
    }

    /// Acquire the resources `cell` (read from `src`, or from this page when `src` is
    /// `None` and `from` names the source cell) needs in this page, returning the cell as it
    /// must be stored and the link item. On `Err` nothing stays acquired. With `lossy`, a
    /// full table degrades the cell (style 0, no link, cluster cut to its first scalar)
    /// instead of failing.
    fn adopt(
        &mut self,
        src: Option<&Page>,
        from: (usize, usize),
        lossy: bool,
    ) -> Result<(Cell, Option<u16>), Full> {
        let cell = match src {
            Some(s) => s.cell(from.0, from.1),
            None => self.cell(from.0, from.1),
        };
        // Style.
        let mut sid = cell.style_id();
        let mut word = cell;
        if sid != 0 {
            match src {
                Some(s) => {
                    let st = s.style_get(sid);
                    match self.style_intern(&st, 1) {
                        Ok(id) => sid = id,
                        Err(w) if lossy => {
                            let _ = w;
                            sid = 0;
                        }
                        Err(w) => return Err(w),
                    }
                }
                None => self.style_acquire(sid, 1),
            }
            word = word.with_style(sid);
        }
        let undo_style = |p: &mut Page, sid: u16| p.style_release(sid);
        // Cluster.
        if cell.tag() == Tag::Cluster {
            let mut buf = ['\0'; MAX_CLUSTER_SCALARS];
            let n = match src {
                Some(s) => s.cluster_read(cell.cluster_index(), &mut buf),
                None => self.cluster_read(cell.cluster_index(), &mut buf),
            };
            match self.cluster_alloc(&buf[..n]) {
                Ok(h) => word = Cell((word.0 & !((1 << 21) - 1)) | u64::from(h)),
                Err(w) => {
                    if !lossy {
                        undo_style(self, sid);
                        return Err(w);
                    }
                    let first = if n > 0 { buf[0] } else { ' ' };
                    word = Cell::scalar(first, sid)
                        .with_wide(word.wide())
                        .with_protected(word.is_protected())
                        .with_semantic(word.semantic())
                        .with_link(word.has_link())
                        .with_multicell(word.is_multicell());
                }
            }
        }
        // Link.
        let mut link = None;
        if cell.has_link() {
            let found = match src {
                Some(s) => s.cell_link(from.0, from.1).map(|l| {
                    let (id, uri) = s.link_get(l);
                    (id.to_owned(), uri.to_owned())
                }),
                None => None,
            };
            let res: Result<u16, Full> = match (src, found) {
                (Some(_), Some((id, uri))) => match self.link_intern(&id, &uri, 1) {
                    Ok(l) => match self.link_reserve(1) {
                        Ok(()) => Ok(l),
                        Err(w) => {
                            self.link_release(l);
                            Err(w)
                        }
                    },
                    Err(LinkError::Full(w)) => Err(w),
                    Err(LinkError::Rejected) => Err(Full::Links),
                },
                (None, _) => match self.cell_link(from.0, from.1) {
                    Some(l) => match self.link_reserve(1) {
                        Ok(()) => {
                            self.link_acquire(l, 1);
                            Ok(l)
                        }
                        Err(w) => Err(w),
                    },
                    None => Err(Full::Links),
                },
                (Some(_), None) => Err(Full::Links),
            };
            match res {
                Ok(l) => link = Some(l),
                Err(w) => {
                    if !lossy {
                        undo_style(self, sid);
                        if word.tag() == Tag::Cluster {
                            self.cluster_free(word.cluster_index());
                        }
                        return Err(w);
                    }
                    word = word.with_link(false);
                }
            }
        }
        Ok((word, link))
    }

    /// Copy columns `left..=right` of row `src_pos` of `src` into row `dst_pos` of this
    /// page, interning styles, clusters and links here. A full-width copy also carries the
    /// row's flags, block id and sequence number. On `Err` the destination span may be
    /// partly written but is consistent; retry after growing. With `lossy`, never fails.
    pub fn import_row(
        &mut self,
        dst_pos: usize,
        src: &Page,
        src_pos: usize,
        left: usize,
        right: usize,
        lossy: bool,
    ) -> Result<(), Full> {
        let cols = self.spec.cols;
        let right = right.min(cols - 1).min(src.spec.cols - 1);
        if left > right {
            return Ok(());
        }
        let whole = left == 0 && right == cols - 1 && src.spec.cols == cols;
        self.repair_range(dst_pos, left, right + 1);
        self.fill_range(dst_pos, left, right + 1, Cell::EMPTY);
        if whole {
            self.reset_row_after_clear(dst_pos, Cell::EMPTY);
        }
        for c in left..=right {
            let sc = src.cell(src_pos, c);
            if sc == Cell::EMPTY {
                continue;
            }
            let (word, link) = self.adopt(Some(src), (src_pos, c), lossy)?;
            self.put(dst_pos, c, word, link);
        }
        self.fixup_seams(dst_pos, left.saturating_sub(1), right + 1);
        if whole {
            let keep = row_flags::WRAPPED
                | row_flags::WRAP_CONTINUATION
                | row_flags::PROMPT_START
                | row_flags::DIRTY;
            let f = (self.row_flags(dst_pos) & row_flags::HEAVY) | (src.row_flags(src_pos) & keep);
            self.set_row_flags(dst_pos, f);
            self.set_row_block(dst_pos, src.row_block(src_pos));
            self.set_row_seq(dst_pos, src.row_seq(src_pos));
        }
        Ok(())
    }

    /// Copy the single cell `(src_pos, sc)` of `src` to `(dst_pos, dc)` of this page,
    /// interning its style, cluster and link here. A blank source cell is skipped. `Err`
    /// and `lossy` as for [`Page::import_row`]; the destination is unchanged on `Err`.
    pub fn import_cell(
        &mut self,
        dst_pos: usize,
        dc: usize,
        src: &Page,
        src_pos: usize,
        sc: usize,
        lossy: bool,
    ) -> Result<(), Full> {
        if src.cell(src_pos, sc) == Cell::EMPTY {
            return Ok(());
        }
        let (word, link) = self.adopt(Some(src), (src_pos, sc), lossy)?;
        self.put(dst_pos, dc, word, link);
        Ok(())
    }

    /// Remove the last row, releasing everything its cells held. Only valid on a page whose
    /// row handles were never rotated (a freshly built page); used by reflow to trim blank
    /// rows. No-op on an empty page.
    pub fn pop_row(&mut self) {
        let used = self.used_rows();
        if used == 0 {
            return;
        }
        let r = used - 1;
        for c in 0..self.spec.cols {
            if self.cell(r, c) != Cell::EMPTY {
                self.blank(r, c);
            }
        }
        self.set_row_flags(r, 0);
        self.wr16(H_USED, r as u16);
    }

    /// Copy columns `left..=right` of row `src_pos` over the same columns of row `dst_pos`
    /// (two different rows of this page). `Err` and `lossy` as for [`Page::import_row`].
    pub fn move_cells_within(
        &mut self,
        src_pos: usize,
        dst_pos: usize,
        left: usize,
        right: usize,
        seq: u64,
        lossy: bool,
    ) -> Result<(), Full> {
        let right = right.min(self.spec.cols - 1);
        if left > right || src_pos == dst_pos {
            return Ok(());
        }
        self.repair_range(dst_pos, left, right + 1);
        self.fill_range(dst_pos, left, right + 1, Cell::EMPTY);
        for c in left..=right {
            if self.cell(src_pos, c) == Cell::EMPTY {
                continue;
            }
            let (word, link) = self.adopt(None, (src_pos, c), lossy)?;
            self.put(dst_pos, c, word, link);
        }
        self.fixup_seams(dst_pos, left.saturating_sub(1), right + 1);
        self.touch(dst_pos, seq);
        Ok(())
    }

    /// Turn the scalar cell at `(r, c)` into a one-or-more scalar cluster `scalars`
    /// (keeping its style, link and flags).
    pub fn make_cluster(&mut self, r: usize, c: usize, scalars: &[char]) -> Result<(), Full> {
        let h = self.cluster_alloc(scalars)?;
        let old = self.cell(r, c);
        let new = Cell(
            (old.0 & !(((1 << 21) - 1) | (3 << 21))) | u64::from(h) | ((Tag::Cluster as u64) << 21),
        );
        self.set_raw(r, c, new);
        self.or_row_flags(r, rf::HAS_CLUSTER);
        Ok(())
    }

    /// Append `ch` to the cluster whose first chunk is `head`. `Ok(false)` at the 64-scalar
    /// cap.
    pub fn cluster_append(&mut self, head: u32, ch: char) -> Result<bool, Full> {
        let ch = if ch == '\0' { '\u{FFFD}' } else { ch };
        let mut idx = head as usize;
        let mut count = 0usize;
        loop {
            let o = self.ck(idx);
            for j in 0..4 {
                if self.rd32(o + j * 4) == 0 {
                    if count >= MAX_CLUSTER_SCALARS {
                        return Ok(false);
                    }
                    self.wr32(o + j * 4, ch as u32);
                    return Ok(true);
                }
                count += 1;
            }
            let next = self.rd32(o + 16) as usize;
            if next == 0 {
                if count >= MAX_CLUSTER_SCALARS {
                    return Ok(false);
                }
                let bump = self.rd32(H_CHUNK_BUMP) as usize;
                if self.spec.chunk_cap - bump + self.rd32(H_CHUNK_FREE_COUNT) as usize == 0 {
                    return Err(Full::Chunks);
                }
                let n = self.chunk_take();
                let no = self.ck(n);
                self.wr32(no, ch as u32);
                for j in 1..4 {
                    self.wr32(no + j * 4, 0);
                }
                self.wr32(no + 16, 0);
                self.wr32(o + 16, (n + 1) as u32);
                return Ok(true);
            }
            idx = next - 1;
        }
    }

    // ---- styles --------------------------------------------------------------------

    #[inline]
    fn si(&self, id: usize) -> usize {
        self.lay.styles + id * STYLE_ITEM_BYTES
    }

    /// The style with page-local id `id` (0 is the default style).
    pub fn style_get(&self, id: u16) -> Style {
        if id == 0 {
            return Style::DEFAULT;
        }
        let o = self.si(usize::from(id));
        Style::decode(&self.buf[o..o + 16])
    }

    /// Cells currently referencing style `id`.
    pub fn style_refs(&self, id: u16) -> u32 {
        self.rd32(self.si(usize::from(id)) + 16)
    }

    /// Style items in use or reclaimable (excluding the default).
    pub fn style_items(&self) -> usize {
        usize::from(self.rd16(H_STYLE_USED)) - 1
    }

    fn style_hash(enc: &[u8; 16]) -> usize {
        let (mut lo, mut hi) = ([0u8; 8], [0u8; 8]);
        lo.copy_from_slice(&enc[..8]);
        hi.copy_from_slice(&enc[8..]);
        let mut h = (u64::from_le_bytes(lo) ^ u64::from_le_bytes(hi).rotate_left(29))
            .wrapping_mul(0x9E37_79B9_7F4A_7C15);
        h ^= h >> 32;
        h = h.wrapping_mul(0xD6E8_FEB8_6659_FD93);
        (h ^ (h >> 29)) as usize
    }

    fn index_insert(&mut self, id: u16, enc: &[u8; 16]) {
        let mask = self.lay.index_slots - 1;
        let mut i = Self::style_hash(enc) & mask;
        loop {
            let o = self.lay.sindex + i * 2;
            if self.rd16(o) == 0 {
                self.wr16(o, id);
                return;
            }
            i = (i + 1) & mask;
        }
    }

    fn rebuild_index(&mut self) {
        let (s, n) = (self.lay.sindex, self.lay.index_slots * 2);
        self.buf[s..s + n].fill(0);
        let used = usize::from(self.rd16(H_STYLE_USED));
        for id in 1..used {
            let o = self.si(id);
            if self.rd32(o + 20) == 1 {
                let mut enc = [0u8; 16];
                enc.copy_from_slice(&self.buf[o..o + 16]);
                self.index_insert(id as u16, &enc);
            }
        }
    }

    /// Reclaim dead (zero-reference) style items; true when any item became free.
    fn style_reclaim(&mut self) -> bool {
        let used = usize::from(self.rd16(H_STYLE_USED));
        let mut any = false;
        for id in 1..used {
            let o = self.si(id);
            if self.rd32(o + 20) == 1 && self.rd32(o + 16) == 0 {
                self.wr32(o + 20, 0);
                any = true;
            }
        }
        if any {
            self.last_style = None;
            self.rebuild_index();
        }
        any
    }

    fn style_alloc_item(&mut self) -> Result<usize, Full> {
        let used = usize::from(self.rd16(H_STYLE_USED));
        if used < self.spec.style_cap {
            self.wr16(H_STYLE_USED, (used + 1) as u16);
            return Ok(used);
        }
        self.style_reclaim();
        for id in 1..used {
            if self.rd32(self.si(id) + 20) == 0 {
                return Ok(id);
            }
        }
        Err(Full::Styles)
    }

    /// Intern `st` and add `add` references to it. The default style is id 0 and takes none.
    pub fn style_intern(&mut self, st: &Style, add: u32) -> Result<u16, Full> {
        if st.is_default() {
            return Ok(0);
        }
        if let Some((last, id)) = self.last_style
            && last == *st
        {
            let o = self.si(usize::from(id)) + 16;
            let refs = self.rd32(o);
            self.wr32(o, refs + add);
            return Ok(id);
        }
        let enc = st.encode();
        let mask = self.lay.index_slots - 1;
        let mut i = Self::style_hash(&enc) & mask;
        loop {
            let id = usize::from(self.rd16(self.lay.sindex + i * 2));
            if id == 0 {
                break;
            }
            let o = self.si(id);
            if self.buf[o..o + 16] == enc {
                let refs = self.rd32(o + 16);
                self.wr32(o + 16, refs + add);
                self.last_style = Some((*st, id as u16));
                return Ok(id as u16);
            }
            i = (i + 1) & mask;
        }
        let id = self.style_alloc_item()?;
        let o = self.si(id);
        self.buf[o..o + 16].copy_from_slice(&enc);
        self.wr32(o + 16, add);
        self.wr32(o + 20, 1);
        self.index_insert(id as u16, &enc);
        self.last_style = Some((*st, id as u16));
        Ok(id as u16)
    }

    /// Add `n` references to style `id`.
    pub fn style_acquire(&mut self, id: u16, n: u32) {
        if id != 0 {
            let o = self.si(usize::from(id)) + 16;
            let v = self.rd32(o);
            self.wr32(o, v + n);
        }
    }

    /// Drop one reference to style `id`. A style at zero references is reclaimed lazily.
    pub fn style_release(&mut self, id: u16) {
        if id != 0 {
            let o = self.si(usize::from(id)) + 16;
            let v = self.rd32(o);
            debug_assert!(v > 0, "style refcount underflow");
            self.wr32(o, v.saturating_sub(1));
        }
    }

    // ---- clusters ------------------------------------------------------------------

    #[inline]
    fn ck(&self, idx: usize) -> usize {
        self.lay.chunks + idx * CHUNK_BYTES
    }

    fn chunk_take(&mut self) -> usize {
        let fh = self.rd32(H_CHUNK_FREE) as usize;
        if fh != 0 {
            let idx = fh - 1;
            let next = self.rd32(self.ck(idx) + 16);
            self.wr32(H_CHUNK_FREE, next);
            let n = self.rd32(H_CHUNK_FREE_COUNT);
            self.wr32(H_CHUNK_FREE_COUNT, n - 1);
            idx
        } else {
            let b = self.rd32(H_CHUNK_BUMP);
            self.wr32(H_CHUNK_BUMP, b + 1);
            b as usize
        }
    }

    /// Store `scalars` (at most 64; empty is rejected) in the arena, returning the first
    /// chunk index.
    pub fn cluster_alloc(&mut self, scalars: &[char]) -> Result<u32, Full> {
        let n = scalars.len().min(MAX_CLUSTER_SCALARS);
        if n == 0 {
            return Err(Full::Chunks);
        }
        let need = n.div_ceil(4);
        let bump = self.rd32(H_CHUNK_BUMP) as usize;
        let avail = (self.spec.chunk_cap - bump) + self.rd32(H_CHUNK_FREE_COUNT) as usize;
        if avail < need {
            return Err(Full::Chunks);
        }
        let mut head = 0usize;
        let mut prev: Option<usize> = None;
        for k in 0..need {
            let idx = self.chunk_take();
            let o = self.ck(idx);
            for j in 0..4 {
                let v = scalars
                    .get(k * 4 + j)
                    .filter(|_| k * 4 + j < n)
                    .map_or(0, |&c| c as u32);
                self.wr32(o + j * 4, v);
            }
            self.wr32(o + 16, 0);
            match prev {
                Some(p) => {
                    let po = self.ck(p) + 16;
                    self.wr32(po, (idx + 1) as u32);
                }
                None => head = idx,
            }
            prev = Some(idx);
        }
        Ok(head as u32)
    }

    /// Return a cluster's chunks to the free list.
    pub fn cluster_free(&mut self, head: u32) {
        let mut idx = head as usize;
        loop {
            let o = self.ck(idx);
            let next = self.rd32(o + 16) as usize;
            for j in 0..4 {
                self.wr32(o + j * 4, 0);
            }
            let fh = self.rd32(H_CHUNK_FREE);
            self.wr32(o + 16, fh);
            self.wr32(H_CHUNK_FREE, (idx + 1) as u32);
            let n = self.rd32(H_CHUNK_FREE_COUNT);
            self.wr32(H_CHUNK_FREE_COUNT, n + 1);
            if next == 0 {
                break;
            }
            idx = next - 1;
        }
    }

    /// Copy a cluster's scalars into `out`, returning how many were written.
    pub fn cluster_read(&self, head: u32, out: &mut [char]) -> usize {
        let mut idx = head as usize;
        let mut n = 0;
        loop {
            let o = self.ck(idx);
            for j in 0..4 {
                let v = self.rd32(o + j * 4);
                if v == 0 || n >= out.len() {
                    return n;
                }
                out[n] = char::from_u32(v).unwrap_or('\u{FFFD}');
                n += 1;
            }
            let next = self.rd32(o + 16) as usize;
            if next == 0 {
                return n;
            }
            idx = next - 1;
        }
    }

    // ---- hyperlinks ----------------------------------------------------------------

    #[inline]
    fn lk(&self, i: usize) -> usize {
        self.lay.links + i * LINK_ITEM_BYTES
    }

    #[inline]
    fn lm(&self, i: usize) -> usize {
        self.lay.lmap + i * LMAP_ENTRY_BYTES
    }

    fn link_str(&self, off: usize, len: usize) -> &str {
        let s = self.lay.strs + off;
        core::str::from_utf8(&self.buf[s..s + len]).unwrap_or("")
    }

    /// The `(id, uri)` of link item `i`.
    pub fn link_get(&self, i: u16) -> (&str, &str) {
        let o = self.lk(usize::from(i));
        let uri_off = self.rd32(o + 4) as usize;
        let id_off = self.rd32(o + 8) as usize;
        let uri_len = usize::from(self.rd16(o + 12));
        let id_len = usize::from(self.rd16(o + 14));
        (
            self.link_str(id_off, id_len),
            self.link_str(uri_off, uri_len),
        )
    }

    /// Cells currently referencing link item `i`.
    pub fn link_refs(&self, i: u16) -> u32 {
        self.rd32(self.lk(usize::from(i)))
    }

    /// Link items allocated.
    pub fn link_items(&self) -> usize {
        usize::from(self.rd16(H_LINK_USED))
    }

    fn link_hash(id: &str, uri: &str) -> u32 {
        let mut h: u32 = 0x811c_9dc5;
        for &b in id.as_bytes() {
            h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
        }
        h = (h ^ 0xff).wrapping_mul(0x0100_0193);
        for &b in uri.as_bytes() {
            h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
        }
        h ^ (h >> 15)
    }

    fn rebuild_link_index(&mut self) {
        let (s, n) = (self.lay.lkidx, self.lay.lkidx_slots * 2);
        self.buf[s..s + n].fill(0);
        let mask = self.lay.lkidx_slots - 1;
        for i in 0..self.link_items() {
            let (id, uri) = self.link_get(i as u16);
            let mut k = Self::link_hash(id, uri) as usize & mask;
            while self.rd16(self.lay.lkidx + k * 2) != 0 {
                k = (k + 1) & mask;
            }
            let o = self.lay.lkidx + k * 2;
            self.wr16(o, (i + 1) as u16);
        }
    }

    /// Intern `(id, uri)` and add `add` references. Items are never reclaimed before the
    /// page. Lookup is a hash probe, not a scan.
    pub fn link_intern(&mut self, id: &str, uri: &str, add: u32) -> Result<u16, LinkError> {
        if uri.is_empty() || uri.len() > MAX_URI || id.len() > MAX_LINK_ID {
            return Err(LinkError::Rejected);
        }
        let used = self.link_items();
        let mask = self.lay.lkidx_slots - 1;
        let mut k = Self::link_hash(id, uri) as usize & mask;
        loop {
            let v = usize::from(self.rd16(self.lay.lkidx + k * 2));
            if v == 0 {
                break;
            }
            let i = v - 1;
            let (cid, curi) = self.link_get(i as u16);
            if cid == id && curi == uri {
                let o = self.lk(i);
                let r = self.rd32(o);
                self.wr32(o, r + add);
                return Ok(i as u16);
            }
            k = (k + 1) & mask;
        }
        if used >= self.spec.link_cap {
            return Err(LinkError::Full(Full::Links));
        }
        let su = self.rd32(H_STR_USED) as usize;
        if su + id.len() + uri.len() > self.spec.str_cap {
            return Err(LinkError::Full(Full::Strings));
        }
        let base = self.lay.strs + su;
        self.buf[base..base + uri.len()].copy_from_slice(uri.as_bytes());
        self.buf[base + uri.len()..base + uri.len() + id.len()].copy_from_slice(id.as_bytes());
        self.wr32(H_STR_USED, (su + uri.len() + id.len()) as u32);
        let o = self.lk(used);
        self.wr32(o, add);
        self.wr32(o + 4, su as u32);
        self.wr32(o + 8, (su + uri.len()) as u32);
        self.wr16(o + 12, uri.len() as u16);
        self.wr16(o + 14, id.len() as u16);
        self.wr16(H_LINK_USED, (used + 1) as u16);
        let io = self.lay.lkidx + k * 2;
        self.wr16(io, (used + 1) as u16);
        Ok(used as u16)
    }

    /// Add `n` references to link item `i`.
    pub fn link_acquire(&mut self, i: u16, n: u32) {
        let o = self.lk(usize::from(i));
        let v = self.rd32(o);
        self.wr32(o, v + n);
    }

    fn link_release(&mut self, i: u16) {
        let o = self.lk(usize::from(i));
        let v = self.rd32(o);
        debug_assert!(v > 0, "link refcount underflow");
        self.wr32(o, v.saturating_sub(1));
    }

    /// Link-map entries still free.
    pub fn lmap_free(&self) -> usize {
        self.spec.lmap_cap - self.rd32(H_LMAP_LEN) as usize
    }

    /// Ensure room for `n` more link-map entries.
    pub fn link_reserve(&self, n: usize) -> Result<(), Full> {
        if self.lmap_free() >= n {
            Ok(())
        } else {
            Err(Full::LinkMap)
        }
    }

    // The link map is an unordered array of `(slot u16, col u16, link u16)` entries with an
    // open-addressed hash index over `(slot, col)`: every lookup, move and removal is O(1)
    // however many cells of the page carry links.

    #[inline]
    fn lidx_home(&self, slot: u16, col: u16) -> usize {
        let key = (u32::from(slot) << 16) | u32::from(col);
        let bits = self.lay.lidx_slots.trailing_zeros();
        (key.wrapping_mul(0x9E37_79B1) >> (32 - bits)) as usize
    }

    fn lmap_find(&self, slot: u16, col: u16) -> Option<usize> {
        let mask = self.lay.lidx_slots - 1;
        let mut i = self.lidx_home(slot, col);
        loop {
            let v = self.rd32(self.lay.lidx + i * 4) as usize;
            if v == 0 {
                return None;
            }
            let o = self.lm(v - 1);
            if self.rd16(o) == slot && self.rd16(o + 2) == col {
                return Some(v - 1);
            }
            i = (i + 1) & mask;
        }
    }

    fn lidx_insert(&mut self, e: usize) {
        let o = self.lm(e);
        let (slot, col) = (self.rd16(o), self.rd16(o + 2));
        let mask = self.lay.lidx_slots - 1;
        let mut i = self.lidx_home(slot, col);
        while self.rd32(self.lay.lidx + i * 4) != 0 {
            i = (i + 1) & mask;
        }
        let io = self.lay.lidx + i * 4;
        self.wr32(io, (e + 1) as u32);
    }

    /// Index position holding entry `e` (the entry's key must still be current).
    fn lidx_pos_of(&self, e: usize) -> usize {
        let o = self.lm(e);
        let (slot, col) = (self.rd16(o), self.rd16(o + 2));
        let mask = self.lay.lidx_slots - 1;
        let mut i = self.lidx_home(slot, col);
        loop {
            if self.rd32(self.lay.lidx + i * 4) as usize == e + 1 {
                return i;
            }
            i = (i + 1) & mask;
        }
    }

    /// Remove entry `e` from the index (backward-shift deletion keeps probes contiguous).
    fn lidx_remove(&mut self, e: usize) {
        let mask = self.lay.lidx_slots - 1;
        let mut hole = self.lidx_pos_of(e);
        let io = self.lay.lidx + hole * 4;
        self.wr32(io, 0);
        let mut j = hole;
        loop {
            j = (j + 1) & mask;
            let v = self.rd32(self.lay.lidx + j * 4) as usize;
            if v == 0 {
                break;
            }
            let o = self.lm(v - 1);
            let k = self.lidx_home(self.rd16(o), self.rd16(o + 2));
            let in_range = if hole <= j {
                hole < k && k <= j
            } else {
                hole < k || k <= j
            };
            if !in_range {
                let ho = self.lay.lidx + hole * 4;
                self.wr32(ho, v as u32);
                let jo = self.lay.lidx + j * 4;
                self.wr32(jo, 0);
                hole = j;
            }
        }
    }

    fn rebuild_lidx(&mut self) {
        let (s, n) = (self.lay.lidx, self.lay.lidx_slots * 4);
        self.buf[s..s + n].fill(0);
        for e in 0..self.rd32(H_LMAP_LEN) as usize {
            self.lidx_insert(e);
        }
    }

    fn lmap_rekey(&mut self, e: usize, col: u16) {
        self.lidx_remove(e);
        let o = self.lm(e) + 2;
        self.wr16(o, col);
        self.lidx_insert(e);
    }

    fn lmap_add(&mut self, r: usize, c: usize, link: u16) {
        let len = self.rd32(H_LMAP_LEN) as usize;
        debug_assert!(
            len < self.spec.lmap_cap,
            "link map overflow: call link_reserve first"
        );
        let slot = self.slot(r) as u16;
        let o = self.lm(len);
        self.wr16(o, slot);
        self.wr16(o + 2, c as u16);
        self.wr16(o + 4, link);
        self.wr32(H_LMAP_LEN, (len + 1) as u32);
        self.lidx_insert(len);
    }

    fn lmap_remove(&mut self, r: usize, c: usize) {
        let slot = self.slot(r) as u16;
        let Some(e) = self.lmap_find(slot, c as u16) else {
            return;
        };
        let o = self.lm(e);
        let link = self.rd16(o + 4);
        let len = self.rd32(H_LMAP_LEN) as usize;
        let last = len - 1;
        self.lidx_remove(e);
        if e != last {
            // Move the last entry into the hole and repoint its index slot.
            let lo = self.lm(last);
            let pos = self.lidx_pos_of(last);
            let (a, b) = (self.rd32(lo), self.rd32(lo + 4));
            self.wr32(o, a);
            self.wr32(o + 4, b);
            let io = self.lay.lidx + pos * 4;
            self.wr32(io, (e + 1) as u32);
        }
        self.wr32(H_LMAP_LEN, last as u32);
        self.link_release(link);
    }

    /// The link item attached to the cell at `(r, c)`, if any.
    pub fn cell_link(&self, r: usize, c: usize) -> Option<u16> {
        if !self.cell(r, c).has_link() {
            return None;
        }
        let slot = self.slot(r) as u16;
        self.lmap_find(slot, c as u16)
            .map(|i| self.rd16(self.lm(i) + 4))
    }

    // ---- growth and audit ----------------------------------------------------------

    /// Copy into a page with a larger table of the kind that ran out. `None` at the ceiling.
    pub fn grown(&self, which: Full) -> Option<Page> {
        self.grown_with(which, &Ceil::MAX)
    }

    /// [`Page::grown`] with explicit ceilings.
    pub fn grown_with(&self, which: Full, ceil: &Ceil) -> Option<Page> {
        let mut spec = self.spec;
        let up = |v: usize, max: usize| (v * 2).min(max).max(v);
        match which {
            Full::Styles => spec.style_cap = up(spec.style_cap, ceil.styles.min(MAX_STYLE_CAP)),
            Full::Chunks => spec.chunk_cap = up(spec.chunk_cap, ceil.chunks.min(MAX_CHUNK_CAP)),
            Full::Links => spec.link_cap = up(spec.link_cap, ceil.links.min(MAX_LINK_CAP)),
            Full::LinkMap => spec.lmap_cap = up(spec.lmap_cap, ceil.lmap.min(MAX_LMAP_CAP)),
            Full::Strings => spec.str_cap = up(spec.str_cap, ceil.strs.min(MAX_STR_CAP)),
        }
        if spec == self.spec {
            return None;
        }
        let mut n = Page::new(spec, self.serial());
        n.buf[..HEADER_BYTES].copy_from_slice(&self.buf[..HEADER_BYTES]);
        // Re-stamp the capacities that changed.
        n.wr16(H_STYLE_CAP, spec.style_cap as u16);
        n.wr32(H_CHUNK_CAP, spec.chunk_cap as u32);
        n.wr16(H_LINK_CAP, spec.link_cap as u16);
        n.wr32(H_LMAP_CAP, spec.lmap_cap as u32);
        n.wr32(H_STR_CAP, spec.str_cap as u32);
        let (a, b) = (self.lay, n.lay);
        let rows_bytes = align8(self.spec.rows_cap * ROW_HEADER_BYTES);
        n.buf[b.rows..b.rows + rows_bytes].copy_from_slice(&self.buf[a.rows..a.rows + rows_bytes]);
        let cells_bytes = self.spec.rows_cap * self.spec.cols * 8;
        n.buf[b.cells..b.cells + cells_bytes]
            .copy_from_slice(&self.buf[a.cells..a.cells + cells_bytes]);
        let sb = self.spec.style_cap * STYLE_ITEM_BYTES;
        n.buf[b.styles..b.styles + sb].copy_from_slice(&self.buf[a.styles..a.styles + sb]);
        n.rebuild_index();
        let cb = self.spec.chunk_cap * CHUNK_BYTES;
        n.buf[b.chunks..b.chunks + cb].copy_from_slice(&self.buf[a.chunks..a.chunks + cb]);
        let lb = self.spec.link_cap * LINK_ITEM_BYTES;
        n.buf[b.links..b.links + lb].copy_from_slice(&self.buf[a.links..a.links + lb]);
        let mb = self.spec.lmap_cap * LMAP_ENTRY_BYTES;
        n.buf[b.lmap..b.lmap + mb].copy_from_slice(&self.buf[a.lmap..a.lmap + mb]);
        n.buf[b.strs..b.strs + self.spec.str_cap]
            .copy_from_slice(&self.buf[a.strs..a.strs + self.spec.str_cap]);
        n.rebuild_link_index();
        n.rebuild_lidx();
        Some(n)
    }

    /// Grow the table named by `which` in place: the row headers and cells (the bulk of the
    /// page) stay where they are, only the side tables behind them move. Same result as
    /// [`Page::grown_with`], without copying every cell. False when the ceiling is reached.
    pub fn grow_in_place(&mut self, which: Full, ceil: &Ceil) -> bool {
        let mut spec = self.spec;
        let up = |v: usize, max: usize| (v * 2).min(max).max(v);
        match which {
            Full::Styles => spec.style_cap = up(spec.style_cap, ceil.styles.min(MAX_STYLE_CAP)),
            Full::Chunks => spec.chunk_cap = up(spec.chunk_cap, ceil.chunks.min(MAX_CHUNK_CAP)),
            Full::Links => spec.link_cap = up(spec.link_cap, ceil.links.min(MAX_LINK_CAP)),
            Full::LinkMap => spec.lmap_cap = up(spec.lmap_cap, ceil.lmap.min(MAX_LMAP_CAP)),
            Full::Strings => spec.str_cap = up(spec.str_cap, ceil.strs.min(MAX_STR_CAP)),
        }
        if spec == self.spec {
            return false;
        }
        let (a, b) = (self.lay, Layout::of(&spec));
        self.buf.resize(b.total, 0);
        // Content regions (old offset, new offset, old length), highest first so a move never
        // lands on a region still waiting to move. The three hash indexes are rebuilt.
        let moves = [
            (a.strs, b.strs, self.spec.str_cap),
            (a.lmap, b.lmap, self.spec.lmap_cap * LMAP_ENTRY_BYTES),
            (a.links, b.links, self.spec.link_cap * LINK_ITEM_BYTES),
            (a.chunks, b.chunks, self.spec.chunk_cap * CHUNK_BYTES),
            (a.styles, b.styles, self.spec.style_cap * STYLE_ITEM_BYTES),
        ];
        for (from, to, len) in moves {
            if from != to {
                self.buf.copy_within(from..from + len, to);
            }
        }
        // Zero everything behind the cells that is not moved content (old bytes may remain
        // in the gaps and in the index regions).
        let mut keep: Vec<(usize, usize)> = moves.iter().map(|&(_, to, len)| (to, len)).collect();
        keep.sort_unstable();
        let mut at = b.styles;
        for (to, len) in keep {
            if to > at {
                self.buf[at..to].fill(0);
            }
            at = at.max(to + len);
        }
        self.buf[at..b.total].fill(0);
        self.spec = spec;
        self.lay = b;
        self.wr16(H_STYLE_CAP, spec.style_cap as u16);
        self.wr32(H_CHUNK_CAP, spec.chunk_cap as u32);
        self.wr16(H_LINK_CAP, spec.link_cap as u16);
        self.wr32(H_LMAP_CAP, spec.lmap_cap as u32);
        self.wr32(H_STR_CAP, spec.str_cap as u32);
        self.rebuild_index();
        self.rebuild_link_index();
        self.rebuild_lidx();
        true
    }

    /// Check every internal invariant: reference counts equal cell counts, no leaked or
    /// double-owned cluster chunks, link map matches the hyperlink bits, and row flags are
    /// never false-negative.
    pub fn audit(&self) -> Result<(), String> {
        let cols = self.spec.cols;
        let style_used = usize::from(self.rd16(H_STYLE_USED));
        let mut style_refs = vec![0u32; style_used];
        let mut link_refs = vec![0u32; self.link_items()];
        let mut chunk_owned = vec![false; self.rd32(H_CHUNK_BUMP) as usize];
        let mut link_cells = 0usize;
        for r in 0..self.used_rows() {
            let flags = self.row_flags(r);
            for c in 0..cols {
                let cell = self.cell(r, c);
                let id = usize::from(cell.style_id());
                if id != 0 {
                    if id >= style_used {
                        return Err(format!("row {r} col {c}: style id {id} out of range"));
                    }
                    style_refs[id] += 1;
                    if flags & rf::HAS_STYLE == 0 {
                        return Err(format!("row {r}: HAS_STYLE false-negative"));
                    }
                }
                if cell.tag() == Tag::Cluster {
                    if flags & rf::HAS_CLUSTER == 0 {
                        return Err(format!("row {r}: HAS_CLUSTER false-negative"));
                    }
                    let mut idx = cell.cluster_index() as usize;
                    loop {
                        if idx >= chunk_owned.len() || chunk_owned[idx] {
                            return Err(format!(
                                "row {r} col {c}: cluster chunk {idx} bad or shared"
                            ));
                        }
                        chunk_owned[idx] = true;
                        let next = self.rd32(self.ck(idx) + 16) as usize;
                        if next == 0 {
                            break;
                        }
                        idx = next - 1;
                    }
                }
                if cell.has_link() {
                    link_cells += 1;
                    if flags & rf::HAS_LINK == 0 {
                        return Err(format!("row {r}: HAS_LINK false-negative"));
                    }
                    match self.cell_link(r, c) {
                        Some(l) => link_refs[usize::from(l)] += 1,
                        None => return Err(format!("row {r} col {c}: link bit without map entry")),
                    }
                }
                if cell.is_multicell() && flags & rf::HAS_MULTICELL == 0 {
                    return Err(format!("row {r}: HAS_MULTICELL false-negative"));
                }
                if cell.tag() != Tag::Empty && c >= self.row_high_water(r) {
                    return Err(format!("row {r} col {c}: beyond high-water column"));
                }
            }
        }
        for (id, &n) in style_refs.iter().enumerate().skip(1) {
            if self.rd32(self.si(id) + 16) != n {
                return Err(format!(
                    "style {id}: refcount {} != cells {n}",
                    self.rd32(self.si(id) + 16)
                ));
            }
        }
        for (i, &n) in link_refs.iter().enumerate() {
            if self.link_refs(i as u16) != n {
                return Err(format!(
                    "link {i}: refcount {} != cells {n}",
                    self.link_refs(i as u16)
                ));
            }
        }
        let map_len = self.rd32(H_LMAP_LEN) as usize;
        if map_len != link_cells {
            return Err(format!(
                "link map has {map_len} entries for {link_cells} linked cells"
            ));
        }
        let owned = chunk_owned.iter().filter(|&&b| b).count();
        let free = self.rd32(H_CHUNK_FREE_COUNT) as usize;
        if owned + free != chunk_owned.len() {
            return Err(format!(
                "chunks: {owned} owned + {free} free != {} allocated",
                chunk_owned.len()
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::{Attrs, Color};

    fn page() -> Page {
        let mut p = Page::new(PageSpec::with_rows(10, 4), 1);
        for _ in 0..4 {
            p.push_row();
        }
        p
    }

    fn bold() -> Style {
        Style {
            attrs: Attrs(Attrs::BOLD),
            ..Style::DEFAULT
        }
    }

    #[test]
    fn put_and_read() {
        let mut p = page();
        p.put(1, 3, Cell::scalar('x', 0), None);
        assert_eq!(p.cell(1, 3).scalar_char(), Some('x'));
        assert_eq!(p.row_high_water(1), 4);
        p.audit().unwrap();
    }

    #[test]
    fn style_refcounts() {
        let mut p = page();
        let id = p.style_intern(&bold(), 2).unwrap();
        assert_eq!(p.style_refs(id), 2);
        p.put(0, 0, Cell::scalar('a', id), None);
        p.put(0, 1, Cell::scalar('b', id), None);
        p.audit().unwrap();
        p.put(0, 0, Cell::scalar('c', 0), None);
        assert_eq!(p.style_refs(id), 1);
        let again = p.style_intern(&bold(), 1).unwrap();
        assert_eq!(again, id);
        p.put(0, 2, Cell::scalar('d', again), None);
        p.audit().unwrap();
        assert_eq!(p.style_get(id), bold());
    }

    #[test]
    fn style_table_reclaims_dead_items() {
        let mut spec = PageSpec::with_rows(4, 2);
        spec.style_cap = 4;
        let mut p = Page::new(spec, 1);
        p.push_row();
        for n in 0..50u8 {
            let st = Style {
                fg: Color::Palette(n),
                ..Style::DEFAULT
            };
            let id = p.style_intern(&st, 1).unwrap();
            p.put(0, 0, Cell::scalar('a', id), None);
            p.audit().unwrap();
        }
        assert!(p.style_items() <= 3);
    }

    #[test]
    fn style_table_full_when_all_live_then_grows() {
        let mut spec = PageSpec::with_rows(8, 2);
        spec.style_cap = 4;
        let mut p = Page::new(spec, 1);
        p.push_row();
        for n in 0..3u8 {
            let st = Style {
                fg: Color::Palette(n),
                ..Style::DEFAULT
            };
            let id = p.style_intern(&st, 1).unwrap();
            p.put(0, usize::from(n), Cell::scalar('a', id), None);
        }
        let st = Style {
            fg: Color::Palette(99),
            ..Style::DEFAULT
        };
        assert_eq!(p.style_intern(&st, 1), Err(Full::Styles));
        let mut g = p.grown(Full::Styles).unwrap();
        let id = g.style_intern(&st, 1).unwrap();
        g.put(0, 5, Cell::scalar('z', id), None);
        g.audit().unwrap();
        assert_eq!(g.cell(0, 0).scalar_char(), Some('a'));
        assert_eq!(
            g.style_get(g.cell(0, 2).style_id()),
            Style {
                fg: Color::Palette(2),
                ..Style::DEFAULT
            }
        );
    }

    #[test]
    fn clusters_alloc_free_reuse() {
        let mut p = page();
        let s: Vec<char> = "e\u{301}\u{302}\u{303}\u{304}\u{305}".chars().collect();
        let h = p.cluster_alloc(&s).unwrap();
        p.put(0, 0, Cell::cluster(h, 0), None);
        let mut out = ['\0'; 64];
        let n = p.cluster_read(h, &mut out);
        assert_eq!(&out[..n], &s[..]);
        p.audit().unwrap();
        p.put(0, 0, Cell::EMPTY, None);
        p.audit().unwrap();
        let h2 = p.cluster_alloc(&s).unwrap();
        p.put(0, 1, Cell::cluster(h2, 0), None);
        p.audit().unwrap();
    }

    #[test]
    fn cluster_cap_is_64() {
        let mut p = page();
        let s = vec!['a'; 100];
        let h = p.cluster_alloc(&s).unwrap();
        let mut out = ['\0'; 128];
        assert_eq!(p.cluster_read(h, &mut out), 64);
    }

    #[test]
    fn links_intern_and_map() {
        let mut p = page();
        let l = p.link_intern("id1", "https://example.org/", 2).unwrap();
        p.link_reserve(2).unwrap();
        p.put(0, 0, Cell::scalar('a', 0).with_link(true), Some(l));
        p.put(0, 1, Cell::scalar('b', 0).with_link(true), Some(l));
        assert_eq!(p.cell_link(0, 1), Some(l));
        assert_eq!(p.link_get(l), ("id1", "https://example.org/"));
        p.audit().unwrap();
        p.put(0, 0, Cell::scalar('c', 0), None);
        assert_eq!(p.link_refs(l), 1);
        p.audit().unwrap();
        assert_eq!(
            p.link_intern("", &"u".repeat(MAX_URI + 1), 1),
            Err(LinkError::Rejected)
        );
        assert_eq!(
            p.link_intern(&"i".repeat(MAX_LINK_ID + 1), "u", 1),
            Err(LinkError::Rejected)
        );
        assert_eq!(p.link_intern("", "", 1), Err(LinkError::Rejected));
    }

    #[test]
    fn wide_seam_repair_is_local() {
        let mut p = page();
        p.put(0, 2, Cell::scalar('W', 0).with_wide(Wide::Wide), None);
        p.put(0, 3, Cell::spacer_tail(0), None);
        p.or_row_flags(0, row_flags::WRAPPED);
        p.put(0, 3, Cell::scalar('x', 0), None);
        assert_eq!(p.cell(0, 2), Cell::EMPTY);
        assert_eq!(p.cell(0, 3).scalar_char(), Some('x'));
        assert!(p.row_flags(0) & row_flags::WRAPPED != 0);
        p.put(0, 5, Cell::scalar('W', 0).with_wide(Wide::Wide), None);
        p.put(0, 6, Cell::spacer_tail(0), None);
        p.put(0, 5, Cell::scalar('y', 0), None);
        assert_eq!(p.cell(0, 6), Cell::EMPTY);
    }

    #[test]
    fn dch_never_pairs_a_head_with_another_cells_tail() {
        let mut p = page();
        let st = p.style_intern(&bold(), 2).unwrap();
        // [W x-1][tail x][W x+1][tail x+2], DCH 2 at x.
        p.put(0, 1, Cell::scalar('A', 0).with_wide(Wide::Wide), None);
        p.put(0, 2, Cell::spacer_tail(0), None);
        p.put(0, 3, Cell::scalar('B', st).with_wide(Wide::Wide), None);
        p.put(0, 4, Cell::spacer_tail(st), None);
        p.shift_left(0, 2, 10, 2, Cell::EMPTY);
        assert_eq!(p.cell(0, 1), Cell::EMPTY, "orphaned head must be blanked");
        assert_eq!(p.cell(0, 2), Cell::EMPTY);
        assert_eq!(p.cell(0, 3), Cell::EMPTY);
        p.audit().unwrap();
        // Cut through the middle of the second wide char.
        p.put(0, 5, Cell::scalar('C', 0).with_wide(Wide::Wide), None);
        p.put(0, 6, Cell::spacer_tail(0), None);
        p.put(0, 7, Cell::scalar('d', 0), None);
        p.shift_left(0, 4, 10, 2, Cell::EMPTY);
        assert_eq!(p.cell(0, 4), Cell::EMPTY);
        assert_eq!(p.cell(0, 5).scalar_char(), Some('d'));
        p.audit().unwrap();
    }

    fn populated() -> Page {
        let mut p = Page::new(PageSpec::with_rows(8, 4), 7);
        for _ in 0..3 {
            p.push_row();
        }
        let st = p.style_intern(&bold(), 2).unwrap();
        let l = p.link_intern("", "https://a/", 2).unwrap();
        p.link_reserve(2).unwrap();
        p.put(0, 0, Cell::scalar('a', st).with_link(true), Some(l));
        p.put(1, 3, Cell::scalar('b', st).with_link(true), Some(l));
        p.put(2, 1, Cell::scalar('c', 0), None);
        p
    }

    #[test]
    fn from_bytes_round_trips_and_validates_header_fields() {
        let p = populated();
        let q = Page::from_bytes(p.as_bytes().to_vec()).unwrap();
        q.audit().unwrap();
        assert_eq!(q.cell(1, 3).scalar_char(), Some('b'));
        for (off, v) in [
            (H_USED, 65_535u16),
            (H_STYLE_USED, 0),
            (H_STYLE_USED, 60_000),
            (H_LINK_USED, 9_999),
            (H_COLS, 0),
            (H_ROWS_CAP, 9),
        ] {
            let mut b = p.as_bytes().to_vec();
            b[off..off + 2].copy_from_slice(&v.to_le_bytes());
            assert!(Page::from_bytes(b).is_err(), "field at {off} = {v}");
        }
        for (off, v) in [
            (H_CHUNK_BUMP, u32::MAX),
            (H_LMAP_LEN, 1 << 30),
            (H_CHUNK_FREE, 5),
            (H_STR_USED, u32::MAX),
            (H_CHUNK_CAP, u32::MAX),
        ] {
            let mut b = p.as_bytes().to_vec();
            b[off..off + 4].copy_from_slice(&v.to_le_bytes());
            assert!(Page::from_bytes(b).is_err(), "field at {off} = {v}");
        }
        // A row offset of 0 and a misaligned one.
        for v in [0u32, p.lay.cells as u32 + 3] {
            let mut b = p.as_bytes().to_vec();
            let o = p.rh(1);
            b[o..o + 4].copy_from_slice(&v.to_le_bytes());
            assert!(Page::from_bytes(b).is_err());
        }
        // Two rows sharing a slot.
        let mut b = p.as_bytes().to_vec();
        let (o0, o1) = (p.rh(0), p.rh(1));
        let first: [u8; 4] = b[o0..o0 + 4].try_into().unwrap();
        b[o1..o1 + 4].copy_from_slice(&first);
        assert!(Page::from_bytes(b).is_err());
    }

    #[test]
    fn from_bytes_survives_random_corruption_without_panic() {
        let p = populated();
        let base = p.as_bytes().to_vec();
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let (mut ok, mut bad) = (0, 0);
        for _ in 0..3000 {
            let mut b = base.clone();
            for _ in 0..(1 + next() % 4) {
                let i = (next() as usize) % b.len().min(1500);
                b[i] = next() as u8;
            }
            match Page::from_bytes(b) {
                Ok(q) => {
                    ok += 1;
                    for r in 0..q.used_rows() {
                        for c in 0..q.cols() {
                            let _ = q.cell(r, c);
                            let _ = q.cell_link(r, c);
                        }
                    }
                    q.audit().unwrap();
                }
                Err(_) => bad += 1,
            }
        }
        assert!(ok > 0 && bad > 0, "ok {ok} bad {bad}");
    }

    #[test]
    fn repair_range_cuts_wide_pairs() {
        let mut p = page();
        p.put(0, 2, Cell::scalar('W', 0).with_wide(Wide::Wide), None);
        p.put(0, 3, Cell::spacer_tail(0), None);
        p.repair_range(0, 3, 6);
        p.fill_range(0, 3, 6, Cell::EMPTY);
        assert_eq!(p.cell(0, 2), Cell::EMPTY);
        p.put(0, 7, Cell::scalar('W', 0).with_wide(Wide::Wide), None);
        p.put(0, 8, Cell::spacer_tail(0), None);
        p.repair_range(0, 5, 8);
        p.fill_range(0, 5, 8, Cell::EMPTY);
        assert_eq!(p.cell(0, 8), Cell::EMPTY);
        p.audit().unwrap();
    }

    #[test]
    fn shifts_keep_links_and_refs() {
        let mut p = page();
        let st = p.style_intern(&bold(), 3).unwrap();
        let l = p.link_intern("", "https://a/", 1).unwrap();
        p.link_reserve(1).unwrap();
        for (i, ch) in "abc".chars().enumerate() {
            let linked = i == 1;
            p.put(
                0,
                i,
                Cell::scalar(ch, st).with_link(linked),
                if linked { Some(l) } else { None },
            );
        }
        p.shift_right(0, 0, 10, 2, Cell::EMPTY);
        assert_eq!(p.cell(0, 3).scalar_char(), Some('b'));
        assert_eq!(p.cell_link(0, 3), Some(l));
        p.audit().unwrap();
        p.shift_left(0, 0, 10, 4, Cell::EMPTY);
        assert_eq!(p.cell(0, 0).scalar_char(), Some('c'));
        assert_eq!(p.link_refs(l), 0);
        p.audit().unwrap();
    }

    #[test]
    fn row_rotation_moves_content_and_links() {
        let mut p = page();
        let l = p.link_intern("", "https://a/", 1).unwrap();
        p.link_reserve(1).unwrap();
        p.put(2, 0, Cell::scalar('q', 0).with_link(true), Some(l));
        p.put(0, 0, Cell::scalar('a', 0), None);
        p.rotate_rows_up(0, 3, 1);
        assert_eq!(p.cell(1, 0).scalar_char(), Some('q'));
        assert_eq!(p.cell_link(1, 0), Some(l));
        assert_eq!(p.cell(3, 0).scalar_char(), Some('a'));
        p.audit().unwrap();
        p.rotate_rows_down(0, 3, 1);
        assert_eq!(p.cell(2, 0).scalar_char(), Some('q'));
        p.audit().unwrap();
    }

    #[test]
    fn serialise_roundtrip() {
        let mut p = page();
        let st = p.style_intern(&bold(), 1).unwrap();
        p.put(0, 0, Cell::scalar('a', st), None);
        let h = p.cluster_alloc(&['e', '\u{301}']).unwrap();
        p.put(1, 0, Cell::cluster(h, 0), None);
        let l = p.link_intern("k", "https://x/", 1).unwrap();
        p.link_reserve(1).unwrap();
        p.put(2, 0, Cell::scalar('l', 0).with_link(true), Some(l));
        let bytes = p.as_bytes().to_vec();
        let q = Page::from_bytes(bytes).unwrap();
        q.audit().unwrap();
        assert_eq!(q.cell(0, 0), p.cell(0, 0));
        assert_eq!(q.style_get(st), bold());
        assert_eq!(q.link_get(l), ("k", "https://x/"));
        assert_eq!(q.serial(), 1);
        assert_eq!(
            Page::from_bytes(vec![0; 10]).unwrap_err(),
            PageError::Corrupt
        );
    }

    #[test]
    fn grow_links_and_clusters() {
        let mut spec = PageSpec::with_rows(4, 2);
        spec.chunk_cap = 2;
        spec.link_cap = 1;
        spec.lmap_cap = 1;
        spec.str_cap = 16;
        let mut p = Page::new(spec, 7);
        p.push_row();
        let h = p.cluster_alloc(&['a', 'b']).unwrap();
        p.put(0, 0, Cell::cluster(h, 0), None);
        let _ = p.cluster_alloc(&['c']).unwrap();
        assert_eq!(
            p.cluster_alloc(&['d', 'e', 'f', 'g', 'h']),
            Err(Full::Chunks)
        );
        let g = p.grown(Full::Chunks).unwrap();
        g.audit().unwrap_err();
        assert_eq!(g.serial(), 7);
        assert_eq!(g.cell(0, 0), p.cell(0, 0));
    }

    #[test]
    fn standard_spec_is_about_512k() {
        let s = PageSpec::standard(80, 512 * 1024);
        let total = s.total_bytes();
        assert!(total <= 512 * 1024 && total > 400 * 1024, "{total}");
        let wide = PageSpec::standard(5000, 512 * 1024);
        assert_eq!(wide.rows_cap, MIN_PAGE_ROWS);
        assert!(wide.total_bytes() > 512 * 1024);
    }

    /// Tiny deterministic generator for the property test.
    fn xs(state: &mut u64) -> usize {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        (x >> 11) as usize
    }

    #[test]
    fn link_map_matches_model_under_shifts_and_erases() {
        // The hash-indexed link map against a plain grid of `Option<link>`, through random
        // puts, ICH, DCH and erases. Audit checks the index against the cells each step.
        const COLS: usize = 48;
        let mut spec = PageSpec::with_rows(COLS, 3);
        spec.lmap_cap = 3 * COLS;
        spec.link_cap = 4;
        let mut p = Page::new(spec, 1);
        for _ in 0..3 {
            p.push_row();
        }
        let links: Vec<u16> = (0..3)
            .map(|i| p.link_intern("", &format!("https://l{i}/"), 0).unwrap())
            .collect();
        let mut model = vec![vec![None::<u16>; COLS]; 3];
        let mut rng = 0x1234_5678_9abc_def1u64;
        for step in 0..4000 {
            let r = xs(&mut rng) % 3;
            match xs(&mut rng) % 5 {
                0 | 1 => {
                    let c = xs(&mut rng) % COLS;
                    let l = (xs(&mut rng) % 4).checked_sub(1).map(|i| links[i]);
                    if let Some(l) = l {
                        p.link_acquire(l, 1);
                    }
                    p.put(r, c, Cell::scalar('a', 0).with_link(l.is_some()), l);
                    model[r][c] = l;
                }
                2 => {
                    let x = xs(&mut rng) % COLS;
                    let n = 1 + xs(&mut rng) % 5;
                    p.shift_right(r, x, COLS, n, Cell::EMPTY);
                    let row = &mut model[r];
                    for c in (x..COLS).rev() {
                        row[c] = if c >= x + n { row[c - n] } else { None };
                    }
                }
                3 => {
                    let x = xs(&mut rng) % COLS;
                    let n = 1 + xs(&mut rng) % 5;
                    p.shift_left(r, x, COLS, n, Cell::EMPTY);
                    let row = &mut model[r];
                    for c in x..COLS {
                        row[c] = if c + n < COLS { row[c + n] } else { None };
                    }
                }
                _ => {
                    let x0 = xs(&mut rng) % COLS;
                    let x1 = x0 + xs(&mut rng) % (COLS - x0 + 1);
                    p.repair_range(r, x0, x1);
                    p.fill_range(r, x0, x1, Cell::EMPTY);
                    model[r][x0..x1].fill(None);
                }
            }
            if let Err(e) = p.audit() {
                panic!("step {step}: {e}");
            }
            for (rr, row) in model.iter().enumerate() {
                for (c, want) in row.iter().enumerate() {
                    assert_eq!(p.cell_link(rr, c), *want, "step {step} ({rr},{c})");
                }
            }
        }
    }

    #[test]
    fn many_unique_links_intern_by_hash() {
        let mut spec = PageSpec::with_rows(4, 2);
        spec.link_cap = 5000;
        spec.str_cap = 1 << 18;
        let mut p = Page::new(spec, 1);
        let ids: Vec<u16> = (0..5000)
            .map(|i| {
                p.link_intern("", &format!("https://example.org/{i}"), 0)
                    .unwrap()
            })
            .collect();
        for (i, &id) in ids.iter().enumerate() {
            assert_eq!(id as usize, i);
            assert_eq!(
                p.link_intern("", &format!("https://example.org/{i}"), 0),
                Ok(id)
            );
        }
        assert_eq!(p.link_items(), 5000);
        assert_eq!(
            p.link_intern("", "https://example.org/x", 0),
            Err(LinkError::Full(Full::Links))
        );
        let g = p.grown(Full::Links).unwrap();
        assert_eq!(g.link_get(4999).1, "https://example.org/4999");
    }
}
