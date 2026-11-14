// SPDX-License-Identifier: AGPL-3.0-only
//! Parsing of scalar blocks and of the collection nodes (binds, rules, profiles, …).
//!
//! Tolerance (T-10 §4): one bad bind, rule or profile is dropped with a diagnostic and the
//! rest applies; a bad scalar leaves the lower layer's value in place.

use crate::chord::{self, parse_chord_seq, parse_mouse_trigger};
use crate::diag::{Diagnostic, Severity, Source, suggest};
use crate::lang;
use crate::model::*;
use crate::schema::{self, TABLE};
use crate::types::Value;
use crate::value::parse_key_value;
use kdl::{KdlDocument, KdlNode, KdlValue};

/// A parse context: one file at one layer.
pub(crate) struct Cx<'a> {
    pub src: Source<'a>,
    pub layer: Layer,
    pub in_when: bool,
    pub diags: &'a mut Vec<Diagnostic>,
}

/// A key a layer sets.
#[derive(Debug, Clone)]
pub(crate) struct Setting {
    pub idx: usize,
    pub value: Value,
    pub origin: Origin,
}

/// Everything one file contributes, in file order.
#[derive(Debug, Default)]
pub(crate) struct Out {
    pub settings: Vec<Setting>,
    /// Keys whose value failed validation (they keep the previous good value).
    pub failed: Vec<usize>,
    pub binds: Vec<Spanned<BindDecl>>,
    pub unbinds: Vec<Spanned<Vec<chord::Chord>>>,
    pub mouse_binds: Vec<Spanned<MouseBindDecl>>,
    pub key_tables: Vec<Spanned<KeyTableDecl>>,
    pub macros: Vec<Spanned<MacroDecl>>,
    pub rules: Vec<Spanned<RuleDecl>>,
    pub profiles: Vec<Spanned<ProfileDecl>>,
    pub layouts: Vec<Spanned<LayoutDecl>>,
    pub menus: Vec<Spanned<MenuDecl>>,
    pub domains: Vec<Spanned<DomainDecl>>,
}

impl Cx<'_> {
    pub(crate) fn origin(&self, offset: usize) -> Origin {
        let (line, column) = self.src.lines.line_col(self.src.text, offset);
        Origin {
            layer: self.layer,
            file: self.src.file.map(|p| p.to_path_buf()),
            line,
            column,
            when: self.in_when,
        }
    }

    pub(crate) fn err(&mut self, offset: usize, key: Option<&str>, msg: impl Into<String>) {
        if self.diags.len() >= crate::diag::MAX_DIAGNOSTICS {
            return;
        }
        let d = self.src.error(offset, key, msg);
        self.diags.push(d);
    }

    pub(crate) fn err_sug(
        &mut self,
        offset: usize,
        key: Option<&str>,
        msg: impl Into<String>,
        sug: Option<String>,
    ) {
        if self.diags.len() >= crate::diag::MAX_DIAGNOSTICS {
            return;
        }
        let d = self.src.error_sug(offset, key, msg, sug);
        self.diags.push(d);
    }

    pub(crate) fn warn(&mut self, offset: usize, key: Option<&str>, msg: impl Into<String>) {
        if self.diags.len() >= crate::diag::MAX_DIAGNOSTICS {
            return;
        }
        let d = self.src.diag(Severity::Warning, offset, key, msg, None);
        self.diags.push(d);
    }
}

/// kdl integers are i128; one that does not fit an i64 is an error, not a wrapped value.
fn arg_of(v: &KdlValue) -> Option<Arg> {
    Some(match v {
        KdlValue::String(s) => Arg::Str(s.clone()),
        KdlValue::Integer(i) => Arg::Int(i64::try_from(*i).ok()?),
        KdlValue::Float(f) => Arg::Float(*f),
        KdlValue::Bool(b) => Arg::Bool(*b),
        KdlValue::Null => Arg::Null,
    })
}

/// Deepest nesting of action children the loader follows (the file-level scan in
/// `load.rs` bounds the KDL parser itself).
pub(crate) const MAX_NEST: usize = 32;

