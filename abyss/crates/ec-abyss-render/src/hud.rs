// SPDX-License-Identifier: AGPL-3.0-only
//! The annotation HUD's artwork, drawn on the CPU (ADR 0071).
//!
//! Everything the annotation pass and the region selector put on screen that
//! is not a solid quad, a shader or the blurred backdrop is painted here into
//! a premultiplied RGBA [`Raster`] once, cached by the caller and uploaded as
//! a texture. Nothing here runs per frame.
//!
//! Geometry is laid out in logical pixels and painted at a whole raster scale
//! `dev` (1 or 2, [`hud_font::raster_scale`]); every shape is a signed
//! distance field with one device pixel of antialiasing, the same edge the
//! rounded-window shader draws, so CPU and GPU curves match.
//!
//! The text drawn here is the caller's, already reduced by `text::sanitize`
//! at the door; the colours are all [`HudPalette`]'s and none is the
//! caller's.

use crate::hud_font::{Face, ADVANCE};
use crate::palette::HudPalette;
use crate::text::{wrap, Raster, Rgba};

// ------------------------------------------------------------------ canvas

/// A float canvas of premultiplied pixels, device-sized.
struct Paint {
    w: usize,
    h: usize,
    px: Vec<[f32; 4]>,
}

/// Signed distance from `(x, y)` to the rounded box at `(bx, by)` sized
/// `bw`x`bh` with corner radius `r`. Negative inside.
fn sd_box(x: f32, y: f32, (bx, by, bw, bh): (f32, f32, f32, f32), r: f32) -> f32 {
    let (hw, hh) = (bw * 0.5, bh * 0.5);
    let r = r.min(hw).min(hh).max(0.0);
    let qx = (x - bx - hw).abs() - (hw - r);
    let qy = (y - by - hh).abs() - (hh - r);
    let (ox, oy) = (qx.max(0.0), qy.max(0.0));
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
}

/// One device pixel of antialiasing across the edge.
fn cov(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}

/// Coverage of the band `lo <= d <= hi` of a distance field: a stroke.
fn band(d: f32, lo: f32, hi: f32) -> f32 {
    (cov(d - hi) - cov(d - lo)).max(0.0)
}

impl Paint {
    fn new(w: usize, h: usize) -> Paint {
        Paint {
            w,
            h,
            px: vec![[0.0; 4]; w * h],
        }
    }

    /// Source-over one pixel of straight `c` at coverage `k`.
    fn blend(&mut self, x: usize, y: usize, c: Rgba, k: f32) {
        let a = c[3] * k.clamp(0.0, 1.0);
        if a <= 0.0 || x >= self.w || y >= self.h {
            return;
        }
        let d = &mut self.px[y * self.w + x];
        for i in 0..3 {
            d[i] = c[i] * a + d[i] * (1.0 - a);
        }
        d[3] = a + d[3] * (1.0 - a);
    }

    /// Blend `c` over every pixel of the device box `(x, y, w, h)` (clipped
    /// to the canvas) at the coverage `f` gives the pixel's centre.
    fn shade(&mut self, (x, y, w, h): (f32, f32, f32, f32), c: Rgba, f: impl Fn(f32, f32) -> f32) {
        let x0 = x.floor().max(0.0) as usize;
        let y0 = y.floor().max(0.0) as usize;
        let x1 = ((x + w).ceil().max(0.0) as usize).min(self.w);
        let y1 = ((y + h).ceil().max(0.0) as usize).min(self.h);
        for py in y0..y1 {
            for px in x0..x1 {
                let k = f(px as f32 + 0.5, py as f32 + 0.5);
                self.blend(px, py, c, k);
            }
        }
    }

    fn fill(&mut self, b: (f32, f32, f32, f32), r: f32, c: Rgba) {
        self.shade(b, c, |x, y| cov(sd_box(x, y, b, r)));
    }

    /// A stroke `width` device px wide lying just inside `b`'s edge.
    fn rim(&mut self, b: (f32, f32, f32, f32), r: f32, width: f32, c: Rgba) {
        self.shade(b, c, |x, y| band(sd_box(x, y, b, r), -width, 0.0));
    }

