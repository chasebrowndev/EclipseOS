// SPDX-License-Identifier: AGPL-3.0-only
//! The trusted UI's face: JetBrains Mono, pre-rasterised (ADR 0009).
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
