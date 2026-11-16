// SPDX-License-Identifier: AGPL-3.0-only
//! The shipped embedded fonts (T-06 §1, §4, §9): fallback selection and shaping snapshots with
//! no host font and no test fixture.

use apoth_font::config::{FontConfig, Ligatures, Style};
use apoth_font::discovery::FontIndex;
use apoth_font::embedded::embedded_fonts;
use apoth_font::shape::{GlyphSource, RowCluster, RowShaper, ShapedRow};
use apoth_font::system::{FontSystem, Rule};

fn system(config: FontConfig) -> FontSystem {
    FontSystem::new(FontIndex::default(), embedded_fonts(), config)
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
fn empty_system_resolves_ascii_to_the_embedded_face() {
    let mut fs = system(FontConfig::default());
    let r = fs.resolve(Style::Regular, "a");
    assert!(r.face.is_some(), "embedded font must cover ASCII");
    assert_eq!(r.rule, Rule::Embedded);
    let m = fs.cell_metrics(16.0).expect("metrics from embedded font");
    assert!(m.width > 0 && m.height > m.width);
}

#[test]
fn uncovered_code_point_is_tofu_not_a_borrowed_face() {
    let mut fs = system(FontConfig::default());
    let r = fs.resolve(Style::Regular, "\u{10FFFE}");
    assert_eq!(r.rule, Rule::Tofu);
    assert!(r.face.is_none());
}

#[test]
fn shaping_snapshot_is_one_cell_per_glyph_and_stable() {
    let mut fs = system(FontConfig::default());
    let mut shaper = RowShaper::new();
    let text = ["h", "e", "l", "l", "o", " ", "w"];
    let clusters = row(&text);
    let (mut a, mut b) = (ShapedRow::default(), ShapedRow::default());
    shaper.shape_row(&mut fs, 16.0, &clusters, None, &mut a);
    shaper.shape_row(&mut fs, 16.0, &clusters, None, &mut b);
    assert_eq!(a.glyphs, b.glyphs);
    assert_eq!(a.glyphs.len(), text.len());
    for (i, g) in a.glyphs.iter().enumerate() {
        assert_eq!((g.col as usize, g.span), (i, 1));
        assert!(matches!(g.source, GlyphSource::Glyph(_)));
    }
    // Same letter, same glyph id: the two `l` are identical.
    assert_eq!(a.glyphs[2].source, a.glyphs[3].source);
    assert_ne!(a.glyphs[0].source, a.glyphs[1].source);
}

#[test]
fn ligature_policy_controls_fusing_with_jetbrains_mono() {
    let clusters = row(&["=", "=", ">"]);
    let mut shaper = RowShaper::new();

    let mut on = system(FontConfig {
        ligatures: Ligatures::On,
        ..FontConfig::default()
    });
    let mut fused = ShapedRow::default();
    shaper.shape_row(&mut on, 16.0, &clusters, None, &mut fused);
    let covered: u32 = fused.glyphs.iter().map(|g| u32::from(g.span)).sum();
    assert_eq!(covered, 3, "every column is covered");

    let mut off = system(FontConfig {
        ligatures: Ligatures::Off,
        ..FontConfig::default()
    });
    let mut plain = ShapedRow::default();
    RowShaper::new().shape_row(&mut off, 16.0, &clusters, None, &mut plain);
    assert!(plain.glyphs.iter().all(|g| g.span == 1));
    // JetBrains Mono ligatures are contextual alternates: one glyph per cell,
    // but different glyph ids than the plain forms.
    let ids = |r: &ShapedRow| r.glyphs.iter().map(|g| g.source).collect::<Vec<_>>();
    assert_ne!(ids(&fused), ids(&plain), "calt changes the glyphs when on");
}

#[test]
fn builtin_glyphs_ignore_the_font() {
    let mut fs = system(FontConfig::default());
    let mut out = ShapedRow::default();
    let clusters = row(&["\u{2500}", "\u{E0B0}", "\u{1FB00}", "\u{2800}"]);
    RowShaper::new().shape_row(&mut fs, 16.0, &clusters, None, &mut out);
    for (g, cp) in out.glyphs.iter().zip([0x2500, 0xE0B0, 0x1FB00, 0x2800]) {
        assert_eq!(g.source, GlyphSource::Sprite(cp));
    }
}
