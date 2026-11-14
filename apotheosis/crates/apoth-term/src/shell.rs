// SPDX-License-Identifier: AGPL-3.0-only
//! OSC 133 marks and command blocks (T-14 §1, SHELLINTEGRATION-01..03, -05, -07..-10).
//!
//! The transition machine is lenient: a primary `A` after a primary `A` with no input
//! discards the empty prompt; `D` with no running block is ignored; a nested shell's
//! primary `A` implicitly ends the outer block. Continuation prompts (`A;k=c|s`,
//! `P;k=c|s`) mark the current row inside the current input zone and never open a block.
//!
//! Command-line text (`cmdline=` / `cmdline_url=`) is held in memory only: it is never
//! written to logs, files or the semantic tree (T-14 §1).

use std::collections::VecDeque;
use std::time::Instant;

use apoth_grid::Pin;

/// Command records kept per pane (T-14 §1).
pub const BLOCKS_CAP: usize = 1000;
/// Longest command line kept, bytes (T-18 §5: every string sink has a budget).
pub const CMDLINE_MAX: usize = 4096;

/// Where a block is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockState {
    /// Prompt drawn (`A`), no input mark yet.
    Prompt,
    /// Input zone open (`B`).
    Input,
    /// Command running (`C`).
    Running,
    /// Finished (`D`, or implicitly by a new primary prompt).
    Done,
}

/// One command record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    /// Row block id (non-zero), also stamped on the block's rows.
    pub id: u32,
    pub state: BlockState,
    /// Where the prompt, input and output zones start, and where the block ended.
    pub prompt: Option<Pin>,
    pub input: Option<Pin>,
    pub output: Option<Pin>,
    pub end: Option<Pin>,
    /// Exit status from `D;<status>`.
    pub exit: Option<i32>,
    pub started: Option<Instant>,
    pub ended: Option<Instant>,
    /// The OSC 7 working directory in force when the command started.
    pub cwd: Option<String>,
    /// Command line from `C;cmdline=` (memory only, never persisted or logged).
    pub cmdline: Option<String>,
}

/// Prompt kind of `A` / `P` (`k=`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptKind {
    /// `k=i` or no `k`: a primary prompt.
    Primary,
    /// `k=r`: a right prompt (prompt cells of the current row).
    Right,
    /// `k=c` / `k=s`: a continuation prompt inside the input zone.
    Continuation,
}

/// Parsed options of one mark. Unknown keys and keys on the wrong mark are ignored; a
/// malformed value ignores that key only (T-14 §1, SHELLINTEGRATION-05).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MarkOpts {
    pub kind: Option<PromptKind>,
    pub redraw: Option<bool>,
    pub special_key: Option<bool>,
    pub click_events: Option<u8>,
    pub cl: Option<String>,
    pub cmdline: Option<String>,
}

fn flag01(v: &str) -> Option<bool> {
    match v {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

/// Percent-decode; `None` on a malformed escape or non-UTF-8 result.
pub(crate) fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = b.get(i + 1..i + 3)?;
            let hi = char::from(h[0]).to_digit(16)?;
            let lo = char::from(h[1]).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Undo POSIX shell quoting (`'…'`, `"…"` with `\` escapes, bare `\x`). `None` when a quote
/// is left open.
pub(crate) fn shell_unquote(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        match c {
            '\'' => loop {
                match it.next()? {
                    '\'' => break,
                    c => out.push(c),
                }
            },
            '"' => loop {
                match it.next()? {
                    '"' => break,
                    '\\' => {
                        let n = it.next()?;
                        if !matches!(n, '"' | '\\' | '$' | '`') {
                            out.push('\\');
                        }
                        out.push(n);
                    }
                    c => out.push(c),
                }
            },
            '\\' => out.push(it.next()?),
            c => out.push(c),
        }
    }
    Some(out)
}

