// SPDX-License-Identifier: AGPL-3.0-only
//! The machine-readable T-03 conformance table (`fixtures/conformance.kdl`) and its checks.
//!
//! ```kdl
//! conformance {
//!   row "T03-CUU" spec="T-03 §3.1" matrix="VTCORE-15" tier="R1" milestone="M9" desc="CUU" {
//!     fixture "cursor-cuu"
//!     fixture "cur-cuu" src="apoth-term"
//!   }
//! }
//! ```
//! The table is the one source of "which fixtures prove which row": each row cites explicit
//! fixture ids (no prefix globs). It is checked, not trusted:
//! * every row due at the enforced milestone cites at least one fixture (T-25 section 1);
//! * every cited id is a fixture of the corpus, none is a glob, none is cited twice by a row;
//! * every fixture of the corpus is cited by some row (one corpus, no orphans);
//! * a fixture's informational `covers "T03-..."` claims name real rows that cite it back;
//! * row ids are unique, every row names its spec section, matrix ids look like matrix ids.
//!
//! Rows of later milestones are listed so the gap is visible; they are enforced from the
//! milestone number given to [`check`] (rows up to and including it must cite a fixture).

use std::collections::{BTreeMap, BTreeSet};

use crate::fixture::Fixture;
use crate::kdl;

/// One row of the table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// `T03-...` id.
    pub id: String,
    /// Spec section, e.g. `T-03 §3.1`.
    pub spec: String,
    /// Feature-matrix ids.
    pub matrix: Vec<String>,
    /// Release tier (`R1`, `R2`, `Never`).
    pub tier: String,
    /// Milestone at which the row must be covered, e.g. `M9`.
    pub milestone: String,
    /// One line.
    pub desc: String,
    /// Explicit fixture ids that prove the row.
    pub fixtures: Vec<String>,
}

/// Parse the table file's text.
///
/// # Errors
/// Syntax or schema errors.
pub fn parse(src: &str) -> Result<Vec<Row>, String> {
    let doc = kdl::parse(src).map_err(|e| e.to_string())?;
    let [root] = doc.as_slice() else {
        return Err("exactly one `conformance` node expected".into());
    };
    if root.name != "conformance" {
        return Err("top-level node must be `conformance`".into());
    }
    let mut rows = Vec::new();
    for n in &root.children {
        if n.name != "row" {
            return Err(format!("line {}: expected `row`", n.line));
        }
        let get = |k: &str| -> Result<String, String> {
            n.prop(k)
                .and_then(kdl::Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("line {}: row needs {k}=\"...\"", n.line))
        };
        let id = n
            .args
            .first()
            .and_then(kdl::Value::as_str)
            .ok_or(format!("line {}: row needs an id", n.line))?;
        rows.push(Row {
            id: id.to_string(),
            spec: get("spec")?,
            matrix: get("matrix")?
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            tier: get("tier")?,
            milestone: get("milestone")?,
            desc: get("desc")?,
            fixtures: n
                .children
                .iter()
                .map(|c| {
                    if c.name != "fixture" {
                        return Err(format!("line {}: expected `fixture`", c.line));
                    }
                    c.args
                        .first()
                        .and_then(kdl::Value::as_str)
                        .map(str::to_string)
                        .ok_or_else(|| format!("line {}: fixture needs an id", c.line))
                })
                .collect::<Result<_, _>>()?,
        });
    }
    Ok(rows)
}

fn milestone_number(m: &str) -> u32 {
    m.trim_start_matches('M').parse().unwrap_or(u32::MAX)
}

/// Row id to the fixture ids it cites that exist in the corpus.
#[must_use]
pub fn coverage(rows: &[Row], fixtures: &[Fixture]) -> BTreeMap<String, Vec<String>> {
    let have: BTreeSet<&str> = fixtures.iter().map(|f| f.id.as_str()).collect();
    rows.iter()
        .map(|r| {
            let v = r
                .fixtures
                .iter()
                .filter(|f| have.contains(f.as_str()))
                .cloned()
                .collect();
            (r.id.clone(), v)
        })
        .collect()
}

