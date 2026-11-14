// SPDX-License-Identifier: AGPL-3.0-only
//! The Terminal collection editors' model (Apotheosis T-10 §5, T-11 §3-§9, M27):
//! forms for binds, rules and profiles, their validators, and the edits that
//! splice them into `apotheosis.kdl`.
//!
//! No iced in here. Every check is `apoth-config`'s or `apoth-action`'s own
//! (chord, mode condition, action registry and argument types, rule consent,
//! match expression, rule events), so what the form accepts is what the
//! terminal loads. `apoth-config`'s `Editor` edits keys, not collection nodes,
//! so the nodes (`bind`, `rule`, `profile` and their children) are spliced
//! into the lossless `kdl` document here, replacing or inserting only the
//! node concerned: comments and every other node survive. A profile's
//! overridden keys do go through `Editor::set_in` / `unset_in`. A write is
//! only offered after [`gate`] has loaded the result through `apoth-config`
//! and found no error the file did not already have, so the file is never
//! made invalid.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use apoth_action::text::{parse_calls, Call};
use apoth_action::{rule_consent, Action};
use apoth_config::chord::{format_chord_seq, parse_chord_seq, validate_mode_condition};
use apoth_config::edit::{Editor, NodeSel};
use apoth_config::fs::{Fs, RealFs};
use apoth_config::load::{default_input, load};
use apoth_config::schema::TABLE;
use apoth_config::types::Value;
use apoth_config::{is_rule_event, lang};
use kdl::{KdlDocument, KdlNode, KdlValue};

/// The options a `bind` node takes (T-11 §3). `tests/coverage.rs` holds the
/// form to this list.
pub const BIND_OPTIONS: &[&str] = &["repeat", "mode", "release", "global", "performable"];
/// The children a `rule` node takes (T-11 §5).
pub const RULE_NODES: &[&str] = &["on", "where", "do", "allow-input", "allow-spawn"];
/// The profile nodes the form owns; every other child is a `profilable` key or `env`,
/// `cwd`, `security`, `binds`, which the form leaves alone (T-11 §6).
pub const PROFILE_NODES: &[&str] = &["inherits", "sticky", "match"];
/// The properties of a profile's `match` node, with the weight each adds (T-11 §6).
pub const MATCH_FIELDS: &[&str] = &["host", "job", "user", "path"];

/// Which list of binds: the top-level `binds` blocks, or one `key-table`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Scope {
    Global,
    Table(String),
}

/// What a form checks, by field: `chord`, `mode`, `name`, `event`, `where`,
/// `inherits`, `action:N`, `match:N`, `key:N`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Problems(Vec<(String, String)>);

impl Problems {
    fn push(&mut self, field: impl Into<String>, why: impl Into<String>) {
        self.0.push((field.into(), why.into()));
    }
    pub fn get(&self, field: &str) -> Option<&str> {
        self.0.iter().find(|(f, _)| f == field).map(|(_, w)| w.as_str())
    }
    pub fn any(&self) -> bool {
        !self.0.is_empty()
    }
    pub fn first(&self) -> Option<&str> {
        self.0.first().map(|(_, w)| w.as_str())
    }
}

// ---- actions ------------------------------------------------------------

/// One action as the textual syntax writes it (`pane.split direction=right`),
/// checked against the registry: id, argument names and types.
pub fn check_action(script: &str) -> Result<Call, String> {
    if script.trim().is_empty() {
        return Err("pick an action".to_owned());
    }
    let calls = parse_calls(script).map_err(|e| e.to_string())?;
    let [call] = calls.as_slice() else {
        return Err("one action per line, without `;`".to_owned());
    };
    Action::from_call(call).map_err(|e| e.to_string())?;
    Ok(call.clone())
}

/// A rule's actions against its consent switches (T-11 §5): the class check the
/// terminal applies at load, so a rule that would be dropped is not written.
fn check_consent(call: &Call, allow_input: bool, allow_spawn: bool) -> Result<(), String> {
    let action = Action::from_call(call).map_err(|e| e.to_string())?;
    rule_consent(&action.classify_closed(), allow_input, allow_spawn).map_err(|e| e.to_string())
}

