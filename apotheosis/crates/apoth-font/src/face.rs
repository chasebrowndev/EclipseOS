// SPDX-License-Identifier: AGPL-3.0-only
//! One loaded face and the per-style instance of it (variation location, synthetic flags).
//! One `read-fonts` serves the shaper (`harfrust`) and the rasteriser (`skrifa`, `swash`).

use crate::config::AxisSetting;
use skrifa::{
    FontRef, MetadataProvider,
    instance::{Location, LocationRef, Size},
    raw::{TableProvider, types::Tag},
    string::StringId,
};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Stable identifier of a loaded face within one [`crate::Library`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FaceId(pub u32);

/// Why a face could not be loaded. Carries no font bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaceError {
    /// Not a parseable sfnt / collection member.
    Parse,
    /// Parseable but without a usable `cmap`/`head` (not a text face).
    NotATextFace,
}

impl fmt::Display for FaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse => f.write_str("font data did not parse"),
            Self::NotATextFace => f.write_str("font has no character map"),
        }
    }
}

impl std::error::Error for FaceError {}

/// Colour glyph sources a face carries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ColorSources {
    /// `COLR` (v0 and/or v1).
    pub colr: bool,
    /// `CBDT`/`CBLC` bitmaps.
    pub cbdt: bool,
    /// `sbix` bitmaps.
    pub sbix: bool,
    /// `SVG ` (recognised, never drawn: OT-SVG is unsupported).
    pub svg: bool,
}

impl ColorSources {
    /// True if we can draw colour glyphs from this face.
    pub fn drawable(self) -> bool {
        self.colr || self.cbdt || self.sbix
    }
}

static NEXT_UID: AtomicU64 = AtomicU64::new(1);

/// Code points below this are mapped once at load for the simple-text fast path.
const LATIN_LEN: u32 = 0x300;

/// A loaded face. The font bytes are shared (`Arc`), so cloning a [`FaceInstance`] is cheap.
pub struct Face {
    id: FaceId,
    uid: u64,
    data: Arc<[u8]>,
    ttc_index: u32,
    family: String,
    units_per_em: u16,
    weight: u16,
    italic: bool,
    variable: bool,
    color: ColorSources,
    ligature_features: bool,
    latin: Box<[u16]>,
    shaper_data: harfrust::ShaperData,
}

impl fmt::Debug for Face {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Face")
            .field("id", &self.id)
            .field("family", &self.family)
            .field("ttc_index", &self.ttc_index)
            .finish_non_exhaustive()
    }
}

/// The English-or-first string for `id`.
pub(crate) fn name_string(font: &FontRef<'_>, id: StringId) -> Option<String> {
    let s: String = font
        .localized_strings(id)
        .english_or_first()?
        .chars()
        .collect();
    (!s.is_empty()).then_some(s)
}

/// Family name preferring the typographic family (name id 16) over id 1.
pub(crate) fn family_name(font: &FontRef<'_>) -> Option<String> {
    name_string(font, StringId::TYPOGRAPHIC_FAMILY_NAME)
        .or_else(|| name_string(font, StringId::FAMILY_NAME))
}

pub(crate) fn color_sources(font: &FontRef<'_>) -> ColorSources {
    let has = |t: &[u8; 4]| font.table_data(Tag::new(t)).is_some();
    ColorSources {
        colr: has(b"COLR"),
        cbdt: has(b"CBDT") && has(b"CBLC"),
        sbix: has(b"sbix"),
        svg: has(b"SVG "),
    }
}

fn gsub_has_ligature_features(font: &FontRef<'_>) -> bool {
    let Ok(gsub) = font.gsub() else { return false };
    let Ok(list) = gsub.feature_list() else {
        return false;
    };
    list.feature_records().iter().any(|r| {
        let t = r.feature_tag();
        [b"liga", b"clig", b"calt", b"dlig", b"rlig"]
            .iter()
            .any(|x| t == Tag::new(x))
    })
}

