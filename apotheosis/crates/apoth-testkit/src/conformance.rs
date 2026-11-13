// SPDX-License-Identifier: AGPL-3.0-only
//! The machine-readable T-03 conformance table (`fixtures/conformance.kdl`) and its checks.
//!
//! ```kdl
//! conformance {
//!   row "T03-CUU" spec="T-03 §3.1" matrix="VTCORE-15" tier="R1" milestone="M9" desc="CUU"
//! }
//! ```
//! A fixture claims rows with `covers "T03-CUU"`. The table is checked, not trusted:
//! * every row of milestone `M9` is claimed by at least one fixture (T-25 §1);
//! * every `T03-*` id a fixture claims is a row of the table;
//! * every fixture claims at least one row;
//! * row ids are unique and every row names its spec section.
//!
//! Rows of later milestones (`M13`, ...) are listed so the gap is visible; they are
//! enforced from the milestone that lands them (`APOTH_CONFORMANCE_MILESTONE`, default
//! `M9`; rows up to and including that milestone number must be covered).

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
        });
    }
    Ok(rows)
}

fn milestone_number(m: &str) -> u32 {
    m.trim_start_matches('M').parse().unwrap_or(u32::MAX)
}

/// Row id to the fixture ids that claim it.
#[must_use]
pub fn coverage(rows: &[Row], fixtures: &[Fixture]) -> BTreeMap<String, Vec<String>> {
    let mut m: BTreeMap<String, Vec<String>> =
        rows.iter().map(|r| (r.id.clone(), Vec::new())).collect();
    for f in fixtures {
        for c in &f.covers {
            if let Some(v) = m.get_mut(c) {
                v.push(f.id.clone());
            }
        }
    }
    m
}

/// All problems with the table against the corpus; empty when consistent.
#[must_use]
pub fn check(rows: &[Row], fixtures: &[Fixture], enforce_up_to: u32) -> Vec<String> {
    let mut p = Vec::new();
    let mut seen = BTreeSet::new();
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
    }
    let cov = coverage(rows, fixtures);
    for r in rows {
        if milestone_number(&r.milestone) <= enforce_up_to
            && cov.get(&r.id).is_none_or(Vec::is_empty)
        {
            p.push(format!(
                "row {} ({}, {}) cites no fixture",
                r.id, r.spec, r.milestone
            ));
        }
    }
    let ids: BTreeSet<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    for f in fixtures {
        let mine: Vec<&String> = f.covers.iter().filter(|c| c.starts_with("T03-")).collect();
        if mine.is_empty() {
            p.push(format!("fixture {} claims no T03-* row", f.id));
        }
        for c in mine {
            if !ids.contains(c.as_str()) {
                p.push(format!("fixture {} claims unknown row {c}", f.id));
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

    const TABLE: &str = r#"conformance {
  row "T03-A" spec="T-03 §3.1" matrix="VTCORE-15, VTCORE-14" tier="R1" milestone="M9" desc="a"
  row "T03-B" spec="T-03 §3.1" matrix="" tier="R1" milestone="M13" desc="b"
}"#;

    #[test]
    fn parses_rows() {
        let r = parse(TABLE).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].matrix, vec!["VTCORE-15", "VTCORE-14"]);
        assert!(parse("conformance { row \"x\" }").is_err());
    }

    #[test]
    fn a_row_without_a_fixture_fails_the_check() {
        let rows = parse(TABLE).unwrap();
        let p = check(&rows, &[], 9);
        assert_eq!(p.len(), 1);
        assert!(p[0].contains("T03-A") && p[0].contains("cites no fixture"));
        // M13 rows are not enforced at M9, but are at M13.
        assert_eq!(
            check(&rows, &[fx("f", "\"T03-A\"")], 9),
            Vec::<String>::new()
        );
        assert_eq!(check(&rows, &[fx("f", "\"T03-A\"")], 13).len(), 1);
    }

    #[test]
    fn unknown_and_missing_claims_are_reported() {
        let rows = parse(TABLE).unwrap();
        let p = check(
            &rows,
            &[fx("f", "\"T03-A\" \"T03-ZZ\""), fx("g", "\"VTCORE-15\"")],
            9,
        );
        assert!(p.iter().any(|s| s.contains("unknown row T03-ZZ")));
        assert!(
            p.iter()
                .any(|s| s.contains("fixture g claims no T03-* row"))
        );
    }
}
