// SPDX-License-Identifier: AGPL-3.0-only
//! Faithful in-place edits to a KDL config file (COMP-13 §1.4).
//!
//! §1.4 carries a blocking VERIFY: can the `kdl` v2 document model round-trip
//! comments and formatting? It can — but the obvious API call is silently
//! wrong, and that is why this module exists rather than a two-line
//! `entry.set_value(); doc.to_string()`.
//!
//! `KdlEntry`'s `Display` prints `format.value_repr` — the *original source
//! text* — whenever `format` is `Some`, falling back to `self.value` only when
//! it is `None` (kdl-6.7.1 `entry.rs:418-441`). `KdlEntry::set_value` mutates
//! only `self.value` (`entry.rs:80`). So on a parsed document:
//!
//! * `entry.set_value(5)` re-serializes the *old* number. A silent no-op write.
//! * `entry.clear_format()` writes the right value but drops that entry's
//!   `leading`/`trailing` — the inline comment and the alignment on that line.
//!
//! So the write path here never re-serializes the document. It byte-splices:
//! find the value's span, replace exactly those bytes with the newly rendered
//! literal, and copy the rest of the file verbatim. Faithful by construction,
//! and the `value_repr` footgun is not reachable from the hot path at all.
//!
//! Prohibited in this path, ever: `autoformat*`, `clear_format_recursive`,
//! `ensure_v1`/`ensure_v2` on the whole document. They are whole-document
//! reformatters; a config file is a human's file.
//!
//! The document model is used only by `config migrate`, where a one-time
//! reformat of the moved node is acceptable and stated.

use kdl::{KdlDocument, KdlNode, KdlValue};

/// Why an edit was refused. Every variant leaves the file untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// The file does not parse as KDL. Nothing is spliced into a broken file.
    Parse(String),
    /// The path names a node that exists but carries no value to replace
    /// (e.g. it is a bare block). Creating a value there would change shape.
    NotAValue(String),
    /// An empty or malformed dotted path.
    BadPath(String),
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "config does not parse: {e}"),
            Self::NotAValue(p) => write!(f, "{p} is a block, not a value"),
            Self::BadPath(p) => write!(f, "malformed config path {p:?}"),
        }
    }
}

impl std::error::Error for EditError {}

/// Set the dotted `path` in `text` to `value`, returning the new file text.
///
/// The result differs from the input in exactly one run of bytes when the key
/// is already present; when it is absent the key (and any missing enclosing
/// blocks) is inserted, indented to match its new siblings. Writing the value a
/// key already holds is byte-identical to the input — a slider dragged back to
/// where it started must not churn the file.
pub fn set_value(text: &str, path: &str, value: &KdlValue) -> Result<String, EditError> {
    let parts: Vec<&str> = path.split('.').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() || parts.len() != path.split('.').count() {
        return Err(EditError::BadPath(path.to_string()));
    }
    let doc: KdlDocument = text.parse().map_err(|e| EditError::Parse(format!("{e}")))?;

    let rendered = value.to_string();
    match resolve(text, &doc, &parts) {
        // The key is present: splice over its value.
        Resolved::Value(span) => {
            if text[span.0..span.1] == rendered {
                return Ok(text.to_string());
            }
            let mut out = String::with_capacity(text.len() + rendered.len());
            out.push_str(&text[..span.0]);
            out.push_str(&rendered);
            out.push_str(&text[span.1..]);
            Ok(out)
        }
        Resolved::Block(path) => Err(EditError::NotAValue(path)),
        // The key is absent: insert it, plus whichever enclosing blocks are
        // missing, at the end of the deepest block that does exist.
        Resolved::Missing {
            depth,
            insert_at,
            indent,
        } => {
            let mut body = String::new();
            for (i, part) in parts[depth..parts.len() - 1].iter().enumerate() {
                body.push_str(&indent);
                body.push_str(&"    ".repeat(i));
                body.push_str(part);
                body.push_str(" {\n");
            }
            let inner = "    ".repeat(parts.len() - 1 - depth);
            body.push_str(&indent);
            body.push_str(&inner);
            body.push_str(parts[parts.len() - 1]);
            body.push(' ');
            body.push_str(&rendered);
            body.push('\n');
            for i in (0..parts.len() - 1 - depth).rev() {
                body.push_str(&indent);
                body.push_str(&"    ".repeat(i));
                body.push_str("}\n");
            }
            let mut out = String::with_capacity(text.len() + body.len());
            out.push_str(&text[..insert_at]);
            if insert_at > 0 && !text[..insert_at].ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&body);
            out.push_str(&text[insert_at..]);
            Ok(out)
        }
    }
}

