// SPDX-License-Identifier: AGPL-3.0-only
//! KDL configuration (COMP-13 §1).
//!
//! Search path, later files overriding earlier ones:
//!   0. `/usr/share/eclipse/widgets/*.kdl`, the premade widget catalog, only
//!      `bar { widget … }` and only while `taskbar-widgets` is on (ADR 0067)
//!   1. `/etc/eclipse/abyss.kdl`
//!   2. `$XDG_CONFIG_HOME/eclipse/abyss.kdl`
//!   3. `$XDG_CONFIG_HOME/eclipse/abyss.d/*.kdl` (sorted)
//!   4. `$XDG_CONFIG_HOME/abyss/config.kdl` (compatibility fallback)
//!      `--config <path>` replaces the whole search path with that one file.
//!
//! Validation is total (COMP-13 §1.2): an unknown key or a malformed value is
//! an error, not a warning. Refusals are collected in [`Config::errors`] as
//! [`ConfigError`]s carrying `file:line:col` and the offending token; the
//! caller decides what to do with them. Startup (COMP-01 §5 step 3, amended by
//! ADR 0064) drops each rejected node, logs the refusal and starts, unless one
//! of them is [`ConfigError::startup_fatal`] — see [`Config::startup`].
//! Hot-reload (`ec-abyss`'s `config::watch::reload_now`) keeps the last good
//! config and emits a `config-error` IPC event; it never half-applies.
//!
//! This crate is the pure half: schema, parser, editor, approvals and widget
//! hashes. It has no smithay and no compositor state, so `ec-settings` and
//! `ec-ctl` can link it. Applying a loaded config (`apply_loaded`), the widget
//! catalog, the file watcher and the approval withholding stay in `ec-abyss`'s
//! `config/`, which re-exports this crate as `crate::config`.

pub mod animations;
pub mod approvals;
pub mod catalog;
pub mod edit;
pub mod input;
pub mod outputs;
pub mod schema;
pub mod transitions;
pub mod trust;
pub mod widget_hash;

mod apply_bar;
mod apply_decoration;
mod apply_general;
mod apply_windowrule;
mod bar;
mod binds;
mod decoration;
mod devices;
mod errors;
mod general;
mod parse;
#[cfg(test)]
pub(crate) mod tests;
mod windowrule;

pub use bar::*;
pub use binds::*;
pub use decoration::*;
pub use devices::*;
pub use errors::*;
pub use general::*;
pub use parse::*;
pub use windowrule::*;

use std::path::{Path, PathBuf};

use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};
use xkbcommon::xkb::{self, Keysym};

use crate::input::{
    Action, Bind, Direction, DragGesture, GestureBind, Mods, MouseAction, MouseBind, MouseButton,
};
use crate::trust::{AppTrust, SeatCompat};

/// `animations { ... }` (COMP-13 §1.1, COMP-02 §9): see [`animations`].
/// Animations are render-only and must never change what an agent sees:
/// `scene`/`get_tree` always report target geometry, not the interpolated
/// value (COMP-08).
pub use animations::Animations;

// Easing curves live in `schema` (EASING_CURVES). Unknown values are
// rejected at parse time rather than silently ignored at render time.
use schema::EASING_CURVES;

/// Parse `text` as one abyss-owned file `a.kdl`, with no search path and no
/// widget catalog. For tests in this crate and in `ec-abyss`, which cannot
/// reach `Config`'s private parse state.
#[doc(hidden)]
pub fn parse_single_for_tests(text: &str) -> Config {
    let doc: KdlDocument = text.parse().unwrap();
    let mut cfg = Config {
        cur: Some((
            Source {
                path: PathBuf::from("a.kdl"),
                owner: schema::Owner::Abyss,
            },
            text.to_owned(),
        )),
        ..Config::default()
    };
    cfg.apply(&doc, &mut Vec::new());
    cfg
}

