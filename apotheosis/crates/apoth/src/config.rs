// SPDX-License-Identifier: AGPL-3.0-only
//! `apoth config …` (T-10 §8, T-12 §5): reads and edits the user file locally, no socket.
//!
//! Exit codes: 0 ok, 1 error (including validation errors), 2 usage. Values are never
//! logged; output is the command's result only.

use apoth_config::diag::suggest;
use apoth_config::edit::{Editor, NodeSel, write_target};
use apoth_config::fs::Fs;
use apoth_config::load::{LoadResult, default_input, load};
use apoth_config::model::{Config, Layer};
use apoth_config::overrides::{parse_override_value, value_fits};
use apoth_config::schema::{self, TABLE};
use apoth_config::types::{Key, Value};
use apoth_config::value::render_node;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

pub(crate) const OK: i32 = 0;
pub(crate) const ERROR: i32 = 1;
pub(crate) const USAGE: i32 = 2;

pub(crate) struct Ctx<'a> {
    pub(crate) fs: &'a dyn Fs,
    pub(crate) home: PathBuf,
    pub(crate) xdg: Option<PathBuf>,
    pub(crate) out: &'a mut dyn Write,
    pub(crate) err: &'a mut dyn Write,
}

#[derive(Default)]
struct Opts {
    json: bool,
    origin: bool,
    changed_only: bool,
    dry_run: bool,
    profile: Option<String>,
    pos: Vec<String>,
}

fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts::default();
    let mut it = args.iter();
    let mut rest_positional = false;
    while let Some(a) = it.next() {
        let negative_number = a.len() > 1
            && a.starts_with('-')
            && a[1..].starts_with(|c: char| c.is_ascii_digit() || c == '.');
        if rest_positional || !a.starts_with('-') || a == "-" || negative_number {
            o.pos.push(a.clone());
            continue;
        }
        match a.as_str() {
            "--" => rest_positional = true,
            "--json" => o.json = true,
            "--origin" => o.origin = true,
            "--changed-only" => o.changed_only = true,
            "--dry-run" => o.dry_run = true,
            "--profile" => {
                o.profile = Some(it.next().ok_or("--profile needs a name")?.clone());
            }
            other => return Err(format!("unknown option `{other}`")),
        }
    }
    Ok(o)
}

fn jstr(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn jarr(items: &[String]) -> String {
    format!(
        "[{}]",
        items.iter().map(|s| jstr(s)).collect::<Vec<_>>().join(",")
    )
}

fn value_json(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) if f.is_finite() => format!("{f:?}"),
        Value::Float(_) => "null".into(),
        Value::Str(s) => jstr(s),
        Value::List(l) => jarr(l),
        Value::Color(c) => jstr(&apoth_config::lang::format_color(*c)),
        Value::Map(m) => format!(
            "{{{}}}",
            m.iter()
                .map(|(k, a)| format!("{}:{}", jstr(k), jarr(a)))
                .collect::<Vec<_>>()
                .join(",")
        ),
    }
}

/// The value as it would be typed after the key in the file (`12.5`, `#true`, `"a" "b"`).
fn value_text(key: &Key, v: &Value) -> String {
    match v {
        Value::Null => "(unset)".into(),
        Value::Map(_) => render_node("map", &key.ty, v, 0)
            .unwrap_or_default()
            .trim_end()
            .to_string(),
        _ => render_node("k", &key.ty, v, 0)
            .and_then(|s| s.trim_end().strip_prefix("k ").map(str::to_string))
            .unwrap_or_default(),
    }
}

fn origin_json(cfg: &Config, path: &str) -> String {
    match cfg.origin(path) {
        Some(o) => format!(
            "{{\"layer\":{},\"file\":{},\"line\":{}}}",
            jstr(o.layer.name()),
            o.file
                .as_ref()
                .map(|f| jstr(&f.display().to_string()))
                .unwrap_or_else(|| "null".into()),
            o.line
        ),
        None => "null".into(),
    }
}

fn say(ctx: &mut Ctx, s: &str) {
    let _ = writeln!(ctx.out, "{s}");
}

fn fail(ctx: &mut Ctx, json: bool, code: i32, msg: &str) -> i32 {
    if json {
        let _ = writeln!(ctx.out, "{{\"ok\":false,\"error\":{}}}", jstr(msg));
    } else {
        let _ = writeln!(ctx.err, "apoth: {msg}");
    }
    code
}

fn usage(ctx: &mut Ctx, json: bool, msg: &str) -> i32 {
    fail(ctx, json, USAGE, msg)
}

/// `_`, `-` and `.` are one separator for key aliases (`font_size`, `font-size`).
fn fold(name: &str) -> String {
    name.chars()
        .map(|c| if matches!(c, '_' | '-') { '.' } else { c })
        .collect()
}

/// A key by its schema path or an alias of it: `_` and `-` match any separator of the path
/// (`font_size` and `font-size` are `font.size`). An alias must match exactly one key. An
/// unknown name gets the nearest key as a suggestion.
pub(crate) fn lookup(name: &str) -> Result<&'static Key, String> {
    if let Some(k) = schema::key(name) {
        return Ok(k);
    }
    if schema::is_container(name) {
        return Err(format!(
            "`{name}` is a section, not a key; use `list {name}`"
        ));
    }
    let folded = fold(name);
    let hits: Vec<&'static Key> = TABLE.iter().filter(|k| fold(k.path) == folded).collect();
    match hits.as_slice() {
        [k] => return Ok(k),
        [] => {}
        many => {
            let names: Vec<&str> = many.iter().map(|k| k.path).collect();
            return Err(format!(
                "`{name}` is ambiguous: it could be `{}`",
                names.join("`, `")
            ));
        }
    }
    let all: Vec<String> = TABLE.iter().map(|k| fold(k.path)).collect();
    let near = suggest(&folded, &all)
        .and_then(|f| TABLE.iter().find(|k| fold(k.path) == f))
        .map(|k| k.path);
    Err(match near {
        Some(s) => format!("unknown key `{name}` (did you mean `{s}`?)"),
        None => format!("unknown key `{name}`"),
    })
}

