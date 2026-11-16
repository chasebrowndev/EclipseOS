// SPDX-License-Identifier: AGPL-3.0-only
//! Key and mouse binds (T-11 §3-§4): the default table, the user's `binds`, `unbind`,
//! `key-table`, `mouse-bind` and `macro` collections compiled into one [`Keymap`], and the
//! per-window [`KeyState`] (key-table stack, pending multi-key sequence).
//!
//! Lookup runs only on real seat key events; `input.send-keys` encodes straight to the PTY
//! and never comes here (T-11 §3). Matching follows `apoth-action`: modifiers exact, then
//! the physical key, the produced symbol, the unshifted symbol. The names are built on the
//! stack: a key that matches nothing costs no allocation on its way to the PTY.
//!
//! Nothing here logs a key: only bind indices, counts and action ids.

use std::time::{Duration, Instant};

use apoth_action::key::{Key as ChordKey, KeyChord, KeyEvent as BindEvent, Mode, MouseOn};
use apoth_action::{Action, Bind, BindOptions, Call, MacroDef, MacroSet, Mods as BMods, MouseBind};
use apoth_config::Config;
use apoth_config::model::{ActionCall, Arg};
use apoth_input::{Key, Kp, Mods, Named};

/// The default key table (T-11 §3, T-05 §3, T-13): chord, action, options. Abyss keeps the
/// Super chords, so every default here is Ctrl or Shift based. Scrolling binds are
/// performable: on the alternate screen (no scrollback) the key reaches the program.
/// A default bind: chord, action, and its options as `(key, value)` properties.
pub(crate) type DefaultBind = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
);

pub(crate) const DEFAULTS: &[DefaultBind] = &[
    // Clipboard and selection (T-05 §3).
    ("ctrl+shift+c", "selection.copy", &[("repeat", "false")]),
    ("ctrl+Insert", "selection.copy", &[("repeat", "false")]),
    ("ctrl+shift+v", "input.paste", &[("repeat", "false")]),
    (
        "shift+Insert",
        "input.paste-selection",
        &[("repeat", "false")],
    ),
    (
        "ctrl+shift+a",
        "selection.select-all",
        &[("repeat", "false")],
    ),
    // Scrollback.
    ("shift+Prior", "scroll.page-up", &[("performable", "true")]),
    ("shift+Next", "scroll.page-down", &[("performable", "true")]),
    (
        "ctrl+shift+Up",
        "scroll.line-up",
        &[("performable", "true")],
    ),
    (
        "ctrl+shift+Down",
        "scroll.line-down",
        &[("performable", "true")],
    ),
    ("shift+Home", "scroll.top", &[("performable", "true")]),
    ("shift+End", "scroll.bottom", &[("performable", "true")]),
    // Tabs (T-13 §4).
    ("ctrl+shift+t", "tab.new", &[("repeat", "false")]),
    ("ctrl+Tab", "tab.next", &[]),
    ("ctrl+shift+Tab", "tab.prev", &[]),
    ("ctrl+Next", "tab.next", &[]),
    ("ctrl+Prior", "tab.prev", &[]),
    ("ctrl+shift+Next", "tab.move 1", &[]),
    ("ctrl+shift+Prior", "tab.move -1", &[]),
    ("alt+1", "tab.select 1", &[("repeat", "false")]),
    ("alt+2", "tab.select 2", &[("repeat", "false")]),
    ("alt+3", "tab.select 3", &[("repeat", "false")]),
    ("alt+4", "tab.select 4", &[("repeat", "false")]),
    ("alt+5", "tab.select 5", &[("repeat", "false")]),
    ("alt+6", "tab.select 6", &[("repeat", "false")]),
    ("alt+7", "tab.select 7", &[("repeat", "false")]),
    ("alt+8", "tab.select 8", &[("repeat", "false")]),
    ("alt+9", "tab.select 9", &[("repeat", "false")]),
    // Panes (T-13 §3).
    ("ctrl+shift+d", "pane.split right", &[("repeat", "false")]),
    ("ctrl+shift+e", "pane.split down", &[("repeat", "false")]),
    ("ctrl+shift+w", "pane.close", &[("repeat", "false")]),
    ("ctrl+alt+Left", "pane.focus left", &[]),
    ("ctrl+alt+Right", "pane.focus right", &[]),
    ("ctrl+alt+Up", "pane.focus up", &[]),
    ("ctrl+alt+Down", "pane.focus down", &[]),
    ("ctrl+alt+shift+Left", "pane.resize left", &[]),
    ("ctrl+alt+shift+Right", "pane.resize right", &[]),
    ("ctrl+alt+shift+Up", "pane.resize up", &[]),
    ("ctrl+alt+shift+Down", "pane.resize down", &[]),
    ("ctrl+shift+z", "pane.toggle-zoom", &[("repeat", "false")]),
    // Command palette (T-14 §9).
    ("ctrl+shift+p", "palette.toggle", &[("repeat", "false")]),
    // Navigation (T-14 §6-§8, §4).
    ("ctrl+shift+f", "search.start", &[("repeat", "false")]),
    (
        "ctrl+shift+space",
        "copy-mode.toggle",
        &[("repeat", "false")],
    ),
    ("ctrl+shift+o", "hints.start", &[("repeat", "false")]),
    // "Open with" (T-14 §11): the menu for the selection, else hints for what can open.
    ("ctrl+shift+m", "open.menu", &[("repeat", "false")]),
    (
        "ctrl+shift+k",
        "block.jump prev",
        &[("performable", "true")],
    ),
    (
        "ctrl+shift+j",
        "block.jump next",
        &[("performable", "true")],
    ),
];