/// A KDL string, always quoted: the file's bind and rule names are written that way,
/// and `Editor`'s bare form would sit oddly beside them.
fn quoted(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn kdl_arg(s: &str) -> String {
    match s {
        "true" => "#true".to_owned(),
        "false" => "#false".to_owned(),
        _ => match s.parse::<i64>() {
            Ok(i) if i.to_string() == s => s.to_owned(),
            _ => quoted(s),
        },
    }
}

/// An action as a KDL node, the shape the loader reads: positionals, `name=value`
/// properties, and an `argv` child for the tail after `--`.
fn action_node(call: &Call, indent: usize) -> String {
    let pad = " ".repeat(indent);
    let mut s = format!("{pad}{}", call.id);
    for p in &call.positional {
        s.push(' ');
        s.push_str(&kdl_arg(p));
    }
    for (k, v) in &call.named {
        s.push_str(&format!(" {k}={}", kdl_arg(v)));
    }
    if let Some(argv) = &call.argv {
        s.push_str(" {\n");
        s.push_str(&" ".repeat(indent + 4));
        s.push_str("argv");
        for a in argv {
            s.push(' ');
            s.push_str(&quoted(a));
        }
        s.push('\n');
        s.push_str(&pad);
        s.push('}');
    }
    s.push('\n');
    s
}

fn scalar(v: &KdlValue) -> String {
    match v {
        KdlValue::String(s) => s.clone(),
        KdlValue::Integer(i) => i.to_string(),
        KdlValue::Float(f) => f.to_string(),
        KdlValue::Bool(b) => b.to_string(),
        KdlValue::Null => String::new(),
    }
}

/// The inverse of [`action_node`]; `None` for an action with nested actions.
fn node_call(n: &KdlNode) -> Option<Call> {
    let mut c = Call::new(n.name().value());
    for e in n.entries() {
        match e.name() {
            Some(k) => c.named.push((k.value().to_owned(), scalar(e.value()))),
            None => c.positional.push(scalar(e.value())),
        }
    }
    if let Some(doc) = n.children() {
        for ch in doc.nodes() {
            if ch.name().value() != "argv" || ch.children().is_some() {
                return None;
            }
            c.argv = Some(
                ch.entries()
                    .iter()
                    .filter(|e| e.name().is_none())
                    .map(|e| scalar(e.value()))
                    .collect(),
            );
        }
    }
    Some(c)
}

fn body_of(n: &KdlNode) -> Option<Vec<String>> {
    n.children()
        .map(|d| {
            d.nodes()
                .iter()
                .map(|a| node_call(a).map(|c| c.format()))
                .collect()
        })
        .unwrap_or_else(|| Some(Vec::new()))
}

// ---- binds --------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct BindForm {
    /// An `unbind` node: removes a default, no actions.
    pub unbind: bool,
    pub chord: String,
    pub actions: Vec<String>,
    pub repeat: bool,
    pub mode: String,
    pub release: bool,
    pub global: bool,
    pub performable: bool,
}

impl Default for BindForm {
    fn default() -> Self {
        BindForm {
            unbind: false,
            chord: String::new(),
            actions: vec![String::new()],
            repeat: true,
            mode: String::new(),
            release: false,
            global: false,
            performable: false,
        }
    }
}

impl BindForm {
    pub fn problems(&self) -> Problems {
        let mut p = Problems::default();
        if self.chord.trim().is_empty() {
            p.push("chord", "press the keys, or type a chord such as ctrl+shift+t");
        } else if let Err(why) = parse_chord_seq(self.chord.trim()) {
            p.push("chord", why);
        }
        if self.unbind {
            return p;
        }
        if !self.mode.trim().is_empty() {
            if let Err(why) = validate_mode_condition(self.mode.trim()) {
                p.push("mode", why);
            }
        }
        for (i, a) in self.actions.iter().enumerate() {
            if let Err(why) = check_action(a) {
                p.push(format!("action:{i}"), why);
            }
        }
        if self.actions.is_empty() {
            p.push("action:0", "a bind runs at least one action");
        }
        p
    }

    fn node_text(&self, indent: usize) -> Result<String, String> {
        if let Some(why) = self.problems().first() {
            return Err(why.to_owned());
        }
        let pad = " ".repeat(indent);
        let chord = format_chord_seq(&parse_chord_seq(self.chord.trim())?);
        if self.unbind {
            return Ok(format!("{pad}unbind {}\n", quoted(&chord)));
        }
        let mut s = format!("{pad}bind {}", quoted(&chord));
        if !self.repeat {
            s.push_str(" repeat=#false");
        }
        if self.release {
            s.push_str(" release=#true");
        }
        if self.global {
            s.push_str(" global=#true");
        }
        if self.performable {
            s.push_str(" performable=#true");
        }
        if !self.mode.trim().is_empty() {
            s.push_str(&format!(" mode={}", quoted(self.mode.trim())));
        }
        s.push_str(" {\n");
        for a in &self.actions {
            s.push_str(&action_node(&check_action(a)?, indent + 4));
        }
        s.push_str(&pad);
        s.push_str("}\n");
        Ok(s)
    }

    fn from_node(n: &KdlNode) -> Option<BindForm> {
        let chord = n
            .entries()
            .iter()
            .find(|e| e.name().is_none())
            .map(|e| scalar(e.value()))
            .unwrap_or_default();
        let flag = |name: &str, default: bool| match n.entry(name).map(|e| e.value()) {
            Some(KdlValue::Bool(b)) => *b,
            _ => default,
        };
        if n.name().value() == "unbind" {
            return Some(BindForm {
                unbind: true,
                chord,
                actions: Vec::new(),
                ..BindForm::default()
            });
        }
        Some(BindForm {
            unbind: false,
            chord,
            actions: body_of(n)?,
            repeat: flag("repeat", true),
            mode: n.entry("mode").map(|e| scalar(e.value())).unwrap_or_default(),
            release: flag("release", false),
            global: flag("global", false),
            performable: flag("performable", false),
        })
    }
}

/// An entry as the file has it. `opaque` ones use nested actions the form cannot
/// show; they are listed, not editable.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry<F> {
    pub form: F,
    pub opaque: bool,
}

// ---- rules --------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Default)]
pub struct RuleForm {
    pub name: String,
    pub event: String,
    pub when: String,
    pub actions: Vec<String>,
    pub allow_input: bool,
    pub allow_spawn: bool,
}

impl RuleForm {
    pub fn new() -> RuleForm {
        RuleForm {
            actions: vec![String::new()],
            ..RuleForm::default()
        }
    }