fn looks_like_matrix_id(s: &str) -> bool {
    s.split_once('-').is_some_and(|(a, b)| {
        !a.is_empty()
            && a.bytes().all(|c| c.is_ascii_uppercase())
            && !b.is_empty()
            && b.bytes().all(|c| c.is_ascii_digit())
    })
}

/// All problems with the table against the corpus; empty when consistent.
#[must_use]
pub fn check(rows: &[Row], fixtures: &[Fixture], enforce_up_to: u32) -> Vec<String> {
    let mut p = Vec::new();
    let mut seen = BTreeSet::new();
    let have: BTreeSet<&str> = fixtures.iter().map(|f| f.id.as_str()).collect();
    let mut cited: BTreeSet<&str> = BTreeSet::new();
    for r in rows {
        if !seen.insert(&r.id) {
            p.push(format!("duplicate row id {}", r.id));
        }
        if !r.id.starts_with("T03-") {
            p.push(format!("row id {} must start with T03-", r.id));
        }
        if !r.spec.starts_with("T-") {
            p.push(format!("row {} has no spec section", r.id));
        }
        for m in &r.matrix {
            if !looks_like_matrix_id(m) {
                p.push(format!("row {} has a malformed matrix id `{m}`", r.id));
            }
        }
        let mut mine = BTreeSet::new();
        for f in &r.fixtures {
            if f.contains(['*', '?', '[']) {
                p.push(format!(
                    "row {} cites a glob `{f}`; cite explicit ids",
                    r.id
                ));
            } else if !have.contains(f.as_str()) {
                p.push(format!("row {} cites missing fixture `{f}`", r.id));
            } else {
                cited.insert(f.as_str());
            }
            if !mine.insert(f) {
                p.push(format!("row {} cites fixture `{f}` twice", r.id));
            }
        }
        if milestone_number(&r.milestone) <= enforce_up_to && r.fixtures.is_empty() {
            p.push(format!(
                "row {} ({}, {}) cites no fixture",
                r.id, r.spec, r.milestone
            ));
        }
    }
    let by_id: BTreeMap<&str, &Row> = rows.iter().map(|r| (r.id.as_str(), r)).collect();
    for f in fixtures {
        if !cited.contains(f.id.as_str()) {
            let hint = f
                .covers
                .iter()
                .filter(|c| c.starts_with("T03-"))
                .cloned()
                .collect::<Vec<_>>();
            p.push(format!(
                "fixture {} is cited by no row (add `fixture \"{}\"` to a row{})",
                f.id,
                f.id,
                if hint.is_empty() {
                    String::new()
                } else {
                    format!(", it claims {}", hint.join(" "))
                }
            ));
        }
        for c in f.covers.iter().filter(|c| c.starts_with("T03-")) {
            match by_id.get(c.as_str()) {
                None => p.push(format!("fixture {} claims unknown row {c}", f.id)),
                Some(r) if !r.fixtures.contains(&f.id) => {
                    p.push(format!(
                        "fixture {} claims {c} but the row does not cite it",
                        f.id
                    ));
                }
                Some(_) => {}
            }
        }
    }
    p
}

