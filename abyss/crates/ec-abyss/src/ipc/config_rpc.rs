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
        "set_config_values" => set_config_values(state, outer, params),
        "set_config_collection" => set_config_collection(state, outer, params),
        "validate_config" => validate_config(state, outer, params),
        "review_widget" => review_widget(state, params),
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

/// `path` only feeds the optional `drag-max` UI hint (schema::DRAG_MAX);
/// the type and its `min`/`max` come from `ty` alone, and `max` is always
/// the real, enforced validation bound — `drag-max` never changes it.
fn ty_json(path: &str, ty: &schema::Ty) -> (&'static str, Value) {
    use schema::Ty::*;
    match ty {
        Bool => ("bool", Value::Null),
        Int { min, max } => {
            let mut c = json!({"min": min, "max": max});
            if let Some(dm) = schema::drag_max(path) {
                c.as_object_mut()
                    .expect("object")
                    .insert("drag-max".into(), json!(dm));
            }
            ("int", c)
        }
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
        D::List(l) => json!(l),
        D::Color(c) => json!(color_hex(*c)),
    }
}

/// One `bar { widget "<name>" { … } }` block as `get_config` serves it under
/// `collections.widget` (ADR 0065). Every field is always present so a client
/// never has to tell "absent" from "null"; the shape is documented on the
/// `widget` collection in `schema.rs` and in `docs/CONFIG.md`.
fn widget_json(w: &crate::config::CustomWidget) -> Value {
    use crate::config::CustomWidgetKind as K;
    let (kind, exec, interval, source, format) = match &w.kind {
        K::Exec { argv, interval_ms } => ("exec", json!(argv), json!(interval_ms), Value::Null, Value::Null),
        K::Stream { argv } => ("stream", json!(argv), Value::Null, Value::Null, Value::Null),
        K::Source { source, format } => ("source", Value::Null, Value::Null, json!(source), json!(format)),
    };
    json!({
        "name": w.name,
        "kind": kind,
        "exec": exec,
        "interval-ms": interval,
        "source": source,
        "format": format,
        "icon": w.icon,
        "on-click": w.on_click,
        "on-scroll-up": w.on_scroll_up,
        "on-scroll-down": w.on_scroll_down,
    })
}

/// One `wallpaper { output "<name>" { … } }` override as `get_config` serves
/// it under `collections."wallpaper.output"`. Every field is present; `null`
/// is a key the block leaves to the global one.
fn wallpaper_output_json(o: &crate::config::WallpaperOutput) -> Value {
    json!({
        "output": o.name,
        "path": o.path,
        "mode": o.mode,
        "color": o.color.map(color_hex),
    })
}

/// The file on disk a key would be written to.
///
/// A settings write comes from a human in a normal session, who cannot write
/// `/etc/eclipse` — so the target is the user tier, even when no file exists
/// there yet (`write_atomically` creates it). Only the last *user-owned*
/// source is considered, so a drop-in that would shadow the write still wins
/// the target. `--config` names the file to edit and overrides all of it.
fn target_path(cfg: &Config, owner: schema::Owner) -> Option<std::path::PathBuf> {
    if owner == schema::Owner::Abyss {
        if let Some(p) = &cfg.explicit {
            return Some(p.clone());
        }
    }
    let base = crate::config::user_config_base();
    if let Some(base) = &base {
        let user = cfg
            .sources
            .iter()
            .rev()
            .find(|s| s.owner == owner && s.path.starts_with(base))
            .map(|s| s.path.clone());
        return Some(user.unwrap_or_else(|| match owner {
            schema::Owner::Abyss => base.join("eclipse/abyss.kdl"),
            schema::Owner::Policy => base.join("eclipse/policy.kdl"),
        }));
    }
    cfg.sources
        .iter()
        .rev()
        .find(|s| s.owner == owner)
        .map(|s| s.path.clone())
}

/// A widget as `get_config` serves it (ADR 0067): the live entry plus
/// `approval: "approved"` and `premade`.
fn live_widget_json(catalog: &[crate::config::CustomWidget], w: &crate::config::CustomWidget) -> Value {
    let mut v = widget_json(w);
    let m = v.as_object_mut().expect("object");
    m.insert("approval".into(), json!("approved"));
    m.insert(
        "premade".into(),
        json!(crate::config::withhold::is_premade(catalog, w)),
    );
    v
}

/// A withheld widget: its name and why, and nothing that runs. No `exec`, no
/// actions, no `kind`-specific fields, so no reader can run it.
fn pending_widget_json(p: &crate::config::withhold::PendingWidget) -> Value {
    json!({
        "name": p.name,
        "approval": "pending",
        "premade": false,
        "altered": p.kind == crate::config::withhold::PendingKind::Altered,
    })
}

/// `collections.widget`: live and withheld entries in merged file order.
fn widgets_json(state: &AbyssState) -> Vec<Value> {
    let live = &state.config.bar.custom_widgets;
    let withheld = &state.widget_approvals.withheld;
    let mut out = Vec::with_capacity(live.len() + withheld.len());
    let mut live_iter = live.iter();
    for i in 0..live.len() + withheld.len() {
        match withheld.iter().find(|(at, _)| *at == i) {
            Some((_, p)) => out.push(pending_widget_json(p)),
            None => {
                if let Some(w) = live_iter.next() {
                    out.push(live_widget_json(&state.config.catalog, w));
                }
            }
        }
    }
    out
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
        let (ty, constraints) = ty_json(key.path, &key.ty);
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
        // A changed Oracle Eyes model command is withheld (served as null)
        // until the owner approves it in the compositor-drawn prompt
        // (owner decision 3, ADR 0067's path). Say which, so a pane can tell
        // "unset" from "awaiting approval" without seeing the command.
        if key.path == "oracle-eyes.model-command" {
            let pending = state.widget_approvals.model_command.withheld.is_some();
            row.as_object_mut().expect("object").insert(
                "approval".into(),
                json!(if pending { "pending" } else { "approved" }),
            );
        }
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
    // Collections are abyss-owned and not keyed by path, so they ride along
    // only on an unfiltered abyss read; the capability check above covers them.
    // Installed add-ons (ADR 0066) ride on every reply: they are not config
    // and not a secret, and a pane that greys out a hooked control needs them
    // next to the keys it is drawing.
    let (addons, hooks_on) = crate::addons::json(&state.addons);
    if only_path.is_none() && only_file != Some(ConfigFile::Policy) {
        let widgets = widgets_json(state);
        let wallpapers: Vec<Value> = state
            .config
            .wallpaper
            .outputs
            .iter()
            .map(wallpaper_output_json)
            .collect();
        return Ok(json!({
            "keys": keys,
            "collections": { "widget": widgets, "wallpaper.output": wallpapers },
            "animations": animations_json(&state.config.animations, state.borders.anim.shaders.styles()),
            "addons": addons,
            "hooks_on": hooks_on,
        }));
    }
    Ok(json!({ "keys": keys, "addons": addons, "hooks_on": hooks_on }))
}

/// `get_config.animations`: the preset, whether anything overrides it, and
/// every event resolved, so Settings and the taskbar never expand a preset
/// themselves. `styles` lists the built-ins and, after them, an add-on
/// style the event is set to, so a picker can always show the current value.
/// `installed` is the transition catalog (empty while the `transition-shaders`
/// hook is off): each style serving an event is listed after the built-ins,
/// and its label is under `addon_labels` by id.
fn animations_json(
    a: &crate::config::Animations,
    installed: &[ec_abyss_config::transitions::TransitionStyle],
) -> Value {
    use crate::config::animations::Event;
    let mut events = Map::new();
    let mut styles = Map::new();
    let mut labels = Map::new();
    for style in installed {
        labels.insert(style.id.clone(), json!(style.label));
    }
    for ev in Event::ALL {
        let r = a.resolve(ev);
        events.insert(
            ev.key().into(),
            json!({
                "style": r.style,
                "duration-ms": r.duration_ms,
                "curve": r.curve.key(),
                "overridden": a.overrides.contains_key(&ev),
            }),
        );
        let mut list: Vec<&str> = ev.styles().to_vec();
        list.extend(installed.iter().filter(|s| s.serves(ev)).map(|s| s.id.as_str()));
        if let Some(s) = a.overrides.get(&ev).and_then(|o| o.style.as_deref()) {
            if !ev.is_builtin(s) && !list.contains(&s) {
                list.push(s);
            }
        }
        styles.insert(ev.key().into(), json!(list));
    }
    json!({
        "preset": a.preset.key(),
        "custom": a.custom(),
        "speed": a.speed,
        "reduce_motion": a.reduce_motion,
        "events": events,
        "styles": styles,
        "addon_labels": labels,
    })
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
    let edit = coerce_edit(raw, &key.ty)?;
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
    let after = edit
        .apply(&before, &path)
        .map_err(|e| RpcError::invalid_params(&format!("{e}")))?;

    if dry_run {
        // The same check the real write's reload makes, so a dry run cannot
        // pass an edit that `commit` would roll back.
        let errors = Config::check_text_with(&target, key.owner, &after, &state.config.catalog);
        let errors: Vec<Value> = errors.iter().map(crate::config::error_json).collect();
        return Ok(json!({
            "file": target.display().to_string(),
            "previous": previous,
            "applied": false,
            "restart_required": key.reload == schema::Reload::NeedsRestart,
            "valid": errors.is_empty(),
            "errors": errors,
        }));
    }

    commit(state, &target, &before, &after)?;

    Ok(json!({
        "file": target.display().to_string(),
        "previous": previous,
        "applied": true,
        "restart_required": key.reload == schema::Reload::NeedsRestart,
        "valid": true,
        "errors": [],
    }))
}

