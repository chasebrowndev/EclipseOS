// SPDX-License-Identifier: AGPL-3.0-only
//! Layered load and validation (T-10 §2, §4).

use crate::collections::{self, Cx, Out};
use crate::diag::{Diagnostic, LineIndex, MAX_DIAGNOSTICS, Severity, Source, suggest};
use crate::fs::{Fs, expand_glob, glob_match};
use crate::lang;
use crate::model::*;
use crate::overrides::parse_override_value;
use crate::schema::{self, TABLE};
use crate::types::{TOP_LEVEL_NODES, Value};
use crate::version;
use apoth_action::macros::MAX_MACROS;
use kdl::{KdlDocument, KdlNode};
use std::path::{Path, PathBuf};

/// Include nesting limit (T-10 §2).
pub const MAX_INCLUDE_DEPTH: usize = 8;
/// Total file loads in one `load` call (includes fan out; this bounds the product).
pub const MAX_FILE_LOADS: usize = 256;
/// Binds and rules kept after merging all layers (the rest are dropped with a diagnostic).
pub const MAX_BINDS: usize = 4096;
pub const MAX_RULES: usize = 1024;
/// Deepest `{` nesting a file may have before it is refused unparsed (the KDL parser
/// recurses once per level).
pub const MAX_BRACE_DEPTH: usize = 64;

/// Cheap pre-parse scan: the deepest `{` nesting outside quoted strings and comments.
/// Raw strings are not tracked, so a file that hides braces in one may be over-counted;
/// that only ever refuses a pathological file, never admits a deep one.
fn brace_depth(text: &str) -> usize {
    let (mut depth, mut max) = (0usize, 0usize);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                while let Some(q) = chars.next() {
                    match q {
                        '\\' => {
                            chars.next();
                        }
                        '"' => break,
                        _ => {}
                    }
                }
            }
            '/' if chars.peek() == Some(&'/') => {
                for q in chars.by_ref() {
                    if q == '\n' {
                        break;
                    }
                }
            }
            '{' => {
                depth += 1;
                max = max.max(depth);
            }
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    max
}

/// The runtime facts `when` predicates are evaluated against (T-10 §3).
#[derive(Debug, Clone, PartialEq)]
pub struct Env {
    pub compositor: String,
    pub host: String,
    /// `light` or `dark`.
    pub scheme: String,
    pub on_battery: bool,
    pub scale: f64,
}

impl Default for Env {
    fn default() -> Self {
        Env {
            compositor: String::new(),
            host: String::new(),
            scheme: "dark".to_string(),
            on_battery: false,
            scale: 1.0,
        }
    }
}

/// What to load.
#[derive(Debug, Clone, Default)]
pub struct LoadInput {
    /// Layer 2.
    pub system: Option<PathBuf>,
    /// Layer 3 files, already expanded and ordered.
    pub fragments: Vec<PathBuf>,
    /// Layer 4 main file (its includes are followed).
    pub user: Option<PathBuf>,
    /// Layer 5 files, each isolated from the others.
    pub profile_files: Vec<PathBuf>,
    /// Layer 6: names of the matched profiles, lowest priority first.
    pub active_profiles: Vec<String>,
    /// Layer 7: `-o key=value`, re-applied on every reload.
    pub launch: Vec<(String, String)>,
    pub env: Env,
}

/// The standard file locations. Eclipse paths are tried first when they exist; otherwise the
/// XDG ones when they exist; otherwise the Eclipse path (where a first write creates it).
pub fn default_input(fs: &dyn Fs, home: &Path, xdg_config_home: Option<&Path>) -> LoadInput {
    let eclipse_user = home.join(".config/eclipse/apotheosis.kdl");
    let xdg_dir = xdg_config_home
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| home.join(".config"));
    let xdg_user = xdg_dir.join("apotheosis/apotheosis.kdl");
    let user = if fs.exists(&eclipse_user) || !fs.exists(&xdg_user) {
        eclipse_user
    } else {
        xdg_user
    };
    let eclipse_sys = PathBuf::from("/etc/eclipse/apotheosis.kdl");
    let xdg_sys = PathBuf::from("/etc/apotheosis/apotheosis.kdl");
    let system = if fs.exists(&eclipse_sys) || !fs.exists(&xdg_sys) {
        eclipse_sys
    } else {
        xdg_sys
    };
    let user_dir = user.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    LoadInput {
        system: Some(system),
        fragments: expand_glob(fs, Path::new("/usr/share/apotheosis/fragments/*.kdl")),
        profile_files: expand_glob(fs, &user_dir.join("apotheosis.d/profiles/*.kdl")),
        user: Some(user),
        ..Default::default()
    }
}

