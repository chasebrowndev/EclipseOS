// SPDX-License-Identifier: Apache-2.0
//! Width corpus (T-04 §6): expected cell counts for ZWJ emoji, flags, keycaps, VS15/VS16,
//! Hangul and Indic conjuncts.
//!
//! Cross-check against kitty, ghostty and wezterm: NOT yet done. The `apoth-prior-art`
//! agent must supply their published widths; divergences are then listed here. Known
//! design choices that other terminals may differ on:
//! - a lone combining mark (no base) takes one cell;
//! - Default_Ignorable scalars (U+3164, U+FFA0, U+2060..) are zero width even when East
//!   Asian Wide;
//! - every Cf scalar, U+00AD SOFT HYPHEN included, is zero width;
//! - Hangul V/T jamo are zero width per scalar (L is two), so L+V+T clusters are 2 cells.

use apoth_unicode::{WidthMethod, Widths, graphemes};

fn clusters(s: &str) -> Vec<(&str, u8)> {
    let w = Widths::default();
    graphemes(s).map(|g| (g, w.cluster_width(g))).collect()
}

fn total(s: &str) -> u32 {
    clusters(s).iter().map(|&(_, w)| u32::from(w)).sum()
}

#[track_caller]
fn one(s: &str, cells: u8) {
    let c = clusters(s);
    assert_eq!(c.len(), 1, "expected one cluster for {s:?}, got {c:?}");
    assert_eq!(c[0].1, cells, "width of {s:?}");
}

#[test]
fn ascii_cjk_halfwidth() {
    one("a", 1);
    one("\u{4E2D}", 2);
    one("\u{FF21}", 2);
    one("\u{FF71}", 1);
    assert_eq!(total("a\u{4E2D}b"), 4);
}

#[test]
fn combining() {
    one("e\u{301}", 1);
    one("\u{301}", 1);
    one("\u{0E01}\u{0E47}", 1);
}

#[test]
fn zero_width_and_controls() {
    one("\u{200B}", 0);
    one("\u{2060}", 0);
    one("\r", 0);
    assert_eq!(clusters("\r\n"), [("\r\n", 0)]);
    one("\u{00AD}", 0);
}

#[test]
fn zwj_sequences() {
    one(
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}",
        2,
    );
    one("\u{1F3F3}\u{FE0F}\u{200D}\u{1F308}", 2);
    one("\u{1F468}\u{1F3FB}\u{200D}\u{1F4BB}", 2);
    one("\u{1F44D}\u{1F3FD}", 2);
}

#[test]
fn flags() {
    one("\u{1F1FA}\u{1F1F8}", 2);
    assert_eq!(clusters("\u{1F1FA}\u{1F1F8}\u{1F1EB}\u{1F1F7}").len(), 2);
    assert_eq!(total("\u{1F1FA}\u{1F1F8}\u{1F1EB}\u{1F1F7}"), 4);
    one("\u{1F1FA}", 2);
    one(
        "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}",
        2,
    );
}

#[test]
fn keycaps() {
    one("1\u{FE0F}\u{20E3}", 2);
    one("#\u{FE0F}\u{20E3}", 2);
    one("*\u{FE0F}\u{20E3}", 2);
}

#[test]
fn variation_selectors() {
    one("\u{2764}", 1);
    one("\u{2764}\u{FE0F}", 2);
    one("\u{263A}", 1);
    one("\u{263A}\u{FE0F}", 2);
    one("\u{231A}", 2);
    one("\u{231A}\u{FE0E}", 1);
    one("\u{1F600}\u{FE0E}", 1);
    one("A\u{FE0F}", 1);
    one("\u{4E2D}\u{FE0E}", 2);
}

#[test]
fn hangul() {
    one("\u{D55C}", 2);
    one("\u{1112}\u{1161}\u{11AB}", 2);
    one("\u{1100}", 2);
    assert_eq!(total("\u{D55C}\u{AE00}"), 4);
}

#[test]
fn indic_conjuncts() {
    one("\u{0915}\u{094D}\u{0937}", 1);
    one("\u{0915}\u{094D}\u{200D}\u{0937}", 1);
    one("\u{0938}\u{094D}\u{0924}\u{0947}", 1);
    assert_eq!(
        clusters("\u{0928}\u{092E}\u{0938}\u{094D}\u{0924}\u{0947}").len(),
        3
    );
}

#[test]
fn wcswidth_differs_on_zwj() {
    let w = Widths {
        method: WidthMethod::Wcswidth,
        ..Widths::default()
    };
    assert_eq!(w.cluster_width("\u{1F468}\u{200D}\u{1F469}"), 4);
    assert_eq!(w.cluster_width("\u{2764}\u{FE0F}"), 1);
}