fn pos_args(node: &KdlNode) -> Vec<&KdlValue> {
    node.entries()
        .iter()
        .filter(|e| e.name().is_none())
        .map(|e| e.value())
        .collect()
}

fn pos_str<'a>(cx: &mut Cx, node: &'a KdlNode, what: &str) -> Option<&'a str> {
    let args = pos_args(node);
    match args.as_slice() {
        [v] => match v.as_string() {
            Some(s) if !s.is_empty() && s.len() <= 128 && !s.chars().any(|c| c.is_control()) => {
                Some(s)
            }
            _ => {
                cx.err(
                    node.span().offset(),
                    None,
                    format!("{what} must be a short non-empty string"),
                );
                None
            }
        },
        _ => {
            cx.err(
                node.span().offset(),
                None,
                format!("`{}` needs exactly one {what}", node.name().value()),
            );
            None
        }
    }
}

fn bool_prop(cx: &mut Cx, node: &KdlNode, name: &str) -> Option<Option<bool>> {
    match node.entry(name) {
        None => Some(None),
        Some(e) => match e.value() {
            KdlValue::Bool(b) => Some(Some(*b)),
            _ => {
                cx.err(
                    e.span().offset(),
                    None,
                    format!("`{name}` must be #true or #false"),
                );
                None
            }
        },
    }
}

fn check_props(cx: &mut Cx, node: &KdlNode, allowed: &[&str]) -> bool {
    let mut ok = true;
    for e in node.entries() {
        if let Some(n) = e.name()
            && !allowed.contains(&n.value())
        {
            let sug = suggest(n.value(), allowed);
            cx.err_sug(
                e.span().offset(),
                None,
                format!(
                    "unknown option `{}` on `{}`",
                    n.value(),
                    node.name().value()
                ),
                sug,
            );
            ok = false;
        }
    }
    ok
}

// ---- actions -------------------------------------------------------------------------

fn parse_call(cx: &mut Cx, node: &KdlNode, validate_id: bool) -> Option<ActionCall> {
    parse_call_at(cx, node, validate_id, 0)
}

fn parse_call_at(
    cx: &mut Cx,
    node: &KdlNode,
    validate_id: bool,
    depth: usize,
) -> Option<ActionCall> {
    if depth > MAX_NEST {
        cx.err(
            node.span().offset(),
            None,
            format!("actions nest deeper than {MAX_NEST} levels"),
        );
        return None;
    }
    let id = node.name().value().to_string();
    if validate_id && let Err(m) = lang::validate_action_id(&id) {
        cx.err(node.span().offset(), None, m);
        return None;
    }
    let mut call = ActionCall {
        id,
        args: Vec::new(),
        props: Vec::new(),
        children: Vec::new(),
    };
    for e in node.entries() {
        let Some(a) = arg_of(e.value()) else {
            cx.err(e.span().offset(), None, "integer does not fit in 64 bits");
            return None;
        };
        match e.name() {
            Some(n) => call.props.push((n.value().to_string(), a)),
            None => call.args.push(a),
        }
    }
    if let Some(doc) = node.children() {
        for child in doc.nodes() {
            if child.name().value() == "argv" {
                let strs: Vec<String> = pos_args(child)
                    .iter()
                    .filter_map(|v| v.as_string().map(|s| s.to_string()))
                    .collect();
                if strs.len() != pos_args(child).len() {
                    cx.err(child.span().offset(), None, "argv elements must be strings");
                    return None;
                }
                if let Err(m) = lang::validate_argv(&strs) {
                    cx.err(child.span().offset(), None, m);
                    return None;
                }
            }
            call.children
                .push(parse_call_at(cx, child, false, depth + 1)?);
        }
    }
    Some(call)
}

