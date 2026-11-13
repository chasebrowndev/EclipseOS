// SPDX-License-Identifier: AGPL-3.0-only
//! Test-only grid dump hook. Active only in builds with debug assertions (so never in a
//! release binary) and only when `APOTH_TEST_DUMP=<path>` is set. After each burst of output
//! the io thread writes the visible rows (trailing blanks trimmed, one per line) to the path
//! atomically. This is pane content: it is written to the file the test names and never logged.

use std::path::PathBuf;

use apoth_term::Term;

const ENV: &str = "APOTH_TEST_DUMP";

#[derive(Clone, Debug)]
pub(crate) struct DumpHook {
    path: PathBuf,
}

impl DumpHook {
    pub(crate) fn from_env() -> Option<DumpHook> {
        if !cfg!(debug_assertions) {
            return None;
        }
        let p = std::env::var_os(ENV).filter(|p| !p.is_empty())?;
        Some(DumpHook {
            path: PathBuf::from(p),
        })
    }

    /// Render the rows to text (the caller holds the term lock only for this).
    pub(crate) fn render(term: &Term) -> String {
        let mut out = String::new();
        for y in 0..term.rows() {
            out.push_str(term.row_text(y).trim_end());
            out.push('\n');
        }
        out
    }

    pub(crate) fn write(&self, term: &Term) {
        let text = Self::render(term);
        let tmp = self.path.with_extension("tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apoth_term::TermConfig;
    use std::time::Instant;

    #[test]
    fn render_trims_rows() {
        let mut t = Term::new(TermConfig {
            rows: 3,
            cols: 10,
            ..TermConfig::default()
        });
        t.feed(b"hi  \r\nyo", Instant::now());
        assert_eq!(DumpHook::render(&t), "hi\nyo\n\n");
    }
}