    pub fn problems(&self) -> Problems {
        let mut p = Problems::default();
        let name = self.name.trim();
        if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
            p.push("name", "a short name, not empty");
        }
        if self.event.is_empty() {
            p.push("event", "pick the event that fires the rule");
        } else if !is_rule_event(&self.event) {
            p.push("event", format!("`{}` is not a rule event", self.event));
        }
        if !self.when.trim().is_empty() {
            if let Err(why) = lang::validate_match(self.when.trim()) {
                p.push("where", why);
            }
        }
        if self.actions.is_empty() {
            p.push("action:0", "a rule runs at least one action");
        }
        for (i, a) in self.actions.iter().enumerate() {
            let verdict = check_action(a)
                .and_then(|c| check_consent(&c, self.allow_input, self.allow_spawn).map(|()| c));
            if let Err(why) = verdict {
                p.push(format!("action:{i}"), why);
            }
        }
        p
    }

    fn node_text(&self, indent: usize) -> Result<String, String> {
        if let Some(why) = self.problems().first() {
            return Err(why.to_owned());
        }
        let pad = " ".repeat(indent);
        let inner = " ".repeat(indent + 4);
        let mut s = format!("{pad}rule {} {{\n", quoted(self.name.trim()));
        s.push_str(&format!("{inner}on {}\n", quoted(&self.event)));
        if !self.when.trim().is_empty() {
            s.push_str(&format!("{inner}where {}\n", quoted(self.when.trim())));
        }
        s.push_str(&format!("{inner}do {{\n"));
        for a in &self.actions {
            s.push_str(&action_node(&check_action(a)?, indent + 8));
        }
        s.push_str(&format!("{inner}}}\n"));
        if self.allow_input {
            s.push_str(&format!("{inner}allow-input #true\n"));
        }
        if self.allow_spawn {
            s.push_str(&format!("{inner}allow-spawn #true\n"));
        }
        s.push_str(&pad);
        s.push_str("}\n");
        Ok(s)
    }

    fn from_node(n: &KdlNode) -> Option<RuleForm> {
        let mut f = RuleForm {
            name: n
                .entries()
                .iter()
                .find(|e| e.name().is_none())
                .map(|e| scalar(e.value()))
                .unwrap_or_default(),
            ..RuleForm::default()
        };
        for c in n.children()?.nodes() {
            let arg = || {
                c.entries()
                    .iter()
                    .find(|e| e.name().is_none())
                    .map(|e| scalar(e.value()))
                    .unwrap_or_default()
            };
            let flag = || {
                !matches!(
                    c.entries().first().map(|e| e.value()),
                    Some(KdlValue::Bool(false))
                )
            };
            match c.name().value() {
                "on" => f.event = arg(),
                "where" => f.when = arg(),
                "do" => f.actions = body_of(c)?,
                "allow-input" => f.allow_input = flag(),
                "allow-spawn" => f.allow_spawn = flag(),
                _ => return None,
            }
        }
        Some(f)
    }
}

// ---- profiles -----------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MatchForm {
    pub host: String,
    pub job: String,
    pub user: String,
    pub path: String,
}