/// Parse the action nodes of a body. Any bad action rejects the whole body (`None`).
fn parse_actions(
    cx: &mut Cx,
    doc: Option<&KdlDocument>,
    owner: &KdlNode,
) -> Option<Vec<ActionCall>> {
    let doc = match doc {
        Some(d) if !d.nodes().is_empty() => d,
        _ => {
            cx.err(
                owner.span().offset(),
                None,
                format!("`{}` has no actions", owner.name().value()),
            );
            return None;
        }
    };
    let mut out = Vec::new();
    let mut ok = true;
    for n in doc.nodes() {
        match parse_call(cx, n, true) {
            Some(c) => out.push(c),
            None => ok = false,
        }
    }
    ok.then_some(out)
}

// ---- binds ---------------------------------------------------------------------------

fn parse_bind(cx: &mut Cx, node: &KdlNode) -> Option<BindDecl> {
    let args = pos_args(node);
    let chord_text = match args.as_slice() {
        [v] => v.as_string(),
        _ => None,
    };
    let Some(chord_text) = chord_text else {
        cx.err(
            node.span().offset(),
            None,
            "`bind` needs one chord string, e.g. bind \"ctrl+shift+t\" { … }",
        );
        return None;
    };
    let chords = match parse_chord_seq(chord_text) {
        Ok(c) => c,
        Err(m) => {
            cx.err(node.span().offset(), None, format!("bad chord: {m}"));
            return None;
        }
    };
    if !check_props(
        cx,
        node,
        &["repeat", "mode", "release", "global", "performable"],
    ) {
        return None;
    }
    let repeat = bool_prop(cx, node, "repeat")?;
    let release = bool_prop(cx, node, "release")?;
    let global = bool_prop(cx, node, "global")?;
    let performable = bool_prop(cx, node, "performable")?;
    let mode = match node.entry("mode") {
        None => None,
        Some(e) => match e.value().as_string() {
            Some(s) => match chord::validate_mode_condition(s) {
                Ok(()) => Some(s.to_string()),
                Err(m) => {
                    cx.err(e.span().offset(), None, m);
                    return None;
                }
            },
            None => {
                cx.err(e.span().offset(), None, "`mode` must be a string");
                return None;
            }
        },
    };
    let actions = parse_actions(cx, node.children(), node)?;
    Some(BindDecl {
        chords,
        actions,
        repeat: repeat.unwrap_or(true),
        mode,
        release: release.unwrap_or(false),
        global: global.unwrap_or(false),
        performable: performable.unwrap_or(false),
    })
}

fn push_bind(out: &mut Vec<Spanned<BindDecl>>, b: Spanned<BindDecl>) {
    // A later bind of the same trigger replaces the earlier one.
    out.retain(|e| {
        !(e.value.chords == b.value.chords
            && e.value.mode == b.value.mode
            && e.value.release == b.value.release)
    });
    out.push(b);
}

/// A children block of `bind`/`unbind` nodes. Names in `skip` are left to the caller.
fn parse_bind_children(
    cx: &mut Cx,
    doc: &KdlDocument,
    binds: &mut Vec<Spanned<BindDecl>>,
    unbinds: &mut Vec<Spanned<Vec<chord::Chord>>>,
    skip: &[&str],
) {
    for n in doc.nodes() {
        match n.name().value() {
            "bind" => {
                if let Some(b) = parse_bind(cx, n) {
                    let origin = cx.origin(n.span().offset());
                    push_bind(binds, Spanned { value: b, origin });
                }
            }
            "unbind" => {
                let args = pos_args(n);
                let text = match args.as_slice() {
                    [v] => v.as_string(),
                    _ => None,
                };
                match text.map(parse_chord_seq) {
                    Some(Ok(chords)) => {
                        let origin = cx.origin(n.span().offset());
                        binds.retain(|e| e.value.chords != chords);
                        unbinds.push(Spanned {
                            value: chords,
                            origin,
                        });
                    }
                    Some(Err(m)) => cx.err(n.span().offset(), None, format!("bad chord: {m}")),
                    None => cx.err(n.span().offset(), None, "`unbind` needs one chord string"),
                }
            }
            other if skip.contains(&other) => {}
            other => {
                let mut names = vec!["bind", "unbind"];
                names.extend_from_slice(skip);
                let sug = suggest(other, &names);
                cx.err_sug(
                    n.span().offset(),
                    None,
                    format!("unknown node `{other}` in a bind block"),
                    sug,
                );
            }
        }
    }
}

