// SPDX-License-Identifier: AGPL-3.0-only
//! KDL node <-> [`Value`], per [`Ty`] (T-10 §1, §3).
//!
//! A key at path `a.b.c` is the node `c` inside `b` inside `a`. Its value is the node's
//! positional arguments (lists), one argument (scalars), or its child nodes (maps).

use crate::diag::suggest;
use crate::lang;
use crate::types::{Dv, Key, Ty, Value};
use ec_config_schema::{format_bytes, format_duration, parse_bytes, parse_duration};
use kdl::{KdlNode, KdlValue};

/// Why a node's value was refused.
#[derive(Debug, Clone, PartialEq)]
pub struct ValueError {
    pub message: String,
    pub suggestion: Option<String>,
    /// Byte offset of the offending entry when it is narrower than the node.
    pub offset: Option<usize>,
}

impl ValueError {
    pub fn new(message: impl Into<String>) -> Self {
        ValueError {
            message: message.into(),
            suggestion: None,
            offset: None,
        }
    }
}

type Res<T> = Result<T, ValueError>;

fn positional(node: &KdlNode) -> Vec<&KdlValue> {
    node.entries()
        .iter()
        .filter(|e| e.name().is_none())
        .map(|e| e.value())
        .collect()
}

fn no_props(node: &KdlNode) -> Res<()> {
    if let Some(e) = node.entries().iter().find(|e| e.name().is_some()) {
        let name = e.name().map(|n| n.value()).unwrap_or("");
        let mut err = ValueError::new(format!("unexpected property `{name}`"));
        err.offset = Some(e.span().offset());
        return Err(err);
    }
    Ok(())
}

fn one<'a>(pos: &[&'a KdlValue]) -> Res<&'a KdlValue> {
    match pos {
        [v] => Ok(v),
        [] => Err(ValueError::new("expected a value")),
        _ => Err(ValueError::new("expected exactly one value")),
    }
}

fn str_of(v: &KdlValue) -> Res<&str> {
    v.as_string()
        .ok_or_else(|| ValueError::new("expected a string"))
}

fn strings(pos: &[&KdlValue]) -> Res<Vec<String>> {
    pos.iter()
        .map(|v| str_of(v).map(|s| s.to_string()))
        .collect()
}

