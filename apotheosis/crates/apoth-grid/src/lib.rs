// SPDX-License-Identifier: AGPL-3.0-only
//! Paged grid: cells, interned styles and links, pins, reflow, history, compression, search iteration.
//!
//! Governing spec: T-02 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`).
//!
//! M12 adds reflow (`Screen::resize_reflow`), selection geometry and extraction
//! (`select`) and the logical-line search primitives (`search`); lazy history reflow and
//! cold-page compression are not yet implemented.
//!
//! M3 scope: paged storage with relocatable pages, 8-byte cells, per-page interned styles
//! and hyperlinks, grapheme arena, rows with wrap flags and sequence numbers, region
//! scrolling, scrollback limits with a page pool, tracked pins, tab stops and the two
//! snapshot APIs. Reflow, selection, lz4 cold pages and the logical-line iterator are M12
//! and leave seams only: `Page::wrap_width`, `Pin::wrap_width`, `Screen::epoch`.
//!
//! The grid takes widths as input (`Glyph::width`); it does not depend on `apoth-unicode`.
//!
//! Decisions that fill spec gaps (reported to the orchestrator):
//! * `BgOnly` cells pack the whole colour (palette or RGB) in the cell word; no style item.
//! * Hyperlinks live in a per-page link map keyed `(slot, col)` with a hash index, so every
//!   cell move, erase and lookup is O(1) however many cells carry links.
//! * A full page table grows the page (copy into a larger page); a table at its ceiling
//!   splits the page; only a row that fits no page degrades the item, counted in
//!   `Stats::degraded`.
//! * `Screen::new` clamps the active area to `MAX_CELLS`; so does `Screen::resize`.
//! * `Screen::resize` (no reflow): truncate/pad columns, blank cut wide cells, grow rows at
//!   the bottom with no history pull, shrink rows by dropping rows below the cursor first and
//!   then pushing the top into scrollback (primary) or discarding it (alternate).
//! * A position-changing scroll bumps the sequence number of every row in the region, so
//!   position-keyed `snapshot_since` consumers see the shift.
//! * The page byte unit for `scrollback.bytes` is `capacity rows x (cols x 8 + 24)`.
#![forbid(unsafe_code)]

pub mod cell;
pub mod page;
pub mod pin;
pub mod screen;
pub mod search;
pub mod select;
pub mod snapshot;
pub mod style;
pub mod tabs;

pub use cell::{Cell, Semantic, Tag, Wide};
pub use page::{MAX_CLUSTER_CPS, MAX_LINK_ID, MAX_URI, PAGE_BYTES, row_flags as flags};
pub use pin::{MAX_PINS, Pin, PinId, PinKind, PinState};
pub use screen::{
    CellAttrs, Glyph, Limits, MAX_CELLS, ReflowOpts, Reflowed, Region, Resized, Screen,
    ScreenConfig, Stats,
};
pub use search::{Dir, LineScratch, LineSpan, MAX_LINE_BYTES, fold_text};
pub use select::{ExtractOpts, SelKind, SelRange, Selection, Side, WordChars};
pub use snapshot::{Changes, Damage, FrameSnapshot, RowSnapshot, SeenSet, SnapCell, SnapRow};
pub use style::{Attrs, Color, Style, Underline};
pub use tabs::TabStops;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_fast;
#[cfg(test)]
mod tests_reflow;
