// SPDX-License-Identifier: AGPL-3.0-only
//! The font configuration surface of T-06 §2, as plain data. Parsing KDL into this is
//! `apoth-config`'s job; this crate only owns the value types and the two small micro
//! syntaxes (feature lists, axis lists).

use crate::ranges::CodepointRanges;
use std::fmt;

/// Text style, one of the four configured stacks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Style {
    /// `regular`
    Regular,
    /// `bold`
    Bold,
    /// `italic`
    Italic,
    /// `bold-italic`
    BoldItalic,
}

impl Style {
    /// All four styles in index order.
    pub const ALL: [Style; 4] = [
        Style::Regular,
        Style::Bold,
        Style::Italic,
        Style::BoldItalic,
    ];

    /// Dense index 0..4.
    pub fn index(self) -> usize {
        self as usize
    }

    /// Builds from attribute flags.
    pub fn from_flags(bold: bool, italic: bool) -> Self {
        match (bold, italic) {
            (false, false) => Self::Regular,
            (true, false) => Self::Bold,
            (false, true) => Self::Italic,
            (true, true) => Self::BoldItalic,
        }
    }

    /// True for `bold` and `bold-italic`.
    pub fn is_bold(self) -> bool {
        matches!(self, Self::Bold | Self::BoldItalic)
    }

    /// True for `italic` and `bold-italic`.
    pub fn is_italic(self) -> bool {
        matches!(self, Self::Italic | Self::BoldItalic)
    }

    /// The KDL name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Regular => "regular",
            Self::Bold => "bold",
            Self::Italic => "italic",
            Self::BoldItalic => "bold-italic",
        }
    }
}

/// `font.ligatures`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Ligatures {
    /// Ligatures everywhere.
    On,
    /// No ligature features.
    Off,
    /// Ligatures, except the run is broken at the cursor cell so it un-fuses.
    #[default]
    NotUnderCursor,
}

/// `font.hinting`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Hinting {
    /// Unhinted outlines.
    None,
    /// Light hinting (default).
    #[default]
    Light,
    /// Full hinting.
    Full,
}

/// One OpenType feature setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FontFeature {
    /// Four-byte tag.
    pub tag: [u8; 4],
    /// 0 = off, 1 = on, larger values select alternates.
    pub value: u32,
}

/// One variation axis setting.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AxisSetting {
    /// Four-byte axis tag.
    pub tag: [u8; 4],
    /// User-space value.
    pub value: f32,
}

/// Parse errors for the two list syntaxes. Carry no input text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListParseError {
    /// Tag is not 1..=4 printable ASCII characters.
    BadTag,
    /// Value is not a number.
    BadValue,
}

impl fmt::Display for ListParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadTag => f.write_str("malformed OpenType tag"),
            Self::BadValue => f.write_str("malformed numeric value"),
        }
    }
}

impl std::error::Error for ListParseError {}

fn parse_tag(s: &str) -> Result<[u8; 4], ListParseError> {
    let s = s.trim_matches('"');
    let b = s.as_bytes();
    if b.is_empty() || b.len() > 4 || !b.iter().all(|c| (0x20..0x7F).contains(c)) {
        return Err(ListParseError::BadTag);
    }
    let mut t = [b' '; 4];
    t[..b.len()].copy_from_slice(b);
    Ok(t)
}

/// Parses `+ss01 -calt liga=0 ss02=3` (whitespace or comma separated). `+tag` and a bare
/// `tag` mean 1, `-tag` means 0, `tag=N` means N. Later settings win at shaping time.
pub fn parse_features(s: &str) -> Result<Vec<FontFeature>, ListParseError> {
    let mut out = Vec::new();
    for tok in s
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
    {
        let (name, value) = if let Some(rest) = tok.strip_prefix('-') {
            (rest, 0)
        } else {
            let rest = tok.strip_prefix('+').unwrap_or(tok);
            match rest.split_once('=') {
                Some((n, v)) => (n, v.parse::<u32>().map_err(|_| ListParseError::BadValue)?),
                None => (rest, 1),
            }
        };
        let name = name.split_once('=').map_or(name, |(n, _)| n);
        out.push(FontFeature {
            tag: parse_tag(name)?,
            value,
        });
    }
    Ok(out)
}

