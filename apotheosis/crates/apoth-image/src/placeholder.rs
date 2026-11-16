// SPDX-License-Identifier: AGPL-3.0-only
//! Kitty Unicode placeholders (T-15 §1, §6; GRAPHICS-10, RENDERING-35).
//!
//! A cell holding U+10EEEE draws one cell of a *virtual placement* (`a=p,U=1`). Its
//! combining marks give the image row, column and the most significant byte of the image
//! id, each as an index into [`DIACRITICS`]; the foreground colour carries the low 24 bits
//! of the id (8 bits from a palette index) and the underline colour the placement id.
//! Missing marks are inferred from the cell to the left; adjacent cells with consecutive
//! coordinates merge into one rectangle so each run is one quad.
//!
//! [`DIACRITICS`] is derived from the Unicode Character Database 6.0.0
//! (`UnicodeData.txt`) by the rule the protocol publishes: general category `Mn`,
//! canonical combining class 230, no decomposition mapping of its own, and not part of any
//! canonical decomposition mapping; ascending code point order. It is 297 entries long and
//! value `n` is entry `n`. Regenerate (numeric sort of the output) with:
//! `awk -F';' '{if($6!~/^</){n=split($6,a," ");for(i=1;i<=n;i++)u[a[i]]=1}
//! if($3=="Mn"&&$4=="230"&&$6=="")c[NR]=$1} END{for(k in c)if(!(c[k] in u))print c[k]}'`.

/// The placeholder character.
pub const PLACEHOLDER: u32 = 0x10EEEE;

/// Row/column/id-byte diacritics; the index is the encoded value.
pub static DIACRITICS: [u32; 297] = [
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A, 0x034B, 0x034C,
    0x0350, 0x0351, 0x0352, 0x0357, 0x035B, 0x0363, 0x0364, 0x0365, 0x0366, 0x0367, 0x0368, 0x0369,
    0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F, 0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592,
    0x0593, 0x0594, 0x0595, 0x0597, 0x0598, 0x0599, 0x059C, 0x059D, 0x059E, 0x059F, 0x05A0, 0x05A1,
    0x05A8, 0x05A9, 0x05AB, 0x05AC, 0x05AF, 0x05C4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065A, 0x065B, 0x065D, 0x065E, 0x06D6, 0x06D7, 0x06D8,
    0x06D9, 0x06DA, 0x06DB, 0x06DC, 0x06DF, 0x06E0, 0x06E1, 0x06E2, 0x06E4, 0x06E7, 0x06E8, 0x06EB,
    0x06EC, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736, 0x073A, 0x073D, 0x073F, 0x0740, 0x0741, 0x0743,
    0x0745, 0x0747, 0x0749, 0x074A, 0x07EB, 0x07EC, 0x07ED, 0x07EE, 0x07EF, 0x07F0, 0x07F1, 0x07F3,
    0x0816, 0x0817, 0x0818, 0x0819, 0x081B, 0x081C, 0x081D, 0x081E, 0x081F, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082A, 0x082B, 0x082C, 0x082D, 0x0951, 0x0953, 0x0954,
    0x0F82, 0x0F83, 0x0F86, 0x0F87, 0x135D, 0x135E, 0x135F, 0x17DD, 0x193A, 0x1A17, 0x1A75, 0x1A76,
    0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C, 0x1B6B, 0x1B6D, 0x1B6E, 0x1B6F, 0x1B70, 0x1B71,
    0x1B72, 0x1B73, 0x1CD0, 0x1CD1, 0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1, 0x1DC3, 0x1DC4,
    0x1DC5, 0x1DC6, 0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3, 0x1DD4, 0x1DD5,
    0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF, 0x1DE0, 0x1DE1,
    0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1, 0x20D4, 0x20D5, 0x20D6, 0x20D7,
    0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0, 0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0, 0x2DE1, 0x2DE2,
    0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6, 0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA, 0x2DEB, 0x2DEC, 0x2DED, 0x2DEE,
    0x2DEF, 0x2DF0, 0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8, 0x2DF9, 0x2DFA,
    0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1, 0xA8E0, 0xA8E1,
    0xA8E2, 0xA8E3, 0xA8E4, 0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9, 0xA8EA, 0xA8EB, 0xA8EC, 0xA8ED,
    0xA8EE, 0xA8EF, 0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2, 0xAAB3, 0xAAB7, 0xAAB8, 0xAABE, 0xAABF, 0xAAC1,
    0xFE20, 0xFE21, 0xFE22, 0xFE23, 0xFE24, 0xFE25, 0xFE26, 0x10A0F, 0x10A38, 0x1D185, 0x1D186,
    0x1D187, 0x1D188, 0x1D189, 0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD, 0x1D242, 0x1D243, 0x1D244,
];

