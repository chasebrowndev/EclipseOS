// SPDX-License-Identifier: Apache-2.0
//! Per-scalar properties from the generated three-stage table.
//!
//! Property word layout (`u16`), shared with the generator:
//!
//! | Bits | Field |
//! |---|---|
//! | 0..1 | table width 0, 1 or 2 (ambiguous-as-narrow) |
//! | 2 | `Emoji_Presentation` |
//! | 3 | `Extended_Pictographic` |
//! | 4 | East Asian Ambiguous |
//! | 5 | `Emoji` |
//! | 6..10 | grapheme-break [`Class`] |

use crate::segment::Class;
use crate::tables::{S1, S2, S3};

/// Mask of the width field.
pub const WIDTH_MASK: u16 = 0b11;
/// `Emoji_Presentation` bit.
pub const EMOJI_PRESENTATION: u16 = 1 << 2;
/// `Extended_Pictographic` bit.
pub const EXTENDED_PICTOGRAPHIC: u16 = 1 << 3;
/// East Asian Ambiguous bit.
pub const EAST_ASIAN_AMBIGUOUS: u16 = 1 << 4;
/// `Emoji` bit.
pub const EMOJI: u16 = 1 << 5;
/// Shift of the class field.
pub const CLASS_SHIFT: u16 = 6;

/// The property word of `c`.
#[inline]
pub fn props(c: char) -> u16 {
    let cp = c as usize;
    let chunk = usize::from(S1[cp >> 12]);
    let page = usize::from(S2[(chunk << 4) | ((cp >> 8) & 15)]);
    S3[(page << 8) | (cp & 255)]
}

/// Grapheme-break class of `c`.
#[inline]
pub fn class_of(c: char) -> Class {
    Class::from_u8((props(c) >> CLASS_SHIFT) as u8)
}

/// Table width of `c` (0, 1 or 2) with ambiguous characters narrow.
#[inline]
pub fn table_width(c: char) -> u8 {
    (props(c) & WIDTH_MASK) as u8
}

/// `Emoji_Presentation`.
#[inline]
pub fn is_emoji_presentation(c: char) -> bool {
    props(c) & EMOJI_PRESENTATION != 0
}

/// `Extended_Pictographic`.
#[inline]
pub fn is_extended_pictographic(c: char) -> bool {
    props(c) & EXTENDED_PICTOGRAPHIC != 0
}

/// East Asian Width `A`.
#[inline]
pub fn is_east_asian_ambiguous(c: char) -> bool {
    props(c) & EAST_ASIAN_AMBIGUOUS != 0
}

/// `Emoji` (the characters that may take VS15/VS16).
#[inline]
pub fn is_emoji(c: char) -> bool {
    props(c) & EMOJI != 0
}

/// The Unicode version the committed tables were generated from.
pub fn unicode_version() -> &'static str {
    crate::tables::UNICODE_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spot_checks() {
        assert_eq!(table_width('a'), 1);
        assert_eq!(table_width('\u{4E2D}'), 2);
        assert_eq!(table_width('\u{301}'), 0);
        assert_eq!(table_width('\u{1F600}'), 2);
        assert_eq!(table_width('\u{200D}'), 0);
        assert_eq!(table_width('\0'), 0);
        assert!(is_east_asian_ambiguous('\u{00A1}'));
        assert!(is_emoji('#'));
        assert!(!is_emoji_presentation('#'));
        assert!(is_extended_pictographic('\u{00A9}'));
        assert_eq!(class_of('\r'), Class::Cr);
        assert_eq!(class_of('\u{200D}'), Class::Zwj);
        assert_eq!(class_of('\u{1F1E6}'), Class::RegionalIndicator);
        assert_eq!(class_of('\u{AC00}'), Class::Lv);
        assert_eq!(class_of('\u{094D}'), Class::InCbLinker);
    }
}