/// Parses `wght=450 slnt=-8` (whitespace or comma separated).
pub fn parse_axes(s: &str) -> Result<Vec<AxisSetting>, ListParseError> {
    let mut out = Vec::new();
    for tok in s
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
    {
        let (n, v) = tok.split_once('=').ok_or(ListParseError::BadValue)?;
        let value = v.parse::<f32>().map_err(|_| ListParseError::BadValue)?;
        if !value.is_finite() {
            return Err(ListParseError::BadValue);
        }
        out.push(AxisSetting {
            tag: parse_tag(n)?,
            value,
        });
    }
    Ok(out)
}

/// `font.<style>.*`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StyleConfig {
    /// `font.<style>.family`. `None` for a non-regular style means "derive from regular".
    pub family: Option<String>,
    /// `font.<style>.instance`: named instance of a variable font.
    pub instance: Option<String>,
    /// `font.<style>.axes`. Applied to both the shaper instance and the outline location.
    pub axes: Vec<AxisSetting>,
    /// `font.<style>.features`.
    pub features: Vec<FontFeature>,
}

/// A `font.adjust-*` adjustment, applied after the 1 px minimum.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Adjustment {
    /// Add this many physical pixels (may be negative).
    Px(i32),
    /// Scale the value by `1 + pct / 100`.
    Percent(f32),
}

impl Adjustment {
    /// Parses `3`, `-2px`, `20%`, `-10%`.
    pub fn parse(s: &str) -> Result<Self, ListParseError> {
        let s = s.trim();
        if let Some(p) = s.strip_suffix('%') {
            let v = p
                .trim()
                .parse::<f32>()
                .map_err(|_| ListParseError::BadValue)?;
            if !v.is_finite() {
                return Err(ListParseError::BadValue);
            }
            Ok(Self::Percent(v))
        } else {
            let p = s.strip_suffix("px").unwrap_or(s);
            p.trim()
                .parse::<i32>()
                .map(Self::Px)
                .map_err(|_| ListParseError::BadValue)
        }
    }

    /// Applies to a metric of `value` physical pixels. The result is not re-clamped here;
    /// callers decide the floor (cell sizes keep a minimum of 1).
    pub fn apply(self, value: i32) -> i32 {
        match self {
            Self::Px(d) => value.saturating_add(d),
            Self::Percent(p) => (value as f32 * (1.0 + p / 100.0)).round() as i32,
        }
    }
}

/// `font.adjust-*` keys.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Adjust {
    /// Cell width.
    pub cell_width: Option<Adjustment>,
    /// Cell height (the extra space is split evenly above and below).
    pub cell_height: Option<Adjustment>,
    /// Baseline shift.
    pub baseline: Option<Adjustment>,
    /// Underline position.
    pub underline_position: Option<Adjustment>,
    /// Underline thickness.
    pub underline_thickness: Option<Adjustment>,
    /// Strikethrough position.
    pub strikethrough_position: Option<Adjustment>,
    /// Strikethrough thickness.
    pub strikethrough_thickness: Option<Adjustment>,
    /// Cursor thickness.
    pub cursor_thickness: Option<Adjustment>,
}

/// Which synthetic styles are allowed when a style has no face of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Synthetic {
    /// Embolden outlines when bold is wanted and only a lighter face exists.
    pub bold: bool,
    /// Skew outlines when italic is wanted and only an upright face exists.
    pub italic: bool,
}

impl Default for Synthetic {
    fn default() -> Self {
        Self {
            bold: true,
            italic: true,
        }
    }
}

/// Cell-width override entry (FONTS-07): code points forced to `cells` cells (1 or 2).
#[derive(Clone, Debug, PartialEq)]
pub struct CellWidthOverride {
    /// Code points affected.
    pub ranges: CodepointRanges,
    /// Forced width in cells.
    pub cells: u8,
}