    /// One row of `s` in `face`. `x` and `top` are logical; the line box's
    /// top is `top`, and every column is [`ADVANCE`]-wide logical whatever
    /// atlas `dev` picks, so text lines up with layout at every scale.
    fn text(&mut self, face: Face, dev: usize, x: usize, top: usize, s: &str, c: Rgba) {
        let a = face.atlas(dev);
        let col = face.advance() * dev;
        // Centre a 2x advance that rounded off its 1x double, and put its
        // baseline where the 1x one lands.
        let dx = (col.saturating_sub(a.advance)) / 2;
        let base = face.ascent() * dev;
        for (i, ch) in s.chars().enumerate() {
            let pen = x * dev + i * col + dx;
            let (Some(cx), Some(cy)) = (
                pen.checked_sub(a.margin),
                (top * dev + base).checked_sub(a.ascent),
            ) else {
                continue;
            };
            let cell = a.cell(ch);
            for gy in 0..a.cell_h {
                for gx in 0..a.cell_w {
                    let k = cell[gy * a.cell_w + gx];
                    if k != 0 {
                        self.blend(cx + gx, cy + gy, c, k as f32 / 255.0);
                    }
                }
            }
        }
    }

    fn into_raster(self) -> Raster {
        let mut px = Vec::with_capacity(self.w * self.h * 4);
        for p in &self.px {
            for v in p {
                px.push((v.clamp(0.0, 1.0) * 255.0).round() as u8);
            }
        }
        Raster {
            w: self.w as i32,
            h: self.h as i32,
            px,
        }
    }
}

/// A logical box at device scale.
fn dbox(x: f32, y: f32, w: f32, h: f32, dev: usize) -> (f32, f32, f32, f32) {
    let d = dev as f32;
    (x * d, y * d, w * d, h * d)
}

// -------------------------------------------------------------- the panel

/// Panel padding, logical px (spec item 1).
pub const PAD_X: usize = 14;
pub const PAD_T: usize = 12;
pub const PAD_B: usize = 12;
/// Leading between lines of one block.
pub const LINE_GAP: usize = 3;
/// Space either side of the hairline between title and body.
const RULE_GAP: usize = 8;
/// Panel corner radius.
pub const RADIUS: f32 = 13.0;
/// The error kind's dot, and the space after it.
const DOT: usize = 6;
const DOT_GAP: usize = 8;
/// Space after the in-panel pick letter.
const CHIP_GAP: usize = 6;
/// Title lines kept after wrapping. A headline that needs more is not one.
const MAX_TITLE_LINES: usize = 3;
/// Narrowest panel, so a one-word answer still reads as a panel.
const MIN_W: usize = 96;

/// The panel's drop shadow, `0 12px 32px -12px` (spec item 1): drawn into
/// the panel's own raster, so the raster overhangs the panel by this much.
pub const SHADOW_X: usize = 20;
pub const SHADOW_TOP: usize = 8;
pub const SHADOW_BOTTOM: usize = 32;
const SHADOW_Y: f32 = 12.0;
const SHADOW_SPREAD: f32 = -12.0;
const SHADOW_BLUR: f32 = 32.0;

/// What a panel says, wrapped and ready to draw. Everything in it has
/// already been through `text::sanitize`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    /// The pick's letter, drawn before the title in the accent text colour.
    /// Empty for none.
    pub chip: String,
    pub title: Vec<String>,
    pub body: Vec<String>,
    /// An error annotation: a danger dot before the title, and no chip.
    pub error: bool,
    /// The narrowest the panel may be, logical px, whatever its text.
    pub min_w: usize,
}

impl Card {
    /// Wrap `title` and `body` at `cols` columns. Title lines hang after the
    /// dot and chip, so they wrap that much narrower.
    pub fn new(chip: &str, title: &str, body: &str, cols: usize, error: bool) -> Card {
        let chip = if error { "" } else { chip };
        let prefix = prefix_w(chip, error).div_ceil(ADVANCE);
        let mut title = wrap(title, cols.saturating_sub(prefix).max(1));
        title.truncate(MAX_TITLE_LINES);
        Card {
            chip: chip.to_string(),
            title,
            body: wrap(body, cols),
            error,
            min_w: MIN_W,
        }
    }

