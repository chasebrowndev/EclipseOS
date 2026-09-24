// SPDX-License-Identifier: AGPL-3.0-only
//! The whole animation as pure functions of time.
//!
//! Everything in `reference/welcome.html`'s `frame()` that is arithmetic lives
//! here, ported one line at a time and with the same constants. Nothing in this
//! module knows about iced, the clock or the window: give it `t` (animation
//! seconds, already multiplied by [`SPEED`]) and it returns what every layer
//! looks like at that instant. That is what makes the phase boundaries
//! unit-testable, and it is why the canvas layer has no state of its own.

/// Wall-clock seconds to animation seconds. The reference's `SPEED`.
pub const SPEED: f32 = 0.9;
/// Length of the greeting fly-by, in animation seconds. The reference's `T`.
pub const T: f32 = 6.0;
/// The moment Space starts to mean something: the keycap has arrived. Before
/// it, pressing does nothing at all.
pub const READY: f32 = 8.4;
/// Reduced motion parks the animation here: everything settled, nothing moving.
pub const REDUCED_T: f32 = 20.0;
/// The fade to black after Space, in wall-clock milliseconds.
pub const EXIT_MS: f32 = 700.0;

/// How a greeting is drawn: which family the reference lists first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Script {
    Latin,
    Japanese,
    ChineseSimplified,
    Korean,
}

/// The ten greetings, in the reference's order. The language tag is not kept:
/// it only drove the browser's font selection, and [`Script`] does that here.
pub const WORDS: [(&str, Script); 10] = [
    ("Welcome", Script::Latin),
    ("Bienvenue", Script::Latin),
    ("Willkommen", Script::Latin),
    ("Bienvenido", Script::Latin),
    ("Benvenuto", Script::Latin),
    ("Добро пожаловать", Script::Latin),
    ("ようこそ", Script::Japanese),
    ("欢迎", Script::ChineseSimplified),
    ("환영합니다", Script::Korean),
    ("Bem-vindo", Script::Latin),
];

/// The reference's even slot: `T / 10`. Kept as the yardstick for how fast a
/// word is moving (the blur, skew and fade thresholds were tuned against it).
pub const WORD_SPAN: f32 = T / WORDS.len() as f32;

/// The owner's addition: the English greeting is the one a person reads, so it
/// holds 2.5 even slots. The nine others share what is left, so the fly-by
/// still ends at [`T`] and nothing after it moves.
pub const WELCOME_SPAN: f32 = 2.5 * WORD_SPAN;

/// Seconds each of the other nine greetings owns: `(T - 1.5) / 9 = 0.5`.
pub const OTHER_SPAN: f32 = (T - WELCOME_SPAN) / (WORDS.len() - 1) as f32;

/// Seconds a greeting spends flying in (and again flying out). The same for
/// every word, so English arrives and leaves at the speed the others do and
/// only its settle, the near-stationary middle, is longer.
pub const FLIGHT_SECS: f32 = 0.22 * OTHER_SPAN;

/// Which greeting owns animation time `t` (`0..T`), and how far through its
/// own slot (`0..1`) it is.
pub fn slot(t: f32) -> (usize, f32) {
    if t < WELCOME_SPAN {
        (0, t / WELCOME_SPAN)
    } else {
        let u = t - WELCOME_SPAN;
        let i = ((u / OTHER_SPAN).floor() as usize).min(WORDS.len() - 2);
        (i + 1, (u - i as f32 * OTHER_SPAN) / OTHER_SPAN)
    }
}

/// The length of greeting `index`'s slot, seconds.
pub fn span(index: usize) -> f32 {
    if index == 0 {
        WELCOME_SPAN
    } else {
        OTHER_SPAN
    }
}

pub fn cl(x: f32) -> f32 {
    x.clamp(0.0, 1.0)
}

/// Ease out, cubic.
pub fn eo(x: f32) -> f32 {
    1.0 - (1.0 - cl(x)).powi(3)
}

/// Ease in-out, cubic.
pub fn eio(x: f32) -> f32 {
    let x = cl(x);
    if x < 0.5 {
        4.0 * x * x * x
    } else {
        1.0 - (-2.0 * x + 2.0).powi(3) / 2.0
    }
}

/// Half-cosine ease. The shrink into the O composes it with itself.
pub fn sine(x: f32) -> f32 {
    0.5 - 0.5 * (std::f32::consts::PI * cl(x)).cos()
}

