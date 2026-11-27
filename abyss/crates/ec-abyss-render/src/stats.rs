// SPDX-License-Identifier: AGPL-3.0-only
//! Frame timing instrumentation (COMP-14 §2).
//!
//! Every composited frame records how long the CPU spent in `render_frame`
//! and in the page-flip submission. Samples land in a fixed-capacity ring —
//! no allocation once the compositor is running — and are summarised as
//! p50/p99 on a fixed interval so a regression shows up in the journal
//! without a profiler attached.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use crate::gpu_timer::{Span, SPAN_KINDS};

/// Number of frames kept for the percentile window. At 144 Hz this is a
/// little under four seconds of history, which comfortably covers the
/// reporting interval below.
const CAPACITY: usize = 1024;

const REPORT_INTERVAL: Duration = Duration::from_secs(5);

/// GPU frames kept for the per-frame percentile (as [`CAPACITY`]).
const GPU_CAPACITY: usize = 1024;

/// Backdrop rebuild reasons ([`Miss`]).
const MISSES: usize = 7;

/// Why a backdrop cache unit was rebuilt. The first that applies is kept, in
/// this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Miss {
    /// Never built: a new window, tier or layer.
    First = 0,
    /// The blur's passes or packing changed (a preset or tunable change).
    Mode = 1,
    /// The region moved or resized, or its crop, clamp or group membership changed.
    Geometry = 2,
    /// Damage in a group's margin: inside its built region, outside every
    /// member's own crop.
    Margin = 3,
    /// Damage within the kernel radius of the region.
    Kernel = 4,
    /// Damage reached only by the glass refraction reach, beyond the kernel.
    Refraction = 5,
    /// No new damage: a throttled rebuild flushed from an earlier frame.
    Defer = 6,
}

/// How one backdrop cache unit (a window's, or a tier's) was served in a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Served {
    /// Clean cache: no rebuild.
    Hit,
    /// Dirty, but held back by the crowded-store throttle: served stale.
    Held,
    /// Rebuild requested, but nothing the blur samples changed: no blits.
    Idle,
    /// Rebuilt.
    Miss(Miss),
}

/// A GPU timer frame that did not contribute to the figures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuLoss {
    /// Not ready in time and dropped unread.
    Lost,
    /// Discarded for `GPU_DISJOINT`.
    Disjoint,
    /// A span that was not timed (ring full, or one already open).
    Skipped,
}

/// Glass counters and GPU stage sums for one report interval (`--stats`).
#[derive(Clone, Copy)]
struct Glass {
    /// Backdrop units served clean.
    hits: u64,
    /// Units held back by the crowded throttle.
    held: u64,
    /// Units rebuild-requested that changed nothing the blur samples.
    idle: u64,
    /// Rebuilt units, by [`Miss`].
    misses: [u64; MISSES],
    /// Blur-backed elements produced (drawn).
    surfaces: u64,
    /// Backdrop texels blurred (the crop, or the damage of a partial rebuild).
    backdrop_px: u64,
    /// Texels the blit passes wrote, summed over the passes.
    pass_px: u64,
    /// GPU frames read back.
    gpu_frames: u64,
    /// GPU nanoseconds per stage, summed over those frames.
    gpu_sum_ns: [u64; SPAN_KINDS],
    /// Per-frame GPU microseconds: the fixed ring the percentiles come from.
    gpu_total_us: [u32; GPU_CAPACITY],
    gpu_len: usize,
    gpu_next: usize,
    lost: u64,
    disjoint: u64,
    skipped: u64,
}

impl Glass {
    const ZERO: Glass = Glass {
        hits: 0,
        held: 0,
        idle: 0,
        misses: [0; MISSES],
        surfaces: 0,
        backdrop_px: 0,
        pass_px: 0,
        gpu_frames: 0,
        gpu_sum_ns: [0; SPAN_KINDS],
        gpu_total_us: [0; GPU_CAPACITY],
        gpu_len: 0,
        gpu_next: 0,
        lost: 0,
        disjoint: 0,
        skipped: 0,
    };
}

thread_local! {
    /// `--stats` is on. The one flag every glass hook reads first.
    static GLASS_ON: Cell<bool> = const { Cell::new(false) };
    static GLASS: RefCell<Glass> = const { RefCell::new(Glass::ZERO) };
}