/// The table with its fixture ids, as plain text (for docs and `xtask`).
#[must_use]
pub fn render(rows: &[Row], fixtures: &[Fixture]) -> String {
    let cov = coverage(rows, fixtures);
    let mut o = String::new();
    for r in rows {
        let f = &cov[&r.id];
        o.push_str(&format!(
            "{} | {} | {} | {} | {} | {} fixtures\n",
            r.id,
            r.spec,
            r.matrix.join(","),
            r.tier,
            r.milestone,
            f.len()
        ));
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    fn fx(id: &str, covers: &str) -> Fixture {
        Fixture::parse(
            &format!("fixture \"{id}\" {{ covers {covers}; size rows=1 cols=1; feed \"x\" }}"),
            format!("{id}.kdl").into(),
        )
        .unwrap()
    }

    fn table(a_fixtures: &str) -> String {
        format!(
            r#"conformance {{
  row "T03-A" spec="T-03 §3.1" matrix="VTCORE-15, VTCORE-14" tier="R1" milestone="M9" desc="a" {{ {a_fixtures} }}
  row "T03-B" spec="T-03 §3.1" matrix="" tier="R1" milestone="M13" desc="b"
}}"#
        )
    }

    #[test]
    fn parses_rows_and_fixture_ids() {
        let r = parse(&table("fixture \"f\"; fixture \"g\" src=\"x\"")).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].matrix, vec!["VTCORE-15", "VTCORE-14"]);
        assert_eq!(r[0].fixtures, vec!["f", "g"]);
        assert!(r[1].fixtures.is_empty());
        assert!(parse("conformance { row \"x\" }").is_err());
        assert!(parse(&table("other \"f\"")).is_err());
    }

    #[test]
    fn a_row_without_a_fixture_fails_the_check() {
        let rows = parse(&table("")).unwrap();
        let p = check(&rows, &[], 9);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].contains("T03-A") && p[0].contains("cites no fixture"));
        // M13 rows are not enforced at M9, but are at M13.
        let rows = parse(&table("fixture \"f\"")).unwrap();
        let f = [fx("f", "\"T03-A\"")];
        assert_eq!(check(&rows, &f, 9), Vec::<String>::new());
        let p = check(&rows, &f, 13);
        assert_eq!(p.len(), 1);
        assert!(p[0].contains("T03-B"));
    }

    #[test]
    fn a_cited_id_missing_from_the_corpus_fails() {
        let rows = parse(&table("fixture \"f\"; fixture \"ghost\"")).unwrap();
        let p = check(&rows, &[fx("f", "\"T03-A\"")], 9);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].contains("missing fixture `ghost`"));
    }

    #[test]
    fn globs_and_duplicates_are_rejected() {
        let rows = parse(&table("fixture \"f*\"; fixture \"f\"; fixture \"f\"")).unwrap();
        let p = check(&rows, &[fx("f", "\"T03-A\"")], 9);
        assert!(p.iter().any(|s| s.contains("cites a glob")), "{p:?}");
        assert!(p.iter().any(|s| s.contains("twice")), "{p:?}");
    }

    #[test]
    fn orphans_and_wrong_claims_are_reported() {
        let rows = parse(&table("fixture \"f\"")).unwrap();
        let p = check(
            &rows,
            &[
                fx("f", "\"T03-A\" \"T03-ZZ\""),
                fx("g", "\"T03-A\""),
                fx("h", "\"VTCORE-15\""),
            ],
            9,
        );
        assert!(p.iter().any(|s| s.contains("unknown row T03-ZZ")), "{p:?}");
        assert!(
            p.iter()
                .any(|s| s.contains("fixture g is cited by no row") && s.contains("T03-A")),
            "{p:?}"
        );
        assert!(
            p.iter()
                .any(|s| s.contains("fixture g claims T03-A but the row does not cite it")),
            "{p:?}"
        );
        assert!(p.iter().any(|s| s.contains("fixture h is cited by no row")));
    }

    #[test]
    fn malformed_matrix_ids_are_reported() {
        let rows = parse(
            r#"conformance { row "T03-A" spec="T-03" matrix="VTCORE-15,INTEGRATION" tier="R1" milestone="M9" desc="a" { fixture "f" } }"#,
        )
        .unwrap();
        let p = check(&rows, &[fx("f", "\"T03-A\"")], 9);
        assert!(
            p.iter()
                .any(|s| s.contains("malformed matrix id `INTEGRATION`")),
            "{p:?}"
        );
    }
}