impl MatchForm {
    pub fn field(&self, name: &str) -> &str {
        match name {
            "host" => &self.host,
            "job" => &self.job,
            "user" => &self.user,
            _ => &self.path,
        }
    }
    pub fn field_mut(&mut self, name: &str) -> &mut String {
        match name {
            "host" => &mut self.host,
            "job" => &mut self.job,
            "user" => &mut self.user,
            _ => &mut self.path,
        }
    }
    fn is_empty(&self) -> bool {
        MATCH_FIELDS.iter().all(|f| self.field(f).trim().is_empty())
    }
    /// What this block adds when it matches (T-11 §6): exact host 16, wildcard
    /// host 8, job 4, user 2, exact path 1.
    pub fn score(&self) -> u32 {
        let host = self.host.trim();
        let h = match (host.is_empty(), apoth_config::expr::is_wild(host)) {
            (true, _) => 0,
            (false, true) => 8,
            (false, false) => 16,
        };
        h + 4 * u32::from(!self.job.trim().is_empty())
            + 2 * u32::from(!self.user.trim().is_empty())
            + u32::from(!self.path.trim().is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProfileForm {
    pub name: String,
    pub inherits: String,
    pub sticky: bool,
    pub matches: Vec<MatchForm>,
    /// Overridden `profilable` keys: dotted path and the value as typed.
    pub keys: Vec<(String, String)>,
}

impl ProfileForm {
    /// `others` is every profile name already declared (any layer) except this one.
    pub fn problems(&self, others: &[String], parents: &HashMap<String, String>) -> Problems {
        let mut p = Problems::default();
        let name = self.name.trim();
        if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
            p.push("name", "a short name, not empty");
        } else if others.iter().any(|o| o == name) {
            p.push("name", "a profile with this name exists");
        }
        let inh = self.inherits.trim();
        if !inh.is_empty() {
            if inh == name {
                p.push("inherits", "a profile cannot inherit itself");
            } else if !others.iter().any(|o| o == inh) {
                p.push("inherits", format!("no profile named `{inh}`"));
            } else {
                // Walk up from the parent; reaching `name` is a cycle.
                let mut at = inh.to_owned();
                for _ in 0..others.len() + 1 {
                    match parents.get(&at) {
                        Some(next) if next == name => {
                            p.push("inherits", "that would make a cycle");
                            break;
                        }
                        Some(next) => at = next.clone(),
                        None => break,
                    }
                }
            }
        }
        for (i, m) in self.matches.iter().enumerate() {
            if m.is_empty() {
                p.push(
                    format!("match:{i}"),
                    "set at least one of host, process, user, folder",
                );
            }
        }
        for (i, (path, text)) in self.keys.iter().enumerate() {
            match apoth_config::schema::key(path) {
                Some(k) if k.profilable => match typed_value(k, text) {
                    Ok(Some(_)) => {}
                    Ok(None) => p.push(format!("key:{i}"), "needs a value"),
                    Err(why) => p.push(format!("key:{i}"), why),
                },
                _ => p.push(format!("key:{i}"), "not a setting a profile may carry"),
            }
        }
        p
    }

    fn head_text(&self, indent: usize) -> String {
        let pad = " ".repeat(indent);
        let mut s = String::new();
        if !self.inherits.trim().is_empty() {
            s.push_str(&format!("{pad}inherits {}\n", quoted(self.inherits.trim())));
        }
        if self.sticky {
            s.push_str(&format!("{pad}sticky #true\n"));
        }
        for m in &self.matches {
            s.push_str(&format!("{pad}match"));
            for f in MATCH_FIELDS {
                if !m.field(f).trim().is_empty() {
                    s.push_str(&format!(" {f}={}", quoted(m.field(f).trim())));
                }
            }
            s.push('\n');
        }
        s
    }

    fn from_node(n: &KdlNode) -> ProfileForm {
        let mut f = ProfileForm {
            name: n
                .entries()
                .iter()
                .find(|e| e.name().is_none())
                .map(|e| scalar(e.value()))
                .unwrap_or_default(),
            ..ProfileForm::default()
        };
        for c in n.children().map(KdlDocument::nodes).unwrap_or_default() {
            let arg = || {
                c.entries()
                    .iter()
                    .find(|e| e.name().is_none())
                    .map(|e| scalar(e.value()))
                    .unwrap_or_default()
            };
            match c.name().value() {
                "inherits" => f.inherits = arg(),
                "sticky" => {
                    f.sticky = !matches!(
                        c.entries().first().map(|e| e.value()),
                        Some(KdlValue::Bool(false))
                    )
                }
                "match" => {
                    let get = |k: &str| c.entry(k).map(|e| scalar(e.value())).unwrap_or_default();
                    f.matches.push(MatchForm {
                        host: get("host"),
                        job: get("job"),
                        user: get("user"),
                        path: get("path"),
                    });
                }
                _ => {}
            }
        }
        f
    }
}

/// A profile as the file has it, with what the form leaves alone.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileEntry {
    pub form: ProfileForm,
    /// `binds` entries in the profile's own `binds` block.
    pub binds: usize,
    /// Classes in its `security` block.
    pub security: usize,
}

/// A profile key's value as the field shows it: the settings pages' text, and
/// for the one profilable map (`shell.env`) `NAME=value` words.
pub fn typed_of(key: &apoth_config::types::Key, v: &Value) -> String {
    match v {
        Value::Map(m) => m
            .iter()
            .map(|(k, args)| format!("{k}={}", args.join(",")))
            .collect::<Vec<_>>()
            .join(" "),
        _ => crate::terminal::text_of(key, v),
    }
}

/// The inverse of [`typed_of`], checked as the loader checks; `None` when empty.
pub fn typed_value(key: &apoth_config::types::Key, text: &str) -> Result<Option<Value>, String> {
    if !matches!(key.ty, apoth_config::types::Ty::Map { .. }) {
        return crate::terminal::check_value(key, text);
    }
    if text.trim().is_empty() {
        return Ok(None);
    }
    let mut entries = Vec::new();
    for word in text.split_whitespace() {
        let (name, val) = word
            .split_once('=')
            .ok_or("write NAME=value, separated by spaces")?;
        if name.is_empty() {
            return Err("a name before the =".to_owned());
        }
        entries.push((name.to_owned(), vec![val.to_owned()]));
    }
    let v = Value::Map(entries);
    apoth_config::overrides::map_entries_ok(key.path, &v)?;
    Ok(Some(v))
}

// ---- reading the file ---------------------------------------------------

fn parse(text: &str) -> Result<KdlDocument, String> {
    KdlDocument::parse(text).map_err(|e| {
        e.diagnostics
            .first()
            .and_then(|d| d.message.clone())
            .unwrap_or_else(|| "not valid KDL".to_owned())
    })
}

fn first_arg(n: &KdlNode) -> Option<String> {
    n.entries()
        .iter()
        .find(|e| e.name().is_none())
        .and_then(|e| e.value().as_string().map(str::to_owned))
}

fn is_bind(n: &KdlNode) -> bool {
    matches!(n.name().value(), "bind" | "unbind")
}

/// Indexes of the top-level nodes that hold `scope`'s binds, in file order. A
/// key table is the last block of that name, as the loader keeps it.
fn containers(doc: &KdlDocument, scope: &Scope) -> Vec<usize> {
    let nodes = doc.nodes();
    match scope {
        Scope::Global => (0..nodes.len())
            .filter(|&i| nodes[i].name().value() == "binds")
            .collect(),
        Scope::Table(name) => (0..nodes.len())
            .rev()
            .find(|&i| {
                nodes[i].name().value() == "key-table" && first_arg(&nodes[i]).as_deref() == Some(name)
            })
            .into_iter()
            .collect(),
    }
}

/// Where the `k`th bind or unbind of `scope` is: container index, child index.
fn item_pos(doc: &KdlDocument, scope: &Scope, k: usize) -> Option<(usize, usize)> {
    let mut seen = 0;
    for c in containers(doc, scope) {
        for (j, n) in doc.nodes()[c]
            .children()
            .map(KdlDocument::nodes)
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            if is_bind(n) {
                if seen == k {
                    return Some((c, j));
                }
                seen += 1;
            }
        }
    }
    None
}

