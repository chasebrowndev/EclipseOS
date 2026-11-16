// SPDX-License-Identifier: AGPL-3.0-only
//! Reflow on column resize (T-02 §3, SCROLLBACKSEARCH-22, VTCORE-07/08).
//!
//! The primary screen re-wraps soft-wrapped runs as logical lines at the new width. Pins,
//! the cursor, block ids, semantic bits, hyperlinks and prompt marks are carried; wide cells
//! never split (a wide cell that does not fit the last column leaves a `SpacerHead` and
//! wraps, the wrap flag preserved); every rewritten row gets a fresh change sequence and the
//! next snapshot is `Full`. The alternate screen never reflows.
//!
//! This implementation is eager: history is converted in the same call under the caller's
//! lock. The lazy, page-by-page conversion of T-02 §3 (wrap_width tag, slices of <= 1 page or
//! 200 us) and LZ4 cold-page compression (T-02 §4, needs `lz4_flex`, which the orchestrator
//! must add) are SPEC DEVIATIONS owed to a later milestone; M12 is not complete against
//! its `history compression` row until they land. The cost of the eager pass is measured by
//! `tests_reflow::resize_reflow_100k_lines_measured` (T-23 RESIZE / MEM-LINE).
//! The per-row loop allocates nothing once the scratch vectors are warm.

use super::*;

/// Reflow options (config-ready: `scrollback.reflow`, `reflow.protect-prompt`).
#[derive(Clone, Copy, Debug)]
pub struct ReflowOpts {
    /// `false` truncates/pads instead of re-wrapping (`scrollback.reflow #false`).
    pub reflow: bool,
    /// A pin at the start of the live prompt: rows from its row down are truncated or
    /// padded, never re-wrapped (the shell redraws them; SHELLINTEGRATION-10).
    pub protect_from: Option<PinId>,
}

impl Default for ReflowOpts {
    fn default() -> Self {
        ReflowOpts {
            reflow: true,
            protect_from: None,
        }
    }
}

/// What [`Screen::resize_reflow`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reflowed {
    /// Visible rows after the (clamped) resize.
    pub rows: usize,
    /// Columns after the (clamped) resize.
    pub cols: usize,
    /// New cursor position `(row, col)` in the active area.
    pub cursor: (usize, usize),
    /// Rows pushed from the top of the old view into scrollback (net).
    pub pushed: usize,
    /// Whether rows were actually re-wrapped.
    pub reflowed: bool,
}

/// One source unit: a cell, or a wide cell with its tail (`w == 2`).
#[derive(Clone, Copy)]
struct Unit {
    /// Offset of the unit's first cell within the logical line.
    off: u32,
    /// Index into the line's row table.
    rm: u32,
    /// Column in the source row.
    x: u16,
    /// Columns (1 or 2).
    w: u8,
    /// Nothing to copy.
    blank: bool,
}

#[derive(Clone, Copy)]
struct RowMeta {
    pi: u32,
    pos: u32,
    prefix: u32,
    len: u32,
    flags: u16,
    block: u32,
}

#[derive(Clone, Copy)]
struct PinIn {
    abs: usize,
    col: u16,
    idx: usize,
}

struct Builder<'a> {
    fresh: Vec<Page>,
    pool: &'a mut PagePool,
    spec: PageSpec,
    ceil: Ceil,
    serial: u64,
    nc: usize,
    seq: u64,
    degraded: u64,
    rows: usize,
}

impl Builder<'_> {
    /// Open a blank row; returns `(page index, position)`.
    fn new_row(&mut self, flags: u16, block: u32) -> (usize, usize) {
        let pos = open_row(
            &mut self.fresh,
            self.pool,
            self.spec,
            &mut self.serial,
            self.nc as u16,
        );
        let di = self.fresh.len() - 1;
        let p = &mut self.fresh[di];
        p.touch(pos, self.seq);
        p.or_row_flags(pos, flags);
        p.set_row_block(pos, block);
        self.rows += 1;
        (di, pos)
    }

    fn copy(&mut self, di: usize, dpos: usize, dc: usize, src: &Page, spos: usize, sc: usize) {
        let mut lossy = false;
        loop {
            match self.fresh[di].import_cell(dpos, dc, src, spos, sc, lossy) {
                Ok(()) => return,
                Err(w) => match self.fresh[di].grown_with(w, &self.ceil) {
                    Some(n) => self.fresh[di] = n,
                    None => {
                        lossy = true;
                        self.degraded += 1;
                    }
                },
            }
        }
    }
}

