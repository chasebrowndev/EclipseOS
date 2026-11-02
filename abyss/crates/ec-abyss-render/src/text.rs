// SPDX-License-Identifier: AGPL-3.0-only
//! Sanitising and rasterising annotation text (COMP-18 §2).
//!
//! Every string that reaches the annotation pass has been written, directly or
//! indirectly, by something outside the compositor -- in Oracle-Eyes' case by a
//! model summarising pixels an attacker chose. So the rules of COMP-18 §2 are
//! enforced *here*, on the compositor side of the socket, and a caller can
//! affect nothing about the result but which glyphs appear:
//!
//! * control characters are **stripped, not escaped** -- an escape is still a
//!   sequence the caller chose, and `\r`-style cursor games are the whole
//!   reason the rule exists;
//! * length is clamped hard, in characters and in lines, so no string can grow
//!   a panel until it covers the screen;
//! * nothing is interpreted. There is no markup, no colour escape, no
//!   substitution. A `<b>` renders as four glyphs.
//!
//! Two sanitisers, one per consumer. [`sanitize`] is the trusted UI's and
//! stays printable ASCII only: a prompt must not carry a lookalike letter
//! (homoglyph spoofing). [`sanitize_annotation`] is the annotation HUD's and
//! keeps the wider set the HUD atlases draw (Latin-1, Greek, math). Both
//! strip controls, bidi overrides and zero-width characters, and both clamp
//! length the same way.

use crate::hud_font;

/// Hard character cap, applied after sanitising and before wrapping.
/// `spec.md` §5 asks for 2-4 sentences; this is roughly twice that, so the
/// cap is a backstop against a runaway reply rather than the usual limit.
pub const MAX_CHARS: usize = 480;

/// Hard line cap, applied after wrapping. Lines beyond it are dropped.
pub const MAX_LINES: usize = 10;

/// Substituted for any character with no glyph, so a dropped codepoint is
/// visible as a gap in the text rather than silently closing up.
const REPLACEMENT: char = '?';

/// Reduce arbitrary input to the printable ASCII the trusted UI can draw.
///
/// Newlines survive as explicit line breaks; every other control character,
/// including tab and carriage return, is dropped. Non-ASCII collapses to
/// [`REPLACEMENT`] -- one glyph per source character, so nothing shifts. The
/// annotation HUD uses [`sanitize_annotation`] instead.
pub fn sanitize(input: &str) -> String {
    let mut out = String::with_capacity(input.len().min(MAX_CHARS));
    let mut chars = 0usize;
    for c in input.chars() {
        if chars >= MAX_CHARS {
            break;
        }
        let keep = match c {
            '\n' => '\n',
            c if (c as u32) < 0x20 || c as u32 == 0x7F => continue,
            c if c.is_ascii() => c,
            _ => REPLACEMENT,
        };
        out.push(keep);
        chars += 1;
    }
    out
}

/// Reduce annotation text to the glyphs the annotation atlases draw, the
/// same way [`sanitize`] reduces trusted text.
///
/// The kept set is exactly [`hud_font::covers`]: printable ASCII, the
/// compositor's two extras, Latin-1, Latin Extended-A, Greek and the math
/// and punctuation set. Everything else is handled in this order:
///
/// * newlines survive as line breaks; no-break space becomes a space;
/// * every other control character (C0, DEL, C1) is **stripped**, so a
///   caller cannot aim a cursor game or an escape at the panel;
/// * bidi overrides and isolates, zero-width characters, the soft hyphen and
///   the byte-order mark are **stripped**: they are invisible and would let
///   a reply read differently from the order its bytes arrive in;
/// * combining marks are **stripped**, so a decomposed `e` + U+0301 draws as
///   `e` rather than as a stray mark over the next glyph;
/// * anything else the atlas lacks becomes [`REPLACEMENT`], one glyph per
///   character, so a dropped codepoint shows as a gap rather than closing up.
///
/// Length is clamped by [`MAX_CHARS`] in characters, as in [`sanitize`].
pub fn sanitize_annotation(input: &str) -> String {
    let mut out = String::with_capacity(input.len().min(MAX_CHARS));
    let mut chars = 0usize;
    for c in input.chars() {
        if chars >= MAX_CHARS {
            break;
        }
        let keep = match c {
            '\n' => '\n',
            '\u{a0}' => ' ',
            c if c.is_control() => continue,
            c if is_invisible(c) || is_combining(c) => continue,
            c if hud_font::covers(c) => c,
            _ => REPLACEMENT,
        };
        out.push(keep);
        chars += 1;
    }
    out
}