/// A built-in key table row: chord and the actions it runs.
pub(crate) type TableBind = (&'static str, &'static [&'static str]);

/// The default `copy-mode` key table, vi style (T-14 §8). An ordinary key table: a user
/// `key-table "copy-mode"` adds to, rebinds or unbinds these keys.
pub(crate) const COPY_MODE_VI: &[TableBind] = &[
    ("h", &["copy-mode.move left"]),
    ("Left", &["copy-mode.move left"]),
    ("j", &["copy-mode.move down"]),
    ("Down", &["copy-mode.move down"]),
    ("k", &["copy-mode.move up"]),
    ("Up", &["copy-mode.move up"]),
    ("l", &["copy-mode.move right"]),
    ("Right", &["copy-mode.move right"]),
    ("w", &["copy-mode.move word-forward"]),
    ("b", &["copy-mode.move word-back"]),
    ("e", &["copy-mode.move word-end"]),
    ("shift+w", &["copy-mode.move big-word-forward"]),
    ("shift+b", &["copy-mode.move big-word-back"]),
    ("shift+e", &["copy-mode.move big-word-end"]),
    ("0", &["copy-mode.move line-start"]),
    ("Home", &["copy-mode.move line-start"]),
    ("shift+4", &["copy-mode.move line-end"]),
    ("End", &["copy-mode.move line-end"]),
    ("shift+6", &["copy-mode.move first-non-blank"]),
    ("g>g", &["copy-mode.move top"]),
    ("shift+g", &["copy-mode.move bottom"]),
    ("shift+h", &["copy-mode.move screen-top"]),
    ("shift+m", &["copy-mode.move screen-middle"]),
    ("shift+l", &["copy-mode.move screen-bottom"]),
    ("ctrl+b", &["copy-mode.move page-up"]),
    ("Prior", &["copy-mode.move page-up"]),
    ("ctrl+f", &["copy-mode.move page-down"]),
    ("Next", &["copy-mode.move page-down"]),
    ("ctrl+u", &["copy-mode.move half-page-up"]),
    ("ctrl+d", &["copy-mode.move half-page-down"]),
    ("shift+bracketleft", &["copy-mode.move paragraph-back"]),
    ("shift+bracketright", &["copy-mode.move paragraph-forward"]),
    ("bracketleft>bracketleft", &["copy-mode.move prompt-prev"]),
    ("bracketright>bracketright", &["copy-mode.move prompt-next"]),
    ("f", &["cursor.jump-char"]),
    ("shift+f", &["cursor.jump-char backward=true"]),
    ("t", &["cursor.jump-char till=true"]),
    ("shift+t", &["cursor.jump-char backward=true till=true"]),
    ("semicolon", &["cursor.jump-repeat"]),
    ("comma", &["cursor.jump-repeat reverse=true"]),
    ("slash", &["search.start"]),
    ("shift+slash", &["search.start"]),
    ("n", &["copy-mode.move search-next"]),
    ("shift+n", &["copy-mode.move search-prev"]),
    ("v", &["copy-mode.toggle-selection"]),
    ("shift+v", &["copy-mode.toggle-selection unit=line"]),
    ("ctrl+v", &["copy-mode.toggle-selection unit=block"]),
    ("y", &["selection.copy", "copy-mode.exit"]),
    ("Return", &["selection.copy", "copy-mode.exit"]),
    ("q", &["copy-mode.exit"]),
    ("Escape", &["copy-mode.exit"]),
];

/// The emacs-style copy-mode table, `copy-mode-emacs`: bind `copy-mode.enter` after a
/// `key-table.push copy-mode-emacs`, or copy its rows into `copy-mode`.
pub(crate) const COPY_MODE_EMACS: &[TableBind] = &[
    ("ctrl+b", &["copy-mode.move left"]),
    ("Left", &["copy-mode.move left"]),
    ("ctrl+f", &["copy-mode.move right"]),
    ("Right", &["copy-mode.move right"]),
    ("ctrl+p", &["copy-mode.move up"]),
    ("Up", &["copy-mode.move up"]),
    ("ctrl+n", &["copy-mode.move down"]),
    ("Down", &["copy-mode.move down"]),
    ("alt+f", &["copy-mode.move word-forward"]),
    ("alt+b", &["copy-mode.move word-back"]),
    ("ctrl+a", &["copy-mode.move line-start"]),
    ("ctrl+e", &["copy-mode.move line-end"]),
    ("alt+v", &["copy-mode.move page-up"]),
    ("ctrl+v", &["copy-mode.move page-down"]),
    ("alt+shift+comma", &["copy-mode.move top"]),
    ("alt+shift+period", &["copy-mode.move bottom"]),
    ("ctrl+space", &["copy-mode.toggle-selection"]),
    ("alt+w", &["selection.copy", "copy-mode.exit"]),
    ("ctrl+s", &["search.start"]),
    ("ctrl+r", &["search.start"]),
    ("ctrl+g", &["copy-mode.exit"]),
    ("Escape", &["copy-mode.exit"]),
];

/// Built-in key tables (name, rows).
pub(crate) const BUILTIN_TABLES: &[(&str, &[TableBind])] = &[
    ("copy-mode", COPY_MODE_VI),
    ("copy-mode-emacs", COPY_MODE_EMACS),
];

fn builtin_table(rows: &[TableBind]) -> Vec<KeyBind> {
    rows.iter()
        .filter_map(|(chord, actions)| {
            let b = Bind::parse(chord, &[]).ok()?;
            let actions = actions
                .iter()
                .map(|a| Action::parse(a).ok())
                .collect::<Option<Vec<_>>>()?;
            Some(KeyBind {
                seq: canon(b.sequence),
                actions,
                opts: b.opts,
            })
        })
        .collect()
}

/// One compiled key bind.
#[derive(Clone, Debug)]
pub(crate) struct KeyBind {
    pub(crate) seq: Vec<KeyChord>,
    pub(crate) actions: Vec<Action>,
    pub(crate) opts: BindOptions,
}

/// One compiled mouse bind.
#[derive(Clone, Debug)]
pub(crate) struct MouseEntry {
    pub(crate) trigger: MouseBind,
    pub(crate) actions: Vec<Action>,
}

/// Every bind the window uses, rebuilt when the configuration's bind collections change.
#[derive(Clone, Debug, Default)]
pub(crate) struct Keymap {
    pub(crate) root: Vec<KeyBind>,
    pub(crate) tables: Vec<(String, Vec<KeyBind>)>,
    pub(crate) mouse: Vec<MouseEntry>,
    pub(crate) macros: MacroSet,
    pub(crate) seq_timeout: Duration,
    /// `keyboard.layout-independent-binds`: a non-Latin key also matches its base-layout name.
    pub(crate) layout_independent: bool,
    /// Binds dropped at compile (bad chord or action), for the log line. Counts only.
    pub(crate) dropped: usize,
}

/// `ActionCall` (config, untyped) to `Call` (the action grammar's untyped form).
pub(crate) fn call_of(a: &ActionCall) -> Call {
    let s = |v: &Arg| match v {
        Arg::Str(s) => Some(s.clone()),
        Arg::Int(i) => Some(i.to_string()),
        Arg::Float(f) => Some(f.to_string()),
        Arg::Bool(b) => Some(b.to_string()),
        Arg::Null => None,
    };
    let mut c = Call::new(a.id.clone());
    c.positional = a.args.iter().filter_map(s).collect();
    c.named = a
        .props
        .iter()
        .filter_map(|(k, v)| s(v).map(|v| (k.clone(), v)))
        .collect();
    c.argv = a
        .children
        .iter()
        .find(|ch| ch.id == "argv")
        .map(|ch| ch.args.iter().filter_map(s).collect());
    c
}

pub(crate) fn actions_of(calls: &[ActionCall]) -> Option<Vec<Action>> {
    calls
        .iter()
        .map(|c| Action::from_call(&call_of(c)).ok())
        .collect()
}

/// Canonical spelling of keysym aliases users write (`Page_Up` is `Prior`).
fn canon_sym(s: &str) -> &str {
    match s {
        "Page_Up" => "Prior",
        "Page_Down" => "Next",
        "KP_Page_Up" => "KP_Prior",
        "KP_Page_Down" => "KP_Next",
        other => other,
    }
}

fn canon(mut seq: Vec<KeyChord>) -> Vec<KeyChord> {
    for c in &mut seq {
        if let ChordKey::Symbol(s) = &c.key {
            let n = canon_sym(s);
            if n != s {
                c.key = ChordKey::Symbol(n.to_owned());
            }
        }
    }
    seq
}

fn bind_of(
    chords: &[apoth_config::chord::Chord],
    calls: &[ActionCall],
    props: &[(&str, &str)],
) -> Option<KeyBind> {
    let text = apoth_config::chord::format_chord_seq(chords);
    let b = Bind::parse(&text, props).ok()?;
    Some(KeyBind {
        seq: canon(b.sequence),
        actions: actions_of(calls)?,
        opts: b.opts,
    })
}

fn decl_bind(d: &apoth_config::model::BindDecl) -> Option<KeyBind> {
    let mut props: Vec<(&str, &str)> = Vec::new();
    if !d.repeat {
        props.push(("repeat", "false"));
    }
    if d.release {
        props.push(("release", "true"));
    }
    if d.global {
        props.push(("global", "true"));
    }
    if d.performable {
        props.push(("performable", "true"));
    }
    if let Some(m) = &d.mode {
        props.push(("mode", m.as_str()));
    }
    bind_of(&d.chords, &d.actions, &props)
}

fn seq_of(chords: &[apoth_config::chord::Chord]) -> Option<Vec<KeyChord>> {
    apoth_action::key::parse_sequence(&apoth_config::chord::format_chord_seq(chords))
        .ok()
        .map(canon)
}

/// Add `b`, replacing a bind with the same sequence and mode condition (a later bind wins).
fn insert(list: &mut Vec<KeyBind>, b: KeyBind) {
    list.retain(|o| !(o.seq == b.seq && o.opts.mode == b.opts.mode));
    list.push(b);
}

impl Keymap {
    /// The default table alone (an empty configuration).
    pub(crate) fn defaults() -> Vec<KeyBind> {
        DEFAULTS
            .iter()
            .filter_map(|(chord, action, props)| {
                let b = Bind::parse(chord, props).ok()?;
                Some(KeyBind {
                    seq: canon(b.sequence),
                    actions: vec![Action::parse(action).ok()?],
                    opts: b.opts,
                })
            })
            .collect()
    }

    pub(crate) fn from_config(c: &Config) -> Keymap {
        let mut dropped = 0usize;
        let mut root = if c.bool("binds.clear-defaults") {
            Vec::new()
        } else {
            Self::defaults()
        };
        for u in &c.unbinds {
            match seq_of(&u.value) {
                Some(seq) => root.retain(|b| b.seq != seq),
                None => dropped += 1,
            }
        }
        for d in &c.binds {
            match decl_bind(&d.value) {
                Some(b) => insert(&mut root, b),
                None => dropped += 1,
            }
        }
        let mut tables: Vec<(String, Vec<KeyBind>)> = BUILTIN_TABLES
            .iter()
            .map(|(n, rows)| ((*n).to_string(), builtin_table(rows)))
            .collect();
        for t in &c.key_tables {
            // A user table of a built-in name edits it (adds, rebinds, unbinds).
            let mut list = match tables.iter().position(|(n, _)| *n == t.value.name) {
                Some(i) => tables.remove(i).1,
                None => Vec::new(),
            };
            for d in &t.value.binds {
                match decl_bind(d) {
                    Some(b) => insert(&mut list, b),
                    None => dropped += 1,
                }
            }
            for u in &t.value.unbinds {
                if let Some(seq) = seq_of(u) {
                    list.retain(|b| b.seq != seq);
                }
            }
            tables.retain(|(n, _): &(String, Vec<KeyBind>)| *n != t.value.name);
            tables.push((t.value.name.clone(), list));
        }
        let mouse = c
            .mouse_binds
            .iter()
            .filter_map(|m| {
                let trigger = MouseBind::parse(&m.value.trigger, Some(&m.value.on)).ok();
                let actions = actions_of(&m.value.actions);
                match (trigger, actions) {
                    (Some(trigger), Some(actions)) => Some(MouseEntry { trigger, actions }),
                    _ => {
                        dropped += 1;
                        None
                    }
                }
            })
            .collect();
        let defs: Vec<MacroDef> = c
            .macros
            .iter()
            .filter_map(|m| {
                Some(MacroDef {
                    name: m.value.name.clone(),
                    members: actions_of(&m.value.actions)?,
                })
            })
            .collect();
        let macros = MacroSet::build(defs).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "macros rejected");
            MacroSet::default()
        });
        Keymap {
            root,
            tables,
            mouse,
            macros,
            seq_timeout: Duration::from_millis(c.millis("keyboard.sequence-timeout").max(1)),
            layout_independent: c.bool("keyboard.layout-independent-binds"),
            dropped,
        }
    }

    fn table(&self, t: TableRef) -> &[KeyBind] {
        match t {
            TableRef::Root => &self.root,
            TableRef::Named(i) => self.tables.get(i).map_or(&[], |(_, l)| l.as_slice()),
        }
    }

    pub(crate) fn table_index(&self, name: &str) -> Option<usize> {
        self.tables.iter().position(|(n, _)| n == name)
    }

    /// The canonical chord text of the first root bind running exactly `a`, for the palette.
    pub(crate) fn binding_of(&self, a: &Action) -> Option<String> {
        self.root
            .iter()
            .find(|b| b.actions.len() == 1 && b.actions[0] == *a)
            .map(|b| apoth_action::key::format_sequence(&b.seq))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TableRef {
    Root,
    Named(usize),
}

#[derive(Clone, Copy, Debug)]
struct Active {
    table: usize,
    one_shot: bool,
    deadline: Option<Instant>,
}

#[derive(Clone, Copy, Debug)]
struct Pending {
    table: TableRef,
    /// The first `len` chords of the sequence were typed.
    len: usize,
    /// Index of a bind whose prefix matched (any; all candidates share the prefix).
    bind: usize,
    deadline: Instant,
}

/// What a key press resolved to.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Resolved {
    /// No bind: the key goes to the program.
    None,
    /// A sequence prefix: consumed, nothing runs yet.
    Prefix,
    /// A bind fired.
    Fire {
        actions: Vec<Action>,
        repeat: bool,
        release: bool,
        performable: bool,
    },
}