fn old_at(starts: &[usize], abs: usize) -> (usize, usize) {
    let pi = starts.partition_point(|&s| s <= abs) - 1;
    (pi, abs - starts[pi])
}

impl Screen {
    /// Resize to `rows x cols`, re-wrapping the primary screen and its history when the
    /// width changes. `cursor` is the cursor's active-area position; the returned
    /// [`Reflowed::cursor`] is where it is afterwards. Tracked pins follow their content; pins
    /// on rows that no longer exist become garbage.
    ///
    /// Falls back to [`Screen::resize_keeping`] (truncate/pad, no re-wrap) on the alternate
    /// screen, when only the rows change, or when `opts.reflow` is false.
    pub fn resize_reflow(
        &mut self,
        rows: usize,
        cols: usize,
        cursor: (usize, usize),
        opts: ReflowOpts,
    ) -> Reflowed {
        let nc = cols.clamp(1, u16::MAX as usize);
        let mut nr = rows.clamp(1, u16::MAX as usize);
        if nr * nc > MAX_CELLS {
            nr = (MAX_CELLS / nc).max(1);
        }
        if !opts.reflow || !self.history || nc == self.cols {
            let r = self.resize_keeping(rows, cols, cursor.0);
            return Reflowed {
                rows: r.rows,
                cols: r.cols,
                cursor: (r.cursor_row, cursor.1.min(r.cols - 1)),
                pushed: r.pushed,
                reflowed: false,
            };
        }
        self.reflow_rebuild(nr, nc, cursor, opts)
    }

