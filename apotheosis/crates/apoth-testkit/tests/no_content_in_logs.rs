// SPDX-License-Identifier: AGPL-3.0-only
//! T-18 §8 / apotheosis CLAUDE.md "no content in logs": no `tracing` call in any Apotheosis
//! crate names a content-bearing value as a field or a format argument. Content means
//! keystrokes, paste and clipboard data, pane text, titles, link targets, search queries,
//! command lines and working directories. Lengths, counts, ids, classes and error kinds
//! are fine and are what the code logs instead.
//!
//! This is a lexical check, so it errs towards false positives: a hit is either renamed
//! (`bytes = s.len()` rather than `text = s`) or justified in `ALLOWED` below.

use std::path::{Path, PathBuf};

/// Identifiers that, used as a log field or format argument, carry user content.
const CONTENT: &[&str] = &[
    "text",
    "title",
    "uri",
    "url",
    "link",
    "query",
    "selection",
    "clipboard",
    "clip",
    "paste",
    "data",
    "argv",
    "args",
    "cmdline",
    "command",
    "cwd",
    "body",
    "content",
    "chars",
    "utf8",
    "preedit",
    "commit",
    "row_text",
    "hint",
    "label",
    "payload",
];

/// `file:line` entries reviewed as not content (none yet).
const ALLOWED: &[&str] = &[];

const MACROS: &[&str] = &["trace!(", "debug!(", "info!(", "warn!(", "error!("];

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// The argument text of the macro call starting at `open` (just past `(`).
fn call_args(src: &str, open: usize) -> &str {
    let mut depth = 1usize;
    let mut in_str = false;
    let mut esc = false;
    for (i, c) in src[open..].char_indices() {
        if in_str {
            match (esc, c) {
                (true, _) => esc = false,
                (false, '\\') => esc = true,
                (false, '"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return &src[open..open + i];
                }
            }
            _ => {}
        }
    }
    &src[open..]
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Content identifiers used as a field (`name =`, `%name`, `?name`, bare `name,`) or a format
/// argument (`{name}` inside the message, or a trailing positional argument).
fn offenders(args: &str) -> Vec<String> {
    let mut hits = Vec::new();
    let b: Vec<char> = args.chars().collect();
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if c == '"' && (i == 0 || b[i - 1] != '\\') {
            in_str = !in_str;
            i += 1;
            continue;
        }
        if in_str {
            i += 1;
            if c == '{' {
                let start = i;
                while i < b.len() && is_ident(b[i]) {
                    i += 1;
                }
                let word: String = b[start..i].iter().collect();
                if CONTENT.contains(&word.as_str()) && matches!(b.get(i), Some('}' | ':')) {
                    hits.push(word);
                }
            }
            continue;
        }
        if is_ident(c) && (i == 0 || !is_ident(b[i - 1])) {
            let start = i;
            while i < b.len() && is_ident(b[i]) {
                i += 1;
            }
            let word: String = b[start..i].iter().collect();
            if !CONTENT.contains(&word.as_str()) {
                continue;
            }
            let prev = b[..start]
                .iter()
                .rev()
                .find(|c| !c.is_whitespace())
                .copied();
            let next = b[i..].iter().find(|c| !c.is_whitespace()).copied();
            let after_next = b[i..]
                .iter()
                .skip_while(|c| c.is_whitespace())
                .nth(1)
                .copied();
            let field = next == Some('=') && after_next != Some('=');
            let sigil = matches!(prev, Some('%' | '?'));
            // `self.text` or `x.title` passed whole as an argument.
            let arg = prev == Some('.') && matches!(next, Some(',' | ')') | None);
            if field || sigil || arg {
                hits.push(word);
            }
            continue;
        }
        i += 1;
    }
    hits
}

#[test]
fn no_tracing_call_logs_content() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for e in std::fs::read_dir(&crates).unwrap().flatten() {
        rs_files(&e.path().join("src"), &mut files);
    }
    assert!(files.len() > 50, "the scan found the crates");
    let mut bad = Vec::new();
    for f in &files {
        let src = std::fs::read_to_string(f).unwrap();
        for m in MACROS {
            let mut from = 0;
            while let Some(at) = src[from..].find(m) {
                let open = from + at + m.len();
                from = open;
                // Only real macro calls: `tracing::warn!(` or a bare `warn!(` that is not
                // part of a longer identifier (`unwarn!(`).
                let before = src[..open - m.len()].chars().next_back();
                if before.is_some_and(is_ident) {
                    continue;
                }
                let args = call_args(&src, open);
                let hits = offenders(args);
                if hits.is_empty() {
                    continue;
                }
                let line = src[..open].lines().count();
                let name = f.strip_prefix(&crates).unwrap_or(f).display().to_string();
                let key = format!("{name}:{line}");
                if !ALLOWED.contains(&key.as_str()) {
                    bad.push(format!("{key}: {hits:?}"));
                }
            }
        }
    }
    assert!(bad.is_empty(), "content in logs:\n{}", bad.join("\n"));
}

#[test]
fn the_lexer_catches_each_form() {
    assert_eq!(offenders(r#"text = s, "x""#), ["text"]);
    assert_eq!(offenders(r#"%title, "x""#), ["title"]);
    assert_eq!(offenders(r#"?uri, "x""#), ["uri"]);
    assert_eq!(offenders(r#""got {query}""#), ["query"]);
    assert_eq!(offenders(r#""got {}", self.paste"#), ["paste"]);
    assert!(offenders(r#"bytes = text.len(), "x""#).is_empty());
    assert!(offenders(r#""the title changed""#).is_empty());
    assert!(offenders(r#"error = %e, "cannot read the cwd""#).is_empty());
    assert!(offenders(r#"same = text == other, "x""#).is_empty());
}
