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
    /// A named block the edit needs is not in this file (`widget "x"`).
    NotFound(String),
    /// A rename would collide with a block of that name already in the file.
    Exists(String),
    /// A move to a position past the end of the collection.
    Index { index: usize, len: usize },
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "config does not parse: {e}"),
            Self::NotAValue(p) => write!(f, "{p} is a block, not a value"),
            Self::BadPath(p) => write!(f, "malformed config path {p:?}"),
            Self::NotFound(b) => write!(f, "{b} is not defined in this file"),
            Self::Exists(b) => write!(f, "{b} already exists"),
            Self::Index { index, len } => {
                write!(f, "index {index} is out of range: there are {len} entries")
            }
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

// ------------------------------------------------------------ named blocks
//
// Collection entries written as named child blocks of a top-level section —
// `bar { widget "<name>" { … } }` (ADR 0065). Same discipline as above: find
// the block's bytes, splice, copy the rest verbatim. A block the edit
// replaces is re-rendered whole (its inner comments go with it); nothing
// outside it moves. Every result is re-parsed before it is returned, so no
// edit here can hand back a file that does not parse.

/// Which named block a collection edit addresses: `<node> "<name>" { … }`
/// directly inside every top-level `<parent>` block.
#[derive(Debug, Clone, Copy)]
pub struct BlockKind {
    pub parent: &'static str,
    pub node: &'static str,
}

/// `bar { widget "<name>" { … } }`.
pub const WIDGET: BlockKind = BlockKind {
    parent: "bar",
    node: "widget",
};

/// A string as a KDL literal, always quoted — `exec "curl"`, never the bare
/// identifier kdl would pick — so a written block reads like the docs.
pub fn quote(s: &str) -> String {
    let r = KdlValue::String(s.to_owned()).to_string();
    if r.starts_with('"') || r.starts_with('#') {
        r
    } else {
        // A bare identifier contains neither `"` nor `\`, so wrapping it is
        // exact.
        format!("\"{r}\"")
    }
}

/// The distinct block names of `kind` in `text`, in order of first appearance
/// (the order the parser lists them in).
pub fn block_names(text: &str, kind: BlockKind) -> Result<Vec<String>, EditError> {
    let doc = parse(text)?;
    let mut out: Vec<String> = Vec::new();
    for n in blocks(&doc, kind) {
        if let Some(name) = block_name(n) {
            if !out.iter().any(|o| o == name) {
                out.push(name.to_owned());
            }
        }
    }
    Ok(out)
}

/// Write block `name` with `body` (one node per line, unindented). An
/// existing block is replaced in place — the last one, the one in effect —
/// and any earlier duplicates are dropped, so the file ends with exactly one.
/// A new block goes after the last block of its kind, else at the end of the
/// last `parent` section, else in a new `parent` section at the end of the
/// file. Writing the block that is already there is byte-identical.
pub fn upsert_block(text: &str, kind: BlockKind, name: &str, body: &[String]) -> Result<String, EditError> {
    let doc = parse(text)?;
    let all = blocks(&doc, kind);
    let found: Vec<&KdlNode> = all
        .iter()
        .copied()
        .filter(|n| block_name(n) == Some(name))
        .collect();
    if let Some((last, earlier)) = found.split_last() {
        let (s, e) = node_range(text, last);
        let rendered = render_block(kind, name, body, &leading_indent(text, s));
        if earlier.is_empty() && text[s..e] == rendered {
            return Ok(text.to_string());
        }
        let mut edits = vec![(s, e, rendered)];
        edits.extend(earlier.iter().map(|n| removal(text, n)));
        return reparse(splice_all(text, edits));
    }
    let out = if let Some(last) = all.last() {
        insert_after(text, last, |indent| render_block(kind, name, body, indent))
    } else if let Some(parent) = doc
        .nodes()
        .iter()
        .rev()
        .find(|n| n.name().value() == kind.parent && n.children().is_some())
    {
        append_child(text, parent, |indent| render_block(kind, name, body, indent))
    } else {
        let mut out = text.to_string();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!(
            "{} {{\n    {}\n}}\n",
            kind.parent,
            render_block(kind, name, body, "    ")
        ));
        out
    };
    reparse(out)
}

/// Delete every block named `name`, each with its whole line when it has one
/// to itself.
pub fn remove_block(text: &str, kind: BlockKind, name: &str) -> Result<String, EditError> {
    let doc = parse(text)?;
    let edits: Vec<_> = blocks(&doc, kind)
        .into_iter()
        .filter(|n| block_name(n) == Some(name))
        .map(|n| removal(text, n))
        .collect();
    if edits.is_empty() {
        return Err(EditError::NotFound(format!("{} {name:?}", kind.node)));
    }
    reparse(splice_all(text, edits))
}