    fn reflow_rebuild(
        &mut self,
        nr: usize,
        nc: usize,
        cursor: (usize, usize),
        opts: ReflowOpts,
    ) -> Reflowed {
        let (or, oc) = (self.rows, self.cols);
        let cy = cursor.0.min(or - 1);
        let cx = cursor.1.min(oc - 1);
        let total_old = self.total_rows;
        let old_top = total_old - or;
        let cursor_abs = old_top + cy;
        let prot_abs = opts
            .protect_from
            .and_then(|id| self.pin_pos(id))
            .map_or(usize::MAX, |(a, _)| a);

        // Gather pins (remap visits live pins in a stable order).
        let mut raw: Vec<Pin> = Vec::new();
        self.pins.remap(|p| {
            raw.push(*p);
            Some(*p)
        });

        let old = std::mem::take(&mut self.pages);
        let mut starts: Vec<usize> = Vec::with_capacity(old.len() + 1);
        let mut acc = 0;
        for p in &old {
            starts.push(acc);
            acc += p.used_rows();
        }
        starts.push(acc);

        let mut pins_in: Vec<PinIn> = Vec::with_capacity(raw.len());
        for (idx, p) in raw.iter().enumerate() {
            if let Some(pi) = old.iter().position(|pg| pg.serial() == p.serial)
                && let Some(pos) = old[pi].pos_of(p.slot)
            {
                pins_in.push(PinIn {
                    abs: starts[pi] + pos,
                    col: p.col,
                    idx,
                });
            }
        }
        pins_in.sort_by_key(|p| p.abs);
        let mut pin_out: Vec<Option<(usize, u16)>> = vec![None; raw.len()];

        let spec = match self.cap {
            Some(c) => PageSpec::with_rows(nc, usize::from(c)),
            None => PageSpec::standard(nc, PAGE_BYTES),
        };
        self.pool.free.clear();
        let seq = self.bump();
        let mut b = Builder {
            fresh: Vec::new(),
            pool: &mut self.pool,
            spec,
            ceil: self.ceil,
            serial: self.next_serial,
            nc,
            seq,
            degraded: 0,
            rows: 0,
        };

        let mut units: Vec<Unit> = Vec::new();
        let mut place: Vec<(u32, u16)> = Vec::new();
        let mut meta: Vec<RowMeta> = Vec::new();
        let mut pptr = 0;
        let mut cursor_new: (usize, u16) = (0, 0);
        let mut top_new = 0usize;
        let mut last_content = 0usize;
        let keep_flags = flags::WRAPPED | flags::WRAP_CONT;

        let mut abs = 0;
        while abs < total_old {
            let protected = abs >= prot_abs;
            let mut end = abs + 1;
            if !protected {
                while end < total_old {
                    let (pi, pos) = old_at(&starts, end - 1);
                    if old[pi].row_flags(pos) & flags::WRAPPED == 0 {
                        break;
                    }
                    end += 1;
                }
            }
            // ---- gather units ----
            units.clear();
            meta.clear();
            let mut prefix = 0u32;
            for r in abs..end {
                let (pi, pos) = old_at(&starts, r);
                let pg = &old[pi];
                let last = r + 1 == end;
                let mut len = oc;
                if last {
                    len = len.min(pg.row_high_water(pos));
                    while len > 0 && pg.cell(pos, len - 1) == Cell::EMPTY {
                        len -= 1;
                    }
                    if r == cursor_abs {
                        len = len.max(cx + 1);
                    }
                } else {
                    let tail = pg.cell(pos, oc - 1);
                    let (ni, np) = old_at(&starts, r + 1);
                    let next_wide = old[ni].cell(np, 0).wide() == Wide::Wide;
                    if tail.wide() == Wide::SpacerHead || (tail == Cell::EMPTY && next_wide) {
                        len = oc - 1;
                    }
                }
                let rm = meta.len() as u32;
                meta.push(RowMeta {
                    pi: pi as u32,
                    pos: pos as u32,
                    prefix,
                    len: len as u32,
                    flags: pg.row_flags(pos),
                    block: pg.row_block(pos),
                });
                let mut x = 0;
                while x < len {
                    let c = pg.cell(pos, x);
                    let wide = c.wide() == Wide::Wide
                        && x + 1 < oc
                        && pg.cell(pos, x + 1).wide() == Wide::SpacerTail;
                    let blank = c == Cell::EMPTY
                        || c.wide() == Wide::SpacerHead
                        || (c.wide() == Wide::SpacerTail)
                        || (c.wide() == Wide::Wide && !wide);
                    units.push(Unit {
                        off: prefix + x as u32,
                        rm,
                        x: x as u16,
                        w: if wide { 2 } else { 1 },
                        blank,
                    });
                    x += if wide { 2 } else { 1 };
                }
                prefix += len as u32;
            }
            let total_len = prefix;

            // ---- place units ----
            place.clear();
            let first = meta[0];
            let line_start = b.rows;
            let row_flags0 = if protected {
                first.flags & keep_flags
            } else {
                0
            };
            let (mut di, mut dpos) = b.new_row(row_flags0, first.block);
            let mut row_idx = 0u32;
            let mut col = 0usize;
            let mut content_row: Option<u32> = None;
            if first.flags & flags::PROMPT_START != 0 {
                b.fresh[di].or_row_flags(dpos, flags::PROMPT_START);
            }
            for u in &units {
                let (w, blank) = if usize::from(u.w) > nc {
                    (1usize, true)
                } else {
                    (usize::from(u.w), u.blank)
                };
                if col + w > nc {
                    if protected {
                        place.push((row_idx, (nc - 1) as u16));
                        continue;
                    }
                    if w == 2 && col + 1 == nc {
                        b.fresh[di].put(dpos, col, Cell::spacer_head(), None);
                    }
                    b.fresh[di].or_row_flags(dpos, flags::WRAPPED);
                    let blk = meta[u.rm as usize].block;
                    (di, dpos) = b.new_row(flags::WRAP_CONT, blk);
                    row_idx += 1;
                    col = 0;
                }
                let m = meta[u.rm as usize];
                if u.x == 0 && m.flags & flags::PROMPT_START != 0 && u.rm != 0 {
                    b.fresh[di].or_row_flags(dpos, flags::PROMPT_START);
                }
                place.push((row_idx, col as u16));
                if !blank {
                    let src = &old[m.pi as usize];
                    b.copy(di, dpos, col, src, m.pos as usize, usize::from(u.x));
                    if w == 2 {
                        b.copy(di, dpos, col + 1, src, m.pos as usize, usize::from(u.x) + 1);
                    }
                    content_row = Some(row_idx);
                }
                col += w;
            }
            if let Some(cr) = content_row {
                last_content = last_content.max(line_start + cr as usize);
            } else if first.flags & flags::PROMPT_START != 0 || first.block != 0 {
                last_content = last_content.max(line_start);
            }

            // ---- map cursor, active top and pins ----
            let map = |rm: usize, col: usize, force_prefix: bool| -> (usize, u16) {
                let m = meta[rm];
                let last = rm + 1 == meta.len();
                let off = if force_prefix {
                    m.prefix
                } else if last {
                    m.prefix + col as u32
                } else {
                    m.prefix + (col as u32).min(m.len)
                };
                if units.is_empty() {
                    return (line_start, col.min(nc - 1) as u16);
                }
                if off >= total_len {
                    let li = units.len() - 1;
                    let (r, c) = place[li];
                    let c =
                        (usize::from(c) + usize::from(units[li].w) + (off - total_len) as usize)
                            .min(nc - 1);
                    return (line_start + r as usize, c as u16);
                }
                let ui = units.partition_point(|u| u.off <= off) - 1;
                let u = units[ui];
                let (r, c) = place[ui];
                let c = (usize::from(c) + (off - u.off) as usize).min(nc - 1);
                (line_start + r as usize, c as u16)
            };
            if (abs..end).contains(&cursor_abs) {
                cursor_new = map(cursor_abs - abs, cx, false);
            }
            if (abs..end).contains(&old_top) {
                top_new = map(old_top - abs, 0, true).0;
            }
            while pptr < pins_in.len() && pins_in[pptr].abs < end {
                let p = pins_in[pptr];
                pin_out[p.idx] = Some({
                    let (r, c) = map(p.abs - abs, usize::from(p.col), false);
                    (r, c)
                });
                pptr += 1;
            }
            abs = end;
        }

        // ---- choose the active area ----
        let mut t = b.rows;
        let c_abs = cursor_new.0;
        let e = (c_abs + 1).max(last_content + 1);
        let s = e.saturating_sub(nr).max(top_new).min(c_abs);
        // Drop rows below the new active area (blank by construction), or pad.
        while t > s + nr {
            let drop_page = {
                let Some(p) = b.fresh.last_mut() else { break };
                p.pop_row();
                p.used_rows() == 0
            };
            if drop_page {
                b.fresh.pop();
            }
            t -= 1;
        }
        b.rows = t;
        while b.rows < s + nr {
            b.new_row(0, 0);
        }

        // ---- pins to their new homes ----
        let fresh = std::mem::take(&mut b.fresh);
        let (serial, degraded) = (b.serial, b.degraded);
        let mut fstarts: Vec<usize> = Vec::with_capacity(fresh.len());
        let mut acc = 0;
        for p in &fresh {
            fstarts.push(acc);
            acc += p.used_rows();
        }
        let mut k = 0;
        self.pins.remap(|_| {
            let r = pin_out.get(k).copied().flatten();
            k += 1;
            let (a, c) = r?;
            if a >= s + nr {
                return None;
            }
            let pi = fstarts.partition_point(|&st| st <= a) - 1;
            let q = &fresh[pi];
            Some(Pin {
                serial: q.serial(),
                slot: q.slot_at(a - fstarts[pi]),
                col: c,
                wrap_width: q.wrap_width(),
            })
        });
        self.next_serial = serial;
        self.stats.degraded += degraded;
        self.rows = nr;
        self.cols = nc;
        self.spec = spec;
        self.total_rows = acc;
        self.pages = fresh;
        self.bytes_total = self.pages.iter().map(|p| self.page_logical_bytes(p)).sum();
        drop(old);
        self.epoch += 1;
        self.force_full = true;
        self.pending = None;
        self.enforce_limits();
        Reflowed {
            rows: nr,
            cols: nc,
            cursor: (c_abs - s, cursor_new.1 as usize),
            pushed: s.saturating_sub(top_new),
            reflowed: true,
        }
    }
}
