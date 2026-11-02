// SPDX-License-Identifier: AGPL-3.0-only
//! The annotation HUD's typeface: JetBrains Mono, pre-rasterised (ADR 0071).
//!
//! The compositor carries no font engine and must not grow one at runtime
//! (ADR 0009), so the face is rasterised offline by
//! `assets/fonts/hud/gen_atlas.c` into 8-bit coverage atlases and compiled in
//! here. Three faces -- title, body, readout -- each at 1x and 2x. A
//! fractional output scale takes the nearer of the two, and the GPU scales
//! that last fraction; there is no third atlas and no smearing hack.
//!
//! Each atlas carries printable ASCII, the two compositor glyphs, and the
//! [`EXTENDED`] set (Latin-1, Latin Extended-A, Greek, math and punctuation)
//! so model replies in those scripts can be drawn. The extended set is
//! annotation-only; see [`crate::text::sanitize_annotation`].
//!
//! This is a separate set from [`crate::glyphs`], the trusted UI's, which
//! keeps an exact integer grid at every scale and stays ASCII-only.
//!
//! Layout is always done in 1x logical metrics, which every scale shares:
//! a column is [`ADVANCE`] logical px wide whatever atlas draws it, so a
//! panel is the same logical size at 1x and 2x and placement never depends
//! on the scale.

/// One atlas file: an 8-byte `HUD2` header, then [`CELLS`] coverage cells.
#[derive(Debug, Clone, Copy)]
pub struct Atlas {
    pub cell_w: usize,
    pub cell_h: usize,
    /// Pen advance, pixels.
    pub advance: usize,
    /// Overhang room left of the pen position inside each cell, pixels.
    pub margin: usize,
    /// Baseline, pixels from the cell's top row (the generator's
    /// `ceil(ascender / em * px)`; not stored in the file).
    pub ascent: usize,
    data: &'static [u8],
}

/// Cells before the extended set: printable ASCII (95), then U+00D7 and
/// U+00B7.
const BASE: usize = 97;
const CELLS: usize = BASE + EXTENDED.len();
const HEADER: usize = 8;

/// Codepoints of cells [`BASE`].. in file order, ascending. Must match the
/// `EXTENDED` table in `assets/fonts/hud/gen_atlas.c` exactly; the atlas
/// length assert below catches a count mismatch at build time.
pub const EXTENDED: &[u32] = &[
    0x00a0, 0x00a1, 0x00a2, 0x00a3, 0x00a4, 0x00a5, 0x00a6, 0x00a7, 0x00a8, 0x00a9, 0x00aa, 0x00ab, 0x00ac,
    0x00ae, 0x00af, 0x00b0, 0x00b1, 0x00b2, 0x00b3, 0x00b4, 0x00b5, 0x00b6, 0x00b8, 0x00b9, 0x00ba, 0x00bb,
    0x00bc, 0x00bd, 0x00be, 0x00bf, 0x00c0, 0x00c1, 0x00c2, 0x00c3, 0x00c4, 0x00c5, 0x00c6, 0x00c7, 0x00c8,
    0x00c9, 0x00ca, 0x00cb, 0x00cc, 0x00cd, 0x00ce, 0x00cf, 0x00d0, 0x00d1, 0x00d2, 0x00d3, 0x00d4, 0x00d5,
    0x00d6, 0x00d8, 0x00d9, 0x00da, 0x00db, 0x00dc, 0x00dd, 0x00de, 0x00df, 0x00e0, 0x00e1, 0x00e2, 0x00e3,
    0x00e4, 0x00e5, 0x00e6, 0x00e7, 0x00e8, 0x00e9, 0x00ea, 0x00eb, 0x00ec, 0x00ed, 0x00ee, 0x00ef, 0x00f0,
    0x00f1, 0x00f2, 0x00f3, 0x00f4, 0x00f5, 0x00f6, 0x00f7, 0x00f8, 0x00f9, 0x00fa, 0x00fb, 0x00fc, 0x00fd,
    0x00fe, 0x00ff, 0x0100, 0x0101, 0x0102, 0x0103, 0x0104, 0x0105, 0x0106, 0x0107, 0x0108, 0x0109, 0x010a,
    0x010b, 0x010c, 0x010d, 0x010e, 0x010f, 0x0110, 0x0111, 0x0112, 0x0113, 0x0114, 0x0115, 0x0116, 0x0117,
    0x0118, 0x0119, 0x011a, 0x011b, 0x011c, 0x011d, 0x011e, 0x011f, 0x0120, 0x0121, 0x0122, 0x0123, 0x0124,
    0x0125, 0x0126, 0x0127, 0x0128, 0x0129, 0x012a, 0x012b, 0x012c, 0x012d, 0x012e, 0x012f, 0x0130, 0x0131,
    0x0134, 0x0135, 0x0136, 0x0137, 0x0138, 0x0139, 0x013a, 0x013b, 0x013c, 0x013d, 0x013e, 0x013f, 0x0140,
    0x0141, 0x0142, 0x0143, 0x0144, 0x0145, 0x0146, 0x0147, 0x0148, 0x0149, 0x014a, 0x014b, 0x014c, 0x014d,
    0x014e, 0x014f, 0x0150, 0x0151, 0x0152, 0x0153, 0x0154, 0x0155, 0x0156, 0x0157, 0x0158, 0x0159, 0x015a,
    0x015b, 0x015c, 0x015d, 0x015e, 0x015f, 0x0160, 0x0161, 0x0162, 0x0163, 0x0164, 0x0165, 0x0166, 0x0167,
    0x0168, 0x0169, 0x016a, 0x016b, 0x016c, 0x016d, 0x016e, 0x016f, 0x0170, 0x0171, 0x0172, 0x0173, 0x0174,
    0x0175, 0x0176, 0x0177, 0x0178, 0x0179, 0x017a, 0x017b, 0x017c, 0x017d, 0x017e, 0x017f, 0x0391, 0x0392,
    0x0393, 0x0394, 0x0395, 0x0396, 0x0397, 0x0398, 0x0399, 0x039a, 0x039b, 0x039c, 0x039d, 0x039e, 0x039f,
    0x03a0, 0x03a1, 0x03a3, 0x03a4, 0x03a5, 0x03a6, 0x03a7, 0x03a8, 0x03a9, 0x03b1, 0x03b2, 0x03b3, 0x03b4,
    0x03b5, 0x03b6, 0x03b7, 0x03b8, 0x03b9, 0x03ba, 0x03bb, 0x03bc, 0x03bd, 0x03be, 0x03bf, 0x03c0, 0x03c1,
    0x03c2, 0x03c3, 0x03c4, 0x03c5, 0x03c6, 0x03c7, 0x03c8, 0x03c9, 0x2013, 0x2014, 0x2018, 0x2019, 0x201c,
    0x201d, 0x2022, 0x2026, 0x2070, 0x2074, 0x2075, 0x2076, 0x2077, 0x2078, 0x2079, 0x2080, 0x2081, 0x2082,
    0x2083, 0x2084, 0x2085, 0x2086, 0x2087, 0x2088, 0x2089, 0x20ac, 0x2190, 0x2191, 0x2192, 0x2193, 0x2194,
    0x2195, 0x2200, 0x2202, 0x2203, 0x2206, 0x2207, 0x2208, 0x2211, 0x2212, 0x221a, 0x221e, 0x2227, 0x2228,
    0x2229, 0x222a, 0x222b, 0x2248, 0x2260, 0x2261, 0x2264, 0x2265, 0x2282, 0x2283,
];

