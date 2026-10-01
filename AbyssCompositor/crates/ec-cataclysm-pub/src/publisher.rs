// SPDX-License-Identifier: Apache-2.0
//! The grid publisher: P-04 §2 node model, §3 command blocks, §6.1 echo-off
//! withholding, §7 publish-rate policy and line diffing.
//!
//! The emulator pushes grid state in (`set_line`, `scroll`, `set_cursor`,
//! OSC 133 payloads, pty `ECHO`), then calls [`Publisher::poll`]. A poll that
//! is due returns one [`Batch`]: an ordered list of `eclipse_semantic_v1`
//! requests (COMP-09 §2) ending in `commit`, which the caller forwards to the
//! wire verbatim. The caller makes no protocol decisions of its own.
//!
//! # Node model
//!
//! - `terminal` root, id [`ROOT_ID`], carrying `ext` `rows`, `cols`,
//!   `cursor`, `alt_screen`, `echo`.
//! - One `terminal_line` per **screen row**, id `SLOT_BASE + row`. A line node
//!   is a row slot, so a scrolling log rewrites values (a `value_rev` bump)
//!   and never adds or removes nodes: P-04 §7 "a scrolling log does not
//!   invalidate node ids", P-07 §2 "value text change → value_rev only".
//! - One `group` per OSC 133 command block, ids from [`GROUP_BASE`], never
//!   reused. Each screen row is a child of the block that owns its line, or
//!   of the root. Rows move between parents only when a block boundary
//!   crosses the screen, so `yes` in one command bumps no generation after
//!   the first screenful.
//!
//! # Rate policy (P-04 §7)
//!
//! Boundary triggers — OSC 133 `A` (new prompt), `C`, `D`, alternate-screen
//! toggle, resize, idle (no output for 100 ms), downgrade after echo-off —
//! publish at most every 100 ms (the P-07 §3 10-bumps/s cap). Output alone
//! publishes at most every 200 ms (5 Hz). A raise to echo-off is published by
//! the very next poll, ahead of every limit. Triggers are delayed, never
//! dropped: the next due publish diffs the whole model.

use std::collections::VecDeque;
use std::ffi::CStr;
use std::fmt::{self, Write as _};

use crate::semantic::{OpKind, Role};

/// Root `terminal` node id.
pub const ROOT_ID: u32 = 1;
/// Screen row `r` is node `SLOT_BASE + r`.
pub const SLOT_BASE: u32 = 0x100;
/// First command-block `group` id. Ids increase and are never reused.
pub const GROUP_BASE: u32 = 0x1_0000;
/// Largest accepted grid.
pub const MAX_ROWS: u16 = 1024;
pub const MAX_COLS: u16 = 4096;
/// A line's text is cut at this many bytes, on a char boundary, so one
/// `set_value_text` stays inside a 4 KiB Wayland message.
pub const MAX_LINE_BYTES: usize = 3072;
/// COMP-09 §7 item 3: `ext` values are at most 256 bytes.
pub const EXT_VALUE_MAX: usize = 256;
/// Most command blocks tracked at once; the oldest is dropped beyond it.
pub const MAX_BLOCKS: usize = 64;

/// Batch flag: the batch adds, removes or moves a node (a generation bump).
pub const BATCH_STRUCTURAL: u32 = 1 << 0;
/// Batch flag: the batch announces an echo-off raise (P-04 §6.1).
pub const BATCH_URGENT: u32 = 1 << 1;

const MS: u64 = 1_000_000;

/// Timing policy. Every field is a floor: a value below the default is
/// raised to the default, so a caller can slow publishing down or lengthen
/// the grace, never speed up or shorten it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// Minimum spacing of output-driven publishes. Default 200 ms (5 Hz).
    pub min_interval_ns: u64,
    /// Minimum spacing of boundary-driven publishes. Default 100 ms.
    pub boundary_interval_ns: u64,
    /// Output quiet time that counts as idle. Default 100 ms.
    pub idle_ns: u64,
    /// Echo must stay on this long before the raise drops. Default 500 ms
    /// (S-05 §5.2 `downgrade_grace`).
    pub downgrade_grace_ns: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            min_interval_ns: 200 * MS,
            boundary_interval_ns: 100 * MS,
            idle_ns: 100 * MS,
            downgrade_grace_ns: 500 * MS,
        }
    }
}

impl Config {
    fn clamped(self) -> Self {
        let d = Self::default();
        Self {
            min_interval_ns: self.min_interval_ns.max(d.min_interval_ns),
            boundary_interval_ns: self.boundary_interval_ns.max(d.boundary_interval_ns),
            idle_ns: self.idle_ns.max(d.idle_ns),
            downgrade_grace_ns: self.downgrade_grace_ns.max(d.downgrade_grace_ns),
        }
    }
}

/// A rejected call. Nothing is mutated when one is returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Row, column or grid size outside the accepted range.
    Range,
}