/// Format and bidi characters a reader cannot see, which must not reach the
/// panel: soft hyphen, Arabic letter mark, Mongolian vowel separator, the
/// zero-width and bidi-control blocks, word joiner and invisible operators,
/// and the byte-order mark.
fn is_invisible(c: char) -> bool {
    matches!(
        c as u32,
        0x00ad
            | 0x061c
            | 0x180e
            | 0x200b..=0x200f
            | 0x202a..=0x202e
            | 0x2060..=0x206f
            | 0xfeff
    )
}

/// The combining-mark blocks: a mark drawn without its base is a stray glyph.
fn is_combining(c: char) -> bool {
    matches!(
        c as u32,
        0x0300..=0x036f | 0x1ab0..=0x1aff | 0x1dc0..=0x1dff | 0x20d0..=0x20ff | 0xfe20..=0xfe2f
    )
}

/// Break sanitised text into at most [`MAX_LINES`] lines of at most `cols`
/// characters, preferring word boundaries and hard-breaking a word longer than
/// the column count. Columns are characters, not bytes: a Greek word is one
/// column per letter, and a cut never splits a character.
pub fn wrap(text: &str, cols: usize) -> Vec<String> {
    let cols = cols.max(1);
    let mut lines = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        // Characters in `line`.
        let mut len = 0usize;
        for word in para.split(' ').filter(|w| !w.is_empty()) {
            let n = word.chars().count();
            // A word wider than the whole panel is cut rather than allowed to
            // overflow; there is nowhere else for it to go.
            if n > cols {
                if !line.is_empty() {
                    lines.push(std::mem::take(&mut line));
                }
                len = 0;
                // The tail of the cut becomes the current line, so a short
                // word after a long one still shares a row with it.
                let chars: Vec<char> = word.chars().collect();
                let mut chunks = chars.chunks(cols).peekable();
                while let Some(chunk) = chunks.next() {
                    let chunk: String = chunk.iter().collect();
                    if chunks.peek().is_none() {
                        len = chunk.chars().count();
                        line = chunk;
                    } else {
                        lines.push(chunk);
                    }
                }
                continue;
            }
            let need = if line.is_empty() { n } else { len + 1 + n };
            if need > cols {
                lines.push(std::mem::take(&mut line));
                len = 0;
            }
            if !line.is_empty() {
                line.push(' ');
                len += 1;
            }
            line.push_str(word);
            len += n;
        }
        lines.push(line);
    }
    // A trailing empty paragraph is an artefact of the split, not a blank line
    // the caller asked for.
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.truncate(MAX_LINES);
    lines
}

/// One line of caller text, for places that have room for exactly one: the
/// same reduction as [`sanitize`], with a newline turned into a space rather
/// than a break.
pub fn sanitize_line(input: &str) -> String {
    sanitize(input).replace('\n', " ")
}

/// [`sanitize_line`] for the annotation HUD: one line, the annotation glyph
/// set.
pub fn sanitize_annotation_line(input: &str) -> String {
    sanitize_annotation(input).replace('\n', " ")
}

/// A rasterised panel: premultiplied RGBA, row-major, `w * h * 4` bytes.
pub struct Raster {
    pub w: i32,
    pub h: i32,
    pub px: Vec<u8>,
}

/// Straight (non-premultiplied) RGBA, as written in config and in the source.
pub type Rgba = [f32; 4];

/// A panel being drawn: logical-pixel shapes and text, kept as a list and
/// replayed at the device scale by [`Canvas::into_raster`], so edges and
/// glyphs are drawn at the real pixel density instead of scaled up from 1x.
/// Shapes are anti-aliased and composite over what is under them, in order.
/// Used by `trusted_ui` for its prompts and cards.
pub struct Canvas {
    w: usize,
    h: usize,
    ops: Vec<Op>,
}

