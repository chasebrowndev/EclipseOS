// SPDX-License-Identifier: AGPL-3.0-only
//! How a Glance (and the clock) is sized inside its tile: one pure table of
//! rules, shared by the greeter, the lock screen and the layout builder's
//! preview so they cannot drift apart (ADR 0126).
//!
//! No iced, no I/O, no dependencies. Every function takes the tile's *inner*
//! box (after [`pad_for`]) in logical px of the real screen, the shape of the
//! data, and the three text sizes the caller's face uses ([`Type`]), and
//! returns plain data: which variant to draw and the sizes to draw it at. The
//! caller only turns that into widgets (or, in the builder, into a miniature
//! scaled by its own factor).
//!
//! Rules every plan keeps:
//! - **Never exceeds the tile.** `used_h` of a plan is at most the inner
//!   height, and a figure is never wider than the inner width, for any input
//!   including zero.
//! - **Degrades in steps that keep size continuous.** full (heading, figure,
//!   rail, note) -> one-row compact -> nothing, and the figure at each
//!   breakpoint is within a small ratio of the next variant's.
//! - **Does not jitter with live data.** Widths are planned against the
//!   widest plausible string for the shape (`100%`, `12:59 AM`, the longest
//!   date), not the current one, so 9:59 -> 10:00 or 99% -> 100% never
//!   reflows the tile.
//! - **Rhythm.** One small gap ([`GAP`]), the heading top-left, the figure
//!   stack hugging the bottom-left, as the clock does.

use crate::{Battery, Charge};

// ---- the tile's own margin ------------------------------------------------

/// Card padding as a share of the tile's short side, and its bounds.
pub const PAD_FRACTION: f32 = 0.08;
pub const PAD_MIN: f32 = 12.0;
pub const PAD_MAX: f32 = 28.0;
/// The pad never exceeds this share of the tile's short side.
pub const PAD_SHARE_MAX: f32 = 0.18;

// ---- text metrics (estimates; the pure side cannot measure) ----------------

/// Line box of a text run, as a share of its size (iced's default).
pub const LINE: f32 = 1.3;
/// Mean advance of a digit / of other text, as a share of its size.
pub const DIGIT_EM: f32 = 0.6;
pub const TEXT_EM: f32 = 0.56;
/// `100%`: the widest figure.
pub const FIGURE_CHARS: f32 = 4.0;

// ---- the shared spacing scale ---------------------------------------------

/// The gap between the parts of a Glance (heading, figure, rail, note).
pub const GAP: f32 = 6.0;
/// The gap between two devices' groups at rest; it grows with spare height.
pub const GROUP_GAP: f32 = 10.0;
pub const GROUP_GAP_MAX: f32 = 24.0;
/// Horizontal gap between a row's cells.
pub const CELL_GAP: f32 = 10.0;
/// The charge rail's height.
pub const RAIL_H: f32 = 6.0;

// ---- figure scale ----------------------------------------------------------

/// A figure is this share of the inner height, clamped.
pub const FIGURE_H: f32 = 0.42;
pub const FIGURE_MIN: f32 = 18.0;
pub const FIGURE_MAX: f32 = 140.0;
/// Below this inner height a Glance is one row: its figure is this share of
/// the row. At [`COMPACT_H`] the full variant's figure (0.42 h) is within
/// about 15% of the compact one's (0.5 h), so the size does not jump.
pub const COMPACT_H: f32 = 68.0;
pub const COMPACT_FIG: f32 = 0.5;
pub const COMPACT_MIN: f32 = 12.0;
/// The compact battery shows its rail beside the figure from this inner width.
pub const COMPACT_RAIL_W: f32 = 120.0;
/// The charge rail shows from this inner height; the note line from [`NOTE_MIN_H`].
pub const DETAIL_MIN_H: f32 = 120.0;
pub const NOTE_MIN_H: f32 = 88.0;
/// Most devices listed.
pub const DEVICES_MAX: usize = 4;
/// "Charging" shows beside a device's name from this inner width.
pub const TAG_MIN_W: f32 = 190.0;
const TAG_CHARS: f32 = 8.0;

// ---- wifi ------------------------------------------------------------------