/// Whether the glass counters and the GPU stage timers run, which is exactly
/// when `--stats` is on ([`FrameStats::new`] sets it). Each hook reads this
/// first, so without `--stats` a hook costs one thread-local read.
pub fn glass_on() -> bool {
    GLASS_ON.with(Cell::get)
}

/// Count one backdrop cache unit this frame. A no-op unless [`glass_on`].
pub fn note_backdrop(served: Served) {
    if !glass_on() {
        return;
    }
    GLASS.with_borrow_mut(|g| match served {
        Served::Hit => g.hits += 1,
        Served::Held => g.held += 1,
        Served::Idle => g.idle += 1,
        Served::Miss(m) => g.misses[m as usize] += 1,
    });
}

/// Count one blur-backed element drawn. A no-op unless [`glass_on`].
pub fn note_surface() {
    if glass_on() {
        GLASS.with_borrow_mut(|g| g.surfaces += 1);
    }
}

/// Count one backdrop rebuild: texels blurred and texels the passes wrote.
/// A no-op unless [`glass_on`].
pub fn note_rebuild(backdrop_px: u64, pass_px: u64) {
    if glass_on() {
        GLASS.with_borrow_mut(|g| {
            g.backdrop_px += backdrop_px;
            g.pass_px += pass_px;
        });
    }
}

/// One GPU timer frame read back: nanoseconds per stage. Allocation-free (it
/// writes the fixed ring). A no-op unless [`glass_on`].
pub fn note_gpu_frame(per_kind: &[u64; SPAN_KINDS]) {
    if !glass_on() {
        return;
    }
    GLASS.with_borrow_mut(|g| {
        let total: u64 = per_kind.iter().sum();
        g.gpu_frames += 1;
        for (sum, ns) in g.gpu_sum_ns.iter_mut().zip(per_kind) {
            *sum += ns;
        }
        g.gpu_total_us[g.gpu_next] = (total / 1000).min(u64::from(u32::MAX)) as u32;
        g.gpu_next = (g.gpu_next + 1) % GPU_CAPACITY;
        g.gpu_len = (g.gpu_len + 1).min(GPU_CAPACITY);
    });
}

/// A GPU timer frame that was lost, discarded or not timed. A no-op unless
/// [`glass_on`].
pub fn note_gpu_loss(loss: GpuLoss) {
    if glass_on() {
        GLASS.with_borrow_mut(|g| match loss {
            GpuLoss::Lost => g.lost += 1,
            GpuLoss::Disjoint => g.disjoint += 1,
            GpuLoss::Skipped => g.skipped += 1,
        });
    }
}

/// Take (and reset) the interval's glass counters.
fn take_glass() -> Glass {
    GLASS.with_borrow_mut(|g| std::mem::replace(g, Glass::ZERO))
}

/// A fixed-capacity ring of frame timings for one compositor.
pub struct FrameStats {
    /// Report on the interval rather than staying silent (`--stats`).
    enabled: bool,
    render_us: Box<[u32; CAPACITY]>,
    submit_us: Box<[u32; CAPACITY]>,
    next: usize,
    len: usize,
    frames: u64,
    flip_us: Box<[u32; CAPACITY]>,
    interval_us: Box<[u32; CAPACITY]>,
    flip_next: usize,
    flip_len: usize,
    interval_next: usize,
    interval_len: usize,
    flips: u64,
    last_report: Instant,
}