/// The `key=value` pairs of `apoth set` / `apoth config set`: every argument is `key=value`,
/// or the single-pair form `key value...` (the rest joined by spaces). The split is at the
/// first `=`, so a value may hold `=`. A key named twice is an error.
pub(crate) fn parse_pairs(pos: &[String]) -> Result<Vec<(String, String)>, String> {
    let Some(first) = pos.first() else {
        return Err("needs key=value".into());
    };
    let pairs: Vec<(String, String)> = if first.contains('=') {
        let mut v = Vec::with_capacity(pos.len());
        for a in pos {
            let Some((k, val)) = a.split_once('=') else {
                return Err(format!(
                    "`{a}` is not key=value (quote a value that has spaces)"
                ));
            };
            v.push((k.to_owned(), val.to_owned()));
        }
        v
    } else if pos.len() >= 2 {
        vec![(first.clone(), pos[1..].join(" "))]
    } else {
        return Err(format!("`{first}` needs a value: {first}=VALUE"));
    };
    for (i, (k, _)) in pairs.iter().enumerate() {
        if k.is_empty() {
            return Err("a pair has an empty key".into());
        }
        if pairs[..i].iter().any(|(p, _)| p == k) {
            return Err(format!("`{k}` is given twice"));
        }
    }
    Ok(pairs)
}

/// One pair checked against the schema.
#[derive(Debug)]
pub(crate) struct Checked {
    pub(crate) key: &'static Key,
    pub(crate) text: String,
    pub(crate) value: Value,
}

/// Check every pair before anything is written: the key resolves, the value parses and fits
/// the key's type. All problems are returned, one line each.
pub(crate) fn check_pairs(pairs: &[(String, String)]) -> Result<Vec<Checked>, Vec<String>> {
    let mut ok = Vec::with_capacity(pairs.len());
    let mut bad = Vec::new();
    for (name, text) in pairs {
        let key = match lookup(name) {
            Ok(k) => k,
            Err(m) => {
                bad.push(m);
                continue;
            }
        };
        if ok.iter().any(|c: &Checked| c.key.path == key.path) {
            bad.push(format!("`{}` is given twice", key.path));
            continue;
        }
        match parse_override_value(key, text) {
            Ok(v) if v != Value::Null && !value_fits(&key.ty, &v) => {
                bad.push(format!("{}: value out of range", key.path));
            }
            Ok(value) => ok.push(Checked {
                key,
                text: text.clone(),
                value,
            }),
            Err(m) => bad.push(format!("{}: {m}", key.path)),
        }
    }
    if bad.is_empty() { Ok(ok) } else { Err(bad) }
}

fn user_path(ctx: &Ctx) -> PathBuf {
    default_input(ctx.fs, &ctx.home, ctx.xdg.as_deref())
        .user
        .unwrap_or_else(|| ctx.home.join(".config/eclipse/apotheosis.kdl"))
}

fn load_cfg(ctx: &Ctx) -> LoadResult {
    load(
        ctx.fs,
        &default_input(ctx.fs, &ctx.home, ctx.xdg.as_deref()),
        None,
    )
}

pub(crate) fn run(args: &[String], ctx: &mut Ctx) -> i32 {
    let Some((verb, rest)) = args.split_first() else {
        return usage(
            ctx,
            false,
            "usage: apoth config <validate|show|get|set|unset|add|remove|explain|diff|migrate|import|path|edit> [--json] ...",
        );
    };
    let o = match parse_opts(rest) {
        Ok(o) => o,
        Err(m) => return usage(ctx, rest.iter().any(|a| a == "--json"), &m),
    };
    match verb.as_str() {
        "path" => cmd_path(ctx, &o),
        "validate" => cmd_validate(ctx, &o),
        "get" => cmd_get(ctx, &o),
        "list" | "show" => cmd_list(ctx, &o, o.pos.first().map(String::as_str)),
        "set" => cmd_set(ctx, &o),
        "unset" => cmd_unset(ctx, &o),
        "add" | "remove" => cmd_collection(ctx, &o, verb == "add"),
        "explain" => cmd_explain(ctx, &o),
        "diff" => cmd_diff(ctx, &o),
        "migrate" => cmd_migrate(ctx, &o),
        "edit" => cmd_edit(ctx, &o, &editor_command()),
        "import-foot" => cmd_import_foot(ctx, &o, o.pos.first().map(String::as_str)),
        "import" => match o.pos.first().map(String::as_str) {
            Some("foot") => cmd_import_foot(ctx, &o, o.pos.get(1).map(String::as_str)),
            Some(other) => usage(
                ctx,
                o.json,
                &format!("importer `{other}` is not available (only foot)"),
            ),
            None => usage(ctx, o.json, "usage: apoth config import foot [file]"),
        },
        other => usage(ctx, o.json, &format!("unknown config command `{other}`")),
    }
}

fn cmd_path(ctx: &mut Ctx, o: &Opts) -> i32 {
    let input = default_input(ctx.fs, &ctx.home, ctx.xdg.as_deref());
    let user = input.user.clone().unwrap_or_default();
    let system = input.system.clone().unwrap_or_default();
    if o.json {
        let fragments: Vec<String> = input
            .fragments
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        let profiles: Vec<String> = input
            .profile_files
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        let line = format!(
            "{{\"ok\":true,\"user\":{},\"system\":{},\"fragments\":{},\"profiles\":{}}}",
            jstr(&user.display().to_string()),
            jstr(&system.display().to_string()),
            jarr(&fragments),
            jarr(&profiles)
        );
        say(ctx, &line);
    } else {
        say(ctx, &user.display().to_string());
    }
    OK
}

fn cmd_validate(ctx: &mut Ctx, o: &Opts) -> i32 {
    let res = load_cfg(ctx);
    let errors = res.error_count();
    let warnings = res.diagnostics.len() - errors;
    if o.json {
        let diags: Vec<String> = res
            .diagnostics
            .iter()
            .map(|d| {
                format!(
                    "{{\"severity\":{},\"file\":{},\"line\":{},\"column\":{},\"key\":{},\"message\":{},\"suggestion\":{}}}",
                    jstr(if d.is_error() { "error" } else { "warning" }),
                    d.file
                        .as_ref()
                        .map(|f| jstr(&f.display().to_string()))
                        .unwrap_or_else(|| "null".into()),
                    d.line,
                    d.column,
                    d.key.as_deref().map(jstr).unwrap_or_else(|| "null".into()),
                    jstr(&d.message),
                    d.suggestion
                        .as_deref()
                        .map(jstr)
                        .unwrap_or_else(|| "null".into()),
                )
            })
            .collect();
        let line = format!(
            "{{\"ok\":{},\"errors\":{errors},\"warnings\":{warnings},\"diagnostics\":[{}]}}",
            errors == 0,
            diags.join(",")
        );
        say(ctx, &line);
    } else {
        for d in &res.diagnostics {
            say(ctx, &d.render());
        }
        if errors == 0 && warnings == 0 {
            say(ctx, "ok");
        } else {
            say(ctx, &format!("{errors} error(s), {warnings} warning(s)"));
        }
    }
    if errors == 0 { OK } else { ERROR }
}