/// One `eclipse_semantic_v1` request. Unused fields are zero / `-1`.
#[derive(Clone, Copy, Debug)]
pub struct Op {
    pub kind: OpKind,
    pub node: u32,
    /// `add_node`/`move_node` parent.
    pub parent: u32,
    /// `add_node`/`move_node` index among the parent's children.
    pub index: u32,
    /// `set_role` role code.
    pub role: u32,
    /// `set_value_text` cursor, `-1` when the cursor is not on this line.
    pub cursor: i32,
    pub sel_start: i32,
    pub sel_end: i32,
    /// `set_ext` key (bare: the `ext.` namespace is the request itself).
    pub key: Option<&'static CStr>,
    str_off: u32,
    str_len: u32,
    has_str: bool,
}

impl Op {
    fn new(kind: OpKind, node: u32) -> Self {
        Self {
            kind,
            node,
            parent: 0,
            index: 0,
            role: 0,
            cursor: -1,
            sel_start: -1,
            sel_end: -1,
            key: None,
            str_off: 0,
            str_len: 0,
            has_str: false,
        }
    }
}

/// A due publish. Borrowed from the publisher: it lives until the next call
/// that takes the publisher mutably.
pub struct Batch<'a> {
    ops: &'a [Op],
    arena: &'a [u8],
    /// Publish sequence number, starting at 1.
    pub seq: u64,
    /// `BATCH_*` flags.
    pub flags: u32,
}

impl<'a> Batch<'a> {
    pub fn ops(&self) -> &'a [Op] {
        self.ops
    }

    /// The string argument of `op` (`set_value_text` text, `set_ext` value).
    pub fn str_of(&self, op: &Op) -> Option<&'a str> {
        self.bytes_with_nul(op)
            .map(|b| std::str::from_utf8(&b[..b.len() - 1]).unwrap_or(""))
    }

    /// The string argument including its NUL terminator.
    pub(crate) fn bytes_with_nul(&self, op: &Op) -> Option<&'a [u8]> {
        if !op.has_str {
            return None;
        }
        let start = op.str_off as usize;
        self.arena.get(start..start + op.str_len as usize + 1)
    }
}

/// Counters for tests and the throughput fixture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Heap bytes the publisher holds (capacities, not lengths).
    pub retained_bytes: u64,
    pub publishes: u64,
    pub structural_publishes: u64,
    pub blocks: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// After `A`.
    Prompt,
    /// After `B`.
    Input,
    /// After `C`.
    Running,
    /// After `D`, or closed by the next `A`.
    Done,
}

struct Block {
    id: u32,
    /// Absolute line of the `A` mark.
    start: u64,
    /// Last absolute line, once closed.
    end: Option<u64>,
    /// Absolute line and column of the `B` mark.
    input: Option<(u64, u16)>,
    phase: Phase,
    command: String,
    command_set: bool,
    command_truncated: bool,
    exit_code: Option<i32>,
    started_ns: Option<u64>,
    ended_ns: Option<u64>,
    published: bool,
    pub_is_prompt: Option<bool>,
    pub_command: bool,
    pub_exit: bool,
    pub_started: bool,
    pub_ended: bool,
    pub_children: Vec<u32>,
}

impl Block {
    fn new(id: u32, start: u64) -> Self {
        Self {
            id,
            start,
            end: None,
            input: None,
            phase: Phase::Prompt,
            command: String::new(),
            command_set: false,
            command_truncated: false,
            exit_code: None,
            started_ns: None,
            ended_ns: None,
            published: false,
            pub_is_prompt: None,
            pub_command: false,
            pub_exit: false,
            pub_started: false,
            pub_ended: false,
            pub_children: Vec::new(),
        }
    }

    /// An open block reaches down to the cursor line, not past it: rows
    /// nothing has been written to yet belong to the root.
    fn covers(&self, line: u64, cursor_line: u64) -> bool {
        self.start <= line && line <= self.end.unwrap_or(cursor_line)
    }

    fn is_prompt(&self) -> bool {
        matches!(self.phase, Phase::Prompt | Phase::Input)
    }
}

struct Screen {
    lines: Vec<String>,
    /// Absolute line number of row 0 (primary screen only advances it).
    top: u64,
}

struct Slot {
    pub_text: String,
    pub_cursor: i32,
    pub_parent: u32,
    pub_is_prompt: bool,
}

#[derive(Default)]
struct Out {
    ops: Vec<Op>,
    arena: Vec<u8>,
}

struct ArenaWriter<'a>(&'a mut Vec<u8>);

impl fmt::Write for ArenaWriter<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

impl Out {
    fn clear(&mut self) {
        self.ops.clear();
        self.arena.clear();
    }

    fn op(&mut self, op: Op) {
        self.ops.push(op);
    }

    fn tree(&mut self, kind: OpKind, node: u32, parent: u32, index: usize) {
        let mut op = Op::new(kind, node);
        op.parent = parent;
        op.index = u32::try_from(index).unwrap_or(u32::MAX);
        self.op(op);
    }