enum Op {
    Rect {
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        c: [u8; 4],
    },
    Round {
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        r: usize,
        fill: [u8; 4],
        line: Option<(usize, [u8; 4])>,
    },
    Text {
        x: usize,
        y: usize,
        s: String,
        c: [u8; 4],
        medium: bool,
    },
}

impl Canvas {
    pub fn new(w: usize, h: usize) -> Canvas {
        Canvas {
            w,
            h,
            ops: Vec::new(),
        }
    }

    /// A square-cornered rectangle.
    pub fn fill(&mut self, x: usize, y: usize, w: usize, h: usize, c: Rgba) {
        self.ops.push(Op::Rect {
            x,
            y,
            w,
            h,
            c: premul(c),
        });
    }

    /// A rectangle with corners of radius `r` (clamped to half the short
    /// side, so `usize::MAX` is a pill), filled, with an optional inside
    /// border `line = (width, colour)`.
    #[allow(clippy::too_many_arguments)]
    pub fn round(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        r: usize,
        fill: Rgba,
        line: Option<(usize, Rgba)>,
    ) {
        self.ops.push(Op::Round {
            x,
            y,
            w,
            h,
            r: r.min(w / 2).min(h / 2),
            fill: premul(fill),
            line: line.map(|(lw, c)| (lw, premul(c))),
        });
    }

    /// One row of glyphs, `y` the top of the cell ([`crate::glyphs::CELL_H`]
    /// tall). `medium` draws the heavier weight. A byte outside printable
    /// ASCII draws nothing; `sanitize` has replaced any such byte already.
    pub fn text(&mut self, x: usize, y: usize, s: &str, c: Rgba, medium: bool) {
        self.ops.push(Op::Text {
            x,
            y,
            s: s.to_owned(),
            c: premul(c),
            medium,
        });
    }

    /// Draw at `scale` device pixels per logical pixel.
    pub fn into_raster(self, scale: usize) -> Raster {
        let scale = scale.max(1);
        let s = scale.min(crate::glyphs::MAX_SCALE);
        let (w, h) = (self.w * s, self.h * s);
        let mut px = vec![[0u8; 4]; w * h];
        for op in &self.ops {
            match *op {
                Op::Rect {
                    x,
                    y,
                    w: rw,
                    h: rh,
                    c,
                } => {
                    for yy in (y * s)..((y + rh) * s).min(h) {
                        for xx in (x * s)..((x + rw) * s).min(w) {
                            over(&mut px[yy * w + xx], c, 1.0);
                        }
                    }
                }
                Op::Round {
                    x,
                    y,
                    w: rw,
                    h: rh,
                    r,
                    fill,
                    line,
                } => {
                    let (x0, y0, x1, y1) = (x * s, y * s, ((x + rw) * s).min(w), ((y + rh) * s).min(h));
                    let (cx, cy) = ((x0 + (x + rw) * s) as f32 / 2.0, (y0 + (y + rh) * s) as f32 / 2.0);
                    let (hw, hh) = ((rw * s) as f32 / 2.0, (rh * s) as f32 / 2.0);
                    let rr = (r * s) as f32;
                    let lw = line.map_or(0.0, |(lw, _)| (lw * s) as f32);
                    for yy in y0..y1 {
                        for xx in x0..x1 {
                            let d = rounded_sdf(xx as f32 + 0.5 - cx, yy as f32 + 0.5 - cy, hw, hh, rr);
                            let outer = (0.5 - d).clamp(0.0, 1.0);
                            if outer <= 0.0 {
                                continue;
                            }
                            let p = &mut px[yy * w + xx];
                            match line {
                                Some((_, lc)) => {
                                    let inner = (0.5 - (d + lw)).clamp(0.0, 1.0);
                                    over(p, fill, inner);
                                    over(p, lc, outer - inner);
                                }
                                None => over(p, fill, outer),
                            }
                        }
                    }
                }
                Op::Text {
                    x,
                    y,
                    s: ref text,
                    c,
                    medium,
                } => {
                    let (cw, ch) = (crate::glyphs::ADVANCE * s, crate::glyphs::CELL_H * s);
                    for (col, b) in text.bytes().enumerate() {
                        let Some(cell) = crate::glyphs::cell(b, medium, s) else {
                            continue;
                        };
                        let ox = (x + col * crate::glyphs::ADVANCE) * s;
                        for gy in 0..ch {
                            let yy = y * s + gy;
                            if yy >= h {
                                break;
                            }
                            for gx in 0..cw {
                                let xx = ox + gx;
                                let a = cell[gy * cw + gx];
                                if xx < w && a != 0 {
                                    over(&mut px[yy * w + xx], c, f32::from(a) / 255.0);
                                }
                            }
                        }
                    }
                }
            }
        }
        let mut raster = Raster {
            w: w as i32,
            h: h as i32,
            px: px.into_iter().flatten().collect(),
        };
        if scale != s {
            raster = resample(&raster, self.w * scale, self.h * scale);
        }
        raster
    }
}