/// Rename block `name` to `new_name`: only the name literal of the block in
/// effect changes; earlier duplicates are dropped so the old name is gone.
pub fn rename_block(text: &str, kind: BlockKind, name: &str, new_name: &str) -> Result<String, EditError> {
    let doc = parse(text)?;
    let all = blocks(&doc, kind);
    if all.iter().any(|n| block_name(n) == Some(new_name)) {
        return Err(EditError::Exists(format!("{} {new_name:?}", kind.node)));
    }
    let found: Vec<&KdlNode> = all.into_iter().filter(|n| block_name(n) == Some(name)).collect();
    let Some((last, earlier)) = found.split_last() else {
        return Err(EditError::NotFound(format!("{} {name:?}", kind.node)));
    };
    let entry = last
        .entries()
        .iter()
        .find(|e| e.name().is_none())
        .expect("block_name matched a positional entry");
    let (s, e) = value_span_of(entry).ok_or_else(|| EditError::NotAValue(name.to_owned()))?;
    let mut edits = vec![(s, e, quote(new_name))];
    edits.extend(earlier.iter().map(|n| removal(text, n)));
    reparse(splice_all(text, edits))
}

/// Move block `name` to position `index` among the distinct blocks of its
/// kind (as [`block_names`] lists them). The block's bytes move verbatim.
pub fn move_block(text: &str, kind: BlockKind, name: &str, index: usize) -> Result<String, EditError> {
    let names = block_names(text, kind)?;
    let Some(pos) = names.iter().position(|n| n == name) else {
        return Err(EditError::NotFound(format!("{} {name:?}", kind.node)));
    };
    if index >= names.len() {
        return Err(EditError::Index {
            index,
            len: names.len(),
        });
    }
    let doc = parse(text)?;
    let found: Vec<&KdlNode> = blocks(&doc, kind)
        .into_iter()
        .filter(|n| block_name(n) == Some(name))
        .collect();
    if pos == index && found.len() == 1 {
        return Ok(text.to_string());
    }
    let last = found.last().expect("position found it");
    let (s, e) = node_range(text, last);
    let moved = text[s..e].to_string();

    let rest = remove_block(text, kind, name)?;
    let doc = parse(&rest)?;
    let mut firsts: Vec<&KdlNode> = Vec::new();
    for n in blocks(&doc, kind) {
        if !firsts.iter().any(|f| block_name(f) == block_name(n)) {
            firsts.push(n);
        }
    }
    let out = match firsts.get(index) {
        Some(target) => {
            let (s, _) = node_range(&rest, target);
            let indent = leading_indent(&rest, s);
            splice_all(&rest, vec![(s, s, format!("{moved}\n{indent}"))])
        }
        None => {
            let last = firsts
                .last()
                .ok_or_else(|| EditError::NotFound(name.to_owned()))?;
            insert_after(&rest, last, |_| moved.clone())
        }
    };
    reparse(out)
}

/// In every definition of the list node at `path`, replace the argument
/// `old` with `new`, or delete it when `new` is `None`. Only those arguments'
/// bytes change. Returns the text unchanged when `old` is not there.
pub fn replace_list_item(text: &str, path: &str, old: &str, new: Option<&str>) -> Result<String, EditError> {
    let parts: Vec<&str> = path.split('.').collect();
    if parts.iter().any(|p| p.is_empty()) {
        return Err(EditError::BadPath(path.to_string()));
    }
    let doc = parse(text)?;
    let mut nodes = Vec::new();
    nodes_at(&doc, &parts, &mut nodes);
    let mut edits = Vec::new();
    for node in nodes {
        let mut prev_end = node.name().span().offset() + node.name().span().len();
        for entry in node.entries() {
            let end = entry_end(text, entry);
            if entry.name().is_none() && entry.value().as_string() == Some(old) {
                match new {
                    Some(n) => {
                        let (s, e) =
                            value_span_of(entry).ok_or_else(|| EditError::NotAValue(path.to_owned()))?;
                        edits.push((s, e, quote(n)));
                    }
                    // Take the blank before it too, so no double space is left.
                    None => edits.push((prev_end, end, String::new())),
                }
            }
            prev_end = end;
        }
    }
    if edits.is_empty() {
        return Ok(text.to_string());
    }
    reparse(splice_all(text, edits))
}

fn parse(text: &str) -> Result<KdlDocument, EditError> {
    text.parse().map_err(|e| EditError::Parse(format!("{e}")))
}

