// SPDX-License-Identifier: AGPL-3.0-only
//! CSI dispatch: cursor, erase, insert/delete, scroll, margins, modes, reports.

use apoth_grid::{CellAttrs, Color, Glyph, ScreenConfig, Wide, flags};
use apoth_input::modes::bit;
use apoth_vt::Params;

use crate::machine::Machine;
use crate::sgr;

/// Count parameter: empty and 0 read as 1, capped.
fn num(p: &Params, i: usize) -> usize {
    match p.get(i) {
        None | Some(0) => 1,
        Some(v) => (v as usize).min(1 << 16),
    }
}

impl Machine {
    pub(crate) fn csi_dispatch(&mut self, p: &Params, inter: &[u8], private: u8, fin: u8) {
        if !(fin == b'm' && private == 0 && inter.is_empty()) {
            self.cursor_moved();
            if fin != b'b' {
                self.last_char = None;
            }
        }
        match (private, inter) {
            (0, []) => self.csi_plain(p, fin),
            (b'?', []) => self.csi_dec(p, fin),
            (b'>', []) => self.csi_gt(p, fin),
            (b'<', []) => {
                if fin == b'u' {
                    let n = p.get_or(0, 1);
                    self.kitty.active_mut().pop(n);
                    self.publish();
                }
            }
            (b'=', []) => match fin {
                b'c' => {
                    if p.get_or(0, 0) == 0 {
                        self.reply_str("\x1bP!|00000000\x1b\\".to_string());
                    }
                }
                b'u' => {
                    let f = p.get_or(0, 0);
                    let m = p.get_or(1, 1);
                    self.kitty.active_mut().set(f, m);
                    self.publish();
                }
                _ => {}
            },
            (0, [b' ']) => {
                if fin == b'q' {
                    self.decscusr(p.get_or(0, 0));
                }
            }
            (0, [b'!']) => {
                if fin == b'p' {
                    self.soft_reset();
                }
            }
            (0, [b'$']) => {
                if fin == b'p' {
                    self.decrqm(false, p);
                }
            }
            (b'?', [b'$']) => {
                if fin == b'p' {
                    self.decrqm(true, p);
                }
            }
            (0, [b'\'']) => match fin {
                b'}' => self.col_edit(num(p, 0), true),
                b'~' => self.col_edit(num(p, 0), false),
                _ => {}
            },
            #[cfg(feature = "esctest")]
            (0, [b'*']) if fin == b'y' => self.decrqcra(p),
            _ => {}
        }
    }