/// Key-table stack and pending sequence of one window.
#[derive(Debug, Default)]
pub(crate) struct KeyState {
    stack: Vec<Active>,
    pending: Option<Pending>,
}

impl KeyState {
    /// Forget tables and sequences (the keymap was rebuilt or the keyboard left).
    pub(crate) fn reset(&mut self) {
        self.stack.clear();
        self.pending = None;
    }

    /// `key-table.push`. Unknown names are the caller's to report.
    pub(crate) fn push(
        &mut self,
        table: usize,
        one_shot: bool,
        timeout: Option<Duration>,
        now: Instant,
    ) {
        self.pending = None;
        self.stack.push(Active {
            table,
            one_shot,
            deadline: timeout.map(|t| now + t),
        });
    }

    pub(crate) fn pop(&mut self) {
        self.pending = None;
        self.stack.pop();
    }

    /// Deactivate every instance of `table` wherever it sits in the stack (copy mode's exit).
    pub(crate) fn remove(&mut self, table: usize) {
        self.pending = None;
        self.stack.retain(|a| a.table != table);
    }

    pub(crate) fn clear(&mut self) {
        self.reset();
    }

    /// Names of the active tables, bottom first (for the test dump).
    pub(crate) fn active<'a>(&self, km: &'a Keymap) -> Vec<&'a str> {
        self.stack
            .iter()
            .filter_map(|a| km.tables.get(a.table).map(|(n, _)| n.as_str()))
            .collect()
    }

    fn expire(&mut self, now: Instant) {
        self.stack.retain(|a| a.deadline.is_none_or(|d| now < d));
        if self.pending.is_some_and(|p| now >= p.deadline) {
            self.pending = None;
        }
    }

    /// Resolve a press (not a repeat) against the keymap. `modes` answers `mode=` conditions.
    pub(crate) fn press(
        &mut self,
        km: &Keymap,
        ev: &BindEvent<'_>,
        now: Instant,
        modes: &dyn Fn(Mode) -> bool,
    ) -> Resolved {
        self.expire(now);
        let ok = |b: &KeyBind| b.opts.mode.as_ref().is_none_or(|m| m.eval(modes));
        if let Some(p) = self.pending.take() {
            let list = km.table(p.table);
            let prefix = list.get(p.bind).map(|b| &b.seq[..p.len]);
            let mut best: Option<(apoth_action::key::Strength, usize)> = None;
            for (i, b) in list.iter().enumerate() {
                if b.seq.len() > p.len
                    && Some(&b.seq[..p.len]) == prefix
                    && ok(b)
                    && let Some(s) = b.seq[p.len].match_strength(ev)
                {
                    let better = match best {
                        None => true,
                        Some((bs, bi)) => {
                            s > bs
                                || (s == bs
                                    && b.seq.len() == p.len + 1
                                    && list[bi].seq.len() > p.len + 1)
                        }
                    };
                    if better {
                        best = Some((s, i));
                    }
                }
            }
            if let Some((_, i)) = best {
                return self.hit(list, i, p.table, p.len + 1, now, km.seq_timeout);
            }
            // No continuation: the prefix stays consumed and this key is looked up afresh.
        }
        let one_shot_top = self.stack.last().is_some_and(|a| a.one_shot);
        // Top of the stack first, the root last; indexed so the hot path does not allocate.
        let depth = self.stack.len();
        let mut out = Resolved::None;
        for step in 0..=depth {
            let t = if step < depth {
                TableRef::Named(self.stack[depth - 1 - step].table)
            } else {
                TableRef::Root
            };
            let list = km.table(t);
            let mut best: Option<(apoth_action::key::Strength, usize)> = None;
            for (i, b) in list.iter().enumerate() {
                if !ok(b) {
                    continue;
                }
                if let Some(s) = b.seq[0].match_strength(ev) {
                    // A complete one-chord bind beats a sequence starting with the same chord.
                    let better = match best {
                        None => true,
                        Some((bs, bi)) => {
                            s > bs || (s == bs && b.seq.len() == 1 && list[bi].seq.len() > 1)
                        }
                    };
                    if better {
                        best = Some((s, i));
                    }
                }
            }
            if let Some((_, i)) = best {
                out = self.hit(list, i, t, 1, now, km.seq_timeout);
                break;
            }
        }
        if one_shot_top && self.pending.is_none() {
            // A one-shot table lasts exactly one key, matched or not.
            self.stack.pop();
        }
        out
    }

    fn hit(
        &mut self,
        list: &[KeyBind],
        i: usize,
        table: TableRef,
        len: usize,
        now: Instant,
        timeout: Duration,
    ) -> Resolved {
        let b = &list[i];
        if b.seq.len() > len {
            self.pending = Some(Pending {
                table,
                len,
                bind: i,
                deadline: now + timeout,
            });
            return Resolved::Prefix;
        }
        Resolved::Fire {
            actions: b.actions.clone(),
            repeat: b.opts.repeat,
            release: b.opts.release,
            performable: b.opts.performable,
        }
    }
}

