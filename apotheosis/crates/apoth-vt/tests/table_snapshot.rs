// SPDX-License-Identifier: Apache-2.0
//! The state table is a reviewed artefact: any transition change shows up as a diff of
//! `tests/table_snapshot.txt`. Regenerate with `UPDATE_SNAPSHOT=1 cargo test -p apoth-vt`.

use std::path::PathBuf;

#[test]
fn table_matches_checked_in_snapshot() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/table_snapshot.txt");
    let rendered = apoth_vt::render_table();
    if std::env::var_os("UPDATE_SNAPSHOT").is_some() {
        std::fs::write(&path, &rendered).expect("write snapshot");
        return;
    }
    let want = std::fs::read_to_string(&path)
        .expect("tests/table_snapshot.txt missing: run once with UPDATE_SNAPSHOT=1 and review it");
    assert_eq!(
        rendered, want,
        "state table changed; review and re-bless with UPDATE_SNAPSHOT=1"
    );
}
