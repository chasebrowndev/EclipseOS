// SPDX-License-Identifier: AGPL-3.0-only
//! The trusted UI's faces, pre-rasterised (ADR 0009 and its amendment): JetBrains
//! Mono for anything a human must read character by character, and Onest ([`sans`])
//! for the compiled-in prose around it.
//!
//! abyss links no font library. `assets/fonts/trusted/gen_glyphs.py`
//! rasterised the face once, offline, into the coverage maps beside it,
//! which are checked in: one
//! byte of coverage per device pixel, printable ASCII only, Regular then
//! Medium, at 1x, 2x and 3x. Nothing here parses a font, so caller text can
//! choose only which of 190 fixed cells is copied.
//!
//! Not the HUD's atlases ([`crate::hud_font`]): those round the 2x advance
//! to 15 px, and the trusted panels lay out on an integer grid at every
//! scale, so an `s`x cell here is exactly `s` times the 1x one.
//!
//! JetBrains Mono, Copyright 2020 The JetBrains Mono Project Authors,
//! SIL Open Font License 1.1 (`assets/fonts/OFL-JetBrainsMono.txt`).

/// Advance and cell width, logical px. The face is monospace, so layout is a
/// column count times this.
pub const ADVANCE: usize = 8;
/// Cell height, logical px, ascent and descent included.
pub const CELL_H: usize = 16;
/// Vertical step between rows of text.
pub const LINE_H: usize = 20;

const FIRST: u8 = 0x20;
const LAST: u8 = 0x7E;
const COUNT: usize = (LAST - FIRST + 1) as usize;

static X1: &[u8] = include_bytes!("../../../../assets/fonts/trusted/jbm-1x.bin");
static X2: &[u8] = include_bytes!("../../../../assets/fonts/trusted/jbm-2x.bin");
static X3: &[u8] = include_bytes!("../../../../assets/fonts/trusted/jbm-3x.bin");

/// The largest scale with its own cells. Above it the canvas draws at this
/// scale and resamples.
pub const MAX_SCALE: usize = 3;

/// The coverage cell for `ch` at `scale` (1..=[`MAX_SCALE`]), `8s x 16s`
/// bytes, or `None` for a byte with no glyph.
pub fn cell(ch: u8, medium: bool, scale: usize) -> Option<&'static [u8]> {
    if !(FIRST..=LAST).contains(&ch) {
        return None;
    }
    let atlas = match scale {
        1 => X1,
        2 => X2,
        3 => X3,
        _ => return None,
    };
    let size = ADVANCE * scale * CELL_H * scale;
    let i = usize::from(medium) * COUNT + usize::from(ch - FIRST);
    atlas.get(i * size..(i + 1) * size)
}

/// Onest, proportional, printable ASCII, for the compiled-in text of a trusted
/// panel: headings, warnings, body, fact labels and button labels. Never for
/// runtime text. A request's command, path, fact value or the personal secret
/// stays in the fixed cells above, where `l`, `I`, `1` and `|` cannot be
/// confused by width alone.
///
/// The exact-grid guarantee holds here too: every advance is a whole number of
/// logical pixels, the same at every scale, and a glyph's cell at scale `s` is
/// the 1x cell's width times `s` (`assets/fonts/trusted/gen_sans.c`, format
/// `TSN1`). Layout is integer sums of advances, so it is the same at 1x, 2x
/// and 3x. Onest, Copyright 2021 The Onest Project Authors, SIL OFL 1.1
/// (`assets/fonts/OFL-Onest.txt`).
pub mod sans {
    use super::{CELL_H, COUNT, FIRST, LAST};

    /// Side margin of every cell, logical px: overhang is kept, not clipped.
    pub const PADX: usize = 2;

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Weight {
        Regular = 0,
        Medium = 1,
        SemiBold = 2,
    }

    static X1: &[u8] = include_bytes!("../../../../assets/fonts/trusted/sans-1x.bin");
    static X2: &[u8] = include_bytes!("../../../../assets/fonts/trusted/sans-2x.bin");
    static X3: &[u8] = include_bytes!("../../../../assets/fonts/trusted/sans-3x.bin");

    const MAGIC: &[u8; 4] = b"TSN1";
    const HEAD: usize = 10;
    const WEIGHTS: usize = 3;
    const ADV_AT: usize = HEAD;
    const CELLS_AT: usize = HEAD + WEIGHTS * COUNT;

