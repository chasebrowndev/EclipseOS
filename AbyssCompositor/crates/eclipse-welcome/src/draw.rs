// SPDX-License-Identifier: AGPL-3.0-only
//! The canvas: `Frame` (what is on screen) -> pixels.
//!
//! Geometry is in the reference's units: `cqw`/`cqh` are a hundredth of the
//! canvas width/height, and the eclipse's own dimensions are in em (its
//! `font-size`), so every number here can be read against `welcome.html`.
//!
//! **Alpha and gamma.** A browser blends in sRGB; the workspace's iced build
//! (no `web-colors`, by design: it would change every other pane) blends in
//! linear light. On this near-black stage that makes every translucent layer
//! too bright. [`lit`] and [`dim`] remap an alpha so the linear-light result
//! lands where the browser's would: `lit` for light over dark (gold glow,
//! white text, keycap), `dim` for black over anything (shadow, the fade).
//! Fully opaque things are unaffected and use the raw colour.

use crate::fonts::Faces;
use crate::logo::{o_box, Layers, O_HOLE_RATIO};
use crate::palette;
use crate::timeline::{lerp, Eclipse, Frame, Greeting, START_CQW, START_X_CQW, START_Y_CQH, WORDS};
use crate::Message;
use iced::advanced::graphics::text::Paragraph as TextParagraph;
use iced::advanced::text::{Paragraph as _, Shaping, Text as CoreText, Wrapping};
use iced::alignment::Vertical;
use iced::mouse;
use iced::widget::canvas::path::lyon_path::math::Transform;
use iced::widget::canvas::{
    self, fill, gradient, Cache, Fill, Geometry, Image, Path, Program, Stroke, Style, Text,
};
use iced::widget::text::{Alignment, LineHeight};
use iced::{Color, Font, Pixels, Point, Rectangle, Renderer, Size, Theme};

fn to_linear(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// The linear-light alpha `a'` for which `fg` over `bg` (sRGB values, `0..1`)
/// blended in linear light gives what alpha `a` gives blended in sRGB.
fn remap(a: f32, fg: f32, bg: f32) -> f32 {
    let a = a.clamp(0.0, 1.0);
    let want = to_linear(bg + a * (fg - bg));
    let (l_bg, l_fg) = (to_linear(bg), to_linear(fg));
    ((want - l_bg) / (l_fg - l_bg)).clamp(0.0, 1.0)
}

/// Alpha for a light layer (gold, white: about 0.85 in sRGB) over this
/// stage's dark ground (about 0.09).
pub fn lit(a: f32) -> f32 {
    remap(a, 0.85, 0.09)
}

/// Alpha for a white sheen over the near-black ground the keycap sits on
/// (about 0.055): the layer is fainter and the ground darker than `lit`
/// assumes.
pub fn sheen(a: f32) -> f32 {
    remap(a, 1.0, 0.055)
}

/// Alpha for a black layer over a dim ground (0.2: the fade covers the dark
/// stage and the brighter logo alike).
pub fn dim(a: f32) -> f32 {
    remap(a, 0.0, 0.2)
}

/// `erfc`, Abramowitz & Stegun 7.1.26 (|error| < 1.5e-7).
fn erfc(x: f32) -> f32 {
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * z);
    let poly =
        t * (0.254_829_6 + t * (-0.284_496_74 + t * (1.421_413_7 + t * (-1.453_152_1 + t * 1.061_405_4))));
    let e = poly * (-z * z).exp();
    if x >= 0.0 {
        e
    } else {
        2.0 - e
    }
}

/// How much of a Gaussian-blurred edge's shadow lies at signed distance `s`
/// outside the edge (negative inside): 1 well inside, 0.5 on it, 0 far out.
/// A CSS box-shadow's blur radius `B` is a Gaussian of `sigma = B / 2`.
pub fn blurred_edge(s: f32, sigma: f32) -> f32 {
    if sigma <= 1e-6 {
        return if s <= 0.0 { 1.0 } else { 0.0 };
    }
    0.5 * erfc(s / (sigma * std::f32::consts::SQRT_2))
}

