// SPDX-License-Identifier: AGPL-3.0-only
//! The Apotheosis terminal emulator.
//!
//! Governing spec: T-00 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`).
#![forbid(unsafe_code)]

fn main() {
    eprintln!(
        "apotheosis {}: not yet implemented (T-00 §7, M11)",
        env!("CARGO_PKG_VERSION")
    );
    std::process::exit(1);
}