pub const WIFI_BARS: usize = 4;
/// Link quality per lit bar, percent.
pub const WIFI_STEP_PCT: usize = 25;
pub const BAR_BASE: f32 = 0.35;
pub const BAR_STEP: f32 = 0.2;
pub const BAR_W_OF_FIG: f32 = 0.12;
pub const BAR_W_MIN: f32 = 3.0;
pub const BAR_W_MAX: f32 = 7.0;
pub const BAR_GAP_OF_W: f32 = 0.8;
pub const BAR_GAP_MIN: f32 = 2.5;
pub const BAR_GAP_MAX: f32 = 5.0;
/// Gap between the percentage and the bars, as a share of the figure.
pub const FIG_GAP_OF_FIG: f32 = 0.3;
pub const FIG_GAP_MIN: f32 = 8.0;
pub const FIG_GAP_MAX: f32 = 18.0;

// ---- clock -----------------------------------------------------------------

pub const CLOCK_H: f32 = 0.56;
pub const CLOCK_MIN: f32 = 24.0;
pub const CLOCK_MAX: f32 = 180.0;
/// The date under the numeral, as a share of the numeral, clamped.
pub const DATE_OF_CLOCK: f32 = 0.16;
pub const DATE_MIN: f32 = 12.0;
pub const DATE_MAX: f32 = 28.0;
/// A date that would be smaller than this is not drawn.
pub const DATE_HIDE_BELOW: f32 = 10.0;
/// The gap between numeral and date.
pub const CLOCK_GAP: f32 = 4.0;
/// The widest each form gets, in characters (`12:59 AM`, `23:59`,
/// `Wednesday, September 30`), so the numeral is sized once per preset.
pub const TIME_CHARS_12: usize = 8;
pub const TIME_CHARS_24: usize = 5;
pub const DATE_CHARS_MAX: usize = 23;

/// The three text sizes the caller's face uses inside a Glance at 1:1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Type {
    /// The small-caps heading and notes.
    pub micro: f32,
    /// A device's name.
    pub body_small: f32,
    /// A device's percentage.
    pub body: f32,
}

/// A tile's inner box, logical px.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Inner {
    pub w: f32,
    pub h: f32,
}

/// The inner margin of a tile: a share of its short side, clamped, and never
/// more than [`PAD_SHARE_MAX`] of that side, so a one-row tile keeps its content.
pub fn pad_for(w: f32, h: f32) -> f32 {
    let short = w.min(h).max(0.0);
    (short * PAD_FRACTION)
        .clamp(PAD_MIN, PAD_MAX)
        .min(short * PAD_SHARE_MAX)
}

/// The inner box of a `w` x `h` tile.
pub fn inner(w: f32, h: f32) -> Inner {
    let pad = pad_for(w, h);
    Inner {
        w: (w - 2.0 * pad).max(0.0),
        h: (h - 2.0 * pad).max(0.0),
    }
}

/// Clamp `avail` into `[lo, hi]`, but never above `avail` itself: a size is a
/// hint, and a tile never gets a size it cannot hold.
pub fn fit_in(avail: f32, lo: f32, hi: f32) -> f32 {
    let avail = if avail.is_finite() { avail.max(0.0) } else { 0.0 };
    avail.min(hi).max(lo.min(avail))
}

/// The line box of text of `px`.
pub fn line(px: f32) -> f32 {
    px * LINE
}

/// How many characters of `px` text fit `w`.
pub fn chars_fit(w: f32, px: f32) -> usize {
    if px <= 0.0 || w <= 0.0 {
        return 0;
    }
    (w / (px * TEXT_EM)).floor() as usize
}

// ---- battery -----------------------------------------------------------------

/// What a battery tile draws.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BatteryPlan {
    /// "No battery", centred.
    Empty,
    /// One row, centred vertically: the figure and, with room, the rail.
    Compact { figure: f32, rail: bool },
    /// Heading at the top; the figure, rail and note hug the bottom.
    One { figure: f32, rail: bool, note: bool },
    /// Heading at the top, then `shown` devices: name, tag, percentage and
    /// (with `rail`) a rail each, `gap` apart. Names are cut to `name_chars`.
    Many {
        shown: usize,
        rail: bool,
        tag: bool,
        gap: f32,
        name_chars: usize,
        /// The rows' text and rail grow by this factor (1.0 up to
        /// [`ROWS_SCALE_MAX`]) when the tile is tall for its few devices.
        scale: f32,
    },
}

