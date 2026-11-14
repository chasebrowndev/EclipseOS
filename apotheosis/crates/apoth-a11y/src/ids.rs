// SPDX-License-Identifier: AGPL-3.0-only
//! Stable AccessKit node ids. Mux ids are never reused (T-12 §3), so a node id names one
//! object for the life of the process.

use accesskit::NodeId;

/// The window (root).
pub const WINDOW: NodeId = NodeId(1);
/// The tab list.
pub const TAB_LIST: NodeId = NodeId(2);
/// The command palette sheet.
pub const PALETTE: NodeId = NodeId(3);
/// The palette's query field.
pub const PALETTE_INPUT: NodeId = NodeId(4);
/// The palette's result list.
pub const PALETTE_LIST: NodeId = NodeId(5);
/// The announcement live region (toast).
pub const TOAST: NodeId = NodeId(6);

const KIND_SHIFT: u32 = 56;
const K_TAB: u64 = 1;
const K_PANE: u64 = 2;
const K_TERM: u64 = 3;
const K_RUN: u64 = 4;
const K_LINE: u64 = 5;
const K_PALETTE_ROW: u64 = 6;
const LOW: u64 = (1 << KIND_SHIFT) - 1;
/// Pane ids that fit in a text-run id.
const PANE_BITS: u32 = 32;
/// Rows that fit in a text-run id.
const ROW_BITS: u32 = 16;

fn make(kind: u64, v: u64) -> NodeId {
    NodeId((kind << KIND_SHIFT) | (v & LOW))
}

/// A tab.
#[must_use]
pub fn tab(id: u64) -> NodeId {
    make(K_TAB, id)
}

/// A pane's group.
#[must_use]
pub fn pane(id: u64) -> NodeId {
    make(K_PANE, id)
}

/// A pane's terminal (text) node.
#[must_use]
pub fn terminal(id: u64) -> NodeId {
    make(K_TERM, id)
}

fn sub(pane: u64, row: usize, chunk: usize) -> u64 {
    ((pane & ((1 << PANE_BITS) - 1)) << 24)
        | (((row as u64) & ((1 << ROW_BITS) - 1)) << 8)
        | ((chunk as u64) & 0xff)
}

/// Text run `chunk` of visible row `row` of a pane.
#[must_use]
pub fn run(pane: u64, row: usize, chunk: usize) -> NodeId {
    make(K_RUN, sub(pane, row, chunk))
}

/// Visible row `row` of a pane in the line-only model.
#[must_use]
pub fn line(pane: u64, row: usize) -> NodeId {
    make(K_LINE, sub(pane, row, 0))
}

/// Palette result row `i`.
#[must_use]
pub fn palette_row(i: usize) -> NodeId {
    make(K_PALETTE_ROW, i as u64)
}

/// What a node id names (for action requests).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// The window or a structural node.
    Window,
    /// A tab by mux id.
    Tab(u64),
    /// A pane (its group, terminal, a run or a line) by mux id; pane ids above 32 bits
    /// are truncated in run and line ids.
    Pane(u64),
    /// A palette row.
    PaletteRow(usize),
    /// Something else (palette field, toast).
    Other,
}

/// Decode a node id.
#[must_use]
pub fn target(id: NodeId) -> Target {
    let v = id.0 & LOW;
    match id.0 >> KIND_SHIFT {
        0 if id == WINDOW || id == TAB_LIST => Target::Window,
        0 => Target::Other,
        K_TAB => Target::Tab(v),
        K_PANE | K_TERM => Target::Pane(v),
        K_RUN | K_LINE => Target::Pane(v >> 24),
        K_PALETTE_ROW => Target::PaletteRow(v as usize),
        _ => Target::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_distinct_and_decode() {
        let all = [
            WINDOW,
            TAB_LIST,
            PALETTE,
            PALETTE_INPUT,
            PALETTE_LIST,
            TOAST,
            tab(7),
            pane(7),
            terminal(7),
            run(7, 0, 0),
            run(7, 0, 1),
            run(7, 1, 0),
            line(7, 0),
            palette_row(0),
        ];
        let mut v: Vec<u64> = all.iter().map(|n| n.0).collect();
        v.sort_unstable();
        v.dedup();
        assert_eq!(v.len(), all.len());
        assert_eq!(target(tab(9)), Target::Tab(9));
        assert_eq!(target(terminal(9)), Target::Pane(9));
        assert_eq!(target(run(9, 3, 2)), Target::Pane(9));
        assert_eq!(target(line(9, 3)), Target::Pane(9));
        assert_eq!(target(palette_row(4)), Target::PaletteRow(4));
        assert_eq!(target(TOAST), Target::Other);
    }
}