    fn csi_plain(&mut self, p: &Params, fin: u8) {
        let c = self.cur;
        let bg = self.pen.bg;
        match fin {
            b'A' => {
                let min = if self.y >= self.top { self.top } else { 0 };
                self.y = self.y.saturating_sub(num(p, 0)).max(min);
                self.wrap = false;
            }
            b'B' | b'e' => {
                let max = if self.y <= self.bottom {
                    self.bottom
                } else {
                    self.rows - 1
                };
                self.y = (self.y + num(p, 0)).min(max);
                self.wrap = false;
            }
            b'C' | b'a' => {
                let max = if self.lrmm && self.x <= self.right {
                    self.right
                } else {
                    self.cols - 1
                };
                self.x = (self.x + num(p, 0)).min(max);
                self.wrap = false;
            }
            b'D' => {
                let min = if self.lrmm && self.x >= self.left {
                    self.left
                } else {
                    0
                };
                self.x = self.x.saturating_sub(num(p, 0)).max(min);
                self.wrap = false;
            }
            b'E' | b'F' => {
                if fin == b'E' {
                    let max = if self.y <= self.bottom {
                        self.bottom
                    } else {
                        self.rows - 1
                    };
                    self.y = (self.y + num(p, 0)).min(max);
                } else {
                    let min = if self.y >= self.top { self.top } else { 0 };
                    self.y = self.y.saturating_sub(num(p, 0)).max(min);
                }
                self.cr();
            }
            b'G' | b'`' => self.set_col(num(p, 0)),
            b'd' => self.set_row(num(p, 0)),
            b'H' | b'f' => {
                self.set_row(num(p, 0));
                self.set_col(num(p, 1));
            }
            b'I' => self.tab_fwd(num(p, 0)),
            b'Z' => {
                self.x = self.tabs.prev(self.x, num(p, 0));
                self.wrap = false;
            }
            b'J' => self.erase_display(p.get_or(0, 0)),
            b'K' => self.erase_line(p.get_or(0, 0)),
            b'@' => {
                if self.inside_lr_pub() {
                    let right = self.lr_right();
                    let (y, x, n) = (self.y, self.x, num(p, 0));
                    self.panes[c].grid.insert_chars(y, x, n, right, bg);
                }
            }
            b'P' => {
                if self.inside_lr_pub() {
                    let right = self.lr_right();
                    let (y, x, n) = (self.y, self.x, num(p, 0));
                    self.panes[c].grid.delete_chars(y, x, n, right, bg);
                }
            }
            b'X' => {
                let (y, x) = (self.y, self.x);
                let x1 = (x + num(p, 0)).min(self.cols);
                self.panes[c].grid.erase_cells(y, x, x1, bg, false);
            }
            b'L' | b'M' => {
                if self.y >= self.top && self.y <= self.bottom && self.inside_lr_pub() {
                    let mut r = self.region();
                    r.top = self.y;
                    let n = num(p, 0);
                    if fin == b'L' {
                        self.panes[c].grid.insert_lines(r, n, bg);
                    } else {
                        self.panes[c].grid.delete_lines(r, n, bg);
                    }
                    self.cr();
                }
            }
            b'S' => {
                let r = self.region();
                self.panes[c].grid.scroll_up(r, num(p, 0), bg);
            }
            b'T' => {
                if p.len() <= 1 {
                    let r = self.region();
                    self.panes[c].grid.scroll_down(r, num(p, 0), bg);
                }
            }
            b'b' => {
                if let Some(ch) = self.last_char {
                    let mut s = String::new();
                    for _ in 0..num(p, 0).min(1 << 15) {
                        s.push(ch);
                    }
                    self.print_text(&s);
                }
            }
            b'c' => {
                if p.get_or(0, 0) == 0 {
                    let attrs = if self.cfg.sixel_advertised {
                        "62;4;22"
                    } else {
                        "62;22"
                    };
                    self.reply_str(format!("\x1b[?{attrs}c"));
                }
            }
            b'g' => match p.get_or(0, 0) {
                0 => {
                    let x = self.x;
                    self.tabs.clear(x);
                }
                3 => self.tabs.clear_all(),
                _ => {}
            },
            b'h' | b'l' => {
                for i in 0..p.len() {
                    if let Some(v) = p.get(i)
                        && let Ok(n) = u16::try_from(v)
                    {
                        self.set_mode(true, n, fin == b'h');
                    }
                }
            }
            b'm' => sgr::apply(&mut self.pen, p),
            b'n' => match p.get(0) {
                Some(5) => self.reply_str("\x1b[0n".to_string()),
                Some(6) => {
                    let (r, c) = self.cpr();
                    self.reply_str(format!("\x1b[{r};{c}R"));
                }
                _ => {}
            },
            b'r' => {
                let t = num(p, 0);
                let b = match p.get(1) {
                    None | Some(0) => self.rows,
                    Some(v) => (v as usize).min(self.rows),
                };
                if t < b {
                    self.top = t - 1;
                    self.bottom = b - 1;
                    self.home();
                }
            }
            b's' => {
                if self.lrmm {
                    let l = num(p, 0);
                    let r = match p.get(1) {
                        None | Some(0) => self.cols,
                        Some(v) => (v as usize).min(self.cols),
                    };
                    if l < r {
                        self.left = l - 1;
                        self.right = r - 1;
                        self.home();
                    }
                } else {
                    self.save_cursor();
                }
            }
            b'u' => self.restore_cursor(),
            b't' => self.xtwinops(p),
            _ => {}
        }
    }

    fn csi_dec(&mut self, p: &Params, fin: u8) {
        match fin {
            b'h' | b'l' => {
                for i in 0..p.len() {
                    if let Some(v) = p.get(i)
                        && let Ok(n) = u16::try_from(v)
                    {
                        self.set_mode(false, n, fin == b'h');
                    }
                }
            }
            b'n' => match p.get(0) {
                Some(6) => {
                    let (r, c) = self.cpr();
                    self.reply_str(format!("\x1b[?{r};{c};1R"));
                }
                Some(15) => self.reply_str("\x1b[?13n".to_string()),
                Some(996) => {
                    let v = if self.cfg.dark { 1 } else { 2 };
                    self.reply_str(format!("\x1b[?997;{v}n"));
                }
                _ => {}
            },
            b'u' => {
                let f = self.kitty.current();
                self.reply_str(format!("\x1b[?{f}u"));
            }
            _ => {}
        }
    }