/// The most a device list's rows grow to fill a tall tile.
pub const ROWS_SCALE_MAX: f32 = 1.8;

impl Type {
    /// The sizes of a device row grown by `scale`.
    pub fn scaled(self, scale: f32) -> Type {
        Type {
            micro: self.micro * scale,
            body_small: self.body_small * scale,
            body: self.body * scale,
        }
    }
}

impl BatteryPlan {
    /// The height the plan occupies; never above the inner height.
    pub fn used_h(&self, t: Type) -> f32 {
        let head = line(t.micro) + GAP;
        match *self {
            BatteryPlan::Empty => line(t.body_small),
            BatteryPlan::Compact { figure, .. } => line(figure),
            BatteryPlan::One { figure, rail, note } => {
                head + line(figure)
                    + if rail { GAP + RAIL_H } else { 0.0 }
                    + if note { GAP + line(t.micro) } else { 0.0 }
            }
            BatteryPlan::Many {
                shown,
                rail,
                gap,
                scale,
                ..
            } => {
                let one = line(t.body * scale) + if rail { GAP + RAIL_H * scale } else { 0.0 };
                head + shown as f32 * one + shown.saturating_sub(1) as f32 * gap
            }
        }
    }
}

/// Plan a battery tile for `batteries` (internal first) in `inner`.
pub fn battery(inner: Inner, batteries: &[Battery], t: Type) -> BatteryPlan {
    let Inner { w, h } = Inner {
        w: inner.w.max(0.0),
        h: inner.h.max(0.0),
    };
    if batteries.is_empty() {
        return BatteryPlan::Empty;
    }
    let width_fig = w / (FIGURE_CHARS * DIGIT_EM);
    if h < COMPACT_H {
        let figure = fit_in((h / LINE).min(h * COMPACT_FIG), COMPACT_MIN, FIGURE_MAX).min(width_fig.max(0.0));
        let rail = w >= COMPACT_RAIL_W && w - figure * FIGURE_CHARS * DIGIT_EM >= COMPACT_RAIL_W * 0.4;
        return BatteryPlan::Compact { figure, rail };
    }
    let head = line(t.micro) + GAP;
    if batteries.len() == 1 {
        let rail = h >= DETAIL_MIN_H;
        let note = h >= NOTE_MIN_H;
        let extra = if rail { GAP + RAIL_H } else { 0.0 } + if note { GAP + line(t.micro) } else { 0.0 };
        let room = ((h - head - extra) / LINE).max(0.0);
        let figure = fit_in((h * FIGURE_H).min(width_fig).min(room), FIGURE_MIN, FIGURE_MAX);
        return BatteryPlan::One { figure, rail, note };
    }
    let want = batteries.len().min(DEVICES_MAX);
    let avail = (h - head).max(0.0);
    let rail = h >= DETAIL_MIN_H;
    let one_of = |rail: bool, k: f32| line(t.body * k) + if rail { GAP + RAIL_H * k } else { 0.0 };
    let fits = |rail: bool, gap: f32, k: f32| {
        let one = one_of(rail, k);
        (((avail + gap) / (one + gap)).floor().max(0.0) as usize).min(want)
    };
    let mut rail = rail;
    let mut shown = fits(rail, GROUP_GAP, 1.0);
    if rail && shown < want && fits(false, GROUP_GAP, 1.0) > shown {
        rail = false;
        shown = fits(false, GROUP_GAP, 1.0);
    }
    let shown = shown.max(1);
    // Grow the rows into a tall tile, never past what fits the width or the
    // tile, so two devices in a tall tile are not a short list over a blank.
    let need1 = shown as f32 * one_of(rail, 1.0) + shown.saturating_sub(1) as f32 * GROUP_GAP;
    let by_w = (0.4 * w / (FIGURE_CHARS * DIGIT_EM * t.body)).max(1.0);
    let mut scale = (avail / need1.max(1.0) * 0.55)
        .clamp(1.0, ROWS_SCALE_MAX)
        .min(by_w);
    while scale > 1.0
        && shown as f32 * one_of(rail, scale) + shown.saturating_sub(1) as f32 * GROUP_GAP > avail
    {
        scale = (scale * 0.95).max(1.0);
    }
    let one = one_of(rail, scale);
    let spare = (avail - shown as f32 * one - shown.saturating_sub(1) as f32 * GROUP_GAP).max(0.0);
    let gap = if shown > 1 {
        (GROUP_GAP + spare / (shown - 1) as f32).min(GROUP_GAP_MAX * scale)
    } else {
        GROUP_GAP
    };
    let st = t.scaled(scale);
    let tag = w >= TAG_MIN_W;
    let reserve = FIGURE_CHARS * DIGIT_EM * st.body
        + CELL_GAP
        + if tag {
            CELL_GAP + TAG_CHARS * TEXT_EM * st.micro
        } else {
            0.0
        };
    BatteryPlan::Many {
        shown,
        rail,
        tag,
        gap,
        name_chars: chars_fit(w - reserve, st.body_small).max(1),
        scale,
    }
}