/// One config file and the half of the surface it is allowed to set.
///
/// COMP-13 §1.3: `abyss.kdl` holds everything the human may retune freely;
/// `policy.kdl` holds the security surface (`schema::Owner::Policy`). Keeping
/// them apart is what lets the config GUI hold a write capability for one and
/// not the other, and what lets a deployment ship policy.kdl root-owned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub path: PathBuf,
    pub owner: schema::Owner,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub general: General,
    pub render: Render,
    pub bar: Bar,
    pub decoration: Decoration,
    pub animations: Animations,
    pub clipboard: Clipboard,
    pub capture: Capture,
    pub xwayland: Xwayland,
    pub idle: Idle,
    pub misc: Misc,
    pub setup: Setup,
    pub launcher: Launcher,
    pub ui: Ui,
    pub settings: SettingsApp,
    pub mode: Mode,
    pub components: Components,
    pub wallpaper: Wallpaper,
    pub annotations: AnnotationColors,
    pub oracle_eyes: OracleEyes,
    pub input: Input,
    pub binds: Vec<Bind>,
    /// Touchpad swipe bindings, one per `(fingers, direction)`: the defaults
    /// with every `gesture` node merged over them.
    pub gesture_binds: Vec<GestureBind>,
    /// Modifier + mouse-button bindings, one per `(mods, button)`: the
    /// defaults with every `mousebind` node merged over them (COMP-04 §5).
    pub mouse_binds: Vec<MouseBind>,
    /// Touchpad window drags, one per finger count: the default with every
    /// `gesture "drag"` node merged over it (COMP-04 §2, amended C-12).
    pub drag_gestures: Vec<DragGesture>,
    /// Per-workspace layout overrides, indexed 1..=10.
    pub workspace_layout: [Option<LayoutKind>; 10],
    /// `output` blocks in file order; the last match wins.
    pub outputs: Vec<OutputRule>,
    /// `windowrule` blocks in file order; all matching rules apply, later ones
    /// overriding earlier ones for the same property.
    pub window_rules: Vec<WindowRule>,
    /// Files this config was built from, in load order. The hot-reload
    /// watcher watches these and the directories that would contain them.
    pub sources: Vec<Source>,
    /// `--config <path>`, if one was given. Reload must honour it rather than
    /// falling back to the search path.
    pub explicit: Option<PathBuf>,
    /// The premade catalog was read as the lowest layer (ADR 0067). Set from
    /// the `taskbar-widgets` hook; reload keeps it.
    pub catalog_layer: bool,
    /// The catalog blocks as shipped, before any user block replaced one of
    /// the same name: the hash reference for `withhold::settle`.
    pub catalog: Vec<CustomWidget>,
    /// Validation refusals collected during the last load. On a freshly
    /// loaded config, non-empty means invalid: hot-reload keeps the last good
    /// one. On the *live* config it is non-empty only after a startup that
    /// dropped nodes (ADR 0064): those refusals stay here and are replayed to
    /// every `config-error` subscriber until a clean load replaces the config.
    pub errors: Vec<ConfigError>,
    /// The file being parsed, and its text, so `reject` can turn a KDL span
    /// into `file:line:col`. Cleared when the load finishes.
    cur: Option<(Source, String)>,
    /// `custom:<name>` ids in the file being parsed, checked at its end.
    pending_custom: Vec<PendingCustom>,
    /// Names of every named `widget` block seen, valid or refused. A refused
    /// block already has its own positioned error, so a `custom:` id naming
    /// it is not also reported missing.
    widget_blocks: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            general: General::default(),
            render: Render::default(),
            bar: Bar::default(),
            decoration: Decoration::default(),
            animations: Animations::default(),
            clipboard: Clipboard::default(),
            capture: Capture::default(),
            xwayland: Xwayland::default(),
            idle: Idle::default(),
            misc: Misc::default(),
            setup: Setup::default(),
            launcher: Launcher::default(),
            ui: Ui::default(),
            settings: SettingsApp::default(),
            mode: Mode::Hybrid,
            components: Components::default(),
            wallpaper: Wallpaper::default(),
            annotations: AnnotationColors::default(),
            oracle_eyes: OracleEyes::default(),
            input: Input::default(),
            binds: default_binds(),
            gesture_binds: default_gesture_binds(),
            mouse_binds: default_mouse_binds(),
            drag_gestures: default_drag_gestures(),
            workspace_layout: Default::default(),
            outputs: Vec::new(),
            window_rules: Vec::new(),
            sources: Vec::new(),
            explicit: None,
            catalog_layer: false,
            catalog: Vec::new(),
            errors: Vec::new(),
            cur: None,
            pending_custom: Vec::new(),
            widget_blocks: Vec::new(),
        }
    }
}