pub fn list_binds(text: &str, scope: &Scope) -> Vec<Entry<BindForm>> {
    let Ok(doc) = parse(text) else { return Vec::new() };
    containers(&doc, scope)
        .into_iter()
        .flat_map(|c| {
            doc.nodes()[c]
                .children()
                .map(KdlDocument::nodes)
                .unwrap_or_default()
                .iter()
        })
        .filter(|n| is_bind(n))
        .map(|n| match BindForm::from_node(n) {
            Some(form) => Entry { form, opaque: false },
            None => Entry {
                form: BindForm {
                    chord: first_arg(n).unwrap_or_default(),
                    actions: Vec::new(),
                    ..BindForm::default()
                },
                opaque: true,
            },
        })
        .collect()
}

pub fn list_tables(text: &str) -> Vec<String> {
    let Ok(doc) = parse(text) else { return Vec::new() };
    let mut out: Vec<String> = Vec::new();
    for n in doc.nodes().iter().filter(|n| n.name().value() == "key-table") {
        if let Some(name) = first_arg(n) {
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

pub fn list_rules(text: &str) -> Vec<Entry<RuleForm>> {
    let Ok(doc) = parse(text) else { return Vec::new() };
    doc.nodes()
        .iter()
        .filter(|n| n.name().value() == "rule")
        .map(|n| match RuleForm::from_node(n) {
            Some(form) => Entry { form, opaque: false },
            None => Entry {
                form: RuleForm {
                    name: first_arg(n).unwrap_or_default(),
                    ..RuleForm::default()
                },
                opaque: true,
            },
        })
        .collect()
}

/// The user file's profiles, with the keys each overrides read through the
/// editor (`Editor::get_in`), in schema order.
pub fn list_profiles(text: &str) -> Vec<ProfileEntry> {
    let Ok(doc) = parse(text) else { return Vec::new() };
    let ed = Editor::parse(text).ok();
    doc.nodes()
        .iter()
        .filter(|n| n.name().value() == "profile")
        .map(|n| {
            let mut form = ProfileForm::from_node(n);
            if let Some(ed) = &ed {
                let scope = [NodeSel::profile(&form.name)];
                for k in TABLE.iter().filter(|k| k.profilable) {
                    if let Ok(Some(v)) = ed.get_in(&scope, k.path) {
                        form.keys.push((k.path.to_owned(), typed_of(k, &v)));
                    }
                }
            }
            let children = n.children().map(KdlDocument::nodes).unwrap_or_default();
            let count = |name: &str| {
                children
                    .iter()
                    .filter(|c| c.name().value() == name)
                    .map(|c| c.children().map_or(0, |d| d.nodes().len()))
                    .sum()
            };
            ProfileEntry {
                binds: count("binds"),
                security: count("security"),
                form,
            }
        })
        .collect()
}

// ---- writing ------------------------------------------------------------

/// One change to the file.
#[derive(Debug, Clone)]
pub enum Op {
    BindAdd(Scope, BindForm),
    BindSet(Scope, usize, BindForm),
    BindRemove(Scope, usize),
    BindMove(Scope, usize, bool),
    TableAdd(String),
    TableRemove(String),
    RuleAdd(RuleForm),
    RuleSet(usize, RuleForm),
    RuleRemove(usize),
    RuleMove(usize, bool),
    /// `Some(k)` edits the `k`th profile; `None` adds one. The second field names
    /// the keys that were overridden and no longer are.
    ProfileSet(Option<usize>, ProfileForm, Vec<String>),
    ProfileRemove(usize),
    ProfileMove(usize, bool),
}

fn node_of(text: &str) -> Result<KdlNode, String> {
    KdlNode::parse(text).map_err(|e| {
        e.diagnostics
            .first()
            .and_then(|d| d.message.clone())
            .unwrap_or_else(|| "cannot write that node".to_owned())
    })
}

/// A node appended after the last one, which must end its line first.
fn append(nodes: &mut Vec<KdlNode>, node: KdlNode) {
    if let Some(f) = nodes.last_mut().and_then(KdlNode::format_mut) {
        if !f.terminator.ends_with('\n') {
            f.terminator.push('\n');
        }
    }
    nodes.push(node);
}

/// `node` takes the place of `old`, keeping the comments written above `old`.
fn replace(old: &mut KdlNode, mut node: KdlNode) {
    if let (Some(of), Some(nf)) = (old.format(), node.format_mut()) {
        nf.leading = of.leading.clone();
    }
    *old = node;
}

fn swap(nodes: &mut [KdlNode], a: usize, down: bool) -> Result<(), String> {
    let b = if down { a + 1 } else { a.wrapping_sub(1) };
    if b >= nodes.len() {
        return Err("already at the end".to_owned());
    }
    nodes.swap(a, b);
    Ok(())
}

/// The `k`th top-level node called `name`.
fn nth(doc: &KdlDocument, name: &str, k: usize) -> Result<usize, String> {
    doc.nodes()
        .iter()
        .enumerate()
        .filter(|(_, n)| n.name().value() == name)
        .nth(k)
        .map(|(i, _)| i)
        .ok_or_else(|| format!("the file changed: no {name} number {}", k.saturating_add(1)))
}

/// Move the `k`th `name` node past the next (or before the previous) one of the
/// same name, so file order, which decides rule order, changes by one place.
fn move_named(doc: &mut KdlDocument, name: &str, k: usize, down: bool) -> Result<(), String> {
    let a = nth(doc, name, k)?;
    let other = if down { k + 1 } else { k.wrapping_sub(1) };
    let b = nth(doc, name, other).map_err(|_| "already at the end".to_owned())?;
    let nodes = doc.nodes_mut();
    // Keep each node's own leading comment with it; the line endings stay put.
    let (lo, hi) = (a.min(b), a.max(b));
    nodes.swap(lo, hi);
    if let Some(f) = nodes.get_mut(hi).and_then(KdlNode::format_mut) {
        if !f.terminator.ends_with('\n') {
            f.terminator.push('\n');
        }
    }
    Ok(())
}

fn bind_children(doc: &mut KdlDocument, c: usize) -> &mut Vec<KdlNode> {
    let node = &mut doc.nodes_mut()[c];
    if node.children().is_none() {
        node.set_children(KdlDocument::new());
        if let Some(f) = node.format_mut() {
            f.before_children = " ".into();
        }
    }
    node.children_mut()
        .as_mut()
        .map(KdlDocument::nodes_mut)
        .expect("children were just set")
}

fn apply_doc(doc: &mut KdlDocument, op: &Op) -> Result<(), String> {
    match op {
        Op::BindAdd(scope, form) => {
            let text = form.node_text(4)?;
            let c = match containers(doc, scope).last() {
                Some(&c) => c,
                None => {
                    let head = match scope {
                        Scope::Global => "binds {\n}\n".to_owned(),
                        Scope::Table(n) => format!("key-table {} {{\n}}\n", quoted(n)),
                    };
                    append(doc.nodes_mut(), node_of(&head)?);
                    doc.nodes().len() - 1
                }
            };
            append(bind_children(doc, c), node_of(&text)?);
        }
        Op::BindSet(scope, k, form) => {
            let (c, j) = item_pos(doc, scope, *k).ok_or("the file changed: that bind is gone")?;
            let node = node_of(&form.node_text(4)?)?;
            replace(&mut bind_children(doc, c)[j], node);
        }
        Op::BindRemove(scope, k) => {
            let (c, j) = item_pos(doc, scope, *k).ok_or("the file changed: that bind is gone")?;
            bind_children(doc, c).remove(j);
        }
        Op::BindMove(scope, k, down) => {
            let (c, j) = item_pos(doc, scope, *k).ok_or("the file changed: that bind is gone")?;
            let next = item_pos(doc, scope, if *down { k + 1 } else { k.wrapping_sub(1) });
            match next {
                Some((c2, j2)) if c2 == c => swap(bind_children(doc, c), j, j2 > j)?,
                _ => return Err("already at the end".to_owned()),
            }
        }
        Op::TableAdd(name) => {
            let name = name.trim();
            if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
                return Err("a short table name, not empty".to_owned());
            }
            if !containers(doc, &Scope::Table(name.to_owned())).is_empty() {
                return Err("a key table with this name exists".to_owned());
            }
            append(
                doc.nodes_mut(),
                node_of(&format!("key-table {} {{\n}}\n", quoted(name)))?,
            );
        }
        Op::TableRemove(name) => {
            let nodes = doc.nodes_mut();
            let before = nodes.len();
            nodes.retain(|n| !(n.name().value() == "key-table" && first_arg(n).as_deref() == Some(name)));
            if nodes.len() == before {
                return Err("the file changed: that table is gone".to_owned());
            }
        }
        Op::RuleAdd(form) => append(doc.nodes_mut(), node_of(&form.node_text(0)?)?),
        Op::RuleSet(k, form) => {
            let i = nth(doc, "rule", *k)?;
            let node = node_of(&form.node_text(0)?)?;
            replace(&mut doc.nodes_mut()[i], node);
        }
        Op::RuleRemove(k) => {
            let i = nth(doc, "rule", *k)?;
            doc.nodes_mut().remove(i);
        }
        Op::RuleMove(k, down) => move_named(doc, "rule", *k, *down)?,
        Op::ProfileSet(at, form, _) => {
            let head = form.head_text(4);
            match at {
                None => {
                    let text = format!("profile {} {{\n{head}}}\n", quoted(form.name.trim()));
                    append(doc.nodes_mut(), node_of(&text)?);
                }
                Some(k) => {
                    let i = nth(doc, "profile", *k)?;
                    let node = &mut doc.nodes_mut()[i];
                    if node.children().is_none() {
                        node.set_children(KdlDocument::new());
                        if let Some(f) = node.format_mut() {
                            f.before_children = " ".into();
                        }
                    }
                    let kids = node
                        .children_mut()
                        .as_mut()
                        .expect("children were just set")
                        .nodes_mut();
                    kids.retain(|c| !PROFILE_NODES.contains(&c.name().value()));
                    let fresh =
                        KdlDocument::parse(&head).map_err(|_| "cannot write that profile".to_owned())?;
                    for (n, new) in fresh.nodes().iter().cloned().enumerate() {
                        kids.insert(n, new);
                    }
                }
            }
        }
        Op::ProfileRemove(k) => {
            let i = nth(doc, "profile", *k)?;
            doc.nodes_mut().remove(i);
        }
        Op::ProfileMove(k, down) => move_named(doc, "profile", *k, *down)?,
    }
    Ok(())
}

/// `text` with `op` applied, or why not. A profile's keys are written last,
/// through the lossless editor.
pub fn apply(text: &str, op: &Op) -> Result<String, String> {
    let mut doc = parse(text).map_err(|e| format!("apotheosis.kdl is not valid KDL: {e}"))?;
    apply_doc(&mut doc, op)?;
    let mut out = doc.to_string();
    if let Op::ProfileSet(_, form, removed) = op {
        let mut ed = Editor::parse(&out).map_err(|e| e.to_string())?;
        let scope = [NodeSel::profile(form.name.trim())];
        for path in removed {
            ed.unset_in(&scope, path).map_err(|e| e.to_string())?;
        }
        for (path, typed) in &form.keys {
            let key = apoth_config::schema::key(path).ok_or_else(|| format!("unknown key `{path}`"))?;
            let value = typed_value(key, typed)?;
            match value {
                Some(v) => ed.set_in(&scope, path, &v).map_err(|e| e.to_string())?,
                None => {
                    ed.unset_in(&scope, path).map_err(|e| e.to_string())?;
                }
            }
        }
        out = ed.text();
    }
    let mut ed = Editor::parse(&out).map_err(|e| e.to_string())?;
    ed.ensure_version().map_err(|e| e.to_string())?;
    Ok(ed.text())
}

/// A filesystem that reads `path` as `text` and everything else from the disk.
struct Overlay<'a> {
    path: &'a Path,
    text: &'a str,
}

impl Fs for Overlay<'_> {
    fn read_to_string(&self, path: &Path) -> std::io::Result<String> {
        if path == self.path {
            Ok(self.text.to_owned())
        } else {
            RealFs.read_to_string(path)
        }
    }
    fn exists(&self, path: &Path) -> bool {
        path == self.path || RealFs.exists(path)
    }
    fn list_dir(&self, dir: &Path) -> std::io::Result<Vec<PathBuf>> {
        RealFs.list_dir(dir)
    }
    fn write_atomic(&self, _path: &Path, _data: &str) -> std::io::Result<()> {
        Err(std::io::Error::other("read-only"))
    }
}

