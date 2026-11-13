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
        fixtures.len() >= 300,
        "T-25 section 1 wants at least 300 golden fixtures, have {}",
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

#[test]
fn conformance_rows_due_at_m9_are_covered() {
    let fixtures = load_fixtures(&fixtures_dir()).expect("fixtures load");
    let problems = conformance::check(&conformance_rows(), &fixtures, 9);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
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