/// The mouse binds matching a phase, in file order (the first wins).
pub(crate) fn mouse_lookup(
    km: &Keymap,
    mods: BMods,
    button: apoth_action::key::MouseButton,
    on: MouseOn,
) -> Option<&[Action]> {
    km.mouse
        .iter()
        .find(|m| m.trigger.mods == mods && m.trigger.button == button && m.trigger.on == on)
        .map(|m| m.actions.as_slice())
}

// ---------------------------------------------------------------------------------------------
// Key names

/// `apoth-input` modifiers to the bind grammar's; `None` when Hyper or Meta is held (no chord
/// can name them, so nothing matches).
pub(crate) fn bind_mods(m: Mods) -> Option<BMods> {
    let m = m.without_locks();
    if m.contains(Mods::HYPER) || m.contains(Mods::META) {
        return None;
    }
    let mut o = BMods::NONE;
    for (a, b) in [
        (Mods::SHIFT, BMods::SHIFT),
        (Mods::CTRL, BMods::CTRL),
        (Mods::ALT, BMods::ALT),
        (Mods::SUPER, BMods::SUPER),
    ] {
        if m.contains(a) {
            o = o.union(b);
        }
    }
    Some(o)
}

/// Stack storage for the two character-derived names of a key event.
#[derive(Default)]
pub(crate) struct NameBuf {
    produced: [u8; 8],
    unshifted: [u8; 8],
}

