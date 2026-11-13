// SPDX-License-Identifier: AGPL-3.0-only
//! Differential-oracle framework (T-25 §2).
//!
//! `vte` and `alacritty_terminal` are dev-dependency oracles: they are driven with the same
//! bytes and their grids are compared with ours cell-by-cell (text and cursor here) under a
//! documented exclusion list. Each exclusion cites a T-03 conformance row; a disagreement
//! without an exclusion fails. Oracles detect accidents, they are never the specification.
//!
//! This module holds everything that does not depend on an oracle crate: the [`Oracle`]
//! trait, the exclusion table, the comparison and the table checks. The adapters live in
//! `tests/oracle_vte.rs` (a parser, so the adapter carries a small screen model and reports
//! `unsupported` for what it does not model) and `tests/oracle_alacritty.rs` (a full terminal
//! grid). Both are dev-dependencies of this crate only.
//!
//! The exclusion list is derived from a real run: a disagreement without an exclusion fails
//! the test, and the failure prints the fixture, the T-03 row that cites it and the diff, which
//! is what an exclusion entry needs. Every entry is classified by who is right (see [`Class`]).

use crate::fixture::{Fixture, Step};
use crate::runner::{Chunking, Driver, execute};

/// What an oracle reports after being fed a byte stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OracleView {
    /// Visible rows, text only, right-trimmed.
    pub lines: Vec<String>,
    /// Cursor `(row, col)`, 0-based.
    pub cursor: (usize, usize),
    /// Set when the stream used something the oracle does not model: the comparison is then
    /// skipped (carrying this reason) instead of counted as a disagreement.
    pub unsupported: Option<String>,
}

/// A reference terminal.
pub trait Oracle {
    /// Name used in exclusions (`alacritty_terminal`, `vte`).
    fn name(&self) -> &'static str;
    /// Feed the whole stream to a fresh terminal of this size and report.
    fn run(&mut self, rows: usize, cols: usize, bytes: &[u8]) -> OracleView;
}

/// Who is right when we and an oracle disagree. Oracles are never the specification: a
/// classification other than [`Class::TheirsRight`] keeps our behaviour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// T-03 (or a matrix row it cites) decides and we follow it; the oracle differs.
    OursRight,
    /// The oracle is right and we owe a fix; the exclusion is a tracked defect, not a verdict.
    TheirsRight,
    /// The spec does not decide (the fixture carries a review note); neither is an authority.
    Unspecified,
}

/// An intentional difference from an oracle.
#[derive(Clone, Copy, Debug)]
pub struct Exclusion {
    /// Oracle name, or `*` for all.
    pub oracle: &'static str,
    /// Fixture id prefix the exclusion covers.
    pub fixture_prefix: &'static str,
    /// The T-03 conformance row that decides this behaviour.
    pub row: &'static str,
    /// Who is right.
    pub class: Class,
    /// Why ours differs.
    pub reason: &'static str,
}

