// SPDX-License-Identifier: AGPL-3.0-only
//! Launch overrides (`-o key=value`, layer 7) and runtime overrides (layer 8), with the
//! override-class gate of T-10 §1.1.

use crate::diag::suggest;
use crate::model::{Config, Layer, Origin};
use crate::schema::{self, TABLE};
use crate::types::{Key, OverrideClass, Scope, Ty, Value};
use crate::value::{kdl_str, parse_ty};
use kdl::KdlNode;
use std::collections::BTreeMap;

/// Parse the text of `-o key=value` (or a socket `config.set` value) against a key's type.
/// The text is KDL argument syntax; scalars that are not valid bare KDL (`150ms`, `#fff`) are
/// retried as one quoted string.
pub fn parse_override_value(key: &Key, text: &str) -> Result<Value, String> {
    if matches!(key.ty, Ty::Map { .. }) {
        return Err("map keys cannot be set with -o; edit the config file".to_string());
    }
    if matches!(key.ty, Ty::Bool) {
        return match text.trim() {
            "true" | "yes" | "on" | "#true" => Ok(Value::Bool(true)),
            "false" | "no" | "off" | "#false" => Ok(Value::Bool(false)),
            _ => Err("expected true or false".to_string()),
        };
    }
    let name = key.path.rsplit('.').next().unwrap_or(key.path);
    let nullable = matches!(
        key.default,
        crate::types::Dv::Null | crate::types::Dv::Computed { .. }
    );
    let attempt = |args: &str| -> Result<Value, String> {
        let src = format!("{} {}", kdl_str(name), args);
        let node = KdlNode::parse(&src).map_err(|_| "not valid KDL".to_string())?;
        parse_ty(&key.ty, &node, nullable).map_err(|e| e.message)
    };
    match attempt(text) {
        Ok(v) => Ok(v),
        Err(first) => {
            if key.ty.is_arg_list() {
                Err(first)
            } else {
                attempt(&kdl_str(text)).map_err(|_| first)
            }
        }
    }
}

/// The value as it is typed after the key (`14.0`, `bar`, `"JetBrains Mono"`): the text
/// form [`parse_override_value`] reads back. `None` for a null or a map.
pub fn value_arg_text(key: &Key, v: &Value) -> Option<String> {
    if matches!(v, Value::Null | Value::Map(_)) {
        return None;
    }
    let s = crate::value::render_node("k", &key.ty, v, 0)?;
    s.trim_end().strip_prefix("k ").map(str::to_string)
}

/// Why a write was refused. The message names the override class (T-10 §2 layer 8).
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    pub message: String,
    pub class: Option<OverrideClass>,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

fn unknown(path: &str) -> Refusal {
    let all: Vec<&str> = TABLE.iter().map(|k| k.path).collect();
    let mut message = format!("unknown key `{path}`");
    if let Some(s) = suggest(path, &all) {
        message.push_str(&format!(" (did you mean `{s}`?)"));
    }
    Refusal {
        message,
        class: None,
    }
}

fn rank(s: Scope) -> u8 {
    match s {
        Scope::Pane => 0,
        Scope::Tab => 1,
        Scope::Window => 2,
        Scope::Global => 3,
    }
}

/// May `config.set` (layer 8) write this key at `scope`? Returns the socket class it needs.
pub fn check_runtime_override(path: &str, scope: Scope) -> Result<&'static str, Refusal> {
    let key = schema::key(path).ok_or_else(|| unknown(path))?;
    let Some(socket) = key.class.runtime_socket_class() else {
        return Err(Refusal {
            message: format!(
                "`{path}` is a `{}` key: runtime overrides are refused (edit the config file{})",
                key.class.name(),
                if key.class == OverrideClass::Security {
                    " or persist it with config-write"
                } else {
                    ""
                }
            ),
            class: Some(key.class),
        });
    };
    if rank(scope) < rank(key.scope) {
        return Err(Refusal {
            message: format!(
                "`{path}` is {}-scoped and cannot be overridden per {}",
                key.scope.name(),
                scope.name()
            ),
            class: Some(key.class),
        });
    }
    Ok(socket)
}

/// May `config.persist` (the socket) write this key? Returns the socket class it needs.
/// `security` keys additionally show the KDL diff on the consent sheet (consumer).
pub fn check_persist(path: &str) -> Result<&'static str, Refusal> {
    let key = schema::key(path).ok_or_else(|| unknown(path))?;
    key.class.persist_socket_class().ok_or_else(|| Refusal {
        message: format!(
            "`{path}` is a `{}` key: it cannot be written over the socket; only a hand edit or Settings changes it",
            key.class.name()
        ),
        class: Some(key.class),
    })
}

/// The per-entry map checks of the load path (`shell.env` names, `security.sequence`, grants)
/// applied to a value about to be stored.
pub fn map_entries_ok(path: &str, v: &Value) -> Result<(), String> {
    let Value::Map(entries) = v else {
        return Ok(());
    };
    for (p, check) in crate::load::map_filters() {
        if p == path {
            for (k, args) in entries {
                check(k, args, &[])?;
            }
        }
    }
    Ok(())
}

/// Does the value fit the key's type (variant, range and content)?
pub fn value_fits(ty: &Ty, v: &Value) -> bool {
    variant_fits(ty, v) && content_ok(ty, v)
}