impl Config {
    /// Load from `explicit` if given, else from the search path. Validation is
    /// total (COMP-13 §1.2): any refusal lands in `errors`, and the caller
    /// decides — startup exits, hot-reload keeps the last good config.
    pub fn load(explicit: Option<&Path>) -> Self {
        Self::load_with(explicit, false)
    }

    /// [`Config::load`], with the premade widget catalog read first as the
    /// lowest layer when `catalog_layer` is set (ADR 0067). Its blocks count
    /// as defined for `custom:<name>` ids, and a later block of the same name
    /// replaces one, exactly like a block in `/etc` would be replaced.
    pub fn load_with(explicit: Option<&Path>, catalog_layer: bool) -> Self {
        // An explicit `--config` names one abyss.kdl; policy stays on the
        // search path, since a flag must not be able to swap the policy file.
        let files: Vec<Source> = match explicit {
            Some(p) => {
                let mut v = vec![Source {
                    path: p.to_path_buf(),
                    owner: schema::Owner::Abyss,
                }];
                v.extend(
                    search_path()
                        .into_iter()
                        .filter(|s| s.owner == schema::Owner::Policy),
                );
                v
            }
            None => search_path(),
        };
        let mut cfg = Config {
            explicit: explicit.map(|p| p.to_path_buf()),
            catalog_layer,
            ..Config::default()
        };
        if catalog_layer {
            cfg.catalog = catalog::load();
            cfg.bar.custom_widgets = cfg.catalog.clone();
            cfg.widget_blocks = cfg.catalog.iter().map(|w| w.name.clone()).collect();
        }
        let mut binds_from_file = Vec::new();
        let mut any = false;
        for f in &files {
            let text = match std::fs::read_to_string(&f.path) {
                Ok(t) => t,
                Err(e) => {
                    if explicit.is_some() && f.owner == schema::Owner::Abyss {
                        cfg.errors.push(ConfigError {
                            file: f.path.clone(),
                            line: 0,
                            col: 0,
                            message: format!("config unreadable: {e}"),
                            snippet: None,
                            span_len: 0,
                            // ADR 0064: an unreadable `--config` contributes
                            // nothing and Abyss starts on the rest.
                            startup_fatal: false,
                            fail_safe: None,
                        });
                    }
                    continue;
                }
            };
            let doc = match text.parse::<KdlDocument>() {
                Ok(d) => d,
                Err(e) => {
                    // Prefer the first diagnostic: it carries the position and the
                    // "expected ..." help, where the outer error is only generic.
                    let (off, message) = match e.diagnostics.first() {
                        Some(d) => {
                            let mut m = d.message.clone().unwrap_or_else(|| "invalid syntax".to_string());
                            if let Some(help) = &d.help {
                                m.push_str(" (");
                                m.push_str(help);
                                m.push(')');
                            }
                            ((d.span.offset(), d.span.len()), m)
                        }
                        None => ((0, 0), format!("{e}")),
                    };
                    let (line, col, snippet, span_len) = locate(&text, off.0, off.1);
                    cfg.errors.push(ConfigError {
                        file: f.path.clone(),
                        line,
                        col,
                        message,
                        snippet: Some(snippet),
                        span_len,
                        // ADR 0064: a file that is not KDL is dropped whole —
                        // unless it is `policy.kdl`, which stays fail-closed.
                        startup_fatal: f.owner == schema::Owner::Policy,
                        fail_safe: None,
                    });
                    continue;
                }
            };
            any = true;
            cfg.sources.push(f.clone());
            cfg.cur = Some((f.clone(), text));
            cfg.apply(&doc, &mut binds_from_file);
            cfg.cur = None;
        }
        let mut defaults = oracle_eyes_binds(&cfg.oracle_eyes);
        defaults.extend(defaults_with_launcher(&cfg.launcher));
        cfg.binds = merge_binds(defaults, binds_from_file);
        if !any {
            tracing::info!("no config found, using built-in defaults");
        } else {
            tracing::info!(sources = ?cfg.sources.iter().map(|s| &s.path).collect::<Vec<_>>(), binds = cfg.binds.len(), "config loaded");
        }
        cfg
    }

