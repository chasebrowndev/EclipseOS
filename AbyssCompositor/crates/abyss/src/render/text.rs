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

use super::font::{ADVANCE, FIRST, FONT, GLYPH_H, GLYPH_W, LAST, LINE_H};

/// Hard character cap, applied after sanitising and before wrapping.
/// `spec.md` §5 asks for 2-4 sentences; this is roughly twice that, so the
/// cap is a backstop against a runaway reply rather than the usual limit.
pub const MAX_CHARS: usize = 480;

/// Hard line cap, applied after wrapping. Lines beyond it are dropped.
pub const MAX_LINES: usize = 10;

/// Substituted for any character with no glyph, so a dropped codepoint is
/// visible as a gap in the text rather than silently closing up.
const REPLACEMENT: char = '?';

/// Reduce arbitrary input to the printable ASCII this font can draw.
///
/// Newlines survive as explicit line breaks; every other control character,
/// including tab and carriage return, is dropped. Non-ASCII collapses to
/// [`REPLACEMENT`] -- one glyph per source character, so nothing shifts.
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

/// Break sanitised text into at most [`MAX_LINES`] lines of at most `cols`
/// characters, preferring word boundaries and hard-breaking a word longer than
/// the column count.
pub fn wrap(text: &str, cols: usize) -> Vec<String> {
    let cols = cols.max(1);
    let mut lines = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        for word in para.split(' ').filter(|w| !w.is_empty()) {
            // A word wider than the whole panel is cut rather than allowed to
            // overflow; there is nowhere else for it to go.
            if word.len() > cols {
                if !line.is_empty() {
                    lines.push(std::mem::take(&mut line));
                }
                // The tail of the cut becomes the current line, so a short
                // word after a long one still shares a row with it.
                let mut chunks = word.as_bytes().chunks(cols).peekable();
                while let Some(chunk) = chunks.next() {
                    let chunk = String::from_utf8_lossy(chunk).into_owned();
                    if chunks.peek().is_none() {
                        line = chunk;
                    } else {
                        lines.push(chunk);
                    }
                }
                continue;
            }
            let need = if line.is_empty() {
                word.len()
            } else {
                line.len() + 1 + word.len()
            };
            if need > cols {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
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

/// A rasterised panel: premultiplied RGBA, row-major, `w * h * 4` bytes.
pub struct Raster {
    pub w: i32,
    pub h: i32,
    pub px: Vec<u8>,
}

/// Straight (non-premultiplied) RGBA, as written in config and in the source.
pub type Rgba = [f32; 4];

/// The card palette. Every pixel of a card is written, never blended, so each
/// colour here is already what it looks like on the panel: the rule is amber
/// mixed down onto the background, not amber at partial alpha. Only the two
/// fills are translucent, so the content beneath still shows through them.
const BG: Rgba = [0.030, 0.024, 0.018, 0.96];
const HEAD: Rgba = [0.100, 0.072, 0.028, 0.98];
const TITLE: Rgba = [1.0, 0.78, 0.26, 1.0];
const BODY: Rgba = [0.87, 0.76, 0.47, 1.0];
const RULE: Rgba = [0.40, 0.29, 0.09, 1.0];
/// Eclipse amber: the stripe, the chamfers, the chip, and everything in
/// `super::annotation` that marks the screen.
pub const AMBER: Rgba = [1.0, 0.72, 0.20, 1.0];
/// Glyphs cut out of an amber fill.
const INK: Rgba = [0.030, 0.024, 0.018, 1.0];

/// Space between the text block and the card's edges, in logical pixels.
const PAD_X: usize = 10;
const PAD_TOP: usize = 8;
/// Less below than above: the font cell already ends in four rows of descent.
const PAD_BOTTOM: usize = 5;
/// The solid amber edge down the card's left side.
const STRIPE: usize = 2;
/// Horizontal space a card spends on everything but its text.
pub const CHROME_W: usize = STRIPE + PAD_X * 2;
/// The corners cut off the top-left and bottom-right. Nothing is rounded:
/// hard edges, the same as the rest of the system's chrome.
const CHAMFER: usize = 8;
/// Horizontal padding inside a chip, and the gap between it and the title.
const CHIP_PAD: usize = 4;
const CHIP_GAP: usize = 8;
/// Title lines kept after wrapping. A headline that needs more is not one.
const MAX_TITLE_LINES: usize = 3;

/// What a card says, wrapped and ready to draw. Everything in it has already
/// been through [`sanitize`]; a card cannot hold anything the font cannot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    /// A short label drawn inverted before the title -- the pick, when there
    /// is one. Empty for none.
    pub chip: String,
    pub title: Vec<String>,
    pub body: Vec<String>,
}

impl Card {
    /// Wrap `title` and `body` at `cols` characters. The title shares its
    /// first row with the chip, so it wraps that much narrower.
    pub fn new(chip: &str, title: &str, body: &str, cols: usize) -> Card {
        let title_cols = cols.saturating_sub(chip_cols(chip)).max(1);
        let mut title = wrap(title, title_cols);
        title.truncate(MAX_TITLE_LINES);
        Card {
            chip: chip.to_string(),
            title,
            body: wrap(body, cols),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.chip.is_empty() && self.title.is_empty() && self.body.is_empty()
    }

    fn has_head(&self) -> bool {
        !self.chip.is_empty() || !self.title.is_empty()
    }

    /// Height of the header band, zero without one.
    fn head_h(&self) -> usize {
        if !self.has_head() {
            return 0;
        }
        PAD_TOP + block_h(self.title.len().max(1)) + PAD_BOTTOM
    }

    fn body_h(&self) -> usize {
        if self.body.is_empty() {
            return 0;
        }
        PAD_TOP + block_h(self.body.len()) + PAD_BOTTOM
    }

    /// The rule between header and body, when there are both.
    fn rule_h(&self) -> usize {
        usize::from(self.has_head() && !self.body.is_empty())
    }

    /// Logical size of the card. The raster is exactly this times its scale,
    /// so the annotation pass can place a card before drawing it.
    pub fn size(&self) -> (usize, usize) {
        if self.is_empty() {
            return (0, 0);
        }
        let head = if self.has_head() {
            chip_w(&self.chip) + text_w(&self.title)
        } else {
            0
        };
        let w = STRIPE + PAD_X + head.max(text_w(&self.body)) + PAD_X;
        let h = self.head_h() + self.rule_h() + self.body_h();
        (w.max(CHAMFER * 3), h.max(CHAMFER * 2 + 1))
    }
}

/// Width of a chip and the gap after it, in pixels. Zero for no chip.
fn chip_w(chip: &str) -> usize {
    if chip.is_empty() {
        0
    } else {
        chip.len() * ADVANCE + CHIP_PAD * 2 + CHIP_GAP
    }
}

/// The same, in whole columns, for wrapping the title beside it.
fn chip_cols(chip: &str) -> usize {
    chip_w(chip).div_ceil(ADVANCE)
}

fn text_w(lines: &[String]) -> usize {
    lines.iter().map(|l| l.len()).max().unwrap_or(0) * ADVANCE
}

fn block_h(rows: usize) -> usize {
    if rows == 0 {
        0
    } else {
        rows * LINE_H - (LINE_H - GLYPH_H)
    }
}

/// A 1x canvas of premultiplied pixels. Cards are laid out and drawn at one
/// pixel per logical pixel and then scaled up whole, so every edge is exactly
/// on the device grid at any integer scale and nothing is ever smeared.
struct Canvas {
    w: usize,
    h: usize,
    px: Vec<[u8; 4]>,
}

impl Canvas {
    fn new(w: usize, h: usize) -> Canvas {
        Canvas {
            w,
            h,
            px: vec![[0; 4]; w * h],
        }
    }

    fn put(&mut self, x: usize, y: usize, c: [u8; 4]) {
        if x < self.w && y < self.h {
            self.px[y * self.w + x] = c;
        }
    }

    /// Fill a rectangle, but only where something is already drawn: that is
    /// how the header band and the stripe take the card's chamfered shape.
    fn fill_on(&mut self, x: usize, y: usize, w: usize, h: usize, c: Rgba) {
        let c = premul(c);
        for yy in y..(y + h).min(self.h) {
            for xx in x..(x + w).min(self.w) {
                if self.px[yy * self.w + xx][3] != 0 {
                    self.px[yy * self.w + xx] = c;
                }
            }
        }
    }

    fn fill(&mut self, x: usize, y: usize, w: usize, h: usize, c: Rgba) {
        let c = premul(c);
        for yy in y..(y + h).min(self.h) {
            for xx in x..(x + w).min(self.w) {
                self.px[yy * self.w + xx] = c;
            }
        }
    }

    /// One row of glyphs. Bold is the same glyph again one pixel to the
    /// right; the cells have a blank last column for exactly that to land in.
    fn text(&mut self, x: usize, y: usize, s: &str, c: Rgba, bold: bool) {
        let c = premul(c);
        for (col, ch) in s.bytes().enumerate() {
            // `sanitize` guarantees this, but the font index must not be able
            // to depend on caller data even if a future path skips it.
            if !(FIRST..=LAST).contains(&ch) {
                continue;
            }
            let glyph = &FONT[(ch - FIRST) as usize];
            let ox = x + col * ADVANCE;
            for (gy, bits) in glyph.iter().enumerate() {
                for gx in 0..GLYPH_W {
                    if bits & (0x80 >> gx) != 0 {
                        self.put(ox + gx, y + gy, c);
                        if bold {
                            self.put(ox + gx + 1, y + gy, c);
                        }
                    }
                }
            }
        }
    }

    /// Scale up by whole pixels into a raster.
    fn into_raster(self, scale: usize) -> Raster {
        let (w, h) = (self.w * scale, self.h * scale);
        let mut px = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            let row = &self.px[(y / scale) * self.w..][..self.w];
            for p in row {
                for _ in 0..scale {
                    px.extend_from_slice(p);
                }
            }
        }
        Raster {
            w: w as i32,
            h: h as i32,
            px,
        }
    }
}

/// Whether `(x, y)` is inside a `w`x`h` card once its two corners are cut.
fn inside(x: usize, y: usize, w: usize, h: usize) -> bool {
    x + y >= CHAMFER && (w - 1 - x) + (h - 1 - y) >= CHAMFER
}

/// Rasterise a card at `scale` device pixels per logical pixel.
///
/// ```text
///  ___________________________
/// /| [B] Headline in bold     |   header band, chip, title
/// ||--------------------------|   rule
/// || Body text, softer, as    |
/// || many lines as it needs.  /   chamfered corner
///  ---------------------------
/// ```
pub fn rasterize(card: &Card, scale: usize) -> Raster {
    let scale = scale.max(1);
    let (w, h) = card.size();
    let mut c = Canvas::new(w, h);
    if w == 0 {
        return c.into_raster(scale);
    }

    // Silhouette, then its edge: a pixel inside with a neighbour outside.
    let bg = premul(BG);
    for y in 0..h {
        for x in 0..w {
            if inside(x, y, w, h) {
                c.px[y * w + x] = bg;
            }
        }
    }
    let head_h = card.head_h();
    c.fill_on(0, 0, w, head_h, HEAD);
    let edge = |x: usize, y: usize| {
        inside(x, y, w, h)
            && (x == 0
                || y == 0
                || x == w - 1
                || y == h - 1
                || !inside(x - 1, y, w, h)
                || !inside(x + 1, y, w, h)
                || !inside(x, y - 1, w, h)
                || !inside(x, y + 1, w, h))
    };
    let (rule, amber) = (premul(RULE), premul(AMBER));
    for y in 0..h {
        for x in 0..w {
            if edge(x, y) {
                // The cuts are drawn at full strength: they are the accent,
                // the straight runs between them are only a boundary.
                let cut = x + y <= CHAMFER + 1 || (w - 1 - x) + (h - 1 - y) <= CHAMFER + 1;
                c.px[y * w + x] = if cut { amber } else { rule };
            }
        }
    }
    c.fill_on(0, 0, STRIPE, h, AMBER);

    let x0 = STRIPE + PAD_X;
    if card.has_head() {
        let y = PAD_TOP;
        let mut tx = x0;
        if !card.chip.is_empty() {
            let cw = card.chip.len() * ADVANCE + CHIP_PAD * 2;
            // The cell's top two rows are blank and its bottom four are
            // descent; cut at fourteen, the capitals sit two pixels in from
            // either edge and on the same line as the title's.
            c.fill(x0, y, cw, GLYPH_H - 2, AMBER);
            c.text(x0 + CHIP_PAD, y, &card.chip, INK, true);
            tx += chip_w(&card.chip);
        }
        for (row, line) in card.title.iter().enumerate() {
            c.text(tx, y + row * LINE_H, line, TITLE, true);
        }
    }
    let mut y = head_h;
    if card.rule_h() > 0 {
        c.fill_on(STRIPE, y, w - STRIPE, 1, RULE);
        y += 1;
    }
    for (row, line) in card.body.iter().enumerate() {
        c.text(x0, y + PAD_TOP + row * LINE_H, line, BODY, false);
    }
    c.into_raster(scale)
}

/// A chip on its own -- the label that marks a pick on screen, drawn exactly
/// like the one in the card's header so the eye pairs them.
pub fn tab(label: &str, scale: usize) -> Raster {
    let (w, h) = (label.len() * ADVANCE + CHIP_PAD * 2, GLYPH_H - 2);
    let mut c = Canvas::new(w, h);
    c.fill(0, 0, w, h, AMBER);
    c.text(CHIP_PAD, 0, label, INK, true);
    c.into_raster(scale.max(1))
}

/// Logical size of [`tab`]'s raster.
pub fn tab_size(label: &str) -> (usize, usize) {
    (label.len() * ADVANCE + CHIP_PAD * 2, GLYPH_H - 2)
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

    fn card(chip: &str, title: &str, body: &str) -> Card {
        Card::new(chip, &sanitize_line(title), &sanitize(body), 32)
    }

    #[test]
    fn every_printable_ascii_has_a_glyph_and_rasterises() {
        // Space is blank by design; everything else must put ink down. The
        // card's own chrome is ink too, so compare against the blank card
        // rather than asking for any opaque pixel at all -- otherwise the
        // frame alone would satisfy this and hide a missing glyph.
        // Space in the middle, where wrapping cannot trim it.
        let blank = rasterize(&card("", "", "x x"), 1);
        for b in FIRST + 1..=LAST {
            let body = format!("x{}x", b as char);
            let r = rasterize(&card("", "", &body), 1);
            assert_eq!((r.w, r.h), (blank.w, blank.h), "geometry moved");
            assert!(r.px != blank.px, "no ink for {:?}", b as char);
        }
    }

    #[test]
    fn the_raster_is_its_logical_size_times_its_scale() {
        // The annotation pass places a card by `size()` before drawing it,
        // and hands the raster to `TextureBuffer` at the same scale. Any
        // disagreement is a panel that drifts off its leader.
        for c in [
            card("", "", "hello world"),
            card("B", "Jupiter", "The largest planet."),
            card("", "Just a headline", ""),
            card("12", "", ""),
        ] {
            let (w, h) = c.size();
            for scale in [1usize, 2, 3] {
                let r = rasterize(&c, scale);
                assert_eq!((r.w as usize, r.h as usize), (w * scale, h * scale));
                assert_eq!(r.px.len(), (r.w * r.h * 4) as usize);
            }
        }
    }

    #[test]
    fn a_card_is_only_as_tall_as_what_it_says() {
        let body = card("", "", "one line").size().1;
        let head = card("", "one line", "").size().1;
        let both = card("", "one line", "one line").size().1;
        assert_eq!(both, head + body + 1, "header, rule, body");
        let two = card("", "", "one line\ntwo lines").size().1;
        assert_eq!(two - body, LINE_H);
        assert!(card("", "", "").is_empty());
        assert_eq!(rasterize(&card("", "", ""), 2).px.len(), 0);
    }

    #[test]
    fn the_chip_narrows_the_titles_first_row() {
        // Two rows at 12 columns; three once the chip takes its share.
        let t = "aaaaa bbbbb ccccc";
        let plain = Card::new("", t, "", 12);
        let chipped = Card::new("B", t, "", 12);
        assert!(chipped.title.len() > plain.title.len());
        assert!(chipped.title.len() <= MAX_TITLE_LINES);
    }

    #[test]
    fn the_corners_are_cut_and_the_rest_is_opaque_enough_to_read() {
        let r = rasterize(&card("", "", "hello"), 1);
        let at = |x: i32, y: i32| r.px[((y * r.w + x) * 4 + 3) as usize];
        assert_eq!(at(0, 0), 0, "top-left is chamfered away");
        assert_eq!(at(r.w - 1, r.h - 1), 0, "bottom-right too");
        assert_eq!(at(r.w - 1, 0), 255, "top-right is square, and edged");
        assert!(at(r.w / 2, r.h / 2) >= 200);
    }

    #[test]
    fn a_tab_matches_its_reported_size() {
        for scale in [1usize, 2] {
            let r = tab("B", scale);
            let (w, h) = tab_size("B");
            assert_eq!((r.w as usize, r.h as usize), (w * scale, h * scale));
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