    /// The same card, never narrower than `w` logical px. A panel stacked
    /// under a wide region keeps a readable measure even for a short answer.
    pub fn at_least(mut self, w: usize) -> Card {
        self.min_w = self.min_w.max(w);
        self
    }

    pub fn is_empty(&self) -> bool {
        !self.has_head() && self.body.is_empty()
    }

    fn has_head(&self) -> bool {
        self.error || !self.chip.is_empty() || !self.title.is_empty()
    }

    fn head_h(&self) -> usize {
        if self.has_head() {
            block(self.title.len().max(1), Face::Title)
        } else {
            0
        }
    }

    fn rule(&self) -> bool {
        self.has_head() && !self.body.is_empty()
    }

    /// Top of the body block, logical, from the panel's top.
    fn body_top(&self) -> usize {
        PAD_T + self.head_h() + if self.rule() { RULE_GAP * 2 + 1 } else { 0 }
    }

    /// Logical size of the panel (not of its raster, which adds the shadow).
    pub fn size(&self) -> (usize, usize) {
        if self.is_empty() {
            return (0, 0);
        }
        let widest = |l: &[String]| l.iter().map(|s| s.len()).max().unwrap_or(0) * ADVANCE;
        let head = if self.has_head() {
            prefix_w(&self.chip, self.error) + widest(&self.title)
        } else {
            0
        };
        let w = PAD_X * 2 + head.max(widest(&self.body));
        let h = self.body_top() + block(self.body.len(), Face::Body) + PAD_B;
        (w.max(self.min_w), h)
    }
}

/// Logical width of the dot and chip before the title.
fn prefix_w(chip: &str, error: bool) -> usize {
    let dot = if error { DOT + DOT_GAP } else { 0 };
    let chip = if chip.is_empty() {
        0
    } else {
        Face::Title.width(chip) + CHIP_GAP
    };
    dot + chip
}

/// Height of `rows` lines of `face`.
fn block(rows: usize, face: Face) -> usize {
    if rows == 0 {
        0
    } else {
        rows * face.line_h() + (rows - 1) * LINE_GAP
    }
}

/// Middle of the panel's first title line, logical, from the panel's top:
/// what placement lines up with a pick.
pub const fn first_line_mid() -> i32 {
    PAD_T as i32 + 9
}

/// Split a trailing parenthetical -- `"... (low confidence)"` -- off the
/// last line of a block, so it can be drawn as meta text. `None` when the
/// line has none, or is nothing else.
fn meta_split(line: &str) -> Option<usize> {
    if !line.ends_with(')') {
        return None;
    }
    let at = line.rfind(" (")? + 1;
    Some(at)
}

/// Draw one block line, its trailing parenthetical (on the block's last
/// line only) in `meta`.
#[allow(clippy::too_many_arguments)]
fn line(
    p: &mut Paint,
    face: Face,
    dev: usize,
    x: usize,
    top: usize,
    s: &str,
    last: bool,
    c: Rgba,
    meta: Rgba,
) {
    match meta_split(s).filter(|_| last) {
        Some(at) => {
            p.text(face, dev, x, top, &s[..at], c);
            p.text(face, dev, x + at * face.advance(), top, &s[at..], meta);
        }
        None => p.text(face, dev, x, top, s, c),
    }
}

/// The two Gaussian-box factors of the shadow at one coordinate: the
/// separable closed form of a blurred box (exact for square corners, which a
/// 16px sigma makes indistinguishable from rounded ones).
fn erf(x: f32) -> f32 {
    // Abramowitz & Stegun 7.1.26; |error| < 1.5e-7, far below 8-bit.
    let s = x.signum();
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let y = 1.0
        - (((((1.061_405_4 * t - 1.453_152) * t) + 1.421_413_8) * t - 0.284_496_73) * t + 0.254_829_6)
            * t
            * (-x * x).exp();
    s * y
}

fn gauss_box(v: f32, lo: f32, hi: f32, sigma: f32) -> f32 {
    let k = 1.0 / (sigma * std::f32::consts::SQRT_2);
    0.5 * (erf((v - lo) * k) - erf((v - hi) * k))
}