    fn role(&mut self, node: u32, role: Role) {
        let mut op = Op::new(OpKind::SetRole, node);
        op.role = role as u32;
        self.op(op);
    }

    fn with_str(&mut self, mut op: Op, args: fmt::Arguments<'_>) {
        let start = self.arena.len();
        let _ = ArenaWriter(&mut self.arena).write_fmt(args);
        let len = self.arena.len() - start;
        self.arena.push(0);
        op.str_off = start as u32;
        op.str_len = len as u32;
        op.has_str = true;
        self.op(op);
    }

    fn ext(&mut self, node: u32, key: &'static CStr, args: fmt::Arguments<'_>) {
        let mut op = Op::new(OpKind::SetExt, node);
        op.key = Some(key);
        self.with_str(op, args);
    }

    fn text(&mut self, node: u32, text: &str, cursor: i32) {
        let mut op = Op::new(OpKind::SetValueText, node);
        op.cursor = cursor;
        self.with_str(op, format_args!("{text}"));
    }
}

fn bool_str(b: bool) -> &'static str {
    if b {
        "true"
    } else {
        "false"
    }
}

fn slot_id(row: usize) -> u32 {
    SLOT_BASE + row as u32
}

/// Cut `s` to at most `max` bytes on a char boundary. True if anything went.
fn truncate_at(s: &mut String, max: usize) -> bool {
    if s.len() <= max {
        return false;
    }
    let mut i = max;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    s.truncate(i);
    true
}

/// Copy grid bytes into `dst`: invalid UTF-8 becomes U+FFFD, C0/C1 controls
/// and DEL become spaces (wire strings cannot carry NUL), trailing spaces go,
/// and the result is capped at [`MAX_LINE_BYTES`]. Reuses `dst`'s buffer.
fn fill_sanitized(dst: &mut String, bytes: &[u8]) {
    dst.clear();
    let push = |dst: &mut String, ch: char| {
        let ch = if ch.is_control() { ' ' } else { ch };
        if dst.len() + ch.len_utf8() > MAX_LINE_BYTES {
            return false;
        }
        dst.push(ch);
        true
    };
    'outer: for chunk in bytes.utf8_chunks() {
        for ch in chunk.valid().chars() {
            if !push(dst, ch) {
                break 'outer;
            }
        }
        if !chunk.invalid().is_empty() && !push(dst, char::REPLACEMENT_CHARACTER) {
            break;
        }
    }
    let keep = dst.trim_end_matches(' ').len();
    dst.truncate(keep);
}

/// Bring `published` to `desired` for `parent`. A parent that only lost
/// children needs no request (the gaining parent's `move_node` covers it);
/// otherwise every child is re-placed in order. True if requests went out.
fn reconcile(out: &mut Out, parent: u32, desired: &[u32], published: &mut Vec<u32>) -> bool {
    if desired == published.as_slice() {
        return false;
    }
    let mut it = published.iter();
    let lost_only = desired.iter().all(|d| it.any(|p| p == d));
    if !lost_only {
        for (i, &child) in desired.iter().enumerate() {
            out.tree(OpKind::MoveNode, child, parent, i);
        }
    }
    published.clear();
    published.extend_from_slice(desired);
    !lost_only
}

/// The publisher for one terminal toplevel.
pub struct Publisher {
    cfg: Config,
    rows: u16,
    cols: u16,
    /// `[primary, alternate]`.
    screens: [Screen; 2],
    alt: bool,
    cursor: (u16, u16),

    echo: bool,
    raised: bool,
    raise_announce: bool,
    echo_on_since: Option<u64>,

    blocks: VecDeque<Block>,
    dead: Vec<Block>,
    next_group: Option<u32>,

    root_published: bool,
    root_children: Vec<u32>,
    slots: Vec<Slot>,
    pub_rows: u16,
    pub_cols: u16,
    pub_cursor: Option<(u16, u16)>,
    pub_alt: Option<bool>,
    pub_echo: Option<bool>,

    dirty: bool,
    boundary: bool,
    output_seen: bool,
    last_publish: Option<u64>,
    last_output: u64,

    out: Out,
    seq: u64,
    flags: u32,
    stats: Stats,

    scratch_line: String,
    scratch_owner: Vec<u32>,
    scratch_keys: Vec<(u64, u8, u32)>,
    scratch_ids: Vec<u32>,
}