fn cap(mut s: String) -> String {
    if s.len() > CMDLINE_MAX {
        let mut end = CMDLINE_MAX;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
    s
}

/// Parse the `key=value` options after a mark letter (`rest` is the text after `X;`).
pub fn parse_opts(mark: u8, rest: &str) -> MarkOpts {
    let mut o = MarkOpts::default();
    if rest.is_empty() {
        return o;
    }
    for kv in rest.split(';') {
        let Some((k, v)) = kv.split_once('=') else {
            continue;
        };
        match (mark, k) {
            (b'A' | b'P', "k") => {
                o.kind = match v {
                    "i" => Some(PromptKind::Primary),
                    "r" if mark == b'A' => Some(PromptKind::Right),
                    "r" => o.kind,
                    "c" | "s" => Some(PromptKind::Continuation),
                    _ => o.kind,
                }
            }
            (b'A', "redraw") => o.redraw = flag01(v).or(o.redraw),
            (b'A', "special_key") => o.special_key = flag01(v).or(o.special_key),
            (b'A', "click_events") => {
                if let Ok(n @ 0..=2) = v.parse::<u8>() {
                    o.click_events = Some(n);
                }
            }
            (b'A', "cl") => {
                if !v.is_empty() && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                    o.cl = Some(v.to_string());
                }
            }
            (b'C', "cmdline") => {
                if let Some(s) = shell_unquote(v) {
                    o.cmdline = Some(cap(s));
                }
            }
            (b'C', "cmdline_url") => {
                if let Some(s) = percent_decode(v) {
                    o.cmdline = Some(cap(s));
                }
            }
            _ => {}
        }
    }
    o
}

/// The per-pane block store: the last [`BLOCKS_CAP`] command records.
#[derive(Clone, Debug)]
pub struct Blocks {
    list: VecDeque<Block>,
    next_id: u32,
    /// Records dropped from the front (count only).
    pub evicted: u64,
}

impl Default for Blocks {
    fn default() -> Self {
        Blocks {
            list: VecDeque::new(),
            next_id: 1,
            evicted: 0,
        }
    }
}

