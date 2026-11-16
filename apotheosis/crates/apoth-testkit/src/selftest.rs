// SPDX-License-Identifier: AGPL-3.0-only
//! `MiniTerm`: a deliberately tiny stateful terminal used only to test the harness itself
//! (T-25 §9): that a wrong fixture fails, that chunk dependence is detected, that bless and
//! replay work. It is not a model of the real terminal.
//!
//! Supports: printable ASCII, CR, LF (scrolls into history), BS, BEL (event `bell`),
//! `CSI n C`, `CSI c` (reply `CSI ? 62 ; 22 c`), `CSI 6 n` (CPR), `CSI ? n $ p`
//! (DECRPM: mode 25 set, others unknown), `OSC 2 ; text BEL|ST` (event `title:text`).
//! With `broken` set, an ESC that ends a chunk is mistaken for text, so results depend on
//! chunking, which the harness must catch.

use crate::runner::Driver;
use crate::snapshot::{CellInfo, CursorInfo, Snapshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum St {
    Ground,
    Esc,
    Csi,
    Osc,
    OscEsc,
}

/// The harness self-test terminal.
#[derive(Debug)]
pub struct MiniTerm {
    rows: usize,
    cols: usize,
    grid: Vec<Vec<char>>,
    cy: usize,
    cx: usize,
    hist: Vec<String>,
    st: St,
    buf: String,
    replies: Vec<u8>,
    events: Vec<String>,
    /// Make results depend on chunking (for the meta-test).
    pub broken: bool,
}

impl MiniTerm {
    /// A terminal that mishandles chunk boundaries.
    #[must_use]
    pub fn broken(rows: usize, cols: usize) -> MiniTerm {
        let mut t = <MiniTerm as Driver>::create(rows, cols, None);
        t.broken = true;
        t
    }

    fn line_feed(&mut self) {
        if self.cy + 1 == self.rows {
            let top: String = self.grid.remove(0).iter().collect();
            self.hist.push(top.trim_end_matches(' ').to_string());
            self.grid.push(vec![' '; self.cols]);
        } else {
            self.cy += 1;
        }
    }

    fn csi(&mut self, params: &str, fin: char) {
        let n = params.parse::<usize>().unwrap_or(1).max(1);
        match fin {
            'C' => self.cx = (self.cx + n).min(self.cols - 1),
            'c' if params.is_empty() || params == "0" => {
                self.replies.extend_from_slice(b"\x1b[?62;22c")
            }
            'n' if params == "6" => {
                self.replies
                    .extend_from_slice(format!("\x1b[{};{}R", self.cy + 1, self.cx + 1).as_bytes());
            }
            'p' if params.starts_with('?') && params.ends_with('$') => {
                let m = params.trim_start_matches('?').trim_end_matches('$');
                let v = if m == "25" { 1 } else { 0 };
                self.replies
                    .extend_from_slice(format!("\x1b[?{m};{v}$y").as_bytes());
            }
            _ => {}
        }
    }

    fn put(&mut self, c: char) {
        if self.cx >= self.cols {
            self.cx = 0;
            self.line_feed();
        }
        self.grid[self.cy][self.cx] = c;
        self.cx += 1;
    }

    fn step(&mut self, b: u8, last_in_chunk: bool) {
        let c = b as char;
        match self.st {
            St::Ground => match b {
                0x1b if !(self.broken && last_in_chunk) => self.st = St::Esc,
                b'\r' => self.cx = 0,
                b'\n' => self.line_feed(),
                0x08 => self.cx = self.cx.saturating_sub(1),
                0x07 => self.events.push("bell".into()),
                0x20..=0x7e => self.put(c),
                0x1b => self.put('^'),
                _ => {}
            },
            St::Esc => match b {
                b'[' => {
                    self.buf.clear();
                    self.st = St::Csi;
                }
                b']' => {
                    self.buf.clear();
                    self.st = St::Osc;
                }
                _ => self.st = St::Ground,
            },
            St::Csi => {
                if (0x40..=0x7e).contains(&b) {
                    let p = std::mem::take(&mut self.buf);
                    self.csi(&p, c);
                    self.st = St::Ground;
                } else {
                    self.buf.push(c);
                }
            }
            St::Osc => match b {
                0x07 => self.end_osc(),
                0x1b => self.st = St::OscEsc,
                _ => self.buf.push(c),
            },
            St::OscEsc => {
                if b == b'\\' {
                    self.end_osc();
                } else {
                    self.st = St::Ground;
                }
            }
        }
    }

    fn end_osc(&mut self) {
        let s = std::mem::take(&mut self.buf);
        if let Some(t) = s.strip_prefix("2;") {
            self.events.push(format!("title:{t}"));
        }
        self.st = St::Ground;
    }
}

impl Driver for MiniTerm {
    fn create(rows: usize, cols: usize, _version: Option<&str>) -> Self {
        MiniTerm {
            rows,
            cols,
            grid: vec![vec![' '; cols]; rows],
            cy: 0,
            cx: 0,
            hist: Vec::new(),
            st: St::Ground,
            buf: String::new(),
            replies: Vec::new(),
            events: Vec::new(),
            broken: false,
        }
    }

    fn feed(&mut self, bytes: &[u8], _at_ms: u64) {
        for (i, &b) in bytes.iter().enumerate() {
            self.step(b, i + 1 == bytes.len());
        }
    }

    fn resize(&mut self, rows: usize, cols: usize) {
        self.grid.resize(rows, vec![' '; cols]);
        for r in &mut self.grid {
            r.resize(cols, ' ');
        }
        self.rows = rows;
        self.cols = cols;
        self.cy = self.cy.min(rows - 1);
        self.cx = self.cx.min(cols - 1);
    }

    fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.replies)
    }

    fn take_events(&mut self) -> Vec<String> {
        std::mem::take(&mut self.events)
    }

    fn snapshot(&self) -> Snapshot {
        let cells = self
            .grid
            .iter()
            .map(|r| {
                r.iter()
                    .map(|&c| CellInfo {
                        ch: if c == ' ' {
                            String::new()
                        } else {
                            c.to_string()
                        },
                        ..CellInfo::blank()
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        Snapshot {
            rows: self.rows,
            cols: self.cols,
            cells,
            cursor: CursorInfo {
                row: self.cy,
                col: self.cx,
                visible: true,
                shape: 0,
            },
            scrollback: self.hist.clone(),
            images: Vec::new(),
            alt: false,
            hold: false,
        }
    }
}
