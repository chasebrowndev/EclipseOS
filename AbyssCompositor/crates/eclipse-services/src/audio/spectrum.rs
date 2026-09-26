// SPDX-License-Identifier: AGPL-3.0-only
//! The monitor tap's band reduction: mono samples in, sixteen smoothed
//! `0.0..=1.0` levels out, about sixty times a second.
//!
//! Every sample lives in [`Analyzer`]'s fixed ring, overwritten in place, and
//! in the FFT's own fixed buffers; nothing is appended, kept past the next
//! window, or copied anywhere else (ADR 0065, "The monitor tap").
//!
//! Each band sums the power of the FFT bins it covers. The bands are
//! log-spaced, so a band's width in bins grows with frequency the way music's
//! energy falls off: pink noise lands level across the bar instead of piling
//! up at the bass end. Power is taken relative to a full-scale sine, so a
//! sine at 0 dBFS reads 0 dB in its band whatever the window length.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

use super::{Bands, BANDS};

/// Samples per analysis window. At 48 kHz this is 85 ms, which gives 11.7 Hz
/// bins: fine enough that the narrowest (lowest) band still owns a bin.
pub(super) const FFT_LEN: usize = 4096;

/// How often a new set of levels is produced.
const FRAMES_PER_SECOND: u32 = 60;

/// The outer edges of the band layout.
const LOW_HZ: f32 = 40.0;
const HIGH_HZ: f32 = 16_000.0;

/// A band at or below `DB_FLOOR` (relative to a full-scale sine) draws empty;
/// at or above `DB_CEIL` it draws full.
const DB_FLOOR: f32 = -60.0;
const DB_CEIL: f32 = -6.0;

/// Per-frame smoothing: how far a band moves toward its new target, rising
/// and falling. Fast attack, slow decay: bars jump with a hit and fall back
/// like a meter, instead of flickering with every window.
const ATTACK: f32 = 0.6;
const DECAY: f32 = 0.12;

/// Turns a stream of mono samples into [`Bands`].
pub(super) struct Analyzer {
    ring: Box<[f32]>,
    /// Next write position in `ring`; also the oldest sample.
    head: usize,
    /// Samples pushed since the last frame.
    since: usize,
    hop: usize,
    window: Box<[f32]>,
    fft: Arc<dyn RealToComplex<f32>>,
    input: Vec<f32>,
    output: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    ranges: [(usize, usize); BANDS],
    /// One-sided band power of a full-scale sine: the 0 dB reference.
    reference: f32,
    smoother: Smoother,
}

impl Analyzer {
    pub(super) fn new(sample_rate: u32) -> Self {
        let sample_rate = sample_rate.max(1);
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT_LEN);
        let window: Box<[f32]> = (0..FFT_LEN)
            .map(|i| {
                let x = std::f32::consts::TAU * i as f32 / FFT_LEN as f32;
                0.5 - 0.5 * x.cos()
            })
            .collect();
        // Parseval: a sine of amplitude 1 puts N * sum(w^2) / 4 into the
        // positive-frequency half of the spectrum.
        let reference = FFT_LEN as f32 * window.iter().map(|w| w * w).sum::<f32>() / 4.0;
        Self {
            ring: vec![0.0; FFT_LEN].into_boxed_slice(),
            head: 0,
            since: 0,
            hop: (sample_rate / FRAMES_PER_SECOND).max(1) as usize,
            window,
            input: fft.make_input_vec(),
            output: fft.make_output_vec(),
            scratch: fft.make_scratch_vec(),
            fft,
            ranges: band_ranges(sample_rate, FFT_LEN),
            reference,
            smoother: Smoother::default(),
        }
    }

    /// Feed one sample. Every `hop` samples this returns a new frame of
    /// smoothed levels.
    pub(super) fn push(&mut self, sample: f32) -> Option<Bands> {
        self.ring[self.head] = if sample.is_finite() { sample } else { 0.0 };
        self.head = (self.head + 1) % FFT_LEN;
        self.since += 1;
        if self.since < self.hop {
            return None;
        }
        self.since = 0;
        let target = self.levels();
        Some(Bands(self.smoother.step(&target)))
    }

    /// Unsmoothed levels of the current window.
    fn levels(&mut self) -> [f32; BANDS] {
        let (old, new) = self.ring.split_at(self.head);
        for ((slot, s), w) in self
            .input
            .iter_mut()
            .zip(new.iter().chain(old))
            .zip(self.window.iter())
        {
            *slot = s * w;
        }
        if self
            .fft
            .process_with_scratch(&mut self.input, &mut self.output, &mut self.scratch)
            .is_err()
        {
            return [0.0; BANDS];
        }
        let mut out = [0.0; BANDS];
        for (level, &(start, end)) in out.iter_mut().zip(self.ranges.iter()) {
            let power: f32 = self.output[start..end].iter().map(|c| c.norm_sqr()).sum();
            *level = level_of(power / self.reference);
        }
        out
    }
}

/// Map band power (relative to a full-scale sine) to `0.0..=1.0`.
fn level_of(relative_power: f32) -> f32 {
    if relative_power <= 0.0 || !relative_power.is_finite() {
        return 0.0;
    }
    let db = 10.0 * relative_power.log10();
    ((db - DB_FLOOR) / (DB_CEIL - DB_FLOOR)).clamp(0.0, 1.0)
}

