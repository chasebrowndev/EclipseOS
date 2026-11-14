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
/// The consent, link-confirm, error or notice bar, or the modal sheet.
pub const BAR: NodeId = NodeId(7);
/// The body text of a modal sheet.
pub const BAR_BODY: NodeId = NodeId(8);
/// The search bar group.
pub const SEARCH: NodeId = NodeId(9);
/// The search query field.
pub const SEARCH_FIELD: NodeId = NodeId(10);
/// The search match count.
pub const SEARCH_COUNT: NodeId = NodeId(11);
/// Search: previous match.
pub const SEARCH_PREV: NodeId = NodeId(12);
/// Search: next match.
pub const SEARCH_NEXT: NodeId = NodeId(13);
/// Search: close.
pub const SEARCH_CLOSE: NodeId = NodeId(14);
/// The link tooltip.
pub const TOOLTIP: NodeId = NodeId(15);
/// The new-tab button.
pub const NEW_TAB: NodeId = NodeId(16);

const KIND_SHIFT: u32 = 56;
const K_TAB: u64 = 1;
const K_PANE: u64 = 2;
const K_TERM: u64 = 3;
const K_RUN: u64 = 4;
const K_LINE: u64 = 5;
const K_PALETTE_ROW: u64 = 6;
const K_BAR_BUTTON: u64 = 7;
const K_CHIP: u64 = 8;
const K_TAB_PROGRESS: u64 = 9;
const K_TAB_PROFILE: u64 = 10;
const K_TAB_CLOSE: u64 = 11;
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

/// Button `i` of the bar or sheet.
#[must_use]
pub fn bar_button(i: usize) -> NodeId {
    make(K_BAR_BUTTON, i as u64)
}

/// Search mode chip `i` (in the order the model lists them).
#[must_use]
pub fn chip(i: usize) -> NodeId {
    make(K_CHIP, i as u64)
}

/// A tab's progress indicator.
#[must_use]
pub fn tab_progress(id: u64) -> NodeId {
    make(K_TAB_PROGRESS, id)
}

/// A tab's profile badge.
#[must_use]
pub fn tab_profile(id: u64) -> NodeId {
    make(K_TAB_PROFILE, id)
}

/// A tab's close button.
#[must_use]
pub fn tab_close(id: u64) -> NodeId {
    make(K_TAB_CLOSE, id)
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
    /// A button of the bar or sheet.
    BarButton(usize),
    /// The search field.
    SearchField,
    /// A search mode chip, by index in the model's list.
    SearchChip(usize),
    /// Search: previous match.
    SearchPrev,
    /// Search: next match.
    SearchNext,
    /// Search: close.
    SearchClose,
    /// A tab's close button, by mux id.
    TabClose(u64),
    /// The new-tab button.
    NewTab,
    /// Something else (palette field, toast, labels).
    Other,
}

/// Decode a node id.
#[must_use]
pub fn target(id: NodeId) -> Target {
    let v = id.0 & LOW;
    match id.0 >> KIND_SHIFT {
        0 if id == WINDOW || id == TAB_LIST => Target::Window,
        0 if id == SEARCH_FIELD => Target::SearchField,
        0 if id == SEARCH_PREV => Target::SearchPrev,
        0 if id == SEARCH_NEXT => Target::SearchNext,
        0 if id == SEARCH_CLOSE => Target::SearchClose,
        0 if id == NEW_TAB => Target::NewTab,
        0 => Target::Other,
        K_BAR_BUTTON => Target::BarButton(v as usize),
        K_CHIP => Target::SearchChip(v as usize),
        K_TAB_CLOSE => Target::TabClose(v),
        K_TAB_PROGRESS | K_TAB_PROFILE => Target::Other,
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
            BAR,
            BAR_BODY,
            SEARCH,
            SEARCH_FIELD,
            SEARCH_COUNT,
            SEARCH_PREV,
            SEARCH_NEXT,
            SEARCH_CLOSE,
            TOOLTIP,
            NEW_TAB,
            bar_button(0),
            bar_button(2),
            chip(0),
            chip(3),
            tab_progress(7),
            tab_profile(7),
            tab_close(7),
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
        assert_eq!(target(bar_button(2)), Target::BarButton(2));
        assert_eq!(target(chip(3)), Target::SearchChip(3));
        assert_eq!(target(tab_close(9)), Target::TabClose(9));
        assert_eq!(target(tab_progress(9)), Target::Other);
        assert_eq!(target(SEARCH_FIELD), Target::SearchField);
        assert_eq!(target(SEARCH_PREV), Target::SearchPrev);
        assert_eq!(target(SEARCH_NEXT), Target::SearchNext);
        assert_eq!(target(SEARCH_CLOSE), Target::SearchClose);
        assert_eq!(target(NEW_TAB), Target::NewTab);
        assert_eq!(target(TOOLTIP), Target::Other);
    }
}