/// The corona's `radial-gradient(circle, .32 0%, .1 38%, 0 68%)` over the
/// `farthest-corner` radius of its 2.4em box, as alpha at `r` em from centre.
pub fn corona_profile(r_em: f32) -> f32 {
    let reach = 1.2 * std::f32::consts::SQRT_2;
    let p = r_em / reach;
    if p < 0.38 {
        0.32 + (0.10 - 0.32) * p / 0.38
    } else if p < 0.68 {
        0.10 * (1.0 - (p - 0.38) / 0.30)
    } else {
        0.0
    }
}

/// The stage background at `t` of the way from the centre (0) to the corner
/// ellipse (1): `#1c1915 0%, #0b0a09 60%, #060505 100%`.
pub fn background_at(t: f32) -> Color {
    let mix = |a: Color, b: Color, p: f32| Color {
        r: a.r + (b.r - a.r) * p,
        g: a.g + (b.g - a.g) * p,
        b: a.b + (b.b - a.b) * p,
        a: 1.0,
    };
    if t < 0.6 {
        mix(palette::BG_CENTRE, palette::BG_MID, t / 0.6)
    } else {
        mix(palette::BG_MID, palette::BG_RIM, ((t - 0.6) / 0.4).min(1.0))
    }
}

/// Per-ring alphas that, stacked with "over" from the outermost disc inward,
/// accumulate to `targets[i]` (nondecreasing inward) at ring `i`. Stacking
/// overlapping discs rather than painting disjoint annuli is what keeps
/// anti-aliased seams from showing.
pub fn stack_alphas(targets: &[f32]) -> Vec<f32> {
    let mut prev = 0.0_f32;
    targets
        .iter()
        .map(|&t| {
            let t = t.clamp(0.0, 0.999).max(prev);
            let a = 1.0 - (1.0 - t) / (1.0 - prev);
            prev = t;
            a.clamp(0.0, 1.0)
        })
        .collect()
}

fn solid(c: Color) -> Fill {
    Fill {
        style: Style::Solid(c),
        rule: fill::Rule::NonZero,
    }
}

fn even_odd(c: Color) -> Fill {
    Fill {
        style: Style::Solid(c),
        rule: fill::Rule::EvenOdd,
    }
}

fn scaled(c: Color, a: f32) -> Color {
    palette::with_alpha(c, a)
}

/// One canvas unit set for a given surface size.
#[derive(Clone, Copy)]
struct Units {
    w: f32,
    h: f32,
}

impl Units {
    fn cqw(self) -> f32 {
        self.w / 100.0
    }
    fn cqh(self) -> f32 {
        self.h / 100.0
    }
}

/// The whole screen as an iced canvas program.
pub struct Stage<'a> {
    pub frame: Frame,
    /// The exit fade, `0..=1`.
    pub fade: f32,
    pub label: &'a str,
    pub faces: &'a Faces,
    pub logo: &'a Layers,
    /// Logo height over width.
    pub logo_aspect: f32,
    /// The logo's resampled size in physical pixels, once known.
    pub logo_px: Option<(u32, u32)>,
    /// Physical pixels per logical pixel, for snapping to whole pixels.
    pub pixel_ratio: f32,
    pub background: &'a Cache,
}