impl FrameStats {
    pub fn new(enabled: bool) -> Self {
        // `--stats` is the one switch for the glass instrumentation too.
        GLASS_ON.with(|on| on.set(enabled));
        Self {
            enabled,
            render_us: Box::new([0; CAPACITY]),
            submit_us: Box::new([0; CAPACITY]),
            next: 0,
            len: 0,
            frames: 0,
            flip_us: Box::new([0; CAPACITY]),
            interval_us: Box::new([0; CAPACITY]),
            flip_next: 0,
            flip_len: 0,
            interval_next: 0,
            interval_len: 0,
            flips: 0,
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

    /// Record one page flip: queue-to-vblank latency, and the time since the
    /// output's previous flip (`None` for its first). Allocation-free.
    pub fn record_flip(&mut self, latency: Duration, interval: Option<Duration>) {
        self.flip_us[self.flip_next] = latency.as_micros().min(u32::MAX as u128) as u32;
        self.flip_next = (self.flip_next + 1) % CAPACITY;
        self.flip_len = (self.flip_len + 1).min(CAPACITY);
        self.flips += 1;
        if let Some(i) = interval {
            self.interval_us[self.interval_next] = i.as_micros().min(u32::MAX as u128) as u32;
            self.interval_next = (self.interval_next + 1) % CAPACITY;
            self.interval_len = (self.interval_len + 1).min(CAPACITY);
        }
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
        // The glass figures (`--stats` only, so always present on this line):
        // per composited frame, except GPU stage times, which are per GPU frame
        // read back (the timers lag the frames by a few), and the loss counts,
        // which are totals over the interval.
        let g = take_glass();
        let n = self.frames.max(1) as f64;
        let gf = g.gpu_frames.max(1) as f64;
        let per = |v: u64| v as f64 / n;
        let stage = |k: Span| g.gpu_sum_ns[k as usize] as f64 / gf / 1000.0;
        let mut gpu_total: Vec<u32> = g.gpu_total_us[..g.gpu_len].to_vec();
        gpu_total.sort_unstable();
        let gpu_sum_us = g.gpu_sum_ns.iter().sum::<u64>() as f64 / gf / 1000.0;
        tracing::info!(
            frames = self.frames,
            fps = format_args!("{fps:.1}"),
            render_p50_us = pct(&render, 50),
            render_p99_us = pct(&render, 99),
            submit_p50_us = pct(&submit, 50),
            submit_p99_us = pct(&submit, 99),
            gpu = crate::gpu_timer::mode_name(),
            gpu_frames = g.gpu_frames,
            gpu_backdrop_us = format_args!("{:.1}", stage(Span::Backdrop)),
            gpu_down_us = format_args!("{:.1}", stage(Span::Down)),
            gpu_up_us = format_args!("{:.1}", stage(Span::Up)),
            gpu_pack_us = format_args!("{:.1}", stage(Span::Pack)),
            gpu_blur_us = format_args!("{:.1}", stage(Span::FinalBlur)),
            gpu_frost_us = format_args!("{:.1}", stage(Span::FinalFrost)),
            gpu_glass_us = format_args!("{:.1}", stage(Span::FinalGlass)),
            gpu_sum_us = format_args!("{gpu_sum_us:.1}"),
            gpu_sum_p50_us = pct(&gpu_total, 50),
            gpu_sum_p99_us = pct(&gpu_total, 99),
            gpu_lost = g.lost,
            gpu_disjoint = g.disjoint,
            gpu_skipped = g.skipped,
            blur_hit = format_args!("{:.2}", per(g.hits)),
            blur_held = format_args!("{:.2}", per(g.held)),
            blur_idle = format_args!("{:.2}", per(g.idle)),
            blur_miss = format_args!("{:.2}", per(g.misses.iter().sum())),
            blur_miss_first = format_args!("{:.2}", per(g.misses[Miss::First as usize])),
            blur_miss_mode = format_args!("{:.2}", per(g.misses[Miss::Mode as usize])),
            blur_miss_geom = format_args!("{:.2}", per(g.misses[Miss::Geometry as usize])),
            blur_miss_margin = format_args!("{:.2}", per(g.misses[Miss::Margin as usize])),
            blur_miss_kernel = format_args!("{:.2}", per(g.misses[Miss::Kernel as usize])),
            blur_miss_refr = format_args!("{:.2}", per(g.misses[Miss::Refraction as usize])),
            blur_miss_defer = format_args!("{:.2}", per(g.misses[Miss::Defer as usize])),
            blur_surfaces = format_args!("{:.2}", per(g.surfaces)),
            blur_px = format_args!("{:.0}", per(g.backdrop_px)),
            blur_pass_px = format_args!("{:.0}", per(g.pass_px)),
            "frame stats"
        );
        self.frames = 0;
    }

    /// Summarise the ring without disturbing it. Allocates two vectors the
    /// size of the window; called only from the control socket, never from
    /// the render path.
    pub fn snapshot(&self) -> Snapshot {
        let mut flip: Vec<u32> = self.flip_us[..self.flip_len].to_vec();
        let mut interval: Vec<u32> = self.interval_us[..self.interval_len].to_vec();
        flip.sort_unstable();
        interval.sort_unstable();
        let flips = (
            self.flips,
            pct(&flip, 50),
            pct(&flip, 99),
            pct(&interval, 50),
            pct(&interval, 99),
        );
        if self.len == 0 {
            return Snapshot {
                flips: flips.0,
                flip_p50_us: flips.1,
                flip_p99_us: flips.2,
                interval_p50_us: flips.3,
                interval_p99_us: flips.4,
                ..Snapshot::default()
            };
        }
        let mut render: Vec<u32> = self.render_us[..self.len].to_vec();
        let mut submit: Vec<u32> = self.submit_us[..self.len].to_vec();
        render.sort_unstable();
        submit.sort_unstable();
        Snapshot {
            samples: self.len,
            frames: self.frames,
            render_p50_us: pct(&render, 50),
            render_p99_us: pct(&render, 99),
            submit_p50_us: pct(&submit, 50),
            submit_p99_us: pct(&submit, 99),
            flips: flips.0,
            flip_p50_us: flips.1,
            flip_p99_us: flips.2,
            interval_p50_us: flips.3,
            interval_p99_us: flips.4,
        }
    }
}

/// A point-in-time summary of the ring, for `get_metrics` (COMP-13 §2.1).
#[derive(Debug, Clone, Copy, Default)]
pub struct Snapshot {
    /// Samples currently in the ring.
    pub samples: usize,
    /// Frames composited since the last periodic report.
    pub frames: u64,
    pub render_p50_us: u32,
    pub render_p99_us: u32,
    pub submit_p50_us: u32,
    pub submit_p99_us: u32,
    /// Page flips recorded since start.
    pub flips: u64,
    pub flip_p50_us: u32,
    pub flip_p99_us: u32,
    pub interval_p50_us: u32,
    pub interval_p99_us: u32,
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
    fn flips_record_latency_and_interval() {
        let mut s = FrameStats::new(false);
        s.record_flip(Duration::from_micros(50_000), None);
        s.record_flip(Duration::from_micros(48_000), Some(Duration::from_micros(50_000)));
        let snap = s.snapshot();
        assert_eq!(snap.flips, 2);
        assert_eq!(snap.flip_p99_us, 50_000);
        assert_eq!(snap.interval_p50_us, 50_000);
        assert_eq!(s.interval_len, 1);
    }

    #[test]
    fn ring_wraps_without_growing() {
        let mut s = FrameStats::new(false);
        for _ in 0..(CAPACITY * 2 + 3) {
            s.record(Duration::from_micros(10), Duration::from_micros(1));
        }
        assert_eq!(s.len, CAPACITY);
    }

    #[test]
    fn glass_counters_count_only_with_stats_and_reset_on_take() {
        // Off (the default): every hook is a no-op.
        note_backdrop(Served::Hit);
        note_surface();
        note_rebuild(100, 40);
        assert_eq!(take_glass().hits, 0);
        assert_eq!(take_glass().backdrop_px, 0);

        // `--stats`: the figures fold into one interval, then reset.
        GLASS_ON.with(|on| on.set(true));
        note_backdrop(Served::Hit);
        note_backdrop(Served::Held);
        note_backdrop(Served::Idle);
        note_backdrop(Served::Miss(Miss::Margin));
        note_surface();
        note_rebuild(100, 40);
        note_gpu_frame(&[1000, 2000, 0, 0, 0, 0, 3000, 0, 0, 0]);
        note_gpu_loss(GpuLoss::Skipped);
        let g = take_glass();
        assert_eq!((g.hits, g.held, g.idle), (1, 1, 1));
        assert_eq!((g.misses[Miss::Margin as usize], g.surfaces), (1, 1));
        assert_eq!((g.backdrop_px, g.pass_px), (100, 40));
        assert_eq!((g.gpu_frames, g.skipped), (1, 1));
        assert_eq!(g.gpu_sum_ns[Span::FinalGlass as usize], 3000);
        assert_eq!(g.gpu_total_us[0], 6);
        assert_eq!(g.gpu_len, 1);
        assert_eq!(take_glass().hits, 0, "a take resets the interval");
        GLASS_ON.with(|on| on.set(false));
    }
}