/// The exclusion list. Entries are reviewed; the unit test `exclusions_cite_real_rows` keeps
/// each one tied to a row of `fixtures/conformance.kdl`.
pub const EXCLUSIONS: &[Exclusion] = &[
    Exclusion {
        oracle: "*",
        fixture_prefix: "c1-",
        row: "T03-C1-IGNORED",
        class: Class::OursRight,
        reason: "raw C1 bytes are never controls here; oracles may execute them (VTCORE-13)",
    },
    Exclusion {
        oracle: "*",
        fixture_prefix: "c0-invalid-utf8-",
        row: "T03-UTF8-INVALID",
        class: Class::OursRight,
        reason: "maximal-subpart replacement differs between decoders (T-01 section 2.2)",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "lrm-",
        row: "T03-DECLRMM",
        class: Class::OursRight,
        reason: "left/right margins (mode 69) are R1 here and not implemented by the oracle (VTCORE-31)",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "wide-cluster-",
        row: "T03-GRAPHEME",
        class: Class::OursRight,
        reason: "grapheme clusters are always on here (T-04 section 2); the oracle widths per scalar",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "wide-vs",
        row: "T03-GRAPHEME",
        class: Class::OursRight,
        reason: "VS15/VS16 change cluster width here (T-04 section 3)",
    },
    Exclusion {
        oracle: "*",
        fixture_prefix: "never-",
        row: "T03-NEVER",
        class: Class::OursRight,
        reason: "VT52, DECUDK and friends are consumed and ignored here (T-03 section 3.3)",
    },
    Exclusion {
        oracle: "*",
        fixture_prefix: "sync-",
        row: "T03-SYNC-2026",
        class: Class::OursRight,
        reason: "the hold is presentation-only here; oracles may buffer bytes (T-03 section 4)",
    },
    Exclusion {
        oracle: "*",
        fixture_prefix: "resize-",
        row: "T03-RESIZE",
        class: Class::Unspecified,
        reason: "oracles cannot be driven through a resize from a byte stream and their reflow \
                 and truncation rules differ from T-02 section 3; resize is covered by the T03-RESIZE \
                 fixtures and apoth-grid reflow tests",
    },
    // Classified over the corpus run against `vte` (tests/oracle_vte.rs). The run's other
    // divergences were adapter bugs or ours-right (DEL printed, LF kept pending wrap) and were
    // fixed in the adapter. This one is unspecified: the spec does not say whether an erase
    // clears pending wrap (fixture review note), so the oracle's model is no authority.
    Exclusion {
        oracle: "vte",
        fixture_prefix: "wrap-erase-line-then-print",
        row: "T03-AUTOWRAP",
        class: Class::Unspecified,
        reason: "unspecified: erase vs pending-wrap flag; the adapter clears it, we keep it",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "scroll-region-top-anchored",
        row: "T03-SCROLL-HISTORY",
        class: Class::OursRight,
        reason: "history feed rule for regions (VTCORE-15) differs",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "alt-1047",
        row: "T03-ALTSCREEN",
        class: Class::OursRight,
        reason: "modes 47/1047/1048 differ: the oracle shares or clears the alternate buffer differently (T-03 alt-screen table)",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "alt-1048",
        row: "T03-ALTSCREEN",
        class: Class::OursRight,
        reason: "1048 saves and restores the cursor per T-03; the oracle's cursor save differs",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "alt-47",
        row: "T03-ALTSCREEN",
        class: Class::OursRight,
        reason: "mode 47 keeps content and shares the cursor per T-03; the oracle differs",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "cur-cursor-shared-by-both-screens",
        row: "T03-ALTSCREEN",
        class: Class::OursRight,
        reason: "the cursor is shared across screens per T-03; the oracle keeps separate state",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "c0-lf-lnm",
        row: "T03-LNM",
        class: Class::OursRight,
        reason: "LNM (mode 20): LF also returns to column 0 per T-03; the oracle ignores LNM",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "scr-lnm",
        row: "T03-LNM",
        class: Class::OursRight,
        reason: "LNM (mode 20): LF also returns to column 0 per T-03; the oracle ignores LNM",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "scroll-lnm",
        row: "T03-LNM",
        class: Class::OursRight,
        reason: "LNM (mode 20): LF also returns to column 0 per T-03; the oracle ignores LNM",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "esc-decsc-saves-origin",
        row: "T03-DECSC",
        class: Class::OursRight,
        reason: "DECSC saves origin mode per T-03; the oracle does not",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "cursor-decsc-saves-origin-mode",
        row: "T03-DECSC",
        class: Class::OursRight,
        reason: "DECSC saves origin mode per T-03; the oracle does not",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "esc-decrc-decom-",
        row: "T03-DECOM",
        class: Class::OursRight,
        reason: "DECRC re-clamps into the current margins under DECOM per T-03; the oracle does not",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "scroll-decom-reset-homes",
        row: "T03-DECOM",
        class: Class::OursRight,
        reason: "resetting DECOM homes the cursor per T-03; the oracle does not",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "esc-ls",
        row: "T03-CHARSET",
        class: Class::OursRight,
        reason: "locking shifts LS2/LS3 are implemented here; the oracle ignores them",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "esc-ss",
        row: "T03-CHARSET",
        class: Class::OursRight,
        reason: "single shifts SS2/SS3 are implemented here; the oracle ignores them",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "charset-",
        row: "T03-CHARSET",
        class: Class::OursRight,
        reason: "G2/G3 shifts and the UK set are implemented here; the oracle lacks them",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "cs-",
        row: "T03-CHARSET",
        class: Class::OursRight,
        reason: "charset designation (UK, G2, DEC special blank) follows T-03; the oracle differs",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "cur-cud-stops-at-bottom-margin",
        row: "T03-CUX",
        class: Class::OursRight,
        reason: "CUD/CUU stop at the scroll margin per T-03; the oracle runs past it",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "cursor-cud-stops-at-bottom-margin",
        row: "T03-CUX",
        class: Class::OursRight,
        reason: "CUD stops at the bottom margin per T-03; the oracle runs past it",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "cursor-cuu-stops-at-top-margin",
        row: "T03-CUX",
        class: Class::OursRight,
        reason: "CUU stops at the top margin per T-03; the oracle runs past it",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "cursor-lf-clears-pending-wrap",
        row: "T03-AUTOWRAP",
        class: Class::OursRight,
        reason: "LF clears pending wrap and keeps the column (no CR) per T-03; the oracle wraps first",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "wrap-exact-then-lf",
        row: "T03-AUTOWRAP",
        class: Class::OursRight,
        reason: "LF after an exact-width line keeps the column (no CR) per T-03; the oracle wraps first",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "wrap-decawm-off-then-on",
        row: "T03-DECAWM",
        class: Class::OursRight,
        reason: "with DECAWM off the last column is overwritten per T-03; the oracle differs",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "edit-dch-",
        row: "T03-ICH-DCH",
        class: Class::OursRight,
        reason: "DCH/ICH clamp the count and never leave half a wide cell per T-03; the oracle differs",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "edit-ich-",
        row: "T03-ICH-DCH",
        class: Class::OursRight,
        reason: "ICH never leaves half a wide cell at the edge per T-03; the oracle keeps it",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "edit-dl-",
        row: "T03-IL-DL",
        class: Class::OursRight,
        reason: "IL/DL move the cursor to the left margin per T-03; the oracle keeps the column",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "edit-il-",
        row: "T03-IL-DL",
        class: Class::OursRight,
        reason: "IL/DL move the cursor to the left margin per T-03; the oracle keeps the column",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "ed-1-above",
        row: "T03-ED",
        class: Class::OursRight,
        reason: "ED 1 erases from the top through the cursor per T-03; the oracle differs",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "el-wrap-pending-erases-last",
        row: "T03-EL",
        class: Class::OursRight,
        reason: "EL with pending wrap erases the last column per T-03; the oracle differs",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "erase-ech-",
        row: "T03-ECH",
        class: Class::OursRight,
        reason: "ECH blanks a cut wide cell wholly per T-03; the oracle keeps half",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "erase-el",
        row: "T03-EL",
        class: Class::OursRight,
        reason: "EL blanks a cut wide cell wholly per T-03; the oracle keeps half",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "lr-",
        row: "T03-DECLRMM",
        class: Class::OursRight,
        reason: "left/right margins (mode 69) are R1 here and not implemented by the oracle (VTCORE-31)",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "decaln-fills-and-homes",
        row: "T03-DECALN",
        class: Class::OursRight,
        reason: "DECALN homes the cursor per T-03; the oracle leaves it",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "cap-rep-count-bounded",
        row: "T03-SEQ-ROBUST",
        class: Class::OursRight,
        reason: "REP is bounded by the robustness caps here; the oracle repeats the full count",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "w-keycap",
        row: "T03-GRAPHEME",
        class: Class::OursRight,
        reason: "grapheme clusters are one cell group here (T-04 section 2); the oracle widths per scalar",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "w-vs16",
        row: "T03-GRAPHEME",
        class: Class::OursRight,
        reason: "VS16 widens the cluster here (T-04 section 3); the oracle widths per scalar",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "w-zwj",
        row: "T03-GRAPHEME",
        class: Class::OursRight,
        reason: "ZWJ sequences are one cluster here (T-04 section 2); the oracle widths per scalar",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "wide-keycap",
        row: "T03-GRAPHEME",
        class: Class::OursRight,
        reason: "keycap sequences are one cluster here (T-04 section 2); the oracle widths per scalar",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "wide-combining-after-cursor-move",
        row: "T03-COMBINING",
        class: Class::OursRight,
        reason: "a combining mark after a cursor move attaches per T-03/T-04; the oracle differs",
    },
];

