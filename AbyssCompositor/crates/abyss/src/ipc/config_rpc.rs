// SPDX-License-Identifier: AGPL-3.0-only
//! The config read/write API (COMP-13 §1.4, F-01 §4).
//!
//! F-01 §4 requires every setting to be reachable without a text editor, and
//! COMP-13 §1.5 requires exactly one write path. This is it. The GUI holds no
//! parser and no file handle of its own: it asks for the schema, renders
//! controls from it, and posts values back here.
//!
//! Three things this module is careful about, in descending order of how bad
//! getting them wrong would be:
//!
//! 1. **`policy.kdl` is unreachable.** [`gate::check_config_file`] runs before
//!    any file is opened and before any `Config` field is read, and its result
//!    is tightened onto the outer gate decision rather than replacing it. A
//!    policy key is still *named* in `get_config` output — with `value: null`
//!    and `readable: false` — because a GUI that silently omits a key cannot
//!    tell the human the setting exists but is not theirs to change.
//! 2. **A refusal is distinguishable from a typo.** An unknown path is
//!    `invalid_params`; a known path in the closed file is a denial carrying
//!    `{file, path, reason}`. Collapsing the two would let a caller probe the
//!    schema by watching error codes, and would tell a confused human "no such
//!    setting" about a setting that plainly exists.
//! 3. **A write never half-applies.** The edit is rendered, spliced, written
//!    via tmp+rename, and then the whole config is re-loaded from disk. If that
//!    load produces errors, the backup goes back and the *first* error is
//!    returned untouched — the same text a file edit would have produced, so
//!    the GUI never invents a second vocabulary for the same refusal.

use serde_json::{json, Map, Value};

use super::gate::{self, Access, ConfigFile, Decision};
use super::RpcError;
use crate::config::{edit, schema, Config};
use crate::state::AbyssState;

type Reply = Result<Value, RpcError>;

pub fn dispatch(state: &mut AbyssState, outer: Decision, method: &str, params: &Value) -> Reply {
    match method {
        "get_config" => get_config(state, outer, params),
        "set_config_value" => set_config_value(state, outer, params),
        "validate_config" => validate_config(state, outer, params),
        other => Err(RpcError::not_implemented(other)),
    }
}

// ---------------------------------------------------------------- helpers

fn obj(params: &Value) -> &Map<String, Value> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    params.as_object().unwrap_or_else(|| EMPTY.get_or_init(Map::new))
}

fn str_param<'a>(params: &'a Value, name: &str) -> Option<&'a str> {
    obj(params).get(name).and_then(Value::as_str)
}

/// A path that is not in the schema is a typo, not a refusal. Kept separate
/// from the denial path on purpose — see the module note.
fn lookup_key(path: &str) -> Result<&'static schema::Key, RpcError> {
    schema::get_key(path).ok_or_else(|| RpcError::invalid_params(&format!("unknown config key {path:?}")))
}

fn file_of(owner: schema::Owner) -> ConfigFile {
    match owner {
        schema::Owner::Abyss => ConfigFile::Abyss,
        schema::Owner::Policy => ConfigFile::Policy,
    }
}

fn file_name(file: ConfigFile) -> &'static str {
    match file {
        ConfigFile::Abyss => "abyss",
        ConfigFile::Policy => "policy",
    }
}

fn parse_file(s: &str) -> Result<ConfigFile, RpcError> {
    match s {
        "abyss" => Ok(ConfigFile::Abyss),
        "policy" => Ok(ConfigFile::Policy),
        other => Err(RpcError::invalid_params(&format!(
            "file must be \"abyss\" or \"policy\", not {other:?}"
        ))),
    }
}

/// Allowed, or a denial naming the file and the reason.
///
/// The denial body is structured rather than prose so a GUI can grey the
/// control out and say *why* without string-matching a message.
fn allow_file(outer: Decision, file: ConfigFile, access: Access, path: &str) -> Result<(), RpcError> {
    match gate::check_config_file(outer, file, access) {
        Decision::Allow => Ok(()),
        Decision::Deny(reason) => {
            tracing::warn!(file = file_name(file), path, reason, "config access denied");
            Err(RpcError::denied(
                &json!({"file": file_name(file), "path": path, "reason": reason}).to_string(),
            ))
        }
    }
}

fn ty_json(ty: &schema::Ty) -> (&'static str, Value) {
    use schema::Ty::*;
    match ty {
        Bool => ("bool", Value::Null),
        Int { min, max } => ("int", json!({"min": min, "max": max})),
        Float { min, max } => ("float", json!({"min": min, "max": max})),
        Str => ("string", Value::Null),
        Enum(v) => ("enum", json!({"values": v})),
        Color => ("color", Value::Null),
        StrList => ("string-list", Value::Null),
    }
}