impl Face {
    /// Parses and prepares a face. `ttc_index` is 0 for a plain font file.
    pub fn from_bytes(id: FaceId, data: Arc<[u8]>, ttc_index: u32) -> Result<Self, FaceError> {
        let font = FontRef::from_index(&data, ttc_index).map_err(|_| FaceError::Parse)?;
        let head = font.head().map_err(|_| FaceError::Parse)?;
        let units_per_em = head.units_per_em();
        if units_per_em == 0 || font.charmap().mappings().next().is_none() {
            return Err(FaceError::NotATextFace);
        }
        let attrs = font.attributes();
        let family = family_name(&font).unwrap_or_default();
        let variable = !font.axes().is_empty();
        let color = color_sources(&font);
        let ligature_features = gsub_has_ligature_features(&font);
        let shaper_data = harfrust::ShaperData::new(&font);
        let cm = font.charmap();
        let latin: Box<[u16]> = (0..LATIN_LEN)
            .map(|cp| {
                cm.map(cp)
                    .map_or(0, |g| u16::try_from(g.to_u32()).unwrap_or(0))
            })
            .collect();
        Ok(Self {
            id,
            uid: NEXT_UID.fetch_add(1, Ordering::Relaxed),
            weight: attrs.weight.value().round().clamp(1.0, 1000.0) as u16,
            italic: !matches!(attrs.style, skrifa::attribute::Style::Normal),
            data,
            ttc_index,
            family,
            units_per_em,
            variable,
            color,
            ligature_features,
            latin,
            shaper_data,
        })
    }

    /// Identifier.
    pub fn id(&self) -> FaceId {
        self.id
    }

    /// Process-unique identifier: never reused, so safe as a cache key across font systems.
    pub fn uid(&self) -> u64 {
        self.uid
    }

    /// Family name (typographic family when present).
    pub fn family(&self) -> &str {
        &self.family
    }

    /// Design units per em.
    pub fn units_per_em(&self) -> u16 {
        self.units_per_em
    }

    /// OS/2 weight class.
    pub fn weight(&self) -> u16 {
        self.weight
    }

    /// True for italic or oblique faces.
    pub fn is_italic(&self) -> bool {
        self.italic
    }

    /// True if the face has an `fvar`.
    pub fn is_variable(&self) -> bool {
        self.variable
    }

    /// Colour sources.
    pub fn color(&self) -> ColorSources {
        self.color
    }

    /// True if GSUB carries `liga`, `clig`, `calt`, `dlig` or `rlig`. When false and no style
    /// feature is set, a run that maps 1:1 can skip the shaper (FONTS-12).
    pub fn has_ligature_features(&self) -> bool {
        self.ligature_features
    }

    /// The raw font file bytes.
    pub fn data(&self) -> &Arc<[u8]> {
        &self.data
    }

    /// Collection index.
    pub fn ttc_index(&self) -> u32 {
        self.ttc_index
    }

    /// The skrifa/read-fonts view. `None` only if the bytes stopped parsing, which cannot
    /// happen after a successful [`Face::from_bytes`] (the data is immutable).
    pub fn font(&self) -> Option<FontRef<'_>> {
        FontRef::from_index(&self.data, self.ttc_index).ok()
    }

    /// The swash view of the same bytes.
    pub fn swash_font(&self) -> Option<swash::FontRef<'_>> {
        swash::FontRef::from_index(&self.data, self.ttc_index as usize)
    }

    pub(crate) fn shaper_data(&self) -> &harfrust::ShaperData {
        &self.shaper_data
    }

    /// Nominal glyph for a code point below U+0300, from the table built at load (no cmap
    /// walk). `None` if unmapped or out of range.
    pub fn latin_glyph(&self, cp: u32) -> Option<u32> {
        match self.latin.get(cp as usize) {
            Some(&g) if g != 0 => Some(u32::from(g)),
            _ => None,
        }
    }

    /// Nominal glyph for a code point.
    pub fn glyph_for(&self, cp: u32) -> Option<u32> {
        self.font()?.charmap().map(cp).map(|g| g.to_u32())
    }

    /// Glyph for a base character plus variation selector, falling back to the nominal glyph
    /// only if the font has no variation map at all.
    pub fn glyph_for_variant(&self, cp: u32, vs: u32) -> Option<u32> {
        let font = self.font()?;
        let cm = font.charmap();
        if cm.has_variant_map() {
            match cm.map_variant(cp, vs)? {
                skrifa::charmap::MapVariant::UseDefault => cm.map(cp).map(|g| g.to_u32()),
                skrifa::charmap::MapVariant::Variant(g) => Some(g.to_u32()),
            }
        } else {
            cm.map(cp).map(|g| g.to_u32())
        }
    }

    /// Advance of a glyph in design units.
    pub fn advance_units(&self, glyph: u32) -> Option<f32> {
        let font = self.font()?;
        font.glyph_metrics(Size::unscaled(), LocationRef::default())
            .advance_width(skrifa::GlyphId::new(glyph))
    }
}