/// The exclusion that covers `fixture_id` for `oracle`, if any.
#[must_use]
pub fn excluded(oracle: &str, fixture_id: &str) -> Option<&'static Exclusion> {
    EXCLUSIONS.iter().find(|e| {
        (e.oracle == "*" || e.oracle == oracle) && fixture_id.starts_with(e.fixture_prefix)
    })
}

/// Why a fixture is outside the byte-stream comparison, with the T-03 row that owns the
/// behaviour. An uncomparable fixture is never skipped silently: `compare` reports this row.
#[must_use]
pub fn uncomparable(fx: &Fixture) -> Option<(&'static str, &'static str)> {
    if fx.steps.iter().any(|s| matches!(s, Step::Resize { .. })) {
        return Some((
            "T03-RESIZE",
            "resize cannot be replayed into an oracle from bytes; reflow rules differ (T-02 section 3)",
        ));
    }
    if !fx.modes_init.is_empty() {
        return Some((
            "T03-MODES-TABLE",
            "initial mode state is set through our mode table, not bytes",
        ));
    }
    None
}

/// The byte stream of a fixture when it is comparable ([`uncomparable`] is `None`): `feed`
/// steps concatenated (checkpoints observe, they do not change state).
#[must_use]
pub fn comparable_stream(fx: &Fixture) -> Option<Vec<u8>> {
    if uncomparable(fx).is_some() {
        return None;
    }
    let mut all = Vec::new();
    for s in &fx.steps {
        if let Step::Feed { bytes, .. } = s {
            all.extend_from_slice(bytes);
        }
    }
    Some(all)
}