fn color_hex(c: [f32; 4]) -> String {
    let b = |f: f32| (f.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]), b(c[3]))
}

fn value_json(v: &schema::Value) -> Value {
    use schema::Value as V;
    match v {
        V::Null => Value::Null,
        V::Bool(b) => json!(b),
        V::Int(i) => json!(i),
        V::Float(f) => json!(f),
        V::Str(s) => json!(s),
        V::List(l) => json!(l),
        V::Color(c) => json!(color_hex(*c)),
    }
}

fn default_json(d: &schema::Dv) -> Value {
    use schema::Dv as D;
    match d {
        D::Null => Value::Null,
        D::Bool(b) => json!(b),
        D::Int(i) => json!(i),
        D::Float(f) => json!(f),
        D::Str(s) => json!(s),
        D::EmptyList => json!([] as [&str; 0]),
        D::Color(c) => json!(color_hex(*c)),
    }
}

/// The file on disk a key would be written to: the *last* source with that
/// owner, which is the one whose value wins.
fn target_path(cfg: &Config, owner: schema::Owner) -> Option<std::path::PathBuf> {
    cfg.sources
        .iter()
        .rev()
        .find(|s| s.owner == owner)
        .map(|s| s.path.clone())
}

// ---------------------------------------------------------------- get_config

fn get_config(state: &mut AbyssState, outer: Decision, params: &Value) -> Reply {
    let want_schema = obj(params)
        .get("schema")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let only_file = match str_param(params, "file") {
        Some(s) => Some(parse_file(s)?),
        None => None,
    };
    let only_path = str_param(params, "path");
    if let Some(p) = only_path {
        lookup_key(p)?;
    }

    // Reading abyss keys at all requires the read capability. This is checked
    // once, before any `Config` field is touched.
    allow_file(outer, ConfigFile::Abyss, Access::Read, only_path.unwrap_or(""))?;

    let mut keys = Vec::new();
    for key in schema::TABLE {
        if only_path.is_some_and(|p| p != key.path) {
            continue;
        }
        let file = file_of(key.owner);
        if only_file.is_some_and(|f| f != file) {
            continue;
        }
        let readable = matches!(
            gate::check_config_file(outer, file, Access::Read),
            Decision::Allow
        );
        let writable = matches!(
            gate::check_config_file(outer, file, Access::Write),
            Decision::Allow
        );
        let (ty, constraints) = ty_json(&key.ty);
        let default = default_json(&key.default);
        // A key in the closed file is listed, never read. The GUI shows it,
        // greyed, with its documentation — an invisible setting is worse than
        // a visible one the human cannot change.
        let value = if readable {
            schema::get(&state.config, key.path)
                .as_ref()
                .map_or(Value::Null, value_json)
        } else {
            Value::Null
        };
        let source = if !readable || value == default {
            Value::Null
        } else {
            target_path(&state.config, key.owner).map_or(Value::Null, |p| json!(p.display().to_string()))
        };
        let mut row = json!({
            "path": key.path,
            "file": file_name(file),
            "value": value,
            "source": source,
            "default": default,
            "readable": readable,
            "writable": writable,
        });
        if want_schema {
            let m = row.as_object_mut().expect("object");
            m.insert("type".into(), json!(ty));
            m.insert("constraints".into(), constraints);
            m.insert("doc".into(), json!(key.doc));
            m.insert(
                "reload".into(),
                json!(match key.reload {
                    schema::Reload::Live => "live",
                    schema::Reload::NeedsRestart => "restart",
                }),
            );
        }
        keys.push(row);
    }
    Ok(json!({ "keys": keys }))
}

// ----------------------------------------------------------- set_config_value