    fn csi_gt(&mut self, p: &Params, fin: u8) {
        match fin {
            b'c' => {
                if p.get_or(0, 0) == 0 {
                    let v = self.cfg.version;
                    let pv = v.0 * 10000 + v.1 * 100 + v.2;
                    self.reply_str(format!("\x1b[>1;{pv};0c"));
                }
            }
            b'q' => {
                if p.get_or(0, 0) == 0 {
                    let v = self.cfg.version;
                    self.reply_str(format!("\x1bP>|Apotheosis {}.{}.{}\x1b\\", v.0, v.1, v.2));
                }
            }
            b'u' => {
                let f = p.get_or(0, 0);
                self.kitty.active_mut().push(f);
                self.publish();
            }
            b'm' if p.get(0) == Some(4) => {
                let lvl = u64::from(p.get_or(1, 0).min(2));
                self.in_bits = (self.in_bits & !(3 << bit::MOK_SHIFT)) | (lvl << bit::MOK_SHIFT);
                self.publish();
            }
            _ => {}
        }
    }

    fn inside_lr_pub(&self) -> bool {
        !self.lrmm || (self.x >= self.left && self.x <= self.right)
    }

    fn lr_right(&self) -> usize {
        if self.lrmm { self.right } else { self.cols - 1 }
    }

    fn set_col(&mut self, n: usize) {
        let base = if self.origin && self.lrmm {
            self.left
        } else {
            0
        };
        let max = if self.origin && self.lrmm {
            self.right
        } else {
            self.cols - 1
        };
        self.x = (base + n - 1).min(max);
        self.wrap = false;
    }

    fn set_row(&mut self, n: usize) {
        let base = if self.origin { self.top } else { 0 };
        let max = if self.origin {
            self.bottom
        } else {
            self.rows - 1
        };
        self.y = (base + n - 1).min(max);
        self.wrap = false;
    }

    fn cpr(&self) -> (usize, usize) {
        let r = self
            .y
            .saturating_sub(if self.origin { self.top } else { 0 })
            + 1;
        let c = self.x.saturating_sub(if self.origin && self.lrmm {
            self.left
        } else {
            0
        }) + 1;
        (r, c)
    }

    fn col_edit(&mut self, n: usize, insert: bool) {
        if !(self.y >= self.top && self.y <= self.bottom && self.inside_lr_pub()) {
            return;
        }
        let (c, x, bg, right) = (self.cur, self.x, self.pen.bg, self.lr_right());
        for y in self.top..=self.bottom {
            if insert {
                self.panes[c].grid.insert_chars(y, x, n, right, bg);
            } else {
                self.panes[c].grid.delete_chars(y, x, n, right, bg);
            }
        }
    }

    fn decscusr(&mut self, ps: u32) {
        if ps > 6 {
            return;
        }
        let shape = match ps {
            0..=2 => 0,
            3 | 4 => 1,
            _ => 2,
        };
        let blink = ps == 0 || ps % 2 == 1;
        self.cursor_style.shape = shape;
        self.cursor_style.blink = blink;
        if let Some(i) = crate::modes::index(false, 12) {
            if blink {
                self.modes |= 1 << i;
            } else {
                self.modes &= !(1 << i);
            }
        }
    }

    fn decrqm(&mut self, private: bool, p: &Params) {
        let n = p.get_or(0, 0);
        let st = match u16::try_from(n) {
            Ok(n16) => self.mode_state(!private, n16) as u8,
            Err(_) => 0,
        };
        let q = if private { "?" } else { "" };
        self.reply_str(format!("\x1b[{q}{n};{st}$y"));
    }

    fn xtwinops(&mut self, p: &Params) {
        let (r, c) = (self.rows, self.cols);
        let (cw, ch) = self.cfg.cell_px;
        match p.get(0) {
            Some(14) => self.reply_str(format!("\x1b[4;{};{}t", r as u32 * ch, c as u32 * cw)),
            Some(16) => self.reply_str(format!("\x1b[6;{ch};{cw}t")),
            Some(18) => self.reply_str(format!("\x1b[8;{r};{c}t")),
            Some(19) => self.reply_str(format!("\x1b[9;{r};{c}t")),
            Some(20) => self.reply_str("\x1b]L\x1b\\".to_string()),
            Some(21) => self.reply_str("\x1b]l\x1b\\".to_string()),
            Some(22) => {
                if self.title_stack.len() < self.cfg.title_stack_max {
                    self.title_stack
                        .push((self.title.clone(), self.icon.clone()));
                }
            }
            Some(23) => {
                if let Some((t, i)) = self.title_stack.pop() {
                    let which = p.get_or(1, 0);
                    if which == 0 || which == 2 {
                        self.title = t;
                        self.events.title = Some(self.title.clone());
                    }
                    if which == 0 || which == 1 {
                        self.icon = i;
                    }
                }
            }
            _ => {}
        }
    }