/// Set a table key from an already-parsed KDL value of a property (`binds clear-defaults=…`).
fn set_scalar(cx: &mut Cx, path: &str, v: &KdlValue, offset: usize, out: &mut Out) {
    let Some(idx) = schema::index_of(path) else {
        return;
    };
    let mut n = KdlNode::new(path.rsplit('.').next().unwrap_or(path));
    n.entries_mut().push(kdl::KdlEntry::new(v.clone()));
    match parse_key_value(&TABLE[idx], &n) {
        Ok(value) => out.settings.push(Setting {
            idx,
            value,
            origin: cx.origin(offset),
        }),
        Err(e) => {
            out.failed.push(idx);
            cx.err(offset, Some(path), e.message);
        }
    }
}

pub(crate) fn parse_binds(cx: &mut Cx, node: &KdlNode, out: &mut Out) {
    if !check_props(cx, node, &["clear-defaults"]) {
        return;
    }
    if let Some(e) = node.entry("clear-defaults") {
        set_scalar(
            cx,
            "binds.clear-defaults",
            e.value(),
            e.span().offset(),
            out,
        );
    }
    if let Some(doc) = node.children() {
        parse_bind_children(
            cx,
            doc,
            &mut out.binds,
            &mut out.unbinds,
            &["clear-defaults"],
        );
        for n in doc.nodes() {
            if n.name().value() == "clear-defaults" {
                walk_node(cx, n, "binds", out, false);
            }
        }
    }
}

pub(crate) fn parse_mouse_binds(cx: &mut Cx, node: &KdlNode, out: &mut Out) {
    if !check_props(cx, node, &["clear-defaults"]) {
        return;
    }
    if let Some(e) = node.entry("clear-defaults") {
        set_scalar(
            cx,
            "mouse-binds.clear-defaults",
            e.value(),
            e.span().offset(),
            out,
        );
    }
    let Some(doc) = node.children() else { return };
    for n in doc.nodes() {
        match n.name().value() {
            "mouse-bind" => {
                if let Some(d) = parse_mouse_bind(cx, n) {
                    let origin = cx.origin(n.span().offset());
                    out.mouse_binds
                        .retain(|e| !(e.value.trigger == d.trigger && e.value.on == d.on));
                    out.mouse_binds.push(Spanned { value: d, origin });
                }
            }
            "clear-defaults" => walk_node(cx, n, "mouse-binds", out, false),
            other => {
                let sug = suggest(other, &["mouse-bind", "clear-defaults"]);
                cx.err_sug(
                    n.span().offset(),
                    None,
                    format!("unknown node `{other}` in mouse-binds"),
                    sug,
                );
            }
        }
    }
}

fn parse_mouse_bind(cx: &mut Cx, node: &KdlNode) -> Option<MouseBindDecl> {
    let args = pos_args(node);
    let text = match args.as_slice() {
        [v] => v.as_string(),
        _ => None,
    };
    let Some(text) = text else {
        cx.err(
            node.span().offset(),
            None,
            "`mouse-bind` needs one trigger string",
        );
        return None;
    };
    let trigger = match parse_mouse_trigger(text) {
        Ok(t) => t,
        Err(m) => {
            cx.err(
                node.span().offset(),
                None,
                format!("bad mouse trigger: {m}"),
            );
            return None;
        }
    };
    if !check_props(cx, node, &["on"]) {
        return None;
    }
    let on = match node.entry("on") {
        None => "press".to_string(),
        Some(e) => match e.value().as_string() {
            Some(s) if chord::MOUSE_EVENTS.contains(&s) => s.to_string(),
            Some(s) => {
                let sug = suggest(s, chord::MOUSE_EVENTS);
                cx.err_sug(
                    e.span().offset(),
                    None,
                    format!("`{s}` is not a mouse event"),
                    sug,
                );
                return None;
            }
            None => {
                cx.err(e.span().offset(), None, "`on` must be a string");
                return None;
            }
        },
    };
    let actions = parse_actions(cx, node.children(), node)?;
    Some(MouseBindDecl {
        trigger,
        on,
        actions,
    })
}