impl Program<Message> for Stage<'_> {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let u = Units {
            w: bounds.width,
            h: bounds.height,
        };
        let background = self
            .background
            .draw(renderer, bounds.size(), |f| paint_background(f, u));

        let mut scene = canvas::Frame::new(renderer, bounds.size());
        let rect = logo_rect(u, self.logo_px, self.logo_aspect, self.pixel_ratio);
        paint_eclipse(&mut scene, u, &self.frame.eclipse, rect);
        // The letters, then the O over the eclipse that has landed on it: the
        // same rectangle, so the two are one picture.
        if self.frame.logo_opacity > 0.0 {
            scene.draw_image(
                rect,
                Image::new(self.logo.body.clone()).opacity(lit(self.frame.logo_opacity)),
            );
        }
        if self.frame.o_opacity > 0.0 {
            scene.draw_image(
                rect,
                Image::new(self.logo.o.clone()).opacity(lit(self.frame.o_opacity)),
            );
        }
        if let Some(g) = &self.frame.greeting {
            paint_greeting(&mut scene, u, self.pixel_ratio, self.faces, g);
        }
        paint_keycap(&mut scene, u, self.frame.key_opacity, self.frame.pulse);
        if self.frame.version_opacity > 0.0 {
            paint_version(
                &mut scene,
                u,
                self.pixel_ratio,
                self.label,
                self.faces.label(),
                scaled(palette::MUTED, lit(self.frame.version_opacity)),
            );
        }

        // The fade is its own layer: iced draws a frame's meshes under its
        // images and text, and the fade has to cover all three.
        let mut out = canvas::Frame::new(renderer, bounds.size());
        if self.fade > 0.0 {
            out.fill_rectangle(
                Point::ORIGIN,
                bounds.size(),
                scaled(palette::BLACK, dim(self.fade)),
            );
        }
        vec![background, scene.into_geometry(), out.into_geometry()]
    }

    /// `cursor: none` in the reference.
    fn mouse_interaction(
        &self,
        _state: &(),
        _bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        mouse::Interaction::Hidden
    }
}

/// `radial-gradient(ellipse at 50% 42%, ...)`, `farthest-corner`: the ellipse
/// through the far corner with the aspect of the farthest sides, so radii are
/// `sqrt(2) * (w/2)` by `sqrt(2) * (.58h)`. Painted as stacked opaque
/// ellipses, outermost first; it is cached, so the cost is once per size.
fn paint_background(frame: &mut canvas::Frame, u: Units) {
    const STEPS: usize = 96;
    frame.fill_rectangle(Point::ORIGIN, Size::new(u.w, u.h), palette::BG_RIM);
    let centre = Point::new(u.w * 0.5, u.h * 0.42);
    let rx = u.w * 0.5 * std::f32::consts::SQRT_2;
    let ry = u.h * 0.58 * std::f32::consts::SQRT_2;
    for i in 0..STEPS {
        let outer = 1.0 - i as f32 / STEPS as f32;
        let mid = outer - 0.5 / STEPS as f32;
        frame.with_save(|f| {
            f.translate(iced::Vector::new(centre.x, centre.y));
            f.scale_nonuniform(iced::Vector::new(rx * outer, ry * outer));
            f.fill(&Path::circle(Point::ORIGIN, 1.0), background_at(mid));
        });
    }
}

/// Paint a radial alpha profile as stacked discs, `r_outer` down to `r_inner`.
/// `profile` gives the browser-space alpha at a radius; `hole`, when set,
/// leaves that inner radius unpainted.
#[allow(clippy::too_many_arguments)]
fn radial(
    frame: &mut canvas::Frame,
    centre: Point,
    r_outer: f32,
    r_inner: f32,
    steps: usize,
    rgb: Color,
    hole: Option<f32>,
    profile: impl Fn(f32) -> f32,
) {
    let radius = |i: usize| r_outer - (r_outer - r_inner) * i as f32 / steps as f32;
    let targets: Vec<f32> = (0..steps)
        .map(|i| lit(profile(0.5 * (radius(i) + radius(i + 1)))))
        .collect();
    for (i, a) in stack_alphas(&targets).into_iter().enumerate() {
        if a <= 0.0 {
            continue;
        }
        let c = scaled(rgb, a);
        match hole {
            Some(h) => frame.fill(
                &Path::new(|b| {
                    b.circle(centre, radius(i));
                    b.circle(centre, h);
                }),
                even_odd(c),
            ),
            None => frame.fill(&Path::circle(centre, radius(i)), solid(c)),
        }
    }
}