/// The xkb keysym name of a character, written into `buf` when it is not a static name.
fn char_name(c: char, buf: &mut [u8; 8]) -> &str {
    let named = match c {
        ' ' => "space",
        '!' => "exclam",
        '"' => "quotedbl",
        '#' => "numbersign",
        '$' => "dollar",
        '%' => "percent",
        '&' => "ampersand",
        '\'' => "apostrophe",
        '(' => "parenleft",
        ')' => "parenright",
        '*' => "asterisk",
        '+' => "plus",
        ',' => "comma",
        '-' => "minus",
        '.' => "period",
        '/' => "slash",
        ':' => "colon",
        ';' => "semicolon",
        '<' => "less",
        '=' => "equal",
        '>' => "greater",
        '?' => "question",
        '@' => "at",
        '[' => "bracketleft",
        '\\' => "backslash",
        ']' => "bracketright",
        '^' => "asciicircum",
        '_' => "underscore",
        '`' => "grave",
        '{' => "braceleft",
        '|' => "bar",
        '}' => "braceright",
        '~' => "asciitilde",
        _ => "",
    };
    if !named.is_empty() {
        return named;
    }
    if c.is_ascii_alphanumeric() {
        buf[0] = c as u8;
        return std::str::from_utf8(&buf[..1]).unwrap_or("");
    }
    if c.is_control() {
        return "";
    }
    // Any other character: the xkb Unicode keysym name `U<hex>` (at least four digits).
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let v = c as u32;
    let digits = (((32 - v.leading_zeros()) as usize).div_ceil(4)).max(4);
    buf[0] = b'U';
    for i in 0..digits {
        buf[1 + i] = HEX[((v >> (4 * (digits - 1 - i))) & 0xf) as usize];
    }
    std::str::from_utf8(&buf[..1 + digits]).unwrap_or("")
}

const F_NAMES: [&str; 35] = [
    "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12", "F13", "F14", "F15",
    "F16", "F17", "F18", "F19", "F20", "F21", "F22", "F23", "F24", "F25", "F26", "F27", "F28",
    "F29", "F30", "F31", "F32", "F33", "F34", "F35",
];

const KP_DIGITS: [&str; 10] = [
    "KP_0", "KP_1", "KP_2", "KP_3", "KP_4", "KP_5", "KP_6", "KP_7", "KP_8", "KP_9",
];

fn named_name(n: Named, shift: bool) -> &'static str {
    match n {
        Named::Escape => "Escape",
        Named::Enter => "Return",
        Named::Tab if shift => "ISO_Left_Tab",
        Named::Tab => "Tab",
        Named::Backspace => "BackSpace",
        Named::Insert => "Insert",
        Named::Delete => "Delete",
        Named::Left => "Left",
        Named::Right => "Right",
        Named::Up => "Up",
        Named::Down => "Down",
        Named::PageUp => "Prior",
        Named::PageDown => "Next",
        Named::Home => "Home",
        Named::End => "End",
        Named::F(i) => F_NAMES
            .get(usize::from(i).wrapping_sub(1))
            .copied()
            .unwrap_or(""),
        Named::Menu => "Menu",
        Named::CapsLock => "Caps_Lock",
        Named::ScrollLock => "Scroll_Lock",
        Named::NumLock => "Num_Lock",
        Named::PrintScreen => "Print",
        Named::Pause => "Pause",
        Named::Kp(k) => match k {
            Kp::Digit(d) => KP_DIGITS.get(usize::from(d)).copied().unwrap_or(""),
            Kp::Decimal => "KP_Decimal",
            Kp::Divide => "KP_Divide",
            Kp::Multiply => "KP_Multiply",
            Kp::Subtract => "KP_Subtract",
            Kp::Add => "KP_Add",
            Kp::Enter => "KP_Enter",
            Kp::Equal => "KP_Equal",
            Kp::Separator => "KP_Separator",
            Kp::Left => "KP_Left",
            Kp::Right => "KP_Right",
            Kp::Up => "KP_Up",
            Kp::Down => "KP_Down",
            Kp::PageUp => "KP_Prior",
            Kp::PageDown => "KP_Next",
            Kp::Home => "KP_Home",
            Kp::End => "KP_End",
            Kp::Insert => "KP_Insert",
            Kp::Delete => "KP_Delete",
            Kp::Begin => "KP_Begin",
        },
        Named::Pua(_) => "",
    }
}