/// The outcome of a load.
#[derive(Debug, Clone)]
pub struct LoadResult {
    /// The tolerant result: bad collection entries dropped, bad scalars at their previous (or
    /// lower-layer) value.
    pub config: Config,
    pub diagnostics: Vec<Diagnostic>,
    /// Every file read or looked for (main files, includes, missing optional includes): the
    /// watch set (M15 watches the parent directories of these).
    pub files: Vec<PathBuf>,
    /// A file failed to parse at all. A running config should keep its last good config.
    pub rejected: bool,
}

impl LoadResult {
    pub fn error_count(&self) -> usize {
        self.diagnostics.iter().filter(|d| d.is_error()).count()
    }
    /// No errors: apply atomically.
    pub fn is_valid(&self) -> bool {
        self.error_count() == 0
    }
    /// Line-numbered report for `apoth config validate`.
    pub fn render(&self) -> String {
        self.diagnostics
            .iter()
            .map(Diagnostic::render)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Full,
    /// Layer 3: profiles and actions only.
    Fragment,
    /// Layer 5: profiles only.
    ProfileFile,
}

struct Loader<'f> {
    fs: &'f dyn Fs,
    env: &'f Env,
    diags: Vec<Diagnostic>,
    files: Vec<PathBuf>,
    stack: Vec<PathBuf>,
    rejected: bool,
    budget_reported: bool,
}

fn eval_when(node: &KdlNode, env: &Env, cx: &mut Cx) -> Option<bool> {
    let preds = ["compositor", "host", "scheme", "on-battery", "scale"];
    let mut all = true;
    let mut any = false;
    for e in node.entries() {
        let Some(name) = e.name() else {
            cx.err(
                e.span().offset(),
                None,
                "`when` takes properties such as scheme=\"dark\"",
            );
            return None;
        };
        any = true;
        let v = e.value();
        let holds = match name.value() {
            "compositor" => v.as_string().map(|s| s == env.compositor),
            "host" => v.as_string().map(|s| glob_match(s, &env.host)),
            "scheme" => v.as_string().map(|s| s == env.scheme),
            "on-battery" => v.as_bool().map(|b| b == env.on_battery),
            "scale" => scale_holds(v, env.scale),
            other => {
                let sug = suggest(other, &preds);
                cx.err_sug(
                    e.span().offset(),
                    None,
                    format!("unknown `when` predicate `{other}`"),
                    sug,
                );
                return None;
            }
        };
        match holds {
            Some(h) => all &= h,
            None => {
                cx.err(
                    e.span().offset(),
                    None,
                    format!("bad value for `{}`", name.value()),
                );
                return None;
            }
        }
    }
    if !any {
        cx.err(
            node.span().offset(),
            None,
            "`when` needs at least one predicate",
        );
        return None;
    }
    Some(all)
}

fn scale_holds(v: &kdl::KdlValue, scale: f64) -> Option<bool> {
    if let Some(f) = v.as_float() {
        return Some((f - scale).abs() < 0.01);
    }
    if let Some(i) = v.as_integer() {
        return Some((i as f64 - scale).abs() < 0.01);
    }
    let s = v.as_string()?;
    let (op, num) = if let Some(n) = s.strip_prefix(">=") {
        (">=", n)
    } else if let Some(n) = s.strip_prefix("<=") {
        ("<=", n)
    } else if let Some(n) = s.strip_prefix('>') {
        (">", n)
    } else if let Some(n) = s.strip_prefix('<') {
        ("<", n)
    } else if let Some(n) = s.strip_prefix('=') {
        ("=", n)
    } else {
        ("=", s)
    };
    let n: f64 = num.trim().parse().ok()?;
    Some(match op {
        ">=" => scale >= n,
        "<=" => scale <= n,
        ">" => scale > n,
        "<" => scale < n,
        _ => (scale - n).abs() < 0.01,
    })
}