/// Where the eclipse is at `land` (`0..=1`) on its way to the O of the logo
/// drawn at `rect`: its centre and its diameter (one em).
///
/// The end is the O itself, measured on the asset, not a constant in `cqw`, so
/// the two coincide at any window size and aspect ratio, and on the pixel grid
/// the logo was snapped to.
fn eclipse_box(u: Units, rect: Rectangle, land: f32) -> (Point, f32) {
    let ((ox, oy), od) = o_box(rect.x, rect.y, rect.width, rect.height);
    let em = lerp(START_CQW * u.cqw(), od, land);
    let x = lerp(START_X_CQW * u.cqw(), ox, land);
    let y = lerp(START_Y_CQH * u.cqh(), oy, land);
    (Point::new(x, y), em)
}

/// The moon's radius in em: 0.4 at the start, and by the time the eclipse has
/// landed the size of the O's black disc, so the two are the same picture.
fn moon_radius_em(land: f32) -> f32 {
    lerp(0.4, 0.5 * O_HOLE_RATIO, land)
}

/// Corona, the sun's glow, the sun and the moon.
///
/// The sun and moon are opaque throughout: the logo's own O is faded in over
/// them (see `logo`), and only then is the eclipse dropped. Only the corona
/// and glow are translucent, and they fade by `halo`.
fn paint_eclipse(frame: &mut canvas::Frame, u: Units, e: &Eclipse, logo: Rectangle) {
    if e.merged {
        return;
    }
    let (centre, em) = eclipse_box(u, logo, e.land);

    if e.corona > 0.0 && e.halo > 0.0 {
        radial(frame, centre, 1.154 * em, 0.0, 72, palette::GOLD, None, |r| {
            corona_profile(r / em) * e.corona * e.halo
        });
    }
    // The sun's box-shadow: a blurred disc, painted only outside the sun.
    if e.halo > 0.0 {
        let sigma = e.glow_blur_em / 2.0;
        let reach = 0.5 + e.glow_spread_em;
        radial(
            frame,
            centre,
            (reach + 3.0 * sigma) * em,
            0.5 * em,
            96,
            palette::GOLD,
            None,
            |r| e.glow_alpha * e.halo * blurred_edge(r / em - reach, sigma),
        );
    }

    frame.fill(&Path::circle(centre, 0.5 * em), solid(palette::GOLD));

    let radius = moon_radius_em(e.land) * em;
    let moon = Point::new(centre.x + e.moon_pct / 100.0 * 2.0 * radius, centre.y);
    frame.fill(&Path::circle(moon, radius), solid(palette::BLACK));
}

/// One greeting, as glyph outlines run through the reference's
/// `translateX() skewX() scaleX()` and a blur.
///
/// Outlines (`Text::draw_with`) rather than `fill_text` because the canvas can
/// scale text but not skew it. The CSS blur is approximated by a few
/// alpha-faded offset copies.
fn paint_greeting(frame: &mut canvas::Frame, u: Units, ratio: f32, faces: &Faces, g: &Greeting) {
    if g.opacity <= 0.0 {
        return;
    }
    let (word, script) = WORDS[g.index];
    let size = 5.0 * u.cqw();
    let text = Text {
        content: word.to_owned(),
        position: Point::ORIGIN,
        max_width: f32::INFINITY,
        color: palette::TEXT,
        size: Pixels(size),
        line_height: LineHeight::Relative(1.2),
        font: faces.greeting(script),
        align_x: Alignment::Center,
        align_y: Vertical::Center,
        shaping: Shaping::Advanced,
    };
    // The transform origin is the middle of the (1.2 line-height) element.
    let cx = 50.0 * u.cqw() + g.x_cqw * u.cqw();
    let cy = 72.0 * u.cqh() + 0.6 * size;

    // At rest a word is upright, unblurred and solid: draw it as text, on the
    // glyph atlas, at a whole pixel. That is how every other pane's text is
    // drawn and it is far crisper than a filled outline, which only gets the
    // canvas's 4x multisampling. Outlines are for words that are moving.
    if is_crisp(g) {
        frame.fill_text(Text {
            position: Point::new(snap(cx, ratio), snap(cy, ratio)),
            ..text
        });
        return;
    }

    let mut glyphs = Vec::new();
    text.draw_with(|path, _| glyphs.push(path));
    let skew = (-g.skew_deg).to_radians().tan();

    for (dx, dy, weight) in blur_copies(g.blur_px) {
        // Soft copies overlap; their weights sum to one, so the sum stays
        // near the opacity instead of above it.
        let colour = scaled(palette::TEXT, lit(g.opacity) * weight);
        let m = Transform::new(g.scale_x, 0.0, skew, 1.0, cx + dx, cy + dy);
        for glyph in &glyphs {
            frame.fill(&glyph.transform(&m), solid(colour));
        }
    }
}