/// `s` cut to at most `max` characters, with an ellipsis when it was cut.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out = out.trim_end().to_owned();
    out.push('\u{2026}');
    out
}

/// The word under a single battery's figure.
pub fn charge_note(c: Charge) -> &'static str {
    match c {
        Charge::Charging => "Charging",
        Charge::Discharging => "On battery",
        Charge::Full => "Full",
        Charge::Unknown => "Unknown",
    }
}

// ---- wifi --------------------------------------------------------------------

/// The signal bars.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bars {
    pub lit: usize,
    pub w: f32,
    pub gap: f32,
}

impl Bars {
    pub fn width(&self) -> f32 {
        WIFI_BARS as f32 * self.w + (WIFI_BARS - 1) as f32 * self.gap
    }
}

/// Height of bar `i` for a figure of `figure` px.
pub fn bar_h(figure: f32, i: usize) -> f32 {
    figure * (BAR_BASE + BAR_STEP * i as f32)
}

/// Bars lit for a link quality, percent.
pub fn lit_bars(percent: u8) -> usize {
    usize::from(percent.min(100))
        .div_ceil(WIFI_STEP_PCT)
        .min(WIFI_BARS)
}

/// What a wifi tile draws.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WifiPlan {
    /// "No link", centred.
    Empty,
    /// The figure and bars on one row, centred vertically, no heading.
    Compact {
        figure: f32,
        bars: Option<Bars>,
        gap: f32,
    },
    /// Heading at the top; the figure and bars hug the bottom.
    Full {
        figure: f32,
        bars: Option<Bars>,
        gap: f32,
    },
}

impl WifiPlan {
    pub fn used_h(&self, t: Type) -> f32 {
        match *self {
            WifiPlan::Empty => line(t.body_small),
            WifiPlan::Compact { figure, .. } => line(figure),
            WifiPlan::Full { figure, .. } => line(t.micro) + GAP + line(figure),
        }
    }

    pub fn used_w(&self) -> f32 {
        let fig =
            |f: f32, b: Option<Bars>, g: f32| f * FIGURE_CHARS * DIGIT_EM + b.map_or(0.0, |b| g + b.width());
        match *self {
            WifiPlan::Empty => 0.0,
            WifiPlan::Compact { figure, bars, gap } | WifiPlan::Full { figure, bars, gap } => {
                fig(figure, bars, gap)
            }
        }
    }
}

