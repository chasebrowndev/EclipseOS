// SPDX-License-Identifier: AGPL-3.0-only
//! The fonts shipped inside the binary (T-06 §1): JetBrains Mono (the terminal grid's last resort) and
//! Onest (the interface face, tabular digits baked in) from the repository's `assets/fonts/`. Both families are SIL
//! OFL 1.1 data assets (licence texts beside them); they are bytes, not linked code, so they
//! are outside `cargo deny`'s licence graph.

use crate::system::EmbeddedFont;
use std::sync::Arc;

static JETBRAINS_MONO_REGULAR: &[u8] =
    include_bytes!("../../../../assets/fonts/JetBrainsMono-Regular.ttf");
static JETBRAINS_MONO_MEDIUM: &[u8] =
    include_bytes!("../../../../assets/fonts/JetBrainsMono-Medium.ttf");
static ONEST_REGULAR: &[u8] = include_bytes!("../../../../assets/fonts/Onest-Regular.ttf");
static ONEST_MEDIUM: &[u8] = include_bytes!("../../../../assets/fonts/Onest-Medium.ttf");
static ONEST_SEMIBOLD: &[u8] = include_bytes!("../../../../assets/fonts/Onest-SemiBold.ttf");

/// The family name the Onest files carry (name ID 1): the interface font.
pub const UI_FAMILY: &str = "Onest";

const EMBEDDED: [(&str, &[u8]); 5] = [
    ("JetBrainsMono-Regular.ttf", JETBRAINS_MONO_REGULAR),
    ("JetBrainsMono-Medium.ttf", JETBRAINS_MONO_MEDIUM),
    ("Onest-Regular.ttf", ONEST_REGULAR),
    ("Onest-Medium.ttf", ONEST_MEDIUM),
    ("Onest-SemiBold.ttf", ONEST_SEMIBOLD),
];

/// Identity of the embedded set (names and sizes): a cache keyed on it rebuilds if the set
/// changes between builds.
pub fn embedded_id() -> u64 {
    static ID: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *ID.get_or_init(|| {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for (name, data) in EMBEDDED {
            for b in name.bytes().chain((data.len() as u64).to_le_bytes()) {
                h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
            }
        }
        h
    })
}

/// The embedded faces in fallback order: monospace first so it is the terminal's last resort.
pub fn embedded_fonts() -> Vec<EmbeddedFont> {
    EMBEDDED
        .into_iter()
        .map(|(name, data)| EmbeddedFont {
            name: name.into(),
            data: Arc::from(data),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fonts_are_present_and_sfnt() {
        let f = embedded_fonts();
        assert_eq!(f.len(), 5);
        for e in &f {
            assert!(e.data.len() > 10_000, "{} is empty", e.name);
            assert!(
                matches!(&e.data[..4], [0, 1, 0, 0] | b"OTTO" | b"true"),
                "{} is not an sfnt",
                e.name
            );
        }
    }
}
