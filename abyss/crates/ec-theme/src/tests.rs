// SPDX-License-Identifier: AGPL-3.0-only
use super::*;

fn hex(h: u32) -> Rgba {
    Rgba([
        ((h >> 16) & 0xff) as f32 / 255.0,
        ((h >> 8) & 0xff) as f32 / 255.0,
        (h & 0xff) as f32 / 255.0,
        1.0,
    ])
}
fn white(a: f32) -> Rgba {
    Rgba([1.0, 1.0, 1.0, a])
}
fn at(c: Rgba, a: f32) -> Rgba {
    Rgba([c.0[0], c.0[1], c.0[2], a])
}
fn black(a: f32) -> Rgba {
    Rgba([0.0, 0.0, 0.0, a])
}

/// Every token as the DE shipped it before themes (literal values from the
/// old `tokens::color`). Exact equality, not approximate.
#[test]
fn dark_golden() {
    let p = derive(&DARK_SEEDS);
    let base = hex(0x0b0906);
    let s1 = hex(0x1a1712);
    let ground = hex(0x17140f);
    let deep = at(ground, 0.62);
    let sidebar = black(0.4);
    let content = black(0.28);
    let accent = hex(0xf2c33c);
    let danger = hex(0xe0553f);
    let cases: [(&str, Rgba, Rgba); 52] = [
        ("base", p.base, base),
        ("surface_0", p.surface_0, hex(0x12100b)),
        ("surface_1", p.surface_1, s1),
        ("surface_2", p.surface_2, hex(0x1c1913)),
        ("glass", p.glass, white(0.045)),
        ("glass_strong", p.glass_strong, white(0.06)),
        ("glass_deep", p.glass_deep, deep),
        ("glass_deep_backed", p.glass_deep_backed, deep.over_opaque(s1)),
        ("sheet_tint", p.sheet_tint, at(ground, 0.34)),
        ("bar_tint", p.bar_tint, at(ground, 0.22)),
        ("cell", p.cell, white(0.006)),
        ("cell_hover", p.cell_hover, white(0.012)),
        ("cell_press", p.cell_press, white(0.02)),
        ("cell_rim", p.cell_rim, white(0.016)),
        ("cell_rim_hover", p.cell_rim_hover, white(0.026)),
        ("cell_rim_away", p.cell_rim_away, white(0.006)),
        ("cell_shadow", p.cell_shadow, black(0.30)),
        ("cell_focus", p.cell_focus, white(0.014)),
        ("cell_focus_hover", p.cell_focus_hover, white(0.02)),
        ("cell_focus_press", p.cell_focus_press, white(0.028)),
        ("cell_focus_rim", p.cell_focus_rim, white(0.04)),
        ("cell_focus_rim_hover", p.cell_focus_rim_hover, white(0.055)),
        ("menu_ground", p.menu_ground, ground),
        ("border", p.border, white(0.10)),
        ("border_strong", p.border_strong, white(0.16)),
        ("hairline", p.hairline, white(0.055)),
        ("highlight", p.highlight, white(0.12)),
        ("highlight_soft", p.highlight_soft, white(0.07)),
        ("highlight_strong", p.highlight_strong, white(0.20)),
        ("lift", p.lift, white(0.08)),
        ("lift_strong", p.lift_strong, white(0.13)),
        ("lift_soft", p.lift_soft, white(0.035)),
        ("veil", p.veil, at(base, 0.62)),
        ("veil_glass", p.veil_glass, black(0.28 * 0.62)),
        ("sidebar", p.sidebar, sidebar),
        ("sidebar_backed", p.sidebar_backed, sidebar.over_opaque(base)),
        ("content", p.content, content),
        ("content_backed", p.content_backed, content.over_opaque(base)),
        ("accent", p.accent, accent),
        ("accent_text", p.accent_text, hex(0xf5cf5c)),
        ("accent_fill", p.accent_fill, at(accent, 0.09)),
        ("accent_border", p.accent_border, at(accent, 0.28)),
        ("accent_wash", p.accent_wash, at(accent, 0.035)),
        ("text", p.text, white(1.0)),
        ("text_secondary", p.text_secondary, white(0.64)),
        ("text_tertiary", p.text_tertiary, white(0.40)),
        ("neutral", p.neutral, hex(0x96918a)),
        ("eye_iris", p.eye_iris, white(0.014)),
        ("control_off", p.control_off, white(0.13)),
        ("track", p.track, white(0.12)),
        ("danger", p.danger, danger),
        ("danger_fill", p.danger_fill, at(danger, 0.14)),
    ];
    for (name, got, want) in cases {
        assert_eq!(got.0.map(f32::to_bits), want.0.map(f32::to_bits), "{name}");
    }
    assert_eq!(p.connected, hex(0x4f9fe0));
    assert_eq!(p.ok, hex(0x7fae5e));
}

