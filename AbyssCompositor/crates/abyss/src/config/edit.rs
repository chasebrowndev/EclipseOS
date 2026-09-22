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
    splice(text, path, &value.to_string(), Shape::Scalar)
}

/// Set a list-valued key — one node carrying every item as a positional
/// argument, `pinned "network" "battery"` — to exactly `values`, in order.
///
/// Same contract as [`set_value`]: one run of bytes changes, the run being
/// the node's arguments (from the first to the last), so a comment after them
/// on the same line survives. An empty list leaves the bare node, which the
/// parser reads as "explicitly empty" — not the same as deleting the key.
///
/// A node carrying a property (`key=value`) or a child block is refused: the
/// argument run would not be contiguous, and guessing would rewrite bytes the
/// human wrote.
pub fn set_list(text: &str, path: &str, values: &[KdlValue]) -> Result<String, EditError> {
    let rendered: Vec<String> = values.iter().map(ToString::to_string).collect();
    splice(text, path, &rendered.join(" "), Shape::List)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Scalar,
    List,
}

fn splice(text: &str, path: &str, rendered: &str, shape: Shape) -> Result<String, EditError> {
    let parts: Vec<&str> = path.split('.').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() || parts.len() != path.split('.').count() {
        return Err(EditError::BadPath(path.to_string()));
    }
    let doc: KdlDocument = text.parse().map_err(|e| EditError::Parse(format!("{e}")))?;

    match resolve(text, &doc, &parts) {
        // The key is present: splice over its value(s).
        Resolved::Node(node) => {
            let not_a_value = || EditError::NotAValue(parts.join("."));
            let (span, replacement) = match shape {
                Shape::Scalar => (value_span(node).ok_or_else(not_a_value)?, rendered.to_string()),
                Shape::List => match list_span(node).ok_or_else(not_a_value)? {
                    // Emptying: take the separating blanks too, so the bare
                    // node keeps no trailing space.
                    (name_end, _, end) if rendered.is_empty() => ((name_end, end), String::new()),
                    // No arguments yet: the new run brings its own space.
                    (name_end, first, end) if first == end => ((name_end, end), format!(" {rendered}")),
                    // Replace first..last value, keeping the human's spacing
                    // after the name.
                    (_, first, end) => ((first, end), rendered.to_string()),
                },
            };
            if text[span.0..span.1] == replacement {
                return Ok(text.to_string());
            }
            let mut out = String::with_capacity(text.len() + replacement.len());
            out.push_str(&text[..span.0]);
            out.push_str(&replacement);
            out.push_str(&text[span.1..]);
            Ok(out)
        }
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
            if !rendered.is_empty() {
                body.push(' ');
                body.push_str(rendered);
            }
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

enum Resolved<'a> {
    /// The node the path names; the caller decides which bytes of it to
    /// replace.
    Node(&'a KdlNode),
    /// Insert `parts[depth..]` at byte `insert_at`, with `indent` leading each
    /// new line.
    Missing {
        depth: usize,
        insert_at: usize,
        indent: String,
    },
}

fn resolve<'a>(text: &str, doc: &'a KdlDocument, parts: &[&str]) -> Resolved<'a> {
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
        return Resolved::Node(node.expect("depth advanced"));
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
    value_span_of(node.entries().iter().find(|e| e.name().is_none())?)
}

fn value_span_of(entry: &kdl::KdlEntry) -> Option<(usize, usize)> {
    let repr_len = entry.format()?.value_repr.len();
    let end = entry.span().offset() + entry.span().len();
    Some((end.checked_sub(repr_len)?, end))
}

/// Where a list node's arguments sit, as `(name_end, first_value, end)`:
/// the end of the node's name, where the first argument's value starts, and
/// where the last one ends. With no arguments all three are `name_end`.
fn list_span(node: &KdlNode) -> Option<(usize, usize, usize)> {
    if node.children().is_some() || node.entries().iter().any(|e| e.name().is_some()) {
        return None;
    }
    let name_end = node.name().span().offset() + node.name().span().len();
    let (Some(first), Some(last)) = (node.entries().first(), node.entries().last()) else {
        return Some((name_end, name_end, name_end));
    };
    let first_value = value_span_of(first)?.0;
    let end = last.span().offset() + last.span().len();
    Some((name_end, first_value, end))
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

    fn strs(v: &[&str]) -> Vec<KdlValue> {
        v.iter().map(|s| KdlValue::String((*s).to_string())).collect()
    }

    #[test]
    fn a_list_replaces_only_its_arguments() {
        let text = "bar {\n    tray {\n        pinned   \"network\" \"battery\" // mine\n    }\n}\n";
        let out = set_list(text, "bar.tray.pinned", &strs(&["volume", "network"])).unwrap();
        assert_eq!(
            out,
            "bar {\n    tray {\n        pinned   volume network // mine\n    }\n}\n"
        );
        assert_eq!(diff_runs(text, &out), 1);
        // Same list again: byte-identical.
        assert_eq!(
            set_list(&out, "bar.tray.pinned", &strs(&["volume", "network"])).unwrap(),
            out
        );
    }

    #[test]
    fn a_list_empties_to_a_bare_node_and_refills() {
        let text = "bar {\n    tray {\n        hidden a b\n    }\n}\n";
        let empty = set_list(text, "bar.tray.hidden", &[]).unwrap();
        assert_eq!(empty, "bar {\n    tray {\n        hidden\n    }\n}\n");
        let back = set_list(&empty, "bar.tray.hidden", &strs(&["a", "b"])).unwrap();
        assert_eq!(back, text);
        back.parse::<KdlDocument>().unwrap();
    }

    #[test]
    fn a_missing_list_is_inserted_with_its_blocks() {
        let out = set_list(
            "bar {\n    position \"top\"\n}\n",
            "bar.tray.pinned",
            &strs(&["network"]),
        )
        .unwrap();
        assert_eq!(
            out,
            "bar {\n    position \"top\"\n    tray {\n        pinned network\n    }\n}\n"
        );
        let bare = set_list("", "bar.tray.hidden", &[]).unwrap();
        assert_eq!(bare, "bar {\n    tray {\n        hidden\n    }\n}\n");
    }

    /// kdl renders a string as a bare identifier when it can, and quotes it
    /// when it must — an id that looks like a keyword or number survives.
    #[test]
    fn a_list_quotes_what_needs_quoting() {
        let out = set_list("", "pinned", &strs(&["true", "1x", "a b", "org.kde.x"])).unwrap();
        let doc: KdlDocument = out.parse().unwrap();
        let got: Vec<_> = doc.nodes()[0]
            .entries()
            .iter()
            .map(|e| e.value().as_string().unwrap().to_string())
            .collect();
        assert_eq!(got, ["true", "1x", "a b", "org.kde.x"]);
    }

    #[test]
    fn a_list_with_a_property_or_block_is_refused() {
        assert!(matches!(
            set_list("pinned \"a\" x=1\n", "pinned", &strs(&["b"])),
            Err(EditError::NotAValue(_))
        ));
        assert!(matches!(
            set_list("pinned { a }\n", "pinned", &strs(&["b"])),
            Err(EditError::NotAValue(_))
        ));
    }
}