fn print_entry(o: &Opts, cfg: &Config, key: &Key) -> Option<String> {
    let slot = cfg.slot(key.path)?;
    Some(if o.json {
        format!(
            "{{\"key\":{},\"value\":{},\"default\":{},\"origin\":{}}}",
            jstr(key.path),
            value_json(&slot.value),
            slot.origin.layer == Layer::Default,
            origin_json(cfg, key.path)
        )
    } else {
        let mut s = format!("{} = {}", key.path, value_text(key, &slot.value));
        if o.origin {
            s.push_str(&format!("  # {}", slot.origin.describe()));
        }
        s
    })
}

fn cmd_get(ctx: &mut Ctx, o: &Opts) -> i32 {
    let Some(name) = o.pos.first() else {
        return usage(ctx, o.json, "usage: apoth config get <key>");
    };
    let key = match lookup(name) {
        Ok(k) => k,
        Err(m) => {
            if schema::is_container(name) {
                return cmd_list(ctx, o, Some(name));
            }
            return fail(ctx, o.json, ERROR, &m);
        }
    };
    let res = load_cfg(ctx);
    let cfg = &res.config;
    let Some(slot) = cfg.slot(key.path) else {
        return fail(ctx, o.json, ERROR, "key has no value");
    };
    if o.json {
        let line = format!(
            "{{\"ok\":true,\"key\":{},\"value\":{},\"origin\":{}}}",
            jstr(key.path),
            value_json(&slot.value),
            origin_json(cfg, key.path)
        );
        say(ctx, &line);
    } else {
        let mut s = value_text(key, &slot.value);
        if o.origin {
            s.push_str(&format!("  # {}", slot.origin.describe()));
        }
        say(ctx, &s);
    }
    OK
}

fn cmd_list(ctx: &mut Ctx, o: &Opts, prefix: Option<&str>) -> i32 {
    let res = load_cfg(ctx);
    let cfg = &res.config;
    let mut lines = Vec::new();
    for (key, slot) in cfg.entries() {
        if let Some(p) = prefix
            && key.path != p
            && !key.path.starts_with(&format!("{p}."))
        {
            continue;
        }
        if o.changed_only && slot.origin.layer == Layer::Default {
            continue;
        }
        if let Some(l) = print_entry(o, cfg, key) {
            lines.push(l);
        }
    }
    if o.json {
        let line = format!("{{\"ok\":true,\"entries\":[{}]}}", lines.join(","));
        say(ctx, &line);
    } else {
        for l in lines {
            say(ctx, &l);
        }
    }
    OK
}

/// Read the target, apply `value` (`None` unsets), write atomically. `Ok(None)` when the
/// text did not change. A new file starts with `version 1`.
fn edit_file(
    ctx: &Ctx,
    cfg: &Config,
    user: &Path,
    scope: &[NodeSel],
    path: &str,
    value: Option<&Value>,
) -> Result<(PathBuf, bool), String> {
    let target = if scope.is_empty() {
        write_target(cfg, user, path)
    } else {
        user.to_path_buf()
    };
    let old = match ctx.fs.read_to_string(&target) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("cannot read {}: {:?}", target.display(), e.kind())),
    };
    let mut ed = Editor::parse(&old).map_err(|e| format!("{}: {e}", target.display()))?;
    if old.trim().is_empty() {
        ed.ensure_version().map_err(|e| e.to_string())?;
    }
    match value {
        Some(v) => ed.set_in(scope, path, v).map_err(|e| e.to_string())?,
        None => {
            ed.unset_in(scope, path).map_err(|e| e.to_string())?;
        }
    }
    let new = ed.text();
    if new == old {
        return Ok((target, false));
    }
    ctx.fs
        .write_atomic(&target, &new)
        .map_err(|e| format!("cannot write {}: {:?}", target.display(), e.kind()))?;
    Ok((target, true))
}

fn scope_of(o: &Opts) -> Vec<NodeSel> {
    o.profile.iter().map(|p| NodeSel::profile(p)).collect()
}

fn report_edit(
    ctx: &mut Ctx,
    o: &Opts,
    key: &str,
    r: Result<(PathBuf, bool), String>,
    verb: &str,
) -> i32 {
    match r {
        Ok((file, changed)) => {
            if o.json {
                let line = format!(
                    "{{\"ok\":true,\"key\":{},\"file\":{},\"changed\":{changed}}}",
                    jstr(key),
                    jstr(&file.display().to_string())
                );
                say(ctx, &line);
            } else if changed {
                say(ctx, &format!("{verb} {key} in {}", file.display()));
            } else {
                say(ctx, &format!("{key}: no change"));
            }
            OK
        }
        Err(m) => fail(ctx, o.json, ERROR, &m),
    }
}