// ---------------------------------------------------------- set_config_values

/// Several keys in one write: `{edits: [{path, value}], dry_run?}`. Every
/// edit is gated, coerced and spliced before anything is written, then the
/// lot goes through one [`commit`] and so one `config` event. A preset card
/// in Settings is the reason: `preset` plus a `null` per override, which as
/// separate writes would reload and redraw a dozen times through states the
/// human never asked for. All or nothing: one bad edit refuses the batch.
fn set_config_values(state: &mut AbyssState, outer: Decision, params: &Value) -> Reply {
    let edits = obj(params)
        .get("edits")
        .and_then(Value::as_array)
        .filter(|e| !e.is_empty())
        .ok_or_else(|| RpcError::invalid_params("edits must be a non-empty array of {path, value}"))?;
    // Gate every path first, before any file is opened or `state.config` read.
    let mut keys = Vec::with_capacity(edits.len());
    for e in edits {
        let path = e
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::invalid_params("each edit needs a string path"))?;
        let key = lookup_key(path)?;
        allow_file(outer, file_of(key.owner), Access::Write, path)?;
        keys.push(key);
    }
    let dry_run = obj(params)
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // Every writable key lives in one file today; a batch spanning two would
    // need two commits, which is exactly what this method exists to avoid.
    let owner = keys[0].owner;
    if keys.iter().any(|k| k.owner != owner) {
        return Err(RpcError::invalid_params(
            "a batch may only write keys of one file",
        ));
    }
    let target = target_path(&state.config, owner)
        .ok_or_else(|| RpcError::invalid_params("no config file on the search path to write to"))?;
    let before = std::fs::read_to_string(&target).unwrap_or_default();
    let mut after = before.clone();
    let mut previous = Map::new();
    for (e, key) in edits.iter().zip(&keys) {
        let raw = e
            .get("value")
            .ok_or_else(|| RpcError::invalid_params(&format!("{}: value is required", key.path)))?;
        let edit = coerce_edit(raw, &key.ty)?;
        after = edit
            .apply(&after, key.path)
            .map_err(|e| RpcError::invalid_params(&format!("{}: {e}", key.path)))?;
        previous.entry(key.path).or_insert_with(|| {
            schema::get(&state.config, key.path)
                .as_ref()
                .map_or(Value::Null, value_json)
        });
    }
    let restart_required = keys.iter().any(|k| k.reload == schema::Reload::NeedsRestart);

    if dry_run {
        let errors = Config::check_text_with(&target, owner, &after, &state.config.catalog);
        let errors: Vec<Value> = errors.iter().map(crate::config::error_json).collect();
        return Ok(json!({
            "file": target.display().to_string(),
            "previous": previous,
            "applied": false,
            "restart_required": restart_required,
            "valid": errors.is_empty(),
            "errors": errors,
        }));
    }

    commit(state, &target, &before, &after)?;

    Ok(json!({
        "file": target.display().to_string(),
        "previous": previous,
        "applied": true,
        "restart_required": restart_required,
        "valid": true,
        "errors": [],
    }))
}

/// The one write path (COMP-13 §1.5): write `after`, re-load the whole config
/// from disk, and either apply it and announce `config`, or put `before` back
/// and return the first load error untouched. Every socket write ends here.
fn commit(
    state: &mut AbyssState,
    target: &std::path::Path,
    before: &str,
    after: &str,
) -> Result<(), RpcError> {
    write_atomically(target, after)?;

    // Re-read everything, not just this file: a key's effective value depends
    // on the whole search path, and the only honest check is the real load.
    let next = state.config.reload();
    if let Some(err) = next.errors.first() {
        let message = err.to_string();
        if let Err(e) = write_atomically(target, before) {
            // The rollback itself failed. Say so loudly; the file on disk is
            // now the rejected version and a human has to look.
            tracing::error!(path = %target.display(), error = %e.message, "config rollback failed");
        }
        return Err(RpcError::invalid_params(&message));
    }

    // `apply_loaded` records the on-disk hashes, which is what stops the
    // inotify event this write just caused from reloading an identical config.
    crate::config::apply_loaded(state, next);
    // The inotify reload this write triggers is suppressed by that hash check,
    // so it will not emit `config`; subscribers (the bar, Settings) hear about
    // a socket-driven change only from here. Same event as `watch::reload_now`.
    crate::ipc::emit(state, "config", json!({}));
    Ok(())
}

// ------------------------------------------------------ set_config_collection

/// Collections the socket can write, by node name. Anything else in
/// `schema::COLLECTIONS` is readable but refused here; a collection joins
/// this list with its own entry renderer and a row in `collection_write`.
const WRITABLE_COLLECTIONS: &[&str] = &["widget"];

/// Lists that name widgets by id: a removal drops `custom:<name>` from them
/// and a rename rewrites it, in the same write (ADR 0065).
const WIDGET_ID_LISTS: &[&str] = &["bar.widgets.order", "bar.widgets.important"];

/// Create, replace, rename, reorder or delete one entry of a collection
/// (COMP-13 §1.4; ADR 0065). Params:
/// `{collection, op: "upsert"|"remove"|"rename"|"move", name, entry?, new_name?, index?, dry_run?}`.
fn set_config_collection(state: &mut AbyssState, outer: Decision, params: &Value) -> Reply {
    let collection = str_param(params, "collection")
        .ok_or_else(|| RpcError::invalid_params("collection must be a string"))?;
    let coll = schema::COLLECTIONS
        .iter()
        .find(|c| c.node == collection)
        .ok_or_else(|| RpcError::invalid_params(&format!("unknown config collection {collection:?}")))?;
    let file = file_of(coll.owner);
    // Before the file is opened, before `state.config` is read.
    allow_file(outer, file, Access::Write, collection)?;
    if !WRITABLE_COLLECTIONS.contains(&collection) {
        return Err(RpcError::invalid_params(&format!(
            "collection {collection:?} is not settable over the socket yet"
        )));
    }
    let target = target_path(&state.config, coll.owner)
        .ok_or_else(|| RpcError::invalid_params("no config file on the search path to write to"))?;
    let dry_run = obj(params)
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let before = std::fs::read_to_string(&target).unwrap_or_default();
    let (previous, after, references) = widget_edit(state, params, &before)?;

    if dry_run {
        let errors = Config::check_text_with(&target, coll.owner, &after, &state.config.catalog);
        let errors: Vec<Value> = errors.iter().map(crate::config::error_json).collect();
        return Ok(json!({
            "file": target.display().to_string(),
            "previous": previous,
            "references": references,
            "applied": false,
            "valid": errors.is_empty(),
            "errors": errors,
        }));
    }

    commit(state, &target, &before, &after)?;
    Ok(json!({
        "file": target.display().to_string(),
        "previous": previous,
        "references": references,
        "applied": true,
        "valid": true,
        "errors": [],
    }))
}

/// Render one `widget` op onto `before`: `(previous entry, new text, the id
/// lists it rewrote)`.
fn widget_edit(
    state: &AbyssState,
    params: &Value,
    before: &str,
) -> Result<(Value, String, Vec<&'static str>), RpcError> {
    let kind = edit::WIDGET;
    let name = str_param(params, "name")
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| RpcError::invalid_params("name must be a non-empty string"))?;
    let op = str_param(params, "op").ok_or_else(|| RpcError::invalid_params("op must be a string"))?;
    let e = |e: edit::EditError| RpcError::invalid_params(&e.to_string());
    let existing = |n: &str| state.config.bar.custom_widgets.iter().find(|w| w.name == n);
    let withheld = |n: &str| state.widget_approvals.withheld.iter().find(|(_, p)| p.name == n);
    // A withheld widget's argv is never served, not even as `previous`.
    let previous = match (existing(name), withheld(name)) {
        (Some(w), _) => widget_json(w),
        (None, Some((_, p))) => pending_widget_json(p),
        (None, None) => Value::Null,
    };
    let id = format!("{}{name}", schema::BAR_WIDGET_CUSTOM_PREFIX);
    let mut references = Vec::new();

    let after = match op {
        "upsert" => {
            let entry = obj(params)
                .get("entry")
                .ok_or_else(|| RpcError::invalid_params("upsert needs an entry"))?;
            let body = widget_body(name, entry)?;
            edit::upsert_block(before, kind, name, &body).map_err(e)?
        }
        "remove" => {
            let mut text = edit::remove_block(before, kind, name).map_err(e)?;
            for path in WIDGET_ID_LISTS {
                let next = edit::replace_list_item(&text, path, &id, None).map_err(e)?;
                if next != text {
                    references.push(*path);
                }
                text = next;
            }
            text
        }
        "rename" => {
            let new_name = str_param(params, "new_name")
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| RpcError::invalid_params("rename needs a non-empty new_name"))?;
            if new_name == name {
                return Err(RpcError::invalid_params("new_name is the current name"));
            }
            if existing(new_name).is_some() || withheld(new_name).is_some() {
                return Err(RpcError::invalid_params(&format!(
                    "widget {new_name:?} already exists"
                )));
            }
            let new_id = format!("{}{new_name}", schema::BAR_WIDGET_CUSTOM_PREFIX);
            let mut text = edit::rename_block(before, kind, name, new_name).map_err(e)?;
            for path in WIDGET_ID_LISTS {
                let next = edit::replace_list_item(&text, path, &id, Some(&new_id)).map_err(e)?;
                if next != text {
                    references.push(*path);
                }
                text = next;
            }
            text
        }
        "move" => {
            let index = obj(params)
                .get("index")
                .and_then(Value::as_u64)
                .ok_or_else(|| RpcError::invalid_params("move needs an index (0-based)"))?;
            edit::move_block(before, kind, name, index as usize).map_err(e)?
        }
        other => {
            return Err(RpcError::invalid_params(&format!(
                "op must be \"upsert\", \"remove\", \"rename\" or \"move\", not {other:?}"
            )))
        }
    };
    Ok((previous, after, references))
}