enum Resolved {
    /// Byte range of the existing value's source text.
    Value((usize, usize)),
    /// The path names a node with children and no value.
    Block(String),
    /// Insert `parts[depth..]` at byte `insert_at`, with `indent` leading each
    /// new line.
    Missing {
        depth: usize,
        insert_at: usize,
        indent: String,
    },
}

fn resolve(text: &str, doc: &KdlDocument, parts: &[&str]) -> Resolved {
    // Later definitions win, matching the parser: it applies nodes in file
    // order, so the last one is the value in effect.
    let mut cur = doc;
    let mut depth = 0;
    let mut node: Option<&KdlNode> = None;
    while let Some(found) = cur
        .nodes()
        .iter()
        .rev()
        .find(|n| n.name().value() == parts[depth])
    {
        node = Some(found);
        depth += 1;
        if depth == parts.len() {
            break;
        }
        match found.children() {
            Some(kids) => cur = kids,
            None => break,
        }
    }

    if depth == parts.len() {
        let node = node.expect("depth advanced");
        return match value_span(node) {
            Some(span) => Resolved::Value(span),
            None => Resolved::Block(parts.join(".")),
        };
    }

    // `cur` is the deepest block that exists. Insert at the end of it, taking
    // indentation from its last child so the new line sits with its siblings.
    let (insert_at, indent) = match cur.nodes().last() {
        Some(last) => {
            let end = line_end(text, last);
            (end, leading_indent(text, last.span().offset()))
        }
        None => (text.len(), String::new()),
    };
    Resolved::Missing {
        depth,
        insert_at,
        indent,
    }
}

/// Byte range of a node's first unnamed (positional) entry, as it appears in
/// the source. The entry's span starts after its leading trivia and ends
/// immediately after the value, so the value is the tail of the span — which
/// is what lets a property (`key=value`) be edited without touching its key.
fn value_span(node: &KdlNode) -> Option<(usize, usize)> {
    let entry = node.entries().iter().find(|e| e.name().is_none())?;
    let repr_len = entry.format()?.value_repr.len();
    let end = entry.span().offset() + entry.span().len();
    Some((end.checked_sub(repr_len)?, end))
}

fn line_end(text: &str, node: &KdlNode) -> usize {
    let end = node.span().offset() + node.span().len();
    let end = end.min(text.len());
    text[end..].find('\n').map_or(text.len(), |i| end + i + 1)
}

