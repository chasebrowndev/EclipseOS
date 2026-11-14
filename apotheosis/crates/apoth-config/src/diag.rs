// SPDX-License-Identifier: AGPL-3.0-only
//! Diagnostics: file, line, column, key, message, near-miss suggestion (T-10 §4).

use std::path::{Path, PathBuf};

/// Diagnostics kept per load; a hostile file cannot make the report grow without bound.
pub const MAX_DIAGNOSTICS: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// One finding. Line and column are 1-based; `line == 0` means "no position" (a missing
/// file, a flag, a runtime override).
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub file: Option<PathBuf>,
    pub line: usize,
    pub column: usize,
    pub key: Option<String>,
    pub message: String,
    pub suggestion: Option<String>,
}

impl Diagnostic {
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    /// `file:line:col: error: message (did you mean `x`?)`. Never includes values.
    pub fn render(&self) -> String {
        let sev = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        let file = self
            .file
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<config>".to_string());
        let mut s = if self.line > 0 {
            format!(
                "{file}:{}:{}: {sev}: {}",
                self.line, self.column, self.message
            )
        } else {
            format!("{file}: {sev}: {}", self.message)
        };
        if let Some(sug) = &self.suggestion {
            s.push_str(&format!(" (did you mean `{sug}`?)"));
        }
        s
    }
}

/// 1-based line and column (in chars) of a byte offset. One-off helper; the loader uses a
/// [`LineIndex`] so the cost per diagnostic is a binary search, not a rescan of the file.
pub fn line_col(src: &str, offset: usize) -> (usize, usize) {
    LineIndex::new(src).line_col(src, offset)
}

/// Byte offsets of the start of every line, built once per file.
#[derive(Debug, Clone, Default)]
pub struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    pub fn new(text: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        LineIndex { starts }
    }

    /// 1-based `(line, column)`; the column counts chars from the line start.
    pub fn line_col(&self, text: &str, offset: usize) -> (usize, usize) {
        let mut offset = offset.min(text.len());
        // Snap onto a char boundary so a span in the middle of a char cannot panic.
        while !text.is_char_boundary(offset) {
            offset -= 1;
        }
        let line = self.starts.partition_point(|&s| s <= offset) - 1;
        let col = text[self.starts[line]..offset].chars().count() + 1;
        (line + 1, col)
    }
}

/// The text of one file plus its path, to turn byte offsets into positions.
#[derive(Debug, Clone, Copy)]
pub struct Source<'a> {
    pub file: Option<&'a Path>,
    pub text: &'a str,
    pub lines: &'a LineIndex,
}

impl Source<'_> {
    pub fn diag(
        &self,
        severity: Severity,
        offset: usize,
        key: Option<&str>,
        message: impl Into<String>,
        suggestion: Option<String>,
    ) -> Diagnostic {
        let (line, column) = self.lines.line_col(self.text, offset);
        Diagnostic {
            severity,
            file: self.file.map(|p| p.to_path_buf()),
            line,
            column,
            key: key.map(|k| k.to_string()),
            message: message.into(),
            suggestion,
        }
    }

    pub fn error(
        &self,
        offset: usize,
        key: Option<&str>,
        message: impl Into<String>,
    ) -> Diagnostic {
        self.diag(Severity::Error, offset, key, message, None)
    }

    pub fn error_sug(
        &self,
        offset: usize,
        key: Option<&str>,
        message: impl Into<String>,
        suggestion: Option<String>,
    ) -> Diagnostic {
        self.diag(Severity::Error, offset, key, message, suggestion)
    }

    pub fn warn(&self, offset: usize, key: Option<&str>, message: impl Into<String>) -> Diagnostic {
        self.diag(Severity::Warning, offset, key, message, None)
    }
}

/// Edit distance between two strings.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            cur.push((prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The closest candidate within a distance of `max(2, len / 3)`, if any.
pub fn suggest<S: AsRef<str>>(input: &str, candidates: &[S]) -> Option<String> {
    let limit = (input.chars().count() / 3).max(2);
    let mut best: Option<(usize, &str)> = None;
    for c in candidates {
        let c = c.as_ref();
        let d = levenshtein(input, c);
        if d <= limit && best.map(|(bd, _)| d < bd).unwrap_or(true) {
            best = Some((d, c));
        }
    }
    best.map(|(_, c)| c.to_string())
}