/// Whether a greeting is at rest: upright, unblurred and solid. The reference
/// blurs by `av * 26` px, so a word gliding at a tenth of the fastest speed is
/// still blurred by a couple of pixels there; that much is not worth softening
/// text over, so it is drawn sharp.
pub fn is_crisp(g: &Greeting) -> bool {
    g.blur_px < CRISP_BLUR && g.skew_deg > -CRISP_SKEW && g.scale_x < 1.005 && g.opacity >= 1.0
}

/// Below this blur radius (px) a word is drawn sharp.
const CRISP_BLUR: f32 = 1.0;
const CRISP_SKEW: f32 = 0.5;

/// Offsets and weights of the copies approximating `filter: blur(sigma)`:
/// the centre plus three rings out to two sigma, weighted like a Gaussian and
/// normalised to sum to one. Up to [`CRISP_BLUR`] the word is a single full
/// copy, and past it the blur starts from zero rather than jumping to the
/// full radius, so a word never pops from sharp to soft.
pub fn blur_copies(sigma: f32) -> Vec<(f32, f32, f32)> {
    let sigma = (sigma - CRISP_BLUR).max(0.0);
    if sigma < 0.05 {
        return vec![(0.0, 0.0, 1.0)];
    }
    const RINGS: [(f32, usize); 3] = [(0.7, 6), (1.4, 12), (2.1, 18)];
    let mut out = vec![(0.0, 0.0, 1.0)];
    for (k, n) in RINGS {
        let w = (-0.5 * k * k).exp();
        for i in 0..n {
            let a = std::f32::consts::TAU * (i as f32 + 0.37 * k) / n as f32;
            out.push((sigma * k * a.cos(), sigma * k * a.sin(), w));
        }
    }
    let total: f32 = out.iter().map(|c| c.2).sum();
    for c in &mut out {
        c.2 /= total;
    }
    out
}

/// Where the logo goes: 50cqw wide, centred. Once its resampled size is known
/// the rectangle is placed on whole physical pixels and is exactly the texture's
/// size, so nothing is stretched or shifted by a fraction of a pixel.
fn logo_rect(u: Units, px: Option<(u32, u32)>, aspect: f32, ratio: f32) -> Rectangle {
    let Some((w, h)) = px else {
        let w = 50.0 * u.cqw();
        let h = w * aspect;
        return Rectangle::new(
            Point::new(50.0 * u.cqw() - w / 2.0, 50.0 * u.cqh() - h / 2.0),
            Size::new(w, h),
        );
    };
    let (w, h) = (w as f32, h as f32);
    let x = (50.0 * u.cqw() * ratio - w / 2.0).round();
    let y = (50.0 * u.cqh() * ratio - h / 2.0).round();
    Rectangle::new(Point::new(x / ratio, y / ratio), Size::new(w / ratio, h / ratio))
}

/// `v` moved to the nearest whole physical pixel.
fn snap(v: f32, ratio: f32) -> f32 {
    (v * ratio).round() / ratio
}