/// The value a diacritic encodes, or `None` for any other code point.
pub fn diacritic_value(cp: u32) -> Option<u32> {
    DIACRITICS.binary_search(&cp).ok().map(|i| i as u32)
}

/// The marks after the placeholder: row, column, id high byte (each optional).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Marks {
    pub row: Option<u32>,
    pub col: Option<u32>,
    pub msb: Option<u32>,
}

/// Decode the combining marks that follow U+10EEEE (the cluster without its base). A mark
/// outside the table makes the cell invalid; marks past the third are ignored.
pub fn marks(tail: &[u32]) -> Option<Marks> {
    let mut m = Marks::default();
    for (i, &cp) in tail.iter().enumerate() {
        let v = diacritic_value(cp)?;
        match i {
            0 => m.row = Some(v),
            1 => m.col = Some(v),
            2 => m.msb = Some(v.min(255)),
            _ => break,
        }
    }
    Some(m)
}

/// One resolved placeholder cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    /// Full image id (low bits from the foreground colour, high byte from the third mark).
    pub image: u32,
    /// Placement id from the underline colour; 0 = any virtual placement of the image.
    pub placement: u32,
    /// Image cell row and column.
    pub row: u32,
    pub col: u32,
}

/// Left-to-right resolver for one screen row: applies the protocol's inference from the
/// previous placeholder cell. Values with nothing to infer from default to 0 (spec gap,
/// decided here: a bare U+10EEEE run starts at row 0, column 0, high byte 0).
#[derive(Clone, Copy, Debug, Default)]
pub struct Resolver {
    prev: Option<(u32, u32, u32, u32, u32)>, // (low id, placement, row, col, msb) of the cell to the left
}

impl Resolver {
    /// A new row (or any non-placeholder cell): nothing to infer from.
    pub fn reset(&mut self) {
        self.prev = None;
    }

    /// Resolve a placeholder cell. `low` is the id from the foreground colour (`None` when
    /// the foreground is the default colour: no image), `placement` from the underline.
    pub fn resolve(&mut self, m: Marks, low: Option<u32>, placement: u32) -> Option<Cell> {
        let Some(low) = low else {
            self.prev = None;
            return None;
        };
        let same = self.prev.filter(|p| p.0 == low && p.1 == placement);
        let (row, col, msb) = match (m.row, m.col, m.msb, same) {
            (None, None, None, Some(p)) => (p.2, p.3 + 1, p.4),
            (Some(r), None, None, Some(p)) if p.2 == r => (r, p.3 + 1, p.4),
            (Some(r), Some(c), None, Some(p)) if p.2 == r && p.3 + 1 == c => (r, c, p.4),
            (r, c, b, _) => (r.unwrap_or(0), c.unwrap_or(0), b.unwrap_or(0)),
        };
        self.prev = Some((low, placement, row, col, msb));
        let image = (low & 0x00ff_ffff) | (msb << 24);
        (image != 0).then_some(Cell {
            image,
            placement,
            row,
            col,
        })
    }
}

/// A rectangle of placeholder cells on screen: `x, y` (screen cell), `w, h` (cells), the
/// image cell `row, col` of its top-left, and the ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    pub image: u32,
    pub placement: u32,
    pub row: u32,
    pub col: u32,
}

/// Builds merged rectangles: cells pushed in row-major order extend the current horizontal
/// run when they continue it; finished runs merge downward into a rectangle whose bottom
/// row they continue (same span, ids and column, next image row).
#[derive(Debug, Default)]
pub struct Merger {
    out: Vec<Rect>,
    /// Rectangles that ended on the previous screen row (candidates for a downward merge).
    open: Vec<usize>,
    /// Rectangles that ended on the current screen row.
    next_open: Vec<usize>,
    run: Option<Rect>,
    y: usize,
}

