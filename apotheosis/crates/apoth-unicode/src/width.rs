// SPDX-License-Identifier: Apache-2.0
//! Cell width of scalars and grapheme clusters (T-04 §3).
//!
//! Width is decided once, at print time, by the caller, and stored in the cell. Nothing
//! here is consulted by the renderer.

use crate::props::{EAST_ASIAN_AMBIGUOUS, EMOJI, WIDTH_MASK, class_of, props};
use crate::segment::Class;

/// The VS15 (text presentation) selector.
pub const VS15: char = '\u{FE0E}';
/// The VS16 (emoji presentation) selector.
pub const VS16: char = '\u{FE0F}';
/// Maximum scalars in one stored cluster (UNICODE-07).
pub const MAX_CLUSTER_SCALARS: usize = 64;

/// `unicode.ambiguous-width`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Ambiguous {
    /// East Asian Ambiguous characters take one cell (default).
    #[default]
    Narrow,
    /// East Asian Ambiguous characters take two cells.
    Wide,
}

/// `unicode.width-method`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WidthMethod {
    /// Width of the cluster from the table, with VS15/VS16 (default).
    #[default]
    Unicode,
    /// Legacy per-scalar sum.
    Wcswidth,
}

/// Why a set of cell-width overrides was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverrideError {
    /// A range has `lo > hi` or runs past U+10FFFF.
    BadRange,
    /// A width other than 1 or 2.
    BadWidth,
    /// Two ranges overlap.
    Overlap,
}

/// `font.cell-width-override`: sorted, non-overlapping scalar ranges forced to 1 or 2
/// cells (FONTS-07). The grid and the font layer build this from the same config rows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CellWidthOverrides {
    ranges: Vec<(u32, u32, u8)>,
}

impl CellWidthOverrides {
    /// No overrides.
    pub fn none() -> Self {
        Self::default()
    }

    /// Build from `(lo, hi, width)` triples (inclusive ranges, width 1 or 2).
    pub fn new(ranges: impl IntoIterator<Item = (u32, u32, u8)>) -> Result<Self, OverrideError> {
        let mut v: Vec<(u32, u32, u8)> = ranges.into_iter().collect();
        for &(lo, hi, w) in &v {
            if lo > hi || hi > 0x10_FFFF {
                return Err(OverrideError::BadRange);
            }
            if w != 1 && w != 2 {
                return Err(OverrideError::BadWidth);
            }
        }
        v.sort_unstable();
        if v.windows(2).any(|p| p[0].1 >= p[1].0) {
            return Err(OverrideError::Overlap);
        }
        Ok(CellWidthOverrides { ranges: v })
    }

    /// The forced width of `c`, if any.
    pub fn lookup(&self, c: char) -> Option<u8> {
        if self.ranges.is_empty() {
            return None;
        }
        let cp = c as u32;
        let i = self.ranges.partition_point(|r| r.1 < cp);
        match self.ranges.get(i) {
            Some(&(lo, _, w)) if lo <= cp => Some(w),
            _ => None,
        }
    }

    /// True when there are no ranges.
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }
}

/// The width policy in force for a pane.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Widths {
    /// `unicode.ambiguous-width`.
    pub ambiguous: Ambiguous,
    /// `unicode.width-method`; apoth-term switches it for mode 2027 under `wcswidth`.
    pub method: WidthMethod,
    /// `font.cell-width-override`.
    pub overrides: CellWidthOverrides,
}

impl Widths {
    /// Width of one scalar: override, else table width with ambiguity applied. Zero for
    /// controls, combining marks, default-ignorables and format characters.
    pub fn scalar_width(&self, c: char) -> u8 {
        if let Some(w) = self.overrides.lookup(c) {
            return w;
        }
        self.table_scalar_width(c)
    }

    fn table_scalar_width(&self, c: char) -> u8 {
        let p = props(c);
        let w = (p & WIDTH_MASK) as u8;
        if w == 1 && p & EAST_ASIAN_AMBIGUOUS != 0 && self.ambiguous == Ambiguous::Wide {
            2
        } else {
            w
        }
    }

    /// Width in cells of the cluster `s` under the method in force.
    pub fn cluster_width(&self, s: &str) -> u8 {
        let mut m = ClusterMeasure::new();
        for c in s.chars() {
            m.push(self, c);
        }
        m.width(self.method)
    }
}

/// Incremental width of the cluster being built. Push the scalars of one cluster, in order;
/// read [`ClusterMeasure::width`] after each push to learn the cells it now needs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClusterMeasure {
    started: bool,
    first_control: bool,
    first_forced: bool,
    first_emoji: bool,
    first_width: u8,
    vs15: bool,
    vs16: bool,
    modifier: bool,
    sum: u16,
}

impl ClusterMeasure {
    /// An empty measure.
    pub const fn new() -> Self {
        ClusterMeasure {
            started: false,
            first_control: false,
            first_forced: false,
            first_emoji: false,
            first_width: 0,
            vs15: false,
            vs16: false,
            modifier: false,
            sum: 0,
        }
    }