pub(crate) fn parse_key_table(cx: &mut Cx, node: &KdlNode, out: &mut Out) {
    let Some(name) = pos_str(cx, node, "table name") else {
        return;
    };
    if !check_props(cx, node, &[]) {
        return;
    }
    let mut binds: Vec<Spanned<BindDecl>> = Vec::new();
    let mut unbinds = Vec::new();
    if let Some(doc) = node.children() {
        parse_bind_children(cx, doc, &mut binds, &mut unbinds, &[]);
    }
    let origin = cx.origin(node.span().offset());
    let decl = KeyTableDecl {
        name: name.to_string(),
        binds: binds.into_iter().map(|b| b.value).collect(),
        unbinds: unbinds.into_iter().map(|u| u.value).collect(),
    };
    out.key_tables.retain(|e| e.value.name != decl.name);
    out.key_tables.push(Spanned {
        value: decl,
        origin,
    });
}

pub(crate) fn parse_macro(cx: &mut Cx, node: &KdlNode, out: &mut Out) {
    let Some(name) = pos_str(cx, node, "macro name") else {
        return;
    };
    if !check_props(cx, node, &[]) {
        return;
    }
    let Some(actions) = parse_actions(cx, node.children(), node) else {
        return;
    };
    let origin = cx.origin(node.span().offset());
    out.macros.retain(|e| e.value.name != name);
    out.macros.push(Spanned {
        value: MacroDecl {
            name: name.to_string(),
            actions,
        },
        origin,
    });
}

/// The closed rule event set (T-11 §5). `media-*` is a family.
pub const RULE_EVENTS: &[&str] = &[
    "bell",
    "focus-in",
    "focus-out",
    "resize",
    "title-changed",
    "cwd-changed",
    "host-changed",
    "foreground-changed",
    "prompt-start",
    "command-started",
    "command-finished",
    "output-idle",
    "output-activity",
    "pane-opened",
    "pane-closed",
    "tab-opened",
    "tab-closed",
    "window-opened",
    "window-closed",
    "user-var-changed",
    "progress-changed",
    "notification",
    "config-reloaded",
    "theme-changed",
    "startup",
    "output-match",
];

/// The media event family (T-11 §5): exactly these four.
pub const MEDIA_EVENTS: &[&str] = &[
    "media-started",
    "media-ended",
    "media-error",
    "media-position",
];

/// Is this a rule event name?
pub fn is_rule_event(e: &str) -> bool {
    RULE_EVENTS.contains(&e) || MEDIA_EVENTS.contains(&e)
}

fn single_str_child<'a>(cx: &mut Cx, n: &'a KdlNode) -> Option<&'a str> {
    let args = pos_args(n);
    match args.as_slice() {
        [v] => match v.as_string() {
            Some(s) => Some(s),
            None => {
                cx.err(
                    n.span().offset(),
                    None,
                    format!("`{}` needs a string", n.name().value()),
                );
                None
            }
        },
        _ => {
            cx.err(
                n.span().offset(),
                None,
                format!("`{}` needs exactly one value", n.name().value()),
            );
            None
        }
    }
}

fn single_bool_child(cx: &mut Cx, n: &KdlNode) -> Option<bool> {
    // A property (`allow-spawn enabled=#false`) is not a value: refuse it rather than
    // read the node as a bare `#true`.
    if n.entries().iter().any(|e| e.name().is_some()) {
        cx.err(
            n.span().offset(),
            None,
            format!(
                "`{}` takes #true or #false, not properties",
                n.name().value()
            ),
        );
        return None;
    }
    let args = pos_args(n);
    match args.as_slice() {
        [] => Some(true),
        [KdlValue::Bool(b)] => Some(*b),
        _ => {
            cx.err(
                n.span().offset(),
                None,
                format!("`{}` takes #true or #false", n.name().value()),
            );
            None
        }
    }
}

