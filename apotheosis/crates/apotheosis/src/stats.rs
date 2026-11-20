// SPDX-License-Identifier: AGPL-3.0-only
//! Opt-in render profiling: `APOTHEOSIS_RENDER_STATS=1` (a debug knob, not a config row).
//!
//! The render thread feeds one [`Sample`] per presented frame; every 5 seconds [`Stats`]
//! returns one info line with the window's averages and maxima. Counts, timings and sizes
//! only: never pane text, image data, paths or titles. Off by default; when off the render
//! thread holds `None` and takes no extra timestamps and creates no GPU queries.

use apoth_render::prof::GpuTimes;
use std::ffi::OsStr;
use std::fmt::Write as _;

/// The environment variable that turns the stats on (value `1`).
pub(crate) const ENV: &str = "APOTHEOSIS_RENDER_STATS";

/// Aggregation window, nanoseconds.
const WINDOW_NS: u64 = 5_000_000_000;

/// True only for the value `1`.
pub(crate) fn enabled(value: Option<&OsStr>) -> bool {
    value.is_some_and(|v| v == "1")
}

/// Whether the process asked for render stats.
pub(crate) fn requested() -> bool {
    enabled(std::env::var_os(ENV).as_deref())
}

/// What one presented frame cost. Durations are nanoseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Sample {
    /// Snapshot, scene build and chrome lowering before `present`, plus the encode and submit
    /// inside it.
    pub(crate) cpu_ns: u64,
    /// Time blocked acquiring the swapchain image plus inside `Queue::present`.
    pub(crate) present_ns: u64,
    pub(crate) rows: u64,
    pub(crate) glyphs: u64,
    pub(crate) image_quads: u64,
    pub(crate) image_uploads: u64,
    pub(crate) upload_bytes: u64,
    pub(crate) image_bytes: u64,
    pub(crate) passes: u64,
    pub(crate) region_copies: u64,
    pub(crate) full: bool,
    /// An animation deadline had come due when this frame started.
    pub(crate) anim_due: bool,
    /// An image animation is running after this frame.
    pub(crate) animating: bool,
    /// How late this frame started against the animation deadline.
    pub(crate) anim_late_ns: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct Span {
    sum: f64,
    max: f64,
    n: u64,
}

impl Span {
    fn add(&mut self, v: f64) {
        self.sum += v;
        self.max = self.max.max(v);
        self.n += 1;
    }

    fn avg(&self) -> f64 {
        if self.n == 0 {
            0.0
        } else {
            self.sum / self.n as f64
        }
    }
}

/// The window accumulator.
#[derive(Debug, Default)]
pub(crate) struct Stats {
    start_ns: Option<u64>,
    frames: u64,
    cpu: Span,
    present: Span,
    rows: u64,
    glyphs: u64,
    quads: u64,
    uploads: u64,
    bytes: u64,
    image_bytes: u64,
    passes: u64,
    copies: u64,
    full: u64,
    due: u64,
    animated: u64,
    late_max_ns: u64,
    /// grid, image, chrome, composite, present
    gpu: [Span; 5],
    gpu_frames: u64,
    timestamps: bool,
}

impl Stats {
    /// `timestamps`: GPU timestamps are available, so the GPU columns are reported.
    pub(crate) fn new(timestamps: bool) -> Self {
        Self {
            timestamps,
            ..Self::default()
        }
    }

    /// Adds the GPU times of a frame whose readback finished.
    pub(crate) fn gpu(&mut self, t: &GpuTimes) {
        self.gpu_frames += 1;
        for (s, v) in self
            .gpu
            .iter_mut()
            .zip([t.grid, t.image, t.chrome, t.composite, t.present])
        {
            if let Some(v) = v {
                s.add(v);
            }
        }
    }

    /// Adds a presented frame at `now_ns`. Returns the window's line when the window ended
    /// (the sample belongs to the new window).
    pub(crate) fn frame(&mut self, now_ns: u64, s: &Sample) -> Option<String> {
        let line = match self.start_ns {
            Some(t0) if now_ns.saturating_sub(t0) >= WINDOW_NS => {
                let l = self.line(now_ns.saturating_sub(t0));
                self.reset();
                Some(l)
            }
            _ => None,
        };
        self.start_ns.get_or_insert(now_ns);
        self.frames += 1;
        self.cpu.add(s.cpu_ns as f64 / 1e6);
        self.present.add(s.present_ns as f64 / 1e6);
        self.rows += s.rows;
        self.glyphs += s.glyphs;
        self.quads += s.image_quads;
        self.uploads += s.image_uploads;
        self.bytes += s.upload_bytes;
        self.image_bytes += s.image_bytes;
        self.passes += s.passes;
        self.copies += s.region_copies;
        self.full += u64::from(s.full);
        self.due += u64::from(s.anim_due);
        self.animated += u64::from(s.animating);
        self.late_max_ns = self.late_max_ns.max(s.anim_late_ns);
        line
    }

    fn reset(&mut self) {
        *self = Self::new(self.timestamps);
    }