pub fn lerp(a: f32, b: f32, p: f32) -> f32 {
    a + (b - a) * p
}

/// A greeting's horizontal position, in `cqw` from centre, over its own span
/// `f` in `0..=1`: flies in fast and decelerates to a slow drift, drifts, then
/// accelerates out. The piecewise curve is the reference's `xf`, whose flights
/// take `0.22` of the span each.
pub fn xf(f: f32) -> f32 {
    xf_flight(f, 0.22)
}

/// [`xf`] with the flights taking `a` of the span each. A longer slot gets a
/// smaller `a`, so its flights last as long as any other word's and the
/// settle takes the difference.
pub fn xf_flight(f: f32, a: f32) -> f32 {
    if f < a {
        -70.0 + 67.0 * (1.0 - (1.0 - f / a).powi(3))
    } else if f < 1.0 - a {
        -3.0 + 6.0 * (f - a) / (1.0 - 2.0 * a)
    } else {
        let p = (f - (1.0 - a)) / a;
        3.0 + 67.0 * p * p * p
    }
}

/// Greeting `index`'s position over its slot `f`.
pub fn word_x(index: usize, f: f32) -> f32 {
    xf_flight(f, FLIGHT_SECS / span(index))
}

/// Animation time for a wall-clock elapsed time. Reduced motion ignores the
/// clock and sits at [`REDUCED_T`].
pub fn anim_time(elapsed_secs: f32, reduced: bool) -> f32 {
    if reduced {
        REDUCED_T
    } else {
        elapsed_secs * SPEED
    }
}

/// True once Space (or a press) is allowed to begin the hand-off.
pub fn is_ready(t: f32) -> bool {
    t >= READY
}

/// The exit fade's opacity, `0..=1`, `elapsed_ms` after the press.
pub fn fade(elapsed_ms: f32) -> f32 {
    cl(elapsed_ms / EXIT_MS)
}

/// One greeting mid-flight.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Greeting {
    pub index: usize,
    /// Offset from screen centre, `cqw`.
    pub x_cqw: f32,
    /// Lean, degrees. Negative leans the top to the right, like italic.
    pub skew_deg: f32,
    pub scale_x: f32,
    /// Gaussian blur radius, px.
    pub blur_px: f32,
    pub opacity: f32,
}

/// The eclipse: a sun with a moon across it, that grows a corona, then flies
/// into the logo's O.
///
/// Its size and place are not stored here. It starts at [`START_CQW`] across
/// at ([`START_X_CQW`], [`START_Y_CQH`]) and lands on the O *as drawn*, which
/// depends on the window (the logo is 50cqw wide, but cqh is not cqw), so
/// `draw` interpolates from the start to the O by [`Eclipse::land`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Eclipse {
    /// How far the shrink into the O has gone, `0..=1`: position, size and the
    /// moon's own size all follow it.
    pub land: f32,
    /// Opacity of the corona and the glow, `0..=1`. The sun and moon are
    /// opaque until they are replaced: fading them as a group would dip the
    /// ring to three quarters of its brightness half-way through the
    /// hand-over, because two same-coloured layers do not sum.
    pub halo: f32,
    /// True once the logo's own O has fully taken over: draw nothing.
    pub merged: bool,
    /// Moon `translateX`, percent of the moon's own width.
    pub moon_pct: f32,
    /// Corona opacity (`c`).
    pub corona: f32,
    /// The sun's box-shadow, in em, plus its alpha.
    pub glow_blur_em: f32,
    pub glow_spread_em: f32,
    pub glow_alpha: f32,
}

/// Where the eclipse starts its shrink: `font-size` in `cqw` (the sun is one
/// em across), and its centre.
pub const START_CQW: f32 = 24.0;
pub const START_X_CQW: f32 = 50.0;
pub const START_Y_CQH: f32 = 44.0;

/// The shrink into the O, animation seconds.
pub const LAND_START: f32 = 6.2;
pub const LAND_SECS: f32 = 2.2;
/// The logo's own O comes in over the landed eclipse between these times
/// (`REVEAL_SECS` long), and only then is the eclipse taken away. The shrink
/// has finished for all practical purposes by [`REVEAL_START`]: the easing
/// leaves under 1% of the travel at 8.1.
pub const REVEAL_START: f32 = 8.1;
pub const REVEAL_SECS: f32 = 0.6;
/// The eclipse's glow starts to give way earlier and more gently, so the
/// brightness falls to the O's own faint halo without a visible step.
pub const HALO_START: f32 = 7.7;
pub const HALO_SECS: f32 = 1.0;

