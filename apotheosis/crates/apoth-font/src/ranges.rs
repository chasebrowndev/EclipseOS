// SPDX-License-Identifier: AGPL-3.0-only
//! Sorted, merged code-point ranges. The coverage index of T-06 §4 and the `font.map` /
//! `font.cell-width-override` keys are all this type (the shared `CodepointRanges` of the
//! config schema is not available at M6, so this is the local equivalent).

use std::fmt;

/// An inclusive code-point range.
pub type Range = (u32, u32);

/// Sorted, non-overlapping, non-adjacent inclusive ranges.
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub struct CodepointRanges {
    ranges: Vec<Range>,
}

impl fmt::Debug for CodepointRanges {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CodepointRanges(")?;
        for (i, (a, b)) in self.ranges.iter().enumerate() {
            if i > 0 {
                f.write_str(",")?;
            }
            if a == b {
                write!(f, "U+{a:04X}")?;
            } else {
                write!(f, "U+{a:04X}-{b:04X}")?;
            }
        }
        f.write_str(")")
    }
}

/// Error from [`CodepointRanges::parse`]. Carries no input text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeParseError {
    /// A token was not `U+XXXX`, `U+XXXX-YYYY` or a single character.
    BadToken,
    /// A range had its end before its start or a value above U+10FFFF.
    OutOfOrder,
}

impl fmt::Display for RangeParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadToken => f.write_str("malformed code-point range"),
            Self::OutOfOrder => f.write_str("code-point range out of order or out of bounds"),
        }
    }
}

impl std::error::Error for RangeParseError {}

const MAX_CP: u32 = 0x10_FFFF;

impl CodepointRanges {
    /// Empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds from arbitrary (possibly unsorted, overlapping) inclusive ranges.
    pub fn from_ranges(iter: impl IntoIterator<Item = Range>) -> Self {
        let mut v: Vec<Range> = iter
            .into_iter()
            .filter(|&(a, b)| a <= b && a <= MAX_CP)
            .map(|(a, b)| (a, b.min(MAX_CP)))
            .collect();
        v.sort_unstable();
        Self::merge_sorted(v)
    }

    /// Builds from code points.
    pub fn from_codepoints(iter: impl IntoIterator<Item = u32>) -> Self {
        let mut v: Vec<u32> = iter.into_iter().filter(|&c| c <= MAX_CP).collect();
        v.sort_unstable();
        v.dedup();
        Self::merge_sorted(v.into_iter().map(|c| (c, c)).collect())
    }

    fn merge_sorted(v: Vec<Range>) -> Self {
        let mut out: Vec<Range> = Vec::with_capacity(v.len().min(1024));
        for (a, b) in v {
            match out.last_mut() {
                Some(last) if a <= last.1.saturating_add(1) => last.1 = last.1.max(b),
                _ => out.push((a, b)),
            }
        }
        Self { ranges: out }
    }

    /// Parses `U+E0A0-E0A3, U+F000 a-z` style lists (comma or whitespace separated; a single
    /// character token is its own code point).
    pub fn parse(s: &str) -> Result<Self, RangeParseError> {
        let mut v = Vec::new();
        for tok in s.split(|c: char| c == ',' || c.is_whitespace()) {
            if tok.is_empty() {
                continue;
            }
            let hex = |t: &str| -> Result<u32, RangeParseError> {
                let t = t
                    .strip_prefix("U+")
                    .or_else(|| t.strip_prefix("u+"))
                    .unwrap_or(t);
                if t.is_empty() || t.len() > 6 {
                    return Err(RangeParseError::BadToken);
                }
                u32::from_str_radix(t, 16).map_err(|_| RangeParseError::BadToken)
            };
            let (a, b) = if tok.starts_with("U+") || tok.starts_with("u+") {
                match tok.split_once('-') {
                    Some((x, y)) => (hex(x)?, hex(y)?),
                    None => {
                        let x = hex(tok)?;
                        (x, x)
                    }
                }
            } else {
                let mut it = tok.chars();
                match (it.next(), it.next()) {
                    (Some(c), None) => (c as u32, c as u32),
                    _ => return Err(RangeParseError::BadToken),
                }
            };
            if a > b || b > MAX_CP {
                return Err(RangeParseError::OutOfOrder);
            }
            v.push((a, b));
        }
        Ok(Self::from_ranges(v))
    }