impl Loader<'_> {
    fn load_file(
        &mut self,
        path: &Path,
        layer: Layer,
        out: &mut Out,
        depth: usize,
        mode: Mode,
        at: Option<(&Source, usize)>,
    ) {
        if self.files.len() >= MAX_FILE_LOADS {
            if !self.budget_reported {
                self.budget_reported = true;
                let msg = format!("more than {MAX_FILE_LOADS} files loaded; the rest are skipped");
                self.diags.push(match at {
                    Some((src, off)) => src.error(off, Some("include"), msg),
                    None => Diagnostic {
                        severity: Severity::Error,
                        file: Some(path.to_path_buf()),
                        line: 0,
                        column: 0,
                        key: None,
                        message: msg,
                        suggestion: None,
                    },
                });
            }
            return;
        }
        let canon = self.fs.canonical(path);
        self.files.push(path.to_path_buf());
        let text = match self.fs.read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                let msg = format!("cannot read file: {}", e.kind());
                self.diags.push(match at {
                    Some((src, off)) => src.error(off, None, msg),
                    None => Diagnostic {
                        severity: Severity::Error,
                        file: Some(path.to_path_buf()),
                        line: 0,
                        column: 0,
                        key: None,
                        message: msg,
                        suggestion: None,
                    },
                });
                return;
            }
        };
        let lines = LineIndex::new(&text);
        if brace_depth(&text) > MAX_BRACE_DEPTH {
            let src = Source {
                file: Some(path),
                text: &text,
                lines: &lines,
            };
            self.diags.push(src.error(
                0,
                None,
                format!("blocks nest deeper than {MAX_BRACE_DEPTH} levels"),
            ));
            if layer != Layer::ProfileDir {
                self.rejected = true;
            }
            return;
        }
        let doc = match KdlDocument::parse(&text) {
            Ok(d) => d,
            Err(e) => {
                let src = Source {
                    file: Some(path),
                    text: &text,
                    lines: &lines,
                };
                if e.diagnostics.is_empty() {
                    self.diags.push(src.error(0, None, "invalid KDL"));
                }
                for d in &e.diagnostics {
                    let msg = d
                        .message
                        .clone()
                        .unwrap_or_else(|| "invalid KDL".to_string());
                    self.diags
                        .push(src.diag(Severity::Error, d.span.offset(), None, msg, None));
                }
                if layer != Layer::ProfileDir {
                    self.rejected = true;
                }
                return;
            }
        };
        let src = Source {
            file: Some(path),
            text: &text,
            lines: &lines,
        };
        self.stack.push(canon);
        self.process_nodes(&src, &doc, layer, false, false, out, depth, mode);
        self.stack.pop();
    }

    #[allow(clippy::too_many_arguments)]
    fn process_nodes(
        &mut self,
        src: &Source,
        doc: &KdlDocument,
        layer: Layer,
        in_when: bool,
        dry: bool,
        out: &mut Out,
        depth: usize,
        mode: Mode,
    ) {
        for node in doc.nodes() {
            let name = node.name().value();
            let offset = node.span().offset();
            macro_rules! cx {
                () => {
                    Cx {
                        src: *src,
                        layer,
                        in_when,
                        diags: &mut self.diags,
                    }
                };
            }
            if !TOP_LEVEL_NODES.contains(&name) {
                let sug = suggest(name, TOP_LEVEL_NODES);
                cx!().err_sug(offset, None, format!("unknown node `{name}`"), sug);
                continue;
            }
            match name {
                "version" => {
                    let n = node
                        .entries()
                        .iter()
                        .find(|e| e.name().is_none())
                        .and_then(|e| e.value().as_integer());
                    if let Err(m) = version::check_version(n) {
                        cx!().err(offset, Some("version"), m);
                    }
                }
                "include" => self.include(src, node, layer, in_when, dry, out, depth, mode),
                "when" => {
                    let verdict = {
                        let mut cx = cx!();
                        eval_when(node, self.env, &mut cx)
                    };
                    if let (Some(hit), Some(children)) = (verdict, node.children()) {
                        // A non-matching branch is still validated, but contributes nothing.
                        if hit && !dry {
                            self.process_nodes(src, children, layer, true, dry, out, depth, mode);
                        } else {
                            let mut scratch = Out::default();
                            self.process_nodes(
                                src,
                                children,
                                layer,
                                true,
                                true,
                                &mut scratch,
                                depth,
                                mode,
                            );
                        }
                    }
                }
                _ => {
                    if mode != Mode::Full && !self.allowed_in_restricted(name, mode) {
                        let what = if mode == Mode::Fragment {
                            "fragments"
                        } else {
                            "profile files"
                        };
                        cx!().err(
                            offset,
                            None,
                            format!("`{name}` cannot appear in {what} (profiles and actions only)"),
                        );
                        continue;
                    }
                    let mut scratch = Out::default();
                    let target: &mut Out = if dry { &mut scratch } else { &mut *out };
                    let mut cx = cx!();
                    dispatch(&mut cx, node, target);
                }
            }
        }
    }

    fn allowed_in_restricted(&self, name: &str, mode: Mode) -> bool {
        match mode {
            Mode::Full => true,
            Mode::Fragment => matches!(name, "profile" | "macro"),
            Mode::ProfileFile => name == "profile",
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn include(
        &mut self,
        src: &Source,
        node: &KdlNode,
        layer: Layer,
        in_when: bool,
        dry: bool,
        out: &mut Out,
        depth: usize,
        mode: Mode,
    ) {
        let offset = node.span().offset();
        let mut cx = Cx {
            src: *src,
            layer,
            in_when,
            diags: &mut self.diags,
        };
        if mode != Mode::Full || !matches!(layer, Layer::System | Layer::User) {
            cx.err(
                offset,
                Some("include"),
                "`include` is only allowed in the system and user files",
            );
            return;
        }
        let pat = match node
            .entries()
            .iter()
            .filter(|e| e.name().is_none())
            .map(|e| e.value().as_string())
            .collect::<Vec<_>>()
            .as_slice()
        {
            [Some(p)] if !p.is_empty() => p.to_string(),
            _ => {
                cx.err(offset, Some("include"), "`include` needs one path string");
                return;
            }
        };
        let prop_optional = node
            .entry("optional")
            .and_then(|e| e.value().as_bool())
            .unwrap_or(false);
        let (optional, pat) = match pat.strip_prefix('?') {
            Some(rest) => (true, rest.to_string()),
            None => (prop_optional, pat),
        };
        if dry {
            return;
        }
        if depth + 1 > MAX_INCLUDE_DEPTH {
            cx.err(
                offset,
                Some("include"),
                format!("include depth exceeds {MAX_INCLUDE_DEPTH}"),
            );
            return;
        }
        let base = src
            .file
            .and_then(|f| f.parent())
            .unwrap_or_else(|| Path::new("."));
        let full = if Path::new(&pat).is_absolute() {
            PathBuf::from(&pat)
        } else {
            base.join(&pat)
        };
        let dir_glob = full
            .parent()
            .is_some_and(|d| crate::fs::has_glob(&d.to_string_lossy()));
        if dir_glob {
            cx.err(
                offset,
                Some("include"),
                "globs are only supported in the final path component",
            );
            return;
        }
        let paths = expand_glob(self.fs, &full);
        let globbed = crate::fs::has_glob(&pat);
        for p in paths {
            let canon = self.fs.canonical(&p);
            if self.stack.contains(&canon) {
                let chain: Vec<String> =
                    self.stack.iter().map(|s| s.display().to_string()).collect();
                let msg = format!(
                    "include cycle: {} -> {}",
                    chain.join(" -> "),
                    canon.display()
                );
                self.diags.push(src.error(offset, Some("include"), msg));
                continue;
            }
            if !optional && !globbed && !self.fs.exists(&p) {
                self.files.push(p.clone());
                self.diags.push(src.error(
                    offset,
                    Some("include"),
                    format!("included file not found: {}", p.display()),
                ));
                continue;
            }
            self.load_file(&p, layer, out, depth + 1, Mode::Full, Some((src, offset)));
        }
        if globbed {
            // The directory is part of the watch set even when nothing matches yet.
            self.files.push(full);
        }
    }
}

/// Dispatch one top-level node (not `version`, `include`, `when`).
fn dispatch(cx: &mut Cx, node: &KdlNode, out: &mut Out) {
    let name = node.name().value();
    let has_args = node.entries().iter().any(|e| e.name().is_none());
    match name {
        "binds" => collections::parse_binds(cx, node, out),
        "mouse-binds" => collections::parse_mouse_binds(cx, node, out),
        "key-table" if has_args => collections::parse_key_table(cx, node, out),
        "macro" => collections::parse_macro(cx, node, out),
        "rule" if has_args => collections::parse_rule(cx, node, out),
        "profile" if has_args => collections::parse_profile(cx, node, out),
        "layout" if has_args => collections::parse_layout(cx, node, out),
        "menu" if has_args => collections::parse_menu(cx, node, out),
        "domain" => collections::parse_domain(cx, node, out),
        _ => collections::walk_node(cx, node, "", out, false),
    }
}

fn merge(cfg: &mut Config, out: Out) {
    for s in out.settings {
        cfg.set_slot(s.idx, s.value, s.origin);
    }
    for u in out.unbinds {
        cfg.binds.retain(|b| b.value.chords != u.value);
        cfg.unbinds.push(u);
    }
    for b in out.binds {
        cfg.binds.retain(|e| {
            !(e.value.chords == b.value.chords
                && e.value.mode == b.value.mode
                && e.value.release == b.value.release)
        });
        cfg.binds.push(b);
    }
    for m in out.mouse_binds {
        cfg.mouse_binds
            .retain(|e| !(e.value.trigger == m.value.trigger && e.value.on == m.value.on));
        cfg.mouse_binds.push(m);
    }
    macro_rules! named {
        ($field:ident) => {
            for x in out.$field {
                cfg.$field.retain(|e| e.value.name != x.value.name);
                cfg.$field.push(x);
            }
        };
    }
    named!(key_tables);
    named!(macros);
    named!(rules);
    named!(profiles);
    named!(layouts);
    named!(menus);
    named!(domains);
}

fn diag_at(origin: &Origin, key: Option<&str>, msg: impl Into<String>) -> Diagnostic {
    Diagnostic {
        severity: Severity::Error,
        file: origin.file.clone(),
        line: origin.line,
        column: origin.column,
        key: key.map(|k| k.to_string()),
        message: msg.into(),
        suggestion: None,
    }
}

/// Collect the `macro.run` references inside a call tree.
fn macro_refs(calls: &[ActionCall], into: &mut Vec<String>) {
    for c in calls {
        if c.id == "macro.run"
            && let Some(Arg::Str(n)) = c.args.first()
        {
            into.push(n.clone());
        }
        macro_refs(&c.children, into);
    }
}

fn check_macros(cfg: &mut Config, diags: &mut Vec<Diagnostic>) {
    // Bound the work below: later definitions beyond the cap are dropped.
    while cfg.macros.len() > MAX_MACROS {
        if let Some(m) = cfg.macros.pop() {
            diags.push(diag_at(
                &m.origin,
                None,
                format!(
                    "macro `{}` dropped: more than {MAX_MACROS} macros",
                    m.value.name
                ),
            ));
        }
    }
    // Drop macros that reference unknown macros or take part in a cycle, until stable. Each
    // round removes every offender at once, so the work is rounds x total references.
    loop {
        let index: std::collections::HashMap<&str, usize> = cfg
            .macros
            .iter()
            .enumerate()
            .map(|(i, m)| (m.value.name.as_str(), i))
            .collect();
        let refs: Vec<Vec<String>> = cfg
            .macros
            .iter()
            .map(|m| {
                let mut r = Vec::new();
                macro_refs(&m.value.actions, &mut r);
                r
            })
            .collect();
        let mut drop: Vec<Option<String>> = vec![None; cfg.macros.len()];
        for (i, rs) in refs.iter().enumerate() {
            if let Some(bad) = rs.iter().find(|r| !index.contains_key(r.as_str())) {
                drop[i] = Some(format!(
                    "macro `{}` runs unknown macro `{bad}`",
                    cfg.macros[i].value.name
                ));
            }
        }
        if drop.iter().all(Option::is_none) {
            // Cycle members: a macro that can reach itself (iterative DFS per macro with a
            // visited set; every edge is known to resolve here).
            for (i, rs) in refs.iter().enumerate() {
                let mut seen = vec![false; refs.len()];
                let mut work: Vec<usize> = rs
                    .iter()
                    .filter_map(|r| index.get(r.as_str()).copied())
                    .collect();
                while let Some(j) = work.pop() {
                    if j == i {
                        drop[i] = Some(format!(
                            "macro `{}` recurses (macros may not recurse)",
                            cfg.macros[i].value.name
                        ));
                        break;
                    }
                    if std::mem::replace(&mut seen[j], true) {
                        continue;
                    }
                    work.extend(
                        refs[j]
                            .iter()
                            .filter_map(|r| index.get(r.as_str()).copied()),
                    );
                }
            }
        }
        if drop.iter().all(Option::is_none) {
            break;
        }
        let old = std::mem::take(&mut cfg.macros);
        for (m, why) in old.into_iter().zip(drop) {
            match why {
                Some(msg) => diags.push(diag_at(&m.origin, None, msg)),
                None => cfg.macros.push(m),
            }
        }
    }
}

/// Follow `inherits` from each profile with its own visited set. A profile with an unknown
/// ancestor is dropped; the members of a cycle are blamed (and only they), reporting the path.
fn check_profiles(cfg: &mut Config, diags: &mut Vec<Diagnostic>) {
    loop {
        let names: Vec<String> = cfg.profiles.iter().map(|p| p.value.name.clone()).collect();
        let mut bad: Option<(usize, String)> = None;
        'profiles: for (i, p) in cfg.profiles.iter().enumerate() {
            let mut path: Vec<usize> = vec![i];
            let mut cur = p.value.inherits.clone();
            while let Some(parent) = cur {
                let Some(j) = names.iter().position(|n| *n == parent) else {
                    bad = Some((
                        i,
                        format!("profile `{}` inherits unknown profile `{parent}`", names[i]),
                    ));
                    break 'profiles;
                };
                if let Some(at) = path.iter().position(|&k| k == j) {
                    // A cycle, but only a member of it is to blame: a profile that merely
                    // leads into one is dropped later, once its ancestor is gone.
                    if at == 0 {
                        let mut chain: Vec<&str> =
                            path.iter().map(|&k| names[k].as_str()).collect();
                        chain.push(names[j].as_str());
                        bad = Some((
                            i,
                            format!(
                                "profile `{}` is in an inheritance cycle: {}",
                                names[i],
                                chain.join(" -> ")
                            ),
                        ));
                        break 'profiles;
                    }
                    break;
                }
                path.push(j);
                cur = cfg.profiles[j].value.inherits.clone();
            }
        }
        match bad {
            Some((i, msg)) => {
                let p = cfg.profiles.remove(i);
                diags.push(diag_at(&p.origin, None, msg));
            }
            None => break,
        }
    }
}

