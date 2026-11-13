// SPDX-License-Identifier: Apache-2.0
//! Generated tables against the Unicode conformance files (T-04 §6) and the
//! `unicode-segmentation` oracle.

mod common;

use apoth_unicode::generator::parse_ranges;
use apoth_unicode::{Ambiguous, Widths, graphemes};
use unicode_segmentation::UnicodeSegmentation;

fn parse_break_line(line: &str) -> Option<(String, Vec<String>)> {
    let line = line.split('#').next()?.trim();
    if line.is_empty() {
        return None;
    }
    let mut text = String::new();
    let mut clusters = vec![String::new()];
    for tok in line.split_whitespace() {
        match tok {
            "\u{F7}" => {
                if clusters.last().is_some_and(|c| !c.is_empty()) {
                    clusters.push(String::new());
                }
            }
            "\u{D7}" => {}
            hex => {
                let c = char::from_u32(u32::from_str_radix(hex, 16).ok()?)?;
                text.push(c);
                clusters.last_mut()?.push(c);
            }
        }
    }
    if clusters.last().is_some_and(String::is_empty) {
        clusters.pop();
    }
    Some((text, clusters))
}

#[test]
fn grapheme_break_test_txt() {
    let mut n = 0;
    for line in common::ucd("GraphemeBreakTest.txt").lines() {
        let Some((text, want)) = parse_break_line(line) else {
            continue;
        };
        let got: Vec<&str> = graphemes(&text).collect();
        assert_eq!(got, want, "line: {line}");
        n += 1;
    }
    assert!(n > 500, "too few cases parsed: {n}");
}

#[test]
fn east_asian_width_txt() {
    let mut ignorable = vec![false; 0x11_0000];
    for (lo, hi, f) in parse_ranges(&common::ucd("DerivedCoreProperties.txt")) {
        if f.first().map(String::as_str) == Some("Default_Ignorable_Code_Point") {
            for cp in lo..=hi {
                ignorable[cp as usize] = true;
            }
        }
    }
    // Zero-width by general category (T-04: marks, format, controls, line/para separators).
    let mut start = None;
    for line in common::ucd("UnicodeData.txt").lines() {
        let f: Vec<&str> = line.split(';').collect();
        let cp = u32::from_str_radix(f[0], 16).expect("hex");
        let (lo, hi) = if f[1].ends_with(", First>") {
            start = Some(cp);
            continue;
        } else if f[1].ends_with(", Last>") {
            (start.take().unwrap_or(cp), cp)
        } else {
            (cp, cp)
        };
        if matches!(f[2], "Mn" | "Me" | "Cf" | "Zl" | "Zp" | "Cc") {
            for cp in lo..=hi {
                ignorable[cp as usize] = true;
            }
        }
    }
    let narrow = Widths::default();
    let wide = Widths {
        ambiguous: Ambiguous::Wide,
        ..Widths::default()
    };
    let mut seen = 0;
    for (lo, hi, f) in parse_ranges(&common::ucd("EastAsianWidth.txt")) {
        let class = f.first().map(String::as_str).unwrap_or("");
        for cp in lo..=hi {
            let Some(c) = char::from_u32(cp) else {
                continue;
            };
            if ignorable[cp as usize] {
                continue;
            }
            match class {
                "W" | "F" => {
                    assert_eq!(narrow.scalar_width(c), 2, "U+{cp:04X}");
                    assert_eq!(wide.scalar_width(c), 2, "U+{cp:04X}");
                    seen += 1;
                }
                "A" => {
                    let n = narrow.scalar_width(c);
                    let w = wide.scalar_width(c);
                    assert!(n <= 1, "ambiguous U+{cp:04X} must be narrow by default");
                    assert_eq!(w, if n == 0 { 0 } else { 2 }, "U+{cp:04X}");
                    seen += 1;
                }
                "Na" | "H" => {
                    // Narrow and halfwidth: one cell unless a control or mark.
                    assert!(narrow.scalar_width(c) <= 1, "U+{cp:04X}");
                    seen += 1;
                }
                _ => {}
            }
        }
    }
    assert!(seen > 10_000);
}

#[test]
fn emoji_test_txt() {
    let w = Widths::default();
    let mut fully = 0;
    for line in common::ucd("emoji-test.txt").lines() {
        let Some(body) = line.split('#').next() else {
            continue;
        };
        let mut it = body.split(';');
        let (Some(codes), Some(status)) = (it.next(), it.next()) else {
            continue;
        };
        let codes = codes.trim();
        if codes.is_empty() {
            continue;
        }
        let s: String = codes
            .split_whitespace()
            .map(|h| char::from_u32(u32::from_str_radix(h, 16).expect("hex")).expect("scalar"))
            .collect();
        let clusters: Vec<&str> = graphemes(&s).collect();
        assert_eq!(
            clusters.len(),
            1,
            "emoji-test sequence must be one cluster: {codes}"
        );
        if status.trim() == "fully-qualified" {
            assert_eq!(
                w.cluster_width(&s),
                2,
                "fully-qualified emoji is 2 cells: {codes}"
            );
            fully += 1;
        }
    }
    assert!(fully > 1000, "too few fully-qualified sequences: {fully}");
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn matches_unicode_segmentation_oracle() {
    // Scalars whose properties are stable across Unicode versions, so the oracle's version
    // does not matter. Indic conjunct scalars are covered by GraphemeBreakTest.txt above.
    let pool: Vec<char> = [
        'a',
        '1',
        '\r',
        '\n',
        '\0',
        '\u{301}',
        '\u{308}',
        '\u{200D}',
        '\u{200C}',
        '\u{1F468}',
        '\u{1F469}',
        '\u{1F3FB}',
        '\u{1F1E6}',
        '\u{1F1E7}',
        '\u{1F1E8}',
        '\u{FE0F}',
        '\u{2764}',
        '\u{AC00}',
        '\u{AC01}',
        '\u{1100}',
        '\u{1161}',
        '\u{11A8}',
        '\u{0600}',
        '\u{0903}',
        '\u{0E33}',
        '\u{20E3}',
        '\u{4E2D}',
        '\u{00A9}',
        '\u{E0067}',
        '\u{E007F}',
    ]
    .to_vec();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..20_000 {
        let len = 1 + (rng.next() % 12) as usize;
        let s: String = (0..len)
            .map(|_| pool[(rng.next() % pool.len() as u64) as usize])
            .collect();
        let ours: Vec<&str> = graphemes(&s).collect();
        let oracle: Vec<&str> = s.graphemes(true).collect();
        assert_eq!(
            ours,
            oracle,
            "input scalars: {:?}",
            s.chars().collect::<Vec<_>>()
        );
    }
}
