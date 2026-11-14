// SPDX-License-Identifier: AGPL-3.0-only
//! Hot reload (T-10 §4), watcher-agnostic.
//!
//! The watcher (inotify on the parent directories, 100 ms debounce, poll fallback) lives in
//! the integration layer; it calls [`Reloader::reload`] once per debounced burst. This module
//! parses the new files, validates, diffs against the live config and says which subsystems
//! the change touches. A file that fails to parse keeps the last good config live and hands
//! the structured diagnostics (file, line, column, key, suggestion) to the glass error bar.
//! Nothing here logs or stores values.

use crate::diag::Diagnostic;
use crate::diff::{Collection, ConfigDiff, KeyChange, diff};
use crate::fs::{Fs, SelfWrites};
use crate::load::{LoadInput, load};
use crate::model::Config;
use crate::types::Reload;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// A part of the terminal that has to react to a config change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Subsystem {
    /// Frame pacing, blending, GPU choice (`render`).
    Render,
    /// Fonts, metrics, glyph policy, unicode width (`font`, `unicode`).
    Font,
    /// Colours, theme, cursor/scrollbar look, window chrome, tabs and pane frames, interface.
    Appearance,
    /// Window creation and geometry (`window`).
    Window,
    /// Key encoding, binds, key tables, macros, repeat, compose (`keyboard`, `binds`, ...).
    Keys,
    /// Pointer and touch behaviour, selection.
    Pointer,
    /// Shell, TERM, environment: applies to the next spawned child (`shell`, `terminal`).
    Pty,
    /// Scrollback, blocks, bell, notifications, progress, shell integration, hints, links.
    Term,
    /// Clipboard, paste, security classes, remote clients, socket grants.
    Security,
    /// Graphics protocols and media.
    Media,
    /// Tabs, panes, profiles, layouts, sessions, domains, quick terminal, rules.
    Mux,
    /// Accessibility.
    A11y,
}

impl Subsystem {
    pub fn name(self) -> &'static str {
        match self {
            Subsystem::Render => "render",
            Subsystem::Font => "font",
            Subsystem::Appearance => "appearance",
            Subsystem::Window => "window",
            Subsystem::Keys => "keys",
            Subsystem::Pointer => "pointer",
            Subsystem::Pty => "pty",
            Subsystem::Term => "term",
            Subsystem::Security => "security",
            Subsystem::Media => "media",
            Subsystem::Mux => "mux",
            Subsystem::A11y => "a11y",
        }
    }
}

/// Which subsystem owns a key, by its path. Unknown top-level nodes fall under `Term`, the
/// conservative "re-read per pane" choice; the table test below proves every real key lands
/// somewhere deliberate.
pub fn subsystem_of(path: &str) -> Subsystem {
    let top = path.split('.').next().unwrap_or(path);
    match top {
        "render" => Subsystem::Render,
        "font" | "unicode" => Subsystem::Font,
        "colors" | "theme" | "cursor" | "scrollbar" | "interface" => Subsystem::Appearance,
        "window" => Subsystem::Window,
        "keyboard" | "binds" | "key-table" | "macro" => Subsystem::Keys,
        "mouse" | "mouse-binds" | "touch" | "selection" => Subsystem::Pointer,
        "shell" | "terminal" => Subsystem::Pty,
        "scrollback" | "blocks" | "bell" | "notifications" | "progress" | "shell-integration"
        | "hints" | "links" => Subsystem::Term,
        "clipboard" | "paste" | "security" | "remote" | "socket" => Subsystem::Security,
        "graphics" | "media" => Subsystem::Media,
        "tabs" | "panes" | "close" | "profile" | "layout" | "menu" | "sessions" | "domain"
        | "quick-terminal" | "rule" => Subsystem::Mux,
        "accessibility" => Subsystem::A11y,
        _ => Subsystem::Term,
    }
}

fn collection_subsystem(c: Collection) -> Subsystem {
    match c {
        Collection::Binds | Collection::Unbinds | Collection::KeyTables | Collection::Macros => {
            Subsystem::Keys
        }
        Collection::MouseBinds => Subsystem::Pointer,
        Collection::Rules
        | Collection::Profiles
        | Collection::Layouts
        | Collection::Menus
        | Collection::Domains => Subsystem::Mux,
    }
}

/// Everything a consumer needs to apply one successful reload.
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    pub diff: ConfigDiff,
    /// Subsystems with at least one changed key or collection, sorted.
    pub affected: Vec<Subsystem>,
    /// Paths whose change cannot apply to what is running: the toast says which class.
    pub deferred: Vec<(&'static str, Reload)>,
    /// True when at least one `Restart`-class key changed.
    pub restart_required: bool,
    /// Warnings and tolerated errors (a dropped bind, a scalar that kept its old value). The
    /// error bar shows these too; the rest of the file applied.
    pub diagnostics: Vec<Diagnostic>,
}

impl Applied {
    /// Does `s` need to react?
    pub fn touches(&self, s: Subsystem) -> bool {
        self.affected.contains(&s)
    }