// -------------------------------------------------------- command approval

/// `review_widget {name}` (ADR 0067; COMP-10 §3.11): re-queue a withheld
/// widget's approval prompt. It carries no answer and cannot approve; a widget
/// already queued or on screen is not queued again (`queued: false`).
fn review_widget(state: &mut AbyssState, params: &Value) -> Reply {
    use crate::config::withhold::{self, ReviewError};
    let name = str_param(params, "name")
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| RpcError::invalid_params("name must be a non-empty string"))?;
    match withhold::review(state, name) {
        Ok(queued) => {
            crate::trusted_ui::approval::schedule(state);
            Ok(json!({ "name": name, "queued": queued }))
        }
        Err(ReviewError::Unknown) => Err(RpcError::invalid_params(&format!("no widget named {name:?}"))),
        Err(ReviewError::NotPending) => Err(RpcError::invalid_params(&format!(
            "widget {name:?} is not waiting on approval"
        ))),
    }
}

fn widgets_hooked(state: &AbyssState) -> Result<(), RpcError> {
    if state.addons.hooks.is_on(crate::addons::Hook::TaskbarWidgets) {
        Ok(())
    } else {
        Err(RpcError::denied(crate::addons::Hook::TaskbarWidgets.off_reason()))
    }
}

/// Revert an altered premade: delete the owner's `widget "<name>"` block so
/// the catalog block applies again. `custom:<name>` ids stay, since the
/// catalog still defines the name. Same write path as `set_config_collection`
/// (render, write, full reload, roll back on refusal, re-apply, `config`).
///
/// **Only the Trusted UI approval surface (`trusted_ui/`, ADR 0067) may call
/// this**, on the owner's Revert. It is not a socket method; a test scans the
/// source to keep every other caller out.
pub fn revert_widget(state: &mut AbyssState, name: &str) -> Result<(), RpcError> {
    widgets_hooked(state)?;
    if !state.config.catalog.iter().any(|w| w.name == name) {
        return Err(RpcError::invalid_params(&format!(
            "no premade widget named {name:?}"
        )));
    }
    let target = target_path(&state.config, schema::Owner::Abyss)
        .ok_or_else(|| RpcError::invalid_params("no config file on the search path to write to"))?;
    let before = std::fs::read_to_string(&target).unwrap_or_default();
    let after = edit::remove_block(&before, edit::WIDGET, name)
        .map_err(|e| RpcError::invalid_params(&e.to_string()))?;
    commit(state, &target, &before, &after)
}

/// Revert the Oracle Eyes model command to the shipped default (`claude`)
/// in the owner's `abyss.kdl`. Same write path as `set_config_value`
/// (render, write, full reload, roll back on refusal, re-apply, `config`).
///
/// **Only the Trusted UI approval surface (`trusted_ui/`, ADR 0067) may call
/// this**, on the owner's Revert. It is not a socket method; a test scans the
/// source to keep every other caller out.
pub fn revert_model_command(state: &mut AbyssState) -> Result<(), RpcError> {
    let target = target_path(&state.config, schema::Owner::Abyss)
        .ok_or_else(|| RpcError::invalid_params("no config file on the search path to write to"))?;
    let before = std::fs::read_to_string(&target).unwrap_or_default();
    let default: Vec<kdl::KdlValue> = crate::config::ORACLE_EYES_MODEL_COMMAND
        .iter()
        .map(|s| kdl::KdlValue::String((*s).to_owned()))
        .collect();
    let after = edit::set_list(&before, "oracle-eyes.model-command", &default)
        .map_err(|e| RpcError::invalid_params(&e.to_string()))?;
    commit(state, &target, &before, &after)
}

/// Remove a new command widget: delete the owner's block and its
/// `custom:<name>` ids, exactly as `set_config_collection` `remove` does.
///
/// **Only the Trusted UI approval surface (`trusted_ui/`, ADR 0067) may call
/// this**, on the owner's Remove. It is not a socket method; a test scans the
/// source to keep every other caller out.
pub fn remove_widget(state: &mut AbyssState, name: &str) -> Result<(), RpcError> {
    widgets_hooked(state)?;
    let target = target_path(&state.config, schema::Owner::Abyss)
        .ok_or_else(|| RpcError::invalid_params("no config file on the search path to write to"))?;
    let before = std::fs::read_to_string(&target).unwrap_or_default();
    let (_, after, _) = widget_edit(state, &json!({ "op": "remove", "name": name }), &before)?;
    commit(state, &target, &before, &after)
}

/// A `get_config` widget entry → the lines of its block. Only the JSON
/// *shape* is checked here; what the values mean (a known source, an
/// interval in range) is the parser's call on reload, so a refusal reads
/// exactly as it would for a hand edit. Defaults (`interval-ms` 5000,
/// `format "{}"`) are left out, so a read-modify-write does not grow the file.
fn widget_body(name: &str, entry: &Value) -> Result<Vec<String>, RpcError> {
    const FIELDS: &[&str] = &[
        "name",
        "kind",
        "exec",
        "interval-ms",
        "source",
        "format",
        "icon",
        "on-click",
        "on-scroll-up",
        "on-scroll-down",
        // Reported by `get_config` (ADR 0067), ignored on write: a pane may
        // post back what it read, and approval is never set from here.
        "approval",
        "premade",
        "altered",
    ];
    let bad = |m: String| RpcError::invalid_params(&m);
    let m = entry
        .as_object()
        .ok_or_else(|| bad("entry must be an object".into()))?;
    if let Some(k) = m.keys().find(|k| !FIELDS.contains(&k.as_str())) {
        return Err(bad(format!("unknown widget field {k:?}")));
    }
    let field = |k: &str| m.get(k).filter(|v| !v.is_null());
    if let Some(n) = field("name") {
        if n.as_str() != Some(name) {
            return Err(bad(format!("entry.name must match name {name:?}")));
        }
    }
    let string = |k: &str| -> Result<Option<String>, RpcError> {
        match field(k) {
            None => Ok(None),
            Some(v) => v
                .as_str()
                .map(|s| Some(s.to_owned()))
                .ok_or_else(|| bad(format!("{k} must be a string or null"))),
        }
    };
    let argv = |k: &str| -> Result<Option<String>, RpcError> {
        let Some(v) = field(k) else { return Ok(None) };
        let msg = || bad(format!("{k} must be a non-empty array of strings, or null"));
        let items = v.as_array().filter(|a| !a.is_empty()).ok_or_else(msg)?;
        let words: Option<Vec<String>> = items.iter().map(|i| i.as_str().map(edit::quote)).collect();
        Ok(Some(format!("{k} {}", words.ok_or_else(msg)?.join(" "))))
    };
    let must_be_null = |k: &str, kind: &str| -> Result<(), RpcError> {
        match field(k) {
            Some(_) => Err(bad(format!("{k} must be null for a {kind} widget"))),
            None => Ok(()),
        }
    };

    let kind = field("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("entry.kind must be \"exec\", \"stream\" or \"source\"".into()))?;
    let mut lines = Vec::new();
    match kind {
        "exec" | "stream" => {
            lines.push(argv("exec")?.ok_or_else(|| bad(format!("a {kind} widget needs exec")))?);
            must_be_null("source", kind)?;
            must_be_null("format", kind)?;
            if kind == "stream" {
                must_be_null("interval-ms", kind)?;
                lines.push("stream #true".into());
            } else if let Some(v) = field("interval-ms") {
                let ms = v
                    .as_u64()
                    .ok_or_else(|| bad("interval-ms must be a positive integer or null".into()))?;
                if ms != u64::from(schema::WIDGET_DEFAULT_INTERVAL_MS) {
                    lines.push(format!("interval-ms {ms}"));
                }
            }
        }
        "source" => {
            must_be_null("exec", kind)?;
            must_be_null("interval-ms", kind)?;
            let source = string("source")?.ok_or_else(|| bad("a source widget needs source".into()))?;
            lines.push(format!("source {}", edit::quote(&source)));
            if let Some(f) = string("format")?.filter(|f| f != "{}") {
                lines.push(format!("format {}", edit::quote(&f)));
            }
        }
        other => {
            return Err(bad(format!(
                "entry.kind must be \"exec\", \"stream\" or \"source\", not {other:?}"
            )))
        }
    }
    if let Some(icon) = string("icon")? {
        lines.push(format!("icon {}", edit::quote(&icon)));
    }
    for k in ["on-click", "on-scroll-up", "on-scroll-down"] {
        lines.extend(argv(k)?);
    }
    Ok(lines)
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

