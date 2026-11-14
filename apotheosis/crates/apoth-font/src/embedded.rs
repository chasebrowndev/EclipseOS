// SPDX-License-Identifier: AGPL-3.0-only
//! The fonts shipped inside the binary (T-06 §1): JetBrains Mono and Instrument Sans from the
//! repository's `assets/fonts/`, the terminal fallback of every chain. Both families are SIL
//! OFL 1.1 data assets (licence texts beside them); they are bytes, not linked code, so they
//! are outside `cargo deny`'s licence graph.

use crate::system::EmbeddedFont;
use std::sync::Arc;

static JETBRAINS_MONO_REGULAR: &[u8] =
    include_bytes!("../../../../assets/fonts/JetBrainsMono-Regular.ttf");
static JETBRAINS_MONO_MEDIUM: &[u8] =
    include_bytes!("../../../../assets/fonts/JetBrainsMono-Medium.ttf");
static INSTRUMENT_SANS_REGULAR: &[u8] =
    include_bytes!("../../../../assets/fonts/InstrumentSans-Regular.ttf");

/// The embedded faces in fallback order: monospace first so it is the terminal's last resort.
pub fn embedded_fonts() -> Vec<EmbeddedFont> {
    [
        ("JetBrainsMono-Regular.ttf", JETBRAINS_MONO_REGULAR),
        ("JetBrainsMono-Medium.ttf", JETBRAINS_MONO_MEDIUM),
        ("InstrumentSans-Regular.ttf", INSTRUMENT_SANS_REGULAR),
    ]
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
        assert_eq!(f.len(), 3);
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