impl Publisher {
    /// A publisher for a `rows`×`cols` grid. The first poll publishes the
    /// initial tree.
    pub fn new(cfg: Config, rows: u16, cols: u16) -> Result<Self, Error> {
        if !(1..=MAX_ROWS).contains(&rows) || !(1..=MAX_COLS).contains(&cols) {
            return Err(Error::Range);
        }
        let screen = || Screen {
            lines: (0..rows).map(|_| String::new()).collect(),
            top: 0,
        };
        Ok(Self {
            cfg: cfg.clamped(),
            rows,
            cols,
            screens: [screen(), screen()],
            alt: false,
            cursor: (0, 0),
            echo: true,
            raised: false,
            raise_announce: false,
            echo_on_since: None,
            blocks: VecDeque::new(),
            dead: Vec::new(),
            next_group: Some(GROUP_BASE),
            root_published: false,
            root_children: Vec::new(),
            slots: Vec::new(),
            pub_rows: 0,
            pub_cols: 0,
            pub_cursor: None,
            pub_alt: None,
            pub_echo: None,
            dirty: true,
            boundary: true,
            output_seen: false,
            last_publish: None,
            last_output: 0,
            out: Out::default(),
            seq: 0,
            flags: 0,
            stats: Stats::default(),
            scratch_line: String::new(),
            scratch_owner: Vec::new(),
            scratch_keys: Vec::new(),
            scratch_ids: Vec::new(),
        })
    }

    fn screen(&mut self) -> &mut Screen {
        &mut self.screens[usize::from(self.alt)]
    }

    /// Grid resized. Rows beyond the new size are dropped, new rows are
    /// empty; the emulator re-sends what reflowed. The cursor is clamped.
    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<(), Error> {
        if !(1..=MAX_ROWS).contains(&rows) || !(1..=MAX_COLS).contains(&cols) {
            return Err(Error::Range);
        }
        if (rows, cols) == (self.rows, self.cols) {
            return Ok(());
        }
        for s in &mut self.screens {
            s.lines.resize_with(usize::from(rows), String::new);
        }
        self.rows = rows;
        self.cols = cols;
        self.cursor = (self.cursor.0.min(rows - 1), self.cursor.1.min(cols - 1));
        self.dirty = true;
        self.boundary = true;
        self.output_seen = true;
        Ok(())
    }

    /// Replace the text of screen row `row` on the active screen.
    pub fn set_line(&mut self, row: u16, utf8: &[u8]) -> Result<(), Error> {
        if row >= self.rows {
            return Err(Error::Range);
        }
        let mut scratch = std::mem::take(&mut self.scratch_line);
        fill_sanitized(&mut scratch, utf8);
        let line = &mut self.screen().lines[usize::from(row)];
        if *line != scratch {
            std::mem::swap(line, &mut scratch);
            self.dirty = true;
        }
        self.scratch_line = scratch;
        self.output_seen = true;
        Ok(())
    }

    /// The top `n` rows of the active screen scrolled away; the bottom `n`
    /// are now empty. On the primary screen this advances absolute lines.
    pub fn scroll(&mut self, n: u16) {
        if n == 0 {
            return;
        }
        let rows = usize::from(self.rows);
        let alt = self.alt;
        let s = self.screen();
        let n = usize::from(n).min(rows);
        s.lines.rotate_left(n);
        for l in &mut s.lines[rows - n..] {
            l.clear();
        }
        if !alt {
            s.top += n as u64;
        }
        self.dirty = true;
        self.output_seen = true;
    }

    pub fn set_cursor(&mut self, row: u16, col: u16) -> Result<(), Error> {
        if row >= self.rows || col >= self.cols {
            return Err(Error::Range);
        }
        if self.cursor != (row, col) {
            self.cursor = (row, col);
            self.dirty = true;
        }
        Ok(())
    }

    /// Alternate screen entered or left (P-04 §5 `ext.alt_screen`).
    pub fn set_alt_screen(&mut self, on: bool) {
        if self.alt != on {
            self.alt = on;
            self.dirty = true;
            self.boundary = true;
            self.output_seen = true;
        }
    }

    /// The pty's `ECHO` flag (P-04 §6.1). Turning it off raises the
    /// terminal at once: the next poll publishes `ext.echo=false` and
    /// nothing else, and every poll after it publishes nothing until echo
    /// has been back on for `downgrade_grace`.
    pub fn set_echo(&mut self, on: bool, now_ns: u64) {
        if !on {
            self.echo = false;
            self.echo_on_since = None;
            if !self.raised {
                self.raised = true;
                self.raise_announce = true;
            }
        } else if !self.echo {
            self.echo = true;
            if self.raised {
                self.echo_on_since = Some(now_ns);
            }
        }
    }