pub(crate) fn parse_rule(cx: &mut Cx, node: &KdlNode, out: &mut Out) {
    let Some(name) = pos_str(cx, node, "rule name") else {
        return;
    };
    let mut event: Option<String> = None;
    let mut when: Option<String> = None;
    let mut actions: Option<Vec<ActionCall>> = None;
    let mut allow_input = false;
    let mut allow_spawn = false;
    let mut ok = check_props(cx, node, &[]);
    if let Some(doc) = node.children() {
        for n in doc.nodes() {
            match n.name().value() {
                "on" => match single_str_child(cx, n) {
                    Some(e) if is_rule_event(e) => event = Some(e.to_string()),
                    Some(e) => {
                        let sug = suggest(e, RULE_EVENTS);
                        cx.err_sug(
                            n.span().offset(),
                            None,
                            format!("`{e}` is not a rule event"),
                            sug,
                        );
                        ok = false;
                    }
                    None => ok = false,
                },
                "where" => match single_str_child(cx, n) {
                    Some(w) => match lang::validate_match(w) {
                        Ok(()) => when = Some(w.to_string()),
                        Err(m) => {
                            cx.err(n.span().offset(), None, format!("bad `where`: {m}"));
                            ok = false;
                        }
                    },
                    None => ok = false,
                },
                "do" => match parse_actions(cx, n.children(), n) {
                    Some(a) => actions = Some(a),
                    None => ok = false,
                },
                "allow-input" => match single_bool_child(cx, n) {
                    Some(b) => allow_input = b,
                    None => ok = false,
                },
                "allow-spawn" => match single_bool_child(cx, n) {
                    Some(b) => allow_spawn = b,
                    None => ok = false,
                },
                other => {
                    let sug = suggest(other, &["on", "where", "do", "allow-input", "allow-spawn"]);
                    cx.err_sug(
                        n.span().offset(),
                        None,
                        format!("unknown node `{other}` in a rule"),
                        sug,
                    );
                    ok = false;
                }
            }
        }
    }
    if event.is_none() {
        cx.err(
            node.span().offset(),
            None,
            format!("rule `{name}` has no `on`"),
        );
        ok = false;
    }
    if actions.is_none() {
        cx.err(
            node.span().offset(),
            None,
            format!("rule `{name}` has no `do`"),
        );
        ok = false;
    }
    if !ok {
        return;
    }
    let origin = cx.origin(node.span().offset());
    out.rules.retain(|e| e.value.name != name);
    out.rules.push(Spanned {
        value: RuleDecl {
            name: name.to_string(),
            event: event.unwrap_or_default(),
            when,
            actions: actions.unwrap_or_default(),
            allow_input,
            allow_spawn,
        },
        origin,
    });
}

fn parse_security_block(cx: &mut Cx, node: &KdlNode) -> Option<Vec<(String, String)>> {
    let mut out = Vec::new();
    let mut ok = true;
    if let Some(doc) = node.children() {
        for n in doc.nodes() {
            let class = n.name().value();
            if !lang::SEQUENCE_CLASSES.contains(&class) {
                let sug = suggest(class, lang::SEQUENCE_CLASSES);
                cx.err_sug(
                    n.span().offset(),
                    None,
                    format!("unknown policy class `{class}`"),
                    sug,
                );
                ok = false;
                continue;
            }
            let Some(d) = single_str_child(cx, n) else {
                ok = false;
                continue;
            };
            if !lang::DECISIONS.contains(&d) {
                let sug = suggest(d, lang::DECISIONS);
                cx.err_sug(
                    n.span().offset(),
                    None,
                    format!("`{d}` is not allow, ask or deny"),
                    sug,
                );
                ok = false;
                continue;
            }
            out.retain(|(c, _): &(String, String)| c != class);
            out.push((class.to_string(), d.to_string()));
        }
    }
    ok.then_some(out)
}