/// The W3C `KeyboardEvent.code` name of an evdev key code (the `physical:` names).
pub(crate) fn physical_name(code: u32) -> &'static str {
    const LETTERS_Q: [&str; 10] = [
        "KeyQ", "KeyW", "KeyE", "KeyR", "KeyT", "KeyY", "KeyU", "KeyI", "KeyO", "KeyP",
    ];
    const LETTERS_A: [&str; 9] = [
        "KeyA", "KeyS", "KeyD", "KeyF", "KeyG", "KeyH", "KeyJ", "KeyK", "KeyL",
    ];
    const LETTERS_Z: [&str; 7] = ["KeyZ", "KeyX", "KeyC", "KeyV", "KeyB", "KeyN", "KeyM"];
    const DIGITS: [&str; 10] = [
        "Digit1", "Digit2", "Digit3", "Digit4", "Digit5", "Digit6", "Digit7", "Digit8", "Digit9",
        "Digit0",
    ];
    match code {
        1 => "Escape",
        2..=11 => DIGITS[(code - 2) as usize],
        12 => "Minus",
        13 => "Equal",
        14 => "Backspace",
        15 => "Tab",
        16..=25 => LETTERS_Q[(code - 16) as usize],
        26 => "BracketLeft",
        27 => "BracketRight",
        28 => "Enter",
        29 => "ControlLeft",
        30..=38 => LETTERS_A[(code - 30) as usize],
        39 => "Semicolon",
        40 => "Quote",
        41 => "Backquote",
        42 => "ShiftLeft",
        43 => "Backslash",
        44..=50 => LETTERS_Z[(code - 44) as usize],
        51 => "Comma",
        52 => "Period",
        53 => "Slash",
        54 => "ShiftRight",
        55 => "NumpadMultiply",
        56 => "AltLeft",
        57 => "Space",
        58 => "CapsLock",
        59..=68 => F_NAMES[(code - 59) as usize],
        69 => "NumLock",
        70 => "ScrollLock",
        71 => "Numpad7",
        72 => "Numpad8",
        73 => "Numpad9",
        74 => "NumpadSubtract",
        75 => "Numpad4",
        76 => "Numpad5",
        77 => "Numpad6",
        78 => "NumpadAdd",
        79 => "Numpad1",
        80 => "Numpad2",
        81 => "Numpad3",
        82 => "Numpad0",
        83 => "NumpadDecimal",
        86 => "IntlBackslash",
        87 => "F11",
        88 => "F12",
        96 => "NumpadEnter",
        97 => "ControlRight",
        98 => "NumpadDivide",
        99 => "PrintScreen",
        100 => "AltRight",
        102 => "Home",
        103 => "ArrowUp",
        104 => "PageUp",
        105 => "ArrowLeft",
        106 => "ArrowRight",
        107 => "End",
        108 => "ArrowDown",
        109 => "PageDown",
        110 => "Insert",
        111 => "Delete",
        117 => "NumpadEqual",
        119 => "Pause",
        125 => "MetaLeft",
        126 => "MetaRight",
        127 => "ContextMenu",
        _ => "",
    }
}