/// Plan a wifi tile for the link `quality` (`None`: no link) in `inner`.
pub fn wifi(inner: Inner, quality: Option<u8>, t: Type) -> WifiPlan {
    let (w, h) = (inner.w.max(0.0), inner.h.max(0.0));
    let Some(q) = quality else {
        return WifiPlan::Empty;
    };
    let compact = h < COMPACT_H;
    let head = if compact { 0.0 } else { line(t.micro) + GAP };
    let by_h = if compact {
        fit_in((h / LINE).min(h * COMPACT_FIG), COMPACT_MIN, FIGURE_MAX)
    } else {
        fit_in(
            (h * FIGURE_H).min(((h - head) / LINE).max(0.0)),
            FIGURE_MIN,
            FIGURE_MAX,
        )
    };
    let text_w = |f: f32| f * FIGURE_CHARS * DIGIT_EM;
    let no_bars = by_h.min(w / (FIGURE_CHARS * DIGIT_EM));
    let bar = |f: f32| {
        let bw = (f * BAR_W_OF_FIG).clamp(BAR_W_MIN, BAR_W_MAX);
        (bw, (bw * BAR_GAP_OF_W).clamp(BAR_GAP_MIN, BAR_GAP_MAX))
    };
    let fig_gap = |f: f32| (f * FIG_GAP_OF_FIG).clamp(FIG_GAP_MIN, FIG_GAP_MAX);
    // Bars stay while the figure keeps most of its size beside them.
    let (bw, bg) = bar(by_h);
    let bars_w = WIFI_BARS as f32 * bw + (WIFI_BARS - 1) as f32 * bg;
    let beside = ((w - bars_w - fig_gap(by_h)) / (FIGURE_CHARS * DIGIT_EM)).min(by_h);
    let (figure, bars) = if beside >= 0.6 * no_bars && beside >= COMPACT_MIN {
        (
            beside.max(0.0),
            Some(Bars {
                lit: lit_bars(q),
                w: bw,
                gap: bg,
            }),
        )
    } else {
        (no_bars, None)
    };
    debug_assert!(text_w(figure) <= w + 0.01 || w == 0.0);
    let gap = fig_gap(figure);
    if compact {
        WifiPlan::Compact { figure, bars, gap }
    } else {
        WifiPlan::Full { figure, bars, gap }
    }
}

// ---- clock -------------------------------------------------------------------

/// Where the clock sits in its tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    /// A wide tile sets the numeral low-left.
    BottomLeft,
    /// A tall, narrow one centres it.
    Centre,
}

/// What a clock tile draws.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClockPlan {
    pub numeral: f32,
    /// The date line's size, when it fits.
    pub date: Option<f32>,
    pub align: Align,
}

impl ClockPlan {
    pub fn used_h(&self) -> f32 {
        line(self.numeral) + self.date.map_or(0.0, |d| CLOCK_GAP + line(d))
    }
}

/// The widest time string of a preset, in characters.
pub fn time_chars(twelve: bool) -> usize {
    if twelve {
        TIME_CHARS_12
    } else {
        TIME_CHARS_24
    }
}

