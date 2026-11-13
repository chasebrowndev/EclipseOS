// SPDX-License-Identifier: Apache-2.0
//! The committed `src/tables.rs` must equal what the generator renders from `data/ucd`.

mod common;

#[test]
fn committed_tables_are_current() {
    let fresh = apoth_unicode::generator::generate(&common::ucd_dir()).expect("generate");
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/tables.rs");
    let committed = std::fs::read_to_string(&path).expect("read committed tables.rs");
    assert!(
        committed == fresh,
        "src/tables.rs is stale: run `cargo run -p apoth-unicode --bin gen-tables` from apotheosis/"
    );
}

#[test]
fn unicode_version_matches_data() {
    let eaw = common::ucd("EastAsianWidth.txt");
    let v = apoth_unicode::generator::unicode_version(&eaw).expect("version");
    assert_eq!(apoth_unicode::unicode_version(), v);
}