    fn line(&self, elapsed_ns: u64) -> String {
        let secs = elapsed_ns as f64 / 1e9;
        let f = self.frames.max(1) as f64;
        let mut l = String::new();
        let _ = write!(
            l,
            "render stats: window={secs:.1}s frames={} fps={:.1} \
             cpu_ms=avg {:.2}/max {:.2} present_ms=avg {:.2}/max {:.2} \
             per_frame: rows={:.1} glyphs={:.1} image_quads={:.1} image_uploads={:.2} \
             upload_bytes={:.0} image_bytes={:.0} passes={:.1} \
             full_redraws={} region_copies={}",
            self.frames,
            self.frames as f64 / secs.max(1e-9),
            self.cpu.avg(),
            self.cpu.max,
            self.present.avg(),
            self.present.max,
            self.rows as f64 / f,
            self.glyphs as f64 / f,
            self.quads as f64 / f,
            self.uploads as f64 / f,
            self.bytes as f64 / f,
            self.image_bytes as f64 / f,
            self.passes as f64 / f,
            self.full,
            self.copies,
        );
        if self.animated > 0 || self.due > 0 {
            let _ = write!(
                l,
                " anim: due={} presented={} late_max_ms={:.2}",
                self.due,
                self.animated,
                self.late_max_ns as f64 / 1e6
            );
        }
        if self.timestamps {
            let names = ["grid", "image", "chrome", "composite", "present"];
            l.push_str(" gpu_ms:");
            for (n, s) in names.iter().zip(&self.gpu) {
                if s.n > 0 {
                    let _ = write!(l, " {n}=avg {:.3}/max {:.3}", s.avg(), s.max);
                }
            }
            let _ = write!(l, " (gpu_frames={})", self.gpu_frames);
        }
        l
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(cpu_ms: u64) -> Sample {
        Sample {
            cpu_ns: cpu_ms * 1_000_000,
            present_ns: 500_000,
            rows: 2,
            glyphs: 10,
            image_quads: 1,
            image_uploads: 1,
            upload_bytes: 400,
            image_bytes: 300,
            passes: 2,
            ..Sample::default()
        }
    }

    #[test]
    fn off_unless_the_value_is_exactly_one() {
        assert!(!enabled(None));
        assert!(!enabled(Some(OsStr::new(""))));
        assert!(!enabled(Some(OsStr::new("0"))));
        assert!(!enabled(Some(OsStr::new("true"))));
        assert!(enabled(Some(OsStr::new("1"))));
    }

    #[test]
    fn stats_are_off_in_the_test_environment() {
        // Nothing sets the variable for `cargo test`; a stray export would make this fail
        // loudly rather than silently profile.
        assert!(!requested());
    }

    #[test]
    fn a_window_emits_one_line_and_starts_over() {
        let mut s = Stats::new(false);
        assert!(s.frame(0, &sample(1)).is_none());
        assert!(s.frame(1_000_000_000, &sample(3)).is_none());
        let line = s.frame(5_000_000_000, &sample(2)).expect("window ended");
        assert!(line.contains("frames=2"), "{line}");
        assert!(line.contains("fps=0.4"), "{line}");
        assert!(line.contains("cpu_ms=avg 2.00/max 3.00"), "{line}");
        assert!(line.contains("image_uploads=1.00"), "{line}");
        assert!(line.contains("passes=2.0"), "{line}");
        assert!(!line.contains("gpu_ms"), "{line}");
        assert!(!line.contains("anim:"), "{line}");
        // The sample that closed the window opened the next one.
        let next = s.frame(10_000_000_000, &sample(1)).expect("second window");
        assert!(next.contains("frames=1"), "{next}");
    }

    #[test]
    fn an_idle_gap_does_not_dilute_the_next_window() {
        let mut s = Stats::new(false);
        assert!(s.frame(0, &sample(1)).is_none());
        // A long idle gap: the window closes on the next frame, which starts a fresh one.
        assert!(s.frame(60_000_000_000, &sample(1)).is_some());
        assert!(s.frame(61_000_000_000, &sample(1)).is_none());
    }

    #[test]
    fn animation_columns_appear_only_while_animating() {
        let mut s = Stats::new(false);
        let mut a = sample(1);
        a.anim_due = true;
        a.animating = true;
        a.anim_late_ns = 2_500_000;
        s.frame(0, &a);
        s.frame(1_000_000_000, &a);
        let line = s.frame(5_000_000_000, &a).expect("line");
        assert!(
            line.contains("anim: due=2 presented=2 late_max_ms=2.50"),
            "{line}"
        );
    }

    #[test]
    fn gpu_columns_need_timestamps() {
        let mut s = Stats::new(true);
        s.gpu(&GpuTimes {
            grid: Some(0.5),
            present: Some(2.0),
            ..GpuTimes::default()
        });
        s.gpu(&GpuTimes {
            grid: Some(1.5),
            present: Some(4.0),
            ..GpuTimes::default()
        });
        s.frame(0, &sample(1));
        let line = s.frame(5_000_000_000, &sample(1)).expect("line");
        assert!(line.contains("grid=avg 1.000/max 1.500"), "{line}");
        assert!(line.contains("present=avg 3.000/max 4.000"), "{line}");
        assert!(!line.contains("image="), "{line}");
        assert!(line.contains("gpu_frames=2"), "{line}");
    }
}