    /// Forget the cluster.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Account for the next scalar of the cluster.
    pub fn push(&mut self, w: &Widths, c: char) {
        let sw = w.scalar_width(c);
        self.sum = self.sum.saturating_add(u16::from(sw));
        if !self.started {
            self.started = true;
            self.first_control = matches!(class_of(c), Class::Cr | Class::Lf | Class::Control);
            self.first_forced = w.overrides.lookup(c).is_some();
            self.first_emoji = props(c) & EMOJI != 0;
            self.first_width = sw;
        } else if c == VS16 {
            self.vs16 = true;
        } else if c == VS15 {
            self.vs15 = true;
        } else if ('\u{1F3FB}'..='\u{1F3FF}').contains(&c) {
            // Fitzpatrick modifier: an emoji base plus modifier presents as emoji.
            self.modifier = true;
        }
    }

    /// Cells needed by the cluster so far. Zero for an empty measure or a cluster headed by
    /// a control; an orphan zero-width head (a combining mark that starts a cell) takes one
    /// cell. Under `Unicode` the result is at most 2.
    pub fn width(&self, method: WidthMethod) -> u8 {
        if !self.started || self.first_control {
            return 0;
        }
        match method {
            WidthMethod::Wcswidth => self.sum.min(255) as u8,
            WidthMethod::Unicode => {
                let mut w = self.first_width;
                if w == 0 {
                    w = 1;
                }
                if !self.first_forced && self.first_emoji {
                    if self.vs16 || self.modifier {
                        w = 2;
                    } else if self.vs15 {
                        w = 1;
                    }
                }
                w.min(2)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> u8 {
        Widths::default().cluster_width(s)
    }

    #[test]
    fn basics() {
        assert_eq!(w("a"), 1);
        assert_eq!(w("\u{4E2D}"), 2);
        assert_eq!(w("e\u{301}"), 1);
        assert_eq!(w("\u{301}"), 1);
        assert_eq!(w("\r"), 0);
        assert_eq!(w("\u{200B}"), 0);
    }

    #[test]
    fn variation_selectors() {
        assert_eq!(w("\u{2764}"), 1);
        assert_eq!(w("\u{2764}\u{FE0F}"), 2);
        assert_eq!(w("\u{1F600}\u{FE0E}"), 1);
        assert_eq!(w("\u{4E2D}\u{FE0E}"), 2);
        assert_eq!(w("A\u{FE0F}"), 1);
        // Text-default emoji base plus skin-tone modifier is an emoji sequence.
        assert_eq!(w("\u{1F590}\u{1F3FB}"), 2);
    }

    #[test]
    fn zwj_and_flags() {
        assert_eq!(w("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}"), 2);
        assert_eq!(w("\u{1F1FA}\u{1F1F8}"), 2);
        assert_eq!(w("1\u{FE0F}\u{20E3}"), 2);
    }

    #[test]
    fn wcswidth_is_a_sum() {
        let widths = Widths {
            method: WidthMethod::Wcswidth,
            ..Widths::default()
        };
        assert_eq!(widths.cluster_width("\u{1F468}\u{200D}\u{1F469}"), 4);
        assert_eq!(widths.cluster_width("e\u{301}"), 1);
    }

    #[test]
    fn ambiguous_wide() {
        let widths = Widths {
            ambiguous: Ambiguous::Wide,
            ..Widths::default()
        };
        assert_eq!(widths.scalar_width('\u{00A1}'), 2);
        assert_eq!(Widths::default().scalar_width('\u{00A1}'), 1);
        assert_eq!(widths.scalar_width('\u{301}'), 0);
    }

    #[test]
    fn overrides() {
        let o = CellWidthOverrides::new([(0xE000, 0xE0FF, 2), (0xF000, 0xF0FF, 1)]).unwrap();
        let widths = Widths {
            overrides: o,
            ..Widths::default()
        };
        assert_eq!(widths.cluster_width("\u{E010}"), 2);
        assert_eq!(widths.cluster_width("\u{F010}"), 1);
        assert_eq!(widths.cluster_width("\u{E100}"), 1);
        assert_eq!(
            CellWidthOverrides::new([(1, 5, 2), (5, 9, 1)]),
            Err(OverrideError::Overlap)
        );
        assert_eq!(
            CellWidthOverrides::new([(1, 5, 3)]),
            Err(OverrideError::BadWidth)
        );
        assert_eq!(
            CellWidthOverrides::new([(6, 5, 1)]),
            Err(OverrideError::BadRange)
        );
    }

    #[test]
    fn measure_is_incremental() {
        let widths = Widths::default();
        let mut m = ClusterMeasure::new();
        m.push(&widths, '\u{2764}');
        assert_eq!(m.width(WidthMethod::Unicode), 1);
        m.push(&widths, VS16);
        assert_eq!(m.width(WidthMethod::Unicode), 2);
    }
}
