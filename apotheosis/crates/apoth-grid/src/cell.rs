// SPDX-License-Identifier: AGPL-3.0-only
//! The 8-byte cell (T-02 §1.2).
//!
//! ```text
//! bits  0..20  code point, or arena index when tag = Cluster
//! bits 21..22  tag: Empty | Scalar | Cluster | BgOnly
//! bits 23..24  wide: Narrow | Wide | SpacerTail | SpacerHead
//! bit  25      protected (DECSCA)
//! bit  26      hyperlink present
//! bits 27..28  semantic: Output | Input | Prompt
//! bit  29      multicell member (OSC 66)
//! bits 30..31  reserved
//! bits 32..47  style id (0 = default)
//! bits 48..63  BgOnly: low 16 bits of the packed colour; otherwise reserved
//! ```
//!
//! `BgOnly` packing (spec gap, decided here): the colour is a 32-bit word (kind in bits
//! 24..25, palette index or RGB below). Its low 16 bits live in bits 48..63 and its high 16
//! bits in the otherwise unused code-point field, bits 0..15.

use crate::style::Color;

/// What a cell holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Tag {
    /// Blank, default background.
    Empty = 0,
    /// One scalar value.
    Scalar = 1,
    /// A grapheme cluster in the page arena.
    Cluster = 2,
    /// Blank with a palette or RGB background and no style item.
    BgOnly = 3,
}

/// Wide-character role of a cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Wide {
    /// One cell.
    Narrow = 0,
    /// Left half of a two-cell character.
    Wide = 1,
    /// Right half of a two-cell character.
    SpacerTail = 2,
    /// Blank left at the end of a row when a wide character wrapped.
    SpacerHead = 3,
}

/// Shell-integration zone of a cell (T-14).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum Semantic {
    /// Command output (default).
    #[default]
    Output = 0,
    /// User input.
    Input = 1,
    /// Prompt text.
    Prompt = 2,
}

/// One grid cell.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Cell(pub u64);

impl core::fmt::Debug for Cell {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Cell({:#018x})", self.0)
    }
}

const CP_MASK: u64 = (1 << 21) - 1;
const TAG_SHIFT: u32 = 21;
const WIDE_SHIFT: u32 = 23;
const PROTECTED: u64 = 1 << 25;
const LINK: u64 = 1 << 26;
const SEM_SHIFT: u32 = 27;
const MULTICELL: u64 = 1 << 29;
const STYLE_SHIFT: u32 = 32;

impl Cell {
    /// The blank cell.
    pub const EMPTY: Cell = Cell(0);

    /// A scalar cell with `style` (0 = default).
    pub fn scalar(ch: char, style: u16) -> Cell {
        Cell(
            u64::from(ch as u32)
                | ((Tag::Scalar as u64) << TAG_SHIFT)
                | (u64::from(style) << STYLE_SHIFT),
        )
    }

    /// A cluster cell whose scalars start at arena chunk `index`.
    pub fn cluster(index: u32, style: u16) -> Cell {
        Cell(
            (u64::from(index) & CP_MASK)
                | ((Tag::Cluster as u64) << TAG_SHIFT)
                | (u64::from(style) << STYLE_SHIFT),
        )
    }

    /// A blank cell with a palette or RGB background. `Color::Default` gives `EMPTY`.
    pub fn bg_only(color: Color) -> Cell {
        if color == Color::Default {
            return Cell::EMPTY;
        }
        let p = u64::from(color.pack());
        Cell((p >> 16) | ((Tag::BgOnly as u64) << TAG_SHIFT) | ((p & 0xffff) << 48))
    }

    /// A right-half spacer carrying the wide cell's style.
    pub fn spacer_tail(style: u16) -> Cell {
        Cell(((Wide::SpacerTail as u64) << WIDE_SHIFT) | (u64::from(style) << STYLE_SHIFT))
    }

    /// The end-of-row spacer used when a wide character wraps.
    pub fn spacer_head() -> Cell {
        Cell((Wide::SpacerHead as u64) << WIDE_SHIFT)
    }

    /// Bits that make a cell non-plain (overwriting needs no release or repair: style, link, cluster or background payload, wide part).
    pub(crate) const NOT_PLAIN: u64 =
        (0xffff << STYLE_SHIFT) | (3 << WIDE_SHIFT) | LINK | (1 << 22);

    /// What the cell holds.
    pub fn tag(self) -> Tag {
        match (self.0 >> TAG_SHIFT) & 3 {
            0 => Tag::Empty,
            1 => Tag::Scalar,
            2 => Tag::Cluster,
            _ => Tag::BgOnly,
        }
    }

    /// Wide-character role.
    pub fn wide(self) -> Wide {
        match (self.0 >> WIDE_SHIFT) & 3 {
            0 => Wide::Narrow,
            1 => Wide::Wide,
            2 => Wide::SpacerTail,
            _ => Wide::SpacerHead,
        }
    }

    /// The scalar of a `Scalar` cell, else `None`.
    pub fn scalar_char(self) -> Option<char> {
        if self.tag() == Tag::Scalar {
            char::from_u32((self.0 & CP_MASK) as u32)
        } else {
            None
        }
    }

    /// The arena chunk index of a `Cluster` cell.
    pub fn cluster_index(self) -> u32 {
        (self.0 & CP_MASK) as u32
    }

    /// Bits 0..20: the scalar, or the arena index of a cluster.
    pub fn payload(self) -> u32 {
        (self.0 & CP_MASK) as u32
    }

    /// With the tag replaced.
    pub fn with_tag(self, t: Tag) -> Cell {
        Cell((self.0 & !(3 << TAG_SHIFT)) | ((t as u64) << TAG_SHIFT))
    }