/// Result of one differential comparison.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Same text and cursor.
    Agree,
    /// Not comparable (multi-step) or excluded (carries the row).
    Skipped(String),
    /// Disagreement without an exclusion; the string is the readable diff.
    Disagree(String),
}

/// Compare our driver with an oracle on a fixture.
///
/// # Errors
/// A malformed fixture.
pub fn compare<D: Driver, O: Oracle>(fx: &Fixture, oracle: &mut O) -> Result<Verdict, String> {
    let Some(bytes) = comparable_stream(fx) else {
        let (row, why) = uncomparable(fx).ok_or("no stream")?;
        return Ok(Verdict::Skipped(format!("{row}: {why}")));
    };
    if let Some(e) = excluded(oracle.name(), &fx.id) {
        return Ok(Verdict::Skipped(format!(
            "excluded by {} ({})",
            e.row, e.reason
        )));
    }
    let pts = execute::<D>(fx, Chunking::Whole)?;
    let ours = &pts.last().ok_or("no points")?.snap;
    let theirs = oracle.run(fx.rows, fx.cols, &bytes);
    if let Some(why) = &theirs.unsupported {
        return Ok(Verdict::Skipped(format!(
            "oracle {} does not model: {why}",
            oracle.name()
        )));
    }
    let mut want = theirs.lines.clone();
    want.resize(ours.rows, String::new());
    let mut got = ours.grid_text();
    got.resize(ours.rows, String::new());
    let mut report = String::new();
    if let Some(d) = crate::snapshot::diff_rows(&format!("grid vs {}", oracle.name()), &want, &got)
    {
        report.push_str(&d);
    }
    if theirs.cursor != (ours.cursor.row, ours.cursor.col) {
        report.push_str(&format!(
            "cursor vs {}: oracle {:?}, ours {:?}\n",
            oracle.name(),
            theirs.cursor,
            (ours.cursor.row, ours.cursor.col)
        ));
    }
    Ok(if report.is_empty() {
        Verdict::Agree
    } else {
        Verdict::Disagree(report)
    })
}

/// The text of a failed differential run: one block per disagreement, each with the T-03
/// rows that cite the fixture (from the conformance table) and a ready-to-review exclusion
/// entry. `disagreements` is `(fixture id, diff)`.
#[must_use]
pub fn disagreement_report(
    oracle: &str,
    rows: &[crate::conformance::Row],
    disagreements: &[(String, String)],
) -> String {
    let mut o = String::new();
    for (id, diff) in disagreements {
        let cited: Vec<&str> = rows
            .iter()
            .filter(|r| r.fixtures.iter().any(|f| f == id))
            .map(|r| r.id.as_str())
            .collect();
        o.push_str(&format!(
            "{id} (rows: {}):\n{diff}  suggested: Exclusion {{ oracle: \"{oracle}\", fixture_prefix: \"{id}\", row: \"{}\", class: Class::/*classify*/, reason: \"...\" }}\n\n",
            cited.join(", "),
            cited.first().copied().unwrap_or("T03-?"),
        ));
    }
    o
}