/// Content validation: render the value as the file would hold it and run the loader's own
/// validators on it, so nothing is accepted that the next load would reject.
fn content_ok(ty: &Ty, v: &Value) -> bool {
    let needs = matches!(
        v,
        Value::Str(_) | Value::List(_) | Value::Map(_) | Value::Color(_)
    );
    if !needs {
        return true;
    }
    let Some(text) = crate::value::render_node("k", ty, v, 0) else {
        return false;
    };
    let Ok(node) = KdlNode::parse(&text) else {
        return false;
    };
    crate::value::parse_ty(ty, &node, false).is_ok()
}

fn variant_fits(ty: &Ty, v: &Value) -> bool {
    match (ty, v) {
        (_, Value::Null) => false,
        (Ty::Bool, Value::Bool(_)) => true,
        (Ty::Int { min, max }, Value::Int(i)) => i >= min && i <= max,
        (Ty::Float { min, max }, Value::Float(f)) => f.is_finite() && f >= min && f <= max,
        (Ty::Duration { min_ms, max_ms }, Value::Int(i)) => {
            *i >= *min_ms as i64 && *i <= *max_ms as i64
        }
        (Ty::Bytes { min, max }, Value::Int(i)) => *i >= *min as i64 && *i <= *max as i64,
        (Ty::Enum(set), Value::Str(s)) => set.contains(&s.as_str()),
        (Ty::Color, Value::Color(_)) => true,
        (Ty::Palette16, Value::List(l)) => l.len() == 16,
        (t, Value::List(_)) => t.is_arg_list(),
        (Ty::Map { .. }, Value::Map(_)) => true,
        (
            Ty::Str
            | Ty::Font
            | Ty::Path
            | Ty::KeyChord
            | Ty::Action
            | Ty::Regex
            | Ty::Template
            | Ty::Match
            | Ty::ThemeRef
            | Ty::Adjust,
            Value::Str(_),
        ) => true,
        _ => false,
    }
}

/// The object an override is scoped to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScopeKey {
    pub scope: Scope,
    /// Window, tab or pane id; 0 for `Global`.
    pub id: u64,
}

/// Layer 8: transient, never written to disk.
#[derive(Debug, Default, Clone)]
pub struct RuntimeOverrides {
    map: BTreeMap<ScopeKey, BTreeMap<usize, Value>>,
}

impl RuntimeOverrides {
    pub fn new() -> Self {
        Self::default()
    }

    /// `config.set`. `Ok(true)` when the stored value changed (an identical set is `Ok(false)`
    /// and must not raise a change event: the loop guard of CONFIG-09).
    pub fn set(&mut self, at: ScopeKey, path: &str, value: Value) -> Result<bool, Refusal> {
        check_runtime_override(path, at.scope)?;
        let idx = schema::index_of(path).ok_or_else(|| unknown(path))?;
        if !value_fits(&TABLE[idx].ty, &value) {
            return Err(Refusal {
                message: format!("value does not fit `{path}`"),
                class: Some(TABLE[idx].class),
            });
        }
        if let Err(message) = map_entries_ok(path, &value) {
            return Err(Refusal {
                message,
                class: Some(TABLE[idx].class),
            });
        }
        let slot = self.map.entry(at).or_default();
        if slot.get(&idx) == Some(&value) {
            return Ok(false);
        }
        slot.insert(idx, value);
        Ok(true)
    }

    /// `config.set` with text (`apoth set --temp font.size=14`).
    pub fn set_text(&mut self, at: ScopeKey, path: &str, text: &str) -> Result<bool, Refusal> {
        check_runtime_override(path, at.scope)?;
        let key = schema::key(path).ok_or_else(|| unknown(path))?;
        let v = parse_override_value(key, text).map_err(|m| Refusal {
            message: format!("{path}: {m}"),
            class: Some(key.class),
        })?;
        self.set(at, path, v)
    }

    /// The override of one key at one scope, if any.
    pub fn get(&self, at: ScopeKey, path: &str) -> Option<&Value> {
        let idx = schema::index_of(path)?;
        self.map.get(&at)?.get(&idx)
    }

    /// `config.reset` of one key at one scope. True if something was cleared.
    pub fn unset(&mut self, at: ScopeKey, path: &str) -> bool {
        let Some(idx) = schema::index_of(path) else {
            return false;
        };
        self.map
            .get_mut(&at)
            .map(|m| m.remove(&idx).is_some())
            .unwrap_or(false)
    }

    /// The overrides at one scope, in table order (what `config.save-override` persists).
    pub fn entries_at(&self, at: ScopeKey) -> Vec<(&'static str, Value)> {
        self.map
            .get(&at)
            .map(|m| m.iter().map(|(i, v)| (TABLE[*i].path, v.clone())).collect())
            .unwrap_or_default()
    }

    /// Clear every override at a scope (a pane or window closing).
    pub fn clear_scope(&mut self, at: ScopeKey) {
        self.map.remove(&at);
    }

    /// Clear everything.
    pub fn clear_all(&mut self) {
        self.map.clear();
    }

    /// The config as seen from `chain`, broadest scope first (global, window, tab, pane).
    pub fn effective(&self, base: &Config, chain: &[ScopeKey]) -> Config {
        let mut cfg = base.clone();
        for at in chain {
            if let Some(m) = self.map.get(at) {
                for (idx, v) in m {
                    cfg.set_slot(*idx, v.clone(), Origin::at(Layer::Runtime));
                }
            }
        }
        cfg
    }
}