/// A face at one style: variation location, synthetic flags, a stable cache key.
#[derive(Clone)]
pub struct FaceInstance {
    face: Arc<Face>,
    location: Location,
    swash_coords: Vec<i16>,
    shaper_instance: harfrust::ShaperInstance,
    synthetic_bold: bool,
    synthetic_italic: bool,
    key: u64,
}

impl fmt::Debug for FaceInstance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FaceInstance")
            .field("face", &self.face.id)
            .field("synthetic_bold", &self.synthetic_bold)
            .field("synthetic_italic", &self.synthetic_italic)
            .finish_non_exhaustive()
    }
}

impl FaceInstance {
    /// Builds an instance. `axes` are applied to the shaper instance and, identically, to the
    /// skrifa/swash outline location (FONTS-05). A named `instance` is applied first, then
    /// `axes` override individual axes; `default_axes` (automatic `wght`) apply below both.
    pub fn new(
        face: Arc<Face>,
        instance: Option<&str>,
        axes: &[AxisSetting],
        default_axes: &[AxisSetting],
        synthetic_bold: bool,
        synthetic_italic: bool,
    ) -> Self {
        let mut location = Location::default();
        if let Some(font) = face.font() {
            let collection = font.axes();
            if !collection.is_empty() {
                let mut settings: Vec<(Tag, f32)> = Vec::new();
                for a in default_axes {
                    settings.push((Tag::new(&a.tag), a.value));
                }
                if let Some(name) = instance {
                    let want = name.trim().to_ascii_lowercase();
                    for ni in font.named_instances().iter() {
                        let n = name_string(&font, ni.subfamily_name_id());
                        if n.is_some_and(|n| n.to_ascii_lowercase() == want) {
                            for (axis, v) in collection.iter().zip(ni.user_coords()) {
                                settings.push((axis.tag(), v));
                            }
                            break;
                        }
                    }
                }
                for a in axes {
                    settings.push((Tag::new(&a.tag), a.value));
                }
                location = collection.location(settings);
            }
        }
        let swash_coords: Vec<i16> = location.coords().iter().map(|c| c.to_bits()).collect();
        let shaper_instance = match face.font() {
            Some(font) if !location.coords().is_empty() => {
                harfrust::ShaperInstance::from_coords(&font, location.coords().iter().copied())
            }
            _ => harfrust::ShaperInstance::default(),
        };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        face.id.hash(&mut h);
        swash_coords.hash(&mut h);
        synthetic_bold.hash(&mut h);
        synthetic_italic.hash(&mut h);
        let key = h.finish();
        Self {
            face,
            location,
            swash_coords,
            shaper_instance,
            synthetic_bold,
            synthetic_italic,
            key,
        }
    }

    /// Instance with no variation and no synthetic styling.
    pub fn plain(face: Arc<Face>) -> Self {
        Self::new(face, None, &[], &[], false, false)
    }

    /// The face.
    pub fn face(&self) -> &Arc<Face> {
        &self.face
    }

    /// Normalized skrifa location (empty for non-variable faces).
    pub fn location(&self) -> &Location {
        &self.location
    }

    /// Location as swash wants it.
    pub fn swash_coords(&self) -> &[i16] {
        &self.swash_coords
    }

    pub(crate) fn shaper_instance(&self) -> &harfrust::ShaperInstance {
        &self.shaper_instance
    }

    /// Embolden because bold was wanted and the face is lighter.
    pub fn synthetic_bold(&self) -> bool {
        self.synthetic_bold
    }

    /// Skew because italic was wanted and the face is upright.
    pub fn synthetic_italic(&self) -> bool {
        self.synthetic_italic
    }

    /// Hash of (face, location, synthetic flags): the face half of every cache key.
    pub fn key(&self) -> u64 {
        self.key
    }
}