    /// Re-read the same sources. The inotify watcher (`config::watch`) calls
    /// this and must check `errors` before applying the result.
    pub fn reload(&self) -> Self {
        Self::load_with(self.explicit.as_deref(), self.catalog_layer)
    }

    /// Validate `text` as if it were the file at `path` owned by `owner`,
    /// without touching disk or the live config.
    ///
    /// This is what `validate_config` answers with, and it is deliberately the
    /// same code the loader runs: a GUI that previews an edit must be told
    /// exactly what a file edit would have been told, down to the wording.
    pub fn check_text(path: &Path, owner: schema::Owner, text: &str) -> Vec<ConfigError> {
        Self::check_text_with(path, owner, text, &[])
    }

    /// [`Config::check_text`], with `known` widget names (the premade
    /// catalog's) counting as defined for `custom:<name>` ids, as they do in
    /// the real load.
    pub fn check_text_with(
        path: &Path,
        owner: schema::Owner,
        text: &str,
        known: &[CustomWidget],
    ) -> Vec<ConfigError> {
        let doc = match text.parse::<KdlDocument>() {
            Ok(d) => d,
            Err(e) => {
                let (off, message) = match e.diagnostics.first() {
                    Some(d) => (
                        (d.span.offset(), d.span.len()),
                        d.message.clone().unwrap_or_else(|| "invalid syntax".to_string()),
                    ),
                    None => ((0, 0), format!("{e}")),
                };
                let (line, col, snippet, span_len) = locate(text, off.0, off.1);
                return vec![ConfigError {
                    file: path.to_path_buf(),
                    line,
                    col,
                    message,
                    snippet: Some(snippet),
                    span_len,
                    startup_fatal: owner == schema::Owner::Policy,
                    fail_safe: None,
                }];
            }
        };
        let mut cfg = Config {
            cur: Some((
                Source {
                    path: path.to_path_buf(),
                    owner,
                },
                text.to_owned(),
            )),
            widget_blocks: known.iter().map(|w| w.name.clone()).collect(),
            ..Config::default()
        };
        cfg.apply(&doc, &mut Vec::new());
        cfg.errors
    }