    /// An OSC 133 payload, with or without its leading `133;`. Unknown or
    /// malformed payloads are ignored: the bytes come from whatever the
    /// terminal is running.
    pub fn osc133(&mut self, payload: &[u8], now_ns: u64) {
        let p = payload.strip_prefix(b"133;").unwrap_or(payload);
        let Some((&kind, rest)) = p.split_first() else {
            return;
        };
        if !(rest.is_empty() || rest[0] == b';') || self.alt {
            return;
        }
        let line = self.screens[0].top + u64::from(self.cursor.0);
        match kind {
            b'A' => self.mark_prompt(line),
            b'B' => {
                if let Some(b) = self.blocks.back_mut().filter(|b| b.phase == Phase::Prompt) {
                    b.input = Some((line, self.cursor.1));
                    b.phase = Phase::Input;
                    self.dirty = true;
                }
            }
            b'C' => self.mark_command(line, now_ns),
            b'D' => {
                let exit = rest
                    .get(1..)
                    .and_then(|r| r.split(|&c| c == b';').next())
                    .and_then(|r| std::str::from_utf8(r).ok())
                    .and_then(|r| r.parse::<i32>().ok());
                let col = self.cursor.1;
                if let Some(b) = self.blocks.back_mut().filter(|b| b.phase != Phase::Done) {
                    let last = if col == 0 { line.saturating_sub(1) } else { line };
                    b.end = Some(last.max(b.start));
                    b.exit_code = exit;
                    b.ended_ns = Some(now_ns);
                    b.phase = Phase::Done;
                    self.boundary = true;
                    self.dirty = true;
                }
            }
            _ => {}
        }
    }

    fn mark_prompt(&mut self, line: u64) {
        self.boundary = true;
        self.dirty = true;
        // A redrawn prompt (zsh reset-prompt, a resize) re-marks `A` with no
        // command in between: move the open prompt, do not open a block.
        if let Some(b) = self.blocks.back_mut().filter(|b| b.is_prompt()) {
            b.start = line;
            b.input = None;
            b.phase = Phase::Prompt;
            return;
        }
        let Some(id) = self.next_group else {
            return;
        };
        self.next_group = id.checked_add(1);
        // Close the previous block above the new prompt; a block that
        // started at or below it was overwritten (`clear`) and goes.
        while let Some(prev) = self.blocks.back_mut() {
            if prev.start >= line {
                let b = self.blocks.pop_back().expect("back exists");
                if b.published {
                    self.dead.push(b);
                }
                continue;
            }
            let end = prev.end.unwrap_or(line - 1).min(line - 1);
            prev.end = Some(end);
            prev.phase = Phase::Done;
            break;
        }
        self.blocks.push_back(Block::new(id, line));
        while self.blocks.len() > MAX_BLOCKS {
            let b = self.blocks.pop_front().expect("non-empty");
            if b.published {
                self.dead.push(b);
            }
        }
    }

    fn mark_command(&mut self, line: u64, now_ns: u64) {
        let open = self.blocks.back().is_some_and(|b| b.is_prompt());
        if !open {
            // `C` with no prompt mark: a block with no command text.
            self.mark_prompt(line);
        }
        let rows = u64::from(self.rows);
        let col = self.cursor.1;
        let screen = &self.screens[0];
        let Some(b) = self.blocks.back_mut().filter(|b| b.is_prompt()) else {
            return;
        };
        b.phase = Phase::Running;
        b.started_ns = Some(now_ns);
        if let Some((lb, cb)) = b.input {
            let top = screen.top;
            let mut truncated = false;
            let (first, first_col) = if lb < top {
                truncated = true;
                (top, 0)
            } else {
                (lb, usize::from(cb))
            };
            let last = if col == 0 && line > first { line - 1 } else { line }.max(first);
            b.command.clear();
            for l in first..=last.min(top + rows - 1) {
                let text = &screen.lines[(l - top) as usize];
                if l == first {
                    let at = text.char_indices().nth(first_col).map_or(text.len(), |(i, _)| i);
                    b.command.push_str(&text[at..]);
                } else {
                    b.command.push('\n');
                    b.command.push_str(text);
                }
            }
            let keep = b.command.trim_end().len();
            b.command.truncate(keep);
            truncated |= truncate_at(&mut b.command, EXT_VALUE_MAX);
            b.command_set = true;
            b.command_truncated = truncated;
        }
        self.boundary = true;
        self.dirty = true;
    }

