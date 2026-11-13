// SPDX-License-Identifier: Apache-2.0
//! Shared helpers for the integration tests.

use std::path::PathBuf;

/// `apoth-unicode/data/ucd`, resolved from this crate's manifest directory.
pub(crate) fn ucd_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/ucd");
    assert!(
        dir.is_dir(),
        "UCD data missing at {}: see data/ucd/README.md",
        dir.display()
    );
    dir
}

/// Read a UCD file by name.
pub(crate) fn ucd(name: &str) -> String {
    apoth_unicode::generator::read(&ucd_dir(), name).unwrap_or_else(|e| panic!("{e}"))
}