/// The version label, `letter-spacing: .06em`, right-aligned at 97cqw/97cqh.
///
/// The canvas has no letter-spacing, so each character is placed by hand at
/// its own measured advance (digits tabular). CSS spaces after every character, the last
/// included, so the run ends one gap short of the anchor.
fn paint_version(frame: &mut canvas::Frame, u: Units, ratio: f32, label: &str, font: Font, color: Color) {
    let size = u.cqw();
    let gap = 0.06 * size;
    let measure = |c: char| -> f32 {
        let mut buf = [0u8; 4];
        TextParagraph::with_text(CoreText {
            content: &*c.encode_utf8(&mut buf),
            bounds: Size::INFINITE,
            size: Pixels(size),
            line_height: LineHeight::default(),
            font,
            align_x: Alignment::Left,
            align_y: Vertical::Top,
            shaping: Shaping::Advanced,
            wrapping: Wrapping::None,
        })
        .min_bounds()
        .width
    };
    // `font-variant-numeric: tabular-nums`: iced cannot switch on the font's
    // `tnum` feature, so give digits and the point one cell with the glyph
    // centred in it. Half an em is what Lora's tabular set measures to in
    // the reference render.
    let cell = 0.5 * size;
    let tabular = |c: char| c.is_ascii_digit() || c == '.';
    let advances: Vec<f32> = label
        .chars()
        .map(|c| if tabular(c) { cell } else { measure(c) } + gap)
        .collect();
    let mut x = 97.0 * u.cqw() - advances.iter().sum::<f32>();
    for (c, advance) in label.chars().zip(advances) {
        let centred = if tabular(c) {
            (cell - measure(c)) / 2.0
        } else {
            0.0
        };
        frame.fill_text(Text {
            content: c.to_string(),
            position: Point::new(snap(x + centred, ratio), snap(97.0 * u.cqh(), ratio)),
            max_width: f32::INFINITY,
            color,
            size: Pixels(size),
            line_height: LineHeight::default(),
            font,
            align_x: Alignment::Left,
            align_y: Vertical::Bottom,
            shaping: Shaping::Advanced,
        });
        x += advance;
    }
}

fn rrect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Path {
    Path::rounded_rectangle(
        Point::new(x, y),
        Size::new(w.max(0.0), h.max(0.0)),
        r.max(0.0).into(),
    )
}

