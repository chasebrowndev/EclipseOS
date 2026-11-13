// SPDX-License-Identifier: Apache-2.0
//! Parse and validation errors for the textual action form, key chords and argv arrays.
//!
//! Errors carry a byte offset when the source is a string, and a near-miss suggestion when
//! the input was an unknown name (T-10 §4 diagnostics shape, applied here too).

use std::fmt;

/// What went wrong, coarsely, so callers can branch without matching on text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Malformed text (quotes, escapes, tokens).
    Syntax,
    /// The action id is not in the registry (consent answers land here too).
    UnknownAction,
    /// The action has no parameter of that name, or too many positionals.
    UnknownParam,
    /// A value failed its type's parse.
    BadValue,
    /// A required parameter is absent.
    MissingParam,
    /// Parsed, but the combination of values is not allowed.
    Invalid,
}

/// A parse or validation error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// Coarse category.
    pub kind: ErrorKind,
    /// Human-readable message. Never contains the offending value for text-bearing
    /// arguments (T-00 rule: no content in logs); names and sizes only.
    pub message: String,
    /// Byte offset into the source text, when there is one.
    pub offset: Option<usize>,
    /// Closest known name, when the input was an unknown name.
    pub suggestion: Option<String>,
}

impl ParseError {
    /// A new error without position or suggestion.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            offset: None,
            suggestion: None,
        }
    }

    /// Attach a byte offset.
    #[must_use]
    pub fn at(mut self, offset: usize) -> Self {
        self.offset = Some(offset);
        self
    }

    /// Attach a suggestion.
    #[must_use]
    pub fn with_suggestion(mut self, suggestion: Option<String>) -> Self {
        self.suggestion = suggestion;
        self
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(o) = self.offset {
            write!(f, " (at byte {o})")?;
        }
        if let Some(s) = &self.suggestion {
            write!(f, "; did you mean `{s}`?")?;
        }
        Ok(())
    }
}

impl std::error::Error for ParseError {}

/// Edit distance between two short names.
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            let v = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
            cur.push(v);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The closest candidate to `name` within a small edit distance, if any.
pub fn suggest<'a>(name: &str, candidates: impl Iterator<Item = &'a str>) -> Option<String> {
    let limit = (name.chars().count() / 3).max(2);
    let mut best: Option<(usize, &str)> = None;
    for c in candidates {
        let d = distance(name, c);
        if d == 0 || d > limit {
            continue;
        }
        if best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, c));
        }
    }
    best.map(|(_, c)| c.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_near_miss() {
        let c = ["pane.split", "pane.close", "tab.new"];
        assert_eq!(
            suggest("pane.splt", c.iter().copied()),
            Some("pane.split".into())
        );
        assert_eq!(suggest("zzzzzzzz", c.iter().copied()), None);
        assert_eq!(suggest("tab.new", c.iter().copied()), None);
    }

    #[test]
    fn display_has_offset_and_suggestion() {
        let e = ParseError::new(ErrorKind::Syntax, "bad")
            .at(3)
            .with_suggestion(Some("x".into()));
        assert_eq!(e.to_string(), "bad (at byte 3); did you mean `x`?");
    }
}