pub(crate) fn parse_profile(cx: &mut Cx, node: &KdlNode, out: &mut Out) {
    let Some(name) = pos_str(cx, node, "profile name") else {
        return;
    };
    if !check_props(cx, node, &[]) {
        return;
    }
    let mut p = ProfileDecl {
        name: name.to_string(),
        ..Default::default()
    };
    let mut ok = true;
    if let Some(doc) = node.children() {
        for n in doc.nodes() {
            match n.name().value() {
                "inherits" => match single_str_child(cx, n) {
                    Some(s) => p.inherits = Some(s.to_string()),
                    None => ok = false,
                },
                "sticky" => match single_bool_child(cx, n) {
                    Some(b) => p.sticky = b,
                    None => ok = false,
                },
                "match" => {
                    if !check_props(cx, n, &["host", "user", "path", "job"]) {
                        ok = false;
                        continue;
                    }
                    let get = |k: &str| {
                        n.entry(k)
                            .and_then(|e| e.value().as_string())
                            .map(|s| s.to_string())
                    };
                    let m = ProfileMatch {
                        host: get("host"),
                        user: get("user"),
                        path: get("path"),
                        job: get("job"),
                    };
                    if m == ProfileMatch::default() {
                        cx.err(
                            n.span().offset(),
                            None,
                            "`match` needs at least one of host, user, path, job",
                        );
                        ok = false;
                    } else {
                        p.matches.push(m);
                    }
                }
                "security" => match parse_security_block(cx, n) {
                    Some(s) => p.security = s,
                    None => ok = false,
                },
                "binds" => {
                    let mut binds: Vec<Spanned<BindDecl>> = Vec::new();
                    let mut unbinds = Vec::new();
                    if let Some(d) = n.children() {
                        parse_bind_children(cx, d, &mut binds, &mut unbinds, &[]);
                    }
                    p.binds = binds.into_iter().map(|b| b.value).collect();
                    p.unbinds = unbinds.into_iter().map(|u| u.value).collect();
                }
                "env" | "cwd" => {
                    let path = if n.name().value() == "env" {
                        "shell.env"
                    } else {
                        "shell.cwd"
                    };
                    profile_key(cx, n, path, &mut p, out);
                }
                _ => {
                    // Any profilable key by its normal nesting.
                    let mut tmp = Out::default();
                    walk_node(cx, n, "", &mut tmp, true);
                    // A bad profile value is the profile's problem, not the global key's.
                    for s in tmp.settings {
                        p.values.retain(|(k, _)| k != TABLE[s.idx].path);
                        p.values.push((TABLE[s.idx].path.to_string(), s.value));
                    }
                }
            }
        }
    }
    if !ok {
        return;
    }
    let origin = cx.origin(node.span().offset());
    out.profiles.retain(|e| e.value.name != p.name);
    out.profiles.push(Spanned { value: p, origin });
}

fn profile_key(cx: &mut Cx, n: &KdlNode, path: &str, p: &mut ProfileDecl, _out: &mut Out) {
    let Some(idx) = schema::index_of(path) else {
        return;
    };
    match parse_key_value(&TABLE[idx], n) {
        Ok(v) => {
            p.values.retain(|(k, _)| k != path);
            p.values.push((path.to_string(), v));
        }
        Err(e) => cx.err(n.span().offset(), Some(path), e.message),
    }
}

pub(crate) fn parse_domain(cx: &mut Cx, node: &KdlNode, out: &mut Out) {
    let Some(name) = pos_str(cx, node, "domain name") else {
        return;
    };
    if !check_props(cx, node, &[]) {
        return;
    }
    let mut d = DomainDecl {
        name: name.to_string(),
        ..Default::default()
    };
    let mut ok = true;
    if let Some(doc) = node.children() {
        for n in doc.nodes() {
            match n.name().value() {
                "remote" => match single_bool_child(cx, n) {
                    Some(b) => d.remote = Some(b),
                    None => ok = false,
                },
                "security" => match parse_security_block(cx, n) {
                    Some(s) => d.security = s,
                    None => ok = false,
                },
                other => {
                    let sug = suggest(other, &["remote", "security"]);
                    cx.err_sug(
                        n.span().offset(),
                        None,
                        format!("unknown node `{other}` in a domain"),
                        sug,
                    );
                    ok = false;
                }
            }
        }
    }
    if !ok {
        return;
    }
    let origin = cx.origin(node.span().offset());
    out.domains.retain(|e| e.value.name != d.name);
    out.domains.push(Spanned { value: d, origin });
}