/// `apoth set` / `apoth config set` (T-10 §5, T-12 §5): save `key=value` pairs to the
/// user file. Every pair is checked and every edit made in memory before any file is
/// written, so a bad pair writes nothing. The running instance picks the file up through
/// its hot reload.
fn cmd_set(ctx: &mut Ctx, o: &Opts) -> i32 {
    let pairs = match parse_pairs(&o.pos) {
        Ok(p) => p,
        Err(m) => {
            let msg = format!("{m}\nusage: apoth set KEY=VALUE [KEY=VALUE...] | KEY VALUE");
            return usage(ctx, o.json, &msg);
        }
    };
    let checked = match check_pairs(&pairs) {
        Ok(c) => c,
        Err(errs) => return fail(ctx, o.json, ERROR, &errs.join("\n")),
    };
    let cfg = load_cfg(ctx).config;
    let user = user_path(ctx);
    let scope = scope_of(o);
    // One editor per target file; nothing is written until every edit succeeded.
    let mut files: Vec<(PathBuf, String, Editor)> = Vec::new();
    let mut targets = Vec::with_capacity(checked.len());
    for c in &checked {
        let target = if scope.is_empty() {
            write_target(&cfg, &user, c.key.path)
        } else {
            user.clone()
        };
        let i = match files.iter().position(|(p, _, _)| *p == target) {
            Some(i) => i,
            None => {
                let old = match ctx.fs.read_to_string(&target) {
                    Ok(t) => t,
                    Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
                    Err(e) => {
                        let msg = format!("cannot read {}: {:?}", target.display(), e.kind());
                        return fail(ctx, o.json, ERROR, &msg);
                    }
                };
                let mut ed = match Editor::parse(&old) {
                    Ok(e) => e,
                    Err(e) => {
                        return fail(ctx, o.json, ERROR, &format!("{}: {e}", target.display()));
                    }
                };
                if old.trim().is_empty()
                    && let Err(e) = ed.ensure_version()
                {
                    return fail(ctx, o.json, ERROR, &e.to_string());
                }
                files.push((target, old, ed));
                files.len() - 1
            }
        };
        if let Err(e) = files[i].2.set_in(&scope, c.key.path, &c.value) {
            return fail(ctx, o.json, ERROR, &format!("{}: {e}", c.key.path));
        }
        targets.push(i);
    }
    let mut changed_files = vec![false; files.len()];
    for (i, (target, old, ed)) in files.iter().enumerate() {
        let new = ed.text();
        if new == *old {
            continue;
        }
        if let Err(e) = ctx.fs.write_atomic(target, &new) {
            let msg = format!("cannot write {}: {:?}", target.display(), e.kind());
            return fail(ctx, o.json, ERROR, &msg);
        }
        changed_files[i] = true;
    }
    // A key changed when its file changed and did not already hold that value there.
    let changes: Vec<(&str, &Path, bool)> = checked
        .iter()
        .zip(&targets)
        .map(|(c, i)| {
            let (target, old, _) = &files[*i];
            let held = scope.is_empty()
                && Editor::parse(old)
                    .ok()
                    .and_then(|e| e.get(c.key.path).ok().flatten())
                    .is_some_and(|v| v == c.value);
            (c.key.path, target.as_path(), changed_files[*i] && !held)
        })
        .collect();
    if o.json {
        let items: Vec<String> = changes
            .iter()
            .map(|(k, f, ch)| {
                format!(
                    "{{\"key\":{},\"file\":{},\"changed\":{ch}}}",
                    jstr(k),
                    jstr(&f.display().to_string())
                )
            })
            .collect();
        let line = format!("{{\"ok\":true,\"changes\":[{}]}}", items.join(","));
        say(ctx, &line);
    } else {
        for (k, f, ch) in &changes {
            if *ch {
                say(ctx, &format!("set {k} in {}", f.display()));
            } else {
                say(ctx, &format!("{k}: no change"));
            }
        }
    }
    OK
}

fn cmd_unset(ctx: &mut Ctx, o: &Opts) -> i32 {
    let Some(name) = o.pos.first() else {
        return usage(ctx, o.json, "usage: apoth config unset <key>");
    };
    let key = match lookup(name) {
        Ok(k) => k,
        Err(m) => return fail(ctx, o.json, ERROR, &m),
    };
    let cfg = load_cfg(ctx).config;
    let user = user_path(ctx);
    // Never create a file just to remove from it.
    let scope = scope_of(o);
    let target = if scope.is_empty() {
        write_target(&cfg, &user, key.path)
    } else {
        user.clone()
    };
    if !ctx.fs.exists(&target) {
        return report_edit(ctx, o, key.path, Ok((target, false)), "unset");
    }
    let r = edit_file(ctx, &cfg, &user, &scope, key.path, None);
    report_edit(ctx, o, key.path, r, "unset")
}

/// `config add|remove <list-key> <item>` (T-10 §5): edits one item of a list key.
fn cmd_collection(ctx: &mut Ctx, o: &Opts, add: bool) -> i32 {
    let verb = if add { "add" } else { "remove" };
    if o.pos.len() < 2 {
        return usage(
            ctx,
            o.json,
            &format!("usage: apoth config {verb} <key> <item>"),
        );
    }
    let key = match lookup(&o.pos[0]) {
        Ok(k) => k,
        Err(m) => return fail(ctx, o.json, ERROR, &m),
    };
    let item = o.pos[1..].join(" ");
    let cfg = load_cfg(ctx).config;
    let user = user_path(ctx);
    let target = write_target(&cfg, &user, key.path);
    let old = match ctx.fs.read_to_string(&target) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
        Err(e) => {
            let msg = format!("cannot read {}: {:?}", target.display(), e.kind());
            return fail(ctx, o.json, ERROR, &msg);
        }
    };
    let mut ed = match Editor::parse(&old) {
        Ok(e) => e,
        Err(e) => return fail(ctx, o.json, ERROR, &format!("{}: {e}", target.display())),
    };
    if old.trim().is_empty()
        && let Err(e) = ed.ensure_version()
    {
        return fail(ctx, o.json, ERROR, &e.to_string());
    }
    let r = if add {
        ed.add(key.path, &item)
    } else {
        ed.remove(key.path, &item).map(|_| ())
    };
    if let Err(e) = r {
        return fail(ctx, o.json, ERROR, &format!("{}: {e}", key.path));
    }
    let new = ed.text();
    let changed = new != old;
    if changed && let Err(e) = ctx.fs.write_atomic(&target, &new) {
        let msg = format!("cannot write {}: {:?}", target.display(), e.kind());
        return fail(ctx, o.json, ERROR, &msg);
    }
    report_edit(
        ctx,
        o,
        key.path,
        Ok((target, changed)),
        if add { "added to" } else { "removed from" },
    )
}

/// `config explain <key>` (T-10 §8): what a key is, its default, its value and where the
/// value came from.
fn cmd_explain(ctx: &mut Ctx, o: &Opts) -> i32 {
    let Some(name) = o.pos.first() else {
        return usage(ctx, o.json, "usage: apoth config explain <key>");
    };
    let key = match lookup(name) {
        Ok(k) => k,
        Err(m) => return fail(ctx, o.json, ERROR, &m),
    };
    let res = load_cfg(ctx);
    let Some(slot) = res.config.slot(key.path) else {
        return fail(ctx, o.json, ERROR, "unknown key");
    };
    let default = key.default_value();
    if o.json {
        let line = format!(
            "{{\"ok\":true,\"key\":{},\"doc\":{},\"reload\":{},\"default\":{},\"value\":{},\"origin\":{}}}",
            jstr(key.path),
            jstr(key.doc),
            jstr(&format!("{:?}", key.reload)),
            value_json(&default),
            value_json(&slot.value),
            origin_json(&res.config, key.path)
        );
        say(ctx, &line);
    } else {
        say(ctx, key.path);
        say(ctx, &format!("  {}", key.doc));
        say(ctx, &format!("  applies: {:?}", key.reload));
        say(ctx, &format!("  default: {}", value_text(key, &default)));
        say(
            ctx,
            &format!(
                "  value:   {}  # {}",
                value_text(key, &slot.value),
                slot.origin.describe()
            ),
        );
    }
    OK
}

