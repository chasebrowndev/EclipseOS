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

/// A rasterised panel: premultiplied RGBA, row-major, `w * h * 4` bytes.
pub struct Raster {
    pub w: i32,
    pub h: i32,
    pub px: Vec<u8>,
}

/// Straight (non-premultiplied) RGBA, as written in config and in the source.
pub type Rgba = [f32; 4];

/// Padding between the panel edge and the text, in unscaled pixels.
const PAD: usize = 4;

/// Rasterise `lines` into a panel. `scale` is an integer pixel multiplier --
/// the font has no hinting and no antialiasing, so a fractional scale would
/// only blur it; the caller rounds the output scale to get here.
pub fn rasterize(lines: &[String], scale: usize, fg: Rgba, bg: Rgba) -> Raster {
    let scale = scale.max(1);
    let cols = lines.iter().map(|l| l.len()).max().unwrap_or(0);
    // The last glyph on a line needs its width, not a full advance.
    let text_w = if cols == 0 {
        0
    } else {
        cols * ADVANCE - (ADVANCE - GLYPH_W)
    };
    let text_h = if lines.is_empty() {
        0
    } else {
        lines.len() * LINE_H - (LINE_H - GLYPH_H)
    };
    let w = (text_w + PAD * 2) * scale;
    let h = (text_h + PAD * 2) * scale;
    let mut px = vec![0u8; w * h * 4];

    let bg8 = premul(bg);
    for chunk in px.chunks_exact_mut(4) {
        chunk.copy_from_slice(&bg8);
    }
    let fg8 = premul(fg);

    for (row, line) in lines.iter().enumerate() {
        let oy = PAD + row * LINE_H;
        for (col, ch) in line.bytes().enumerate() {
            // `sanitize` guarantees this, but the font index must not be able
            // to depend on caller data even if a future path skips it.
            if !(FIRST..=LAST).contains(&ch) {
                continue;
            }
            let glyph = &FONT[(ch - FIRST) as usize];
            let ox = PAD + col * ADVANCE;
            for (gy, bits) in glyph.iter().enumerate() {
                for gx in 0..GLYPH_W {
                    if bits & (1 << (GLYPH_W - 1 - gx)) == 0 {
                        continue;
                    }
                    blit(&mut px, w, (ox + gx) * scale, (oy + gy) * scale, scale, fg8);
                }
            }
        }
    }

    Raster {
        w: w as i32,
        h: h as i32,
        px,
    }
}

/// One scaled font pixel: a `scale`x`scale` block of solid colour.
fn blit(px: &mut [u8], w: usize, x: usize, y: usize, scale: usize, color: [u8; 4]) {
    for dy in 0..scale {
        let row = (y + dy) * w;
        for dx in 0..scale {
            let i = (row + x + dx) * 4;
            px[i..i + 4].copy_from_slice(&color);
        }
    }
}

/// Straight float RGBA to premultiplied bytes, which is what the GL texture
/// and every `TextureRenderElement` downstream of it expect.
fn premul(c: Rgba) -> [u8; 4] {
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

    #[test]
    fn every_printable_ascii_has_a_glyph_and_rasterises() {
        let line: String = (FIRST..=LAST).map(|b| b as char).collect();
        let lines = wrap(&sanitize(&line), 32);
        let r = rasterize(&lines, 2, [1.0, 1.0, 1.0, 1.0], [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(r.px.len(), (r.w * r.h * 4) as usize);
        // Space is blank by design; everything else must put ink down.
        for b in FIRST + 1..=LAST {
            let r = rasterize(&[(b as char).to_string()], 1, [1.0, 1.0, 1.0, 1.0], [0.0; 4]);
            assert!(
                r.px.chunks_exact(4).any(|p| p[3] != 0),
                "no ink for {:?}",
                b as char
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
