// SPDX-License-Identifier: AGPL-3.0-only
//! Frame timing instrumentation (COMP-14 §2).
//!
//! Every composited frame records how long the CPU spent in `render_frame`
//! and in the page-flip submission. Samples land in a fixed-capacity ring —
//! no allocation once the compositor is running — and are summarised as
//! p50/p99 on a fixed interval so a regression shows up in the journal
//! without a profiler attached.

use std::time::{Duration, Instant};

/// Number of frames kept for the percentile window. At 144 Hz this is a
/// little under four seconds of history, which comfortably covers the
/// reporting interval below.
const CAPACITY: usize = 1024;

const REPORT_INTERVAL: Duration = Duration::from_secs(5);

/// A fixed-capacity ring of frame timings for one compositor.
pub struct FrameStats {
    /// Report on the interval rather than staying silent (`--stats`).
    enabled: bool,
    render_us: Box<[u32; CAPACITY]>,
    submit_us: Box<[u32; CAPACITY]>,
    next: usize,
    len: usize,
    frames: u64,
    last_report: Instant,
}

impl FrameStats {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            render_us: Box::new([0; CAPACITY]),
            submit_us: Box::new([0; CAPACITY]),
            next: 0,
            len: 0,
            frames: 0,
            last_report: Instant::now(),
        }
    }

    /// Record one composited frame. Cheap and allocation-free.
    pub fn record(&mut self, render: Duration, submit: Duration) {
        self.render_us[self.next] = render.as_micros().min(u32::MAX as u128) as u32;
        self.submit_us[self.next] = submit.as_micros().min(u32::MAX as u128) as u32;
        self.next = (self.next + 1) % CAPACITY;
        self.len = (self.len + 1).min(CAPACITY);
        self.frames += 1;
    }

    /// Emit a summary if the reporting interval has elapsed. Called once per
    /// frame; does nothing until it is due.
    pub fn maybe_report(&mut self) {
        if !self.enabled || self.len == 0 {
            return;
        }
        let elapsed = self.last_report.elapsed();
        if elapsed < REPORT_INTERVAL {
            return;
        }
        self.last_report = Instant::now();
        let mut render: Vec<u32> = self.render_us[..self.len].to_vec();
        let mut submit: Vec<u32> = self.submit_us[..self.len].to_vec();
        render.sort_unstable();
        submit.sort_unstable();
        let fps = self.frames as f64 / elapsed.as_secs_f64();
        tracing::info!(
            frames = self.frames,
            fps = format_args!("{fps:.1}"),
            render_p50_us = pct(&render, 50),
            render_p99_us = pct(&render, 99),
            submit_p50_us = pct(&submit, 50),
            submit_p99_us = pct(&submit, 99),
            "frame stats"
        );
        self.frames = 0;
    }
}

/// Nearest-rank percentile over an already sorted slice.
fn pct(sorted: &[u32], p: usize) -> u32 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (sorted.len() * p).div_ceil(100).max(1) - 1;
    sorted[rank.min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_are_nearest_rank() {
        let v: Vec<u32> = (1..=100).collect();
        assert_eq!(pct(&v, 50), 50);
        assert_eq!(pct(&v, 99), 99);
        assert_eq!(pct(&[], 50), 0);
        assert_eq!(pct(&[7], 99), 7);
    }

    #[test]
    fn ring_wraps_without_growing() {
        let mut s = FrameStats::new(false);
        for _ in 0..(CAPACITY * 2 + 3) {
            s.record(Duration::from_micros(10), Duration::from_micros(1));
        }
        assert_eq!(s.len, CAPACITY);
    }
}