/// Rasterise a panel at raster scale `dev`: the shadow, the fill, the rim,
/// the hairline and the text. `glass` says a blurred backdrop is drawn
/// behind it (so the fill is the translucent tint); otherwise the panel is
/// the near-opaque fallback with a top highlight.
///
/// The raster is `(card.size() + shadow margins) * dev`; the panel sits at
/// `(SHADOW_X, SHADOW_TOP)` inside it.
pub fn panel(card: &Card, pal: &HudPalette, glass: bool, dev: usize) -> Raster {
    let dev = dev.max(1);
    let (w, h) = card.size();
    let (rw, rh) = (w + SHADOW_X * 2, h + SHADOW_TOP + SHADOW_BOTTOM);
    let mut p = Paint::new(rw * dev, rh * dev);
    if w == 0 {
        return p.into_raster();
    }
    let d = dev as f32;
    let body = dbox(SHADOW_X as f32, SHADOW_TOP as f32, w as f32, h as f32, dev);
    let r = RADIUS * d;

    // Shadow: only outside the panel, so it never darkens the glass.
    let (sx, sy) = (
        body.0 - SHADOW_SPREAD * d,
        body.1 - SHADOW_SPREAD * d + SHADOW_Y * d,
    );
    let (sr, sb) = (
        body.0 + body.2 + SHADOW_SPREAD * d,
        body.1 + body.3 + SHADOW_SPREAD * d + SHADOW_Y * d,
    );
    let sigma = SHADOW_BLUR * 0.5 * d;
    let full = (0.0, 0.0, p.w as f32, p.h as f32);
    p.shade(full, pal.shadow, |x, y| {
        let outside = 1.0 - cov(sd_box(x, y, body, r));
        gauss_box(x, sx, sr, sigma) * gauss_box(y, sy, sb, sigma) * outside
    });

    if glass {
        p.fill(body, r, pal.panel_tint);
    } else {
        p.fill(body, r, pal.fallback_fill);
        // A 1px highlight along the top edge, fading out down the corners.
        let top = body.1;
        p.shade(body, pal.fallback_highlight, |x, y| {
            let fade = (1.0 - (y - top) / r).clamp(0.0, 1.0);
            band(sd_box(x, y, body, r), -d, 0.0) * fade
        });
    }
    p.rim(body, r, d, pal.panel_rim);

    let (ox, oy) = (SHADOW_X, SHADOW_TOP);
    if card.rule() {
        let y = oy + PAD_T + card.head_h() + RULE_GAP;
        p.fill(
            dbox((ox + PAD_X) as f32, y as f32, (w - PAD_X * 2) as f32, 1.0, dev),
            0.0,
            pal.hairline,
        );
    }
    if card.has_head() {
        let top = oy + PAD_T;
        let mut x = ox + PAD_X;
        if card.error {
            // Centred on the capitals: JetBrains Mono's cap height is .73 em.
            let cy = top as f32 + Face::Title.ascent() as f32 - 4.75;
            let dot = dbox(x as f32, cy - DOT as f32 * 0.5, DOT as f32, DOT as f32, dev);
            p.fill(dot, DOT as f32 * 0.5 * d, pal.danger);
            x += DOT + DOT_GAP;
        }
        if !card.chip.is_empty() {
            p.text(Face::Title, dev, x, top, &card.chip, pal.accent_text);
            x += Face::Title.width(&card.chip) + CHIP_GAP;
        }
        let pitch = Face::Title.line_h() + LINE_GAP;
        let n = card.title.len();
        for (i, s) in card.title.iter().enumerate() {
            line(
                &mut p,
                Face::Title,
                dev,
                x,
                top + i * pitch,
                s,
                i + 1 == n,
                pal.text,
                pal.text_tertiary,
            );
        }
    }
    let top = oy + card.body_top();
    let pitch = Face::Body.line_h() + LINE_GAP;
    let n = card.body.len();
    for (i, s) in card.body.iter().enumerate() {
        line(
            &mut p,
            Face::Body,
            dev,
            ox + PAD_X,
            top + i * pitch,
            s,
            i + 1 == n,
            pal.text,
            pal.text_tertiary,
        );
    }
    p.into_raster()
}

// --------------------------------------------------------------- the rest