#[test]
fn builtins_classify_and_validate() {
    assert_eq!(DARK_SEEDS.scheme(), Scheme::Dark);
    assert_eq!(LIGHT_SEEDS.scheme(), Scheme::Light);
    assert_eq!(validate(&DARK_SEEDS), Ok(()));
    assert_eq!(validate(&LIGHT_SEEDS), Ok(()));
}

#[test]
fn low_contrast_fails() {
    let mut s = DARK_SEEDS;
    s.ink = Rgba::rgb(0x2a2620);
    assert!(matches!(
        validate(&s),
        Err(ThemeError::LowContrast { what: "text", .. })
    ));
    let mut s = LIGHT_SEEDS;
    s.accent_text = Some(Rgba::rgb(0xf2c33c));
    assert!(matches!(
        validate(&s),
        Err(ThemeError::LowContrast {
            what: "accent text",
            ..
        })
    ));
}

#[test]
fn bad_values_fail() {
    let mut s = DARK_SEEDS;
    s.accent = Rgba::new(f32::NAN, 0.0, 0.0, 1.0);
    assert_eq!(validate(&s), Err(ThemeError::NonFinite("accent")));
    let mut s = DARK_SEEDS;
    s.rim = 2.0;
    assert_eq!(validate(&s), Err(ThemeError::OutOfRange("rim")));
    assert!(!format!("{}", ThemeError::OutOfRange("rim")).is_empty());
}

#[test]
fn contrast_extremes() {
    let c = contrast(Rgba::rgb(0xffffff), Rgba::rgb(0x000000));
    assert!((c - 21.0).abs() < 0.01);
    assert!((srgb_to_linear(linear_to_srgb(0.3)) - 0.3).abs() < 1e-5);
}

#[test]
fn custom_base_shifts_surfaces() {
    let mut s = DARK_SEEDS;
    s.base = Rgba::rgb(0x100d08);
    let p = derive(&s);
    assert_ne!(p.surface_1, derive(&DARK_SEEDS).surface_1);
    assert_eq!(p.surface_1.a(), 1.0);
}

/// The control grounds that used to be literal `Color::WHITE` alphas in
/// `ec-ui`; dark must keep exactly those numbers.
#[test]
fn dark_control_grounds_match_the_old_literals() {
    let p = derive(&DARK_SEEDS);
    assert_eq!(p.scheme, Scheme::Dark);
    assert_eq!(p.on_accent, hex(0x0b0906));
    for (name, got, want) in [
        ("nav_active", p.nav_active, white(0.07)),
        ("nav_hover", p.nav_hover, white(0.04)),
        ("pill", p.pill, white(0.035)),
        ("pill_hover", p.pill_hover, white(0.07)),
        ("input_fill", p.input_fill, white(0.04)),
        ("scroller", p.scroller, white(0.13)),
        ("scroller_hover", p.scroller_hover, white(0.22)),
        ("chart_history", p.chart_history, white(0.13)),
        ("control_off_hover", p.control_off_hover, white(0.18)),
        ("knob_off", p.knob_off, white(0.62)),
        ("highlight_rim", p.highlight_rim, p.highlight_strong),
    ] {
        assert_eq!(got, want, "{name}");
    }
}

#[test]
fn light_ink_on_accent_is_the_ink() {
    let p = derive(&LIGHT_SEEDS);
    assert_eq!(p.scheme, Scheme::Light);
    assert_eq!(p.on_accent, hex(0x1a1610));
    assert!(contrast(p.on_accent, p.accent) >= MIN_TEXT_CONTRAST);
}

/// What is on screen, not the CSS figure: the light ink alphas are brought to
/// linear light, so secondary text keeps its contrast on the base.
#[test]
fn light_text_reads_on_the_base() {
    let p = derive(&LIGHT_SEEDS);
    assert!(contrast(p.text, p.base) >= 12.0);
    assert!(contrast(p.text_secondary, p.base) >= MIN_TEXT_CONTRAST);
    assert!(contrast(p.text_tertiary, p.base) >= 2.5);
    assert!(contrast(p.accent_text, p.base) >= MIN_TEXT_CONTRAST);
    assert!(contrast(p.danger, p.base) >= MIN_TEXT_CONTRAST);
    assert!(contrast(p.neutral, p.base) >= MIN_TEXT_CONTRAST);
}

#[test]
fn linear_alpha_is_the_identity_at_the_ends_and_stronger_for_dark_ink() {
    let ink = LIGHT_SEEDS.ink;
    let base = LIGHT_SEEDS.base;
    assert_eq!(linear_alpha(ink, base, 0.0), 0.0);
    assert!((linear_alpha(ink, base, 1.0) - 1.0).abs() < 1e-5);
    assert!(linear_alpha(ink, base, 0.62) > 0.8);
}
