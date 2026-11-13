// SPDX-License-Identifier: AGPL-3.0-only
//! The two snapshot APIs (T-02 §5). Each consumer of grid changes uses exactly one and
//! owns its change state.
//!
//! * [`Screen::snapshot_dirty`] is for the renderer only: it reads and clears the row
//!   `dirty` flags and the pending-scroll record.
//! * [`Screen::snapshot_since`] is for every other consumer: it keys on the per-row change
//!   sequence number against a consumer-owned [`SeenSet`] and never touches `dirty`.
//!
//! Both resolve styles into [`SnapCell`]s and, once their buffers have capacity, allocate
//! nothing. The caller holds the term lock only for the call itself.

use crate::cell::{Semantic, Tag, Wide};
use crate::page::{MAX_CLUSTER_CPS, Page, row_flags as flags};
use crate::screen::Screen;
use crate::style::Style;

/// A cell with its style resolved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SnapCell {
    /// Scalar code point, or the first code point of a cluster; 0 for empty cells.
    pub cp: u32,
    /// Start of this cell's cluster in its row's `clusters` buffer (when `cluster_len > 0`).
    pub cluster_start: u32,
    /// Code points in the cluster; 0 when the cell is a scalar or empty.
    pub cluster_len: u8,
    /// Width class.
    pub wide: Wide,
    /// Resolved style (a BgOnly cell resolves to a style with just its background).
    pub style: Style,
    /// Carries a hyperlink.
    pub link: bool,
    /// DECSCA protected.
    pub protected: bool,
    /// Semantic zone.
    pub semantic: Semantic,
}

/// One copied row.
#[derive(Clone, Debug, Default)]
pub struct SnapRow {
    /// The row's cells (`cols` of them once filled).
    pub cells: Vec<SnapCell>,
    /// Cluster code points referenced by `cells`.
    pub clusters: Vec<u32>,
    /// Soft-wrapped into the next row.
    pub wrapped: bool,
    /// Prompt-start mark.
    pub prompt_start: bool,
    /// Block id.
    pub block: u32,
    /// Change sequence number at copy time.
    pub seq: u64,
}

impl SnapRow {
    /// The code points of a cluster cell.
    #[must_use]
    pub fn cluster(&self, c: &SnapCell) -> &[u32] {
        let s = c.cluster_start as usize;
        &self.clusters[s..s + c.cluster_len as usize]
    }
}

/// What the renderer must redraw. After `snapshot_dirty` the `FrameSnapshot` already equals
/// the grid; this tells the renderer how much changed since *its previous frame*.
///
/// `Rows` and `Scroll` list their rows in [`FrameSnapshot::changed`] (a reusable buffer, so
/// the call allocates nothing). For `Scroll`, `delta > 0` means row `y + delta` became row
/// `y` within the region; the previous frame shifted that way plus the rows in `changed`
/// equals `Full`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Damage {
    /// Nothing changed.
    None,
    /// Only the cursor.
    CursorOnly,
    /// The rows in `changed`.
    Rows,
    /// A region scroll, then the rows in `changed`.
    Scroll {
        /// First row of the region.
        top: usize,
        /// Last row of the region.
        bottom: usize,
        /// First column.
        left: usize,
        /// Last column.
        right: usize,
        /// Net rows moved up (positive) or down (negative).
        delta: i32,
    },
    /// Everything.
    Full,
}

/// Renderer-owned snapshot buffer.
#[derive(Debug, Default)]
pub struct FrameSnapshot {
    /// All visible rows, current as of the last `snapshot_dirty`.
    pub rows: Vec<SnapRow>,
    /// Rows copied by the last call (all of them after a `Full`).
    pub changed: Vec<u32>,
    screen_id: u64,
    epoch: u64,
    cols: usize,
    valid: bool,
}

impl FrameSnapshot {
    /// An empty buffer; its first snapshot is `Full`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// Consumer-owned "last seen" sequence per row position.
#[derive(Debug, Default)]
pub struct SeenSet {
    seen: Vec<u64>,
    screen_id: u64,
    epoch: u64,
}

impl SeenSet {
    /// Sized for a screen of `rows` rows, nothing seen.
    #[must_use]
    pub fn new(rows: usize) -> Self {
        SeenSet {
            seen: vec![0; rows],
            screen_id: 0,
            epoch: 0,
        }
    }

    /// Forget everything (resize, screen switch, reflow); the next call is a full copy.
    pub fn reset(&mut self) {
        self.seen.fill(0);
        self.screen_id = 0;
    }
}

/// Buffer for `snapshot_since` consumers.
#[derive(Debug, Default)]
pub struct RowSnapshot {
    /// All visible rows as of the last call.
    pub rows: Vec<SnapRow>,
    /// Rows copied by the last call.
    pub changed: Vec<u32>,
}

/// Result of `snapshot_since`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Changes {
    /// Nothing new.
    None,
    /// The rows in `RowSnapshot::changed`.
    Rows,
    /// First call, or the seen-set was reset: every row was copied.
    Full,
}