/// The shrink's progress at time `t`: half-cosine twice, so it starts and
/// ends at rest and both position and size are smooth (C1) at both ends.
pub fn land(t: f32) -> f32 {
    sine(sine((t - LAND_START) / LAND_SECS))
}

/// How much of the logo's own O is showing at `t`. It is zero until the
/// eclipse has landed, and it is what the eclipse hands over to.
pub fn o_opacity(t: f32) -> f32 {
    sine((t - REVEAL_START) / REVEAL_SECS)
}

/// Everything on screen at one instant, except the exit fade (which runs on
/// wall-clock time from the press, not on `t`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub t: f32,
    pub eclipse: Eclipse,
    /// The letters of the logo.
    pub logo_opacity: f32,
    /// The logo's O and its glow, which are held back until the eclipse has
    /// landed on the spot.
    pub o_opacity: f32,
    /// `None` once the fly-by is over: the reference sets opacity 0.
    pub greeting: Option<Greeting>,
    pub key_opacity: f32,
    /// The keycap press, `0..=1`. Always 0 under reduced motion.
    pub pulse: f32,
    pub version_opacity: f32,
}

impl Frame {
    pub fn at(t: f32, reduced: bool) -> Frame {
        // Moon crossing + corona.
        let c = eo((t - 5.2) / 1.2);
        let o_opacity = o_opacity(t);
        let eclipse = Eclipse {
            land: land(t),
            halo: 1.0 - sine((t - HALO_START) / HALO_SECS),
            merged: t >= REVEAL_START + REVEAL_SECS,
            moon_pct: lerp(-150.0, 0.0, eio(t / T)),
            corona: c,
            glow_blur_em: 0.08 + 0.5 * c,
            glow_spread_em: 0.01 + 0.08 * c,
            glow_alpha: 0.45 + 0.45 * c,
        };

        // Fly-by greetings.
        let greeting = if t < T {
            let (i, f) = slot(t);
            // Speed in reference units: `cqw` per reference slot, over 300.
            // Measured in time, not in `f`, so a long slot's settle is slow
            // and every word's flight is as fast as the reference's.
            let span = span(i);
            let e = 0.004 * WORD_SPAN / span;
            let av = ((word_x(i, (f + e).min(1.0)) - word_x(i, (f - e).max(0.0))) / (2.0 * e) / 300.0
                * WORD_SPAN
                / span)
                .abs();
            Some(Greeting {
                index: i,
                x_cqw: word_x(i, f),
                skew_deg: -(av * 34.0).min(22.0),
                scale_x: 1.0 + (av * 0.7).min(0.5),
                blur_px: (av * 26.0).min(14.0),
                opacity: cl(1.0 - (av - 0.35).max(0.0) * 1.4),
            })
        } else {
            None
        };

        // Spacebar prompt.
        let k = (t - 8.6).max(0.0) % 1.6;
        let pulse = if reduced {
            0.0
        } else if k < 0.12 {
            k / 0.12
        } else if k < 0.36 {
            1.0
        } else if k < 0.52 {
            1.0 - (k - 0.36) / 0.16
        } else {
            0.0
        };

        Frame {
            t,
            eclipse,
            logo_opacity: sine((t - 7.4) / 1.3),
            o_opacity,
            greeting,
            key_opacity: cl((t - READY) / 0.8),
            pulse,
            version_opacity: eo((t - 8.6) / 1.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn clamped_easings_hit_their_ends() {
        for f in [cl, eo, eio, sine] {
            assert!(close(f(-3.0), 0.0));
            assert!(close(f(0.0), 0.0));
            assert!(close(f(1.0), 1.0));
            assert!(close(f(9.0), 1.0));
        }
        assert!(close(eio(0.5), 0.5));
        assert!(close(sine(0.5), 0.5));
        // Ease-out is ahead of linear, ease-in-out is behind it early on.
        assert!(eo(0.3) > 0.3);
        assert!(eio(0.25) < 0.25);
    }

    #[test]
    fn easings_are_monotone() {
        for f in [eo, eio, sine] {
            let mut last = 0.0;
            for i in 0..=200 {
                let v = f(i as f32 / 200.0);
                assert!(v >= last - 1e-6);
                last = v;
            }
        }
    }

    #[test]
    fn xf_is_continuous_across_its_three_pieces() {
        assert!(close(xf(0.0), -70.0));
        assert!(close(xf(0.22), -3.0));
        assert!(close(xf(0.78), 3.0));
        assert!(close(xf(1.0), 70.0));
        // Continuity at the joins, from both sides.
        assert!((xf(0.22 - 1e-5) - xf(0.22 + 1e-5)).abs() < 0.01);
        assert!((xf(0.78 - 1e-5) - xf(0.78 + 1e-5)).abs() < 0.01);
    }

    #[test]
    fn a_greeting_is_fastest_at_the_ends_and_slow_in_the_middle() {
        let speed = |f: f32| (xf(f + 0.001) - xf(f - 0.001)).abs() / 0.002;
        assert!(speed(0.01) > 10.0 * speed(0.5));
        assert!(speed(0.99) > 10.0 * speed(0.5));
    }

    #[test]
    fn the_greeting_is_solid_and_upright_while_it_lingers() {
        let g = Frame::at(0.3 * WELCOME_SPAN, false).greeting.unwrap();
        assert!(close(g.opacity, 1.0));
        assert!(g.blur_px < 1.0);
        assert!(g.skew_deg > -2.0);
        assert!(g.scale_x < 1.05);
    }

    #[test]
    fn the_greeting_blurs_skews_and_fades_at_speed() {
        // Just after entry: the fastest part of the flight.
        let g = Frame::at(0.002, false).greeting.unwrap();
        assert!(g.blur_px > 5.0);
        assert!(g.skew_deg < -5.0);
        assert!(g.scale_x > 1.1);
        assert!(g.opacity < 1.0);
        // Bounded by the reference's clamps.
        assert!(g.blur_px <= 14.0 && g.skew_deg >= -22.0 && g.scale_x <= 1.5);
    }

    #[test]
    fn ten_words_own_the_first_six_seconds_in_order() {
        let mut start = 0.0;
        for (i, _) in WORDS.iter().enumerate() {
            let mid = start + 0.5 * span(i);
            assert_eq!(Frame::at(mid, false).greeting.unwrap().index, i);
            assert_eq!(slot(mid).0, i);
            assert!(close(slot(mid).1, 0.5));
            start += span(i);
        }
        // The slots tile the fly-by exactly.
        assert!(close(start, T));
        assert_eq!(Frame::at(T - 1e-3, false).greeting.unwrap().index, 9);
        assert!(Frame::at(T, false).greeting.is_none());
        assert!(Frame::at(T + 3.0, false).greeting.is_none());
    }

    #[test]
    fn welcome_holds_two_and_a_half_slots_and_the_rest_share_the_remainder() {
        assert!(close(WELCOME_SPAN, 1.5));
        assert!(close(OTHER_SPAN, 0.5));
        const {
            assert!(WELCOME_SPAN / OTHER_SPAN >= 2.5 && WELCOME_SPAN / OTHER_SPAN <= 3.0);
        }
        // Slot boundaries: English to 1.5 s, then one word every half second.
        assert_eq!(slot(1.499).0, 0);
        assert_eq!(slot(1.5).0, 1);
        assert_eq!(slot(1.999).0, 1);
        assert_eq!(slot(2.0).0, 2);
        assert_eq!(slot(5.999).0, 9);
    }

    #[test]
    fn every_word_flies_in_and_out_in_the_same_time_and_only_welcome_settles_longer() {
        // Fly-in ends where xf reaches -3, after FLIGHT_SECS for everyone.
        for i in 0..WORDS.len() {
            let a = FLIGHT_SECS / span(i);
            assert!(close(word_x(i, 0.0), -70.0));
            assert!(close(word_x(i, a), -3.0));
            assert!(close(word_x(i, 1.0 - a), 3.0));
            assert!(close(word_x(i, 1.0), 70.0));
        }
        let settle = |i: usize| span(i) - 2.0 * FLIGHT_SECS;
        assert!(close(settle(0), 1.28));
        assert!(close(settle(1), 0.28));
        assert!(settle(0) > 4.0 * settle(1));
        // The even words are the reference's own curve.
        assert!(close(word_x(1, 0.5), xf(0.5)));
        assert!(close(word_x(9, 0.1), xf(0.1)));
    }

    #[test]
    fn welcome_is_still_and_crisp_for_most_of_its_slot() {
        // From a quarter of the way in to three quarters: settled.
        for k in 0..=10 {
            let t = WELCOME_SPAN * (0.25 + 0.05 * k as f32);
            let g = Frame::at(t, false).greeting.unwrap();
            assert_eq!(g.index, 0);
            assert!(g.blur_px < 0.75, "blurred at t={t}");
            assert!(close(g.opacity, 1.0));
            assert!(g.x_cqw.abs() <= 3.0 + 1e-3);
        }
    }

    #[test]
    fn the_moon_crosses_from_offscreen_to_centre_by_t6() {
        assert!(close(Frame::at(0.0, false).eclipse.moon_pct, -150.0));
        assert!(close(Frame::at(T, false).eclipse.moon_pct, 0.0));
        assert!(close(Frame::at(12.0, false).eclipse.moon_pct, 0.0));
    }

    #[test]
    fn the_eclipse_shrinks_from_its_start_to_rest_on_the_o() {
        assert!(close(Frame::at(LAND_START, false).eclipse.land, 0.0));
        assert!(close(Frame::at(LAND_START + LAND_SECS, false).eclipse.land, 1.0));
        // Before the shrink it has not moved at all.
        assert!(close(Frame::at(0.0, false).eclipse.land, 0.0));
        // By the time the O starts to appear the eclipse has all but arrived.
        assert!(Frame::at(REVEAL_START, false).eclipse.land > 0.99);
    }

    #[test]
    fn the_shrink_is_monotone_and_starts_and_ends_at_rest() {
        let mut last = 0.0;
        for i in 0..=2200 {
            let t = LAND_START + i as f32 / 1000.0;
            let p = land(t);
            assert!(p >= last - 1e-6, "went backwards at {t}");
            last = p;
        }
        // Zero velocity at both ends: no lurch at the start, no bump at the
        // landing. One millisecond covers next to nothing.
        let v = |t: f32| (land(t + 0.001) - land(t)) / 0.001;
        assert!(v(LAND_START) < 0.001);
        assert!(v(LAND_START + LAND_SECS - 0.001) < 0.001);
        // And it is the fastest in the middle.
        assert!(v(LAND_START + 0.5 * LAND_SECS) > 0.5);
    }

    #[test]
    fn the_logos_o_is_not_there_until_the_eclipse_has_landed() {
        // Not a trace of the O while the eclipse is still travelling.
        let mut t = 0.0;
        while t < REVEAL_START {
            assert_eq!(Frame::at(t, false).o_opacity, 0.0, "O visible at {t}");
            t += 0.01;
        }
        // The letters, on the other hand, are already coming in around the gap.
        assert!(Frame::at(REVEAL_START, false).logo_opacity > 0.5);
        // By the time the O starts to show, the eclipse is on top of it.
        assert!(Frame::at(REVEAL_START, false).eclipse.land > 0.99);
    }

    #[test]
    fn the_o_and_the_eclipse_hand_over_without_a_dip_or_a_double() {
        let start = Frame::at(REVEAL_START, false);
        let end = Frame::at(REVEAL_START + REVEAL_SECS, false);
        assert!(close(start.o_opacity, 0.0) && !start.eclipse.merged);
        assert!(close(end.o_opacity, 1.0) && end.eclipse.merged);
        // The eclipse's body is never translucent: it is there, whole, right
        // up to the frame the O is whole, so the ring never dims.
        for i in 0..=60 {
            let t = REVEAL_START + i as f32 * REVEAL_SECS / 60.0 - 1e-3;
            let f = Frame::at(t, false);
            assert!(!f.eclipse.merged, "eclipse gone early at {t}");
            assert!(f.o_opacity <= 1.0);
        }
        // The glows trade places: by the time the O is whole the eclipse's is
        // out, and it only ever goes down.
        assert!(close(Frame::at(HALO_START, false).eclipse.halo, 1.0));
        assert!(close(end.eclipse.halo, 0.0));
        let mut last = 1.0;
        for i in 0..=100 {
            let h = Frame::at(HALO_START + i as f32 / 100.0, false).eclipse.halo;
            assert!(h <= last + 1e-6);
            last = h;
        }
        // The glow is out before the eclipse is removed, not after.
        const {
            assert!(HALO_START + HALO_SECS <= REVEAL_START + REVEAL_SECS + 1e-4);
        }
    }

    #[test]
    fn the_handover_finishes_before_the_prompt_is_pressable() {
        // READY is unchanged at 8.4; the O is whole by 8.7, the prompt is
        // whole at 9.2, so the merge is over before anything asks for input.
        const {
            assert!(REVEAL_START + REVEAL_SECS <= 8.7 + 1e-4);
        }
        const {
            assert!(REVEAL_START + REVEAL_SECS < 8.4 + 0.4);
        }
        assert!(close(READY, 8.4));
    }

    #[test]
    fn the_letters_fade_in_between_7_4_and_8_7() {
        assert!(close(Frame::at(7.4, false).logo_opacity, 0.0));
        assert!(close(Frame::at(8.7, false).logo_opacity, 1.0));
    }

    #[test]
    fn corona_and_glow_swell_between_5_2_and_6_4() {
        let before = Frame::at(5.2, false).eclipse;
        let after = Frame::at(6.4, false).eclipse;
        assert!(close(before.corona, 0.0) && close(after.corona, 1.0));
        assert!(close(before.glow_blur_em, 0.08) && close(after.glow_blur_em, 0.58));
        assert!(close(before.glow_alpha, 0.45) && close(after.glow_alpha, 0.90));
    }

    #[test]
    fn ready_is_exactly_8_4() {
        assert!(!is_ready(8.399));
        assert!(is_ready(8.4));
        assert!(is_ready(REDUCED_T));
        // The prompt starts to appear at the same instant and is whole 0.8s on.
        assert!(close(Frame::at(8.4, false).key_opacity, 0.0));
        assert!(close(Frame::at(9.2, false).key_opacity, 1.0));
    }

    #[test]
    fn nothing_is_pressable_before_the_prompt_exists() {
        // The keycap is invisible for the whole of the pre-ready timeline, so
        // there is nothing to press.
        for i in 0..840 {
            let t = i as f32 / 100.0;
            assert!(!is_ready(t));
            assert!(close(Frame::at(t, false).key_opacity, 0.0));
        }
    }

    #[test]
    fn the_keycap_pulses_from_8_6_and_repeats_every_1_6s() {
        assert!(close(Frame::at(8.5, false).pulse, 0.0));
        assert!(close(Frame::at(8.6, false).pulse, 0.0));
        assert!(close(Frame::at(8.6 + 0.06, false).pulse, 0.5));
        assert!(close(Frame::at(8.6 + 0.2, false).pulse, 1.0));
        assert!(close(Frame::at(8.6 + 0.44, false).pulse, 0.5));
        assert!(close(Frame::at(8.6 + 1.0, false).pulse, 0.0));
        assert!(close(
            Frame::at(8.6 + 0.2 + 1.6, false).pulse,
            Frame::at(8.6 + 0.2, false).pulse
        ));
    }

    #[test]
    fn reduced_motion_parks_at_t20_with_no_pulse() {
        assert!(close(anim_time(0.0, true), REDUCED_T));
        assert!(close(anim_time(123.0, true), REDUCED_T));
        assert!(is_ready(anim_time(0.0, true)));
        // Sample the whole pulse period: it never moves.
        for i in 0..160 {
            let f = Frame::at(REDUCED_T + i as f32 / 100.0, true);
            assert!(close(f.pulse, 0.0));
        }
        let f = Frame::at(REDUCED_T, true);
        assert!(f.greeting.is_none());
        assert!(close(f.logo_opacity, 1.0));
        assert!(close(f.key_opacity, 1.0));
        assert!(close(f.version_opacity, 1.0));
        assert!(f.eclipse.merged);
        assert!(close(f.o_opacity, 1.0));
        assert!(close(f.eclipse.halo, 0.0));
    }

    #[test]
    fn wall_clock_is_slowed_by_speed() {
        assert!(close(anim_time(10.0, false), 9.0));
        // 8.4 animation seconds is 9.333.. wall-clock seconds.
        assert!(!is_ready(anim_time(9.3, false)));
        assert!(is_ready(anim_time(9.34, false)));
    }

    #[test]
    fn the_exit_fade_takes_700ms() {
        assert!(close(fade(0.0), 0.0));
        assert!(close(fade(350.0), 0.5));
        assert!(close(fade(700.0), 1.0));
        assert!(close(fade(5000.0), 1.0));
    }
}