/// Problems with the exclusion list against the conformance table: every entry must cite a
/// row of the table and give a reason.
#[must_use]
pub fn check_exclusions(rows: &[crate::conformance::Row]) -> Vec<String> {
    let mut p = Vec::new();
    for e in EXCLUSIONS {
        if !rows.iter().any(|r| r.id == e.row) {
            p.push(format!(
                "exclusion `{}` cites unknown row {}",
                e.fixture_prefix, e.row
            ));
        }
        if e.reason.trim().is_empty() || e.reason.contains("...") {
            p.push(format!("exclusion `{}` has no reason", e.fixture_prefix));
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selftest::MiniTerm;

    struct Fake(OracleView);
    impl Oracle for Fake {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn run(&mut self, _r: usize, _c: usize, _b: &[u8]) -> OracleView {
            self.0.clone()
        }
    }

    fn fx(id: &str) -> Fixture {
        Fixture::parse(
            &format!("fixture \"{id}\" {{ size rows=2 cols=5; feed \"AB\" }}"),
            format!("{id}.kdl").into(),
        )
        .unwrap()
    }

    #[test]
    fn disagreement_without_exclusion_fails() {
        let mut o = Fake(OracleView {
            lines: vec!["AX".into()],
            cursor: (0, 2),
            unsupported: None,
        });
        let v = compare::<MiniTerm, _>(&fx("plain"), &mut o).unwrap();
        assert!(
            matches!(&v, Verdict::Disagree(d) if d.contains("grid vs fake")),
            "{v:?}"
        );
        let mut o = Fake(OracleView {
            lines: vec!["AB".into()],
            cursor: (0, 2),
            unsupported: None,
        });
        assert_eq!(
            compare::<MiniTerm, _>(&fx("plain"), &mut o).unwrap(),
            Verdict::Agree
        );
        let mut o = Fake(OracleView {
            lines: vec!["AB".into()],
            cursor: (1, 0),
            unsupported: None,
        });
        assert!(matches!(
            compare::<MiniTerm, _>(&fx("plain"), &mut o).unwrap(),
            Verdict::Disagree(_)
        ));
    }

    #[test]
    fn exclusion_turns_a_disagreement_into_a_skip() {
        let mut o = Fake(OracleView {
            lines: vec!["ZZ".into()],
            cursor: (0, 0),
            unsupported: None,
        });
        let v = compare::<MiniTerm, _>(&fx("c1-anything"), &mut o).unwrap();
        assert!(
            matches!(&v, Verdict::Skipped(s) if s.contains("T03-C1-IGNORED")),
            "{v:?}"
        );
        assert!(excluded("alacritty_terminal", "lrm-decic").is_some());
        assert!(excluded("vte", "lrm-decic").is_none());
    }

    #[test]
    fn feeds_and_checkpoints_concatenate_resizes_do_not_compare() {
        let two = Fixture::parse(
            "fixture \"two\" { size rows=2 cols=5; feed \"A\"; checkpoint \"m\" { grid { row \"A\" } }; feed \"B\" }",
            "two.kdl".into(),
        )
        .unwrap();
        assert_eq!(comparable_stream(&two).as_deref(), Some(&b"AB"[..]));
        let rs = Fixture::parse(
            "fixture \"rs\" { size rows=2 cols=5; feed \"A\"; resize rows=3 cols=5 }",
            "rs.kdl".into(),
        )
        .unwrap();
        assert_eq!(comparable_stream(&rs), None);
    }

    #[test]
    fn the_report_names_rows_and_a_suggested_entry() {
        let rows = crate::conformance::parse(
            r#"conformance { row "T03-A" spec="T-03" matrix="VTCORE-15" tier="R1" milestone="M9" desc="a" { fixture "f" } }"#,
        )
        .unwrap();
        let r = disagreement_report("vte", &rows, &[("f".into(), "diff\n".into())]);
        assert!(
            r.contains("f (rows: T03-A)") && r.contains("row: \"T03-A\""),
            "{r}"
        );
    }

    #[test]
    fn every_exclusion_is_classified_and_reasoned() {
        for e in EXCLUSIONS {
            assert!(!e.reason.contains("..."), "{}", e.fixture_prefix);
        }
        assert!(EXCLUSIONS.iter().any(|e| e.class == Class::Unspecified));
    }
}