    fn erase_display(&mut self, mode: u32) {
        let (y, x, rows, cols) = (self.y, self.x, self.rows, self.cols);
        let (c, bg) = (self.cur, self.pen.bg);
        match mode {
            0 => {
                self.panes[c].grid.erase_cells(y, x, cols, bg, false);
                self.panes[c].grid.set_row_flag(y, flags::WRAPPED, false);
                if y + 1 < rows {
                    self.panes[c].grid.erase_rows(y + 1, rows - 1, bg, false);
                }
            }
            1 => {
                if y > 0 {
                    self.panes[c].grid.erase_rows(0, y - 1, bg, false);
                }
                self.panes[c]
                    .grid
                    .erase_cells(y, 0, (x + 1).min(cols), bg, false);
            }
            2 => self.panes[c].grid.erase_rows(0, rows - 1, bg, false),
            3 => self.clear_history(),
            _ => {}
        }
    }

    fn erase_line(&mut self, mode: u32) {
        let (y, x, cols) = (self.y, self.x, self.cols);
        let (c, bg) = (self.cur, self.pen.bg);
        match mode {
            0 => {
                self.panes[c].grid.erase_cells(y, x, cols, bg, false);
                self.panes[c].grid.set_row_flag(y, flags::WRAPPED, false);
            }
            1 => self.panes[c]
                .grid
                .erase_cells(y, 0, (x + 1).min(cols), bg, false),
            2 => {
                self.panes[c].grid.erase_cells(y, 0, cols, bg, false);
                self.panes[c].grid.set_row_flag(y, flags::WRAPPED, false);
            }
            _ => {}
        }
    }

    /// `ED 3`: the grid has no history-clear call yet, so the active rows are copied into a
    /// fresh primary screen (hyperlinks are not carried over).
    fn clear_history(&mut self) {
        let (rows, cols) = (self.rows, self.cols);
        let mut new = apoth_grid::Screen::new(
            rows,
            cols,
            ScreenConfig {
                history: true,
                limits: self.cfg.scrollback,
            },
        );
        let mut row = apoth_grid::SnapRow::default();
        let mut cont = false;
        for y in 0..rows {
            self.panes[0].grid.copy_row(y, &mut row);
            write_row(&mut new, y, &row, cols, cont);
            cont = row.wrapped;
        }
        self.panes[0].grid = new;
    }

    #[cfg(feature = "esctest")]
    fn decrqcra(&mut self, p: &Params) {
        let id = p.get_or(0, 0);
        let (rows, cols) = (self.rows, self.cols);
        let t = num(p, 2).min(rows);
        let l = num(p, 3).min(cols);
        let b = match p.get(4) {
            None | Some(0) => rows,
            Some(v) => (v as usize).min(rows),
        };
        let r = match p.get(5) {
            None | Some(0) => cols,
            Some(v) => (v as usize).min(cols),
        };
        let mut sum: u16 = 0;
        let mut cps = Vec::new();
        for y in t.saturating_sub(1)..b {
            for x in l.saturating_sub(1)..r {
                let g = &self.panes[self.cur].grid;
                if g.cell(y, x).wide() == Wide::SpacerTail {
                    continue;
                }
                g.cell_text(y, x, &mut cps);
                let v = cps.first().copied().unwrap_or(0x20);
                sum = sum.wrapping_add(v as u16);
            }
        }
        let ck = 0u16.wrapping_sub(sum);
        self.reply_str(format!("\x1bP{id}!~{ck:04X}\x1b\\"));
    }
}

/// Copy a snapshot row into `new` at row `y` (truncating at `cols`).
pub(crate) fn write_row(
    new: &mut apoth_grid::Screen,
    y: usize,
    row: &apoth_grid::SnapRow,
    cols: usize,
    cont: bool,
) {
    let mut x = 0;
    while x < row.cells.len().min(cols) {
        let c = row.cells[x];
        let w = match c.wide {
            Wide::SpacerTail | Wide::SpacerHead => {
                x += 1;
                continue;
            }
            Wide::Wide => 2,
            Wide::Narrow => 1,
        };
        if x + w > cols {
            break;
        }
        let attrs = CellAttrs {
            style: c.style,
            link: None,
            protected: c.protected,
            semantic: c.semantic,
        };
        if c.cluster_len > 0 {
            new.print_cluster(y, x, row.cluster(&c), w as u8, &attrs);
        } else if c.cp != 0 {
            new.print_run(
                y,
                x,
                &[Glyph {
                    cp: c.cp,
                    width: w as u8,
                }],
                &attrs,
            );
        } else if c.style.bg != Color::Default {
            new.erase_cells(y, x, x + 1, c.style.bg, false);
        }
        x += w;
    }
    if row.wrapped {
        new.set_row_flag(y, flags::WRAPPED, true);
    }
    if cont {
        new.set_row_flag(y, flags::WRAP_CONT, true);
    }
    if row.prompt_start {
        new.set_row_flag(y, flags::PROMPT_START, true);
    }
    if row.block != 0 {
        new.set_row_block(y, row.block);
    }
}
