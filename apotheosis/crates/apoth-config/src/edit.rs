// SPDX-License-Identifier: AGPL-3.0-only
//! Lossless editing (T-10 §5): comments, ordering and unknown nodes survive; only the
//! affected node changes; writes are temp + fsync + rename.

use crate::fs::{Fs, SelfWrites};
use crate::model::{Config, Layer};
use crate::overrides::{RuntimeOverrides, ScopeKey};
use crate::schema::{self, TABLE};
use crate::types::{Key, Value};
use crate::value::{kdl_str, parse_key_value, render_node};
use kdl::{KdlDocument, KdlNode};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub enum EditError {
    UnknownKey(String),
    /// A `Null` value has no node; use `unset`.
    NullValue,
    /// The existing text is not KDL.
    Parse(String),
    /// The existing node holds something that does not parse as the key's type.
    Existing(String),
    /// The value does not fit the key.
    Invalid(String),
    Io(String),
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditError::UnknownKey(k) => write!(f, "unknown key `{k}`"),
            EditError::NullValue => f.write_str("cannot write an unset value; use unset"),
            EditError::Parse(m) => write!(f, "file is not valid KDL: {m}"),
            EditError::Existing(m) => write!(f, "existing value is invalid: {m}"),
            EditError::Invalid(m) => write!(f, "{m}"),
            EditError::Io(m) => write!(f, "write failed: {m}"),
        }
    }
}

impl std::error::Error for EditError {}

/// One row of `config list`.
#[derive(Debug, Clone, PartialEq)]
pub struct ListEntry {
    pub path: &'static str,
    /// The value in the file, or why it does not parse.
    pub value: Result<Value, String>,
}

/// A node to descend into before the key: `profile "work"`, or a `when scheme="dark"` block.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeSel {
    pub name: String,
    pub arg: Option<String>,
    pub props: Vec<(String, String)>,
}

