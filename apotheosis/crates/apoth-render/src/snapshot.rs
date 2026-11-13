// SPDX-License-Identifier: AGPL-3.0-only
//! Render-side snapshot buffers (T-07 §1, T-23 §3 rule 2).
//!
//! The render thread holds the term lock for exactly one `snapshot_dirty` call, bounded at
//! 100 us for a full 300x80 screen. Allocation under the lock is the part this crate controls:
//! [`prewarm`] sizes a [`FrameSnapshot`] for the screen *before* the lock is taken, so the
//! first (full) snapshot and every later one copy into existing capacity and never reallocate.
//! Call it again after a resize, outside the lock; the copy itself (apoth-grid) is then the
//! whole critical section.

use apoth_grid::{FrameSnapshot, SnapRow};

/// Cluster code points reserved per row beyond one per column / 8 (rare in real traffic).
const CLUSTER_SLACK: usize = 16;

/// Sizes `snap` for `rows` x `cols`: every row has capacity for `cols` cells and a cluster
/// buffer, and the changed-row list has capacity for `rows`. Rows already present keep their
/// contents; existing capacity is never shrunk.
pub fn prewarm(snap: &mut FrameSnapshot, rows: usize, cols: usize) {
    let per_row = |r: &mut SnapRow| {
        r.cells.reserve(cols.saturating_sub(r.cells.len()));
        r.clusters
            .reserve((cols / 8 + CLUSTER_SLACK).saturating_sub(r.clusters.len()));
    };
    snap.rows.resize_with(rows, SnapRow::default);
    snap.rows.iter_mut().for_each(per_row);
    snap.changed
        .reserve(rows.saturating_sub(snap.changed.len()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use apoth_grid::{CellAttrs, Glyph, Screen, ScreenConfig};

    fn filled(rows: usize, cols: usize) -> Screen {
        let mut s = Screen::new(rows, cols, ScreenConfig::default());
        let run: Vec<Glyph> = (0..cols)
            .map(|i| Glyph {
                cp: 0x61 + (i % 26) as u32,
                width: 1,
            })
            .collect();
        for y in 0..rows {
            s.print_run(y, 0, &run, &CellAttrs::default());
        }
        s
    }

    fn row_ptrs(snap: &FrameSnapshot) -> Vec<(*const u8, *const u8)> {
        snap.rows
            .iter()
            .map(|r| (r.cells.as_ptr().cast(), r.clusters.as_ptr().cast()))
            .collect()
    }

    /// The first full snapshot of a prewarmed buffer reallocates nothing: the buffers the
    /// lock-holder writes into are the ones that existed before the lock was taken.
    #[test]
    fn prewarmed_full_snapshot_does_not_reallocate() {
        let mut s = filled(80, 300);
        let mut snap = FrameSnapshot::new();
        prewarm(&mut snap, 80, 300);
        let before = row_ptrs(&snap);
        let changed_cap = snap.changed.capacity();
        let _ = s.snapshot_dirty(&mut snap);
        assert_eq!(row_ptrs(&snap), before, "row buffers moved under the lock");
        assert_eq!(snap.changed.capacity(), changed_cap);
        assert!(snap.rows.iter().all(|r| r.cells.len() == 300));
        // A second full snapshot is equally allocation free.
        s.mark_full();
        let _ = s.snapshot_dirty(&mut snap);
        assert_eq!(row_ptrs(&snap), before);
    }

    #[test]
    fn cold_buffer_does_allocate_which_is_why_prewarm_exists() {
        let mut s = filled(4, 40);
        let mut snap = FrameSnapshot::new();
        let _ = s.snapshot_dirty(&mut snap);
        assert!(snap.rows.iter().all(|r| r.cells.capacity() >= 40));
    }

    #[test]
    fn prewarm_keeps_rows_and_never_shrinks() {
        let mut snap = FrameSnapshot::new();
        prewarm(&mut snap, 10, 100);
        prewarm(&mut snap, 5, 50);
        assert_eq!(snap.rows.len(), 5);
        assert!(snap.rows[0].cells.capacity() >= 100);
    }

    /// T-23 §3 rule 2 / T-07 §1: a full 300x80 snapshot (warm buffers) holds the lock for at
    /// most 100 us at p99. Timing is meaningless unoptimised, so this is run explicitly:
    /// `cargo test --release -p apoth-render -- --ignored snapshot_lock`.
    #[test]
    #[ignore = "timing bound; run in release"]
    fn snapshot_lock_300x80_p99_within_100us() {
        let mut s = filled(80, 300);
        let mut snap = FrameSnapshot::new();
        prewarm(&mut snap, 80, 300);
        let mut times = Vec::new();
        for _ in 0..500 {
            s.mark_full();
            let t0 = std::time::Instant::now();
            let _ = s.snapshot_dirty(&mut snap);
            times.push(t0.elapsed());
        }
        times.sort();
        let p99 = times[times.len() * 99 / 100];
        assert!(
            p99 <= std::time::Duration::from_micros(100),
            "p99 {p99:?} exceeds the 100 us snapshot bound"
        );
    }
}