    /// Record a validation refusal against `node`'s position (COMP-13 §1.2).
    /// Also logged, so the journal shows the same text the caller gets.
    /// The parser's `unknown key` fallthrough, checked against the schema.
    ///
    /// This is the second of the three anti-drift sides in `schema.rs`: it runs
    /// at parse time, on the real user's file. A name the parser does not
    /// handle but the schema *does* claim is a wiring bug in this file, and the
    /// message says so rather than telling the human their config is wrong.
    fn unknown_key(&mut self, node: &KdlNode, prefix: &str, what: &str) {
        let name = node.name().value();
        let path = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}.{name}")
        };
        match schema::get_key(&path) {
            Some(_) => self.reject(
                node,
                format!("{path} is in the config schema but is not wired into the parser \u{2014} this is a bug in abyss, not in your config"),
            ),
            None => {
                let hint = did_you_mean(&path);
                self.reject(
                    node,
                    match hint {
                        Some(h) => format!("unknown {what} {name:?} (did you mean {h:?}?)"),
                        None => format!("unknown {what} {name:?}"),
                    },
                )
            }
        }
    }

    /// COMP-13 §1.3: refuse a key that belongs in the other file.
    ///
    /// Both directions. A policy key in `abyss.kdl` is the one that matters —
    /// `abyss.kdl` is writable by the config GUI and `policy.kdl` is not, so
    /// accepting it there would be a way around the write gate. The reverse is
    /// refused too, so `policy.kdl` stays small enough to read.
    ///
    /// The message names the file the key does belong in; a refusal the human
    /// cannot act on is a worse bug than the one it reports.
    fn owned_here(&mut self, node: &KdlNode, what: &str, owner: schema::Owner) -> bool {
        let Some((src, _)) = &self.cur else { return true };
        if src.owner == owner {
            return true;
        }
        let dest = match owner {
            schema::Owner::Abyss => "abyss.kdl",
            schema::Owner::Policy => "policy.kdl",
        };
        // ADR 0064 fail-closed case 2: a misplaced policy key is a protection
        // the owner meant to have; dropping it would silently discard it.
        self.reject_fatal(node, format!("{what} belongs in {dest}, not in this file"));
        false
    }

    fn reject(&mut self, node: &KdlNode, message: impl Into<String>) {
        // Underline the node name only; the node's own span runs to the end of
        // its children, which would drown the line in carets.
        self.reject_at(node.span().offset(), node.name().value().len(), message);
    }

    /// Refuse one argument of a node, underlining that argument rather than
    /// the node name: in `order "clock" "clokc"` the caret belongs under the
    /// typo. An entry's span may start at the whitespace before it.
    fn reject_entry(&mut self, entry: &KdlEntry, message: impl Into<String>) {
        let span = entry.span();
        let (offset, len) = match &self.cur {
            Some((_, text)) => {
                let raw = text.get(span.offset()..span.offset() + span.len()).unwrap_or("");
                let lead = raw.len() - raw.trim_start().len();
                (span.offset() + lead, raw.trim().len())
            }
            None => (span.offset(), span.len()),
        };
        self.reject_at(offset, len, message);
    }

    fn reject_at(&mut self, offset: usize, len: usize, message: impl Into<String>) {
        let message = message.into();
        let (file, line, col, snippet, span_len, policy) = match &self.cur {
            Some((f, text)) => {
                let (line, col, snippet, len) = locate(text, offset, len);
                (
                    f.path.clone(),
                    line,
                    col,
                    Some(snippet),
                    len,
                    f.owner == schema::Owner::Policy,
                )
            }
            None => (PathBuf::new(), 0, 0, None, 0, false),
        };
        tracing::error!(path = %file.display(), line, col, %message, "invalid config");
        self.errors.push(ConfigError {
            file,
            line,
            col,
            message,
            snippet,
            span_len,
            // ADR 0064 fail-closed case 1: nothing in `policy.kdl` is dropped
            // to a default at startup.
            startup_fatal: policy,
            fail_safe: None,
        });
    }

    /// Mark the refusal just recorded as a protection that did not take.
    fn fail_safe_last(&mut self, g: FailSafe) {
        if let Some(e) = self.errors.last_mut() {
            e.fail_safe = Some(g);
        }
    }

    /// [`Config::reject`] for a node whose default is not safe to fall back
    /// to: Abyss refuses to start on it rather than dropping it (ADR 0064).
    fn reject_fatal(&mut self, node: &KdlNode, message: impl Into<String>) {
        self.reject(node, message);
        if let Some(e) = self.errors.last_mut() {
            e.startup_fatal = true;
        }
    }

    /// What startup does with this config (COMP-01 §5 step 3, amended by
    /// ADR 0064): start with every rejected node dropped, or refuse when any
    /// refusal is in the fail-closed set.
    ///
    /// Starting also settles the [`FailSafe`] refusals: any refusal touching
    /// `xwayland` turns Xwayland off, and an auto-lock notice is kept only
    /// when auto-lock really is off (a lower-precedence file may still have
    /// set both lock keys), so the summary never claims what is not true.
    pub fn startup(&mut self) -> Startup {
        let fatal = self.errors.iter().filter(|e| e.startup_fatal).count();
        if fatal > 0 {
            return Startup::Refuse { fatal };
        }
        if self
            .errors
            .iter()
            .any(|e| e.fail_safe == Some(FailSafe::XwaylandOff))
        {
            self.xwayland.enable = false;
        }
        // A refused `widget` block is dropped whole, but its name was already
        // claimed, so a `custom:<name>` placed by `order`/`important` survived
        // validation. Drop those ids too: the bar never lists a widget that
        // has no block (ADR 0064 with ADR 0065). Withholding (ADR 0067) runs
        // later on what is left.
        let blocks: Vec<String> = self.bar.custom_widgets.iter().map(|w| w.name.clone()).collect();
        let listed = |id: &String| match id.strip_prefix(schema::BAR_WIDGET_CUSTOM_PREFIX) {
            Some(name) => blocks.iter().any(|b| b == name),
            None => true,
        };
        self.bar.widgets.order.retain(listed);
        self.bar.widgets.important.retain(listed);
        if self.idle.lock_command.is_some() && self.idle.lock_timeout.is_some() {
            for e in &mut self.errors {
                if e.fail_safe == Some(FailSafe::AutoLockOff) {
                    e.fail_safe = None;
                }
            }
        }
        Startup::Start {
            ignored: self.errors.len(),
        }
    }

    fn apply(&mut self, doc: &KdlDocument, binds: &mut Vec<Bind>) {
        let launcher_style_set = doc.nodes().iter().any(|n| {
            n.name().value() == "launcher"
                && n.children()
                    .is_some_and(|c| c.nodes().iter().any(|k| k.name().value() == "style"))
        });
        for node in doc.nodes() {
            let name = node.name().value();
            // Whole-node ownership, before the node is parsed at all. `misc`
            // and `windowrule` are mixed and check themselves, one key or one
            // action at a time.
            if let Some(owner) = schema::node_owner(name) {
                if !self.owned_here(node, &format!("{name:?}"), owner) {
                    continue;
                }
            }
            match name {
                "general" => self.apply_general(node),
                "bind" => match parse_bind(node) {
                    Ok(b) => binds.push(b),
                    Err(e) => self.reject(node, format!("ignoring bind (error={})", e)),
                },
                // Same merge rule as `merge_binds`, applied as each node
                // arrives: a later entry for the same `(fingers, direction)`
                // replaces the earlier one (default or file) in place.
                "gesture" => match parse_gesture(node) {
                    // A finger count is either swiped or dragged, never both:
                    // the begin could not tell them apart. Whichever node
                    // comes second is the one refused.
                    Ok(ParsedGesture::Swipe(g)) if self.drag_gestures.iter().any(|d| d.fingers == g.fingers) => {
                        self.reject(
                            node,
                            format!(
                                "ignoring gesture (error={n}-finger swipe collides with the {n}-finger drag gesture; disable that with gesture \"drag\" {n} {{ none; }})",
                                n = g.fingers
                            ),
                        )
                    }
                    Ok(ParsedGesture::Swipe(g)) => match self
                        .gesture_binds
                        .iter_mut()
                        .find(|o| o.fingers == g.fingers && o.direction == g.direction)
                    {
                        Some(slot) => *slot = g,
                        None => self.gesture_binds.push(g),
                    },
                    Ok(ParsedGesture::Drag(n, Some(_))) if self.gesture_bound(n) => self.reject(
                        node,
                        format!("ignoring gesture (error={n}-finger drag collides with a bound {n}-finger swipe)"),
                    ),
                    // Keyed on fingers: a later entry replaces, `none` removes.
                    Ok(ParsedGesture::Drag(fingers, mods)) => {
                        self.drag_gestures.retain(|d| d.fingers != fingers);
                        if let Some(mods) = mods {
                            self.drag_gestures.push(DragGesture { fingers, mods });
                        }
                    }
                    Err(e) => self.reject(node, format!("ignoring gesture (error={})", e)),
                },
                // Same replace-in-place rule, keyed on `(mods, button)`.
                "mousebind" => match parse_mousebind(node) {
                    Ok(b) => match self
                        .mouse_binds
                        .iter_mut()
                        .find(|o| o.mods == b.mods && o.button == b.button)
                    {
                        Some(slot) => *slot = b,
                        None => self.mouse_binds.push(b),
                    },
                    Err(e) => self.reject(node, format!("ignoring mousebind (error={})", e)),
                },
                "workspace" => self.apply_workspace(node),
                "render" => self.apply_render(node),
                "bar" => self.apply_bar(node, launcher_style_set),
                "launcher" => self.apply_launcher(node),
                "ui" => self.apply_ui(node),
                "settings" => self.apply_settings(node),
                "clipboard" => self.apply_clipboard(node),
                "capture" => self.apply_capture(node),
                "xwayland" => self.apply_xwayland(node),
                "idle" => self.apply_idle(node),
                "misc" => self.apply_misc(node),
                "setup" => self.apply_setup(node),
                "mode" => match arg(node).and_then(KdlValue::as_string) {
                    Some("wm") => self.mode = Mode::Wm,
                    Some("hybrid") => self.mode = Mode::Hybrid,
                    Some("de") => self.mode = Mode::De,
                    other => self.reject(
                        node,
                        format!(
                            "mode: unknown value (other={other:?}); expected one of {}",
                            schema::MODES.join(", ")
                        ),
                    ),
                },
                "components" => self.apply_components(node),
                "wallpaper" => self.apply_wallpaper(node),
                "annotations" => self.apply_annotations(node),
                "oracle-eyes" => self.apply_oracle_eyes(node),
                "input" => self.apply_input(node),
                "output" => self.apply_output(node),
                "decoration" => self.apply_decoration(node),
                "animations" => self.apply_animations(node),
                "windowrule" => self.apply_windowrule(node),
                _ => {
                    self.unknown_key(node, "", "config node");
                    if let Some(g) = misspelt_guard(name) {
                        self.fail_safe_last(g);
                    }
                }
            }
        }
        self.check_custom_widget_ids();
    }

    /// Every `custom:<name>` in `bar.widgets.*` must name a `widget` block
    /// from this file or one loaded before it. Run at the end of each file
    /// so a block may follow its use, and while `cur` still points at the
    /// file the id is in.
    fn check_custom_widget_ids(&mut self) {
        for p in std::mem::take(&mut self.pending_custom) {
            if self.widget_blocks.contains(&p.name) {
                continue;
            }
            let near = nearest(&p.name, self.bar.custom_widgets.iter().map(|w| w.name.as_str()));
            let message = match near {
                Some(n) => format!(
                    "custom:{} names no widget block (did you mean \"custom:{n}\"?)",
                    p.name
                ),
                None => format!(
                    "custom:{} names no widget block; define one inside bar: widget {:?} {{ exec \"…\" }}",
                    p.name, p.name
                ),
            };
            self.reject_at(p.offset, p.len, message);
        }
    }

    /// The effective rule for an output, later blocks overriding earlier ones.
    pub fn output_rule(&self, connector: &str, identity: &str) -> OutputRule {
        let mut out = OutputRule::default();
        for r in &self.outputs {
            if !(glob_match(&r.pattern, connector) || glob_match(&r.pattern, identity)) {
                continue;
            }
            out.pattern = r.pattern.clone();
            out.mode = r.mode.or(out.mode);
            out.position = r.position.or(out.position);
            out.scale = r.scale.or(out.scale);
            out.transform = r.transform.clone().or(out.transform);
            out.enabled = r.enabled.or(out.enabled);
            out.lid_close = r.lid_close.clone().or(out.lid_close);
            out.vrr = r.vrr.or(out.vrr);
            out.overscan = r.overscan.or(out.overscan);
            out.number = r.number.or(out.number);
        }
        out
    }

    pub fn layout_for(&self, workspace: usize) -> LayoutKind {
        self.workspace_layout
            .get(workspace.saturating_sub(1))
            .copied()
            .flatten()
            .unwrap_or(self.general.layout)
    }

    /// `mods` is the held modifier set (a binding matches on equality).
    pub fn action_for(&self, mods: &Mods, key: Keysym) -> Option<&Action> {
        self.binds
            .iter()
            .find(|b| b.key == key && b.mods == *mods)
            .map(|b| &b.action)
    }

    /// Whether any `fingers`-finger swipe is bound. Asked at swipe begin,
    /// before the direction is known, to decide who owns the whole swipe.
    pub fn gesture_bound(&self, fingers: u32) -> bool {
        self.gesture_binds.iter().any(|g| g.fingers == fingers)
    }

    /// Whether a `fingers`-finger touchpad drag with exactly `mods` held moves
    /// a window. Asked once per gesture, at its begin; allocates nothing.
    pub fn drag_gesture_bound(&self, fingers: u32, mods: &Mods) -> bool {
        self.drag_gestures
            .iter()
            .any(|d| d.fingers == fingers && d.mods == *mods)
    }

    /// The mouse binding for `button` pressed with exactly `mods` held.
    /// Called on every button press, so it borrows and allocates nothing.
    pub fn mouse_bind_for(&self, mods: &Mods, button: u32) -> Option<MouseAction> {
        self.mouse_binds
            .iter()
            .find(|b| b.button.code() == button && b.mods == *mods)
            .map(|b| b.action)
    }

    pub fn gesture_for(&self, fingers: u32, direction: Direction) -> Option<&Action> {
        self.gesture_binds
            .iter()
            .find(|g| g.fingers == fingers && g.direction == direction)
            .map(|g| &g.action)
    }
}

