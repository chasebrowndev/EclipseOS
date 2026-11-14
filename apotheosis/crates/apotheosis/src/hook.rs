// SPDX-License-Identifier: AGPL-3.0-only
//! Test-only grid dump hook. Active only in builds with debug assertions (so never in a
//! release binary) and only when `APOTH_TEST_DUMP=<path>` is set. After each burst of output
//! the io thread writes the visible rows (trailing blanks trimmed, one per line) to the path
//! atomically. This is pane content: it is written to the file the test names and never logged.
//! With several panes the first pane keeps the path and pane `id` writes `<path>.p<id>`.
//!
//! [`MuxDump`] (`APOTH_TEST_MUX=<path>`, debug builds only) is the mux state for the headless
//! tests: tabs, panes, their grid sizes and focus, and the palette. Ids, sizes and the palette
//! query the test typed; never pane text.

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

    /// The hook of a pane: the first pane writes the path itself, pane `id` `<path>.p<id>`.
    pub(crate) fn for_pane(first: bool, id: u64) -> Option<DumpHook> {
        let mut h = Self::from_env()?;
        if !first {
            let mut p = h.path.into_os_string();
            p.push(format!(".p{id}"));
            h.path = PathBuf::from(p);
        }
        Some(h)
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
        // Appended, not replaced: `dump.txt.p3` and `dump.txt.p4` must not share a temp file.
        let mut tmp = self.path.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }
}

/// Test-only dump of what the frame draws (scrolled rows, preedit), as opposed to [`DumpHook`]
/// which dumps the live screen. `APOTH_TEST_VIEW=<path>`, debug builds only. The first line is
/// `offset=<rows scrolled back>`, then one line per visible row. Written when it changes.
pub(crate) struct ViewDump {
    path: PathBuf,
    last: String,
}

impl ViewDump {
    pub(crate) fn from_env() -> Option<ViewDump> {
        if !cfg!(debug_assertions) {
            return None;
        }
        let p = std::env::var_os("APOTH_TEST_VIEW").filter(|p| !p.is_empty())?;
        Some(ViewDump {
            path: PathBuf::from(p),
            last: String::new(),
        })
    }

    pub(crate) fn write(&mut self, rows: &[apoth_grid::SnapRow], offset: usize) {
        let text = format!("offset={offset}\n{}", crate::view::rows_text(rows));
        if text == self.last {
            return;
        }
        let tmp = self.path.with_extension("tmp");
        if std::fs::write(&tmp, &text).is_ok() && std::fs::rename(&tmp, &self.path).is_ok() {
            self.last = text;
        }
    }
}

/// Test-only dump of the image quads the frame draws (T-15 §6): `APOTH_TEST_IMAGES=<path>`,
/// debug builds only, first pane only. One line per quad, bottom to top:
/// `<width>x<height> dst=<x>,<y>,<w>,<h> z=<z>` (pixel sizes and geometry, never pixels).
pub(crate) struct ImageDump {
    path: PathBuf,
    last: Option<String>,
    /// Frames whose texture keys differed from the previous frame's (animation frames
    /// shown), written to `<path>.frames` (T-16 §6 CI-N count).
    keys: Vec<u64>,
    shown: u64,
}

impl ImageDump {
    pub(crate) fn from_env() -> Option<ImageDump> {
        if !cfg!(debug_assertions) {
            return None;
        }
        let p = std::env::var_os("APOTH_TEST_IMAGES").filter(|p| !p.is_empty())?;
        Some(ImageDump {
            path: PathBuf::from(p),
            last: None,
            keys: Vec::new(),
            shown: 0,
        })
    }

    pub(crate) fn write(&mut self, draws: &[apoth_term::ImageDraw]) {
        use std::fmt::Write as _;
        if !draws.is_empty() && !draws.iter().map(|d| d.key).eq(self.keys.iter().copied()) {
            self.keys.clear();
            self.keys.extend(draws.iter().map(|d| d.key));
            self.shown += 1;
            let _ = std::fs::write(self.path.with_extension("frames"), self.shown.to_string());
        }
        let mut text = String::new();
        for d in draws {
            let [x, y, w, h] = d.dst;
            let _ = writeln!(
                text,
                "{}x{} dst={x},{y},{w},{h} z={}",
                d.image.width, d.image.height, d.z
            );
        }
        if self.last.as_deref() == Some(text.as_str()) {
            return;
        }
        let tmp = self.path.with_extension("tmp");
        if std::fs::write(&tmp, &text).is_ok() && std::fs::rename(&tmp, &self.path).is_ok() {
            self.last = Some(text);
        }
    }
}

/// Test-only mux state dump (see the module doc). Written by main when the state changes.
pub(crate) struct MuxDump {
    path: PathBuf,
    last: String,
}

impl MuxDump {
    pub(crate) fn from_env() -> Option<MuxDump> {
        if !cfg!(debug_assertions) {
            return None;
        }
        let p = std::env::var_os("APOTH_TEST_MUX").filter(|p| !p.is_empty())?;
        Some(MuxDump {
            path: PathBuf::from(p),
            last: String::new(),
        })
    }

    pub(crate) fn write(&mut self, text: String) {
        if text == self.last {
            return;
        }
        let tmp = self.path.with_extension("tmp");
        if std::fs::write(&tmp, &text).is_ok() && std::fs::rename(&tmp, &self.path).is_ok() {
            self.last = text;
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
