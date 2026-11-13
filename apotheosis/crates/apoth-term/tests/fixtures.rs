// SPDX-License-Identifier: AGPL-3.0-only
//! The golden-grid corpus (T-25 §1), the conformance table (T-03 §3) and the harness
//! self-tests (T-25 §9).

mod support;

use support::kdl::{self, Node};
use support::*;

#[test]
fn corpus_passes_at_every_chunking() {
    let fx = load_all();
    let fails = run_everything(&fx);
    assert!(
        fails.is_empty(),
        "{} of {} fixtures failed:\n{}",
        fails.len(),
        fx.len(),
        fails.join("\n")
    );
}

#[test]
fn corpus_has_at_least_300_fixtures_with_unique_ids() {
    let fx = load_all();
    assert!(fx.len() >= 300, "only {} fixtures", fx.len());
    let mut ids: Vec<_> = fx.iter().map(|f| f.id.clone()).collect();
    ids.sort();
    let n = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), n, "duplicate fixture ids");
}

fn glob_match(pat: &str, id: &str) -> bool {
    match pat.strip_suffix('*') {
        Some(p) => id.starts_with(p),
        None => pat == id,
    }
}

fn conformance() -> Vec<Node> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("conformance.kdl");
    let src = std::fs::read_to_string(p).expect("conformance.kdl");
    kdl::parse(&src).expect("conformance parse")
}

#[test]
fn every_t03_row_cites_a_fixture() {
    let fx = load_all();
    let rows = conformance();
    assert!(rows.len() >= 25, "conformance table too small");
    let mut bad = Vec::new();
    for r in &rows {
        assert_eq!(r.name, "row");
        let id = r.str_arg(0).unwrap();
        let pats = r.strs();
        if pats.len() < 2 {
            bad.push(format!("{id}: cites no fixture"));
        }
        for pat in &pats[1.min(pats.len())..] {
            if !fx.iter().any(|f| glob_match(pat, &f.id)) {
                bad.push(format!("{id}: `{pat}` matches no fixture"));
            }
        }
        if r.prop_str("section").is_none() {
            bad.push(format!("{id}: no section"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn conformance_covers_the_spec_sections() {
    let rows = conformance();
    let ids: Vec<String> = rows
        .iter()
        .map(|r| r.str_arg(0).unwrap().to_string())
        .collect();
    for want in [
        "T-03.2/mode-table",
        "T-03.2/input-mirror",
        "T-03.3.1/c0-esc",
        "T-03.3.1/csi-cursor-erase-scroll",
        "T-03.3.1/sgr",
        "T-03.3.1/sgr-ambiguity-table",
        "T-03.3.1/charsets",
        "T-03.3.1/alt-screens",
        "T-03.3.1/lr-margins",
        "T-03.3.1/replies-da",
        "T-03.3.1/decrqss",
        "T-03.3.1/osc-title",
        "T-03.3.1/osc-colors",
        "T-03.3.1/osc-8",
        "T-03.3.1/osc-52",
        "T-03.3.1/osc-133",
        "T-03.3.1/osc-9-progress",
        "T-03.3.1/xtwinops",
        "T-03.3.3/never",
        "T-03.4/sync",
        "T-03.5/caps",
    ] {
        assert!(
            ids.iter().any(|i| i == want),
            "missing conformance row {want}"
        );
    }
}

// ---- harness self-tests (T-25 §9) ------------------------------------------------------

#[test]
fn a_deliberately_wrong_fixture_fails() {
    let fx = load_str(
        "fixture \"wrong\" rows=1 cols=4 { feed \"abc\"; grid \"abd\" }",
        "self",
    )
    .unwrap();
    assert!(run(&fx[0], Chunking::Whole).is_err());
    let fx = load_str(
        "fixture \"wrong2\" rows=1 cols=4 { feed \"abc\"; cursor 0 2 }",
        "self",
    )
    .unwrap();
    assert!(run(&fx[0], Chunking::Whole).is_err());
    let fx = load_str(
        "fixture \"wrong3\" rows=1 cols=4 { feed \"\\e[5n\"; replies \"\\e[1n\" }",
        "self",
    )
    .unwrap();
    assert!(run(&fx[0], Chunking::Whole).is_err());
}

#[test]
fn a_right_fixture_passes_and_unknown_steps_fail() {
    let fx = load_str(
        "fixture \"right\" rows=1 cols=4 { feed \"abc\"; grid \"abc\"; cursor 0 3 }",
        "self",
    )
    .unwrap();
    assert!(run(&fx[0], Chunking::Whole).is_ok());
    let fx = load_str("fixture \"nope\" rows=1 cols=4 { frobnicate }", "self").unwrap();
    assert!(run(&fx[0], Chunking::Whole).is_err());
}

#[test]
fn chunking_invariance_meta_test_on_a_stateful_stream() {
    // Split UTF-8, split CSI, split OSC, SGR state, charsets, modes, replies: everything
    // that carries state across a chunk edge.
    let stream = "\x1b[1;31mA\u{e9}\u{4e16}\x1b(0lqk\x1b(B\x1b[?1049hx\x1b]2;t\u{e9}\x07\x1b[6n\
        \x1b]52;c;aGk=\x07\x1b[?1h\x1b[>5u\x1b[?1049l\x1b[38:2::1:2:3mZ\x1bP$qm\x1b\\e\u{301}x"
        .as_bytes();
    let cfg = || apoth_term::TermConfig {
        rows: 4,
        cols: 12,
        ..Default::default()
    };
    let whole = digest(cfg(), stream, Chunking::Whole);
    assert!(
        whole.contains("R\\eP1$r"),
        "digest should include CPR then DECRQSS replies: {whole}"
    );
    for &ch in CHUNKINGS {
        assert_eq!(digest(cfg(), stream, ch), whole, "{ch:?}");
    }
    for n in 1..=stream.len().min(40) {
        assert_eq!(digest(cfg(), stream, Chunking::Size(n)), whole, "size {n}");
    }
}

#[test]
fn the_harness_would_notice_a_chunk_dependent_difference() {
    // Different content must give different digests (the digest is not constant).
    let cfg = || apoth_term::TermConfig {
        rows: 2,
        cols: 6,
        ..Default::default()
    };
    assert_ne!(
        digest(cfg(), b"abc", Chunking::Whole),
        digest(cfg(), b"abd", Chunking::Whole)
    );
}
