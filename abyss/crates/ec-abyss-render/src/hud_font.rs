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
//! This is a separate set from [`crate::glyphs`], the trusted UI's, which
//! keeps an exact integer grid at every scale.
//!
//! Layout is always done in 1x logical metrics, which every scale shares:
//! a column is [`ADVANCE`] logical px wide whatever atlas draws it, so a
//! panel is the same logical size at 1x and 2x and placement never depends
//! on the scale.

/// One atlas file: an 8-byte `HUD1` header, then [`CELLS`] coverage cells.
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

/// Printable ASCII, then U+00D7 and U+00B7.
const CELLS: usize = 97;
const HEADER: usize = 8;

impl Atlas {
    /// Read the header at compile time; a malformed file fails the build.
    const fn new(data: &'static [u8], ascent: usize) -> Atlas {
        assert!(data.len() > HEADER);
        assert!(data[0] == b'H' && data[1] == b'U' && data[2] == b'D' && data[3] == b'1');
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
    /// `?`: callers' text has already been reduced to printable ASCII by
    /// `text::sanitize`, so only compositor-owned strings reach the two
    /// extras.
    pub fn cell(&self, c: char) -> &'static [u8] {
        let i = match c {
            ' '..='~' => c as usize - 0x20,
            '\u{d7}' => 95,
            '\u{b7}' => 96,
            _ => '?' as usize - 0x20,
        };
        let n = self.cell_w * self.cell_h;
        let data: &'static [u8] = self.data;
        &data[HEADER + i * n..HEADER + (i + 1) * n]
    }
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
                for c in ('!'..='~').chain(['\u{d7}', '\u{b7}']) {
                    assert!(a.cell(c).iter().any(|&b| b > 0), "{face:?} {dev}x {c:?}");
                }
                // Anything else is the replacement glyph, not a panic.
                assert_eq!(a.cell('\u{e9}'), a.cell('?'));
            }
        }
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