    fn atlas(scale: usize) -> Option<&'static [u8]> {
        let a = match scale {
            1 => X1,
            2 => X2,
            3 => X3,
            _ => return None,
        };
        a.starts_with(MAGIC).then_some(a)
    }

    fn index(ch: u8, w: Weight) -> Option<usize> {
        (FIRST..=LAST)
            .contains(&ch)
            .then(|| w as usize * COUNT + usize::from(ch - FIRST))
    }

    /// The advance of `ch`, logical px, or `None` for a byte with no glyph.
    pub fn advance(ch: u8, w: Weight) -> Option<usize> {
        let i = index(ch, w)?;
        X1.get(ADV_AT + i).map(|&a| usize::from(a))
    }

    /// The width of `s`, logical px. A byte with no glyph counts as nothing,
    /// as it draws nothing.
    pub fn width(s: &str, w: Weight) -> usize {
        s.bytes().filter_map(|b| advance(b, w)).sum()
    }

    /// The coverage cell for `ch` at `scale` (1..=[`MAX_SCALE`]): its bytes and
    /// its width in device px (`(advance + 2 * PADX) * scale`), height
    /// `CELL_H * scale`. The pen origin is `PADX * scale` into the cell.
    pub fn cell(ch: u8, w: Weight, scale: usize) -> Option<(&'static [u8], usize)> {
        let i = index(ch, w)?;
        let a = atlas(scale)?;
        let size = |g: usize| (usize::from(X1[ADV_AT + g]) + 2 * PADX) * scale * CELL_H * scale;
        let off = CELLS_AT + (0..i).map(size).sum::<usize>();
        let cw = (usize::from(X1[ADV_AT + i]) + 2 * PADX) * scale;
        a.get(off..off + size(i)).map(|c| (c, cw))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::glyphs::MAX_SCALE;

        const ALL: [Weight; 3] = [Weight::Regular, Weight::Medium, Weight::SemiBold];

        #[test]
        fn every_atlas_is_the_size_its_advances_say() {
            for s in 1..=MAX_SCALE {
                let a = atlas(s).unwrap();
                assert_eq!(&a[..4], MAGIC);
                assert_eq!(usize::from(a[HEAD - 1]), s);
                let cells: usize = (0..WEIGHTS * COUNT)
                    .map(|g| (usize::from(X1[ADV_AT + g]) + 2 * PADX) * s * CELL_H * s)
                    .sum();
                assert_eq!(a.len(), CELLS_AT + cells, "scale {s}");
            }
        }

        #[test]
        fn the_advances_are_the_same_at_every_scale() {
            assert_eq!(&X1[ADV_AT..CELLS_AT], &X2[ADV_AT..CELLS_AT]);
            assert_eq!(&X1[ADV_AT..CELLS_AT], &X3[ADV_AT..CELLS_AT]);
        }

        #[test]
        fn digits_are_tabular_in_every_weight() {
            for w in ALL {
                let d = advance(b'0', w).unwrap();
                assert!((b'1'..=b'9').all(|c| advance(c, w) == Some(d)), "{w:?}");
            }
        }

        #[test]
        fn space_is_blank_and_letters_are_not() {
            for s in 1..=MAX_SCALE {
                assert!(cell(b' ', Weight::Regular, s).unwrap().0.iter().all(|&c| c == 0));
                assert!(cell(b'M', Weight::Medium, s).unwrap().0.iter().any(|&c| c > 200));
            }
        }

        #[test]
        fn a_cell_is_its_advance_plus_margins_times_the_scale() {
            for s in 1..=MAX_SCALE {
                let (bytes, cw) = cell(b'W', Weight::SemiBold, s).unwrap();
                assert_eq!(cw, (advance(b'W', Weight::SemiBold).unwrap() + 2 * PADX) * s);
                assert_eq!(bytes.len(), cw * CELL_H * s);
            }
        }

        #[test]
        fn bytes_outside_the_face_have_no_cell() {
            assert!(cell(0x1F, Weight::Regular, 1).is_none());
            assert!(cell(0x7F, Weight::Regular, 1).is_none());
            assert!(cell(b'a', Weight::Regular, 4).is_none());
            assert_eq!(width("a\u{7f}b", Weight::Regular), width("ab", Weight::Regular));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_atlas_holds_both_weights_of_printable_ascii() {
        for (s, a) in [(1, X1), (2, X2), (3, X3)] {
            assert_eq!(a.len(), 2 * COUNT * ADVANCE * s * CELL_H * s);
        }
    }

    #[test]
    fn space_is_blank_and_letters_are_not() {
        for s in 1..=MAX_SCALE {
            assert!(cell(b' ', false, s).unwrap().iter().all(|&c| c == 0));
            assert!(cell(b'M', true, s).unwrap().iter().any(|&c| c > 200));
        }
    }

    #[test]
    fn bytes_outside_the_face_have_no_cell() {
        assert!(cell(0x1F, false, 1).is_none());
        assert!(cell(0x7F, false, 1).is_none());
        assert!(cell(b'a', false, 4).is_none());
    }
}