fn resolve(page: &Page, slot: usize, x: usize, clusters: &mut Vec<u32>) -> SnapCell {
    let c = page.cell(slot, x);
    let mut cp = 0;
    let mut cluster_start = 0;
    let mut cluster_len = 0u8;
    match c.tag() {
        Tag::Scalar => cp = c.payload(),
        Tag::Cluster => {
            cluster_start = clusters.len() as u32;
            let mut buf = ['\0'; MAX_CLUSTER_CPS];
            let n = page.cluster_read(c.payload(), &mut buf);
            clusters.extend(buf[..n].iter().map(|&ch| ch as u32));
            cluster_len = (clusters.len() as u32 - cluster_start) as u8;
            if cluster_len > 0 {
                cp = clusters[cluster_start as usize];
            }
        }
        _ => {}
    }
    let style = if c.tag() == Tag::BgOnly {
        Style {
            bg: c.bg_color(),
            ..Style::DEFAULT
        }
    } else {
        page.style_get(c.style_id())
    };
    SnapCell {
        cp,
        cluster_start,
        cluster_len,
        wide: c.wide(),
        style,
        link: c.has_link(),
        protected: c.is_protected(),
        semantic: c.semantic(),
    }
}

fn fill(page: &Page, slot: usize, cols: usize, out: &mut SnapRow) {
    let hf = page.row_flags(slot);
    out.cells.clear();
    out.clusters.clear();
    out.wrapped = hf & flags::WRAPPED != 0;
    out.prompt_start = hf & flags::PROMPT_START != 0;
    out.block = page.row_block(slot);
    out.seq = page.row_seq(slot);
    for x in 0..cols {
        let c = resolve(page, slot, x, &mut out.clusters);
        out.cells.push(c);
    }
}

impl Screen {
    /// Copy one active-area row, styles resolved. Allocation-free when `out` has capacity.
    pub fn copy_row(&self, y: usize, out: &mut SnapRow) {
        if y >= self.rows {
            out.cells.clear();
            out.clusters.clear();
            return;
        }
        let (pi, slot) = self.slot_of(y);
        fill(&self.pages[pi], slot, self.cols, out);
    }

    /// Copy row `abs`, counted from the oldest retained row, so a renderer scrolled into
    /// history can read it. False if out of range.
    pub fn copy_abs_row(&self, abs: usize, out: &mut SnapRow) -> bool {
        let Some((pi, pos)) = self.loc_abs(abs) else {
            return false;
        };
        let page = &self.pages[pi];
        fill(page, pos, self.cols, out);
        true
    }

    fn clear_dirty(&mut self, y: usize) {
        let (pi, slot) = self.slot_of(y);
        self.pages[pi].clear_row_flags(slot, flags::DIRTY);
    }

    /// Renderer snapshot: copies only dirty rows (all after `Full`), clears the dirty flags
    /// and the pending-scroll record, and tells the renderer what changed.
    pub fn snapshot_dirty(&mut self, snap: &mut FrameSnapshot) -> Damage {
        let stale = !snap.valid
            || snap.screen_id != self.id
            || snap.epoch != self.epoch
            || snap.rows.len() != self.rows
            || snap.cols != self.cols;
        if stale || self.force_full {
            snap.rows.resize_with(self.rows, SnapRow::default);
            snap.changed.clear();
            for y in 0..self.rows {
                self.copy_row(y, &mut snap.rows[y]);
                self.clear_dirty(y);
                snap.changed.push(y as u32);
            }
            snap.screen_id = self.id;
            snap.epoch = self.epoch;
            snap.cols = self.cols;
            snap.valid = true;
            self.force_full = false;
            self.pending = None;
            self.cursor_dirty = false;
            return Damage::Full;
        }
        let scroll = self.pending.take();
        if let Some(sc) = scroll {
            let r = sc.region;
            let rows = &mut snap.rows[r.top..=r.bottom];
            if sc.delta > 0 {
                rows.rotate_left(sc.delta.unsigned_abs());
            } else {
                rows.rotate_right(sc.delta.unsigned_abs());
            }
        }
        snap.changed.clear();
        for y in 0..self.rows {
            let (pi, slot) = self.slot_of(y);
            if self.pages[pi].row_flags(slot) & flags::DIRTY != 0 {
                self.copy_row(y, &mut snap.rows[y]);
                self.clear_dirty(y);
                snap.changed.push(y as u32);
            }
        }
        let cursor = std::mem::take(&mut self.cursor_dirty);
        if let Some(sc) = scroll {
            Damage::Scroll {
                top: sc.region.top,
                bottom: sc.region.bottom,
                left: sc.region.left,
                right: sc.region.right,
                delta: sc.delta as i32,
            }
        } else if !snap.changed.is_empty() {
            Damage::Rows
        } else if cursor {
            Damage::CursorOnly
        } else {
            Damage::None
        }
    }

    /// Snapshot for a11y, the semantic publisher, the mux tee and `read-content`: copies the
    /// rows whose change sequence exceeds this consumer's `seen` entry. Never reads or
    /// clears `dirty`, so it cannot disturb the renderer or another consumer.
    pub fn snapshot_since(&self, snap: &mut RowSnapshot, seen: &mut SeenSet) -> Changes {
        let reset = seen.screen_id != self.id
            || seen.epoch != self.epoch
            || seen.seen.len() != self.rows
            || snap.rows.len() != self.rows;
        if reset {
            seen.seen.clear();
            seen.seen.resize(self.rows, 0);
            seen.screen_id = self.id;
            seen.epoch = self.epoch;
            snap.rows.resize_with(self.rows, SnapRow::default);
        }
        snap.changed.clear();
        for y in 0..self.rows {
            let (pi, slot) = self.slot_of(y);
            let s = self.pages[pi].row_seq(slot);
            if s > seen.seen[y] {
                self.copy_row(y, &mut snap.rows[y]);
                seen.seen[y] = s;
                snap.changed.push(y as u32);
            }
        }
        if reset {
            Changes::Full
        } else if snap.changed.is_empty() {
            Changes::None
        } else {
            Changes::Rows
        }
    }
}