/// `config diff` (T-10 §8): every key whose effective value differs from the default.
fn cmd_diff(ctx: &mut Ctx, o: &Opts) -> i32 {
    let res = load_cfg(ctx);
    let d = apoth_config::diff(&Config::defaults(), &res.config);
    let mut lines = Vec::new();
    for k in &d.keys {
        let Some(key) = schema::key(k.path) else {
            continue;
        };
        lines.push(if o.json {
            format!(
                "{{\"key\":{},\"default\":{},\"value\":{}}}",
                jstr(k.path),
                value_json(&k.old),
                value_json(&k.new)
            )
        } else {
            format!(
                "{}: {} -> {}",
                k.path,
                value_text(key, &k.old),
                value_text(key, &k.new)
            )
        });
    }
    if o.json {
        let line = format!("{{\"ok\":true,\"changes\":[{}]}}", lines.join(","));
        say(ctx, &line);
    } else {
        for l in lines {
            say(ctx, &l);
        }
    }
    OK
}

/// `config migrate` (T-10 §6): bring the user file to the current schema version. Only
/// version 1 exists, so the one migration is adding the `version` line to a file that lacks
/// it. The diff is shown; `--dry-run` writes nothing.
fn cmd_migrate(ctx: &mut Ctx, o: &Opts) -> i32 {
    let target = user_path(ctx);
    let old = match ctx.fs.read_to_string(&target) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return fail(
                ctx,
                o.json,
                ERROR,
                &format!("{} does not exist", target.display()),
            );
        }
        Err(e) => {
            let msg = format!("cannot read {}: {:?}", target.display(), e.kind());
            return fail(ctx, o.json, ERROR, &msg);
        }
    };
    let mut ed = match Editor::parse(&old) {
        Ok(e) => e,
        Err(e) => return fail(ctx, o.json, ERROR, &format!("{}: {e}", target.display())),
    };
    if let Err(e) = ed.ensure_version() {
        return fail(ctx, o.json, ERROR, &e.to_string());
    }
    let new = ed.text();
    let changed = new != old;
    if changed {
        if !o.json {
            say(ctx, "--- current");
            say(ctx, "+++ migrated");
            for l in new.lines().filter(|l| !old.lines().any(|x| x == *l)) {
                say(ctx, &format!("+{l}"));
            }
        }
        if !o.dry_run
            && let Err(e) = ctx.fs.write_atomic(&target, &new)
        {
            let msg = format!("cannot write {}: {:?}", target.display(), e.kind());
            return fail(ctx, o.json, ERROR, &msg);
        }
    }
    if o.json {
        let line = format!(
            "{{\"ok\":true,\"file\":{},\"changed\":{changed},\"dry_run\":{}}}",
            jstr(&target.display().to_string()),
            o.dry_run
        );
        say(ctx, &line);
    } else if !changed {
        say(ctx, "already at the current version");
    }
    OK
}

