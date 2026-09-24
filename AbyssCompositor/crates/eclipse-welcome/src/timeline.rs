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

/// Seconds each greeting owns of the fly-by.
pub const WORD_SPAN: f32 = T / WORDS.len() as f32;

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
/// accelerates out. The piecewise curve is the reference's `xf`.
pub fn xf(f: f32) -> f32 {
    if f < 0.22 {
        -70.0 + 67.0 * (1.0 - (1.0 - f / 0.22).powi(3))
    } else if f < 0.78 {
        -3.0 + 6.0 * (f - 0.22) / 0.56
    } else {
        let p = (f - 0.78) / 0.22;
        3.0 + 67.0 * p * p * p
    }
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

/// The eclipse's box: `font-size` doubles as the em everything inside is
/// measured in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Eclipse {
    /// `font-size` in `cqw`; the disc is one em across.
    pub size_cqw: f32,
    pub x_cqw: f32,
    pub y_cqh: f32,
    /// Group opacity of the whole eclipse.
    pub opacity: f32,
    /// Moon `translateX`, percent of the moon's own width.
    pub moon_pct: f32,
    /// Corona opacity (`c`).
    pub corona: f32,
    /// The sun's box-shadow, in em, plus its alpha.
    pub glow_blur_em: f32,
    pub glow_spread_em: f32,
    pub glow_alpha: f32,
}

/// Everything on screen at one instant, except the exit fade (which runs on
/// wall-clock time from the press, not on `t`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub t: f32,
    pub eclipse: Eclipse,
    pub logo_opacity: f32,
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
        // Shrink into the logo's O.
        let sp = sine(sine((t - 6.2) / 2.2));
        let eclipse = Eclipse {
            size_cqw: lerp(24.0, 6.25, sp),
            x_cqw: lerp(50.0, 65.5, sp),
            y_cqh: lerp(44.0, 50.0, sp),
            opacity: 1.0 - sine((t - 8.2) / 0.9),
            moon_pct: lerp(-150.0, 0.0, eio(t / T)),
            corona: c,
            glow_blur_em: 0.08 + 0.5 * c,
            glow_spread_em: 0.01 + 0.08 * c,
            glow_alpha: 0.45 + 0.45 * c,
        };

        // Fly-by greetings.
        let greeting = if t < T {
            let i = ((t / WORD_SPAN).floor() as usize).min(WORDS.len() - 1);
            let f = (t % WORD_SPAN) / WORD_SPAN;
            let e = 0.004;
            let av = ((xf((f + e).min(1.0)) - xf((f - e).max(0.0))) / (2.0 * e) / 300.0).abs();
            Some(Greeting {
                index: i,
                x_cqw: xf(f),
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
        let g = Frame::at(0.3 * WORD_SPAN, false).greeting.unwrap();
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
        for (i, _) in WORDS.iter().enumerate() {
            let t = (i as f32 + 0.5) * WORD_SPAN;
            assert_eq!(Frame::at(t, false).greeting.unwrap().index, i);
        }
        assert_eq!(Frame::at(T - 1e-3, false).greeting.unwrap().index, 9);
        assert!(Frame::at(T, false).greeting.is_none());
        assert!(Frame::at(T + 3.0, false).greeting.is_none());
    }

    #[test]
    fn the_moon_crosses_from_offscreen_to_centre_by_t6() {
        assert!(close(Frame::at(0.0, false).eclipse.moon_pct, -150.0));
        assert!(close(Frame::at(T, false).eclipse.moon_pct, 0.0));
        assert!(close(Frame::at(12.0, false).eclipse.moon_pct, 0.0));
    }

    #[test]
    fn the_eclipse_shrinks_into_the_o_and_is_gone_by_9_1() {
        let start = Frame::at(6.2, false).eclipse;
        assert!(close(start.size_cqw, 24.0));
        assert!(close(start.x_cqw, 50.0));
        assert!(close(start.y_cqh, 44.0));
        let end = Frame::at(8.4, false).eclipse;
        assert!(close(end.size_cqw, 6.25));
        assert!(close(end.x_cqw, 65.5));
        assert!(close(end.y_cqh, 50.0));
        assert!(close(Frame::at(8.2, false).eclipse.opacity, 1.0));
        assert!(close(Frame::at(9.1, false).eclipse.opacity, 0.0));
    }

    #[test]
    fn the_logo_fades_in_between_7_4_and_8_7() {
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
        assert!(close(f.eclipse.opacity, 0.0));
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