fn valid_env_name(s: &str) -> bool {
    let mut it = s.chars();
    matches!(it.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && it.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub(crate) type MapFilter = fn(&str, &[String], &[String]) -> Result<(), String>;

/// The per-entry checks of the policy-carrying map keys. Shared with the runtime-override
/// and editor gate so what they accept is what the next load accepts.
pub(crate) fn map_filters() -> [(&'static str, MapFilter); 4] {
    [
        ("open.handlers", |k, v, _| {
            crate::lang::validate_open_handler(k, v)
        }),
        ("socket.grants", |k, v, extra| {
            if !k.starts_with('/') {
                return Err(format!("grant path `{k}` must be absolute"));
            }
            if lang::is_interpreter(k, extra) {
                return Err(format!(
                    "`{k}` is an interpreter; grants to interpreters are refused"
                ));
            }
            for c in v {
                if c == "keys" {
                    return Err("`keys` cannot be granted here (use socket.key-stream)".to_string());
                }
                if !lang::CAPABILITY_CLASSES.contains(&c.as_str()) {
                    return Err(format!("unknown capability class `{c}`"));
                }
            }
            Ok(())
        }),
        ("security.sequence", |k, v, _| {
            if !lang::SEQUENCE_CLASSES.contains(&k) {
                return Err(format!("unknown sequence class `{k}`"));
            }
            if v.is_empty()
                || v.len() > 2
                || v.iter().any(|d| !lang::DECISIONS.contains(&d.as_str()))
            {
                return Err(format!(
                    "`{k}` takes `local [remote]`, each allow, ask or deny"
                ));
            }
            Ok(())
        }),
        ("shell.env", |k, v, _| {
            if !valid_env_name(k) {
                return Err(format!("`{k}` is not a valid environment variable name"));
            }
            if v.len() != 1 || v[0].contains('\0') {
                return Err(format!("`{k}` takes one value without NUL"));
            }
            Ok(())
        }),
    ]
}

/// Per-entry checks of the map keys that carry policy (T-12 §4, T-18 §3): invalid entries are
/// dropped with a diagnostic, the rest stays.
fn check_maps(cfg: &mut Config, diags: &mut Vec<Diagnostic>) {
    let extra: Vec<String> = cfg.list("socket.interpreters").to_vec();
    for (path, check) in map_filters() {
        let Some(idx) = schema::index_of(path) else {
            continue;
        };
        let slot = cfg.slots[idx].clone();
        if let Value::Map(entries) = &slot.value {
            let mut kept = Vec::new();
            for (k, v) in entries {
                match check(k, v, &extra) {
                    Ok(()) => kept.push((k.clone(), v.clone())),
                    Err(m) => diags.push(diag_at(&slot.origin, Some(path), m)),
                }
            }
            if kept.len() != entries.len() {
                cfg.slots[idx].value = Value::Map(kept);
            }
        }
    }
}

/// Resolve a profile and its ancestors (base first) into `(path, value)` pairs.
pub fn profile_values(cfg: &Config, name: &str) -> Vec<(String, Value)> {
    let mut chain: Vec<&Spanned<ProfileDecl>> = Vec::new();
    let mut cur = Some(name.to_string());
    while let Some(n) = cur {
        match cfg.profiles.iter().find(|p| p.value.name == n) {
            Some(p) if !chain.iter().any(|c| c.value.name == p.value.name) => {
                cur = p.value.inherits.clone();
                chain.push(p);
            }
            _ => break,
        }
    }
    let mut out: Vec<(String, Value)> = Vec::new();
    for p in chain.iter().rev() {
        for (k, v) in &p.value.values {
            out.retain(|(ek, _)| ek != k);
            out.push((k.clone(), v.clone()));
        }
    }
    out
}

/// Apply a profile as layer 6 (explicit choice or a matched profile).
pub fn apply_profile(cfg: &mut Config, name: &str) -> bool {
    let Some(decl) = cfg.profiles.iter().find(|p| p.value.name == name).cloned() else {
        return false;
    };
    for (path, value) in profile_values(cfg, name) {
        let origin = Origin {
            layer: Layer::Profile,
            ..decl.origin.clone()
        };
        cfg.set_at(&path, value, origin);
    }
    true
}

/// Load every layer into one config.
pub fn load(fs: &dyn Fs, input: &LoadInput, prev: Option<&Config>) -> LoadResult {
    let mut loader = Loader {
        fs,
        env: &input.env,
        diags: Vec::new(),
        files: Vec::new(),
        stack: Vec::new(),
        rejected: false,
        budget_reported: false,
    };
    let mut cfg = Config::defaults();
    // Per slot: the last layer position that failed validation / that set a valid value.
    let mut last_fail: std::collections::HashMap<usize, usize> = Default::default();
    let mut last_set: std::collections::HashMap<usize, usize> = Default::default();

    // Layers 2-4 read one logical stream each, so an include sits at its own position.
    let mut layered: Vec<Out> = Vec::new();
    if let Some(p) = &input.system {
        let mut out = Out::default();
        loader.load_file(p, Layer::System, &mut out, 0, Mode::Full, None);
        layered.push(out);
    }
    {
        let mut out = Out::default();
        for p in &input.fragments {
            loader.load_file(p, Layer::Fragment, &mut out, 0, Mode::Fragment, None);
        }
        layered.push(out);
    }
    if let Some(p) = &input.user {
        let mut out = Out::default();
        loader.load_file(p, Layer::User, &mut out, 0, Mode::Full, None);
        layered.push(out);
    }
    // Layer 5: each file in isolation; a broken file loses only itself.
    for p in &input.profile_files {
        let mut out = Out::default();
        loader.load_file(p, Layer::ProfileDir, &mut out, 0, Mode::ProfileFile, None);
        layered.push(out);
    }
    for (pos, mut out) in layered.into_iter().enumerate() {
        for idx in out.failed.drain(..) {
            last_fail.insert(idx, pos);
        }
        for s in &out.settings {
            last_set.insert(s.idx, pos);
        }
        merge(&mut cfg, out);
    }

    if cfg.binds.len() > MAX_BINDS {
        for b in cfg.binds.drain(MAX_BINDS..) {
            loader.diags.push(diag_at(
                &b.origin,
                None,
                format!("bind dropped: more than {MAX_BINDS} binds"),
            ));
        }
    }
    if cfg.rules.len() > MAX_RULES {
        for r in cfg.rules.drain(MAX_RULES..) {
            loader.diags.push(diag_at(
                &r.origin,
                None,
                format!(
                    "rule `{}` dropped: more than {MAX_RULES} rules",
                    r.value.name
                ),
            ));
        }
    }
    check_macros(&mut cfg, &mut loader.diags);
    check_profiles(&mut cfg, &mut loader.diags);
    check_maps(&mut cfg, &mut loader.diags);

    // A bad scalar keeps the previous good value, unless the same or a higher layer
    // supplied a valid one. Done before profiles so those still win.
    if let Some(prev) = prev {
        for (idx, fpos) in &last_fail {
            if last_set.get(idx).is_none_or(|spos| spos < fpos) {
                cfg.slots[*idx] = prev.slots[*idx].clone();
            }
        }
    }

    // Layer 6.
    for name in &input.active_profiles {
        if !apply_profile(&mut cfg, name) {
            loader.diags.push(Diagnostic {
                severity: Severity::Error,
                file: None,
                line: 0,
                column: 0,
                key: None,
                message: format!("profile `{name}` is not defined"),
                suggestion: suggest(
                    name,
                    &cfg.profiles
                        .iter()
                        .map(|p| p.value.name.clone())
                        .collect::<Vec<_>>(),
                ),
            });
        }
    }

    // Layer 7.
    for (key, text) in &input.launch {
        let origin = Origin::at(Layer::Launch);
        match schema::index_of(key) {
            None => {
                let all: Vec<&str> = TABLE.iter().map(|k| k.path).collect();
                loader.diags.push(Diagnostic {
                    severity: Severity::Error,
                    file: None,
                    line: 0,
                    column: 0,
                    key: Some(key.clone()),
                    message: format!("-o: unknown key `{key}`"),
                    suggestion: suggest(key, &all),
                });
            }
            Some(idx) => match parse_override_value(&TABLE[idx], text) {
                Ok(v) => cfg.set_slot(idx, v, origin),
                Err(m) => loader.diags.push(Diagnostic {
                    severity: Severity::Error,
                    file: None,
                    line: 0,
                    column: 0,
                    key: Some(key.clone()),
                    message: format!("-o {key}: {m}"),
                    suggestion: None,
                }),
            },
        }
    }

    let mut files = loader.files;
    files.dedup();
    let mut diagnostics = loader.diags;
    if diagnostics.len() > MAX_DIAGNOSTICS {
        diagnostics.truncate(MAX_DIAGNOSTICS);
        diagnostics.push(Diagnostic {
            severity: Severity::Error,
            file: None,
            line: 0,
            column: 0,
            key: None,
            message: format!("more than {MAX_DIAGNOSTICS} problems; the rest are not listed"),
            suggestion: None,
        });
    }
    LoadResult {
        config: cfg,
        diagnostics,
        files,
        rejected: loader.rejected,
    }
}
