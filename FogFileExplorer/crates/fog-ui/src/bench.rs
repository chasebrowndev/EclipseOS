// SPDX-License-Identifier: AGPL-3.0-only

//! Scrolling frame times for `fog-bench frames` (FOG §Performance model).
//!
//! `FOG_UI_BENCH_SCROLL=N` scrolls the list a fixed step every frame for N
//! frames after a warm-up, then prints one JSON line with the intervals
//! between frames (`window::frames`, one per redraw the compositor paced)
//! and exits. `FOG_UI_BENCH_SHEET=1` holds the palette open over the list
//! for the whole run, so the blurred sheet is drawn on every frame too.
//! Read once, before iced starts; nothing is timed unless asked.

use std::time::{Duration, Instant};

/// Frames before timing starts: fonts, pipelines and the first listing.
const WARMUP: usize = 60;
/// Pixels scrolled per frame: a brisk fling.
pub const STEP: f32 = 48.0;

#[derive(Debug, Clone)]
pub struct Bench {
    pub frames: usize,
    pub sheet: bool,
    seen: usize,
    last: Option<Instant>,
    intervals: Vec<Duration>,
}

impl Bench {
    pub fn from_env() -> Option<Bench> {
        let frames = std::env::var("FOG_UI_BENCH_SCROLL").ok()?.parse().ok()?;
        Some(Bench {
            frames,
            sheet: std::env::var("FOG_UI_BENCH_SHEET").is_ok_and(|v| v == "1"),
            seen: 0,
            last: None,
            intervals: Vec::with_capacity(frames),
        })
    }

    /// Record a frame; the scroll offset for it, or `None` when done.
    pub fn frame(&mut self, now: Instant, rows: usize, row_h: f32) -> Option<f32> {
        self.seen += 1;
        if self.seen > WARMUP {
            if let Some(l) = self.last {
                self.intervals.push(now - l);
            }
        }
        self.last = Some(now);
        if self.intervals.len() >= self.frames {
            return None;
        }
        // Down and back up, a triangle over the whole list.
        let span = (rows as f32 * row_h).max(1.0);
        let d = (self.seen as f32 * STEP) % (2.0 * span);
        Some(if d > span { 2.0 * span - d } else { d })
    }

    /// `{"frames":N,"p50_ms":…,"p95_ms":…,"p99_ms":…,"max_ms":…,"over_16_7":…}`
    pub fn report(&self) -> String {
        let mut ms: Vec<f64> = self
            .intervals
            .iter()
            .map(|d| d.as_secs_f64() * 1e3)
            .collect();
        ms.sort_by(f64::total_cmp);
        let q = |p: f64| {
            ms.get(((ms.len() as f64 - 1.0) * p).round() as usize)
                .copied()
                .unwrap_or(0.0)
        };
        let late = ms.iter().filter(|&&m| m > 1000.0 / 60.0 * 1.5).count();
        format!(
            "{{\"frames\":{},\"sheet\":{},\"p50_ms\":{:.2},\"p95_ms\":{:.2},\"p99_ms\":{:.2},\"max_ms\":{:.2},\"late\":{}}}",
            ms.len(),
            self.sheet,
            q(0.5),
            q(0.95),
            q(0.99),
            ms.last().copied().unwrap_or(0.0),
            late
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_after_warmup_and_bounces() {
        let mut b = Bench {
            frames: 3,
            sheet: false,
            seen: 0,
            last: None,
            intervals: Vec::new(),
        };
        let t0 = Instant::now();
        let mut offsets = Vec::new();
        let mut i = 0;
        while let Some(o) = b.frame(t0 + Duration::from_millis(16 * i), 10, 26.0) {
            offsets.push(o);
            i += 1;
        }
        assert_eq!(b.intervals.len(), 3);
        assert!(offsets.iter().all(|&o| (0.0..=260.0).contains(&o)));
        assert!(b.report().contains("\"frames\":3"));
        assert!(b.report().contains("\"p50_ms\":16.00"));
    }
}
