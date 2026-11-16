// SPDX-License-Identifier: AGPL-3.0-only
//! Test infrastructure: golden-grid fixtures, oracles, headless driver, benches.
//!
//! Governing spec: T-25 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`).
//!
//! M9 scope: the golden-grid harness (T-25 §1) with chunking re-runs, readable diffs,
//! env-var bless mode, stream replay (§7) and the machine-readable T-03 conformance table.
//!
//! * [`kdl`], [`escapes`], [`fixture`]: the fixture format (`fixtures/**/*.kdl`).
//! * [`snapshot`]: what is observed, and diffs of it.
//! * [`runner`]: [`runner::Driver`], chunked execution, verification, bless, show.
//! * [`adapter`]: the single module that touches `apoth-term` (reconciled at build time).
//! * [`selftest`]: `MiniTerm`, a tiny terminal for testing the harness itself.
//! * [`conformance`]: the T-03 row table and its coverage checks.
//! * [`replay`]: `.apothrec` replay.
//! * [`oracle`]: differential-oracle framework (exclusion list citing T-03 rows).
//!
//! Fixture ids equal file stems and are unique across `fixtures/`. `fixtures/REVIEW.kdl`
//! lists every fixture whose expectation comes from reasoning that needs an owner's eye.
#![forbid(unsafe_code)]

pub mod adapter;
pub mod conformance;
pub mod escapes;
pub mod fixture;
pub mod kdl;
pub mod oracle;
pub mod perf;
pub mod replay;
pub mod runner;
pub mod selftest;
pub mod session;
pub mod snapshot;

use std::path::{Path, PathBuf};

use fixture::Fixture;

/// The crate's fixture directory.
#[must_use]
pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

/// Every `.kdl` fixture under `dir` (recursive), sorted by path, excluding the
/// `conformance.kdl` and `REVIEW.kdl` index files.
///
/// # Errors
/// I/O or parse errors, naming the file.
pub fn load_fixtures(dir: &Path) -> Result<Vec<Fixture>, String> {
    let mut paths = Vec::new();
    collect(dir, &mut paths).map_err(|e| format!("{}: {e}", dir.display()))?;
    paths.sort();
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for p in paths {
        let src = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        let f = Fixture::parse(&src, p.clone()).map_err(|e| e.to_string())?;
        if !seen.insert(f.id.clone()) {
            return Err(format!("duplicate fixture id `{}` ({})", f.id, p.display()));
        }
        out.push(f);
    }
    Ok(out)
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        if p.is_dir() {
            collect(&p, out)?;
        } else if p.extension().is_some_and(|x| x == "kdl") {
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if stem != "conformance" && stem != "REVIEW" {
                out.push(p);
            }
        }
    }
    Ok(())
}