fn leading_indent(text: &str, offset: usize) -> String {
    let off = offset.min(text.len());
    let start = text[..off].rfind('\n').map_or(0, |i| i + 1);
    text[start..off]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The §1.4 VERIFY fixture: every formatting feature a human file can
    /// carry, all at once. Deliberately ugly.
    const FIXTURE: &str = "\
// eclipse config
// SPDX-License-Identifier: AGPL-3.0-only
/* a block comment
   spanning lines */
general {
\tgaps-in 5 // inner gap, in px
\tgaps-out 10
\tborder-size 2
}
/-general { gaps-in 99 }
render {
    max-fps 0xDEADbeef
    backend #\"raw \\ string\"#
    note \"\"\"
        multi-line
        \"\"\"
}
output \"eDP-1\" scale   =   1.5
misc {
    vrr 1
}";

    fn diff_runs(a: &str, b: &str) -> usize {
        let (ab, bb) = (a.as_bytes(), b.as_bytes());
        let pre = ab.iter().zip(bb).take_while(|(x, y)| x == y).count();
        let suf = ab[pre..]
            .iter()
            .rev()
            .zip(bb[pre..].iter().rev())
            .take_while(|(x, y)| x == y)
            .count();
        usize::from(pre + suf < ab.len().max(bb.len()))
    }

    #[test]
    fn round_trip_preserves_everything_else() {
        let out = set_value(FIXTURE, "general.gaps-in", &KdlValue::Integer(8)).unwrap();
        // Exactly one run of bytes changed, and it is the one we asked for.
        assert_eq!(diff_runs(FIXTURE, &out), 1, "more than one edit:\n{out}");
        assert!(out.contains("\tgaps-in 8 // inner gap, in px"));
        // Everything a naive re-serialize would have eaten is still there.
        for kept in [
            "// eclipse config",
            "/* a block comment",
            "/-general { gaps-in 99 }",
            "0xDEADbeef",
            "#\"raw \\ string\"#",
            "scale   =   1.5",
        ] {
            assert!(out.contains(kept), "lost {kept:?}");
        }
        assert!(!out.ends_with('\n'), "grew a trailing newline");
        out.parse::<KdlDocument>().expect("still parses");
    }

    /// The trap this module exists to avoid, asserted so a future refactor to
    /// `set_value` + `to_string` fails loudly instead of silently no-opping.
    #[test]
    fn document_model_would_have_lied() {
        let mut doc: KdlDocument = FIXTURE.parse().unwrap();
        let node = doc
            .get_mut("general")
            .and_then(|n| n.children_mut().as_mut())
            .and_then(|d| d.get_mut("gaps-in"))
            .unwrap();
        node.entries_mut()[0].set_value(8);
        assert!(
            doc.to_string().contains("gaps-in 5"),
            "kdl 6.7.1 stopped printing value_repr — re-check the §1.4 VERIFY"
        );
    }

    #[test]
    fn writing_the_same_value_twice_is_byte_identical() {
        let once = set_value(FIXTURE, "general.gaps-in", &KdlValue::Integer(8)).unwrap();
        let twice = set_value(&once, "general.gaps-in", &KdlValue::Integer(8)).unwrap();
        assert_eq!(once, twice);
        // And setting the value it already holds does not touch the file.
        assert_eq!(
            set_value(FIXTURE, "general.gaps-in", &KdlValue::Integer(5)).unwrap(),
            FIXTURE
        );
    }

    #[test]
    fn edits_the_last_definition_that_wins() {
        let text = "general {\n    gaps-in 1\n}\ngeneral {\n    gaps-in 2\n}\n";
        let out = set_value(text, "general.gaps-in", &KdlValue::Integer(9)).unwrap();
        assert_eq!(out, "general {\n    gaps-in 1\n}\ngeneral {\n    gaps-in 9\n}\n");
    }

    #[test]
    fn inserts_a_missing_key_beside_its_siblings() {
        let out = set_value(FIXTURE, "misc.vfr", &KdlValue::Bool(true)).unwrap();
        assert!(out.contains("    vrr 1\n    vfr #true\n}"), "{out}");
        out.parse::<KdlDocument>().unwrap();
    }

    #[test]
    fn inserts_missing_blocks_for_a_deep_path() {
        let out = set_value(FIXTURE, "input.touchpad.natural-scroll", &KdlValue::Bool(true)).unwrap();
        assert!(
            out.ends_with("input {\n    touchpad {\n        natural-scroll #true\n    }\n}\n"),
            "{out}"
        );
        out.parse::<KdlDocument>().unwrap();
    }

    #[test]
    fn refuses_a_block_and_a_bad_path() {
        assert_eq!(
            set_value(FIXTURE, "general", &KdlValue::Integer(1)),
            Err(EditError::NotAValue("general".into()))
        );
        assert!(matches!(
            set_value(FIXTURE, "general..gaps-in", &KdlValue::Integer(1)),
            Err(EditError::BadPath(_))
        ));
        assert!(matches!(
            set_value("general {", "general.x", &KdlValue::Integer(1)),
            Err(EditError::Parse(_))
        ));
    }
}