impl Blocks {
    /// Records, oldest first.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &Block> + ExactSizeIterator {
        self.list.iter()
    }

    /// The newest record.
    pub fn last(&self) -> Option<&Block> {
        self.list.back()
    }

    /// Number of records.
    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// No records.
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// The record with `id`.
    pub fn get(&self, id: u32) -> Option<&Block> {
        self.list.iter().rev().find(|b| b.id == id)
    }

    /// The id rows printed now belong to (0 when no block is open).
    pub fn current_id(&self) -> u32 {
        match self.list.back() {
            Some(b) if b.state != BlockState::Done => b.id,
            _ => 0,
        }
    }

    /// The cursor is in a prompt or input zone.
    pub fn at_prompt(&self) -> bool {
        matches!(
            self.list.back().map(|b| b.state),
            Some(BlockState::Prompt | BlockState::Input)
        )
    }

    fn open(&mut self, at: Option<Pin>) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).unwrap_or(1);
        if self.list.len() >= BLOCKS_CAP {
            self.list.pop_front();
            self.evicted += 1;
        }
        self.list.push_back(Block {
            id,
            state: BlockState::Prompt,
            prompt: at,
            input: None,
            output: None,
            end: None,
            exit: None,
            started: None,
            ended: None,
            cwd: None,
            cmdline: None,
        });
        id
    }

    /// A primary prompt (`A`). Returns the id of the block now open.
    pub(crate) fn prompt(&mut self, at: Option<Pin>, now: Option<Instant>) -> u32 {
        if let Some(b) = self.list.back_mut() {
            match b.state {
                // A after A with no input: the empty prompt is discarded (its record reused).
                BlockState::Prompt => {
                    b.prompt = at;
                    return b.id;
                }
                // An abandoned input line, or a nested shell's prompt under a running
                // command: the outer block ends here.
                BlockState::Input | BlockState::Running => {
                    b.state = BlockState::Done;
                    b.end = at;
                    b.ended = now;
                }
                BlockState::Done => {}
            }
        }
        self.open(at)
    }

    /// `B`: the input zone starts. Without an open prompt it opens a block (sloppy shells).
    pub(crate) fn input(&mut self, at: Option<Pin>) -> u32 {
        match self.list.back_mut() {
            Some(b) if b.state == BlockState::Prompt => {
                b.state = BlockState::Input;
                b.input = at;
                b.id
            }
            Some(b) if b.state == BlockState::Input => b.id,
            _ => {
                let id = self.open(at);
                if let Some(b) = self.list.back_mut() {
                    b.state = BlockState::Input;
                    b.input = at;
                }
                id
            }
        }
    }

    /// `C`: the command runs.
    pub(crate) fn output(
        &mut self,
        at: Option<Pin>,
        now: Option<Instant>,
        cwd: Option<&str>,
        cmdline: Option<String>,
    ) -> u32 {
        let open = matches!(
            self.list.back().map(|b| b.state),
            Some(BlockState::Prompt | BlockState::Input)
        );
        if !open {
            self.open(at);
        }
        let Some(b) = self.list.back_mut() else {
            return 0;
        };
        b.state = BlockState::Running;
        b.output = at;
        b.started = now;
        b.cwd = cwd.map(str::to_string);
        b.cmdline = cmdline;
        b.id
    }

    /// `D[;status]`: only a running block ends; otherwise ignored.
    pub(crate) fn end(&mut self, at: Option<Pin>, now: Option<Instant>, exit: Option<i32>) {
        if let Some(b) = self.list.back_mut()
            && b.state == BlockState::Running
        {
            b.state = BlockState::Done;
            b.end = at;
            b.ended = now;
            b.exit = exit;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_per_mark() {
        let o = parse_opts(
            b'A',
            "k=s;redraw=1;special_key=0;click_events=2;cl=m;cmdline=x",
        );
        assert_eq!(o.kind, Some(PromptKind::Continuation));
        assert_eq!(o.redraw, Some(true));
        assert_eq!(o.special_key, Some(false));
        assert_eq!(o.click_events, Some(2));
        assert_eq!(o.cl.as_deref(), Some("m"));
        assert_eq!(o.cmdline, None, "cmdline is a C option: ignored on A");
        let p = parse_opts(b'P', "k=r;redraw=1");
        assert_eq!(p.kind, None, "k=r is not a P value; redraw is not a P key");
        assert_eq!(p.redraw, None);
        let c = parse_opts(b'C', "k=c;cmdline='ls -l' \"a b\"\\ c");
        assert_eq!(c.kind, None);
        assert_eq!(c.cmdline.as_deref(), Some("ls -l a b c"));
    }

    #[test]
    fn malformed_values_drop_only_that_key() {
        let o = parse_opts(b'A', "redraw=7;k=i;click_events=9;cl=;bogus;=x");
        assert_eq!(o.redraw, None);
        assert_eq!(o.kind, Some(PromptKind::Primary));
        assert_eq!(o.click_events, None);
        assert_eq!(o.cl, None);
        let c = parse_opts(b'C', "cmdline='open");
        assert_eq!(c.cmdline, None);
        let u = parse_opts(b'C', "cmdline_url=echo%20hi%2");
        assert_eq!(u.cmdline, None);
        let u = parse_opts(b'C', "cmdline_url=echo%20hi");
        assert_eq!(u.cmdline.as_deref(), Some("echo hi"));
    }

    #[test]
    fn cmdline_is_capped() {
        let long = "x".repeat(CMDLINE_MAX * 2);
        let c = parse_opts(b'C', &format!("cmdline={long}"));
        assert_eq!(c.cmdline.unwrap().len(), CMDLINE_MAX);
    }

    #[test]
    fn lenient_machine() {
        let mut b = Blocks::default();
        let a = b.prompt(None, None);
        assert_eq!(
            b.prompt(None, None),
            a,
            "A after A with no input reuses the record"
        );
        assert_eq!(b.len(), 1);
        b.input(None);
        b.output(None, None, None, Some("make".into()));
        b.end(None, None, Some(2));
        assert_eq!(b.last().unwrap().state, BlockState::Done);
        assert_eq!(b.last().unwrap().exit, Some(2));
        b.end(None, None, Some(9));
        assert_eq!(
            b.last().unwrap().exit,
            Some(2),
            "D without a running block is ignored"
        );
        // A nested shell's primary prompt ends the outer running block.
        b.prompt(None, None);
        b.input(None);
        b.output(None, None, None, None);
        let outer = b.last().unwrap().id;
        let inner = b.prompt(None, None);
        assert_ne!(inner, outer);
        assert_eq!(b.get(outer).unwrap().state, BlockState::Done);
        assert!(b.at_prompt());
    }

    #[test]
    fn store_keeps_the_last_thousand() {
        let mut b = Blocks::default();
        for _ in 0..BLOCKS_CAP + 5 {
            b.prompt(None, None);
            b.input(None);
            b.output(None, None, None, None);
            b.end(None, None, Some(0));
        }
        assert_eq!(b.len(), BLOCKS_CAP);
        assert_eq!(b.evicted, 5);
        assert_eq!(b.iter().next().unwrap().id, 6);
    }
}