/// The last safety net: a splice that produced unparseable text is refused
/// rather than written.
fn reparse(out: String) -> Result<String, EditError> {
    parse(&out)?;
    Ok(out)
}

fn blocks(doc: &KdlDocument, kind: BlockKind) -> Vec<&KdlNode> {
    doc.nodes()
        .iter()
        .filter(|n| n.name().value() == kind.parent)
        .filter_map(|n| n.children())
        .flat_map(|c| c.nodes())
        .filter(|n| n.name().value() == kind.node)
        .collect()
}

fn block_name(n: &KdlNode) -> Option<&str> {
    n.entries()
        .iter()
        .find(|e| e.name().is_none())?
        .value()
        .as_string()
}

fn nodes_at<'a>(doc: &'a KdlDocument, parts: &[&str], out: &mut Vec<&'a KdlNode>) {
    for n in doc.nodes().iter().filter(|n| n.name().value() == parts[0]) {
        if parts.len() == 1 {
            out.push(n);
        } else if let Some(kids) = n.children() {
            nodes_at(kids, &parts[1..], out);
        }
    }
}

/// A node's bytes, from its name to its last non-blank byte (kdl's span can
/// carry trailing blanks).
fn node_range(text: &str, n: &KdlNode) -> (usize, usize) {
    let s = n.span().offset().min(text.len());
    let e = (s + n.span().len()).min(text.len());
    (s, s + text[s..e].trim_end().len())
}

fn entry_end(text: &str, e: &kdl::KdlEntry) -> usize {
    let s = e.span().offset().min(text.len());
    let end = (s + e.span().len()).min(text.len());
    s + text[s..end].trim_end().len()
}

/// Whether the rest of a line after `at` holds nothing a removal or an
/// insertion would disturb: blanks, an optional `;`, a `//` comment.
fn tail_is_trivial(text: &str, at: usize) -> bool {
    let le = text[at..].find('\n').map_or(text.len(), |i| at + i);
    let tail = text[at..le].trim_start();
    let tail = tail.strip_prefix(';').unwrap_or(tail).trim_start();
    tail.is_empty() || tail.starts_with("//")
}

/// The bytes to delete to remove node `n`: its whole line(s) when it has
/// them to itself, otherwise the node and a `;` right after it.
fn removal(text: &str, n: &KdlNode) -> (usize, usize, String) {
    let (s, e) = node_range(text, n);
    let ls = text[..s].rfind('\n').map_or(0, |i| i + 1);
    if text[ls..s].trim().is_empty() && tail_is_trivial(text, e) {
        let le = text[e..].find('\n').map_or(text.len(), |i| e + i + 1);
        return (ls, le, String::new());
    }
    let after = text[e..].trim_start_matches([' ', '\t']);
    let e = match after.strip_prefix(';') {
        Some(_) => text.len() - after.len() + 1,
        None => e,
    };
    (s, e, String::new())
}

/// Insert a sibling after `node`, on its own line at the node's indent.
fn insert_after(text: &str, node: &KdlNode, render: impl Fn(&str) -> String) -> String {
    let (s, e) = node_range(text, node);
    let indent = leading_indent(text, s);
    let rendered = render(&indent);
    if tail_is_trivial(text, e) {
        match text[e..].find('\n') {
            Some(i) => splice_all(
                text,
                vec![(e + i + 1, e + i + 1, format!("{indent}{rendered}\n"))],
            ),
            None => splice_all(
                text,
                vec![(text.len(), text.len(), format!("\n{indent}{rendered}"))],
            ),
        }
    } else {
        splice_all(text, vec![(e, e, format!("\n{indent}{rendered}"))])
    }
}

/// Insert a child at the end of `parent`'s block.
fn append_child(text: &str, parent: &KdlNode, render: impl Fn(&str) -> String) -> String {
    if let Some(last) = parent.children().and_then(|c| c.nodes().last()) {
        return insert_after(text, last, render);
    }
    // An empty block: `bar {}` or `bar {\n}`. Insert before its `}`.
    let (s, e) = node_range(text, parent);
    let close = s + text[s..e].rfind('}').unwrap_or(e - s);
    let outer = leading_indent(text, s);
    let unit = if outer.contains('\t') { "\t" } else { "    " };
    let inner = format!("{outer}{unit}");
    let rendered = render(&inner);
    let ls = text[..close].rfind('\n').map_or(0, |i| i + 1);
    if ls > s && text[ls..close].trim().is_empty() {
        splice_all(text, vec![(ls, ls, format!("{inner}{rendered}\n"))])
    } else {
        splice_all(
            text,
            vec![(close, close, format!("\n{inner}{rendered}\n{outer}"))],
        )
    }
}