impl Atlas {
    /// Read the header at compile time; a malformed file fails the build.
    const fn new(data: &'static [u8], ascent: usize) -> Atlas {
        assert!(data.len() > HEADER);
        assert!(data[0] == b'H' && data[1] == b'U' && data[2] == b'D' && data[3] == b'2');
        let (cell_w, cell_h) = (data[4] as usize, data[5] as usize);
        assert!(data.len() == HEADER + CELLS * cell_w * cell_h);
        assert!(ascent < cell_h);
        Atlas {
            cell_w,
            cell_h,
            advance: data[6] as usize,
            margin: data[7] as usize,
            ascent,
            data,
        }
    }

    /// The coverage cell for `c`. Anything the atlas does not carry draws as
    /// `?`. Callers' text has already been reduced by
    /// `text::sanitize_annotation`, which keeps exactly the set [`covers`]
    /// accepts, so the fallback only shows for compositor strings that
    /// strayed outside it.
    pub fn cell(&self, c: char) -> &'static [u8] {
        let i = index_of(c).unwrap_or('?' as usize - 0x20);
        let n = self.cell_w * self.cell_h;
        let data: &'static [u8] = self.data;
        &data[HEADER + i * n..HEADER + (i + 1) * n]
    }
}

/// The cell index for `c`, or `None` if the atlas does not carry it. A
/// binary search over [`EXTENDED`]; no allocation.
pub fn index_of(c: char) -> Option<usize> {
    match c {
        ' '..='~' => Some(c as usize - 0x20),
        '\u{d7}' => Some(95),
        '\u{b7}' => Some(96),
        _ => EXTENDED.binary_search(&(c as u32)).ok().map(|i| BASE + i),
    }
}

/// Whether the annotation atlases draw `c` as itself.
pub fn covers(c: char) -> bool {
    index_of(c).is_some()
}

/// Which of the three faces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Face {
    /// JetBrains Mono Medium 13px, emboldened: panel titles, the pick letter.
    Title,
    /// JetBrains Mono Regular 12.5px: panel body, the selector hint.
    Body,
    /// JetBrains Mono Medium 11px: the selector's size readout.
    Readout,
}