fn set_config_value(state: &mut AbyssState, outer: Decision, params: &Value) -> Reply {
    let path = str_param(params, "path")
        .ok_or_else(|| RpcError::invalid_params("path must be a string"))?
        .to_owned();
    let key = lookup_key(&path)?;
    let file = file_of(key.owner);
    // Before the file is opened, before `state.config` is read.
    allow_file(outer, file, Access::Write, &path)?;

    let raw = obj(params)
        .get("value")
        .ok_or_else(|| RpcError::invalid_params("value is required"))?;
    let kdl = coerce(raw, &key.ty)?;
    let dry_run = obj(params)
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let previous = schema::get(&state.config, &path)
        .as_ref()
        .map_or(Value::Null, value_json);
    let target = target_path(&state.config, key.owner)
        .ok_or_else(|| RpcError::invalid_params("no config file on the search path to write to"))?;

    let before = std::fs::read_to_string(&target).unwrap_or_default();
    let after =
        edit::set_value(&before, &path, &kdl).map_err(|e| RpcError::invalid_params(&format!("{e}")))?;

    if dry_run {
        return Ok(json!({
            "file": target.display().to_string(),
            "previous": previous,
            "applied": false,
            "restart_required": key.reload == schema::Reload::NeedsRestart,
        }));
    }

    write_atomically(&target, &after)?;

    // Re-read everything, not just this file: a key's effective value depends
    // on the whole search path, and the only honest check is the real load.
    let next = state.config.reload();
    if let Some(err) = next.errors.first() {
        let message = err.to_string();
        if let Err(e) = write_atomically(&target, &before) {
            // The rollback itself failed. Say so loudly; the file on disk is
            // now the rejected version and a human has to look.
            tracing::error!(path = %target.display(), error = %e.message, "config rollback failed");
        }
        return Err(RpcError::invalid_params(&message));
    }

    // `apply_loaded` records the on-disk hashes, which is what stops the
    // inotify event this write just caused from reloading an identical config.
    crate::config::apply_loaded(state, next);

    Ok(json!({
        "file": target.display().to_string(),
        "previous": previous,
        "applied": true,
        "restart_required": key.reload == schema::Reload::NeedsRestart,
    }))
}

/// tmp + rename in the same directory, so a reader never sees a half-written
/// config and a crash mid-write leaves the old one intact.
fn write_atomically(path: &std::path::Path, text: &str) -> Result<(), RpcError> {
    let dir = path.parent().unwrap_or(std::path::Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| RpcError::invalid_params(&format!("{}: {e}", dir.display())))?;
    let tmp = path.with_extension("kdl.tmp");
    std::fs::write(&tmp, text).map_err(|e| RpcError::invalid_params(&format!("{}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        RpcError::invalid_params(&format!("{}: {e}", path.display()))
    })
}

pub fn hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// JSON value → KDL value, checked against the key's type and range here
/// rather than by the caller. The client sends what the human typed; the
/// constraint lives with the schema, in one place.
fn coerce(raw: &Value, ty: &schema::Ty) -> Result<kdl::KdlValue, RpcError> {
    use schema::Ty::*;
    let bad = |what: &str| RpcError::invalid_params(what);
    Ok(match ty {
        Bool => kdl::KdlValue::Bool(raw.as_bool().ok_or_else(|| bad("value must be a boolean"))?),
        Int { min, max } => {
            let v = raw.as_i64().ok_or_else(|| bad("value must be an integer"))?;
            if v < *min || v > *max {
                return Err(bad(&format!("value must be in {min}..={max}")));
            }
            kdl::KdlValue::Integer(v as i128)
        }
        Float { min, max } => {
            let v = raw.as_f64().ok_or_else(|| bad("value must be a number"))?;
            if v < *min || v > *max {
                return Err(bad(&format!("value must be in {min}..={max}")));
            }
            kdl::KdlValue::Float(v)
        }
        Str => kdl::KdlValue::String(
            raw.as_str()
                .ok_or_else(|| bad("value must be a string"))?
                .to_owned(),
        ),
        Enum(allowed) => {
            let s = raw.as_str().ok_or_else(|| bad("value must be a string"))?;
            if !allowed.contains(&s) {
                return Err(bad(&format!("value must be one of {allowed:?}")));
            }
            kdl::KdlValue::String(s.to_owned())
        }
        Color => {
            let s = raw
                .as_str()
                .ok_or_else(|| bad("value must be a #rrggbb or #rrggbbaa string"))?;
            let hex = s.strip_prefix('#').unwrap_or(s);
            if !matches!(hex.len(), 6 | 8) || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(bad("value must be #rrggbb or #rrggbbaa"));
            }
            kdl::KdlValue::String(format!("#{hex}"))
        }
        // A list is many arguments on one node, which the single-value splice
        // in `edit.rs` cannot express. A6: lists start on the GUI-coverage
        // exception list and get their own verb later.
        StrList => return Err(RpcError::not_implemented("list-valued keys are not writable yet")),
    })
}

// ------------------------------------------------------------ validate_config

