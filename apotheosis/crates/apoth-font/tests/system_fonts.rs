// SPDX-License-Identifier: AGPL-3.0-only
//! End-to-end checks on a real font file. The fixture is Fira Mono Medium (SIL OFL 1.1) under
//! tests/fixtures; T-06's JetBrains Mono / Instrument Sans embeds are not in the tree yet. The
//! tests never depend on host fonts.

use apoth_font::config::{FontConfig, Ligatures, Style};
use apoth_font::discovery::FontIndex;
use apoth_font::raster::{GlyphCache, RasterParams, Rasterizer};
use apoth_font::shape::{GlyphSource, RowCluster, RowShaper, ShapedRow};
use apoth_font::system::{EmbeddedFont, FontSystem, Rule};
use std::sync::Arc;

/// The embedded fixture (SIL OFL 1.1, see tests/fixtures/README.md): no host font dependency.
fn font_or_skip() -> Option<(String, Vec<u8>)> {
    Some((
        "FiraMono-Medium.ttf".to_string(),
        include_bytes!("fixtures/FiraMono-Medium.ttf").to_vec(),
    ))
}

fn system_with(font: &(String, Vec<u8>), config: FontConfig) -> FontSystem {
    let emb = EmbeddedFont {
        name: font.0.clone(),
        data: Arc::from(font.1.clone()),
    };
    FontSystem::new(FontIndex::default(), vec![emb], config)
}

fn row<'a>(text: &'a [&'a str]) -> Vec<RowCluster<'a>> {
    text.iter()
        .enumerate()
        .map(|(i, t)| RowCluster {
            col: i as u16,
            cells: 1,
            text: t,
            style: Style::Regular,
        })
        .collect()
}

#[test]
fn metrics_are_integer_and_inside_the_cell() {
    let Some(font) = font_or_skip() else { return };
    let mut fs = system_with(&font, FontConfig::default());
    for px in [12.0f32, 15.0, 18.0, 24.0] {
        let m = fs.cell_metrics(px).expect("metrics");
        assert!(m.width > 0 && m.height > 0 && m.baseline_from_bottom < m.height);
        assert!(
            m.width as f32 <= px * 1.2,
            "monospace cell narrower than 1.2 em"
        );
        assert!(
            m.underline_thickness >= 1 && m.strikethrough_thickness >= 1 && m.cursor_thickness >= 1
        );
    }
}

#[test]
fn shaping_covers_every_column_and_is_repeatable() {
    let Some(font) = font_or_skip() else { return };
    let mut fs = system_with(&font, FontConfig::default());
    let mut shaper = RowShaper::new();
    let text = ["f", "i", " ", "=", ">", " ", "-", ">", "a"];
    let clusters = row(&text);
    let mut a = ShapedRow::default();
    let mut b = ShapedRow::default();
    shaper.shape_row(&mut fs, 16.0, &clusters, None, &mut a);
    let after_first = shaper.cache().len();
    shaper.shape_row(&mut fs, 16.0, &clusters, None, &mut b);
    assert_eq!(shaper.cache().len(), after_first, "second pass is all hits");
    assert_eq!(a.glyphs, b.glyphs);
    let covered: u32 = a.glyphs.iter().map(|g| u32::from(g.span)).sum();
    assert_eq!(covered as usize, text.len());
    assert!(
        a.glyphs
            .iter()
            .all(|g| matches!(g.source, GlyphSource::Glyph(_)))
    );
}

#[test]
fn ligature_policy_off_never_fuses() {
    let Some(font) = font_or_skip() else { return };
    let cfg = FontConfig {
        ligatures: Ligatures::Off,
        ..FontConfig::default()
    };
    let mut fs = system_with(&font, cfg);
    let mut shaper = RowShaper::new();
    let clusters = row(&["=", "=", ">", "-", ">"]);
    let mut out = ShapedRow::default();
    shaper.shape_row(&mut fs, 16.0, &clusters, None, &mut out);
    assert!(out.glyphs.iter().all(|g| g.span == 1));
}

#[test]
fn box_drawing_is_a_sprite_and_unknown_is_tofu() {
    let Some(font) = font_or_skip() else { return };
    let mut fs = system_with(&font, FontConfig::default());
    let mut shaper = RowShaper::new();
    let clusters = row(&["\u{2500}", "\u{10FFFE}", "a"]);
    let mut out = ShapedRow::default();
    shaper.shape_row(&mut fs, 16.0, &clusters, None, &mut out);
    assert_eq!(out.glyphs[0].source, GlyphSource::Sprite(0x2500));
    assert_eq!(out.glyphs[1].source, GlyphSource::Tofu(0x10FFFE));
    assert!(matches!(out.glyphs[2].source, GlyphSource::Glyph(_)));
    let r = fs.resolve(Style::Regular, "\u{10FFFE}");
    assert_eq!(r.rule, Rule::Tofu);
    assert!(r.face.is_none());
}

#[test]
fn grayscale_raster_is_cached_and_nonblank() {
    let Some(font) = font_or_skip() else { return };
    let mut fs = system_with(&font, FontConfig::default());
    let inst = fs.primary(Style::Regular).expect("primary");
    let gid = inst.face().glyph_for(u32::from('a')).expect("a is mapped");
    let params = RasterParams::grid(16.0, apoth_font::config::Hinting::Light, 0.0);
    let (mut r, mut cache) = (Rasterizer::new(), GlyphCache::new());
    let g1 = cache.gray(&mut r, &inst, gid, &params).expect("outline");
    assert!(!g1.is_blank());
    let g2 = cache.gray(&mut r, &inst, gid, &params).expect("cached");
    assert!(Arc::ptr_eq(&g1, &g2));
    assert_eq!(cache.rasterized, 1);
    // Four sub-pixel bins are distinct cache entries.
    let binned = RasterParams {
        subpixel_bin: 2,
        ..params
    };
    cache.gray(&mut r, &inst, gid, &binned).expect("binned");
    assert_eq!(cache.rasterized, 2);
}

#[test]
fn coverage_index_cache_invalidates_on_size_or_mtime() {
    let Some(font) = font_or_skip() else { return };
    let tmp = std::env::temp_dir().join(format!("apoth-font-test-{}", std::process::id()));
    let dir = tmp.join("fonts");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("Fixture.ttf");
    std::fs::write(&file, &font.1).unwrap();
    let cache = tmp.join("index.bin");
    let dirs = vec![dir.clone()];

    let (idx, s1) = FontIndex::load_or_build(&dirs, &cache);
    assert_eq!((s1.parsed, s1.reused), (1, 0));
    assert_eq!(idx.face_count(), 1);
    assert!(idx.faces_covering(u32::from('a')).next().is_some());
    assert!(idx.faces_covering(0x10FFFE).next().is_none());

    let (_, s2) = FontIndex::load_or_build(&dirs, &cache);
    assert_eq!(
        (s2.parsed, s2.reused),
        (0, 1),
        "unchanged file is not re-read"
    );

    // A trailing byte changes the size: the entry is parsed again.
    let mut grown = font.1.clone();
    grown.push(0);
    std::fs::write(&file, &grown).unwrap();
    let (_, s3) = FontIndex::load_or_build(&dirs, &cache);
    assert_eq!((s3.parsed, s3.reused), (1, 0));

    // A new file appears next to it.
    std::fs::write(dir.join("Second.ttf"), &font.1).unwrap();
    let (idx, s4) = FontIndex::load_or_build(&dirs, &cache);
    assert_eq!((s4.parsed, s4.reused), (1, 1));
    assert_eq!(idx.face_count(), 2);

    let _ = std::fs::remove_dir_all(&tmp);
}