/// Plan a clock tile for a preset (`twelve`-hour or not, with a `date` line or
/// not) in `inner`. Sized against the preset's widest strings, so the time
/// rolling over never changes it.
pub fn clock(inner: Inner, twelve: bool, date: bool) -> ClockPlan {
    let (w, h) = (inner.w.max(0.0), inner.h.max(0.0));
    let by_w = w / (time_chars(twelve) as f32 * DIGIT_EM);
    let date_for = |n: f32| {
        let d = (n * DATE_OF_CLOCK).clamp(DATE_MIN, DATE_MAX);
        d.min(w / (DATE_CHARS_MAX as f32 * TEXT_EM))
    };
    let mut numeral = fit_in((h * CLOCK_H).min(by_w), CLOCK_MIN, CLOCK_MAX);
    let mut date_px = None;
    if date {
        let mut n = numeral;
        for _ in 0..3 {
            let d = date_for(n);
            // The date takes its height out of the numeral's.
            n = numeral.min(((h - CLOCK_GAP - line(d)) / LINE).max(0.0));
        }
        let d = date_for(n);
        if d >= DATE_HIDE_BELOW && n >= CLOCK_MIN.min(by_w) {
            numeral = n;
            date_px = Some(d);
        } else {
            numeral = fit_in((h / LINE).min(numeral), CLOCK_MIN, CLOCK_MAX);
        }
    } else {
        numeral = numeral.min(h / LINE);
    }
    ClockPlan {
        numeral,
        date: date_px,
        align: if h > w { Align::Centre } else { Align::BottomLeft },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Type = Type {
        micro: 10.0,
        body_small: 12.0,
        body: 13.0,
    };

    fn bat(n: usize) -> Vec<Battery> {
        (0..n)
            .map(|i| Battery {
                internal: i == 0,
                name: format!("Device {i}"),
                percent: ((i * 37) % 101) as u8,
                charge: Charge::Discharging,
            })
            .collect()
    }

    fn sizes() -> Vec<f32> {
        let mut v = vec![0.0, 1.0, 5.0, 20.0];
        let mut s = 30.0;
        while s <= 1900.0 {
            v.push(s);
            s += 17.0;
        }
        v
    }

    #[test]
    fn nothing_panics_or_exceeds_the_tile() {
        for w in sizes() {
            for h in sizes() {
                let i = Inner { w, h };
                for n in [0, 1, 2, 5] {
                    let p = battery(i, &bat(n), T);
                    assert!(
                        p.used_h(T) <= h + 0.01 || matches!(p, BatteryPlan::Empty),
                        "{w}x{h} n{n} {p:?}"
                    );
                    if let BatteryPlan::One { figure, .. } | BatteryPlan::Compact { figure, .. } = p {
                        assert!(figure * FIGURE_CHARS * DIGIT_EM <= w + 0.01, "{w}x{h} {p:?}");
                    }
                }
                for q in [None, Some(0), Some(1), Some(50), Some(100), Some(255)] {
                    let p = wifi(i, q, T);
                    assert!(
                        p.used_h(T) <= h + 0.01 || matches!(p, WifiPlan::Empty),
                        "{w}x{h} {p:?}"
                    );
                    assert!(p.used_w() <= w + 0.01, "{w}x{h} {p:?}");
                }
                for (tw, d) in [(false, false), (false, true), (true, false), (true, true)] {
                    let p = clock(i, tw, d);
                    assert!(p.used_h() <= h + 0.01, "{w}x{h} {p:?}");
                    assert!(
                        p.numeral * time_chars(tw) as f32 * DIGIT_EM <= w + 0.01,
                        "{w}x{h} {p:?}"
                    );
                    assert!(p.numeral.is_finite() && p.numeral >= 0.0);
                }
            }
        }
    }

    #[test]
    fn variant_follows_the_data_shape() {
        let big = Inner { w: 300.0, h: 200.0 };
        assert_eq!(battery(big, &[], T), BatteryPlan::Empty);
        assert!(matches!(
            battery(big, &bat(1), T),
            BatteryPlan::One {
                rail: true,
                note: true,
                ..
            }
        ));
        assert!(matches!(
            battery(big, &bat(2), T),
            BatteryPlan::Many { shown: 2, .. }
        ));
        let BatteryPlan::Many { scale, .. } = battery(Inner { w: 300.0, h: 600.0 }, &bat(2), T) else {
            panic!()
        };
        assert!(scale > 1.0 && scale <= ROWS_SCALE_MAX);
        assert!(matches!(
            battery(big, &bat(9), T),
            BatteryPlan::Many {
                shown: DEVICES_MAX,
                ..
            }
        ));
        assert!(matches!(
            battery(Inner { w: 300.0, h: 40.0 }, &bat(3), T),
            BatteryPlan::Compact { .. }
        ));
        assert_eq!(wifi(big, None, T), WifiPlan::Empty);
        assert!(matches!(
            wifi(big, Some(60), T),
            WifiPlan::Full { bars: Some(_), .. }
        ));
        assert!(matches!(
            wifi(Inner { w: 300.0, h: 40.0 }, Some(60), T),
            WifiPlan::Compact { .. }
        ));
    }

    #[test]
    fn short_tiles_drop_detail_before_the_figure() {
        let p = |h| battery(Inner { w: 300.0, h }, &bat(1), T);
        assert!(matches!(
            p(70.0),
            BatteryPlan::One {
                rail: false,
                note: false,
                ..
            }
        ));
        assert!(matches!(
            p(100.0),
            BatteryPlan::One {
                rail: false,
                note: true,
                ..
            }
        ));
        assert!(matches!(
            p(130.0),
            BatteryPlan::One {
                rail: true,
                note: true,
                ..
            }
        ));
    }

    #[test]
    fn truncate_cuts_with_an_ellipsis() {
        assert_eq!(truncate("Mouse", 8), "Mouse");
        assert_eq!(truncate("Keyboard K380", 6), "Keybo\u{2026}");
        assert_eq!(truncate("abc", 0), "\u{2026}");
    }

    #[test]
    fn lit_bars_follow_quality() {
        assert_eq!(lit_bars(0), 0);
        assert_eq!(lit_bars(1), 1);
        assert_eq!(lit_bars(25), 1);
        assert_eq!(lit_bars(26), 2);
        assert_eq!(lit_bars(100), WIFI_BARS);
        assert_eq!(lit_bars(255), WIFI_BARS);
        let Inner { .. } = Inner { w: 0.0, h: 0.0 };
        let WifiPlan::Full { bars: Some(b), .. } = wifi(Inner { w: 300.0, h: 160.0 }, Some(60), T) else {
            panic!("full plan with bars");
        };
        assert_eq!(b.lit, 3);
    }

    #[test]
    fn a_figure_never_shrinks_as_its_tile_grows() {
        let fig = |h: f32| match battery(Inner { w: 320.0, h }, &bat(1), T) {
            BatteryPlan::One { figure, .. } | BatteryPlan::Compact { figure, .. } => figure,
            _ => 0.0,
        };
        let mut prev = fig(30.0);
        let mut h = 31.0;
        while h < 400.0 {
            let f = fig(h);
            // Monotonic, except the one step at the compact breakpoint, which
            // is bounded.
            if (h - COMPACT_H).abs() > 1.0 {
                assert!(f + 0.01 >= prev, "h {h}: {f} < {prev}");
            } else {
                assert!(f >= prev * 0.6, "h {h}: {f} vs {prev}");
            }
            prev = f;
            h += 1.0;
        }
    }

    #[test]
    fn the_clock_does_not_depend_on_the_current_string() {
        // The plan has no string input: rolling 9:59 to 10:00 cannot move it.
        let a = clock(Inner { w: 500.0, h: 200.0 }, true, true);
        let b = clock(Inner { w: 500.0, h: 200.0 }, true, true);
        assert_eq!(a, b);
        assert_eq!(a.align, Align::BottomLeft);
        assert_eq!(
            clock(Inner { w: 150.0, h: 400.0 }, false, false).align,
            Align::Centre
        );
    }

    #[test]
    fn the_date_shows_only_with_room_and_the_preset() {
        let i = Inner { w: 600.0, h: 220.0 };
        assert!(clock(i, true, true).date.is_some());
        assert!(clock(i, true, false).date.is_none());
        assert!(clock(Inner { w: 80.0, h: 220.0 }, true, true).date.is_none());
        // A wider preset string gets a smaller numeral in the same tile.
        let narrow = Inner { w: 220.0, h: 200.0 };
        assert!(clock(narrow, true, false).numeral < clock(narrow, false, false).numeral);
    }

    #[test]
    fn the_clock_numeral_is_monotonic_in_both_axes() {
        for (tw, d) in [(false, false), (true, true)] {
            let mut prev = 0.0;
            let mut h = 40.0;
            while h < 900.0 {
                let n = clock(Inner { w: 700.0, h }, tw, d).numeral;
                assert!(n + 0.01 >= prev, "h {h}: {n} < {prev}");
                prev = n;
                h += 7.0;
            }
            let mut prev = 0.0;
            let mut w = 40.0;
            while w < 1500.0 {
                let n = clock(Inner { w, h: 300.0 }, tw, d).numeral;
                assert!(n + 0.01 >= prev, "w {w}: {n} < {prev}");
                prev = n;
                w += 7.0;
            }
        }
    }

    #[test]
    fn the_pad_is_bounded_by_the_tile() {
        for w in sizes() {
            for h in sizes() {
                let p = pad_for(w, h);
                assert!(p >= 0.0 && 2.0 * p <= w.min(h).max(0.0) * 2.0 * PAD_SHARE_MAX + 0.01);
                let i = inner(w, h);
                assert!(i.w <= w && i.h <= h && i.w >= 0.0 && i.h >= 0.0);
            }
        }
    }
}
