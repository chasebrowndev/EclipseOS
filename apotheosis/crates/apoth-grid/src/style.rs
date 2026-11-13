// SPDX-License-Identifier: AGPL-3.0-only
//! Colours and text styles, and their 16-byte page encoding.

/// A terminal colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Color {
    /// The default foreground or background.
    #[default]
    Default,
    /// A palette index (0..=255).
    Palette(u8),
    /// A 24-bit colour.
    Rgb(u8, u8, u8),
}

impl Color {
    /// Four-byte encoding: kind then three data bytes.
    pub(crate) fn encode(self) -> [u8; 4] {
        match self {
            Color::Default => [0, 0, 0, 0],
            Color::Palette(i) => [1, i, 0, 0],
            Color::Rgb(r, g, b) => [2, r, g, b],
        }
    }

    pub(crate) fn decode(b: [u8; 4]) -> Color {
        match b[0] {
            1 => Color::Palette(b[1]),
            2 => Color::Rgb(b[1], b[2], b[3]),
            _ => Color::Default,
        }
    }

    /// Packed 32-bit form used by `BgOnly` cells: kind in bits 24..25, data below.
    pub(crate) fn pack(self) -> u32 {
        match self {
            Color::Default => 0,
            Color::Palette(i) => (1 << 24) | u32::from(i),
            Color::Rgb(r, g, b) => {
                (2 << 24) | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
            }
        }
    }

    pub(crate) fn unpack(v: u32) -> Color {
        match (v >> 24) & 3 {
            1 => Color::Palette(v as u8),
            2 => Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8),
            _ => Color::Default,
        }
    }
}

/// Underline shape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Underline {
    /// No underline.
    #[default]
    None = 0,
    /// SGR 4.
    Single = 1,
    /// SGR 21 / 4:2.
    Double = 2,
    /// 4:3.
    Curly = 3,
    /// 4:4.
    Dotted = 4,
    /// 4:5.
    Dashed = 5,
}

impl Underline {
    fn from_u8(v: u8) -> Underline {
        match v {
            1 => Underline::Single,
            2 => Underline::Double,
            3 => Underline::Curly,
            4 => Underline::Dotted,
            5 => Underline::Dashed,
            _ => Underline::None,
        }
    }
}

/// Attribute bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Attrs(pub u16);

impl Attrs {
    /// No attributes.
    pub const NONE: Attrs = Attrs(0);
    /// Bold.
    pub const BOLD: u16 = 1;
    /// Faint.
    pub const FAINT: u16 = 1 << 1;
    /// Italic.
    pub const ITALIC: u16 = 1 << 2;
    /// Blink.
    pub const BLINK: u16 = 1 << 3;
    /// Inverse video.
    pub const INVERSE: u16 = 1 << 4;
    /// Invisible.
    pub const INVISIBLE: u16 = 1 << 5;
    /// Strikethrough.
    pub const STRIKE: u16 = 1 << 6;
    /// Overline.
    pub const OVERLINE: u16 = 1 << 7;

    /// True when every bit of `bits` is set.
    pub fn contains(self, bits: u16) -> bool {
        self.0 & bits == bits
    }

    /// A copy with `bits` set or cleared.
    pub fn with(self, bits: u16, on: bool) -> Attrs {
        if on {
            Attrs(self.0 | bits)
        } else {
            Attrs(self.0 & !bits)
        }
    }
}

/// A resolved text style. Equal styles intern to one page item.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Style {
    /// Foreground.
    pub fg: Color,
    /// Background.
    pub bg: Color,
    /// Underline colour.
    pub ul_color: Color,
    /// Underline shape.
    pub underline: Underline,
    /// Attribute bits.
    pub attrs: Attrs,
}

impl Style {
    /// The default style (id 0 in every page).
    pub const DEFAULT: Style = Style {
        fg: Color::Default,
        bg: Color::Default,
        ul_color: Color::Default,
        underline: Underline::None,
        attrs: Attrs::NONE,
    };

    /// True for the default style.
    pub fn is_default(&self) -> bool {
        *self == Style::DEFAULT
    }

    /// True when only the background differs from the default and it is not default.
    pub fn is_bg_only(&self) -> bool {
        self.bg != Color::Default
            && Style {
                bg: Color::Default,
                ..*self
            } == Style::DEFAULT
    }

    pub(crate) fn encode(&self) -> [u8; 16] {
        let mut o = [0u8; 16];
        o[0..4].copy_from_slice(&self.fg.encode());
        o[4..8].copy_from_slice(&self.bg.encode());
        o[8..12].copy_from_slice(&self.ul_color.encode());
        o[12..14].copy_from_slice(&self.attrs.0.to_le_bytes());
        o[14] = self.underline as u8;
        o
    }

    pub(crate) fn decode(b: &[u8]) -> Style {
        let c = |i: usize| Color::decode([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        Style {
            fg: c(0),
            bg: c(4),
            ul_color: c(8),
            attrs: Attrs(u16::from_le_bytes([b[12], b[13]])),
            underline: Underline::from_u8(b[14]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_roundtrip() {
        let s = Style {
            fg: Color::Rgb(1, 2, 3),
            bg: Color::Palette(9),
            ul_color: Color::Rgb(255, 0, 7),
            underline: Underline::Curly,
            attrs: Attrs(Attrs::BOLD | Attrs::STRIKE),
        };
        assert_eq!(Style::decode(&s.encode()), s);
        assert!(Style::DEFAULT.is_default());
        assert!(!s.is_default());
    }

    #[test]
    fn color_pack_roundtrip() {
        for c in [
            Color::Palette(0),
            Color::Palette(255),
            Color::Rgb(0, 0, 0),
            Color::Rgb(255, 1, 128),
        ] {
            assert_eq!(Color::unpack(c.pack()), c);
        }
    }

    #[test]
    fn bg_only() {
        assert!(
            Style {
                bg: Color::Palette(1),
                ..Style::DEFAULT
            }
            .is_bg_only()
        );
        assert!(!Style::DEFAULT.is_bg_only());
        assert!(
            !Style {
                bg: Color::Palette(1),
                attrs: Attrs(1),
                ..Style::DEFAULT
            }
            .is_bg_only()
        );
    }
}