/// The whole `font { }` block.
#[derive(Clone, Debug, PartialEq)]
pub struct FontConfig {
    /// `font.regular|bold|italic|bold-italic`, indexed by [`Style::index`].
    pub styles: [StyleConfig; 4],
    /// `font.size`, in points (1 pt = 96/72 px at scale 1).
    pub size_pt: f32,
    /// `font.fallback`: ordered families.
    pub fallback: Vec<String>,
    /// `font.map`: code-point ranges bound to a family ahead of fallback.
    pub map: Vec<(CodepointRanges, String)>,
    /// `font.cell-width-override`.
    pub cell_width_override: Vec<CellWidthOverride>,
    /// `font.synthetic`.
    pub synthetic: Synthetic,
    /// `font.ligatures`.
    pub ligatures: Ligatures,
    /// `font.builtin-glyphs`.
    pub builtin_glyphs: bool,
    /// `font.hinting`.
    pub hinting: Hinting,
    /// `font.thicken`: extra stem weight in 1/64 em-percent style units; 0 = off. Applied as
    /// an embolden strength in pixels (`thicken * px_size / 100`).
    pub thicken: f32,
    /// `font.adjust-*`.
    pub adjust: Adjust,
    /// Extra font directories (FONTS-15 `font.directories`).
    pub directories: Vec<std::path::PathBuf>,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            styles: Default::default(),
            size_pt: 11.0,
            fallback: Vec::new(),
            map: Vec::new(),
            cell_width_override: Vec::new(),
            synthetic: Synthetic::default(),
            ligatures: Ligatures::default(),
            builtin_glyphs: true,
            hinting: Hinting::default(),
            thicken: 0.0,
            adjust: Adjust::default(),
            directories: Vec::new(),
        }
    }
}

impl FontConfig {
    /// The style's config.
    pub fn style(&self, s: Style) -> &StyleConfig {
        &self.styles[s.index()]
    }

    /// Pixel size at a surface scale: `size_pt * 96/72 * scale`.
    pub fn pixel_size(&self, scale: f32) -> f32 {
        self.size_pt * (96.0 / 72.0) * scale
    }

    /// `font.cell-width-override` lookup.
    pub fn width_override(&self, cp: u32) -> Option<u8> {
        self.cell_width_override
            .iter()
            .find(|o| o.ranges.contains(cp))
            .map(|o| o.cells)
    }

    /// Features of a style plus the ligature policy, as the shaper sees them. With
    /// [`Ligatures::Off`] the ligature features are forced to 0 ahead of user features, so a
    /// user `+calt` still wins.
    pub fn effective_features(&self, style: Style, ligatures_enabled: bool) -> Vec<FontFeature> {
        let mut v = Vec::new();
        if !ligatures_enabled {
            for t in [b"liga", b"clig", b"calt", b"dlig"] {
                v.push(FontFeature { tag: *t, value: 0 });
            }
        }
        v.extend_from_slice(&self.style(style).features);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn features_syntax() {
        let f = parse_features("+ss01 -calt liga=0 ss02=3 zero").unwrap();
        let t = |s: &[u8; 4], v| FontFeature { tag: *s, value: v };
        assert_eq!(
            f,
            vec![
                t(b"ss01", 1),
                t(b"calt", 0),
                t(b"liga", 0),
                t(b"ss02", 3),
                t(b"zero", 1)
            ]
        );
        assert_eq!(parse_features("+toolong"), Err(ListParseError::BadTag));
        assert_eq!(parse_features("liga=x"), Err(ListParseError::BadValue));
        assert!(parse_features("").unwrap().is_empty());
    }

    #[test]
    fn axes_syntax() {
        let a = parse_axes("wght=450, slnt=-8").unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].tag, *b"wght");
        assert_eq!(a[1].value, -8.0);
        assert!(parse_axes("wght").is_err());
        assert!(parse_axes("wght=nan").is_err());
    }

    #[test]
    fn adjustment_px_and_percent() {
        assert_eq!(Adjustment::parse("3").unwrap(), Adjustment::Px(3));
        assert_eq!(Adjustment::parse("-2px").unwrap(), Adjustment::Px(-2));
        assert_eq!(Adjustment::parse("20%").unwrap(), Adjustment::Percent(20.0));
        assert_eq!(Adjustment::Px(-2).apply(10), 8);
        assert_eq!(Adjustment::Percent(50.0).apply(10), 15);
        assert!(Adjustment::parse("x").is_err());
    }

    #[test]
    fn ligature_off_prepends_disables() {
        let mut c = FontConfig::default();
        c.styles[0].features = parse_features("+calt").unwrap();
        let f = c.effective_features(Style::Regular, false);
        assert_eq!(f.len(), 5);
        assert_eq!(
            f.last().unwrap(),
            &FontFeature {
                tag: *b"calt",
                value: 1
            }
        );
        assert_eq!(c.effective_features(Style::Regular, true).len(), 1);
    }

    #[test]
    fn pixel_size_scales() {
        let c = FontConfig {
            size_pt: 12.0,
            ..Default::default()
        };
        assert_eq!(c.pixel_size(1.0), 16.0);
        assert_eq!(c.pixel_size(2.0), 32.0);
    }
}