    /// Changed keys in `s`.
    pub fn keys_in(&self, s: Subsystem) -> impl Iterator<Item = &KeyChange> {
        self.diff
            .keys
            .iter()
            .filter(move |k| subsystem_of(k.path) == s)
    }
}

/// What one reload attempt did.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The new config is live.
    Applied(Box<Applied>),
    /// Same effective config; nothing to do. The error bar state is refreshed from
    /// [`Reloader::diagnostics`].
    Unchanged,
    /// A file failed to parse: the last good config is still live.
    Rejected,
}

/// The live config plus the last diagnostics, for the glass error bar.
#[derive(Debug, Clone)]
pub struct Reloader {
    current: Config,
    diagnostics: Vec<Diagnostic>,
    files: Vec<PathBuf>,
}

impl Reloader {
    /// First start. A broken file runs on defaults (plus whatever loaded) and the error bar.
    pub fn start(fs: &dyn Fs, input: &LoadInput) -> Reloader {
        let r = load(fs, input, None);
        Reloader {
            current: r.config,
            diagnostics: r.diagnostics,
            files: r.files,
        }
    }

    /// The config consumers read.
    pub fn config(&self) -> &Config {
        &self.current
    }

    /// Findings of the last attempt, errors and warnings, in file order.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Errors only: the error bar is shown while this is non-empty.
    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics.iter().filter(|d| d.is_error())
    }

    pub fn has_errors(&self) -> bool {
        self.errors().next().is_some()
    }

    /// One line for a collapsed bar: `file:line:col: error: message (+N more)`.
    pub fn error_summary(&self) -> Option<String> {
        let first = self.errors().next()?;
        let more = self.errors().count() - 1;
        let mut s = first.render();
        if more > 0 {
            s.push_str(&format!(" (+{more} more)"));
        }
        Some(s)
    }

    /// Every file read or looked for by the last load.
    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    /// The directories to watch: the parents of every file, deduplicated and symlinks
    /// resolved, so an editor's rename-replace is seen (T-10 §4).
    pub fn watch_dirs(&self, fs: &dyn Fs) -> Vec<PathBuf> {
        watch_dirs(fs, &self.files)
    }

    /// Parse, validate, diff and (when not rejected) swap in the new config.
    ///
    /// A file that does not parse keeps the last good config. Per-entry problems in a file
    /// that did parse are tolerated as T-10 §4 says: the bad bind is dropped and a bad
    /// scalar keeps its previous value, both reported in the diagnostics.
    pub fn reload(&mut self, fs: &dyn Fs, input: &LoadInput) -> Outcome {
        let r = load(fs, input, Some(&self.current));
        self.diagnostics = r.diagnostics;
        self.files = r.files;
        if r.rejected {
            return Outcome::Rejected;
        }
        let d = diff(&self.current, &r.config);
        self.current = r.config;
        if d.is_empty() {
            return Outcome::Unchanged;
        }
        Outcome::Applied(Box::new(plan(d, self.diagnostics.clone())))
    }

    /// Should this event cause a reload? False when `path` holds exactly what we last wrote
    /// (own-write suppression by content hash), or cannot be read.
    pub fn is_foreign_change(fs: &dyn Fs, path: &Path, own: &SelfWrites) -> bool {
        match fs.read_to_string(path) {
            Ok(text) => !own.is_self_write(path, &text),
            // A deleted or unreadable file is a change the reload must see.
            Err(_) => true,
        }
    }
}

/// Turn a diff into the consumer's plan.
pub fn plan(d: ConfigDiff, diagnostics: Vec<Diagnostic>) -> Applied {
    let mut set: BTreeSet<Subsystem> = BTreeSet::new();
    for k in &d.keys {
        set.insert(subsystem_of(k.path));
    }
    for c in &d.collections {
        set.insert(collection_subsystem(*c));
    }
    let deferred: Vec<(&'static str, Reload)> = d.deferred().map(|k| (k.path, k.reload)).collect();
    let restart_required = deferred.iter().any(|(_, r)| *r == Reload::Restart);
    Applied {
        diff: d,
        affected: set.into_iter().collect(),
        deferred,
        restart_required,
        diagnostics,
    }
}

/// Parent directories of `files`, canonicalised, sorted, deduplicated.
pub fn watch_dirs(fs: &dyn Fs, files: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();
    for f in files {
        // Canonicalise the file when it exists (a symlink swap changes the target's
        // directory); otherwise watch the lexical parent so its creation is seen.
        let target = if fs.exists(f) {
            fs.canonical(f)
        } else {
            crate::fs::normalize(f)
        };
        if let Some(p) = target.parent() {
            dirs.insert(p.to_path_buf());
        }
        // The lexical parent too: the symlink itself may be replaced.
        if let Some(p) = crate::fs::normalize(f).parent() {
            dirs.insert(p.to_path_buf());
        }
    }
    dirs.into_iter().collect()
}