/// The bind event of a seat key: produced symbol (the shifted character while Shift is held),
/// unshifted symbol (the base-layout key for a non-Latin character when
/// `keyboard.layout-independent-binds` is on), and the physical name. No allocation.
pub(crate) fn bind_event<'a>(
    look: &apoth_input::backend::KeyLookup,
    code: u32,
    mods: BMods,
    layout_independent: bool,
    buf: &'a mut NameBuf,
) -> BindEvent<'a> {
    let shift = mods.contains(BMods::SHIFT);
    let NameBuf {
        produced,
        unshifted,
    } = buf;
    let (p, u) = match look.key {
        Key::Named(n) => (named_name(n, shift), named_name(n, false)),
        Key::Char(c) => {
            let pc = if shift { look.shifted.unwrap_or(c) } else { c };
            let base = (layout_independent && !c.is_ascii())
                .then_some(look.base)
                .flatten()
                .unwrap_or(c);
            (char_name(pc, produced), char_name(base, unshifted))
        }
    };
    BindEvent {
        mods,
        produced: p,
        unshifted: u,
        physical: physical_name(code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apoth_input::backend::KeyLookup;

    fn look(key: Key, shifted: Option<char>) -> KeyLookup {
        KeyLookup {
            key,
            shifted,
            base: None,
            text: apoth_input::InlineText::default(),
            mods: Mods::NONE,
            ctrl_fallback: None,
            repeats: true,
        }
    }

    fn ev<'a>(
        buf: &'a mut NameBuf,
        key: Key,
        shifted: Option<char>,
        code: u32,
        m: BMods,
    ) -> BindEvent<'a> {
        let l = look(key, shifted);
        // The lookup is a temporary: copy what the names borrow into the caller's buffer.
        let e = bind_event(&l, code, m, true, buf);
        BindEvent { ..e }
    }

    fn cs() -> BMods {
        BMods::CTRL.union(BMods::SHIFT)
    }

    fn keymap(binds: &[DefaultBind]) -> Keymap {
        let root = binds
            .iter()
            .map(|(c, a, p)| {
                let b = Bind::parse(c, p).unwrap();
                KeyBind {
                    seq: canon(b.sequence),
                    actions: Action::parse_script(a).unwrap(),
                    opts: b.opts,
                }
            })
            .collect();
        Keymap {
            root,
            seq_timeout: Duration::from_millis(1000),
            layout_independent: true,
            ..Keymap::default()
        }
    }

    fn none(_: Mode) -> bool {
        false
    }

    fn fired(r: &Resolved) -> Option<&str> {
        match r {
            Resolved::Fire { actions, .. } => actions.first().map(|a| a.id()),
            _ => None,
        }
    }

    #[test]
    fn every_default_compiles() {
        assert_eq!(Keymap::defaults().len(), DEFAULTS.len());
    }

    #[test]
    fn defaults_cover_tabs_splits_and_the_palette() {
        let km = Keymap {
            root: Keymap::defaults(),
            ..Keymap::default()
        };
        for id in [
            "tab.new",
            "pane.split",
            "pane.close",
            "palette.toggle",
            "tab.next",
        ] {
            assert!(
                km.root.iter().any(|b| b.actions[0].id() == id),
                "{id} has no default bind"
            );
        }
        let t = Action::parse("tab.new").unwrap();
        assert_eq!(km.binding_of(&t).as_deref(), Some("ctrl+shift+t"));
    }

    #[test]
    fn builtin_tables_compile_and_resolve_shifted_symbols() {
        for (name, rows) in BUILTIN_TABLES {
            assert_eq!(builtin_table(rows).len(), rows.len(), "table {name}");
        }
        let km = Keymap::from_config(&apoth_config::Config::defaults());
        let vi = km.table_index("copy-mode").expect("copy-mode table");
        let mut st = KeyState::default();
        let now = Instant::now();
        st.push(vi, false, None, now);
        let mut b = NameBuf::default();
        let e = ev(&mut b, Key::Char('4'), Some('$'), 5, BMods::SHIFT);
        assert_eq!(
            fired(&st.press(&km, &e, now, &none)),
            Some("copy-mode.move")
        );
        let mut b = NameBuf::default();
        let e = ev(&mut b, Key::Char('g'), Some('G'), 34, BMods::NONE);
        assert_eq!(st.press(&km, &e, now, &none), Resolved::Prefix);
        let mut b = NameBuf::default();
        let e = ev(&mut b, Key::Char('g'), Some('G'), 34, BMods::NONE);
        assert_eq!(
            fired(&st.press(&km, &e, now, &none)),
            Some("copy-mode.move")
        );
        let mut b = NameBuf::default();
        let e = ev(&mut b, Key::Char('y'), Some('Y'), 21, BMods::NONE);
        match st.press(&km, &e, now, &none) {
            Resolved::Fire { actions, .. } => assert_eq!(actions.len(), 2),
            r => panic!("y: {r:?}"),
        }
        st.remove(vi);
        let mut b = NameBuf::default();
        let e = ev(&mut b, Key::Char('y'), Some('Y'), 21, BMods::NONE);
        assert_eq!(st.press(&km, &e, now, &none), Resolved::None);
    }

    #[test]
    fn names_are_xkb_and_w3c() {
        let mut b = NameBuf::default();
        let e = ev(&mut b, Key::Char('t'), Some('T'), 20, cs());
        assert_eq!((e.produced, e.unshifted, e.physical), ("T", "t", "KeyT"));
        let mut b = NameBuf::default();
        let e = ev(&mut b, Key::Char('='), Some('+'), 13, BMods::SHIFT);
        assert_eq!(
            (e.produced, e.unshifted, e.physical),
            ("plus", "equal", "Equal")
        );
        let mut b = NameBuf::default();
        let e = ev(&mut b, Key::Named(Named::Tab), None, 15, cs());
        assert_eq!((e.produced, e.unshifted), ("ISO_Left_Tab", "Tab"));
        let mut b = NameBuf::default();
        let e = ev(&mut b, Key::Named(Named::PageUp), None, 104, BMods::SHIFT);
        assert_eq!((e.produced, e.physical), ("Prior", "PageUp"));
        let mut b = NameBuf::default();
        let e = ev(&mut b, Key::Char('ä'), None, 40, BMods::NONE);
        assert_eq!(e.produced, "U00E4");
        let mut b = NameBuf::default();
        let e = ev(&mut b, Key::Named(Named::F(12)), None, 88, BMods::NONE);
        assert_eq!((e.produced, e.physical), ("F12", "F12"));
    }

    #[test]
    fn non_latin_keys_match_their_base_layout_name() {
        let mut l = look(Key::Char('ф'), Some('Ф'));
        l.base = Some('a');
        let mut b = NameBuf::default();
        let e = bind_event(&l, 30, cs(), true, &mut b);
        assert_eq!(e.unshifted, "a");
        let mut b = NameBuf::default();
        let e = bind_event(&l, 30, cs(), false, &mut b);
        assert_eq!(e.unshifted, "U0444");
    }

    #[test]
    fn hyper_or_meta_never_match() {
        assert_eq!(bind_mods(Mods::CTRL | Mods::HYPER), None);
        assert_eq!(bind_mods(Mods::CTRL | Mods::CAPS), Some(BMods::CTRL));
        assert_eq!(
            bind_mods(Mods::CTRL | Mods::ALT),
            Some(BMods::CTRL.union(BMods::ALT))
        );
    }

    #[test]
    fn modifiers_must_match_exactly() {
        let km = keymap(&[("ctrl+shift+c", "selection.copy", &[])]);
        let mut st = KeyState::default();
        let now = Instant::now();
        let mut b = NameBuf::default();
        let e = ev(&mut b, Key::Char('c'), Some('C'), 46, BMods::CTRL);
        assert_eq!(st.press(&km, &e, now, &none), Resolved::None);
        let mut b = NameBuf::default();
        let e = ev(&mut b, Key::Char('c'), Some('C'), 46, cs());
        assert_eq!(
            fired(&st.press(&km, &e, now, &none)),
            Some("selection.copy")
        );
    }

    #[test]
    fn physical_wins_over_symbol() {
        let km = keymap(&[
            ("ctrl+a", "tab.new", &[]),
            ("ctrl+physical:KeyQ", "tab.close", &[]),
        ]);
        let mut st = KeyState::default();
        // AZERTY: the key at KeyQ produces `a`.
        let mut b = NameBuf::default();
        let e = ev(&mut b, Key::Char('a'), Some('A'), 16, BMods::CTRL);
        assert_eq!(
            fired(&st.press(&km, &e, Instant::now(), &none)),
            Some("tab.close")
        );
    }

    #[test]
    fn sequences_wait_then_fire_or_time_out() {
        let km = keymap(&[("ctrl+a>c", "tab.new", &[])]);
        let mut st = KeyState::default();
        let t0 = Instant::now();
        let mut b = NameBuf::default();
        let a = ev(&mut b, Key::Char('a'), None, 30, BMods::CTRL);
        assert_eq!(st.press(&km, &a, t0, &none), Resolved::Prefix);
        let mut b2 = NameBuf::default();
        let c = ev(&mut b2, Key::Char('c'), None, 46, BMods::NONE);
        assert_eq!(fired(&st.press(&km, &c, t0, &none)), Some("tab.new"));
        // Timeout: the second key is looked up afresh and goes to the program.
        assert_eq!(st.press(&km, &a, t0, &none), Resolved::Prefix);
        let late = t0 + Duration::from_millis(1500);
        assert_eq!(st.press(&km, &c, late, &none), Resolved::None);
        // A wrong second key falls through too.
        assert_eq!(st.press(&km, &a, t0, &none), Resolved::Prefix);
        let mut b3 = NameBuf::default();
        let x = ev(&mut b3, Key::Char('x'), None, 45, BMods::NONE);
        assert_eq!(st.press(&km, &x, t0, &none), Resolved::None);
    }

    #[test]
    fn key_tables_push_pop_one_shot_and_timeout() {
        let mut km = keymap(&[("ctrl+b", "key-table.push resize one-shot=true", &[])]);
        km.tables.push((
            "resize".into(),
            vec![KeyBind {
                seq: vec![KeyChord::parse("h").unwrap()],
                actions: vec![Action::parse("pane.resize left").unwrap()],
                opts: BindOptions::default(),
            }],
        ));
        let mut st = KeyState::default();
        let t0 = Instant::now();
        let mut b = NameBuf::default();
        let h = ev(&mut b, Key::Char('h'), None, 35, BMods::NONE);
        assert_eq!(st.press(&km, &h, t0, &none), Resolved::None);
        st.push(0, true, None, t0);
        assert_eq!(st.active(&km), ["resize"]);
        assert_eq!(fired(&st.press(&km, &h, t0, &none)), Some("pane.resize"));
        // One-shot: gone after one key.
        assert!(st.active(&km).is_empty());
        assert_eq!(st.press(&km, &h, t0, &none), Resolved::None);
        // Persistent with a timeout.
        st.push(0, false, Some(Duration::from_millis(100)), t0);
        assert_eq!(fired(&st.press(&km, &h, t0, &none)), Some("pane.resize"));
        assert_eq!(fired(&st.press(&km, &h, t0, &none)), Some("pane.resize"));
        let late = t0 + Duration::from_millis(200);
        assert_eq!(st.press(&km, &h, late, &none), Resolved::None);
        st.push(0, false, None, t0);
        st.pop();
        assert!(st.active(&km).is_empty());
    }

    #[test]
    fn mode_conditions_gate_binds() {
        let km = keymap(&[("shift+Prior", "scroll.page-up", &[("mode", "!alt-screen")])]);
        let mut st = KeyState::default();
        let mut b = NameBuf::default();
        let e = ev(&mut b, Key::Named(Named::PageUp), None, 104, BMods::SHIFT);
        let alt = |m: Mode| m == Mode::AltScreen;
        assert_eq!(st.press(&km, &e, Instant::now(), &alt), Resolved::None);
        assert_eq!(
            fired(&st.press(&km, &e, Instant::now(), &none)),
            Some("scroll.page-up")
        );
    }

    #[test]
    fn page_up_alias_is_canonical() {
        let km = keymap(&[("shift+Page_Up", "scroll.page-up", &[])]);
        let mut st = KeyState::default();
        let mut b = NameBuf::default();
        let e = ev(&mut b, Key::Named(Named::PageUp), None, 104, BMods::SHIFT);
        assert_eq!(
            fired(&st.press(&km, &e, Instant::now(), &none)),
            Some("scroll.page-up")
        );
    }

    #[test]
    fn config_binds_unbinds_and_clear_defaults() {
        let src = r#"
            version 1
            binds {
                bind "ctrl+shift+t" { pane.split "down" }
                bind "ctrl+a>c" { tab.new }
                unbind "ctrl+shift+w"
            }
        "#;
        let c = config(src);
        let km = Keymap::from_config(&c);
        assert_eq!(km.dropped, 0);
        let t = km
            .root
            .iter()
            .find(|b| b.seq == vec![KeyChord::parse("ctrl+shift+t").unwrap()])
            .unwrap();
        assert_eq!(t.actions[0].id(), "pane.split");
        assert!(
            !km.root
                .iter()
                .any(|b| b.seq == vec![KeyChord::parse("ctrl+shift+w").unwrap()])
        );
        assert!(km.root.iter().any(|b| b.seq.len() == 2));
        let c = config("version 1\nbinds clear-defaults=#true\n");
        assert!(Keymap::from_config(&c).root.is_empty());
    }

    fn config(src: &str) -> Config {
        let fs = apoth_config::fs::MemFs::new().with("/c.kdl", src);
        let input = apoth_config::LoadInput {
            user: Some("/c.kdl".into()),
            ..apoth_config::LoadInput::default()
        };
        let r = apoth_config::load(&fs, &input, None);
        assert!(
            r.diagnostics
                .iter()
                .all(|d| d.severity != apoth_config::Severity::Error),
            "{:?}",
            r.diagnostics
        );
        r.config
    }
}