impl NodeSel {
    pub fn profile(name: &str) -> NodeSel {
        NodeSel {
            name: "profile".into(),
            arg: Some(name.into()),
            props: Vec::new(),
        }
    }
    pub fn when(props: &[(&str, &str)]) -> NodeSel {
        NodeSel {
            name: "when".into(),
            arg: None,
            props: props
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn matches(&self, n: &KdlNode) -> bool {
        if n.name().value() != self.name {
            return false;
        }
        if let Some(a) = &self.arg {
            let first = n
                .entries()
                .iter()
                .find(|e| e.name().is_none())
                .and_then(|e| e.value().as_string());
            if first != Some(a.as_str()) {
                return false;
            }
        }
        self.props.iter().all(|(k, v)| {
            n.entry(k.as_str()).and_then(|e| e.value().as_string()) == Some(v.as_str())
        })
    }

    fn text(&self, indent: usize) -> String {
        let pad = " ".repeat(indent);
        let mut s = format!("{pad}{}", kdl_str(&self.name));
        if let Some(a) = &self.arg {
            s.push(' ');
            s.push_str(&kdl_str(a));
        }
        for (k, v) in &self.props {
            s.push_str(&format!(" {}={}", kdl_str(k), kdl_str(v)));
        }
        s.push_str(" {\n");
        s.push_str(&pad);
        s.push_str("}\n");
        s
    }
}

/// A lossless KDL document.
#[derive(Debug, Clone)]
pub struct Editor {
    doc: KdlDocument,
}

fn append_node(doc: &mut KdlDocument, node: KdlNode) {
    if let Some(last) = doc.nodes_mut().last_mut()
        && let Some(f) = last.format_mut()
        && !f.terminator.ends_with('\n')
    {
        f.terminator.push('\n');
    }
    doc.nodes_mut().push(node);
}

fn parse_node(text: &str) -> Result<KdlNode, EditError> {
    KdlNode::parse(text).map_err(|e| {
        EditError::Invalid(format!(
            "cannot render node: {}",
            e.diagnostics
                .first()
                .and_then(|d| d.message.clone())
                .unwrap_or_default()
        ))
    })
}

fn render_chain(segs: &[&str], key: &Key, value: &Value, base: usize) -> Result<String, EditError> {
    let (leaf, parents) = segs.split_last().ok_or(EditError::NullValue)?;
    let mut out = String::new();
    for (i, p) in parents.iter().enumerate() {
        out.push_str(&" ".repeat(base + 4 * i));
        out.push_str(&kdl_str(p));
        out.push_str(" {\n");
    }
    let leaf_text =
        render_node(leaf, &key.ty, value, base + 4 * parents.len()).ok_or(EditError::NullValue)?;
    out.push_str(&leaf_text);
    for i in (0..parents.len()).rev() {
        out.push_str(&" ".repeat(base + 4 * i));
        out.push_str("}\n");
    }
    Ok(out)
}

fn set_in_doc(
    doc: &mut KdlDocument,
    segs: &[&str],
    key: &Key,
    value: &Value,
    level: usize,
) -> Result<(), EditError> {
    let name = segs[0];
    // The loader lets the last node win, so edit the last one (for a block: the last that
    // already holds the key, else the last block).
    let pos = if segs.len() == 1 {
        doc.nodes().iter().rposition(|n| n.name().value() == name)
    } else {
        doc.nodes()
            .iter()
            .rposition(|n| {
                n.name().value() == name
                    && n.children()
                        .is_some_and(|c| find_in(c, &segs[1..]).is_some())
            })
            .or_else(|| doc.nodes().iter().rposition(|n| n.name().value() == name))
    };
    if segs.len() == 1 {
        let text = render_node(name, &key.ty, value, level * 4).ok_or(EditError::NullValue)?;
        let mut new_node = parse_node(&text)?;
        match pos {
            Some(i) => {
                let old = &mut doc.nodes_mut()[i];
                if let Some(mut f) = old.format().cloned() {
                    if old.children().is_none() && new_node.children().is_some() {
                        f.before_children = " ".into();
                    }
                    new_node.set_format(f);
                }
                *old = new_node;
            }
            None => append_node(doc, new_node),
        }
        return Ok(());
    }
    match pos {
        Some(i) => {
            let node = &mut doc.nodes_mut()[i];
            if node.children().is_none() {
                node.set_children(KdlDocument::new());
                if let Some(f) = node.format_mut() {
                    f.before_children = " ".into();
                }
            }
            match node.children_mut().as_mut() {
                Some(child) => set_in_doc(child, &segs[1..], key, value, level + 1),
                None => Err(EditError::Invalid("cannot open block".into())),
            }
        }
        None => {
            let text = render_chain(segs, key, value, level * 4)?;
            append_node(doc, parse_node(&text)?);
            Ok(())
        }
    }
}

fn only_space(s: Option<&kdl::KdlDocumentFormat>) -> bool {
    s.map(|f| f.leading.trim().is_empty() && f.trailing.trim().is_empty())
        .unwrap_or(true)
}

fn unset_in(doc: &mut KdlDocument, segs: &[&str]) -> bool {
    // Every node of that name goes: the loader resolves duplicates, so leaving one behind
    // would keep the key set.
    let mut any = false;
    let mut i = doc.nodes().len();
    while i > 0 {
        i -= 1;
        if doc.nodes()[i].name().value() != segs[0] {
            continue;
        }
        if segs.len() == 1 {
            let removed = doc.nodes_mut().remove(i);
            any = true;
            // A document header comment lives in the first node's leading text; keep it.
            if i == 0
                && let Some(f) = removed.format()
                && !f.leading.trim().is_empty()
            {
                let lead = f.leading.clone();
                if let Some(next) = doc.nodes_mut().first_mut() {
                    if let Some(nf) = next.format_mut() {
                        nf.leading = format!("{lead}{}", nf.leading);
                    }
                } else if let Some(df) = doc.format_mut() {
                    df.trailing = format!("{lead}{}", df.trailing);
                }
            }
            continue;
        }
        let removed = match doc.nodes_mut()[i].children_mut().as_mut() {
            Some(c) => unset_in(c, &segs[1..]),
            None => false,
        };
        if removed {
            any = true;
            let empty = doc.nodes()[i]
                .children()
                .map(|c| c.nodes().is_empty() && only_space(c.format()))
                .unwrap_or(false);
            if empty {
                doc.nodes_mut().remove(i);
            }
        }
    }
    any
}

fn descend<'a>(
    doc: &'a mut KdlDocument,
    sels: &[NodeSel],
    level: usize,
) -> Result<(&'a mut KdlDocument, usize), EditError> {
    let Some((first, rest)) = sels.split_first() else {
        return Ok((doc, level));
    };
    let idx = match doc.nodes().iter().rposition(|n| first.matches(n)) {
        Some(i) => i,
        None => {
            let node = parse_node(&first.text(level * 4))?;
            append_node(doc, node);
            doc.nodes().len() - 1
        }
    };
    let node = &mut doc.nodes_mut()[idx];
    if node.children().is_none() {
        node.set_children(KdlDocument::new());
        if let Some(f) = node.format_mut() {
            f.before_children = " ".into();
        }
    }
    match node.children_mut().as_mut() {
        Some(child) => descend(child, rest, level + 1),
        None => Err(EditError::Invalid("cannot open block".into())),
    }
}

fn find_in<'a>(doc: &'a KdlDocument, segs: &[&str]) -> Option<&'a KdlNode> {
    // Last match wins, as in the loader; fall back to earlier blocks that hold the key.
    doc.nodes()
        .iter()
        .rev()
        .filter(|n| n.name().value() == segs[0])
        .find_map(|n| {
            if segs.len() == 1 {
                Some(n)
            } else {
                find_in(n.children()?, &segs[1..])
            }
        })
}