/// A pill: `text` in `face` on a fully rounded `fill`, with an optional
/// inside `rim` and a 1px outside `keyline` (so it reads on a white page).
/// Returns the raster, which overhangs the pill by [`PILL_MARGIN`] on every
/// side, and the pill's logical size.
#[allow(clippy::too_many_arguments)]
pub fn pill(
    text: &str,
    face: Face,
    (pad_y, pad_x): (usize, usize),
    fill: Rgba,
    rim: Option<Rgba>,
    keyline: Rgba,
    ink: Rgba,
    dev: usize,
) -> (Raster, (usize, usize)) {
    let dev = dev.max(1);
    let (w, h) = pill_size(text, face, (pad_y, pad_x));
    let m = PILL_MARGIN;
    let mut p = Paint::new((w + m * 2) * dev, (h + m * 2) * dev);
    let d = dev as f32;
    let b = dbox(m as f32, m as f32, w as f32, h as f32, dev);
    let r = b.3 * 0.5;
    let outer = (b.0 - d, b.1 - d, b.2 + 2.0 * d, b.3 + 2.0 * d);
    p.shade(outer, keyline, |x, y| band(sd_box(x, y, b, r), 0.0, d));
    p.fill(b, r, fill);
    if let Some(rim) = rim {
        p.rim(b, r, d, rim);
    }
    p.text(face, dev, m + pad_x, m + pad_y, text, ink);
    (p.into_raster(), (w, h))
}

/// Room around a pill's raster for its keyline.
pub const PILL_MARGIN: usize = 1;

/// Logical size of a [`pill`].
pub fn pill_size(text: &str, face: Face, (pad_y, pad_x): (usize, usize)) -> (usize, usize) {
    (face.width(text) + pad_x * 2, face.line_h() + pad_y * 2)
}

/// A plain rounded rectangle `w`x`h` logical: `fill` inside, then a rim of
/// `(width, colour)` just inside its edge. The pick wash, the leader's dot
/// and runs, the selector band in the dumps.
pub fn shape(
    w: usize,
    h: usize,
    radius: f32,
    fill: Option<Rgba>,
    rim: Option<(f32, Rgba)>,
    dev: usize,
) -> Raster {
    let dev = dev.max(1);
    let d = dev as f32;
    let mut p = Paint::new(w * dev, h * dev);
    let b = dbox(0.0, 0.0, w as f32, h as f32, dev);
    if let Some(c) = fill {
        p.fill(b, radius * d, c);
    }
    if let Some((width, c)) = rim {
        p.rim(b, radius * d, width * d, c);
    }
    p.into_raster()
}

/// The region rim's corner radius.
pub const REGION_RADIUS: f32 = 10.0;
/// How far the rim and keyline reach outside the region: 1px each.
pub const REGION_REACH: i32 = 2;

/// One piece of the rounded rim around a region, so a large region costs
/// four thin textures rather than one the size of the screen.
///
/// `region` is the rectangle the rim goes round and `part` the piece of it
/// to draw, both logical in one space; the result covers exactly `part`.
/// The rim is 1px of `region_outline` just outside the region, and outside
/// that 1px of keyline -- never a fill, so nothing in the region is covered.
pub fn rim_part(
    region: (i32, i32, i32, i32),
    part: (i32, i32, i32, i32),
    pal: &HudPalette,
    dev: usize,
) -> Raster {
    let dev = dev.max(1);
    let d = dev as f32;
    let (pw, ph) = (part.2.max(0) as usize, part.3.max(0) as usize);
    let mut p = Paint::new(pw * dev, ph * dev);
    let b = dbox(
        (region.0 - part.0) as f32,
        (region.1 - part.1) as f32,
        region.2 as f32,
        region.3 as f32,
        dev,
    );
    let r = REGION_RADIUS * d;
    let all = (0.0, 0.0, p.w as f32, p.h as f32);
    p.shade(all, pal.keyline, |x, y| band(sd_box(x, y, b, r), d, 2.0 * d));
    p.shade(all, pal.region_outline, |x, y| band(sd_box(x, y, b, r), 0.0, d));
    p.into_raster()
}

