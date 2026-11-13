// SPDX-License-Identifier: AGPL-3.0-only
//! The golden corpus and the conformance table run against the real terminal (T-25 section 1).

use apoth_testkit::adapter::TermDriver;
use apoth_testkit::runner::{Outcome, run_fixture};
use apoth_testkit::{conformance, fixtures_dir, load_fixtures, oracle};

fn conformance_rows() -> Vec<conformance::Row> {
    let src =
        std::fs::read_to_string(fixtures_dir().join("conformance.kdl")).expect("conformance.kdl");
    conformance::parse(&src).expect("conformance table parses")
}

#[test]
fn every_fixture_passes_in_every_chunking() {
    let fixtures = load_fixtures(&fixtures_dir()).expect("fixtures load");
    assert!(
        fixtures.len() >= 900,
        "the corpus must not shrink below 900 (the M13 target of 1500 is the ignored test below), have {}",
        fixtures.len()
    );
    let mut failures = Vec::new();
    let mut skipped = 0usize;
    for fx in &fixtures {
        match run_fixture::<TermDriver>(fx) {
            Outcome::Pass => {}
            Outcome::Skipped(_) => skipped += 1,
            Outcome::Fail(report) => failures.push(format!("{}\n{report}", fx.path.display())),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} fixtures failed ({skipped} skipped):\n\n{}",
        failures.len(),
        fixtures.len(),
        failures.join("\n")
    );
}

/// T-25 section 1: 1500 golden fixtures at M13. The corpus is short of it (fixture authoring
/// is owed), so this runs only on request (`--ignored`) until the shortfall is closed; the
/// milestone is not done while it fails.
#[test]
#[ignore = "M13 target of 1500 fixtures not yet met; authoring owed"]
fn corpus_meets_the_m13_target() {
    let fixtures = load_fixtures(&fixtures_dir()).expect("fixtures load");
    assert!(
        fixtures.len() >= 1500,
        "T-25 section 1 sets 1500 at M13, have {}",
        fixtures.len()
    );
}

/// Rows up to this milestone must cite fixtures (M13 is the milestone that lands every R1
/// mode and sequence; the R2 rows of M31 are listed but not yet enforced).
const ENFORCED_MILESTONE: u32 = 13;

#[test]
fn conformance_table_cites_only_existing_fixtures_and_every_due_row_cites_some() {
    let fixtures = load_fixtures(&fixtures_dir()).expect("fixtures load");
    let problems = conformance::check(&conformance_rows(), &fixtures, ENFORCED_MILESTONE);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn conformance_table_has_no_globs_and_every_r1_row_is_there() {
    let rows = conformance_rows();
    assert!(
        rows.iter()
            .all(|r| r.fixtures.iter().all(|f| !f.contains('*')))
    );
    for want in [
        "T03-OSC7",
        "T03-OSC9-PROGRESS",
        "T03-MODE-2027",
        "T03-DECRQSS",
        "T03-SGR-AMBIG",
        "T03-OSC-CAPS",
        "T03-SYNC-2026",
    ] {
        assert!(rows.iter().any(|r| r.id == want), "missing row {want}");
    }
    // Matrix ids were corrected against FEATURE-MATRIX.md: OSC 7 is SHELLINTEGRATION-13
    // (SHELLINTEGRATION-03 is the block store); mode 2027 is UNICODE-03/20 and VTCORE-61
    // (VTCORE-30 is the umbrella row, not 2027).
    let m = |id: &str| rows.iter().find(|r| r.id == id).unwrap().matrix.clone();
    assert!(m("T03-OSC7").contains(&"SHELLINTEGRATION-13".to_string()));
    assert!(!m("T03-OSC7").contains(&"SHELLINTEGRATION-03".to_string()));
    for id in ["UNICODE-03", "UNICODE-20", "VTCORE-61"] {
        assert!(m("T03-MODE-2027").contains(&id.to_string()));
    }
    assert!(!m("T03-MODE-2027").contains(&"VTCORE-30".to_string()));
}

#[test]
fn oracle_exclusions_cite_real_rows() {
    let problems = oracle::check_exclusions(&conformance_rows());
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn every_review_note_is_indexed() {
    let fixtures = load_fixtures(&fixtures_dir()).expect("fixtures load");
    let index = std::fs::read_to_string(fixtures_dir().join("REVIEW.kdl")).expect("REVIEW.kdl");
    for fx in fixtures.iter().filter(|f| f.review.is_some()) {
        assert!(
            index.contains(&format!("\"{}\"", fx.id)),
            "fixture {} has a review note but is not in REVIEW.kdl",
            fx.id
        );
    }
}

#[test]
fn golden_reply_probes_exist() {
    let fixtures = load_fixtures(&fixtures_dir()).expect("fixtures load");
    for id in [
        "reports-da1-explicit-zero",
        "reports-da2-0-1-0",
        "reports-da2-1-4-2",
        "reports-da2-12-34-56",
        "reports-da3",
        "reports-decrqss-sgr-rgb-neovim-probe",
    ] {
        assert!(
            fixtures.iter().any(|f| f.id == id),
            "missing golden reply fixture {id}"
        );
    }
}

/// Every matrix id the table cites exists in FEATURE-MATRIX.md (checked when the docs tree is
/// next to the crate; the table's ids were corrected against it).
#[test]
fn conformance_matrix_ids_exist_in_the_feature_matrix() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../docs/internal/apotheosis/research/FEATURE-MATRIX.md");
    let Ok(matrix) = std::fs::read_to_string(&path) else {
        return;
    };
    let mut bad = Vec::new();
    for r in conformance_rows() {
        for m in &r.matrix {
            if !matrix.contains(&format!("| {m} |")) {
                bad.push(format!("row {} cites unknown matrix id {m}", r.id));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