fn dedupe(v: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(v.len());
    for s in v {
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

fn int_of(v: &KdlValue) -> Res<i128> {
    v.as_integer()
        .ok_or_else(|| ValueError::new("expected a whole number"))
}

/// Parse the node of a table key into its value.
pub fn parse_key_value(key: &Key, node: &KdlNode) -> Res<Value> {
    let nullable = matches!(key.default, Dv::Null | Dv::Computed { .. });
    parse_ty(&key.ty, node, nullable)
}

/// Parse a node against a type.
pub fn parse_ty(ty: &Ty, node: &KdlNode, nullable: bool) -> Res<Value> {
    if !matches!(ty, Ty::ThemeRef) {
        no_props(node)?;
    }
    let pos = positional(node);
    if nullable && pos.len() == 1 && pos[0].is_null() && !matches!(ty, Ty::Map { .. }) {
        return Ok(Value::Null);
    }
    match *ty {
        Ty::Bool => match one(&pos)? {
            KdlValue::Bool(b) => Ok(Value::Bool(*b)),
            _ => Err(ValueError::new("expected #true or #false")),
        },
        Ty::Int { min, max } => {
            let i = int_of(one(&pos)?)?;
            if i < min as i128 || i > max as i128 {
                return Err(ValueError::new(format!("{i} is outside {min}..={max}")));
            }
            Ok(Value::Int(i as i64))
        }
        Ty::Float { min, max } => {
            let v = match one(&pos)? {
                KdlValue::Float(f) => *f,
                KdlValue::Integer(i) => *i as f64,
                _ => return Err(ValueError::new("expected a number")),
            };
            if !v.is_finite() || v < min || v > max {
                return Err(ValueError::new(format!("{v} is outside {min}..={max}")));
            }
            Ok(Value::Float(v))
        }
        Ty::Str | Ty::Font => {
            let s = str_of(one(&pos)?)?;
            Ok(Value::Str(s.to_string()))
        }
        Ty::Path => {
            let s = str_of(one(&pos)?)?;
            if s.is_empty() || s.contains('\0') {
                return Err(ValueError::new("empty or invalid path"));
            }
            Ok(Value::Str(s.to_string()))
        }
        Ty::Enum(set) => {
            let s = str_of(one(&pos)?)?;
            if set.contains(&s) {
                Ok(Value::Str(s.to_string()))
            } else {
                let mut e = ValueError::new(format!("`{s}` is not one of: {}", set.join(", ")));
                e.suggestion = suggest(s, set);
                Err(e)
            }
        }
        Ty::Color => {
            let s = str_of(one(&pos)?)?;
            lang::parse_color(s)
                .map(Value::Color)
                .ok_or_else(|| ValueError::new("expected #rrggbb or #rrggbbaa"))
        }
        Ty::StrList => Ok(Value::List(strings(&pos)?)),
        Ty::StringSet => Ok(Value::List(dedupe(strings(&pos)?))),
        Ty::FontFeatures => {
            let list = strings(&pos)?;
            for f in &list {
                let tag = f.strip_prefix(['+', '-']).unwrap_or(f);
                let (name, val) = match tag.split_once('=') {
                    Some((n, v)) => (n, Some(v)),
                    None => (tag, None),
                };
                let ok = name.len() == 4
                    && name.bytes().all(|b| b.is_ascii_alphanumeric())
                    && val.is_none_or(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()));
                if !ok {
                    return Err(ValueError::new(format!(
                        "`{f}` is not an OpenType feature tag"
                    )));
                }
            }
            Ok(Value::List(list))
        }
        Ty::KeyChord => {
            let s = str_of(one(&pos)?)?;
            crate::chord::parse_chord_seq(s).map_err(ValueError::new)?;
            Ok(Value::Str(s.to_string()))
        }
        Ty::Duration { min_ms, max_ms } => {
            let s = str_of(one(&pos)?)?;
            let ms = parse_duration(s).ok_or_else(|| {
                ValueError::new("expected a duration such as \"150ms\" or \"2s\"")
            })?;
            if ms < min_ms || ms > max_ms {
                return Err(ValueError::new(format!(
                    "{} is outside {}..={}",
                    format_duration(ms),
                    format_duration(min_ms),
                    format_duration(max_ms)
                )));
            }
            Ok(Value::Int(ms as i64))
        }
        Ty::Bytes { min, max } => {
            let v = one(&pos)?;
            let n = match v {
                KdlValue::String(s) => parse_bytes(s),
                KdlValue::Integer(i) => u64::try_from(*i).ok(),
                _ => None,
            }
            .ok_or_else(|| ValueError::new("expected a size such as \"64MiB\""))?;
            if n < min || n > max {
                return Err(ValueError::new(format!(
                    "{} is outside {}..={}",
                    format_bytes(n),
                    format_bytes(min),
                    format_bytes(max)
                )));
            }
            Ok(Value::Int(n as i64))
        }
        Ty::Palette16 => {
            let list = strings(&pos)?;
            if list.len() != 16 {
                return Err(ValueError::new(format!(
                    "a palette needs 16 colours, found {}",
                    list.len()
                )));
            }
            let mut out = Vec::with_capacity(16);
            for c in &list {
                let col = lang::parse_color(c)
                    .ok_or_else(|| ValueError::new(format!("`{c}` is not #rrggbb")))?;
                out.push(lang::format_color(col));
            }
            Ok(Value::List(out))
        }
        Ty::Action => {
            let s = str_of(one(&pos)?)?;
            let id = s.split_whitespace().next().unwrap_or("");
            lang::validate_action_id(id).map_err(ValueError::new)?;
            Ok(Value::Str(s.to_string()))
        }
        Ty::Regex => {
            let s = str_of(one(&pos)?)?;
            lang::validate_regex(s).map_err(ValueError::new)?;
            Ok(Value::Str(s.to_string()))
        }
        Ty::Template => {
            let s = str_of(one(&pos)?)?;
            lang::validate_template(s).map_err(ValueError::new)?;
            Ok(Value::Str(s.to_string()))
        }
        Ty::Match => {
            let s = str_of(one(&pos)?)?;
            lang::validate_match(s).map_err(ValueError::new)?;
            Ok(Value::Str(s.to_string()))
        }
        Ty::Argv => {
            let list = strings(&pos)?;
            lang::validate_argv(&list).map_err(ValueError::new)?;
            Ok(Value::List(list))
        }
        Ty::CodepointRanges => {
            let list = strings(&pos)?;
            for r in &list {
                lang::validate_codepoint_range(r).map_err(ValueError::new)?;
            }
            Ok(Value::List(list))
        }
        Ty::Adjust => {
            let s = str_of(one(&pos)?)?;
            lang::validate_adjust(s).map_err(ValueError::new)?;
            Ok(Value::Str(s.to_string()))
        }
        Ty::ThemeRef => parse_theme_ref(node, &pos),
        Ty::Map { key, value } => parse_map(key, value, node),
    }
}

fn valid_scheme_name(s: &str) -> bool {
    !s.is_empty() && !s.contains('=') && !s.contains(" dark=") && !s.contains('\0')
}

fn parse_theme_ref(node: &KdlNode, pos: &[&KdlValue]) -> Res<Value> {
    let light = node.entry("light").map(|e| e.value());
    let dark = node.entry("dark").map(|e| e.value());
    for e in node.entries() {
        if let Some(n) = e.name()
            && n.value() != "light"
            && n.value() != "dark"
        {
            let mut err = ValueError::new(format!("unexpected property `{}`", n.value()));
            err.offset = Some(e.span().offset());
            return Err(err);
        }
    }
    match (light, dark) {
        (Some(l), Some(d)) if pos.is_empty() => {
            let (l, d) = (str_of(l)?, str_of(d)?);
            if !valid_scheme_name(l) || !valid_scheme_name(d) {
                return Err(ValueError::new("invalid scheme name"));
            }
            Ok(Value::Str(format!("light={l} dark={d}")))
        }
        (None, None) => {
            let s = str_of(one(pos)?)?;
            if !valid_scheme_name(s) {
                return Err(ValueError::new("invalid scheme name"));
            }
            Ok(Value::Str(s.to_string()))
        }
        _ => Err(ValueError::new(
            "theme takes one scheme, or both light=… and dark=…",
        )),
    }
}

fn validate_map_key(kty: &Ty, k: &str) -> Res<()> {
    match kty {
        Ty::CodepointRanges => {
            for r in k.split(',') {
                lang::validate_codepoint_range(r.trim()).map_err(ValueError::new)?;
            }
            Ok(())
        }
        Ty::Path => {
            if k.is_empty() || k.contains('\0') {
                Err(ValueError::new("empty or invalid path"))
            } else {
                Ok(())
            }
        }
        _ => {
            if k.is_empty() {
                Err(ValueError::new("empty key"))
            } else {
                Ok(())
            }
        }
    }
}

fn map_args(vty: &Ty, child: &KdlNode) -> Res<Vec<String>> {
    no_props(child)?;
    let pos = positional(child);
    match vty {
        Ty::StringSet => Ok(dedupe(strings(&pos)?)),
        Ty::StrList => strings(&pos),
        Ty::Int { min, max } => {
            let i = int_of(one(&pos)?)?;
            if i < *min as i128 || i > *max as i128 {
                return Err(ValueError::new(format!("{i} is outside {min}..={max}")));
            }
            Ok(vec![i.to_string()])
        }
        _ => Ok(vec![str_of(one(&pos)?)?.to_string()]),
    }
}

fn parse_map(kty: &Ty, vty: &Ty, node: &KdlNode) -> Res<Value> {
    if !positional(node).is_empty() {
        return Err(ValueError::new("a map takes child nodes, not arguments"));
    }
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    if let Some(children) = node.children() {
        for c in children.nodes() {
            let k = c.name().value().to_string();
            let at = |mut e: ValueError| {
                e.offset.get_or_insert(c.span().offset());
                e
            };
            validate_map_key(kty, &k).map_err(at)?;
            let args = map_args(vty, c).map_err(at)?;
            if out.iter().any(|(ek, _)| *ek == k) {
                return Err(at(ValueError::new(format!("duplicate entry `{k}`"))));
            }
            out.push((k, args));
        }
    }
    Ok(Value::Map(out))
}

// ---------------------------------------------------------------------------------------
// Rendering

/// A string as a KDL value token (bare when plain, quoted otherwise).
pub fn kdl_str(s: &str) -> String {
    KdlValue::String(s.to_string()).to_string()
}

fn scalar_token(ty: &Ty, v: &Value) -> Option<String> {
    Some(match v {
        Value::Null => return None,
        Value::Bool(b) => format!("#{b}"),
        Value::Int(i) => match ty {
            Ty::Duration { .. } => kdl_str(&format_duration((*i).max(0) as u64)),
            Ty::Bytes { .. } => kdl_str(&format_bytes((*i).max(0) as u64)),
            _ => i.to_string(),
        },
        Value::Float(f) => format!("{f:?}"),
        Value::Str(s) => kdl_str(s),
        Value::Color(c) => kdl_str(&lang::format_color(*c)),
        Value::List(_) | Value::Map(_) => return None,
    })
}

/// Render `name <value>` as KDL source (with the trailing newline), indented by `indent`
/// spaces. `Null` renders nothing: an unset key has no node.
pub fn render_node(name: &str, ty: &Ty, v: &Value, indent: usize) -> Option<String> {
    let pad = " ".repeat(indent);
    let name_tok = kdl_str(name);
    match v {
        Value::Null => None,
        Value::List(l) => {
            let mut s = format!("{pad}{name_tok}");
            for a in l {
                s.push(' ');
                s.push_str(&kdl_str(a));
            }
            s.push('\n');
            Some(s)
        }
        Value::Map(m) => {
            let mut s = format!("{pad}{name_tok} {{\n");
            for (k, args) in m {
                s.push_str(&" ".repeat(indent + 4));
                s.push_str(&kdl_str(k));
                for a in args {
                    s.push(' ');
                    // Integer-valued maps keep integers bare, but only real integers.
                    if matches!(
                        ty,
                        Ty::Map {
                            value: Ty::Int { .. },
                            ..
                        }
                    ) && a.parse::<i64>().is_ok()
                    {
                        s.push_str(&a.parse::<i64>().unwrap_or_default().to_string());
                    } else {
                        s.push_str(&kdl_str(a));
                    }
                }
                s.push('\n');
            }
            s.push_str(&pad);
            s.push_str("}\n");
            Some(s)
        }
        Value::Str(t) if matches!(ty, Ty::ThemeRef) => {
            if let Some(rest) = t.strip_prefix("light=")
                && let Some((l, d)) = rest.split_once(" dark=")
            {
                return Some(format!(
                    "{pad}{name_tok} light={} dark={}\n",
                    kdl_str(l),
                    kdl_str(d)
                ));
            }
            Some(format!("{pad}{name_tok} {}\n", kdl_str(t)))
        }
        other => scalar_token(ty, other).map(|tok| format!("{pad}{name_tok} {tok}\n")),
    }
}

/// Render a table key at top level (full dotted path nested into blocks), for synthesising a
/// config from defaults.
pub fn render_key_nested(key: &Key, v: &Value) -> Option<String> {
    let segs: Vec<&str> = key.path.split('.').collect();
    let (leaf, parents) = segs.split_last()?;
    let leaf_text = render_node(leaf, &key.ty, v, 4 * parents.len())?;
    let mut out = String::new();
    for (i, p) in parents.iter().enumerate() {
        out.push_str(&" ".repeat(4 * i));
        out.push_str(&kdl_str(p));
        out.push_str(" {\n");
    }
    out.push_str(&leaf_text);
    for i in (0..parents.len()).rev() {
        out.push_str(&" ".repeat(4 * i));
        out.push_str("}\n");
    }
    Some(out)
}