    /// With the 21-bit payload replaced.
    pub fn with_payload(self, v: u32) -> Cell {
        Cell((self.0 & !CP_MASK) | (u64::from(v) & CP_MASK))
    }

    /// Short name of [`Cell::is_protected`].
    pub fn protected(self) -> bool {
        self.is_protected()
    }

    /// The palette index of a `BgOnly` cell whose background is a palette colour.
    pub fn bg_palette_index(self) -> Option<u8> {
        match self.bg_color() {
            Color::Palette(n) if self.tag() == Tag::BgOnly => Some(n),
            _ => None,
        }
    }

    /// DECSCA protection.
    pub fn is_protected(self) -> bool {
        self.0 & PROTECTED != 0
    }

    /// Whether a hyperlink is attached (the link lives in the page's link map).
    pub fn has_link(self) -> bool {
        self.0 & LINK != 0
    }

    /// Shell-integration zone.
    pub fn semantic(self) -> Semantic {
        match (self.0 >> SEM_SHIFT) & 3 {
            1 => Semantic::Input,
            2 => Semantic::Prompt,
            _ => Semantic::Output,
        }
    }

    /// OSC 66 multicell member bit (reserved; set by R2 code only).
    pub fn is_multicell(self) -> bool {
        self.0 & MULTICELL != 0
    }

    /// Page-local style id (0 = default).
    pub fn style_id(self) -> u16 {
        if self.tag() == Tag::BgOnly {
            0
        } else {
            (self.0 >> STYLE_SHIFT) as u16
        }
    }

    /// The background of a `BgOnly` cell.
    pub fn bg_color(self) -> Color {
        if self.tag() != Tag::BgOnly {
            return Color::Default;
        }
        let packed = ((self.0 & 0xffff) << 16) | (self.0 >> 48);
        Color::unpack(packed as u32)
    }

    /// True for a cell that holds nothing printable and no background.
    pub fn is_blank(self) -> bool {
        self.tag() == Tag::Empty
    }

    /// With the wide role set.
    pub fn with_wide(self, w: Wide) -> Cell {
        Cell((self.0 & !(3 << WIDE_SHIFT)) | ((w as u64) << WIDE_SHIFT))
    }

    /// With DECSCA protection set or cleared.
    pub fn with_protected(self, on: bool) -> Cell {
        Cell(if on {
            self.0 | PROTECTED
        } else {
            self.0 & !PROTECTED
        })
    }

    /// With the hyperlink bit set or cleared.
    pub fn with_link(self, on: bool) -> Cell {
        Cell(if on { self.0 | LINK } else { self.0 & !LINK })
    }

    /// With the semantic zone set.
    pub fn with_semantic(self, s: Semantic) -> Cell {
        Cell((self.0 & !(3 << SEM_SHIFT)) | ((s as u64) << SEM_SHIFT))
    }

    /// With the multicell bit set or cleared.
    pub fn with_multicell(self, on: bool) -> Cell {
        Cell(if on {
            self.0 | MULTICELL
        } else {
            self.0 & !MULTICELL
        })
    }

    /// With the style id replaced (not valid on `BgOnly` cells).
    pub fn with_style(self, id: u16) -> Cell {
        Cell((self.0 & !(0xffff << STYLE_SHIFT)) | (u64::from(id) << STYLE_SHIFT))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_fields() {
        let c = Cell::scalar('\u{1F600}', 7)
            .with_wide(Wide::Wide)
            .with_protected(true)
            .with_link(true)
            .with_semantic(Semantic::Prompt)
            .with_multicell(true);
        assert_eq!(c.scalar_char(), Some('\u{1F600}'));
        assert_eq!(c.tag(), Tag::Scalar);
        assert_eq!(c.wide(), Wide::Wide);
        assert!(c.is_protected() && c.has_link() && c.is_multicell());
        assert_eq!(c.semantic(), Semantic::Prompt);
        assert_eq!(c.style_id(), 7);
        let c = c
            .with_protected(false)
            .with_link(false)
            .with_multicell(false);
        assert!(!c.is_protected() && !c.has_link() && !c.is_multicell());
        assert_eq!(c.style_id(), 7);
    }

    #[test]
    fn max_codepoint_fits() {
        let c = Cell::scalar('\u{10FFFF}', 0xffff);
        assert_eq!(c.scalar_char(), Some('\u{10FFFF}'));
        assert_eq!(c.style_id(), 0xffff);
        assert_eq!(c.tag(), Tag::Scalar);
    }

    #[test]
    fn bg_only_roundtrip() {
        for col in [
            Color::Palette(0),
            Color::Palette(200),
            Color::Rgb(1, 2, 3),
            Color::Rgb(255, 255, 255),
        ] {
            let c = Cell::bg_only(col);
            assert_eq!(c.tag(), Tag::BgOnly);
            assert_eq!(c.bg_color(), col);
            assert_eq!(c.style_id(), 0);
        }
        assert_eq!(Cell::bg_only(Color::Default), Cell::EMPTY);
    }

    #[test]
    fn cluster_and_spacers() {
        let c = Cell::cluster(12345, 3);
        assert_eq!(c.tag(), Tag::Cluster);
        assert_eq!(c.cluster_index(), 12345);
        assert_eq!(Cell::spacer_tail(2).wide(), Wide::SpacerTail);
        assert_eq!(Cell::spacer_tail(2).tag(), Tag::Empty);
        assert_eq!(Cell::spacer_head().wide(), Wide::SpacerHead);
        assert!(Cell::EMPTY.is_blank());
    }
}