fn key_of(path: &str) -> Result<&'static Key, EditError> {
    schema::key(path).ok_or_else(|| EditError::UnknownKey(path.to_string()))
}

impl Editor {
    pub fn parse(text: &str) -> Result<Editor, EditError> {
        KdlDocument::parse(text)
            .map(|doc| Editor { doc })
            .map_err(|e| {
                EditError::Parse(
                    e.diagnostics
                        .first()
                        .and_then(|d| d.message.clone())
                        .unwrap_or_default(),
                )
            })
    }

    /// The edited text. Unedited nodes are byte-identical to the input.
    pub fn text(&self) -> String {
        self.doc.to_string()
    }

    /// Read a key's current value from the document (top level, or under `scope`).
    pub fn get_in(&self, scope: &[NodeSel], path: &str) -> Result<Option<Value>, EditError> {
        let key = key_of(path)?;
        let mut doc = &self.doc;
        for s in scope {
            match doc
                .nodes()
                .iter()
                .rfind(|n| s.matches(n))
                .and_then(|n| n.children())
            {
                Some(c) => doc = c,
                None => return Ok(None),
            }
        }
        let segs: Vec<&str> = path.split('.').collect();
        match find_in(doc, &segs) {
            Some(n) => parse_key_value(key, n)
                .map(Some)
                .map_err(|e| EditError::Existing(e.message)),
            None => Ok(None),
        }
    }

    pub fn get(&self, path: &str) -> Result<Option<Value>, EditError> {
        self.get_in(&[], path)
    }

    /// `config list`: every schema key present at top level of the document, in schema order,
    /// optionally under a dotted prefix (`font` matches `font.size`). A node that holds a
    /// value the key's type rejects is listed with its error, never skipped.
    pub fn list(&self, prefix: Option<&str>) -> Vec<ListEntry> {
        let mut out = Vec::new();
        for k in TABLE {
            if let Some(p) = prefix
                && k.path != p
                && !k.path.strip_prefix(p).is_some_and(|r| r.starts_with('.'))
            {
                continue;
            }
            match self.get(k.path) {
                Ok(Some(v)) => out.push(ListEntry {
                    path: k.path,
                    value: Ok(v),
                }),
                Ok(None) => {}
                Err(e) => out.push(ListEntry {
                    path: k.path,
                    value: Err(e.to_string()),
                }),
            }
        }
        out
    }

    /// `config set`: write the key at top level.
    pub fn set(&mut self, path: &str, value: &Value) -> Result<(), EditError> {
        self.set_in(&[], path, value)
    }

    /// Write the key inside the given scope nodes (a profile, a `when` block), creating them.
    pub fn set_in(
        &mut self,
        scope: &[NodeSel],
        path: &str,
        value: &Value,
    ) -> Result<(), EditError> {
        let key = key_of(path)?;
        if matches!(value, Value::Null) {
            return Err(EditError::NullValue);
        }
        if !crate::overrides::value_fits(&key.ty, value) {
            return Err(EditError::Invalid(format!("value does not fit `{path}`")));
        }
        crate::overrides::map_entries_ok(path, value).map_err(EditError::Invalid)?;
        let segs: Vec<&str> = path.split('.').collect();
        let (doc, level) = descend(&mut self.doc, scope, 0)?;
        set_in_doc(doc, &segs, key, value, level)
    }

    /// `config unset`. True if a node was removed.
    pub fn unset(&mut self, path: &str) -> Result<bool, EditError> {
        self.unset_in(&[], path)
    }

    pub fn unset_in(&mut self, scope: &[NodeSel], path: &str) -> Result<bool, EditError> {
        key_of(path)?;
        let segs: Vec<&str> = path.split('.').collect();
        // Do not create scope nodes just to remove from them.
        let mut doc = &self.doc;
        for s in scope {
            match doc
                .nodes()
                .iter()
                .rfind(|n| s.matches(n))
                .and_then(|n| n.children())
            {
                Some(c) => doc = c,
                None => return Ok(false),
            }
        }
        let (doc, _) = descend(&mut self.doc, scope, 0)?;
        Ok(unset_in(doc, &segs))
    }

    /// `config add`: append an item to a list key (no-op when present).
    pub fn add(&mut self, path: &str, item: &str) -> Result<(), EditError> {
        let key = key_of(path)?;
        if !key.ty.is_arg_list() {
            return Err(EditError::Invalid(format!("`{path}` is not a list")));
        }
        let mut list = match self.get(path)? {
            Some(Value::List(l)) => l,
            _ => Vec::new(),
        };
        if !list.iter().any(|i| i == item) || matches!(key.ty, crate::types::Ty::Argv) {
            list.push(item.to_string());
        }
        self.set(path, &Value::List(list))
    }