/// Signed distance from a rounded rectangle centred on the origin, negative
/// inside.
fn rounded_sdf(px: f32, py: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let qx = px.abs() - (hw - r);
    let qy = py.abs() - (hh - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - r
}

/// Premultiplied source over destination, the source scaled by coverage `k`.
fn over(dst: &mut [u8; 4], src: [u8; 4], k: f32) {
    if k <= 0.0 {
        return;
    }
    let k = k.min(1.0);
    let sa = f32::from(src[3]) * k / 255.0;
    for i in 0..4 {
        let v = f32::from(src[i]) * k + f32::from(dst[i]) * (1.0 - sa);
        dst[i] = v.round().clamp(0.0, 255.0) as u8;
    }
}

/// Nearest-neighbour resample, for scales above the largest glyph set.
fn resample(r: &Raster, w: usize, h: usize) -> Raster {
    let (sw, sh) = (r.w as usize, r.h as usize);
    let mut px = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let sy = y * sh / h;
        for x in 0..w {
            let sx = x * sw / w;
            px.extend_from_slice(&r.px[(sy * sw + sx) * 4..][..4]);
        }
    }
    Raster {
        w: w as i32,
        h: h as i32,
        px,
    }
}

/// Straight float RGBA to premultiplied bytes, which is what the GL texture
/// and every `TextureRenderElement` downstream of it expect.
pub(super) fn premul(c: Rgba) -> [u8; 4] {
    let a = c[3].clamp(0.0, 1.0);
    let f = |v: f32| (v.clamp(0.0, 1.0) * a * 255.0).round() as u8;
    [f(c[0]), f(c[1]), f(c[2]), (a * 255.0).round() as u8]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_are_stripped_not_escaped() {
        // COMP-18 §2. The output must contain neither the control byte nor a
        // textual rendering of it that the caller could have aimed at.
        let dirty = "a\u{1b}[31mb\r\nc\td\u{0}e";
        let clean = sanitize(dirty);
        assert_eq!(clean, "a[31mb\ncde");
        assert!(!clean.contains('\u{1b}'));
        assert!(!clean.contains("\\x1b"));
        assert!(!clean.contains("^["));
        assert!(clean.bytes().all(|b| b == b'\n' || (0x20..0x7f).contains(&b)));
    }

    #[test]
    fn non_ascii_becomes_one_replacement_glyph_each() {
        assert_eq!(sanitize("caf\u{e9} \u{1f480}"), "caf? ?");
    }

    #[test]
    fn length_is_clamped_hard() {
        let clean = sanitize(&"x".repeat(MAX_CHARS * 4));
        assert_eq!(clean.chars().count(), MAX_CHARS);
        let lines = wrap(&"y ".repeat(MAX_CHARS), 10);
        assert!(lines.len() <= MAX_LINES);
    }

    #[test]
    fn annotation_keeps_greek_latin_and_math() {
        let s = "Σ = π/2 ≤ 4 · αβγ Ωμ café ±×÷ ²³¹ H₂O → ∞ — “ok”";
        assert_eq!(sanitize_annotation(s), s);
    }

    #[test]
    fn model_reply_headline_survives_sanitise_and_wrap() {
        // Replay-style: the headline a model returns, through the same steps
        // the annotation pass runs on it, comes out byte-for-byte intact.
        let reply = "Σ = π/2 ≤ 4";
        let clean = sanitize_annotation(reply);
        assert_eq!(clean, reply);
        assert_eq!(wrap(&clean, 24), [reply]);
        assert_eq!(sanitize_annotation_line(reply), reply);
    }

    #[test]
    fn annotation_strips_bidi_zero_width_and_combining() {
        // An RTL override would reorder the reader's view of the reply.
        assert_eq!(sanitize_annotation("a\u{202e}b\u{202c}c"), "abc");
        assert_eq!(sanitize_annotation("x\u{2066}y\u{2069}z"), "xyz");
        assert_eq!(
            sanitize_annotation("a\u{200b}b\u{200d}c\u{feff}d\u{ad}e"),
            "abcde"
        );
        // A decomposed accent folds away instead of drawing over the next glyph.
        assert_eq!(sanitize_annotation("e\u{301}x"), "ex");
    }

    #[test]
    fn annotation_strips_c0_and_c1_controls() {
        assert_eq!(sanitize_annotation("a\u{1b}[31mb\rc\td\u{7f}"), "a[31mbcd");
        assert_eq!(sanitize_annotation("x\u{85}y\u{9b}z"), "xyz");
    }

    #[test]
    fn annotation_outside_the_atlas_is_one_replacement_each() {
        assert_eq!(sanitize_annotation("\u{4e2d}\u{1f480}!"), "??!");
        assert_eq!(sanitize_annotation("a\u{a0}b"), "a b");
    }

    #[test]
    fn annotation_length_is_clamped_in_characters() {
        let clean = sanitize_annotation(&"α".repeat(MAX_CHARS * 2));
        assert_eq!(clean.chars().count(), MAX_CHARS);
    }

    #[test]
    fn the_trusted_sanitiser_stays_ascii() {
        // Homoglyph guard: the trusted UI never draws a Greek or accented
        // letter, even though the annotation HUD does.
        assert_eq!(sanitize("caf\u{e9} \u{3a3}"), "caf? ?");
    }

    #[test]
    fn wrap_counts_characters_not_bytes() {
        assert_eq!(wrap("αβγδ εζ", 4), ["αβγδ", "εζ"]);
        // A hard cut never splits a character.
        assert_eq!(wrap("αβγδεζ", 4), ["αβγδ", "εζ"]);
    }

    #[test]
    fn markup_is_inert() {
        // Nothing is interpreted: the tag survives as literal glyphs, which is
        // the point -- a caller cannot reach styling at all.
        assert_eq!(sanitize("<b>bold</b> &amp; %s"), "<b>bold</b> &amp; %s");
    }

    #[test]
    fn wrapping_prefers_words_and_cuts_only_when_it_must() {
        assert_eq!(wrap("the quick brown fox", 10), ["the quick", "brown fox"]);
        assert_eq!(wrap("aaaaaaaaaaaa b", 5), ["aaaaa", "aaaaa", "aa b"]);
    }

    #[test]
    fn a_rounded_panel_is_opaque_inside_and_clear_at_the_corner() {
        let mut c = Canvas::new(40, 30);
        c.round(
            0,
            0,
            40,
            30,
            10,
            [0.1, 0.1, 0.1, 1.0],
            Some((1, [1.0, 1.0, 1.0, 1.0])),
        );
        let r = c.into_raster(2);
        let at = |x: usize, y: usize| &r.px[(y * 80 + x) * 4..][..4];
        assert_eq!(at(0, 0)[3], 0, "the corner is cut");
        assert_eq!(at(40, 30), &[26, 26, 26, 255], "the middle is the fill");
        assert_eq!(at(40, 0), &[255, 255, 255, 255], "the top edge is the line");
    }

    #[test]
    fn scales_above_the_glyph_sets_are_the_layout_times_the_scale() {
        for s in 1..=5 {
            let mut c = Canvas::new(10, 7);
            c.text(0, 0, "hi", [1.0; 4], false);
            let r = c.into_raster(s);
            assert_eq!(
                (r.w, r.h, r.px.len()),
                (10 * s as i32, 7 * s as i32, 10 * 7 * s * s * 4)
            );
        }
    }

    #[test]
    fn colours_are_premultiplied() {
        // Half-transparent white must be half-bright, or it composites as a
        // bright halo over dark content.
        assert_eq!(premul([1.0, 1.0, 1.0, 0.5]), [128, 128, 128, 128]);
        assert_eq!(premul([1.0, 0.0, 0.0, 1.0]), [255, 0, 0, 255]);
    }
}