    /// True if no code point is in the set.
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// The merged ranges.
    pub fn ranges(&self) -> &[Range] {
        &self.ranges
    }

    /// Number of code points in the set.
    pub fn count(&self) -> u64 {
        self.ranges.iter().map(|&(a, b)| u64::from(b - a) + 1).sum()
    }

    /// Binary search membership test.
    pub fn contains(&self, cp: u32) -> bool {
        let i = self.ranges.partition_point(|&(_, b)| b < cp);
        self.ranges.get(i).is_some_and(|&(a, _)| a <= cp)
    }

    /// True if every code point yielded is in the set.
    pub fn covers_all(&self, cps: impl IntoIterator<Item = u32>) -> bool {
        cps.into_iter().all(|c| self.contains(c))
    }

    /// How many of the yielded code points are in the set.
    pub fn count_covered(&self, cps: impl IntoIterator<Item = u32>) -> usize {
        cps.into_iter().filter(|&c| self.contains(c)).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_range_input_is_dropped_or_clamped() {
        let r = CodepointRanges::from_ranges([(0x11_0000, 0x12_0000)]);
        assert_eq!(r.count(), 0);
        assert!(r.ranges().is_empty());
        let r = CodepointRanges::from_ranges([(0x10_FFF0, 0x20_0000)]);
        assert_eq!(r.ranges(), &[(0x10_FFF0, 0x10_FFFF)]);
        assert_eq!(r.count(), 16);
        let r = CodepointRanges::from_codepoints([0x20_0000, 0x41]);
        assert_eq!(r.ranges(), &[(0x41, 0x41)]);
    }

    #[test]
    fn merge_and_contains() {
        let r = CodepointRanges::from_ranges([(10, 20), (5, 9), (30, 30), (18, 25)]);
        assert_eq!(r.ranges(), &[(5, 25), (30, 30)]);
        assert!(r.contains(5) && r.contains(25) && r.contains(30));
        assert!(!r.contains(4) && !r.contains(26) && !r.contains(31));
        assert_eq!(r.count(), 22);
    }

    #[test]
    fn ranges_above_unicode_are_dropped_or_clamped() {
        // Config `font.map` ranges reach here unchecked (system.rs).
        let r = CodepointRanges::from_ranges([(0x11_0000, 0x12_0000)]);
        assert!(r.is_empty());
        assert_eq!(r.count(), 0);
        let r = CodepointRanges::from_ranges([(0x10_FFF0, 0x20_0000), (u32::MAX, u32::MAX)]);
        assert_eq!(r.ranges(), &[(0x10_FFF0, MAX_CP)]);
        assert_eq!(r.count(), 16);
        assert!(CodepointRanges::from_codepoints([0x11_0000, u32::MAX]).is_empty());
        assert!(!r.contains(0x11_0000));
    }

    #[test]
    fn from_codepoints_merges_adjacent() {
        let r = CodepointRanges::from_codepoints([3, 1, 2, 2, 9]);
        assert_eq!(r.ranges(), &[(1, 3), (9, 9)]);
    }

    #[test]
    fn parse_forms() {
        let r = CodepointRanges::parse("U+E0A0-E0A3, U+F000 x").unwrap();
        assert_eq!(
            r.ranges(),
            &[(0x78, 0x78), (0xE0A0, 0xE0A3), (0xF000, 0xF000)]
        );
        assert_eq!(
            CodepointRanges::parse("U+10-5"),
            Err(RangeParseError::OutOfOrder)
        );
        assert_eq!(
            CodepointRanges::parse("U+ZZ"),
            Err(RangeParseError::BadToken)
        );
        assert_eq!(CodepointRanges::parse("ab"), Err(RangeParseError::BadToken));
        assert_eq!(
            CodepointRanges::parse("U+110000"),
            Err(RangeParseError::OutOfOrder)
        );
        assert!(CodepointRanges::parse("").unwrap().is_empty());
    }

    #[test]
    fn covers_all_checks_every_point() {
        let r = CodepointRanges::from_ranges([(0x41, 0x5A)]);
        assert!(r.covers_all([0x41, 0x5A]));
        assert!(!r.covers_all([0x41, 0x61]));
        assert_eq!(r.count_covered([0x41, 0x61, 0x42]), 2);
    }
}
