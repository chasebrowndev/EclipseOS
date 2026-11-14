// SPDX-License-Identifier: AGPL-3.0-only
//! T-08 §6 lint: no colour literal outside `tokens.rs`; no gradient/blur/glow primitive.

const SOURCES: &[(&str, &str)] = &[
    ("bar.rs", include_str!("bar.rs")),
    ("draw.rs", include_str!("draw.rs")),
    ("frame.rs", include_str!("frame.rs")),
    ("geom.rs", include_str!("geom.rs")),
    ("hit.rs", include_str!("hit.rs")),
    ("motion.rs", include_str!("motion.rs")),
    ("palette.rs", include_str!("palette.rs")),
    ("pane.rs", include_str!("pane.rs")),
    ("scrollbar.rs", include_str!("scrollbar.rs")),
    ("searchbar.rs", include_str!("searchbar.rs")),
    ("state.rs", include_str!("state.rs")),
    ("tabs.rs", include_str!("tabs.rs")),
    ("text.rs", include_str!("text.rs")),
    ("tooltip.rs", include_str!("tooltip.rs")),
    ("lib.rs", include_str!("lib.rs")),
];

fn has_hex_literal(line: &str) -> bool {
    let b = line.as_bytes();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'0' && b[i + 1] == b'x' {
            let n = b[i + 2..]
                .iter()
                .take_while(|c| c.is_ascii_hexdigit())
                .count();
            if n >= 6 {
                return true;
            }
        }
        i += 1;
    }
    false
}

#[test]
fn no_colour_literals_outside_tokens() {
    for (name, src) in SOURCES {
        for (n, line) in src.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            assert!(!code.contains("Rgba(["), "{name}:{} colour literal", n + 1);
            assert!(
                !code.contains("Rgba::hex"),
                "{name}:{} colour literal",
                n + 1
            );
            assert!(!has_hex_literal(code), "{name}:{} hex literal", n + 1);
        }
    }
}

#[test]
fn no_forbidden_primitives() {
    for (name, src) in SOURCES {
        let lower = src.to_ascii_lowercase();
        for bad in ["gradient", "blur(", "bloom", "halo"] {
            for line in lower.lines() {
                let code = line.split("//").next().unwrap_or("");
                assert!(!code.contains(bad), "{name}: forbidden primitive `{bad}`");
            }
        }
    }
}