/// Every file that may contribute, in apply order.
///
/// Each directory contributes its `policy.kdl` after its `abyss.kdl`, so a
/// policy-owned key set in the wrong file is refused rather than quietly
/// shadowed. There are deliberately no `policy.d/` drop-ins: the security
/// surface is one file per directory, so "what is the policy here" has one
/// answer a human can read.
/// `$XDG_CONFIG_HOME` (or `$HOME/.config`), the tier a normal user can write.
/// `None` when neither is set, which is the case for a daemon with no home.
#[must_use]
pub fn user_config_base() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
}

fn search_path() -> Vec<Source> {
    let abyss = |p: PathBuf| Source {
        path: p,
        owner: schema::Owner::Abyss,
    };
    let policy = |p: PathBuf| Source {
        path: p,
        owner: schema::Owner::Policy,
    };
    let mut out = vec![
        abyss(PathBuf::from("/etc/eclipse/abyss.kdl")),
        policy(PathBuf::from("/etc/eclipse/policy.kdl")),
    ];
    let Some(base) = user_config_base() else {
        return out;
    };
    out.push(abyss(base.join("eclipse/abyss.kdl")));
    if let Ok(dir) = std::fs::read_dir(base.join("eclipse/abyss.d")) {
        let mut drop_ins: Vec<PathBuf> = dir
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "kdl"))
            .collect();
        drop_ins.sort();
        out.extend(drop_ins.into_iter().map(abyss));
    }
    out.push(policy(base.join("eclipse/policy.kdl")));
    // Compatibility with the path used by the M2 task brief.
    out.push(abyss(base.join("abyss/config.kdl")));
    out
}