fn parse_tree_body(cx: &mut Cx, node: &KdlNode) -> Option<Vec<ActionCall>> {
    let mut body = Vec::new();
    let mut ok = true;
    if let Some(doc) = node.children() {
        for n in doc.nodes() {
            match parse_call(cx, n, false) {
                Some(c) => body.push(c),
                None => ok = false,
            }
        }
    }
    ok.then_some(body)
}

pub(crate) fn parse_layout(cx: &mut Cx, node: &KdlNode, out: &mut Out) {
    let Some(name) = pos_str(cx, node, "layout name") else {
        return;
    };
    if !check_props(cx, node, &[]) {
        return;
    }
    let Some(body) = parse_tree_body(cx, node) else {
        return;
    };
    let origin = cx.origin(node.span().offset());
    out.layouts.retain(|e| e.value.name != name);
    out.layouts.push(Spanned {
        value: LayoutDecl {
            name: name.to_string(),
            body,
        },
        origin,
    });
}

pub(crate) fn parse_menu(cx: &mut Cx, node: &KdlNode, out: &mut Out) {
    let Some(name) = pos_str(cx, node, "menu name") else {
        return;
    };
    if !check_props(cx, node, &[]) {
        return;
    }
    let Some(items) = parse_tree_body(cx, node) else {
        return;
    };
    let origin = cx.origin(node.span().offset());
    out.menus.retain(|e| e.value.name != name);
    out.menus.push(Spanned {
        value: MenuDecl {
            name: name.to_string(),
            items,
        },
        origin,
    });
}

// ---- scalar keys ---------------------------------------------------------------------

/// Walk the children of a block whose dotted prefix is `prefix`.
pub(crate) fn walk_settings(
    cx: &mut Cx,
    doc: &KdlDocument,
    prefix: &str,
    out: &mut Out,
    profile_only: bool,
) {
    for n in doc.nodes() {
        walk_node(cx, n, prefix, out, profile_only);
    }
}

/// One node at `prefix`: a key, a nested block, or an unknown name.
pub(crate) fn walk_node(
    cx: &mut Cx,
    node: &KdlNode,
    prefix: &str,
    out: &mut Out,
    profile_only: bool,
) {
    let name = node.name().value();
    let path = if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}.{name}")
    };
    let offset = node.span().offset();
    if path.matches('.').count() > MAX_NEST {
        cx.err(offset, None, "blocks nest too deeply");
        return;
    }
    let mut resolved = schema::index_of(&path);
    if resolved.is_none()
        && let Some(new_path) = crate::version::renamed(&path)
    {
        resolved = schema::index_of(new_path);
        cx.warn(
            offset,
            Some(&path),
            format!("`{path}` is deprecated, use `{new_path}`"),
        );
    }
    if let Some(idx) = resolved {
        let key = &TABLE[idx];
        if profile_only && !key.profilable {
            cx.err(
                offset,
                Some(&path),
                format!("`{path}` is not profilable and cannot be set in a profile"),
            );
            return;
        }
        match parse_key_value(key, node) {
            Ok(value) => out.settings.push(Setting {
                idx,
                value,
                origin: cx.origin(offset),
            }),
            Err(e) => {
                out.failed.push(idx);
                cx.err_sug(
                    e.offset.unwrap_or(offset),
                    Some(&path),
                    e.message,
                    e.suggestion,
                );
            }
        }
    } else if schema::is_container(&path) {
        if !pos_args(node).is_empty() {
            cx.err(
                offset,
                Some(&path),
                format!("`{path}` is a block of keys, not a value"),
            );
            return;
        }
        if let Some(doc) = node.children() {
            walk_settings(cx, doc, &path, out, profile_only);
        }
    } else {
        let sug = suggest(name, &schema::children_of(prefix));
        cx.err_sug(offset, Some(&path), format!("unknown key `{path}`"), sug);
    }
}