/// The four pieces [`rim_part`] is drawn in, for a region `(x, y, w, h)`:
/// a top and a bottom band holding the corners, and two thin sides.
pub fn rim_parts(r: (i32, i32, i32, i32)) -> Vec<(i32, i32, i32, i32)> {
    let (x, y, w, h) = r;
    let e = REGION_REACH;
    // Deep enough to hold the corner's whole curve; a short region is just
    // split in two, so no row is drawn twice.
    let k = (REGION_RADIUS as i32 + 1).min(h / 2).max(0);
    let mut v = vec![
        (x - e, y - e, w + 2 * e, k + e),
        (x - e, y + h - k, w + 2 * e, k + e),
    ];
    let side = h - 2 * k;
    if side > 0 {
        v.push((x - e, y + k, e, side));
        v.push((x + w, y + k, e, side));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(chip: &str, title: &str, body: &str, error: bool) -> Card {
        Card::new(chip, title, body, 32, error)
    }

    #[test]
    fn the_raster_is_the_panel_plus_its_shadow_times_the_scale() {
        let pal = HudPalette::default();
        for c in [
            card("", "", "hello world", false),
            card("B", "Jupiter", "The largest planet.", false),
            card("", "Just a headline", "", false),
            card("", "no answer", "rate limited", true),
        ] {
            let (w, h) = c.size();
            for dev in [1usize, 2] {
                let r = panel(&c, &pal, true, dev);
                assert_eq!(
                    (r.w as usize, r.h as usize),
                    ((w + 2 * SHADOW_X) * dev, (h + SHADOW_TOP + SHADOW_BOTTOM) * dev)
                );
            }
        }
    }

    #[test]
    fn a_card_is_only_as_tall_as_what_it_says() {
        let body = card("", "", "one line", false).size().1;
        let head = card("", "one line", "", false).size().1;
        let both = card("", "one line", "one line", false).size().1;
        assert_eq!(both, head + body - PAD_T - PAD_B + RULE_GAP * 2 + 1);
        let two = card("", "", "one line\ntwo lines", false).size().1;
        assert_eq!(two - body, Face::Body.line_h() + LINE_GAP);
        assert!(card("", "", "", false).is_empty());
        assert!(!card("", "", "", true).is_empty(), "an error always says so");
    }

    #[test]
    fn an_error_drops_the_chip() {
        // Error kind: zero gold. The chip is the only gold a panel draws.
        let c = card("B", "no answer", "", true);
        assert!(c.chip.is_empty());
    }

    #[test]
    fn the_chip_narrows_the_titles_first_row() {
        let t = "aaaaa bbbbb ccccc";
        let plain = Card::new("", t, "", 12, false);
        let chipped = Card::new("B", t, "", 12, false);
        assert!(chipped.title.len() > plain.title.len());
        assert!(chipped.title.len() <= MAX_TITLE_LINES);
    }

    #[test]
    fn a_trailing_parenthetical_is_meta() {
        assert_eq!(meta_split("Jupiter (low confidence)"), Some(8));
        assert_eq!(meta_split("Jupiter"), None);
        assert_eq!(meta_split("(only this)"), None);
    }

    #[test]
    fn the_shadow_stays_outside_the_glass() {
        // The panel centre is the tint and nothing else: a shadow under
        // translucent glass would darken it.
        let pal = HudPalette::default();
        let c = card("", "", &"x ".repeat(20), false);
        let r = panel(&c, &pal, true, 1);
        let (w, h) = c.size();
        // Inside the right padding, clear of text, rim and corners.
        let (cx, cy) = (SHADOW_X + w - PAD_X / 2, SHADOW_TOP + h / 2);
        let a = r.px[(cy * r.w as usize + cx) * 4 + 3];
        let want = (pal.panel_tint[3] * 255.0).round() as i32;
        assert!((a as i32 - want).abs() <= 1, "{a} vs {want}");
    }

    #[test]
    fn the_rim_parts_cover_the_rim_and_not_the_inside() {
        let parts = rim_parts((100, 100, 300, 200));
        assert_eq!(parts.len(), 4);
        for (x, y, w, h) in &parts {
            assert!(*w > 0 && *h > 0);
            // No part reaches into the region past its corner band.
            let inside = (x + w > 112 && *x < 388) && (y + h > 112 && *y < 288);
            assert!(!inside, "{x},{y} {w}x{h}");
        }
        // A tiny region is just the two corner bands.
        assert_eq!(rim_parts((0, 0, 6, 6)).len(), 2);
    }
}