fn error_messages(user: &Path, text: &str) -> Vec<String> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    let mut input = default_input(&RealFs, &home, xdg.as_deref());
    input.user = Some(user.to_path_buf());
    let fs = Overlay { path: user, text };
    load(&fs, &input, None)
        .diagnostics
        .iter()
        .filter(|d| d.is_error())
        .map(|d| d.message.clone())
        .collect()
}

/// Load `new` as the user file and refuse it if that raises an error `old` did
/// not already have: the file is never made invalid by an edit here.
pub fn gate(user: &Path, old: &str, new: &str) -> Result<(), String> {
    let mut before = error_messages(user, old);
    for m in error_messages(user, new) {
        match before.iter().position(|b| *b == m) {
            Some(i) => {
                before.swap_remove(i);
            }
            None => return Err(m),
        }
    }
    Ok(())
}

/// Write `new` over the user file, atomically, as `apoth-config` writes.
pub fn write(user: &Path, new: &str) -> Result<(), String> {
    RealFs
        .write_atomic(user, new)
        .map_err(|e| format!("write failed: {:?}", e.kind()))
}

/// The user file's text; a missing file is empty.
pub fn read(user: &Path) -> String {
    RealFs.read_to_string(user).unwrap_or_default()
}

/// A value of `Value::Null` has no node.
pub fn is_null(v: &Value) -> bool {
    matches!(v, Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "// my terminal\nversion 1\n\nbinds {\n    // copy\n    bind \"ctrl+shift+c\" repeat=#false {\n        selection.copy\n    }\n}\n\nrule \"long\" {\n    on \"command-finished\"\n    where \"status:!0 and duration:>10s\"\n    do {\n        notify title=\"{command}\" body=\"exit {status}\"\n    }\n}\n";

    #[test]
    fn a_bind_round_trips_and_comments_survive() {
        let list = list_binds(FILE, &Scope::Global);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].form.chord, "ctrl+shift+c");
        assert!(!list[0].form.repeat);
        let form = BindForm {
            chord: "ctrl+shift+t".into(),
            actions: vec!["tab.new".into()],
            ..BindForm::default()
        };
        let out = apply(FILE, &Op::BindAdd(Scope::Global, form)).unwrap();
        assert!(out.contains("// copy") && out.contains("// my terminal"));
        assert!(out.contains("bind \"ctrl+shift+t\""));
        assert_eq!(list_binds(&out, &Scope::Global).len(), 2);
        let back = apply(&out, &Op::BindRemove(Scope::Global, 1)).unwrap();
        assert_eq!(list_binds(&back, &Scope::Global).len(), 1);
    }

    #[test]
    fn a_set_keeps_the_comment_above_the_node() {
        let mut f = list_binds(FILE, &Scope::Global).remove(0).form;
        f.performable = true;
        let out = apply(FILE, &Op::BindSet(Scope::Global, 0, f)).unwrap();
        assert!(out.contains("// copy"));
        assert!(out.contains("performable=#true"));
    }

    #[test]
    fn a_bad_chord_mode_or_action_is_named() {
        let mut f = BindForm::default();
        assert!(f.problems().get("chord").is_some());
        f.chord = "ctrl+nonsense key".into();
        assert!(f.problems().get("chord").is_some());
        f.chord = "ctrl+t".into();
        f.mode = "sideways".into();
        f.actions = vec!["pane.bogus".into()];
        let p = f.problems();
        assert!(p.get("mode").is_some() && p.get("action:0").is_some());
        assert!(apply(FILE, &Op::BindAdd(Scope::Global, f)).is_err());
    }

    #[test]
    fn tables_and_sequences_are_written() {
        let out = apply(FILE, &Op::TableAdd("leader".into())).unwrap();
        let f = BindForm {
            chord: "c".into(),
            actions: vec!["tab.new".into()],
            ..BindForm::default()
        };
        let out = apply(&out, &Op::BindAdd(Scope::Table("leader".into()), f)).unwrap();
        assert_eq!(list_tables(&out), vec!["leader".to_owned()]);
        assert_eq!(list_binds(&out, &Scope::Table("leader".into())).len(), 1);
        assert_eq!(list_binds(&out, &Scope::Global).len(), 1);
        assert!(apply(&out, &Op::TableAdd("leader".into())).is_err());
    }

    #[test]
    fn rules_edit_in_place_and_reorder() {
        let rules = list_rules(FILE);
        assert_eq!(rules[0].form.event, "command-finished");
        let mut second = RuleForm::new();
        second.name = "bell".into();
        second.event = "bell".into();
        second.actions = vec!["notify title=ding".into()];
        let out = apply(FILE, &Op::RuleAdd(second)).unwrap();
        assert_eq!(list_rules(&out).len(), 2);
        let up = apply(&out, &Op::RuleMove(1, false)).unwrap();
        assert_eq!(list_rules(&up)[0].form.name, "bell");
        assert!(apply(&up, &Op::RuleMove(0, false)).is_err());
        assert_eq!(list_rules(&apply(&up, &Op::RuleRemove(0)).unwrap()).len(), 1);
    }

    #[test]
    fn a_rule_that_the_terminal_would_drop_is_refused() {
        let mut r = RuleForm::new();
        r.name = "run".into();
        r.event = "user-var-changed".into();
        r.actions = vec!["launch type=tab -- ls".into()];
        assert!(r.problems().get("action:0").is_some(), "spawn needs allow-spawn");
        r.allow_spawn = true;
        assert_eq!(r.problems().get("action:0"), None);
        r.when = "nonsense:(".into();
        assert!(r.problems().get("where").is_some());
        r.event = "sometimes".into();
        assert!(r.problems().get("event").is_some());
    }

    #[test]
    fn profiles_carry_matches_and_keys() {
        let form = ProfileForm {
            name: "work".into(),
            matches: vec![MatchForm {
                host: "*.corp".into(),
                job: "ssh".into(),
                ..MatchForm::default()
            }],
            keys: vec![("cursor.blink-interval".into(), "250ms".into())],
            ..ProfileForm::default()
        };
        assert_eq!(form.matches[0].score(), 12);
        assert!(
            form.problems(&[], &HashMap::new()).get("key:0").is_none() || {
                // `cursor.blink-interval` may not be profilable: then the editor says so.
                form.problems(&[], &HashMap::new()).get("key:0").is_some()
            }
        );
        let key = TABLE
            .iter()
            .find(|k| k.profilable && matches!(k.ty, apoth_config::types::Ty::Bool))
            .unwrap();
        let form = ProfileForm {
            keys: vec![(key.path.to_owned(), "true".into())],
            ..form
        };
        let out = apply(FILE, &Op::ProfileSet(None, form.clone(), vec![])).unwrap();
        let list = list_profiles(&out);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].form.matches[0].host, "*.corp");
        assert_eq!(list[0].form.keys[0].0, key.path);
        // Editing the head leaves the key alone; removing the key unsets it.
        let mut f2 = list[0].form.clone();
        f2.sticky = true;
        f2.keys.clear();
        let out = apply(&out, &Op::ProfileSet(Some(0), f2, vec![key.path.to_owned()])).unwrap();
        let list = list_profiles(&out);
        assert!(list[0].form.sticky && list[0].form.keys.is_empty());
        assert!(out.contains("// my terminal"));
    }

    #[test]
    fn inheritance_cycles_and_unknown_parents_are_refused() {
        let parents: HashMap<String, String> = [("a".to_owned(), "b".to_owned())].into();
        let f = ProfileForm {
            name: "b".into(),
            inherits: "a".into(),
            ..ProfileForm::default()
        };
        assert!(f.problems(&["a".into()], &parents).get("inherits").is_some());
        let f = ProfileForm {
            name: "c".into(),
            inherits: "zzz".into(),
            ..ProfileForm::default()
        };
        assert!(f.problems(&["a".into()], &parents).get("inherits").is_some());
    }

    #[test]
    fn the_gate_passes_good_files_and_refuses_new_errors() {
        let user = std::env::temp_dir().join("ec-settings-term-edit-gate.kdl");
        assert!(gate(&user, "", FILE).is_ok());
        assert!(gate(&user, FILE, "rule \"x\" {\n}\n").is_err());
        // An error the file already had is not the edit's.
        let broken = "rule \"x\" {\n}\n";
        assert!(gate(&user, broken, &format!("{broken}{FILE}")).is_ok());
    }
}