    /// Run the rate policy at `now_ns` (monotonic). Call it after every batch
    /// of input, and when [`Publisher::next_deadline`] expires.
    pub fn poll(&mut self, now_ns: u64) -> Option<Batch<'_>> {
        if self.raise_announce {
            self.raise_announce = false;
            let flags = self.build_raise();
            return Some(self.finish(now_ns, flags));
        }
        if self.output_seen {
            self.output_seen = false;
            self.last_output = now_ns;
        }
        if self.raised {
            match self.echo_on_since {
                Some(t) if now_ns.saturating_sub(t) >= self.cfg.downgrade_grace_ns => {
                    self.raised = false;
                    self.echo_on_since = None;
                    self.boundary = true;
                    self.dirty = true;
                }
                _ => return None,
            }
        }
        let since = self.last_publish.map_or(u64::MAX, |t| now_ns.saturating_sub(t));
        let idle = self.dirty && now_ns.saturating_sub(self.last_output) >= self.cfg.idle_ns;
        let due = ((self.boundary || idle) && since >= self.cfg.boundary_interval_ns)
            || (self.dirty && since >= self.cfg.min_interval_ns);
        if !due {
            return None;
        }
        self.boundary = false;
        self.dirty = false;
        let flags = self.build();
        if self.out.ops.is_empty() {
            return None;
        }
        Some(self.finish(now_ns, flags))
    }

    /// When the next poll could publish, in the clock `poll` is given.
    /// `0` means poll now; `u64::MAX` means nothing is pending.
    pub fn next_deadline(&self) -> u64 {
        if self.raise_announce {
            return 0;
        }
        if self.raised {
            return self
                .echo_on_since
                .map_or(u64::MAX, |t| t.saturating_add(self.cfg.downgrade_grace_ns));
        }
        if self.output_seen {
            return 0;
        }
        let lp = self.last_publish.unwrap_or(0);
        let boundary_at = lp.saturating_add(self.cfg.boundary_interval_ns);
        let mut d = u64::MAX;
        if self.boundary {
            d = d.min(if self.last_publish.is_none() {
                0
            } else {
                boundary_at
            });
        }
        if self.dirty {
            d = d.min(lp.saturating_add(self.cfg.min_interval_ns));
            d = d.min(self.last_output.saturating_add(self.cfg.idle_ns).max(boundary_at));
        }
        d
    }

    pub fn stats(&self) -> Stats {
        let strings = |v: &[String]| v.iter().map(|s| s.capacity()).sum::<usize>();
        let mut bytes = 0usize;
        for s in &self.screens {
            bytes += strings(&s.lines) + s.lines.capacity() * size_of::<String>();
        }
        bytes += self.slots.iter().map(|s| s.pub_text.capacity()).sum::<usize>();
        bytes += self.slots.capacity() * size_of::<Slot>();
        for b in self.blocks.iter().chain(&self.dead) {
            bytes += b.command.capacity() + b.pub_children.capacity() * 4;
        }
        bytes += (self.blocks.capacity() + self.dead.capacity()) * size_of::<Block>();
        bytes += self.out.ops.capacity() * size_of::<Op>() + self.out.arena.capacity();
        bytes += self.root_children.capacity() * 4 + self.scratch_line.capacity();
        bytes += self.scratch_owner.capacity() * 4 + self.scratch_ids.capacity() * 4;
        bytes += self.scratch_keys.capacity() * size_of::<(u64, u8, u32)>();
        Stats {
            retained_bytes: bytes as u64,
            blocks: self.blocks.len() as u64,
            ..self.stats
        }
    }

    pub(crate) fn last_batch(&self) -> Batch<'_> {
        Batch {
            ops: &self.out.ops,
            arena: &self.out.arena,
            seq: self.seq,
            flags: self.flags,
        }
    }

    fn finish(&mut self, now_ns: u64, flags: u32) -> Batch<'_> {
        self.out.op(Op::new(OpKind::Commit, 0));
        self.last_publish = Some(now_ns);
        self.seq += 1;
        self.flags = flags;
        self.stats.publishes += 1;
        if flags & BATCH_STRUCTURAL != 0 {
            self.stats.structural_publishes += 1;
        }
        self.last_batch()
    }

    fn publish_root(&mut self) -> bool {
        if self.root_published {
            return false;
        }
        self.out.op(Op::new(OpKind::SetRoot, ROOT_ID));
        self.out.role(ROOT_ID, Role::Terminal);
        self.root_published = true;
        true
    }

    /// The echo-off announcement: `ext.echo=false` on the root and nothing
    /// else, so the raise reaches the compositor before any further text.
    fn build_raise(&mut self) -> u32 {
        self.out.clear();
        let structural = self.publish_root();
        self.out.ext(ROOT_ID, c"echo", format_args!("false"));
        self.pub_echo = Some(false);
        if structural {
            BATCH_URGENT | BATCH_STRUCTURAL
        } else {
            BATCH_URGENT
        }
    }

    /// Diff the model against the last publish into `self.out`.
    fn build(&mut self) -> u32 {
        self.out.clear();
        let mut structural = self.publish_root();
        let rows = usize::from(self.rows);
        let top = self.screens[0].top;
        let cursor_line = top + u64::from(self.cursor.0);

        // Blocks that scrolled entirely off the screen.
        let mut i = 0;
        while i < self.blocks.len() {
            let b = &self.blocks[i];
            if b.phase == Phase::Done && b.end.is_some_and(|e| e < top) {
                let b = self.blocks.remove(i).expect("index in range");
                if b.published {
                    self.dead.push(b);
                }
            } else {
                i += 1;
            }
        }

        // Row slots follow the grid size.
        while self.slots.len() > rows {
            let slot = self.slots.pop().expect("non-empty");
            let id = slot_id(self.slots.len());
            self.out.tree(OpKind::RemoveNode, id, 0, 0);
            let list = if slot.pub_parent == ROOT_ID {
                Some(&mut self.root_children)
            } else {
                self.blocks
                    .iter_mut()
                    .chain(self.dead.iter_mut())
                    .find(|b| b.id == slot.pub_parent)
                    .map(|b| &mut b.pub_children)
            };
            if let Some(list) = list {
                list.retain(|&c| c != id);
            }
            structural = true;
        }
        while self.slots.len() < rows {
            let row = self.slots.len();
            let id = slot_id(row);
            self.out
                .tree(OpKind::AddNode, id, ROOT_ID, self.root_children.len());
            self.out.role(id, Role::TerminalLine);
            self.out.ext(id, c"row", format_args!("{row}"));
            self.out.ext(id, c"scrollback", format_args!("false"));
            self.out.ext(id, c"is_prompt", format_args!("false"));
            self.root_children.push(id);
            self.slots.push(Slot {
                pub_text: String::new(),
                pub_cursor: -1,
                pub_parent: ROOT_ID,
                pub_is_prompt: false,
            });
            structural = true;
        }
        if self.pub_rows != self.rows {
            self.out.ext(ROOT_ID, c"rows", format_args!("{}", self.rows));
            self.pub_rows = self.rows;
        }
        if self.pub_cols != self.cols {
            self.out.ext(ROOT_ID, c"cols", format_args!("{}", self.cols));
            self.pub_cols = self.cols;
        }

        // New command blocks.
        for b in self.blocks.iter_mut().filter(|b| !b.published) {
            self.out
                .tree(OpKind::AddNode, b.id, ROOT_ID, self.root_children.len());
            self.out.role(b.id, Role::Group);
            self.root_children.push(b.id);
            b.published = true;
            structural = true;
        }

        // Which parent owns each row.
        self.scratch_owner.clear();
        for r in 0..rows {
            let owner = if self.alt {
                ROOT_ID
            } else {
                let line = top + r as u64;
                self.blocks
                    .iter()
                    .rev()
                    .find(|b| b.covers(line, cursor_line))
                    .map_or(ROOT_ID, |b| b.id)
            };
            self.scratch_owner.push(owner);
        }

        // Root: groups and unowned rows in reading order.
        self.scratch_keys.clear();
        for b in &self.blocks {
            let line = if self.alt { 0 } else { b.start };
            self.scratch_keys.push((line, 0, b.id));
        }
        for (r, &owner) in self.scratch_owner.iter().enumerate() {
            if owner == ROOT_ID {
                let line = if self.alt { 1 } else { top + r as u64 };
                self.scratch_keys.push((line, 1, slot_id(r)));
            }
        }
        self.scratch_keys.sort_unstable();
        self.scratch_ids.clear();
        self.scratch_ids.extend(self.scratch_keys.iter().map(|k| k.2));
        structural |= reconcile(&mut self.out, ROOT_ID, &self.scratch_ids, &mut self.root_children);

        for b in &mut self.blocks {
            self.scratch_ids.clear();
            self.scratch_ids.extend(
                self.scratch_owner
                    .iter()
                    .enumerate()
                    .filter(|(_, &o)| o == b.id)
                    .map(|(r, _)| slot_id(r)),
            );
            structural |= reconcile(&mut self.out, b.id, &self.scratch_ids, &mut b.pub_children);
        }
        for (r, &owner) in self.scratch_owner.iter().enumerate() {
            self.slots[r].pub_parent = owner;
        }

        // Dead groups are empty now: every row they held was moved above.
        for b in self.dead.drain(..) {
            if b.published {
                self.out.tree(OpKind::RemoveNode, b.id, 0, 0);
                structural = true;
            }
        }

        // Command-block fields (P-04 §3).
        for b in &mut self.blocks {
            let is_prompt = b.is_prompt();
            if b.pub_is_prompt != Some(is_prompt) {
                self.out
                    .ext(b.id, c"is_prompt", format_args!("{}", bool_str(is_prompt)));
                b.pub_is_prompt = Some(is_prompt);
            }
            if b.command_set && !b.pub_command {
                self.out.ext(b.id, c"command", format_args!("{}", b.command));
                if b.command_truncated {
                    self.out.ext(b.id, c"command_truncated", format_args!("true"));
                }
                b.pub_command = true;
            }
            if let Some(t) = b.started_ns.filter(|_| !b.pub_started) {
                self.out.ext(b.id, c"started", format_args!("{t}"));
                b.pub_started = true;
            }
            if let Some(t) = b.ended_ns.filter(|_| !b.pub_ended) {
                self.out.ext(b.id, c"ended", format_args!("{t}"));
                b.pub_ended = true;
            }
            if let Some(code) = b.exit_code.filter(|_| !b.pub_exit) {
                self.out.ext(b.id, c"exit_code", format_args!("{code}"));
                b.pub_exit = true;
            }
        }

        // Line diffing: only rows whose text or cursor changed.
        let screen = &self.screens[usize::from(self.alt)];
        for (r, slot) in self.slots.iter_mut().enumerate() {
            let owner = self.scratch_owner[r];
            let is_prompt = owner != ROOT_ID && self.blocks.iter().any(|b| b.id == owner && b.is_prompt());
            if slot.pub_is_prompt != is_prompt {
                self.out
                    .ext(slot_id(r), c"is_prompt", format_args!("{}", bool_str(is_prompt)));
                slot.pub_is_prompt = is_prompt;
            }
            let text = &screen.lines[r];
            let cursor = if usize::from(self.cursor.0) == r {
                i32::from(self.cursor.1)
            } else {
                -1
            };
            if slot.pub_text != *text || slot.pub_cursor != cursor {
                self.out.text(slot_id(r), text, cursor);
                slot.pub_text.clear();
                slot.pub_text.push_str(text);
                slot.pub_cursor = cursor;
            }
        }

        // Root fields.
        if self.pub_cursor != Some(self.cursor) {
            let (row, col) = self.cursor;
            self.out.ext(
                ROOT_ID,
                c"cursor",
                format_args!("{{\"row\":{row},\"col\":{col}}}"),
            );
            self.pub_cursor = Some(self.cursor);
        }
        if self.pub_alt != Some(self.alt) {
            self.out
                .ext(ROOT_ID, c"alt_screen", format_args!("{}", bool_str(self.alt)));
            self.pub_alt = Some(self.alt);
        }
        if self.pub_echo != Some(self.echo) {
            self.out
                .ext(ROOT_ID, c"echo", format_args!("{}", bool_str(self.echo)));
            self.pub_echo = Some(self.echo);
        }

        if structural {
            BATCH_STRUCTURAL
        } else {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(p: &mut Publisher, now: u64) -> Option<Vec<OpKind>> {
        p.poll(now).map(|b| b.ops().iter().map(|o| o.kind).collect())
    }

    #[test]
    fn first_poll_publishes_the_tree() {
        let mut p = Publisher::new(Config::default(), 3, 10).unwrap();
        let b = p.poll(0).expect("initial publish");
        assert_eq!(b.ops()[0].kind, OpKind::SetRoot);
        assert_eq!(b.ops().last().unwrap().kind, OpKind::Commit);
        assert!(b.flags & BATCH_STRUCTURAL != 0);
        let adds = b.ops().iter().filter(|o| o.kind == OpKind::AddNode).count();
        assert_eq!(adds, 3);
        assert!(p.poll(1).is_none());
    }

    #[test]
    fn sanitize_strips_controls_and_bad_utf8() {
        let mut s = String::new();
        fill_sanitized(&mut s, b"a\0b\x1b[c\xffd   ");
        assert_eq!(s, "a b [c\u{FFFD}d");
        fill_sanitized(&mut s, &[b'x'; MAX_LINE_BYTES + 50]);
        assert_eq!(s.len(), MAX_LINE_BYTES);
    }

    #[test]
    fn output_is_limited_to_5hz() {
        let mut p = Publisher::new(Config::default(), 2, 10).unwrap();
        p.poll(0).unwrap();
        // Output keeps arriving, so idle never fires: only the 5 Hz limit.
        for (t, text) in [(50, &b"x"[..]), (120, b"xy"), (190, b"xyz")] {
            p.set_line(0, text).unwrap();
            assert!(kinds(&mut p, t * MS).is_none(), "published at {t} ms");
        }
        p.set_line(0, b"xyzw").unwrap();
        let k = kinds(&mut p, 200 * MS).unwrap();
        assert_eq!(k, [OpKind::SetValueText, OpKind::Commit]);
        // Then quiet: idle publishes 100 ms after the last output.
        p.set_line(0, b"done").unwrap();
        assert!(kinds(&mut p, 250 * MS).is_none());
        assert!(kinds(&mut p, 349 * MS).is_none());
        assert!(kinds(&mut p, 350 * MS).is_some());
    }

    #[test]
    fn config_cannot_shorten_the_grace() {
        let cfg = Config {
            downgrade_grace_ns: 0,
            min_interval_ns: 0,
            boundary_interval_ns: 0,
            idle_ns: 0,
        };
        assert_eq!(cfg.clamped(), Config::default());
    }

    #[test]
    fn prompt_redraw_does_not_open_a_block() {
        let mut p = Publisher::new(Config::default(), 4, 20).unwrap();
        p.osc133(b"A", 0);
        p.osc133(b"133;A;aid=1", 0);
        assert_eq!(p.blocks.len(), 1);
    }

    #[test]
    fn malformed_osc133_is_ignored() {
        let mut p = Publisher::new(Config::default(), 4, 20).unwrap();
        for junk in [&b""[..], b"133;", b"Ax", b"Z", b"D;notanumber", b"\xff\xfe"] {
            p.osc133(junk, 0);
        }
        assert!(p.blocks.is_empty());
    }
}