fn validate_config(state: &mut AbyssState, outer: Decision, params: &Value) -> Reply {
    let file = match str_param(params, "file") {
        Some(s) => parse_file(s)?,
        None => ConfigFile::Abyss,
    };
    allow_file(outer, file, Access::Read, "")?;

    // Validating a proposed *edit* needs the write capability too: otherwise
    // it is an oracle for whether a value would be accepted in a file the
    // caller may not touch.
    let proposed = obj(params).get("value");
    if proposed.is_some() {
        allow_file(
            outer,
            file,
            Access::Write,
            str_param(params, "path").unwrap_or(""),
        )?;
    }

    let owner = match file {
        ConfigFile::Abyss => schema::Owner::Abyss,
        ConfigFile::Policy => schema::Owner::Policy,
    };
    let target = target_path(&state.config, owner);

    let text = match (str_param(params, "text"), &target) {
        (Some(t), _) => t.to_owned(),
        (None, Some(p)) => std::fs::read_to_string(p).unwrap_or_default(),
        (None, None) => String::new(),
    };
    let text = match (str_param(params, "path"), proposed) {
        (Some(path), Some(raw)) => {
            let key = lookup_key(path)?;
            let kdl = coerce(raw, &key.ty)?;
            match edit::set_value(&text, path, &kdl) {
                Ok(t) => t,
                Err(e) => {
                    return Ok(json!({
                        "valid": false,
                        "errors": [json!({
                            "file": "", "line": 0, "col": 0,
                            "message": e.to_string(), "snippet": Value::Null, "spanLen": 0,
                        })],
                    }))
                }
            }
        }
        _ => text,
    };

    let path = target.unwrap_or_else(|| std::path::PathBuf::from("<text>"));
    let errors = Config::check_text(&path, owner, &text);
    let errors: Vec<Value> = errors.iter().map(crate::config::error_json).collect();
    Ok(json!({
        "valid": errors.is_empty(),
        "errors": errors,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The distinction the GUI depends on: a typo is `invalid_params`, a key
    /// in `policy.kdl` is a denial. Collapsing them would tell a human "no
    /// such setting" about a setting they can plainly see in the file.
    #[test]
    fn a_policy_key_is_denied_and_a_typo_is_not_found() {
        // Unknown path: invalid_params, and nothing about policy.
        let e = lookup_key("general.gaps-inn").unwrap_err();
        assert_eq!(e.code, super::super::INVALID_PARAMS, "{}", e.message);

        // Known policy path: denial, structured, naming the file.
        let key = lookup_key("capture.allow")
            .ok()
            .expect("capture.allow is in the schema");
        let e = allow_file(Decision::Allow, file_of(key.owner), Access::Write, key.path).unwrap_err();
        assert_eq!(e.code, super::super::DENIED);
        let body: Value = serde_json::from_str(&e.message).expect("structured denial");
        assert_eq!(body["file"], "policy");
        assert_eq!(body["path"], "capture.allow");
        assert!(body["reason"].is_string());
    }

    /// Writing `abyss.kdl` is allowed; `policy.kdl` is closed in both
    /// directions and no parameter can open it.
    #[test]
    fn abyss_is_writable_and_policy_is_not() {
        for access in [Access::Read, Access::Write] {
            assert!(matches!(
                gate::check_config_file(Decision::Allow, ConfigFile::Abyss, access),
                Decision::Allow
            ));
            assert!(matches!(
                gate::check_config_file(Decision::Allow, ConfigFile::Policy, access),
                Decision::Deny(_)
            ));
        }
    }

    /// Range and enum checks live with the schema, so the client cannot get
    /// them wrong by not knowing about them.
    #[test]
    fn values_are_checked_against_the_schema() {
        assert!(coerce(&json!(true), &schema::Ty::Bool).is_ok());
        assert!(coerce(&json!(1), &schema::Ty::Bool).is_err());
        let ty = schema::Ty::Int { min: 0, max: 10 };
        assert!(coerce(&json!(10), &ty).is_ok());
        assert!(coerce(&json!(11), &ty).is_err());
        let ty = schema::Ty::Enum(&["a", "b"]);
        assert!(coerce(&json!("b"), &ty).is_ok());
        assert!(coerce(&json!("c"), &ty).is_err());
        assert!(coerce(&json!("#ff8800"), &schema::Ty::Color).is_ok());
        assert!(coerce(&json!("#ff88"), &schema::Ty::Color).is_err());
        // A6: lists are not writable in v1 and say so, rather than silently
        // writing a one-element list.
        assert!(coerce(&json!(["x"]), &schema::Ty::StrList).is_err());
    }
}