/// Lower edge of band `i` (and upper edge of band `i - 1`), in Hz.
fn edge_hz(i: usize) -> f32 {
    LOW_HZ * (HIGH_HZ / LOW_HZ).powf(i as f32 / BANDS as f32)
}

/// The half-open FFT bin range each band sums. Ranges are contiguous and
/// never empty: a band narrower than a bin still gets the next bin, and the
/// bands above it shift up to follow.
fn band_ranges(sample_rate: u32, fft_len: usize) -> [(usize, usize); BANDS] {
    let bins = fft_len / 2 + 1;
    let bin_of = |hz: f32| (hz * fft_len as f32 / sample_rate as f32).ceil() as usize;
    let mut ranges = [(0, 0); BANDS];
    let mut prev_end = 1; // skip DC
    for (i, range) in ranges.iter_mut().enumerate() {
        let start = bin_of(edge_hz(i)).max(prev_end).min(bins - 1);
        let end = bin_of(edge_hz(i + 1)).max(start + 1).min(bins);
        *range = (start, end);
        prev_end = end;
    }
    ranges
}

/// Per-band attack/decay, one step per frame.
#[derive(Debug, Default)]
struct Smoother {
    levels: [f32; BANDS],
}

impl Smoother {
    fn step(&mut self, target: &[f32; BANDS]) -> [f32; BANDS] {
        for (level, &t) in self.levels.iter_mut().zip(target) {
            let rate = if t > *level { ATTACK } else { DECAY };
            *level += (t - *level) * rate;
        }
        self.levels
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    /// Run `seconds` of `f(t)` through a fresh analyzer and return the last
    /// frame's unsmoothed levels.
    fn levels_of(f: impl Fn(f32) -> f32) -> [f32; BANDS] {
        let mut a = Analyzer::new(RATE);
        for n in 0..FFT_LEN {
            a.push(f(n as f32 / RATE as f32));
        }
        a.levels()
    }

    fn sine(hz: f32, amp: f32) -> impl Fn(f32) -> f32 {
        move |t| amp * (std::f32::consts::TAU * hz * t).sin()
    }

    fn band_of(hz: f32) -> usize {
        (0..BANDS)
            .find(|&i| hz >= edge_hz(i) && hz < edge_hz(i + 1))
            .unwrap()
    }

    fn loudest(levels: &[f32; BANDS]) -> usize {
        (0..BANDS)
            .max_by(|&a, &b| levels[a].total_cmp(&levels[b]))
            .unwrap()
    }

    #[test]
    fn a_1khz_tone_peaks_its_own_band() {
        let levels = levels_of(sine(1000.0, 0.5));
        assert_eq!(band_of(1000.0), 8);
        assert_eq!(loudest(&levels), 8);
        // A -6 dBFS tone sits at the top of the scale; its neighbours are
        // well down.
        assert!(levels[8] > 0.9, "{levels:?}");
        assert!(levels[7] < 0.5 && levels[9] < 0.5, "{levels:?}");
    }

    #[test]
    fn low_and_high_tones_land_at_the_ends() {
        assert_eq!(loudest(&levels_of(sine(50.0, 0.5))), band_of(50.0));
        assert_eq!(loudest(&levels_of(sine(10_000.0, 0.5))), band_of(10_000.0));
        assert_eq!(band_of(50.0), 0);
    }

    #[test]
    fn silence_is_empty() {
        assert_eq!(levels_of(|_| 0.0), [0.0; BANDS]);
    }

    #[test]
    fn a_quiet_tone_reads_lower_than_a_loud_one() {
        let loud = levels_of(sine(1000.0, 0.5))[8];
        let quiet = levels_of(sine(1000.0, 0.01))[8];
        assert!(quiet > 0.0 && quiet < loud, "{quiet} vs {loud}");
    }

    #[test]
    fn ranges_are_contiguous_and_never_empty() {
        let r = band_ranges(RATE, FFT_LEN);
        assert!(r[0].0 >= 1);
        for w in r.windows(2) {
            assert_eq!(w[0].1, w[1].0);
        }
        assert!(r.iter().all(|&(s, e)| e > s));
        assert!(r[BANDS - 1].1 <= FFT_LEN / 2 + 1);
    }

    #[test]
    fn frames_come_at_sixty_hertz() {
        let mut a = Analyzer::new(RATE);
        let frames = (0..RATE).filter_map(|_| a.push(0.0)).count();
        assert_eq!(frames, 60);
    }

    #[test]
    fn attack_is_fast_and_decay_is_slow() {
        let mut s = Smoother::default();
        let up = s.step(&[1.0; BANDS])[0];
        assert!((up - ATTACK).abs() < 1e-6);
        let mut s = Smoother { levels: [1.0; BANDS] };
        let down = s.step(&[0.0; BANDS])[0];
        assert!((down - (1.0 - DECAY)).abs() < 1e-6);
        assert!(up > 1.0 - down, "rises faster than it falls");
        // And it settles on the target either way.
        for _ in 0..200 {
            s.step(&[0.25; BANDS]);
        }
        assert!((s.levels[0] - 0.25).abs() < 1e-3);
    }

    #[test]
    fn non_finite_samples_do_not_poison_the_window() {
        let mut a = Analyzer::new(RATE);
        a.push(f32::NAN);
        a.push(f32::INFINITY);
        assert!(a.levels().iter().all(|l| l.is_finite()));
    }
}
