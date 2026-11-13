# apoth-grid

Governing spec: T-02 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`). AGPL-3.0-only, not TCB,
`#![forbid(unsafe_code)]`, pure (no I/O, no wgpu, no wayland). No dependency on
`apoth-unicode`: widths are inputs.

## Design (one API; do not reintroduce a second)
- `page.rs`: `Page` is one byte buffer (`Vec<u8>`), every reference page-local, so it can be
  moved, serialised (`as_bytes` / `from_bytes`, which validates every header field, row
  offset, free list and link-map entry, rebuilds the derived hash indexes, then runs `audit`).
  Tables: styles (refcounted, open-addressed), cluster arena (4-cp chunks, 64 cp max),
  links (hash-indexed `(id, uri)`, plus a `(slot, col)` hash map so cell moves/erases are
  O(1)). `Page::audit()` checks refcounts == cell counts, no leaked chunks, flags never
  false-negative; tests call it after mutations.
- A full table is `Full::{Styles,Chunks,Links,LinkMap,Strings}`; `Screen` grows the page
  (`Page::grown`), or splits it when a table is at its ceiling (`Ceil`). Only a row that
  fits no page degrades the item and counts in `Stats::degraded`.
- `cell.rs`: 8-byte `Cell` (T-02 §1.2). `style.rs`: `Style`, `Color`, `Attrs`. `tabs.rs`:
  `TabStops`. `pin.rs`: tracked pins. `snapshot.rs`: the two snapshot APIs.
- `Screen::new(rows, cols, ScreenConfig)` clamps the active area to `MAX_CELLS` (hostile
  sizes cannot allocate). `Screen::resize(rows, cols)` / `resize_keeping(rows, cols, cursor_row)` resize
  in place WITHOUT reflow (reflow is M12): columns truncate/pad (cut wide cells blanked), rows
  grow at the bottom (no history pull), shrink drops rows below the cursor then pushes the top
  into scrollback (primary) or discards it (alt). Pins follow; discarded rows' pins go garbage;
  everything is marked dirty (`epoch` bump, next snapshot `Full`; reset `SeenSet`s).

## How apoth-term uses it
Coordinates `(y, x)` are active-area, 0-based; margins/regions are the caller's.
- Write: `print_run(y, x, &[Glyph{cp,width}], &CellAttrs)` returns glyphs consumed (stops
  at the row end; term wraps and sets `set_row_flag(y, flags::WRAPPED / WRAP_CONT, true)`).
  `print_cluster(y, x, &cps, width, &attrs)`; zero-width marks via `append_cp(y, x, cp)`.
- Edit: `erase_cells`, `erase_rows`, `insert_chars` (ICH), `delete_chars` (DCH),
  `insert_lines`/`delete_lines`, `scroll_up`/`scroll_down(Region, n, bg)`. Wide-cell seams
  are repaired inside each. Full-screen scroll on the primary screen copies no cells.
- Metadata: `set_row_flag`, `set_row_block` (T-14 block id), `tabs` via `TabStops`.
- Pins: `track_active` / `track_abs` -> `PinId`; `pin_state`, `pin_pos`, `pin_active_row`,
  `set_pin_col`, `untrack_pin`; after output call `take_garbage(&mut Vec<(PinId, PinKind)>)`
  to learn which pins' rows were evicted. Never store raw row indices across calls.
- Limits: `set_limits(Limits{bytes, lines})`; eviction drops whole head pages.
- Read: `cell`, `cell_text`, `row_flags`, `row_block`, `history_rows`, `copy_row`,
  `copy_abs_row`.
- Snapshots (each consumer owns its state): renderer `snapshot_dirty(&mut FrameSnapshot)
  -> Damage`; others `snapshot_since(&mut RowSnapshot, &mut SeenSet) -> Changes`.
  `mark_full()` forces the next renderer snapshot to `Full` (palette change, alt switch);
  allocate `SeenSet::new(rows)` per consumer and `reset()` it on resize/screen switch.
  Both reuse buffer capacity and allocate nothing once warm.
- Determinism: no clocks, no randomness; same call sequence -> same state.

## Tests
`src/tests.rs` (naive-model property tests, scroll/eviction/pin/snapshot/overflow/split
tests) and `src/page.rs` unit tests (refcounts, link map vs model, seams, from_bytes
validation and corruption fuzz-by-loop). Run:
`CARGO_INCREMENTAL=0 cargo test -p apoth-grid -j 2`.