static TITLE: [Atlas; 2] = [
    Atlas::new(include_bytes!("../../../../assets/fonts/hud/title-1x.a8"), 14),
    Atlas::new(include_bytes!("../../../../assets/fonts/hud/title-2x.a8"), 27),
];
static BODY: [Atlas; 2] = [
    Atlas::new(include_bytes!("../../../../assets/fonts/hud/body-1x.a8"), 13),
    Atlas::new(include_bytes!("../../../../assets/fonts/hud/body-2x.a8"), 26),
];
static READOUT: [Atlas; 2] = [
    Atlas::new(include_bytes!("../../../../assets/fonts/hud/readout-1x.a8"), 12),
    Atlas::new(include_bytes!("../../../../assets/fonts/hud/readout-2x.a8"), 23),
];

impl Face {
    fn set(self) -> &'static [Atlas; 2] {
        match self {
            Face::Title => &TITLE,
            Face::Body => &BODY,
            Face::Readout => &READOUT,
        }
    }

    /// The atlas for raster scale `dev` (1 or 2; anything above 2 uses 2x).
    pub fn atlas(self, dev: usize) -> &'static Atlas {
        &self.set()[usize::from(dev >= 2)]
    }

    /// Logical column width: the 1x advance.
    pub fn advance(self) -> usize {
        self.set()[0].advance
    }

    /// Logical line box: the 1x cell height.
    pub fn line_h(self) -> usize {
        self.set()[0].cell_h
    }

    /// Logical baseline within the line box.
    pub fn ascent(self) -> usize {
        self.set()[0].ascent
    }

    /// Logical width of `s` set in this face.
    pub fn width(self, s: &str) -> usize {
        s.chars().count() * self.advance()
    }
}

/// The column width every layout counts in. Title and body share it, so a
/// panel can wrap both at one column count.
pub const ADVANCE: usize = 8;

/// The raster scale for an output's fractional scale: the nearer atlas.
pub fn raster_scale(fractional: f64) -> usize {
    if fractional >= 1.5 {
        2
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_atlases_parse_and_agree_on_logical_metrics() {
        for face in [Face::Title, Face::Body, Face::Readout] {
            let [one, two] = face.set();
            assert_eq!(one.cell_w, one.advance + 2 * one.margin);
            assert_eq!(two.cell_w, two.advance + 2 * two.margin);
            // 2x is twice 1x to within the generator's whole-pixel rounding
            // (the line box rounds twice, so it may be off by two). Layout
            // uses 1x metrics only, so this is a sanity bound, not a contract.
            assert!(two.advance.abs_diff(one.advance * 2) <= 1, "{face:?}");
            assert!(two.cell_h.abs_diff(one.cell_h * 2) <= 2, "{face:?}");
        }
        assert_eq!(Face::Title.advance(), ADVANCE);
        assert_eq!(Face::Body.advance(), ADVANCE);
    }

    #[test]
    fn every_printable_glyph_has_ink_and_space_has_none() {
        for face in [Face::Title, Face::Body, Face::Readout] {
            for dev in [1, 2] {
                let a = face.atlas(dev);
                assert!(a.cell(' ').iter().all(|&b| b == 0));
                // U+00A0 is a blank space by design; everything else inks.
                let extended = EXTENDED
                    .iter()
                    .filter(|&&c| c != 0xa0)
                    .map(|&c| char::from_u32(c).unwrap());
                for c in ('!'..='~').chain(['\u{d7}', '\u{b7}']).chain(extended) {
                    assert!(a.cell(c).iter().any(|&b| b > 0), "{face:?} {dev}x {c:?}");
                }
                // Outside the atlas is the replacement glyph, not a panic.
                assert!(!covers('\u{4e2d}'));
                assert_eq!(a.cell('\u{4e2d}'), a.cell('?'));
            }
        }
    }

    #[test]
    fn the_extended_set_is_sorted_unique_and_disjoint_from_the_base() {
        assert!(EXTENDED.windows(2).all(|w| w[0] < w[1]));
        assert!(EXTENDED.iter().all(|&c| c > 0x7e && c != 0xd7 && c != 0xb7));
        assert_eq!(CELLS, 97 + EXTENDED.len());
    }

    #[test]
    fn greek_latin_and_math_each_get_their_own_cell() {
        let mut seen = std::collections::HashSet::new();
        for &c in EXTENDED {
            let ch = char::from_u32(c).unwrap();
            assert!(covers(ch), "{c:04X}");
            assert!(seen.insert(index_of(ch).unwrap()), "{c:04X}");
        }
        assert!(covers('\u{3a3}') && covers('\u{3c0}') && covers('\u{2264}') && covers('\u{e9}'));
        assert!(!covers('\u{1f480}'));
    }

    #[test]
    fn the_nearer_atlas_wins() {
        assert_eq!(raster_scale(1.0), 1);
        assert_eq!(raster_scale(1.25), 1);
        assert_eq!(raster_scale(1.5), 2);
        assert_eq!(raster_scale(2.0), 2);
        assert_eq!(raster_scale(3.0), 2);
    }
}