    /// `config remove`: drop an item from a list key.
    pub fn remove(&mut self, path: &str, item: &str) -> Result<bool, EditError> {
        let key = key_of(path)?;
        if !key.ty.is_arg_list() {
            return Err(EditError::Invalid(format!("`{path}` is not a list")));
        }
        let Some(Value::List(mut list)) = self.get(path)? else {
            return Ok(false);
        };
        let before = list.len();
        list.retain(|i| i != item);
        if list.len() == before {
            return Ok(false);
        }
        if list.is_empty() {
            self.unset(path)?;
        } else {
            self.set(path, &Value::List(list))?;
        }
        Ok(true)
    }

    /// Set one entry of a map key (a grant, an env var, a symbol-map range).
    pub fn map_set(&mut self, path: &str, entry: &str, args: &[String]) -> Result<(), EditError> {
        let key = key_of(path)?;
        let mut map = match self.get(path)? {
            Some(Value::Map(m)) => m,
            _ => Vec::new(),
        };
        map.retain(|(k, _)| k != entry);
        map.push((entry.to_string(), args.to_vec()));
        if !matches!(key.ty, crate::types::Ty::Map { .. }) {
            return Err(EditError::Invalid(format!("`{path}` is not a map")));
        }
        self.set(path, &Value::Map(map))
    }

    /// Remove one entry of a map key.
    pub fn map_remove(&mut self, path: &str, entry: &str) -> Result<bool, EditError> {
        let Some(Value::Map(mut map)) = self.get(path)? else {
            return Ok(false);
        };
        let before = map.len();
        map.retain(|(k, _)| k != entry);
        if map.len() == before {
            return Ok(false);
        }
        self.set(path, &Value::Map(map))?;
        Ok(true)
    }

    /// Make sure the file starts with a `version` node (T-10 §6).
    pub fn ensure_version(&mut self) -> Result<(), EditError> {
        if self
            .doc
            .nodes()
            .iter()
            .any(|n| n.name().value() == "version")
        {
            return Ok(());
        }
        let node = parse_node(&format!("version {}\n", crate::version::CURRENT_VERSION))?;
        self.doc.nodes_mut().insert(0, node);
        Ok(())
    }
}

/// Where a write for `path` goes (T-10 §5): the main user file at top level, unless the caller
/// is in a scoped context, or the winning value comes from an included user file (edited in
/// place). `/etc` and fragment values are never edited: the write lands in the user file.
pub fn write_target(cfg: &Config, user_main: &Path, path: &str) -> PathBuf {
    if let Some(o) = cfg.origin(path)
        && o.layer == Layer::User
        && let Some(f) = &o.file
        && !o.when
    {
        return f.clone();
    }
    user_main.to_path_buf()
}

/// Read, edit, write atomically. `value: None` unsets. Returns the file written, or `None`
/// if the text was already as wanted. Records the content hash in `suppress` so the watcher
/// ignores our own write (CONFIG-16).
pub fn apply_write(
    fs: &dyn Fs,
    cfg: &Config,
    user_main: &Path,
    scope: &[NodeSel],
    path: &str,
    value: Option<&Value>,
    suppress: Option<&mut SelfWrites>,
) -> Result<Option<PathBuf>, EditError> {
    key_of(path)?;
    let target = if scope.is_empty() {
        write_target(cfg, user_main, path)
    } else {
        user_main.to_path_buf()
    };
    let old = match fs.read_to_string(&target) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(EditError::Io(format!("{:?}", e.kind()))),
    };
    let mut ed = Editor::parse(&old)?;
    match value {
        Some(v) => ed.set_in(scope, path, v)?,
        None => {
            ed.unset_in(scope, path)?;
        }
    }
    let new = ed.text();
    if new == old {
        return Ok(None);
    }
    fs.write_atomic(&target, &new)
        .map_err(|e| EditError::Io(format!("{:?}", e.kind())))?;
    if let Some(s) = suppress {
        s.record(&target, &new);
    }
    Ok(Some(target))
}

/// `config.save-override`: persist every runtime override at a scope.
pub fn save_overrides(
    fs: &dyn Fs,
    cfg: &Config,
    user_main: &Path,
    overrides: &RuntimeOverrides,
    at: ScopeKey,
    mut suppress: Option<&mut SelfWrites>,
) -> Result<Vec<PathBuf>, EditError> {
    let mut written = Vec::new();
    for (path, v) in overrides.entries_at(at) {
        debug_assert!(TABLE.iter().any(|k| k.path == path));
        if let Some(p) = apply_write(
            fs,
            cfg,
            user_main,
            &[],
            path,
            Some(&v),
            suppress.as_deref_mut(),
        )? && !written.contains(&p)
        {
            written.push(p);
        }
    }
    Ok(written)
}