fn editor_command() -> String {
    ["VISUAL", "EDITOR"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .unwrap_or_else(|| "vi".to_owned())
}

/// `config edit` (T-10 §8): open the user file in `$VISUAL`/`$EDITOR`, then validate it.
fn cmd_edit(ctx: &mut Ctx, o: &Opts, editor: &str) -> i32 {
    let target = user_path(ctx);
    if !ctx.fs.exists(&target)
        && let Err(e) = ctx.fs.write_atomic(&target, "version 1\n")
    {
        let msg = format!("cannot create {}: {:?}", target.display(), e.kind());
        return fail(ctx, o.json, ERROR, &msg);
    }
    let mut words = editor.split_whitespace();
    let Some(prog) = words.next() else {
        return fail(ctx, o.json, ERROR, "no editor: set $VISUAL or $EDITOR");
    };
    match std::process::Command::new(prog)
        .args(words)
        .arg(&target)
        .status()
    {
        Ok(s) if s.success() => {}
        Ok(_) => return fail(ctx, o.json, ERROR, "the editor exited with an error"),
        Err(e) => {
            let msg = format!("cannot run the editor: {:?}", e.kind());
            return fail(ctx, o.json, ERROR, &msg);
        }
    }
    cmd_validate(ctx, o)
}

fn default_foot_path(ctx: &Ctx) -> PathBuf {
    let base = ctx.xdg.clone().unwrap_or_else(|| ctx.home.join(".config"));
    base.join("foot/foot.ini")
}

fn cmd_import_foot(ctx: &mut Ctx, o: &Opts, file: Option<&str>) -> i32 {
    let src = file
        .map(PathBuf::from)
        .unwrap_or_else(|| default_foot_path(ctx));
    let text = match ctx.fs.read_to_string(&src) {
        Ok(t) => t,
        Err(e) => {
            let msg = format!("cannot read {}: {:?}", src.display(), e.kind());
            return fail(ctx, o.json, ERROR, &msg);
        }
    };
    // The one importer lives in `apoth-config` (T-10 §6). It translates to a document; the
    // values are read back out of it and merged below without overwriting the user's own.
    let imp = apoth_config::import::import_foot(&text);
    let translated = match Editor::parse(&imp.kdl) {
        Ok(e) => e,
        Err(e) => return fail(ctx, o.json, ERROR, &format!("importer output: {e}")),
    };
    let mut settings: Vec<(&'static str, Value)> = Vec::new();
    for e in &imp.report {
        if e.status == apoth_config::import::Status::Unmapped {
            continue;
        }
        for t in &e.targets {
            if settings.iter().any(|(p, _)| p == t) {
                continue;
            }
            if let Ok(Some(v)) = translated.get(t) {
                settings.push((*t, v));
            }
        }
    }
    let untranslated: Vec<(usize, String, String)> = imp
        .unmapped()
        .map(|e| (e.line, e.key.clone(), e.note.clone()))
        .collect();
    let target = user_path(ctx);
    let old = match ctx.fs.read_to_string(&target) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
        Err(e) => {
            let msg = format!("cannot read {}: {:?}", target.display(), e.kind());
            return fail(ctx, o.json, ERROR, &msg);
        }
    };
    // The importer never overwrites a value the user already set.
    let base = if o.dry_run { "" } else { old.as_str() };
    let mut ed = match Editor::parse(base) {
        Ok(e) => e,
        Err(e) => return fail(ctx, o.json, ERROR, &format!("{}: {e}", target.display())),
    };
    if base.trim().is_empty()
        && let Err(e) = ed.ensure_version()
    {
        return fail(ctx, o.json, ERROR, &e.to_string());
    }
    let mut imported: Vec<&str> = Vec::new();
    let mut skipped: Vec<&str> = Vec::new();
    let mut rejected: Vec<(&str, String)> = Vec::new();
    for (path, v) in &settings {
        if matches!(ed.get(path), Ok(Some(_))) {
            skipped.push(path);
            continue;
        }
        match ed.set(path, v) {
            Ok(()) => imported.push(path),
            Err(e) => rejected.push((path, e.to_string())),
        }
    }
    let new = ed.text();
    if o.dry_run {
        if !o.json {
            let _ = write!(ctx.out, "{new}");
        }
    } else if new != old
        && let Err(e) = ctx.fs.write_atomic(&target, &new)
    {
        let msg = format!("cannot write {}: {:?}", target.display(), e.kind());
        return fail(ctx, o.json, ERROR, &msg);
    }
    if o.json {
        let notes: Vec<String> =
            untranslated
                .iter()
                .map(|(line, key, reason)| {
                    format!(
                        "{{\"key\":{},\"line\":{},\"reason\":{}}}",
                        jstr(key),
                        line,
                        jstr(reason)
                    )
                })
                .chain(rejected.iter().map(|(k, r)| {
                    format!("{{\"key\":{},\"line\":0,\"reason\":{}}}", jstr(k), jstr(r))
                }))
                .collect();
        let list = |v: &[&str]| jarr(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let kdl = if o.dry_run {
            format!(",\"kdl\":{}", jstr(&new))
        } else {
            String::new()
        };
        let line = format!(
            "{{\"ok\":true,\"file\":{},\"dry_run\":{},\"imported\":{},\"skipped\":{},\"untranslated\":[{}]{kdl}}}",
            jstr(&target.display().to_string()),
            o.dry_run,
            list(&imported),
            list(&skipped),
            notes.join(",")
        );
        say(ctx, &line);
    } else {
        let head = if o.dry_run {
            format!("would import {} key(s)", imported.len())
        } else {
            format!(
                "imported {} key(s) into {}",
                imported.len(),
                target.display()
            )
        };
        let _ = writeln!(ctx.err, "apoth: {head}");
        for k in &skipped {
            let _ = writeln!(ctx.err, "apoth: kept existing {k}");
        }
        for (line, key, reason) in &untranslated {
            let _ = writeln!(ctx.err, "foot.ini:{line}: not imported: {key} ({reason})");
        }
        for (k, r) in &rejected {
            let _ = writeln!(ctx.err, "apoth: not imported: {k} ({r})");
        }
    }
    OK
}

#[cfg(test)]
mod tests {
    use super::*;
    use apoth_config::fs::{MemFs, RealFs};

    const HOME: &str = "/home/t";
    const USER: &str = "/home/t/.config/eclipse/apotheosis.kdl";

    struct Run {
        code: i32,
        out: String,
        err: String,
    }

    fn run_with(fs: &dyn Fs, args: &[&str]) -> Run {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = {
            let mut ctx = Ctx {
                fs,
                home: PathBuf::from(HOME),
                xdg: None,
                out: &mut out,
                err: &mut err,
            };
            run(&args, &mut ctx)
        };
        Run {
            code,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
        }
    }

    #[test]
    fn path_prints_user_file() {
        let fs = MemFs::new();
        let r = run_with(&fs, &["path"]);
        assert_eq!(r.code, OK);
        assert_eq!(r.out.trim(), USER);
        let j = run_with(&fs, &["path", "--json"]);
        assert!(j.out.contains(&format!("\"user\":\"{USER}\"")));
    }

    #[test]
    fn set_creates_file_with_version_and_get_reads_it() {
        let fs = MemFs::new();
        let r = run_with(&fs, &["set", "font.size", "14"]);
        assert_eq!(r.code, OK, "{}", r.err);
        let text = fs.get(USER).unwrap();
        assert!(text.starts_with("version 1"), "{text}");
        assert!(text.contains("size 14"), "{text}");
        let g = run_with(&fs, &["get", "font.size"]);
        assert_eq!(g.code, OK);
        assert_eq!(g.out.trim(), "14.0");
        let gj = run_with(&fs, &["get", "font.size", "--json"]);
        assert!(gj.out.contains("\"value\":14.0"), "{}", gj.out);
        assert!(gj.out.contains("\"layer\":\"user\""), "{}", gj.out);
    }

    #[test]
    fn set_preserves_comments_and_unset_removes() {
        let fs = MemFs::new().with(USER, "// mine\nversion 1\nfont {\n    size 12 // keep\n}\n");
        assert_eq!(run_with(&fs, &["set", "cursor.style", "bar"]).code, OK);
        let t = fs.get(USER).unwrap();
        assert!(t.contains("// mine") && t.contains("// keep") && t.contains("bar"));
        let u = run_with(&fs, &["unset", "cursor.style"]);
        assert_eq!(u.code, OK);
        assert!(!fs.get(USER).unwrap().contains("cursor"));
        let again = run_with(&fs, &["unset", "cursor.style", "--json"]);
        assert!(again.out.contains("\"changed\":false"));
    }

    #[test]
    fn set_same_value_is_no_change() {
        let fs = MemFs::new();
        run_with(&fs, &["set", "scrollback.lines", "100"]);
        let r = run_with(&fs, &["set", "scrollback.lines", "100", "--json"]);
        assert!(r.out.contains("\"changed\":false"), "{}", r.out);
    }

    #[test]
    fn set_string_with_spaces_and_negative_numbers() {
        let fs = MemFs::new();
        let r = run_with(&fs, &["set", "font.family", "JetBrains", "Mono"]);
        assert_eq!(r.code, OK, "{}", r.err);
        assert_eq!(
            run_with(&fs, &["get", "font.family"]).out.trim(),
            "\"JetBrains Mono\""
        );
        let n = run_with(&fs, &["set", "font.adjust-baseline", "-2px"]);
        assert_eq!(n.code, OK, "{}", n.err);
    }

    #[test]
    fn unknown_key_suggests_and_bad_value_errors_without_writing() {
        let fs = MemFs::new();
        let r = run_with(&fs, &["set", "font.sise", "14"]);
        assert_eq!(r.code, ERROR);
        assert!(r.err.contains("font.size"), "{}", r.err);
        let b = run_with(&fs, &["set", "font.size", "huge"]);
        assert_eq!(b.code, ERROR);
        let s = run_with(&fs, &["set", "font", "x"]);
        assert_eq!(s.code, ERROR);
        assert!(fs.get(USER).is_none());
    }

    #[test]
    fn usage_errors_exit_2() {
        let fs = MemFs::new();
        assert_eq!(run_with(&fs, &[]).code, USAGE);
        assert_eq!(run_with(&fs, &["frobnicate"]).code, USAGE);
        assert_eq!(run_with(&fs, &["set", "font.size"]).code, USAGE);
        assert_eq!(run_with(&fs, &["get"]).code, USAGE);
        assert_eq!(run_with(&fs, &["unset"]).code, USAGE);
        assert_eq!(run_with(&fs, &["get", "--bogus", "x"]).code, USAGE);
        assert_eq!(run_with(&fs, &["import", "kitty", "x"]).code, USAGE);
    }

    #[test]
    fn validate_ok_and_errors_with_line_numbers() {
        let fs = MemFs::new().with(USER, "version 1\nfont {\n    size 12\n}\n");
        let r = run_with(&fs, &["validate"]);
        assert_eq!(r.code, OK, "{}", r.out);
        assert!(r.out.contains("ok"));
        fs.set(USER, "version 1\nfont {\n    size 9999\n}\n");
        let bad = run_with(&fs, &["validate"]);
        assert_eq!(bad.code, ERROR);
        assert!(bad.out.contains(":3:"), "{}", bad.out);
        let j = run_with(&fs, &["validate", "--json"]);
        assert!(j.out.starts_with("{\"ok\":false"), "{}", j.out);
        assert!(j.out.contains("\"line\":3"), "{}", j.out);
    }

    #[test]
    fn list_filters_prefix_and_changed_only() {
        let fs = MemFs::new().with(USER, "version 1\nfont {\n    size 13\n}\n");
        let all = run_with(&fs, &["list", "font"]);
        assert_eq!(all.code, OK);
        assert!(all.out.contains("font.size = 13.0"), "{}", all.out);
        assert!(all.out.contains("font.family"));
        assert!(!all.out.contains("cursor.style"));
        let ch = run_with(&fs, &["list", "--changed-only", "--origin"]);
        assert_eq!(ch.out.lines().count(), 1, "{}", ch.out);
        assert!(ch.out.contains("# user"), "{}", ch.out);
        let j = run_with(&fs, &["list", "--changed-only", "--json"]);
        assert!(
            j.out.starts_with("{\"ok\":true,\"entries\":[{"),
            "{}",
            j.out
        );
        let section = run_with(&fs, &["get", "font"]);
        assert!(section.out.contains("font.size"));
    }

    #[test]
    fn profile_scope_writes_profile_node() {
        let fs = MemFs::new();
        let r = run_with(&fs, &["set", "--profile", "work", "font.size", "15"]);
        assert_eq!(r.code, OK, "{}", r.err);
        let t = fs.get(USER).unwrap();
        assert!(
            t.contains("profile") && t.contains("work") && t.contains("size 15"),
            "{t}"
        );
    }

    #[test]
    fn add_remove_explain_diff_migrate() {
        let fs = MemFs::new().with(USER, "font {\n    size 13\n}\n");
        let m = run_with(&fs, &["migrate", "--dry-run"]);
        assert_eq!(m.code, OK, "{}", m.err);
        assert!(m.out.contains("+version 1"), "{}", m.out);
        assert!(
            !fs.get(USER).unwrap().contains("version"),
            "dry run writes nothing"
        );
        assert_eq!(run_with(&fs, &["migrate"]).code, OK);
        assert!(fs.get(USER).unwrap().contains("version 1"));
        assert!(run_with(&fs, &["migrate"]).out.contains("already"));

        let e = run_with(&fs, &["explain", "font.size"]);
        assert_eq!(e.code, OK, "{}", e.err);
        assert!(
            e.out.contains("default:") && e.out.contains("value:") && e.out.contains("applies:")
        );
        let d = run_with(&fs, &["diff"]);
        assert!(
            d.out.contains("font.size:") && d.out.contains("->"),
            "{}",
            d.out
        );
        assert_eq!(run_with(&fs, &["explain", "nope.nope"]).code, ERROR);

        let a = run_with(&fs, &["add", "font.fallback", "Noto Sans"]);
        assert_eq!(a.code, OK, "{}", a.err);
        assert!(fs.get(USER).unwrap().contains("Noto Sans"));
        let r = run_with(&fs, &["remove", "font.fallback", "Noto Sans"]);
        assert_eq!(r.code, OK, "{}", r.err);
        assert!(!fs.get(USER).unwrap().contains("Noto Sans"));
        assert_eq!(
            run_with(&fs, &["add", "font.size", "x"]).code,
            ERROR,
            "not a list"
        );
        assert_eq!(run_with(&fs, &["add", "font.fallback"]).code, USAGE);
    }

    #[test]
    fn edit_runs_the_editor_then_validates() {
        let fs = MemFs::new();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut ctx = Ctx {
            fs: &fs,
            home: PathBuf::from(HOME),
            xdg: None,
            out: &mut out,
            err: &mut err,
        };
        assert_eq!(cmd_edit(&mut ctx, &Opts::default(), "true"), OK);
        assert_eq!(cmd_edit(&mut ctx, &Opts::default(), "false"), ERROR);
        assert_eq!(fs.get(USER).unwrap(), "version 1\n");
    }

    const FOOT: &str = "[main]\nfont=Fira Code:size=10\npad=8x8\n[cursor]\nstyle=beam\n[key-bindings]\nfullscreen=F11\n";

    #[test]
    fn import_foot_writes_user_file_and_reports() {
        let fs = MemFs::new().with("/home/t/.config/foot/foot.ini", FOOT);
        let r = run_with(&fs, &["import-foot"]);
        assert_eq!(r.code, OK, "{}", r.err);
        let t = fs.get(USER).unwrap();
        assert!(t.starts_with("version 1"), "{t}");
        assert!(
            t.contains("Fira Code") && t.contains("bar") && t.contains("padding-x"),
            "{t}"
        );
        assert!(r.err.contains("fullscreen"), "{}", r.err);
        assert!(!r.err.contains("F11"));
        let v = run_with(&fs, &["validate"]);
        assert_eq!(v.code, OK, "{}", v.out);
    }

    #[test]
    fn import_foot_keeps_existing_values_and_foot_file_untouched() {
        let fs = MemFs::new()
            .with("/home/t/.config/foot/foot.ini", FOOT)
            .with(USER, "version 1\n// mine\nfont {\n    size 20\n}\n");
        let r = run_with(&fs, &["import", "foot", "--json"]);
        assert_eq!(r.code, OK, "{}", r.err);
        assert!(r.out.contains("\"skipped\":[\"font.size\"]"), "{}", r.out);
        let t = fs.get(USER).unwrap();
        assert!(t.contains("// mine") && t.contains("size 20") && t.contains("Fira Code"));
        assert_eq!(fs.get("/home/t/.config/foot/foot.ini").unwrap(), FOOT);
    }

    #[test]
    fn import_foot_dry_run_prints_kdl_and_writes_nothing() {
        let fs = MemFs::new().with("/x/foot.ini", FOOT);
        let r = run_with(&fs, &["import-foot", "/x/foot.ini", "--dry-run"]);
        assert_eq!(r.code, OK);
        assert!(r.out.contains("Fira Code"), "{}", r.out);
        assert!(fs.get(USER).is_none());
    }

    #[test]
    fn import_foot_missing_file_errors() {
        let fs = MemFs::new();
        assert_eq!(run_with(&fs, &["import-foot"]).code, ERROR);
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn pairs_parse_in_both_forms() {
        assert_eq!(
            parse_pairs(&strs(&["font.size=12", "cursor.style=bar"])).unwrap(),
            [
                ("font.size".to_string(), "12".to_string()),
                ("cursor.style".to_string(), "bar".to_string())
            ]
        );
        assert_eq!(
            parse_pairs(&strs(&["font.family", "JetBrains", "Mono"])).unwrap(),
            [("font.family".to_string(), "JetBrains Mono".to_string())]
        );
        // The split is at the first `=`; the value may hold more.
        assert_eq!(
            parse_pairs(&strs(&["links.regex=a=b"])).unwrap(),
            [("links.regex".to_string(), "a=b".to_string())]
        );
        assert_eq!(
            parse_pairs(&strs(&["k="])).unwrap(),
            [("k".to_string(), String::new())]
        );
        for bad in [
            &[][..],
            &["font.size"],
            &["font.size=12", "cursor.style"],
            &["=12"],
            &["font.size=1", "font.size=2"],
        ] {
            assert!(parse_pairs(&strs(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn key_aliases_resolve_only_when_unambiguous() {
        for alias in ["font_size", "font-size", "font.size", "FONT_SIZE"] {
            let r = lookup(alias);
            if alias == "FONT_SIZE" {
                assert!(r.is_err(), "case is not folded");
            } else {
                assert_eq!(r.unwrap().path, "font.size", "{alias}");
            }
        }
        assert_eq!(lookup("window_padding_x").unwrap().path, "window.padding-x");
        assert_eq!(lookup("window.padding.x").unwrap().path, "window.padding-x");
        let e = lookup("font_sise").unwrap_err();
        assert!(e.contains("`font.size`"), "{e}");
        // Every key is reachable by its underscore alias, or the alias is reported ambiguous.
        for k in TABLE {
            let alias = k.path.replace(['.', '-'], "_");
            match lookup(&alias) {
                Ok(found) => assert_eq!(found.path, k.path),
                Err(m) => assert!(m.contains("ambiguous"), "{alias}: {m}"),
            }
        }
    }

    #[test]
    fn set_many_pairs_with_aliases_preserves_comments() {
        let fs = MemFs::new().with(USER, "// mine\nversion 1\nfont {\n    size 12 // keep\n}\n");
        let r = run_with(&fs, &["set", "font_size=14", "cursor-style=bar", "--json"]);
        assert_eq!(r.code, OK, "{}{}", r.out, r.err);
        assert!(r.out.starts_with("{\"ok\":true,\"changes\":["), "{}", r.out);
        assert!(r.out.contains("\"key\":\"font.size\""), "{}", r.out);
        let t = fs.get(USER).unwrap();
        assert!(t.contains("// mine") && t.contains("// keep"), "{t}");
        assert!(t.contains("size 14") && t.contains("bar"), "{t}");
        // Only the edited bytes moved.
        assert!(t.starts_with("// mine\nversion 1\nfont {\n"), "{t}");
    }

    #[test]
    fn set_is_all_or_nothing() {
        let before = "version 1\n// mine\nfont {\n    size 12\n}\n";
        let fs = MemFs::new().with(USER, before);
        for args in [
            &["set", "cursor.style=bar", "font.size=9999"][..],
            &["set", "cursor.style=bar", "font.sise=13"],
            &["set", "cursor.style=bar", "font.size=huge"],
            &["set", "cursor.style=bar", "font_size=13", "font.size=14"],
        ] {
            let r = run_with(&fs, args);
            assert_eq!(r.code, ERROR, "{args:?}: {}", r.err);
            assert_eq!(fs.get(USER).unwrap(), before, "{args:?} wrote the file");
        }
        // Every problem is named, not only the first.
        let r = run_with(&fs, &["set", "font.sise=1", "cursor.style=round"]);
        assert!(
            r.err.contains("font.size") && r.err.contains("cursor.style"),
            "{}",
            r.err
        );
        let j = run_with(&fs, &["set", "font.size=huge", "--json"]);
        assert!(j.out.starts_with("{\"ok\":false"), "{}", j.out);
        // Malformed pairs are usage errors.
        assert_eq!(run_with(&fs, &["set", "font.size=1", "bar"]).code, USAGE);
        assert_eq!(fs.get(USER).unwrap(), before);
    }

    #[test]
    fn json_escaping() {
        assert_eq!(jstr("a\"b\\c\n\u{1}"), "\"a\\\"b\\\\c\\n\\u0001\"");
    }

    #[test]
    fn real_temp_dir_atomic_write() {
        let dir = std::env::temp_dir().join(format!("apoth-config-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let args: Vec<String> = ["set", "font.size", "16"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = {
            let mut ctx = Ctx {
                fs: &RealFs,
                home: dir.clone(),
                xdg: None,
                out: &mut out,
                err: &mut err,
            };
            run(&args, &mut ctx)
        };
        assert_eq!(code, OK);
        let file = dir.join(".config/eclipse/apotheosis.kdl");
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("size 16"), "{text}");
        let leftovers: Vec<_> = std::fs::read_dir(file.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
