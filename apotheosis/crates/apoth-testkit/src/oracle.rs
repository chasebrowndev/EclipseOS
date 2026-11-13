// SPDX-License-Identifier: AGPL-3.0-only
//! Differential-oracle framework (T-25 §2).
//!
//! `vte` and `alacritty_terminal` are dev-dependency oracles: they are driven with the same
//! bytes and their grids are compared with ours cell-by-cell (text and cursor here) under a
//! documented exclusion list. Each exclusion cites a T-03 conformance row; a disagreement
//! without an exclusion fails. Oracles detect accidents, they are never the specification.
//!
//! This module holds everything that does not depend on an oracle crate: the [`Oracle`]
//! trait, the exclusion table, the comparison and the table checks. The `vte` adapter lives in
//! `tests/oracle_vte.rs` (vte is a dev-dependency; it is a parser, so the adapter carries a
//! small screen model and reports `unsupported` for what it does not model). The
//! `alacritty_terminal` adapter is NOT written: the crate is not in the workspace lockfile and
//! needs `cargo deny` first (T-00 section 3); T-25 section 2 is deferred for that oracle.

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

/// An intentional difference from an oracle.
#[derive(Clone, Copy, Debug)]
pub struct Exclusion {
    /// Oracle name, or `*` for all.
    pub oracle: &'static str,
    /// Fixture id prefix the exclusion covers.
    pub fixture_prefix: &'static str,
    /// The T-03 conformance row that decides this behaviour.
    pub row: &'static str,
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
        reason: "raw C1 bytes are never controls here; oracles may execute them (VTCORE-13)",
    },
    Exclusion {
        oracle: "*",
        fixture_prefix: "c0-invalid-utf8-",
        row: "T03-UTF8-INVALID",
        reason: "maximal-subpart replacement differs between decoders (T-01 section 2.2)",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "lrm-",
        row: "T03-DECLRMM",
        reason: "left/right margins (mode 69) are R1 here and not implemented by the oracle (VTCORE-31)",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "wide-cluster-",
        row: "T03-GRAPHEME",
        reason: "grapheme clusters are always on here (T-04 section 2); the oracle widths per scalar",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "wide-vs",
        row: "T03-GRAPHEME",
        reason: "VS15/VS16 change cluster width here (T-04 section 3)",
    },
    Exclusion {
        oracle: "*",
        fixture_prefix: "never-",
        row: "T03-NEVER",
        reason: "VT52, DECUDK and friends are consumed and ignored here (T-03 section 3.3)",
    },
    Exclusion {
        oracle: "*",
        fixture_prefix: "sync-",
        row: "T03-SYNC-2026",
        reason: "the hold is presentation-only here; oracles may buffer bytes (T-03 section 4)",
    },
    Exclusion {
        oracle: "*",
        fixture_prefix: "resize-",
        row: "T03-RESIZE",
        reason: "resize semantics differ (no reflow before M12)",
    },
    // Classified over the corpus run against `vte` (tests/oracle_vte.rs). The run's other
    // divergences were adapter bugs or ours-right (DEL printed, LF kept pending wrap) and were
    // fixed in the adapter. This one is unspecified: the spec does not say whether an erase
    // clears pending wrap (fixture review note), so the oracle's model is no authority.
    Exclusion {
        oracle: "vte",
        fixture_prefix: "wrap-erase-line-then-print",
        row: "T03-AUTOWRAP",
        reason: "unspecified: erase vs pending-wrap flag; the adapter clears it, we keep it",
    },
    Exclusion {
        oracle: "alacritty_terminal",
        fixture_prefix: "scroll-region-top-anchored",
        row: "T03-SCROLL-HISTORY",
        reason: "history feed rule for regions (VTCORE-15) differs",
    },
];

/// The exclusion that covers `fixture_id` for `oracle`, if any.
#[must_use]
pub fn excluded(oracle: &str, fixture_id: &str) -> Option<&'static Exclusion> {
    EXCLUSIONS.iter().find(|e| {
        (e.oracle == "*" || e.oracle == oracle) && fixture_id.starts_with(e.fixture_prefix)
    })
}

/// The single byte stream of a fixture when it is comparable: one `feed` step, no resize,
/// no checkpoints, no initial modes. `None` otherwise.
#[must_use]
pub fn comparable_stream(fx: &Fixture) -> Option<&[u8]> {
    if !fx.modes_init.is_empty() {
        return None;
    }
    match fx.steps.as_slice() {
        [Step::Feed { bytes, .. }] => Some(bytes),
        _ => None,
    }
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
        return Ok(Verdict::Skipped("not a single-feed fixture".into()));
    };
    if let Some(e) = excluded(oracle.name(), &fx.id) {
        return Ok(Verdict::Skipped(format!(
            "excluded by {} ({})",
            e.row, e.reason
        )));
    }
    let pts = execute::<D>(fx, Chunking::Whole)?;
    let ours = &pts.last().ok_or("no points")?.snap;
    let theirs = oracle.run(fx.rows, fx.cols, bytes);
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
        if e.reason.trim().is_empty() {
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
}