/// A checked value, ready to splice: one argument, a whole list node, or
/// the key's removal.
enum Edit {
    Value(kdl::KdlValue),
    List(Vec<kdl::KdlValue>),
    Remove,
}

impl Edit {
    fn apply(&self, text: &str, path: &str) -> Result<String, edit::EditError> {
        match self {
            Self::Value(v) => edit::set_value(text, path, v),
            Self::List(vs) => edit::set_list(text, path, vs),
            Self::Remove => edit::remove_value(text, path),
        }
    }
}

/// JSON `null` removes the key from the file, so it follows the next file
/// down the search path or its default; anything else is checked against
/// the key's type.
fn coerce_edit(raw: &Value, ty: &schema::Ty) -> Result<Edit, RpcError> {
    match ty {
        _ if raw.is_null() => Ok(Edit::Remove),
        schema::Ty::StrList => coerce_list(raw).map(Edit::List),
        _ => coerce(raw, ty).map(Edit::Value),
    }
}

/// JSON array of strings → the arguments of one list node. Anything else in
/// the array is refused outright rather than stringified.
fn coerce_list(raw: &Value) -> Result<Vec<kdl::KdlValue>, RpcError> {
    let bad = || RpcError::invalid_params("value must be an array of strings");
    raw.as_array()
        .ok_or_else(bad)?
        .iter()
        .map(|v| {
            v.as_str()
                .map(|s| kdl::KdlValue::String(s.to_owned()))
                .ok_or_else(bad)
        })
        .collect()
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
        // A list is many arguments on one node: `coerce_edit` routes it to
        // `coerce_list` and `edit::set_list`, never through here.
        StrList => return Err(RpcError::invalid_params("value must be an array of strings")),
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
            match coerce_edit(raw, &key.ty)?.apply(&text, path) {
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
    // Same layers as the real load: the catalog defines `custom:<premade>`.
    let errors = Config::check_text_with(&path, owner, &text, &state.config.catalog);
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
        // A list is a JSON array of strings, spliced as one node; a scalar or
        // a mixed array is refused, never stringified.
        assert!(coerce(&json!(["x"]), &schema::Ty::StrList).is_err());
        assert!(
            matches!(coerce_edit(&json!(["a", "b"]), &schema::Ty::StrList), Ok(Edit::List(v)) if v.len() == 2)
        );
        assert!(coerce_edit(&json!([]), &schema::Ty::StrList).is_ok());
        assert!(coerce_edit(&json!("a"), &schema::Ty::StrList).is_err());
        assert!(coerce_edit(&json!(["a", 1]), &schema::Ty::StrList).is_err());
    }

    /// `drag-max` rides along on the wire schema for the keys the catalog
    /// names, and never for any other key; `min`/`max` — the real, enforced
    /// bound — are unaffected either way.
    #[test]
    fn drag_max_hint_is_additive_and_only_on_named_keys() {
        let key = lookup_key("general.gaps-in").ok().expect("in the schema");
        let (_, c) = ty_json(key.path, &key.ty);
        assert_eq!(c["min"], 0);
        assert_eq!(c["max"], 512, "validation max is untouched by the hint");
        assert_eq!(c["drag-max"], 64);

        let key = lookup_key("input.repeat-rate").ok().expect("in the schema");
        let (_, c) = ty_json(key.path, &key.ty);
        assert!(c.get("drag-max").is_none(), "no hint for this key: {c}");
    }

    /// A settings write from a normal session must never target `/etc`: the
    /// user cannot write it, and the whole point of the user tier is that it
    /// overrides the system one anyway.
    /// A write over the socket is announced on `config` itself: the inotify
    /// reload it causes is suppressed by the hash check and emits nothing, so
    /// without this the bar and Settings never hear of the change.
    #[test]
    fn a_successful_set_emits_config() {
        let mut h = crate::shell::focus::state_tests::harness();
        let dir = std::env::temp_dir().join(format!("abyss-set-emit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("abyss.kdl");
        std::fs::write(&file, "bar {\n    rounding 20\n}\n").unwrap();
        h.state.config.explicit = Some(file.clone());
        crate::ipc::capture::take();

        let ok = set_config_value(
            &mut h.state,
            Decision::Allow,
            &json!({"path": "bar.popup-anchor", "value": "pointer"}),
        );
        let events = crate::ipc::capture::take();
        assert!(ok.is_ok(), "{:?}", ok.err().map(|e| e.message));
        assert_eq!(
            events.iter().filter(|(k, _)| k == "config").count(),
            1,
            "{events:?}"
        );

        // A refused value changes nothing and announces nothing.
        let bad = set_config_value(
            &mut h.state,
            Decision::Allow,
            &json!({"path": "bar.popup-anchor", "value": "corner"}),
        );
        assert!(bad.is_err());
        assert!(crate::ipc::capture::take().iter().all(|(k, _)| k != "config"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `value: null` removes the key from the file, so it reads back as its
    /// default; removing a key that is not written changes nothing.
    #[test]
    fn a_null_value_removes_the_key() {
        let mut h = crate::shell::focus::state_tests::harness();
        let dir = std::env::temp_dir().join(format!("abyss-set-null-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("abyss.kdl");
        std::fs::write(
            &file,
            "animations {\n    preset \"smooth\"\n    focus {\n        duration-ms 300\n    }\n}\n",
        )
        .unwrap();
        h.state.config.explicit = Some(file.clone());
        let next = h.state.config.reload();
        crate::config::apply_loaded(&mut h.state, next);
        assert!(h.state.config.animations.custom());

        let ok = set_config_value(
            &mut h.state,
            Decision::Allow,
            &json!({"path": "animations.focus.duration-ms", "value": null}),
        );
        assert!(ok.is_ok(), "{:?}", ok.err().map(|e| e.message));
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(!text.contains("duration-ms"), "{text}");
        assert!(text.contains("preset \"smooth\""), "{text}");
        assert!(!h.state.config.animations.custom());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A batch is one write: every edit lands, and subscribers hear exactly
    /// one `config` event. One bad edit refuses the lot and writes nothing.
    #[test]
    fn a_batch_emits_one_config_event() {
        let mut h = crate::shell::focus::state_tests::harness();
        let dir = std::env::temp_dir().join(format!("abyss-set-batch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("abyss.kdl");
        std::fs::write(
            &file,
            "animations {\n    window-open {\n        style \"zoom\"\n    }\n}\n",
        )
        .unwrap();
        h.state.config.explicit = Some(file.clone());
        let next = h.state.config.reload();
        crate::config::apply_loaded(&mut h.state, next);
        crate::ipc::capture::take();

        let ok = set_config_values(
            &mut h.state,
            Decision::Allow,
            &json!({"edits": [
                {"path": "animations.preset", "value": "lively"},
                {"path": "animations.speed", "value": 2.0},
                {"path": "animations.window-open.style", "value": null},
            ]}),
        );
        let events = crate::ipc::capture::take();
        assert!(ok.is_ok(), "{:?}", ok.err().map(|e| e.message));
        assert_eq!(
            events.iter().filter(|(k, _)| k == "config").count(),
            1,
            "{events:?}"
        );
        let a = &h.state.config.animations;
        assert_eq!(a.preset, crate::config::animations::Preset::Lively);
        assert!((a.speed - 2.0).abs() < f64::EPSILON);
        assert!(!a.custom());

        let before = std::fs::read_to_string(&file).unwrap();
        let bad = set_config_values(
            &mut h.state,
            Decision::Allow,
            &json!({"edits": [
                {"path": "animations.preset", "value": "subtle"},
                {"path": "animations.speed", "value": 9.0},
            ]}),
        );
        assert!(bad.is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before);
        assert!(crate::ipc::capture::take().iter().all(|(k, _)| k != "config"));

        // Empty, and denied, batches are refused before anything is read.
        assert!(set_config_values(&mut h.state, Decision::Allow, &json!({"edits": []})).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `get_config` carries every event resolved, so no client expands a
    /// preset itself.
    #[test]
    fn get_config_reports_resolved_animations() {
        let mut a = crate::config::Animations {
            preset: crate::config::animations::Preset::Smooth,
            ..Default::default()
        };
        a.overrides.insert(
            crate::config::animations::Event::WindowOpen,
            crate::config::animations::Override {
                style: Some("mypack:swirl".into()),
                ..Default::default()
            },
        );
        let v = animations_json(&a, &[]);
        assert_eq!(v["preset"], "smooth");
        assert_eq!(v["custom"], true);
        assert_eq!(v["reduce_motion"], false);
        assert_eq!(v["events"]["window-open"]["style"], "mypack:swirl");
        assert_eq!(v["events"]["window-open"]["overridden"], true);
        assert_eq!(v["events"]["window-close"]["overridden"], false);
        assert!(v["events"]["focus"]["duration-ms"].is_u64());
        let styles = v["styles"]["window-open"].as_array().unwrap();
        assert_eq!(styles.last().unwrap(), "mypack:swirl");
        assert_eq!(
            v["events"].as_object().unwrap().len(),
            crate::config::animations::Event::ALL.len()
        );
    }

    /// Installed transition styles are listed for the events they serve, after
    /// the built-ins, once.
    #[test]
    fn get_config_lists_installed_transition_styles() {
        use crate::config::animations::{Curve, Event};
        let style = ec_abyss_config::transitions::TransitionStyle {
            id: "fx:ripple".into(),
            label: "Ripple".into(),
            events: vec![Event::WindowClose],
            source: String::new(),
            margin: 0,
            duration_ms: 300,
            curve: Curve::EaseOut,
        };
        let v = animations_json(&crate::config::Animations::default(), &[style]);
        let close = v["styles"]["window-close"].as_array().unwrap();
        assert_eq!(close.last().unwrap(), "fx:ripple");
        let open = v["styles"]["window-open"].as_array().unwrap();
        assert!(!open.iter().any(|s| s == "fx:ripple"));
        assert_eq!(v["addon_labels"]["fx:ripple"], "Ripple");
    }

    /// A dry run runs the loader's check on the edited text: it reports
    /// `valid: false` exactly when the real write would be rolled back, and
    /// never touches the file. An empty `bar {}` is edited in place, so the
    /// dry run and the write both pass.
    #[test]
    fn a_dry_run_agrees_with_the_write() {
        let mut h = crate::shell::focus::state_tests::harness();
        let dir = std::env::temp_dir().join(format!("abyss-set-dry-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("abyss.kdl");
        h.state.config.explicit = Some(file.clone());
        let set = |h: &mut crate::shell::focus::state_tests::Harness, dry: bool| {
            set_config_value(
                &mut h.state,
                Decision::Allow,
                &json!({"path": "bar.rounding", "value": 6, "dry_run": dry}),
            )
        };

        // The loader rejects this file whatever the edit: dry says invalid,
        // the write is refused, the bytes stay.
        let rejected = "bar {\n    rounding 20\n}\nbogus-node 1\n";
        std::fs::write(&file, rejected).unwrap();
        let got = set(&mut h, true).ok().expect("a dry run answers");
        assert_eq!(got["applied"], json!(false));
        assert_eq!(got["valid"], json!(false));
        assert!(got["errors"][0]["line"].as_u64().unwrap() > 0, "{got}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), rejected);
        assert!(set(&mut h, false).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), rejected);

        for empty in ["bar {}\n", "bar {\n}\n"] {
            std::fs::write(&file, empty).unwrap();
            let got = set(&mut h, true).ok().expect("a dry run answers");
            assert_eq!(got["valid"], json!(true), "{got}");
            assert_eq!(got["errors"], json!([]));
            assert_eq!(std::fs::read_to_string(&file).unwrap(), empty);
            let got = set(&mut h, false).unwrap_or_else(|e| panic!("{}", e.message));
            assert_eq!(got["applied"], json!(true));
            assert_eq!(got["valid"], json!(true));
            assert_eq!(
                std::fs::read_to_string(&file).unwrap(),
                "bar {\n    rounding 6\n}\n"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `bar.eye` is served as a plain JSON bool, default `true`, and a write
    /// round-trips to `false` in the user file.
    #[test]
    fn bar_eye_is_served_as_a_bool() {
        let mut h = crate::shell::focus::state_tests::harness();
        let dir = std::env::temp_dir().join(format!("abyss-bar-eye-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("abyss.kdl");
        std::fs::write(&file, "bar {\n    rounding 20\n}\n").unwrap();
        h.state.config.explicit = Some(file.clone());

        let got = get_config(&mut h.state, Decision::Allow, &json!({"path": "bar.eye"}))
            .ok()
            .expect("bar.eye is readable");
        let row = &got["keys"][0];
        assert_eq!(row["path"], "bar.eye");
        assert_eq!(row["value"], json!(true));
        assert_eq!(row["default"], json!(true));

        let ok = set_config_value(
            &mut h.state,
            Decision::Allow,
            &json!({"path": "bar.eye", "value": false}),
        );
        assert!(ok.is_ok(), "{:?}", ok.err().map(|e| e.message));
        let got = get_config(&mut h.state, Decision::Allow, &json!({"path": "bar.eye"}))
            .ok()
            .expect("bar.eye is readable");
        assert_eq!(got["keys"][0]["value"], json!(false));
        assert!(std::fs::read_to_string(&file).unwrap().contains("eye #false"));

        assert!(set_config_value(
            &mut h.state,
            Decision::Allow,
            &json!({"path": "bar.eye", "value": "yes"}),
        )
        .is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `wallpaper.{path,mode,color}` round-trip through `set_config_value`
    /// without touching a per-output block's keys of the same name, and the
    /// overrides ride along under `collections."wallpaper.output"`.
    #[test]
    fn wallpaper_keys_round_trip_and_overrides_are_served() {
        use crate::shell::focus::state_tests::Harness;
        let mut h = crate::shell::focus::state_tests::harness();
        let dir = std::env::temp_dir().join(format!("abyss-wallpaper-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("abyss.kdl");
        std::fs::write(
            &file,
            "wallpaper {\n    output \"DP-1\" {\n        mode \"fit\"\n        color \"#112233\"\n    }\n}\n",
        )
        .unwrap();
        h.state.config.explicit = Some(file.clone());
        h.state.config = h.state.config.reload();

        let row = |h: &mut Harness, path: &str| {
            get_config(&mut h.state, Decision::Allow, &json!({"path": path}))
                .ok()
                .expect("readable")["keys"][0]
                .clone()
        };
        assert_eq!(row(&mut h, "wallpaper.path")["value"], Value::Null);
        assert_eq!(row(&mut h, "wallpaper.mode")["value"], json!("fill"));
        assert_eq!(row(&mut h, "wallpaper.color")["value"], json!("#0b0906ff"));
        assert_eq!(row(&mut h, "wallpaper.color")["default"], json!("#0b0906ff"));

        for (path, v) in [
            ("wallpaper.path", json!("~/Pictures/x.png")),
            ("wallpaper.mode", json!("center")),
            ("wallpaper.color", json!("#202122")),
        ] {
            let r = set_config_value(&mut h.state, Decision::Allow, &json!({"path": path, "value": v}));
            assert!(r.is_ok(), "{path}: {:?}", r.err().map(|e| e.message));
        }
        assert_eq!(row(&mut h, "wallpaper.path")["value"], json!("~/Pictures/x.png"));
        assert_eq!(row(&mut h, "wallpaper.mode")["value"], json!("center"));
        assert_eq!(row(&mut h, "wallpaper.color")["value"], json!("#202122ff"));
        assert!(set_config_value(
            &mut h.state,
            Decision::Allow,
            &json!({"path": "wallpaper.mode", "value": "stretch"}),
        )
        .is_err());

        let all = get_config(&mut h.state, Decision::Allow, &json!({}))
            .ok()
            .unwrap();
        assert_eq!(
            all["collections"]["wallpaper.output"],
            json!([{"output": "DP-1", "path": null, "mode": "fit", "color": "#112233ff"}])
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// D-07 §4: `setup.*` is readable, defaults are served, a write round-trips
    /// through the user file with its comments and neighbours intact, and a
    /// bad profile or an unknown sub-key is refused without touching the file.
    #[test]
    fn setup_keys_round_trip_and_refuse_junk() {
        use crate::shell::focus::state_tests::Harness;
        let mut h = crate::shell::focus::state_tests::harness();
        let dir = std::env::temp_dir().join(format!("abyss-setup-keys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("abyss.kdl");
        let start = "// hand-written\nbar {\n    rounding 20 // keep\n}\nsetup {\n    profile \"full\"\n    complete #true\n}\n";
        std::fs::write(&file, start).unwrap();
        h.state.config.explicit = Some(file.clone());
        h.state.config = h.state.config.reload();

        let row = |h: &mut Harness, path: &str| {
            let got = get_config(&mut h.state, Decision::Allow, &json!({"path": path}))
                .ok()
                .expect("readable");
            got["keys"][0].clone()
        };
        assert_eq!(row(&mut h, "setup.profile")["value"], json!("full"));
        assert_eq!(row(&mut h, "setup.profile")["default"], json!("standard"));
        assert_eq!(row(&mut h, "setup.complete")["value"], json!(true));
        assert_eq!(row(&mut h, "setup.pending-preset")["value"], json!(false));

        let set = |h: &mut Harness, path: &str, v: Value| {
            set_config_value(&mut h.state, Decision::Allow, &json!({"path": path, "value": v}))
        };
        assert!(set(&mut h, "setup.complete", json!(false)).is_ok());
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("// hand-written"), "{text}");
        assert!(text.contains("rounding 20 // keep"), "{text}");
        assert!(text.contains("profile \"full\""), "{text}");
        assert!(text.contains("complete #false"), "{text}");
        assert_eq!(row(&mut h, "setup.complete")["value"], json!(false));

        let before = std::fs::read_to_string(&file).unwrap();
        assert!(set(&mut h, "setup.profile", json!("bespoke")).is_err());
        assert!(set(&mut h, "setup.pending-preset", json!("yes")).is_err());
        assert!(set(&mut h, "setup.mode", json!("wm")).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before);

        // An unknown sub-key in a file is refused with file:line:col.
        let bad = validate_config(
            &mut h.state,
            Decision::Allow,
            &json!({"text": "setup {\n    complet #true\n}\n"}),
        )
        .ok()
        .expect("validate answers");
        assert_eq!(bad["valid"], json!(false));
        assert_eq!(bad["errors"][0]["line"], json!(2));
        assert_eq!(bad["errors"][0]["col"], json!(5));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `widget` blocks are served under `collections.widget` with every field
    /// present, and not at all on a filtered read.
    #[test]
    fn widget_blocks_are_served_as_a_collection() {
        let mut h = crate::shell::focus::state_tests::harness();
        let text = "bar {\n    widgets { order \"custom:cpu\" \"custom:weather\" \"clock\"; }\n    \
                    widget \"cpu\" { source \"usage.cpu\"; format \"{}%\"; }\n    \
                    widget \"weather\" { exec \"curl\" \"-s\" \"wttr.in\"; interval-ms \"10m\"; on-click \"xdg-open\" \"https://wttr.in\"; }\n}\n";
        let dir = std::env::temp_dir().join(format!("abyss-widget-coll-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("abyss.kdl");
        std::fs::write(&file, text).unwrap();
        h.state.config = Config::load(Some(&file));
        let _ = std::fs::remove_dir_all(&dir);
        let mine: Vec<_> = h.state.config.errors.iter().filter(|e| e.file == file).collect();
        assert!(mine.is_empty(), "{mine:?}");

        let got = get_config(&mut h.state, Decision::Allow, &json!({}))
            .ok()
            .expect("abyss is readable");
        assert_eq!(
            got["collections"]["widget"],
            json!([
                {"name": "cpu", "kind": "source", "exec": null, "interval-ms": null,
                 "source": "usage.cpu", "format": "{}%", "icon": null,
                 "on-click": null, "on-scroll-up": null, "on-scroll-down": null,
                 "approval": "approved", "premade": false},
                {"name": "weather", "kind": "exec", "exec": ["curl", "-s", "wttr.in"],
                 "interval-ms": 600000, "source": null, "format": null, "icon": null,
                 "on-click": ["xdg-open", "https://wttr.in"], "on-scroll-up": null,
                 "on-scroll-down": null, "approval": "approved", "premade": false},
            ])
        );
        let got = get_config(&mut h.state, Decision::Allow, &json!({"path": "bar.eye"}))
            .ok()
            .expect("bar.eye is readable");
        assert!(got.get("collections").is_none());
    }

    /// ADR 0066: without `taskbar-widgets`, `widget` blocks load without error
    /// but stay out of the live config (and so out of `collections.widget`),
    /// with their `custom:` ids; with the hook they are live.
    #[test]
    fn widget_blocks_are_inert_without_the_hook() {
        let text = "bar {\n    widgets { order \"custom:cpu\" \"clock\"; important \"custom:cpu\"; }\n    \
                    widget \"cpu\" { source \"usage.cpu\"; format \"{}%\"; }\n}\n";
        let (mut h, file) = widget_harness("inert", text);
        h.state.addons.hooks = crate::addons::HookSet::default();
        let next = Config::load(Some(&file));
        assert!(
            next.errors.iter().all(|e| e.file != file),
            "a widget block became an error"
        );
        crate::config::apply_loaded(&mut h.state, next);
        assert!(names(&h).is_empty());
        assert_eq!(h.state.config.bar.widgets.order, ["clock"]);
        assert!(h.state.config.bar.widgets.important.is_empty());
        let got = get_config(&mut h.state, Decision::Allow, &json!({}))
            .ok()
            .unwrap();
        assert_eq!(got["collections"]["widget"], json!([]));

        h.state.addons.hooks.insert(crate::addons::Hook::TaskbarWidgets);
        crate::config::apply_loaded(&mut h.state, Config::load(Some(&file)));
        assert_eq!(names(&h), ["cpu"]);
        assert_eq!(h.state.config.bar.widgets.order, ["custom:cpu", "clock"]);
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    /// `get_config` reports installed add-ons and the hooks that are on, on
    /// every reply shape.
    #[test]
    fn get_config_reports_addons() {
        let mut h = crate::shell::focus::state_tests::harness();
        let got = get_config(&mut h.state, Decision::Allow, &json!({}))
            .ok()
            .unwrap();
        assert_eq!(
            (got["addons"].clone(), got["hooks_on"].clone()),
            (json!([]), json!([]))
        );

        h.state.addons.manifests.push(crate::addons::Manifest {
            id: "oracle-eyes".into(),
            name: "Oracle Eyes".into(),
            hooks: vec!["annotations".into(), "region-select".into()],
            capture_requested: true,
        });
        h.state.addons.hooks.insert(crate::addons::Hook::Annotations);
        h.state.addons.hooks.insert(crate::addons::Hook::RegionSelect);
        for params in [json!({}), json!({"path": "bar.eye"})] {
            let got = get_config(&mut h.state, Decision::Allow, &params).ok().unwrap();
            assert_eq!(
                got["addons"],
                json!([{"id": "oracle-eyes", "name": "Oracle Eyes",
                        "hooks": ["annotations", "region-select"], "capture_requested": true}])
            );
            assert_eq!(got["hooks_on"], json!(["annotations", "region-select"]));
        }
    }

    /// A harness whose config is `text` in a scratch file, loaded as `--config`.
    fn widget_harness(
        tag: &str,
        text: &str,
    ) -> (crate::shell::focus::state_tests::Harness, std::path::PathBuf) {
        let mut h = crate::shell::focus::state_tests::harness();
        // The collection only exists with its add-on hook on (ADR 0066).
        h.state.addons.hooks.insert(crate::addons::Hook::TaskbarWidgets);
        let dir = std::env::temp_dir().join(format!("abyss-coll-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("abyss.kdl");
        std::fs::write(&file, text).unwrap();
        h.state.config = Config::load(Some(&file));
        let mine: Vec<_> = h.state.config.errors.iter().filter(|e| e.file == file).collect();
        assert!(mine.is_empty(), "{mine:?}");
        (h, file)
    }

    fn coll(h: &mut crate::shell::focus::state_tests::Harness, params: Value) -> Reply {
        let mut p = params;
        p["collection"] = json!("widget");
        set_config_collection(&mut h.state, Decision::Allow, &p)
    }

    fn names(h: &crate::shell::focus::state_tests::Harness) -> Vec<String> {
        h.state
            .config
            .bar
            .custom_widgets
            .iter()
            .map(|w| w.name.clone())
            .collect()
    }

    const WIDGETS: &str = "// mine\nbar {\n    widget \"cpu\" { source \"usage.cpu\"; format \"{}%\"; } // c\n    \
                           widgets {\n        order \"custom:cpu\" \"clock\"\n        important \"custom:cpu\" \"clock\"\n    }\n}\n";

    #[test]
    fn widget_upsert_adds_and_replaces_through_the_real_load() {
        let (mut h, file) = widget_harness("upsert", WIDGETS);
        crate::ipc::capture::take();
        let entry = json!({"name": "weather", "kind": "exec", "exec": ["curl", "-s", "wttr.in"],
            "interval-ms": 600000, "source": null, "format": null, "icon": "weather-clear",
            "on-click": ["xdg-open", "https://wttr.in"], "on-scroll-up": null, "on-scroll-down": null});
        let got = coll(&mut h, json!({"op": "upsert", "name": "weather", "entry": entry}))
            .unwrap_or_else(|e| panic!("{}", e.message));
        assert_eq!(got["applied"], json!(true));
        assert_eq!(got["previous"], Value::Null);
        assert_eq!(
            crate::ipc::capture::take()
                .iter()
                .filter(|(k, _)| k == "config")
                .count(),
            1
        );
        // ADR 0067: a new command widget is written but withheld.
        assert_eq!(names(&h), ["cpu"]);
        let pending = h.state.widget_approvals.queue[0].clone();
        assert_eq!(
            (pending.name.as_str(), pending.kind),
            ("weather", crate::config::withhold::PendingKind::New)
        );
        // The owner approves it (stand-in for the Trusted UI surface).
        let store = file.parent().unwrap().join("approvals.kdl");
        crate::config::approvals::set_test_path(Some(store));
        crate::config::approvals::record_approval("weather", pending.hash).unwrap();
        crate::config::withhold::reapply(&mut h.state);
        assert_eq!(names(&h), ["cpu", "weather"]);
        assert!(h.state.widget_approvals.queue.is_empty());
        // get_config serves back exactly what was written.
        let read = get_config(&mut h.state, Decision::Allow, &json!({}))
            .ok()
            .unwrap();
        let mut served = entry.clone();
        served["approval"] = json!("approved");
        served["premade"] = json!(false);
        assert_eq!(read["collections"]["widget"][1], served);
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.starts_with(
            "// mine\nbar {\n    widget \"cpu\" { source \"usage.cpu\"; format \"{}%\"; } // c\n"
        ));
        assert!(text.contains("    widget \"weather\" {\n        exec \"curl\" \"-s\" \"wttr.in\"\n        interval-ms 600000\n"));

        // Replace, as a stream; previous is the old entry.
        let got = coll(
            &mut h,
            json!({"op": "upsert", "name": "weather", "entry": {"kind": "stream", "exec": ["tail", "-f", "x"]}}),
        )
        .unwrap_or_else(|e| panic!("{}", e.message));
        assert_eq!(got["previous"]["kind"], json!("exec"));
        // A changed definition is a new hash: withheld again until approved.
        assert_eq!(names(&h), ["cpu"]);
        assert_eq!(h.state.widget_approvals.withheld[0].1.name, "weather");
        crate::config::approvals::set_test_path(None);
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    #[test]
    fn widget_remove_drops_its_references_in_the_same_write() {
        let (mut h, file) = widget_harness("remove", WIDGETS);
        let got =
            coll(&mut h, json!({"op": "remove", "name": "cpu"})).unwrap_or_else(|e| panic!("{}", e.message));
        assert_eq!(
            got["references"],
            json!(["bar.widgets.order", "bar.widgets.important"])
        );
        assert!(names(&h).is_empty());
        assert_eq!(h.state.config.bar.widgets.order, ["clock"]);
        assert_eq!(h.state.config.bar.widgets.important, ["clock"]);
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.starts_with("// mine\nbar {\n    widgets {\n"), "{text}");
        // Not there any more: a typo-class error, nothing written.
        let e = coll(&mut h, json!({"op": "remove", "name": "cpu"})).unwrap_err();
        assert_eq!(e.code, super::super::INVALID_PARAMS);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    #[test]
    fn widget_rename_rewrites_its_references() {
        let (mut h, file) = widget_harness("rename", WIDGETS);
        let got = coll(&mut h, json!({"op": "rename", "name": "cpu", "new_name": "load"}))
            .unwrap_or_else(|e| panic!("{}", e.message));
        assert_eq!(
            got["references"],
            json!(["bar.widgets.order", "bar.widgets.important"])
        );
        assert_eq!(names(&h), ["load"]);
        assert_eq!(h.state.config.bar.widgets.order, ["custom:load", "clock"]);
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("widget \"load\" { source \"usage.cpu\"; format \"{}%\"; } // c\n"));
        // Onto an existing name: refused.
        coll(
            &mut h,
            json!({"op": "upsert", "name": "x", "entry": {"kind": "exec", "exec": ["true"]}}),
        )
        .unwrap_or_else(|e| panic!("{}", e.message));
        assert!(coll(&mut h, json!({"op": "rename", "name": "x", "new_name": "load"})).is_err());
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    #[test]
    fn widget_move_reorders_the_collection() {
        let (mut h, file) = widget_harness("move", WIDGETS);
        coll(
            &mut h,
            json!({"op": "upsert", "name": "a", "entry": {"kind": "source", "source": "usage.cpu"}}),
        )
        .unwrap_or_else(|e| panic!("{}", e.message));
        assert_eq!(names(&h), ["cpu", "a"]);
        coll(&mut h, json!({"op": "move", "name": "a", "index": 0}))
            .unwrap_or_else(|e| panic!("{}", e.message));
        assert_eq!(names(&h), ["a", "cpu"]);
        let e = coll(&mut h, json!({"op": "move", "name": "a", "index": 5})).unwrap_err();
        assert!(e.message.contains("out of range"), "{}", e.message);
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    /// An entry the parser refuses is rolled back: the file is byte-identical,
    /// the live config unchanged, no `config` event, and the error is the
    /// positioned text a hand edit would have produced. A dry run reports the
    /// same error without writing.
    #[test]
    fn an_invalid_widget_is_rolled_back() {
        let (mut h, file) = widget_harness("rollback", WIDGETS);
        crate::ipc::capture::take();
        let bad = json!({"op": "upsert", "name": "cpu", "entry": {"kind": "source", "source": "usage.cpuu"}});
        let e = coll(&mut h, bad.clone()).unwrap_err();
        assert_eq!(e.code, super::super::INVALID_PARAMS);
        assert!(e.message.contains("abyss.kdl:"), "{}", e.message);
        assert!(e.message.contains("did you mean \"usage.cpu\""), "{}", e.message);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), WIDGETS);
        assert!(crate::ipc::capture::take().iter().all(|(k, _)| k != "config"));
        assert_eq!(names(&h), ["cpu"]);

        let mut dry = bad;
        dry["dry_run"] = json!(true);
        let got = coll(&mut h, dry).ok().expect("a dry run answers");
        assert_eq!(got["applied"], json!(false));
        assert_eq!(got["valid"], json!(false));
        assert!(got["errors"][0]["line"].as_u64().unwrap() > 0);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), WIDGETS);

        // Shape errors never reach the file at all.
        for entry in [
            json!({"kind": "exec"}),
            json!({"kind": "exec", "exec": []}),
            json!({"kind": "exec", "exec": ["x"], "source": "usage.cpu"}),
            json!({"kind": "stream", "exec": ["x"], "interval-ms": 5}),
            json!({"kind": "source"}),
            json!({"kind": "nope"}),
            json!({"kind": "exec", "exec": ["x"], "colour": "red"}),
            json!({"name": "other", "kind": "exec", "exec": ["x"]}),
        ] {
            assert!(
                coll(&mut h, json!({"op": "upsert", "name": "cpu", "entry": entry})).is_err(),
                "{entry}"
            );
        }
        assert_eq!(std::fs::read_to_string(&file).unwrap(), WIDGETS);
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    /// The collection write takes the same two gates as `set_config_value`:
    /// a denied outer decision is never loosened, and nothing is written.
    #[test]
    fn the_collection_write_is_gated() {
        let (mut h, file) = widget_harness("gate", WIDGETS);
        let e = set_config_collection(
            &mut h.state,
            Decision::Deny("peer uid is not the session owner"),
            &json!({"collection": "widget", "op": "remove", "name": "cpu"}),
        )
        .unwrap_err();
        assert_eq!(e.code, super::super::DENIED);
        let body: Value = serde_json::from_str(&e.message).expect("structured denial");
        assert_eq!(body["file"], "abyss");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), WIDGETS);
        // Unknown collection: a typo. Known but not writable yet: refused.
        for c in ["widgets", "bind"] {
            let e = set_config_collection(
                &mut h.state,
                Decision::Allow,
                &json!({"collection": c, "op": "remove", "name": "cpu"}),
            )
            .unwrap_err();
            assert_eq!(e.code, super::super::INVALID_PARAMS, "{c}");
        }
        let row = gate::lookup("set_config_collection").expect("row");
        assert_eq!(row.kind, gate::Kind::Command);
        assert!(row.implemented);
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    // ------------------------------------------------ command approval (ADR 0067)

    const PREMADE: &str = "bar {\n    widget \"load\" { exec \"cut\" \"-d\" \" \" \"-f1\" \"/proc/loadavg\"; interval-ms 5000; }\n}\n";

    /// A harness with a scratch catalog holding [`PREMADE`], a scratch
    /// approvals store, the hook on, and the owner's file `text`, applied
    /// through the inotify reload path (`watch::reload_now`).
    fn approval_harness(
        tag: &str,
        text: &str,
    ) -> (crate::shell::focus::state_tests::Harness, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("abyss-approve-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("widgets")).unwrap();
        std::fs::write(dir.join("widgets/premade.kdl"), PREMADE).unwrap();
        crate::config::catalog::set_test_dir(Some(dir.join("widgets")));
        crate::config::approvals::set_test_path(Some(dir.join("approvals.kdl")));
        let file = dir.join("abyss.kdl");
        std::fs::write(&file, text).unwrap();
        let mut h = crate::shell::focus::state_tests::harness();
        h.state.addons.hooks.insert(crate::addons::Hook::TaskbarWidgets);
        h.state.config = Config::load_with(Some(&file), true);
        let mine: Vec<_> = h.state.config.errors.iter().filter(|e| e.file == file).collect();
        assert!(mine.is_empty(), "{mine:?}");
        crate::config::withhold::reapply(&mut h.state);
        (h, file)
    }

    fn approval_done(file: &std::path::Path) {
        crate::config::catalog::set_test_dir(None);
        crate::config::approvals::set_test_path(None);
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    fn served(h: &mut crate::shell::focus::state_tests::Harness) -> Value {
        get_config(&mut h.state, Decision::Allow, &json!({}))
            .ok()
            .unwrap()["collections"]["widget"]
            .clone()
    }

    fn rewrite(h: &mut crate::shell::focus::state_tests::Harness, file: &std::path::Path, text: &str) {
        std::fs::write(file, text).unwrap();
        crate::config::withhold::reapply(&mut h.state);
    }

    #[test]
    fn a_catalog_block_is_live_without_an_approval() {
        let (mut h, file) = approval_harness(
            "premade",
            "bar {\n    widgets { order \"custom:load\" \"clock\"; }\n}\n",
        );
        assert_eq!(names(&h), ["load"]);
        assert!(h.state.widget_approvals.queue.is_empty());
        assert_eq!(h.state.config.bar.widgets.order, ["custom:load", "clock"]);
        let w = served(&mut h);
        assert_eq!(w[0]["approval"], json!("approved"));
        assert_eq!(w[0]["premade"], json!(true));
        assert_eq!(w[0]["exec"][0], json!("cut"));
        approval_done(&file);
    }

    #[test]
    fn an_edited_premade_is_withheld_until_approved_and_again_after_an_edit() {
        let altered = "bar {\n    widgets { order \"custom:load\"; }\n    \
                       widget \"load\" { exec \"sh\" \"-c\" \"secret-argv\"; interval-ms 5000; }\n}\n";
        let (mut h, file) = approval_harness("altered", altered);
        assert!(names(&h).is_empty());
        // The id stays and draws nothing.
        assert_eq!(h.state.config.bar.widgets.order, ["custom:load"]);
        let q = &h.state.widget_approvals.queue;
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].kind, crate::config::withhold::PendingKind::Altered);
        assert_eq!(q[0].command_text, "sh -c secret-argv");
        let hash = q[0].hash;
        let w = served(&mut h);
        assert_eq!(
            w,
            json!([{"name": "load", "approval": "pending", "premade": false, "altered": true}])
        );
        // A withheld widget's argv never reaches get_config.
        let all = get_config(&mut h.state, Decision::Allow, &json!({}))
            .ok()
            .unwrap();
        assert!(!all.to_string().contains("secret-argv"));

        // A store entry for exactly this hash makes it live.
        crate::config::approvals::record_approval("load", hash).unwrap();
        crate::config::withhold::reapply(&mut h.state);
        assert_eq!(names(&h), ["load"]);
        assert!(h.state.widget_approvals.queue.is_empty());
        let w = served(&mut h);
        assert_eq!(
            (w[0]["approval"].clone(), w[0]["premade"].clone()),
            (json!("approved"), json!(false))
        );

        // A further edit is a new hash: withheld again.
        rewrite(&mut h, &file, &altered.replace("secret-argv", "other"));
        assert!(names(&h).is_empty());
        assert_eq!(h.state.widget_approvals.queue.len(), 1);
        assert_ne!(h.state.widget_approvals.queue[0].hash, hash);

        // Revert drops the owner's block; the catalog block is live again.
        revert_widget(&mut h.state, "load").unwrap_or_else(|e| panic!("{}", e.message));
        assert_eq!(names(&h), ["load"]);
        assert!(h.state.widget_approvals.queue.is_empty());
        assert_eq!(served(&mut h)[0]["premade"], json!(true));
        assert!(!std::fs::read_to_string(&file)
            .unwrap()
            .contains("widget \"load\""));
        assert_eq!(h.state.config.bar.widgets.order, ["custom:load"]);
        approval_done(&file);
    }

    /// The `set_config_collection` commit path withholds exactly as the
    /// reload path does; Not now holds across reloads; `review_widget`
    /// re-queues once and never stacks.
    #[test]
    fn a_new_command_widget_is_withheld_and_review_cannot_stack_prompts() {
        use crate::config::withhold::{self, PendingKind};
        let (mut h, file) = approval_harness("new", WIDGETS);
        coll(
            &mut h,
            json!({"op": "upsert", "name": "x", "entry": {"kind": "exec", "exec": ["hidden-argv"]}}),
        )
        .unwrap_or_else(|e| panic!("{}", e.message));
        assert_eq!(names(&h), ["load", "cpu"]);
        assert!(!served(&mut h).to_string().contains("hidden-argv"));
        assert_eq!(h.state.widget_approvals.queue.len(), 1);
        assert_eq!(h.state.widget_approvals.queue[0].kind, PendingKind::New);

        // One prompt at a time; review while it is on screen queues nothing.
        let shown = withhold::next_pending(&mut h.state).expect("queued");
        assert_eq!(shown.name, "x");
        assert!(withhold::next_pending(&mut h.state).is_none());
        let r = dispatch(
            &mut h.state,
            Decision::Allow,
            "review_widget",
            &json!({"name": "x"}),
        )
        .unwrap_or_else(|e| panic!("{}", e.message));
        assert_eq!(r, json!({"name": "x", "queued": false}));
        assert!(h.state.widget_approvals.queue.is_empty());

        // Not now (what the surface does), then a reload: not re-queued.
        h.state.widget_approvals.declined.insert(("x".into(), shown.hash));
        withhold::prompt_closed(&mut h.state);
        crate::config::withhold::reapply(&mut h.state);
        assert!(h.state.widget_approvals.queue.is_empty());
        assert!(withhold::is_declined(&h.state, "x", &shown.hash));
        assert!(withhold::next_pending(&mut h.state).is_none());

        // review_widget re-queues it once; asking again does not stack.
        let r = dispatch(
            &mut h.state,
            Decision::Allow,
            "review_widget",
            &json!({"name": "x"}),
        )
        .ok()
        .unwrap();
        assert_eq!(r["queued"], json!(true));
        assert!(!withhold::is_declined(&h.state, "x", &shown.hash));
        for _ in 0..3 {
            let r = dispatch(
                &mut h.state,
                Decision::Allow,
                "review_widget",
                &json!({"name": "x"}),
            )
            .ok()
            .unwrap();
            assert_eq!(r["queued"], json!(false));
        }
        assert_eq!(h.state.widget_approvals.queue.len(), 1);

        // Errors: unknown name, a live widget, a missing name.
        for (params, needle) in [
            (json!({"name": "nope"}), "no widget named"),
            (json!({"name": "load"}), "not waiting on approval"),
            (json!({"name": "cpu"}), "not waiting on approval"),
            (json!({}), "name must be"),
        ] {
            let e = dispatch(&mut h.state, Decision::Allow, "review_widget", &params).unwrap_err();
            assert_eq!(e.code, super::super::INVALID_PARAMS);
            assert!(e.message.contains(needle), "{}", e.message);
        }

        // Remove drops the block and its ids; nothing is left to prompt for.
        remove_widget(&mut h.state, "x").unwrap_or_else(|e| panic!("{}", e.message));
        assert!(h.state.widget_approvals.withheld.is_empty());
        assert!(h.state.widget_approvals.queue.is_empty());
        assert!(!std::fs::read_to_string(&file).unwrap().contains("hidden-argv"));
        approval_done(&file);
    }

    /// With `taskbar-widgets` off the catalog is not read, nothing is queued,
    /// and `widget` blocks are dropped with their ids as before ADR 0067.
    #[test]
    fn without_the_hook_the_catalog_is_not_read() {
        let text =
            "bar {\n    widgets { order \"custom:x\" \"clock\"; }\n    widget \"x\" { exec \"true\"; }\n}\n";
        let (mut h, file) = approval_harness("off", text);
        h.state.addons.hooks = crate::addons::HookSet::default();
        let next = Config::load_with(Some(&file), false);
        assert!(next.catalog.is_empty());
        assert!(next.errors.iter().all(|e| e.file != file));
        crate::config::apply_loaded(&mut h.state, next);
        assert!(names(&h).is_empty());
        assert_eq!(h.state.config.bar.widgets.order, ["clock"]);
        assert!(h.state.widget_approvals.queue.is_empty());
        assert!(h.state.widget_approvals.withheld.is_empty());
        assert_eq!(served(&mut h), json!([]));
        approval_done(&file);
    }

    #[test]
    fn writes_land_in_the_user_tier_not_etc() {
        let Some(base) = crate::config::user_config_base() else {
            return; // no HOME in this environment; nothing to assert
        };
        let mut cfg = Config::default();
        cfg.sources.push(crate::config::Source {
            path: std::path::PathBuf::from("/etc/eclipse/abyss.kdl"),
            owner: schema::Owner::Abyss,
        });
        assert_eq!(
            target_path(&cfg, schema::Owner::Abyss),
            Some(base.join("eclipse/abyss.kdl"))
        );
        // `--config` names the file to edit and wins over the search path.
        cfg.explicit = Some(std::path::PathBuf::from("/tmp/explicit.kdl"));
        assert_eq!(
            target_path(&cfg, schema::Owner::Abyss),
            Some(std::path::PathBuf::from("/tmp/explicit.kdl"))
        );
    }

    /// A withheld model command is served as null with `approval: "pending"`;
    /// the command text itself never reaches the socket before approval.
    #[test]
    fn a_withheld_model_command_is_served_as_null_and_pending() {
        let mut h = crate::shell::focus::state_tests::harness();
        let path = "oracle-eyes.model-command";
        let got = get_config(&mut h.state, Decision::Allow, &json!({ "path": path }))
            .ok()
            .expect("readable");
        assert_eq!(got["keys"][0]["value"], json!(["claude"]));
        assert_eq!(got["keys"][0]["approval"], "approved");

        h.state.config.oracle_eyes.model_command = None;
        h.state.widget_approvals.model_command.withheld =
            Some(crate::config::withhold::PendingModelCommand {
                command_text: "sh -c secret".into(),
                hash: crate::config::widget_hash::model_command_hash(&["sh".into()]),
            });
        let got = get_config(&mut h.state, Decision::Allow, &json!({ "path": path }))
            .ok()
            .expect("readable");
        assert_eq!(got["keys"][0]["value"], Value::Null);
        assert_eq!(got["keys"][0]["approval"], "pending");
        assert!(!got.to_string().contains("secret"));

        // Other rows carry no approval field.
        let got = get_config(
            &mut h.state,
            Decision::Allow,
            &json!({ "path": "oracle-eyes.hold-ms" }),
        )
        .ok()
        .expect("readable");
        assert!(got["keys"][0].get("approval").is_none());
    }

    #[test]
    fn revert_model_command_writes_the_default_back() {
        let mut h = crate::shell::focus::state_tests::harness();
        let dir = std::env::temp_dir().join(format!("abyss-oe-revert-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("abyss.kdl");
        std::fs::write(
            &file,
            "oracle-eyes {\n    model-command \"other\" \"--x\"\n    hold-ms 5000\n}\n",
        )
        .unwrap();
        h.state.config.explicit = Some(file.clone());
        let ok = revert_model_command(&mut h.state);
        assert!(ok.is_ok(), "{:?}", ok.err().map(|e| e.message));
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("model-command"), "{text}");
        assert!(!text.contains("other"), "{text}");
        assert!(text.contains("hold-ms 5000"), "{text}");
        assert_eq!(
            h.state.config.oracle_eyes.model_command,
            Some(vec!["claude".to_owned()])
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