impl Merger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start screen row `y` (rows must come in increasing order).
    pub fn row(&mut self, y: usize) {
        self.flush_run();
        if y != self.y + 1 {
            self.next_open.clear();
        }
        self.open = std::mem::take(&mut self.next_open);
        self.y = y;
    }

    /// A resolved cell at screen column `x` of the current row.
    pub fn cell(&mut self, x: usize, c: Cell) {
        if let Some(r) = self.run.as_mut()
            && r.x + r.w == x
            && r.image == c.image
            && r.placement == c.placement
            && r.row == c.row
            && r.col + r.w as u32 == c.col
        {
            r.w += 1;
            return;
        }
        self.flush_run();
        self.run = Some(Rect {
            x,
            y: self.y,
            w: 1,
            h: 1,
            image: c.image,
            placement: c.placement,
            row: c.row,
            col: c.col,
        });
    }

    /// A non-placeholder cell: the run ends.
    pub fn gap(&mut self) {
        self.flush_run();
    }

    fn flush_run(&mut self) {
        let Some(r) = self.run.take() else { return };
        let below = self.open.iter().position(|&i| {
            let o = &self.out[i];
            o.x == r.x
                && o.w == r.w
                && o.y + o.h == r.y
                && o.image == r.image
                && o.placement == r.placement
                && o.col == r.col
                && o.row + o.h as u32 == r.row
        });
        let idx = match below {
            Some(k) => {
                let i = self.open.swap_remove(k);
                self.out[i].h += 1;
                i
            }
            None => {
                self.out.push(r);
                self.out.len() - 1
            }
        };
        self.next_open.push(idx);
    }

    /// The merged rectangles.
    pub fn finish(mut self) -> Vec<Rect> {
        self.flush_run();
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_and_starts_and_ends_as_published() {
        assert!(DIACRITICS.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(&DIACRITICS[..5], &[0x0305, 0x030D, 0x030E, 0x0310, 0x0312]);
        assert_eq!(DIACRITICS[296], 0x1D244);
        assert_eq!(diacritic_value(0x030D), Some(1));
        assert_eq!(diacritic_value(0x0300), None);
    }

    #[test]
    fn missing_marks_continue_the_left_neighbour() {
        let mut r = Resolver::default();
        let d = |v: usize| DIACRITICS[v];
        let a = r
            .resolve(marks(&[d(2), d(5), d(1)]).unwrap(), Some(42), 0)
            .unwrap();
        assert_eq!((a.image, a.row, a.col), (42 | 1 << 24, 2, 5));
        let b = r.resolve(Marks::default(), Some(42), 0).unwrap();
        assert_eq!((b.image, b.row, b.col), (42 | 1 << 24, 2, 6));
        let c = r.resolve(marks(&[d(2)]).unwrap(), Some(42), 0).unwrap();
        assert_eq!((c.image, c.col), (42 | 1 << 24, 7));
        // A different row restarts: no inference, high byte 0.
        let e = r.resolve(marks(&[d(3)]).unwrap(), Some(42), 0).unwrap();
        assert_eq!((e.image, e.row, e.col), (42, 3, 0));
        // Default foreground: no image.
        assert_eq!(r.resolve(Marks::default(), None, 0), None);
        assert_eq!(marks(&[0x0300]), None);
    }

    #[test]
    fn consecutive_cells_merge_into_one_rectangle() {
        let mut m = Merger::new();
        for y in 0..2 {
            m.row(y);
            for x in 0..3 {
                m.cell(
                    x + 1,
                    Cell {
                        image: 9,
                        placement: 0,
                        row: y as u32,
                        col: x as u32,
                    },
                );
            }
            m.gap();
        }
        let rects = m.finish();
        assert_eq!(rects.len(), 1, "{rects:?}");
        assert_eq!(
            (rects[0].x, rects[0].y, rects[0].w, rects[0].h),
            (1, 0, 3, 2)
        );
        // A break in the column sequence splits the run.
        let mut m = Merger::new();
        m.row(0);
        for (x, col) in [(0, 0), (1, 1), (2, 5)] {
            m.cell(
                x,
                Cell {
                    image: 9,
                    placement: 0,
                    row: 0,
                    col,
                },
            );
        }
        assert_eq!(m.finish().len(), 2);
    }
}