/// `node "name" {` … `}` with `body` one level in from `indent`. The first
/// line carries no indent (the caller places it); no trailing newline.
fn render_block(kind: BlockKind, name: &str, body: &[String], indent: &str) -> String {
    let unit = if indent.contains('\t') { "\t" } else { "    " };
    let mut s = format!("{} {} {{\n", kind.node, quote(name));
    for line in body {
        s.push_str(indent);
        s.push_str(unit);
        s.push_str(line);
        s.push('\n');
    }
    s.push_str(indent);
    s.push('}');
    s
}

/// Apply non-overlapping `(start, end, replacement)` edits.
fn splice_all(text: &str, mut edits: Vec<(usize, usize, String)>) -> String {
    edits.sort_by_key(|e| std::cmp::Reverse(e.0));
    let mut out = text.to_string();
    for (s, e, r) in edits {
        out.replace_range(s..e, &r);
    }
    out
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

    const BAR: &str = "\
// my config
general { gaps-in 5; } // keep
bar {
    position \"top\" // top!
    // the weather
    widget \"weather\" {
        exec \"curl\" \"wttr.in\" // mine
        interval-ms \"10m\"
    } // after weather
    widget \"cpu\" { source \"usage.cpu\"; }
    widgets {
        order \"custom:weather\" \"clock\" \"custom:cpu\" // order
        important \"custom:cpu\"
    }
}
/* tail */
";

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    /// Everything outside the edited block survives byte for byte.
    fn keeps_the_rest(out: &str) {
        for kept in [
            "// my config\n",
            "general { gaps-in 5; } // keep\n",
            "    position \"top\" // top!\n",
            "    // the weather\n",
            "/* tail */\n",
        ] {
            assert!(out.contains(kept), "lost {kept:?} in\n{out}");
        }
        out.parse::<KdlDocument>().expect("still parses");
    }

    #[test]
    fn upsert_replaces_a_block_in_place() {
        let out = upsert_block(
            BAR,
            WIDGET,
            "cpu",
            &lines(&["source \"usage.mem\"", "format \"{}%\""]),
        )
        .unwrap();
        keeps_the_rest(&out);
        assert!(out.contains(
            "    } // after weather\n    widget \"cpu\" {\n        source \"usage.mem\"\n        format \"{}%\"\n    }\n    widgets {"
        ), "{out}");
        assert_eq!(diff_runs(BAR, &out), 1);
        // Same block again: byte-identical.
        let again = upsert_block(
            &out,
            WIDGET,
            "cpu",
            &lines(&["source \"usage.mem\"", "format \"{}%\""]),
        )
        .unwrap();
        assert_eq!(again, out);
    }

    #[test]
    fn upsert_appends_after_the_last_block() {
        let out = upsert_block(BAR, WIDGET, "up", &lines(&["exec \"uptime\""])).unwrap();
        keeps_the_rest(&out);
        assert!(out.contains(
            "    widget \"cpu\" { source \"usage.cpu\"; }\n    widget \"up\" {\n        exec \"uptime\"\n    }\n    widgets {"
        ), "{out}");
        assert_eq!(block_names(&out, WIDGET).unwrap(), ["weather", "cpu", "up"]);
    }

    #[test]
    fn upsert_creates_bar_when_missing_and_fills_an_empty_one() {
        let out = upsert_block("// only a comment\n", WIDGET, "w", &lines(&["exec \"x\""])).unwrap();
        assert_eq!(
            out,
            "// only a comment\nbar {\n    widget \"w\" {\n        exec \"x\"\n    }\n}\n"
        );
        let out = upsert_block("", WIDGET, "w", &[]).unwrap();
        assert_eq!(out, "bar {\n    widget \"w\" {\n    }\n}\n");
        let out = upsert_block("bar {}\n", WIDGET, "w", &lines(&["exec \"x\""])).unwrap();
        assert_eq!(out, "bar {\n    widget \"w\" {\n        exec \"x\"\n    }\n}\n");
        let out = upsert_block("bar {\n}\n", WIDGET, "w", &lines(&["exec \"x\""])).unwrap();
        assert_eq!(out, "bar {\n    widget \"w\" {\n        exec \"x\"\n    }\n}\n");
        let out = upsert_block("bar {\n    eye #false\n}\n", WIDGET, "w", &lines(&["exec \"x\""])).unwrap();
        assert_eq!(
            out,
            "bar {\n    eye #false\n    widget \"w\" {\n        exec \"x\"\n    }\n}\n"
        );
    }

    #[test]
    fn upsert_collapses_duplicates_to_one() {
        let text = "bar {\n    widget \"a\" { exec \"1\"; }\n    widget \"a\" { exec \"2\"; }\n}\n";
        let out = upsert_block(text, WIDGET, "a", &lines(&["exec \"3\""])).unwrap();
        assert_eq!(out, "bar {\n    widget \"a\" {\n        exec \"3\"\n    }\n}\n");
    }

    #[test]
    fn remove_takes_the_whole_line_and_nothing_else() {
        let out = remove_block(BAR, WIDGET, "weather").unwrap();
        keeps_the_rest(&out);
        assert!(!out.contains("widget \"weather\""), "{out}");
        assert!(!out.contains("after weather"), "{out}");
        assert!(out.contains("    // the weather\n    widget \"cpu\""), "{out}");
        assert_eq!(
            remove_block(BAR, WIDGET, "nope"),
            Err(EditError::NotFound("widget \"nope\"".into()))
        );
        // One-line form: the node and its `;` go, the rest of the line stays.
        let out = remove_block("bar { widget \"a\" { exec \"x\"; }; eye #true; }\n", WIDGET, "a").unwrap();
        out.parse::<KdlDocument>().unwrap();
        assert!(!out.contains("widget"), "{out}");
        assert!(out.contains("eye #true"), "{out}");
    }

    #[test]
    fn rename_changes_only_the_name() {
        let out = rename_block(BAR, WIDGET, "weather", "wttr").unwrap();
        assert_eq!(diff_runs(BAR, &out), 1);
        assert!(out.contains("    widget \"wttr\" {\n        exec \"curl\" \"wttr.in\" // mine\n"));
        assert_eq!(
            rename_block(BAR, WIDGET, "weather", "cpu"),
            Err(EditError::Exists("widget \"cpu\"".into()))
        );
        assert!(matches!(
            rename_block(BAR, WIDGET, "x", "y"),
            Err(EditError::NotFound(_))
        ));
    }

    #[test]
    fn move_reorders_blocks_verbatim() {
        let out = move_block(BAR, WIDGET, "cpu", 0).unwrap();
        keeps_the_rest(&out);
        assert_eq!(block_names(&out, WIDGET).unwrap(), ["cpu", "weather"]);
        // The moved block's bytes are the original ones.
        assert!(out.contains("widget \"cpu\" { source \"usage.cpu\"; }\n"));
        let back = move_block(&out, WIDGET, "cpu", 1).unwrap();
        assert_eq!(block_names(&back, WIDGET).unwrap(), ["weather", "cpu"]);
        assert!(back.contains("        interval-ms \"10m\"\n    } // after weather\n"));
        // In place: untouched.
        assert_eq!(move_block(BAR, WIDGET, "weather", 0).unwrap(), BAR);
        assert_eq!(
            move_block(BAR, WIDGET, "cpu", 2),
            Err(EditError::Index { index: 2, len: 2 })
        );
    }

    #[test]
    fn list_items_are_renamed_and_removed_in_every_definition() {
        let out = replace_list_item(BAR, "bar.widgets.order", "custom:weather", None).unwrap();
        assert!(
            out.contains("        order \"clock\" \"custom:cpu\" // order\n"),
            "{out}"
        );
        let out = replace_list_item(&out, "bar.widgets.important", "custom:cpu", Some("custom:c2")).unwrap();
        assert!(out.contains("        important \"custom:c2\"\n"), "{out}");
        let out = replace_list_item(&out, "bar.widgets.important", "custom:c2", None).unwrap();
        assert!(out.contains("        important\n"), "{out}");
        keeps_the_rest(&out);
        // Absent: byte-identical.
        assert_eq!(
            replace_list_item(BAR, "bar.widgets.order", "custom:zz", None).unwrap(),
            BAR
        );
        // Shadowed earlier definitions are cleaned too.
        let two =
            "bar { widgets { order \"custom:a\" \"clock\"; }; }\nbar { widgets { order \"custom:a\"; }; }\n";
        let out = replace_list_item(two, "bar.widgets.order", "custom:a", None).unwrap();
        assert!(!out.contains("custom:a"), "{out}");
    }

    #[test]
    fn quote_always_quotes_and_round_trips() {
        for s in ["plain", "a b", "true", "1x", "q\"uote", "back\\slash", "{}%", ""] {
            let q = quote(s);
            assert!(q.starts_with('"') || q.starts_with('#'), "{q}");
            let doc: KdlDocument = format!("n {q}").parse().unwrap();
            assert_eq!(doc.nodes()[0].entries()[0].value().as_string(), Some(s));
        }
    }
}