/// The spacebar: a translucent keycap with a hard lower edge, a soft drop
/// shadow and, while pressed, a gold glow. `d` is the press, `0..=1`.
fn paint_keycap(frame: &mut canvas::Frame, u: Units, opacity: f32, d: f32) {
    if opacity <= 0.0 {
        return;
    }
    let cqw = u.cqw();
    let (w, h, r) = (20.0 * cqw, 4.6 * cqw, 0.9 * cqw);
    let x = 50.0 * u.cqw() - w / 2.0;
    let y = 78.0 * u.cqh() + d * 0.35 * cqw;
    let bottom = y + h;
    let below = Rectangle::new(Point::new(0.0, bottom), Size::new(u.w, 50.0 * cqw));

    // `0 1.2cqw 2cqw rgba(0,0,0,.5)`: a soft drop shadow. Only the part below
    // the key is drawn; the sides are black on near-black.
    let drop = (1.2 - d * 0.8) * cqw;
    let sigma = (2.0 - d) * cqw / 2.0;
    let steps = 14;
    let s_at = |i: usize| 2.0 * sigma - 4.0 * sigma * i as f32 / steps as f32;
    frame.with_clip(below, |f| {
        let alphas = stack_alphas(
            &(0..steps)
                .map(|i| {
                    let s = 0.5 * (s_at(i) + s_at(i + 1));
                    dim(0.5 * opacity * blurred_edge(s, sigma))
                })
                .collect::<Vec<_>>(),
        );
        for (i, a) in alphas.into_iter().enumerate() {
            let s = s_at(i);
            f.fill(
                &rrect(x - s, y + drop - s, w + 2.0 * s, h + 2.0 * s, r + s),
                solid(scaled(palette::BLACK, a)),
            );
        }
    });

    // (Painted after the soft shadow: the first CSS shadow is the top one.)
    // `0 .4cqw 0 rgba(255,255,255,.10)`: the key's lower edge. A box-shadow
    // is never painted under its own box: hole it with the key (even-odd) and
    // keep the lower half, where the key lies wholly inside the shadow.
    let lift = (0.4 - d * 0.32) * cqw;
    let lower = Rectangle::new(Point::new(0.0, y + h / 2.0), Size::new(u.w, 50.0 * cqw));
    frame.with_clip(lower, |f| {
        f.fill(
            &Path::new(|b| {
                b.rounded_rectangle(Point::new(x, y + lift), Size::new(w, h), r.into());
                b.rounded_rectangle(Point::new(x, y), Size::new(w, h), r.into());
            }),
            even_odd(scaled(palette::WHITE, sheen(0.10 * opacity))),
        );
    });

    // `0 0 2.4cqw*d rgba(244,187,60,.35d)`: the glow, all round, holed by the
    // key so it never shows through the translucent fill.
    if d > 0.005 {
        let sigma = d * 2.4 * cqw / 2.0;
        let steps = 20;
        let s_at = |i: usize| 3.0 * sigma * (1.0 - i as f32 / steps as f32);
        let targets: Vec<f32> = (0..steps)
            .map(|i| {
                let s = 0.5 * (s_at(i) + s_at(i + 1));
                lit(0.35 * d * opacity * blurred_edge(s, sigma))
            })
            .collect();
        for (i, a) in stack_alphas(&targets).into_iter().enumerate() {
            let s = s_at(i);
            frame.fill(
                &Path::new(|b| {
                    b.rounded_rectangle(
                        Point::new(x - s, y - s),
                        Size::new(w + 2.0 * s, h + 2.0 * s),
                        (r + s).into(),
                    );
                    b.rounded_rectangle(Point::new(x, y), Size::new(w, h), r.into());
                }),
                even_odd(scaled(palette::GOLD, a)),
            );
        }
    }

    // The keycap itself: `linear-gradient(180deg, white .10, white .04)`.
    let top = scaled(palette::WHITE, sheen(0.10 * opacity));
    let bottom_c = scaled(palette::WHITE, sheen(0.04 * opacity));
    let ground = gradient::Linear::new(Point::new(x, y), Point::new(x, y + h))
        .add_stop(0.0, top)
        .add_stop(1.0, bottom_c);
    frame.fill(&rrect(x, y, w, h, r), Fill::from(ground));
    // `border: 1px solid rgba(244,187,60, .18 + d*.5)`, inside the box.
    let edge = scaled(palette::GOLD, lit((0.18 + d * 0.5) * opacity));
    frame.stroke(
        &rrect(x + 0.5, y + 0.5, w - 1.0, h - 1.0, r - 0.5),
        Stroke::default().with_width(1.0).with_color(edge),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_remaps_keep_the_ends_and_darken_the_middle() {
        for f in [lit, dim] {
            assert!(f(0.0).abs() < 1e-6);
            assert!((f(1.0) - 1.0).abs() < 1e-6);
        }
        // Light over dark: linear-light blending needs *less* alpha.
        assert!(lit(0.5) < 0.5);
        // Black over anything needs *more*.
        assert!(dim(0.5) > 0.5);
    }

    #[test]
    fn a_blurred_edge_is_half_on_the_edge_and_falls_off_outward() {
        assert!((blurred_edge(0.0, 3.0) - 0.5).abs() < 1e-4);
        assert!(blurred_edge(-20.0, 3.0) > 0.999);
        assert!(blurred_edge(20.0, 3.0) < 0.001);
        assert!(blurred_edge(1.0, 3.0) > blurred_edge(2.0, 3.0));
        assert_eq!(blurred_edge(1.0, 0.0), 0.0);
        assert_eq!(blurred_edge(-1.0, 0.0), 1.0);
    }

    #[test]
    fn the_corona_profile_matches_its_gradient_stops() {
        let reach = 1.2 * std::f32::consts::SQRT_2;
        assert!((corona_profile(0.0) - 0.32).abs() < 1e-6);
        assert!((corona_profile(0.38 * reach) - 0.10).abs() < 1e-4);
        assert!(corona_profile(0.68 * reach + 1e-3) == 0.0);
        assert!(corona_profile(1.2) < 0.01);
    }

    #[test]
    fn the_background_runs_from_the_centre_stop_to_the_rim() {
        let near = |a: Color, b: Color| (a.r - b.r).abs() + (a.g - b.g).abs() + (a.b - b.b).abs() < 1e-5;
        assert!(near(background_at(0.0), palette::BG_CENTRE));
        assert!(near(background_at(0.6), palette::BG_MID));
        assert!(near(background_at(1.0), palette::BG_RIM));
    }

    #[test]
    fn stacked_rings_accumulate_to_their_targets() {
        let targets = [0.1, 0.25, 0.4, 0.4, 0.7];
        let mut acc = 0.0_f32;
        for (a, t) in stack_alphas(&targets).into_iter().zip(targets) {
            acc = acc + a * (1.0 - acc);
            assert!((acc - t).abs() < 1e-5, "{acc} vs {t}");
        }
    }

    #[test]
    fn an_unblurred_word_is_one_copy_and_a_blurred_one_is_a_cluster() {
        assert_eq!(blur_copies(0.0).len(), 1);
        assert_eq!(blur_copies(14.0).len(), 37);
        let sum: f32 = blur_copies(14.0).iter().map(|c| c.2).sum();
        assert!((sum - 1.0).abs() < 1e-4);
    }

    #[test]
    fn a_slow_word_is_sharp_and_blur_starts_from_zero_past_the_dead_zone() {
        assert_eq!(blur_copies(CRISP_BLUR).len(), 1);
        assert_eq!(blur_copies(0.4).len(), 1);
        // Just past it the cluster is nearly the centre: no jump to full radius.
        let near = blur_copies(CRISP_BLUR + 0.2);
        assert_eq!(near.len(), 37);
        let reach = near.iter().map(|c| c.0.hypot(c.1)).fold(0.0, f32::max);
        assert!(reach < 0.6, "{reach}");
    }

    fn greeting(blur: f32, skew: f32, scale: f32, opacity: f32) -> Greeting {
        Greeting {
            index: 0,
            x_cqw: 0.0,
            skew_deg: skew,
            blur_px: blur,
            scale_x: scale,
            opacity,
        }
    }

    #[test]
    fn only_a_settled_word_is_drawn_as_crisp_text() {
        assert!(is_crisp(&greeting(0.2, -0.1, 1.001, 1.0)));
        assert!(!is_crisp(&greeting(3.0, -0.1, 1.001, 1.0)));
        assert!(!is_crisp(&greeting(0.2, -6.0, 1.001, 1.0)));
        assert!(!is_crisp(&greeting(0.2, -0.1, 1.2, 1.0)));
        assert!(!is_crisp(&greeting(0.2, -0.1, 1.001, 0.6)));
    }

    #[test]
    fn the_eclipse_lands_exactly_on_the_logos_o_at_any_window_shape() {
        for (w, h) in [(1920.0, 1080.0), (2880.0, 1920.0), (1000.0, 1000.0)] {
            let u = Units { w, h };
            let rect = logo_rect(
                u,
                Some(((0.5 * w) as u32, ((0.5 * w) * 144.0 / 796.0) as u32)),
                144.0 / 796.0,
                1.0,
            );
            let ((ox, oy), od) = o_box(rect.x, rect.y, rect.width, rect.height);
            let (c, em) = eclipse_box(u, rect, 1.0);
            assert!((c.x - ox).abs() < 1e-3 && (c.y - oy).abs() < 1e-3 && (em - od).abs() < 1e-3);
            // And it starts where the reference put it.
            let (c, em) = eclipse_box(u, rect, 0.0);
            assert!((c.x - 0.5 * w).abs() < 1e-2 && (c.y - 0.44 * h).abs() < 1e-2);
            assert!((em - 0.24 * w).abs() < 1e-2);
        }
    }

    #[test]
    fn the_moon_ends_the_size_of_the_o_s_black_disc() {
        assert!((moon_radius_em(0.0) - 0.4).abs() < 1e-6);
        assert!((moon_radius_em(1.0) - 0.5 * O_HOLE_RATIO).abs() < 1e-6);
    }
}
