// SPDX-License-Identifier: AGPL-3.0-only
//! The Terminal collection editors (Apotheosis T-10 §9, T-11 §3-§9, M27): Key
//! binds (with key tables), Profiles and Rules, each a list you add to, edit in
//! place, reorder and remove from. The model and every edit are in
//! `term_edit`; this is state, messages and the views.
//!
//! Shapes (no two adjacent blocks alike):
//! - Key binds: a status grid, a strip of tabs (Global and one per key table),
//!   one list panel.
//! - Profiles: a bar chart of the scoring weights (how a profile is picked),
//!   one list panel.
//! - Rules: a status grid, one list panel.
//!
//! Accent ledger: Key binds, the chord recorder while it is listening (the one
//! live state). Rules, the count of rules that opted in to input or spawn, in
//! the status grid, when it is not zero. Profiles spends none: nothing on that
//! page is live. Everything else is white at 1.0 / 0.64 / 0.40.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use apoth_action::{lookup, search, FAMILIES};
use apoth_config::chord::{format_chord_seq, BIND_MODES};
use apoth_config::meta::META;
use apoth_config::model::Layer;
use apoth_config::schema::TABLE;
use apoth_config::{MEDIA_EVENTS, RULE_EVENTS};
use iced::widget::{button, column, container, pick_list, row, scrollable, text, text_input, Column, Space};
use iced::{Element, Length, Subscription, Task, Theme};

use ec_ui::theme;
use ec_ui::tokens::{color, font, size, space};
use ec_ui::widget::{
    badge, edge_note, glass_inset, glass_panel as panel, hairline, micro_label, pick_pill, pill, row_caption,
    status_cell, status_grid, BarChart, Highlight,
};

use crate::app::{App, Message};
use crate::pane::Page;
use crate::term_edit::{
    apply, check_action, gate, list_binds, list_profiles, list_rules, list_tables, read, write, BindForm,
    Entry, MatchForm, Op, ProfileEntry, ProfileForm, RuleForm, Scope, MATCH_FIELDS,
};
use crate::terminal::{dropdown_style, menu_style, text_of, Terminal};

/// Which item an open form is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum At {
    New,
    Item(usize),
}

/// The form that is open; one at a time.
#[derive(Debug, Clone)]
pub enum Edit {
    Bind(Scope, At, BindForm),
    Rule(At, RuleForm),
    Profile(At, ProfileForm, Vec<String>),
}

/// A toggle in a form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    Unbind,
    Repeat,
    Release,
    Global,
    Performable,
    AllowInput,
    AllowSpawn,
    Sticky,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Tab(Scope),
    TableName(String),
    AddTable,
    RemoveTable(String),
    New(Page),
    Edit(Page, usize),
    Cancel,
    Save,
    Remove(Page, usize),
    Move(Page, usize, bool),
    Chord(String),
    /// Listen for the next chord; `true` appends a step to a sequence.
    Record(bool),
    /// What the recorder heard: a chord, or `None` for Escape.
    Heard(Option<String>),
    Script(usize, String),
    Browse(usize),
    Query(String),
    Pick(usize, String),
    AddAction,
    RemoveAction(usize),
    Flag(Flag, bool),
    Mode(String),
    Name(String),
    Event(String),
    When(String),
    Inherits(String),
    MatchField(usize, &'static str, String),
    AddMatch,
    RemoveMatch(usize),
    KeyValue(usize, String),
    KeyAdd(String),
    KeyRemove(usize),
    KeyQuery(String),
}

#[derive(Debug, Default)]
pub struct Lists {
    text: String,
    scope: Option<Scope>,
    tables: Vec<String>,
    binds: Vec<Entry<BindForm>>,
    rules: Vec<Entry<RuleForm>>,
    profiles: Vec<ProfileEntry>,
    edit: Option<Edit>,
    new_table: String,
    /// Listening for a chord; `true` appends.
    recording: Option<bool>,
    /// The action browser is open on this slot, with its query.
    browsing: Option<usize>,
    query: String,
    key_query: String,
    error: Option<String>,
    user: PathBuf,
}

impl Lists {
    /// Re-read the user file's collections.
    pub fn refresh(&mut self, user: &Path) {
        self.user = user.to_path_buf();
        self.text = read(user);
        self.tables = list_tables(&self.text);
        let scope = self.scope().clone();
        if let Scope::Table(n) = &scope {
            if !self.tables.contains(n) {
                self.scope = Some(Scope::Global);
            }
        }
        self.binds = list_binds(&self.text, self.scope());
        self.rules = list_rules(&self.text);
        self.profiles = list_profiles(&self.text);
    }

    fn scope(&self) -> &Scope {
        self.scope.as_ref().unwrap_or(&Scope::Global)
    }

    /// Is a key being recorded? The window subscribes to keys only then.
    pub fn recording(&self) -> bool {
        self.recording.is_some()
    }

    fn close(&mut self) {
        self.edit = None;
        self.recording = None;
        self.browsing = None;
        self.query.clear();
        self.key_query.clear();
    }

    /// The names of the rules in the user's file, but for the `except`th.
    fn rule_names(&self, except: Option<usize>) -> Vec<String> {
        self.rules
            .iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) != except)
            .map(|(_, r)| r.form.name.clone())
            .collect()
    }

    /// May the profile move a place? Not past the profile it inherits, and not
    /// before one that inherits it: a parent is declared first.
    pub fn can_move_profile(&self, i: usize, down: bool) -> bool {
        let Some(me) = self.profiles.get(i) else {
            return false;
        };
        let Some(other) = self.profiles.get(if down { i + 1 } else { i.wrapping_sub(1) }) else {
            return false;
        };
        let (a, b) = (&me.form, &other.form);
        if down {
            b.inherits != a.name
        } else {
            a.inherits != b.name
        }
    }

    fn profile_names(&self, cfg: &apoth_config::Config, except: Option<usize>) -> Vec<String> {
        let mut out: Vec<String> = self
            .profiles
            .iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) != except)
            .map(|(_, p)| p.form.name.clone())
            .collect();
        for p in &cfg.profiles {
            if !out.contains(&p.value.name) && !self.profiles.iter().any(|u| u.form.name == p.value.name) {
                out.push(p.value.name.clone());
            }
        }
        out
    }
}

// ---- keys ---------------------------------------------------------------

/// What a key press means to the recorder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Heard {
    Chord(String),
    Cancel,
    Ignore,
}

fn keysym(c: char) -> Option<String> {
    let name = match c {
        ' ' => "space",
        ',' => "comma",
        '.' => "period",
        '/' => "slash",
        ';' => "semicolon",
        '\'' => "apostrophe",
        '[' => "bracketleft",
        ']' => "bracketright",
        '\\' => "backslash",
        '-' => "minus",
        '=' => "equal",
        '`' => "grave",
        '+' => "plus",
        '<' => "less",
        '>' => "greater",
        c if c.is_ascii_alphanumeric() => return Some(c.to_ascii_lowercase().to_string()),
        _ => return None,
    };
    Some(name.to_owned())
}

/// A key press as an XKB-named chord (`ctrl+shift+Prior`). The unshifted key is
/// used, so Shift is a modifier and `shift+4` stays `shift+4`.
pub fn hear(key: &iced::keyboard::Key, mods: iced::keyboard::Modifiers) -> Heard {
    use iced::keyboard::key::Named as N;
    use iced::keyboard::Key;
    let name: String = match key {
        Key::Character(s) => match s
            .chars()
            .next()
            .filter(|_| s.chars().count() == 1)
            .and_then(keysym)
        {
            Some(n) => n,
            None => return Heard::Ignore,
        },
        Key::Named(n) => match n {
            N::Escape => return Heard::Cancel,
            N::Enter => "Return".into(),
            N::Tab => "Tab".into(),
            N::Space => "space".into(),
            N::Backspace => "BackSpace".into(),
            N::Delete => "Delete".into(),
            N::Insert => "Insert".into(),
            N::Home => "Home".into(),
            N::End => "End".into(),
            N::PageUp => "Prior".into(),
            N::PageDown => "Next".into(),
            N::ArrowUp => "Up".into(),
            N::ArrowDown => "Down".into(),
            N::ArrowLeft => "Left".into(),
            N::ArrowRight => "Right".into(),
            N::ContextMenu => "Menu".into(),
            N::PrintScreen => "Print".into(),
            N::Pause => "Pause".into(),
            N::ScrollLock => "Scroll_Lock".into(),
            N::F1 => "F1".into(),
            N::F2 => "F2".into(),
            N::F3 => "F3".into(),
            N::F4 => "F4".into(),
            N::F5 => "F5".into(),
            N::F6 => "F6".into(),
            N::F7 => "F7".into(),
            N::F8 => "F8".into(),
            N::F9 => "F9".into(),
            N::F10 => "F10".into(),
            N::F11 => "F11".into(),
            N::F12 => "F12".into(),
            // Modifiers on their own, locks and the rest wait for a real key.
            _ => return Heard::Ignore,
        },
        Key::Unidentified => return Heard::Ignore,
    };
    let mut out = String::new();
    for (on, m) in [
        (mods.control(), "ctrl"),
        (mods.alt(), "alt"),
        (mods.shift(), "shift"),
        (mods.logo(), "super"),
    ] {
        if on {
            out.push_str(m);
            out.push('+');
        }
    }
    out.push_str(&name);
    Heard::Chord(out)
}

/// Key presses, while the chord recorder is listening.
pub fn record_keys() -> Subscription<Message> {
    iced::event::listen_with(|event, _status, _window| {
        let iced::Event::Keyboard(iced::keyboard::Event::KeyPressed { key, modifiers, .. }) = event else {
            return None;
        };
        let heard = match hear(&key, modifiers) {
            Heard::Chord(c) => Some(c),
            Heard::Cancel => None,
            Heard::Ignore => return None,
        };
        Some(m(Msg::Heard(heard)))
    })
}

fn m(x: Msg) -> Message {
    Message::Terminal(crate::terminal::Msg::Lists(x))
}

// ---- update -------------------------------------------------------------

fn open_item(l: &Lists, page: Page, i: usize) -> Option<Edit> {
    match page {
        Page::TermRules => {
            let e = l.rules.get(i)?;
            (!e.opaque).then(|| Edit::Rule(At::Item(i), e.form.clone()))
        }
        Page::TermProfiles => {
            let e = l.profiles.get(i)?;
            let keys = e.form.keys.iter().map(|(p, _)| p.clone()).collect();
            Some(Edit::Profile(At::Item(i), e.form.clone(), keys))
        }
        _ => {
            let e = l.binds.get(i)?;
            (!e.opaque).then(|| Edit::Bind(l.scope().clone(), At::Item(i), e.form.clone()))
        }
    }
}

fn open_index(e: &Option<Edit>) -> Option<usize> {
    match e {
        Some(
            Edit::Bind(_, At::Item(j), _) | Edit::Rule(At::Item(j), _) | Edit::Profile(At::Item(j), _, _),
        ) => Some(*j),
        _ => None,
    }
}

/// Apply `op` to the file: build it, let `apoth-config` load the result, write
/// it atomically, re-read. `false` leaves the file as it was, with the reason shown.
fn commit(t: &mut Terminal, op: &Op) -> bool {
    let user = t.user_main.clone();
    let old = read(&user);
    let result = apply(&old, op)
        .and_then(|new| gate(&user, &old, &new).map(|()| new))
        .and_then(|new| if new == old { Ok(()) } else { write(&user, &new) });
    match result {
        Ok(()) => {
            t.lists.error = None;
            t.read();
            true
        }
        Err(why) => {
            t.lists.error = Some(why);
            false
        }
    }
}

fn save(t: &mut Terminal) {
    let cfg = t.cfg();
    let Some(edit) = t.lists.edit.clone() else { return };
    let op = match edit {
        Edit::Bind(scope, At::New, f) => Op::BindAdd(scope, f),
        Edit::Bind(scope, At::Item(i), f) => Op::BindSet(scope, i, f),
        Edit::Rule(at, f) => {
            let except = match at {
                At::Item(i) => Some(i),
                At::New => None,
            };
            if let Some(why) = f.problems_among(&t.lists.rule_names(except)).first() {
                t.lists.error = Some(why.to_owned());
                return;
            }
            match at {
                At::Item(i) => Op::RuleSet(i, f),
                At::New => Op::RuleAdd(f),
            }
        }
        Edit::Profile(at, f, orig) => {
            let except = match at {
                At::Item(i) => Some(i),
                At::New => None,
            };
            let others = t.lists.profile_names(&cfg, except);
            let parents = parents_of(&t.lists, &cfg);
            if let Some(why) = f.problems(&others, &parents).first() {
                t.lists.error = Some(why.to_owned());
                return;
            }
            let kept: Vec<&String> = f.keys.iter().map(|(p, _)| p).collect();
            let removed = orig.into_iter().filter(|p| !kept.contains(&p)).collect();
            Op::ProfileSet(
                match at {
                    At::Item(i) => Some(i),
                    At::New => None,
                },
                f,
                removed,
            )
        }
    };
    if commit(t, &op) {
        t.lists.close();
    }
}

fn parents_of(l: &Lists, cfg: &apoth_config::Config) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = cfg
        .profiles
        .iter()
        .filter_map(|p| p.value.inherits.clone().map(|i| (p.value.name.clone(), i)))
        .collect();
    for p in &l.profiles {
        if !p.form.inherits.is_empty() {
            out.insert(p.form.name.clone(), p.form.inherits.clone());
        }
    }
    out
}

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    let t = &mut app.terminal;
    match msg {
        Msg::Tab(s) => {
            t.lists.close();
            t.lists.error = None;
            t.lists.scope = Some(s);
            t.lists.binds = list_binds(&t.lists.text, t.lists.scope());
        }
        Msg::TableName(n) => t.lists.new_table = n,
        Msg::AddTable => {
            let name = t.lists.new_table.trim().to_owned();
            if commit(t, &Op::TableAdd(name.clone())) {
                t.lists.new_table.clear();
                t.lists.scope = Some(Scope::Table(name));
                t.lists.binds = list_binds(&t.lists.text, t.lists.scope());
            }
        }
        Msg::RemoveTable(n) => {
            if commit(t, &Op::TableRemove(n)) {
                t.lists.close();
                t.lists.scope = Some(Scope::Global);
                t.lists.binds = list_binds(&t.lists.text, &Scope::Global);
            }
        }
        Msg::New(page) => {
            t.lists.close();
            t.lists.error = None;
            t.lists.edit = Some(match page {
                Page::TermRules => Edit::Rule(At::New, RuleForm::new()),
                Page::TermProfiles => Edit::Profile(At::New, ProfileForm::default(), Vec::new()),
                _ => Edit::Bind(t.lists.scope().clone(), At::New, BindForm::default()),
            });
        }
        Msg::Edit(page, i) => {
            let was = open_index(&t.lists.edit) == Some(i);
            t.lists.close();
            t.lists.error = None;
            if !was {
                t.lists.edit = open_item(&t.lists, page, i);
            }
        }
        Msg::Cancel => {
            t.lists.close();
            t.lists.error = None;
        }
        Msg::Save => save(t),
        Msg::Remove(page, i) => {
            let op = match page {
                Page::TermRules => Op::RuleRemove(i),
                Page::TermProfiles => Op::ProfileRemove(i),
                _ => Op::BindRemove(t.lists.scope().clone(), i),
            };
            t.lists.close();
            commit(t, &op);
        }
        Msg::Move(page, i, down) => {
            let op = match page {
                Page::TermRules => Op::RuleMove(i, down),
                Page::TermProfiles => Op::ProfileMove(i, down),
                _ => Op::BindMove(t.lists.scope().clone(), i, down),
            };
            t.lists.close();
            commit(t, &op);
        }
        other => form(&mut t.lists, other),
    }
    Task::none()
}

/// A keystroke or click inside the open form.
fn form(l: &mut Lists, msg: Msg) {
    match msg {
        Msg::Record(append) => {
            l.recording = Some(append);
            l.browsing = None;
            return;
        }
        Msg::Heard(h) => {
            let append = l.recording.take().unwrap_or(false);
            if let (Some(c), Some(Edit::Bind(_, _, f))) = (h, l.edit.as_mut()) {
                if append && !f.chord.trim().is_empty() {
                    f.chord = format!("{}>{c}", f.chord.trim());
                } else {
                    f.chord = c;
                }
            }
            return;
        }
        Msg::Browse(slot) => {
            l.browsing = if l.browsing == Some(slot) {
                None
            } else {
                Some(slot)
            };
            l.query.clear();
            return;
        }
        Msg::Query(q) => {
            l.query = q;
            return;
        }
        Msg::KeyQuery(q) => {
            l.key_query = q;
            return;
        }
        _ => {}
    }
    if matches!(msg, Msg::Pick(..)) {
        l.browsing = None;
    }
    match (l.edit.as_mut(), msg) {
        (Some(Edit::Bind(_, _, f)), Msg::Chord(c)) => f.chord = c,
        (Some(Edit::Bind(_, _, f)), Msg::Mode(c)) => f.mode = c,
        (Some(Edit::Bind(_, _, f)), Msg::Flag(flag, on)) => match flag {
            Flag::Unbind => f.unbind = on,
            Flag::Repeat => f.repeat = on,
            Flag::Release => f.release = on,
            Flag::Global => f.global = on,
            Flag::Performable => f.performable = on,
            _ => {}
        },
        (Some(Edit::Bind(_, _, f)), Msg::Script(i, s)) => set_slot(&mut f.actions, i, s),
        (Some(Edit::Bind(_, _, f)), Msg::Pick(i, s)) => set_slot(&mut f.actions, i, s),
        (Some(Edit::Bind(_, _, f)), Msg::AddAction) => f.actions.push(String::new()),
        (Some(Edit::Bind(_, _, f)), Msg::RemoveAction(i)) => drop_slot(&mut f.actions, i),
        (Some(Edit::Rule(_, f)), Msg::Name(s)) => f.name = s,
        (Some(Edit::Rule(_, f)), Msg::Event(s)) => f.event = s,
        (Some(Edit::Rule(_, f)), Msg::When(s)) => f.when = s,
        (Some(Edit::Rule(_, f)), Msg::Flag(flag, on)) => match flag {
            Flag::AllowInput => f.allow_input = on,
            Flag::AllowSpawn => f.allow_spawn = on,
            _ => {}
        },
        (Some(Edit::Rule(_, f)), Msg::Script(i, s) | Msg::Pick(i, s)) => set_slot(&mut f.actions, i, s),
        (Some(Edit::Rule(_, f)), Msg::AddAction) => f.actions.push(String::new()),
        (Some(Edit::Rule(_, f)), Msg::RemoveAction(i)) => drop_slot(&mut f.actions, i),
        (Some(Edit::Profile(at, f, _)), Msg::Name(s)) if *at == At::New => f.name = s,
        (Some(Edit::Profile(_, f, _)), Msg::Inherits(s)) => f.inherits = s,
        (Some(Edit::Profile(_, f, _)), Msg::Flag(Flag::Sticky, on)) => f.sticky = on,
        (Some(Edit::Profile(_, f, _)), Msg::MatchField(i, field, s)) => {
            if let Some(mf) = f.matches.get_mut(i) {
                *mf.field_mut(field) = s;
            }
        }
        (Some(Edit::Profile(_, f, _)), Msg::AddMatch) => f.matches.push(MatchForm::default()),
        (Some(Edit::Profile(_, f, _)), Msg::RemoveMatch(i)) => {
            if i < f.matches.len() {
                f.matches.remove(i);
            }
        }
        (Some(Edit::Profile(_, f, _)), Msg::KeyValue(i, s)) => {
            if let Some(k) = f.keys.get_mut(i) {
                k.1 = s;
            }
        }
        (Some(Edit::Profile(_, f, _)), Msg::KeyAdd(path)) => {
            if !f.keys.iter().any(|(p, _)| *p == path) {
                let text = apoth_config::schema::key(&path)
                    .map(|k| text_of(k, &k.default_value()))
                    .unwrap_or_default();
                f.keys.push((path, text));
            }
            l.key_query.clear();
        }
        (Some(Edit::Profile(_, f, _)), Msg::KeyRemove(i)) if i < f.keys.len() => {
            f.keys.remove(i);
        }
        _ => {}
    }
}

fn set_slot(slots: &mut Vec<String>, i: usize, s: String) {
    match slots.get_mut(i) {
        Some(x) => *x = s,
        None => slots.push(s),
    }
}

fn drop_slot(slots: &mut Vec<String>, i: usize) {
    if i < slots.len() {
        slots.remove(i);
    }
    if slots.is_empty() {
        slots.push(String::new());
    }
}

// ---- views --------------------------------------------------------------

/// The inherit choice that names no parent.
const NOTHING: &str = "Nothing";

pub(crate) fn tight() -> iced::Padding {
    iced::Padding::from([space::PILL_Y, space::CARD])
}

/// A quiet verb in a row: lifts to primary ink under the pointer; absent
/// (greyed, inert) when `msg` is `None`.
pub(crate) fn verb<'a>(label: &str, msg: Option<Message>) -> Element<'a, Message, Theme> {
    let t = text(label.to_owned())
        .font(font::UI_MEDIUM)
        .size(size::BODY_SMALL);
    match msg {
        Some(x) => button(t)
            .padding([space::BADGE_Y, space::BADGE_X])
            .on_press(x)
            .style(theme::link)
            .into(),
        None => button(t.style(theme::text_tertiary))
            .padding([space::BADGE_Y, space::BADGE_X])
            .style(theme::link)
            .into(),
    }
}

fn problem_line<'a>(why: Option<&str>) -> Element<'a, Message, Theme> {
    match why {
        Some(w) => container(
            text(w.to_owned())
                .font(font::UI)
                .size(size::BODY_SMALL)
                .style(theme::text_danger),
        )
        .padding([0.0, space::CARD])
        .into(),
        None => Space::new().height(0).into(),
    }
}

fn input<'a>(
    placeholder: &str,
    value: &str,
    bad: bool,
    on: impl Fn(String) -> Msg + 'a,
) -> Element<'a, Message, Theme> {
    text_input(placeholder, value)
        .on_input(move |s| m(on(s)))
        .font(font::DATA)
        .size(size::MONO)
        .style(if bad {
            theme::eclipse_input_invalid
        } else {
            theme::eclipse_input
        })
        .into()
}

/// A labelled field with its reason under it.
pub(crate) fn field<'a>(
    label: &str,
    control: Element<'a, Message, Theme>,
    why: Option<&str>,
) -> Element<'a, Message, Theme> {
    column![
        row![
            container(
                text(label.to_owned())
                    .font(font::UI)
                    .size(size::BODY_SMALL)
                    .style(theme::text_secondary)
            )
            .width(Length::Fixed(space::FIELD_W * 0.5)),
            container(control).width(Length::Fill),
        ]
        .spacing(space::CONTROL_GAP)
        .align_y(iced::Alignment::Center),
        problem_line(why),
    ]
    .into()
}

fn toggle<'a>(label: &str, on: bool, flag: Flag) -> Element<'a, Message, Theme> {
    pick_pill(label, on, Some(m(Msg::Flag(flag, !on))))
}

/// A chord as keycaps: one per modifier and key, `>` between sequence steps.
fn keycaps<'a>(chord: &str) -> Element<'a, Message, Theme> {
    let mut r = iced::widget::Row::new()
        .spacing(space::PILL_GAP)
        .align_y(iced::Alignment::Center);
    for (i, step) in chord.split('>').enumerate() {
        if i > 0 {
            r = r.push(
                text(">")
                    .font(font::DATA)
                    .size(size::MONO)
                    .style(theme::text_tertiary),
            );
        }
        for part in step.split('+') {
            r = r.push(badge(part));
        }
    }
    r.into()
}

fn summary(actions: &[String]) -> String {
    match actions {
        [] => "nothing".to_owned(),
        [a] => a.clone(),
        [a, rest @ ..] => format!("{a}  +{}", rest.len()),
    }
}

/// One list row: the item (a button, so it brightens and opens its form) and
/// the verbs beside it.
fn item_row<'a>(
    page: Page,
    i: usize,
    open: bool,
    main: Element<'a, Message, Theme>,
    editable: bool,
    can_move: (bool, bool),
) -> Element<'a, Message, Theme> {
    let body = button(
        container(main)
            .width(Length::Fill)
            .align_x(iced::Alignment::Start),
    )
    .width(Length::Fill)
    .padding(tight())
    .style(theme::fleet_row(open));
    let body = if editable || page == Page::TermProfiles {
        body.on_press(m(Msg::Edit(page, i)))
    } else {
        body
    };
    row![
        body,
        verb("Up", can_move.0.then(|| m(Msg::Move(page, i, false)))),
        verb("Down", can_move.1.then(|| m(Msg::Move(page, i, true)))),
        verb(
            if open { "Close" } else { "Edit" },
            editable.then(|| m(Msg::Edit(page, i)))
        ),
        verb("Remove", Some(m(Msg::Remove(page, i)))),
    ]
    .spacing(space::PILL_GAP)
    .align_y(iced::Alignment::Center)
    .into()
}

pub(crate) fn mono_dim<'a>(s: &str) -> Element<'a, Message, Theme> {
    text(s.to_owned())
        .font(font::DATA)
        .size(size::MONO)
        .style(theme::text_tertiary)
        .into()
}

pub(crate) fn mono_text<'a>(s: &str) -> Element<'a, Message, Theme> {
    text(s.to_owned())
        .font(font::DATA)
        .size(size::MONO)
        .style(theme::text_secondary)
        .into()
}

fn empty<'a>(headline: &str, body: &str) -> Element<'a, Message, Theme> {
    edge_note(headline, body, color::neutral())
}

fn actions_editor<'a>(
    l: &'a Lists,
    actions: &'a [String],
    problems: &crate::term_edit::Problems,
) -> Element<'a, Message, Theme> {
    let mut col = Column::new().spacing(space::PILL_GAP);
    for (i, a) in actions.iter().enumerate() {
        let why = problems.get(&format!("action:{i}"));
        let hint = a.split_whitespace().next().and_then(lookup).map(|info| {
            let params: Vec<String> = info
                .params
                .iter()
                .map(|p| {
                    if p.required {
                        p.name.to_owned()
                    } else {
                        format!("[{}]", p.name)
                    }
                })
                .collect();
            if params.is_empty() {
                info.title.to_owned()
            } else {
                format!("{}  \u{b7}  {}", info.title, params.join(" "))
            }
        });
        col = col.push(field(
            if i == 0 { "Action" } else { "then" },
            row![
                container(input("pane.split direction=right", a, why.is_some(), move |s| {
                    Msg::Script(i, s)
                }))
                .width(Length::Fill),
                pick_pill("Browse", l.browsing == Some(i), Some(m(Msg::Browse(i)))),
                verb("Remove", (actions.len() > 1).then(|| m(Msg::RemoveAction(i)))),
            ]
            .spacing(space::CONTROL_GAP)
            .align_y(iced::Alignment::Center)
            .into(),
            why,
        ));
        if let Some(h) = hint.filter(|_| why.is_none()) {
            col = col.push(row_caption(&h));
        }
        if l.browsing == Some(i) {
            col = col.push(browser(l, i));
        }
    }
    col.push(row![verb("+ Add another action", Some(m(Msg::AddAction)))])
        .into()
}

/// The registry, searchable: title and id per row, one click to pick.
fn browser<'a>(l: &'a Lists, slot: usize) -> Element<'a, Message, Theme> {
    let q = &l.query;
    let hits = search(q);
    let mut list = Column::new();
    for info in hits.iter().take(80) {
        let id = info.id.to_owned();
        list = list.push(
            button(
                row![
                    text(info.title.to_owned())
                        .font(font::UI)
                        .size(size::BODY_SMALL)
                        .style(theme::text_primary),
                    Space::new().width(Length::Fill),
                    text(info.id.to_owned())
                        .font(font::DATA)
                        .size(size::MONO)
                        .style(theme::text_tertiary),
                ]
                .spacing(space::CONTROL_GAP),
            )
            .width(Length::Fill)
            .padding([space::BADGE_Y, space::CARD])
            .on_press(m(Msg::Pick(slot, id)))
            .style(theme::fleet_row(false)),
        );
    }
    let count = format!(
        "{} of {} actions, in {} families",
        hits.len().min(80),
        apoth_action::REGISTRY.len(),
        FAMILIES.len()
    );
    column![
        text_input("Search actions by name or id", q)
            .on_input(|s| m(Msg::Query(s)))
            .font(font::UI)
            .size(size::BODY)
            .style(theme::eclipse_input),
        container(scrollable(list).style(theme::eclipse_scrollable)).height(Length::Fixed(space::FIELD_W)),
        row_caption(&count),
    ]
    .spacing(space::PILL_GAP)
    .into()
}

fn save_bar<'a>(ok: bool, new: bool) -> Element<'a, Message, Theme> {
    row![
        pick_pill(if new { "Add" } else { "Save" }, ok, ok.then(|| m(Msg::Save))),
        pill("Cancel", false, m(Msg::Cancel)),
    ]
    .spacing(space::PILL_GAP)
    .into()
}

fn bind_form<'a>(l: &'a Lists, at: At, f: &'a BindForm) -> Element<'a, Message, Theme> {
    let p = f.problems();
    let rec = l.recording;
    let chord_row: Element<'a, Message, Theme> = row![
        container(input(
            if rec.is_some() {
                "Press the keys\u{2026}  Escape cancels"
            } else {
                "ctrl+shift+t"
            },
            &f.chord,
            p.get("chord").is_some() && rec.is_none(),
            Msg::Chord,
        ))
        .width(Length::Fill),
        // The recorder is the page's live state: the one yellow while it listens.
        pill("Record", rec == Some(false), m(Msg::Record(false))),
        pill("Add step", rec == Some(true), m(Msg::Record(true))),
    ]
    .spacing(space::CONTROL_GAP)
    .align_y(iced::Alignment::Center)
    .into();
    let mut col = Column::new().spacing(space::PILL_GAP);
    col = col.push(field(
        "Keys",
        chord_row,
        if rec.is_some() { None } else { p.get("chord") },
    ));
    col = col.push(row![
        toggle("Unbind a default", f.unbind, Flag::Unbind),
        Space::new().width(Length::Fill),
    ]);
    if !f.unbind {
        col = col.push(actions_editor(l, &f.actions, &p));
        col = col.push(field(
            "Only when",
            input(
                "alt-screen, !copy-mode",
                &f.mode,
                p.get("mode").is_some(),
                Msg::Mode,
            ),
            p.get("mode"),
        ));
        col = col.push(row_caption(&format!(
            "Modes: {}. Join with commas; ! negates.",
            BIND_MODES.join(", ")
        )));
        col = col.push(
            row![
                toggle("Repeat", f.repeat, Flag::Repeat),
                toggle("On release", f.release, Flag::Release),
                toggle("Performable", f.performable, Flag::Performable),
                toggle("Global", f.global, Flag::Global),
            ]
            .spacing(space::PILL_GAP),
        );
    }
    col = col.push(save_bar(!p.any(), at == At::New));
    glass_inset(container(col).padding(space::CARD))
}

fn rule_form<'a>(l: &'a Lists, at: At, f: &'a RuleForm) -> Element<'a, Message, Theme> {
    let p = f.problems_among(&l.rule_names(match at {
        At::Item(i) => Some(i),
        At::New => None,
    }));
    let events: Vec<String> = RULE_EVENTS
        .iter()
        .chain(MEDIA_EVENTS)
        .map(|e| (*e).to_owned())
        .collect();
    let picker = pick_list(events, (!f.event.is_empty()).then(|| f.event.clone()), |e| {
        m(Msg::Event(e))
    })
    .placeholder("Pick an event")
    .font(font::DATA)
    .text_size(size::MONO)
    .style(dropdown_style)
    .menu_style(menu_style);
    let mut col = Column::new().spacing(space::PILL_GAP);
    col = col.push(field(
        "Name",
        input("long-command", &f.name, p.get("name").is_some(), Msg::Name),
        p.get("name"),
    ));
    col = col.push(field("Event", picker.into(), p.get("event")));
    col = col.push(field(
        "Only if",
        input(
            "status:!0 and duration:>10s and not focused",
            &f.when,
            p.get("where").is_some(),
            Msg::When,
        ),
        p.get("where"),
    ));
    col = col.push(row_caption(
        "Fields: id title pid cwd cmdline process host user profile tab window state var:name status duration. ~ starts a regex.",
    ));
    col = col.push(actions_editor(l, &f.actions, &p));
    col = col.push(row_caption(
        "Events a program can cause may run only layout, appearance and observe actions unless the rule opts in.",
    ));
    col = col.push(
        row![
            toggle("Allow input", f.allow_input, Flag::AllowInput),
            toggle("Allow spawn", f.allow_spawn, Flag::AllowSpawn),
        ]
        .spacing(space::PILL_GAP),
    );
    col = col.push(save_bar(!p.any(), at == At::New));
    glass_inset(container(col).padding(space::CARD))
}

fn key_label(path: &str) -> String {
    apoth_config::schema::index_of(path).map_or_else(|| path.to_owned(), |i| META[i].label.to_owned())
}

fn profile_form<'a>(
    l: &'a Lists,
    cfg: &apoth_config::Config,
    at: At,
    f: &'a ProfileForm,
) -> Element<'a, Message, Theme> {
    let except = match at {
        At::Item(i) => Some(i),
        At::New => None,
    };
    let others = l.profile_names(cfg, except);
    let p = f.problems(&others, &parents_of(l, cfg));
    let mut col = Column::new().spacing(space::PILL_GAP);
    col = col.push(field(
        "Name",
        if at == At::New {
            input("work", &f.name, p.get("name").is_some(), Msg::Name)
        } else {
            mono_text(&f.name)
        },
        p.get("name"),
    ));
    let mut names = vec![NOTHING.to_owned()];
    names.extend(others.iter().cloned());
    col = col.push(field(
        "Inherits",
        pick_list(
            names,
            Some(if f.inherits.is_empty() {
                NOTHING.to_owned()
            } else {
                f.inherits.clone()
            }),
            |n| m(Msg::Inherits(if n == NOTHING { String::new() } else { n })),
        )
        .placeholder("Nothing")
        .font(font::DATA)
        .text_size(size::MONO)
        .style(dropdown_style)
        .menu_style(menu_style)
        .into(),
        p.get("inherits"),
    ));
    col = col.push(row![toggle("Sticky", f.sticky, Flag::Sticky)]);
    col = col.push(micro_label("Picked when"));
    for (i, mf) in f.matches.iter().enumerate() {
        let mut cells = row![].spacing(space::CONTROL_GAP);
        for (name, label, hold) in [
            ("host", "Host", "*.corp"),
            ("job", "Process", "ssh"),
            ("user", "User", "root"),
            ("path", "Folder", "~/work"),
        ] {
            cells = cells.push(
                column![
                    text(label)
                        .font(font::UI)
                        .size(size::BODY_SMALL)
                        .style(theme::text_tertiary),
                    input(
                        hold,
                        mf.field(name),
                        p.get(&format!("match:{i}")).is_some(),
                        move |s| { Msg::MatchField(i, name, s) }
                    ),
                ]
                .spacing(space::BADGE_Y)
                .width(Length::FillPortion(1)),
            );
        }
        col = col.push(
            row![
                cells,
                badge(&format!("score {}", mf.score())),
                verb("Remove", Some(m(Msg::RemoveMatch(i)))),
            ]
            .spacing(space::CONTROL_GAP)
            .align_y(iced::Alignment::End),
        );
        col = col.push(problem_line(p.get(&format!("match:{i}"))));
    }
    col = col.push(row![verb("+ Add a match", Some(m(Msg::AddMatch)))]);
    col = col.push(micro_label("Overrides"));
    if f.keys.is_empty() {
        col = col.push(row_caption(
            "No settings overridden: the profile changes nothing but what it inherits.",
        ));
    }
    for (i, (path, val)) in f.keys.iter().enumerate() {
        col = col.push(field(
            &key_label(path),
            row![
                container(input(
                    "value",
                    val,
                    p.get(&format!("key:{i}")).is_some(),
                    move |s| Msg::KeyValue(i, s)
                ))
                .width(Length::Fill),
                verb("Remove", Some(m(Msg::KeyRemove(i)))),
            ]
            .spacing(space::CONTROL_GAP)
            .align_y(iced::Alignment::Center)
            .into(),
            p.get(&format!("key:{i}")),
        ));
    }
    col = col.push(
        text_input("Override a setting: search by name", &l.key_query)
            .on_input(|s| m(Msg::KeyQuery(s)))
            .font(font::UI)
            .size(size::BODY)
            .style(theme::eclipse_input),
    );
    let q = l.key_query.trim().to_lowercase();
    if !q.is_empty() {
        let mut hits = Column::new();
        for (k, meta) in TABLE
            .iter()
            .zip(META.iter())
            .filter(|(k, meta)| {
                k.profilable
                    && !f.keys.iter().any(|(p, _)| p == k.path)
                    && q.split_whitespace()
                        .all(|w| meta.label.to_lowercase().contains(w) || k.path.contains(w))
            })
            .take(6)
        {
            hits = hits.push(
                button(
                    row![
                        text(meta.label)
                            .font(font::UI)
                            .size(size::BODY_SMALL)
                            .style(theme::text_primary),
                        Space::new().width(Length::Fill),
                        text(k.path)
                            .font(font::DATA)
                            .size(size::MONO)
                            .style(theme::text_tertiary),
                    ]
                    .spacing(space::CONTROL_GAP),
                )
                .width(Length::Fill)
                .padding([space::BADGE_Y, space::CARD])
                .on_press(m(Msg::KeyAdd(k.path.to_owned())))
                .style(theme::fleet_row(false)),
            );
        }
        col = col.push(hits);
    }
    col = col.push(save_bar(!p.any(), at == At::New));
    glass_inset(container(col).padding(space::CARD))
}

fn list_head<'a>(
    heading: &str,
    count: usize,
    page: Page,
    add: &str,
    extra: Option<Element<'a, Message, Theme>>,
) -> Element<'a, Message, Theme> {
    let mut r = row![
        micro_label(heading),
        mono_dim(&match count {
            0 => "none".to_owned(),
            1 => "1 entry".to_owned(),
            n => format!("{n} entries"),
        }),
        Space::new().width(Length::Fill),
    ]
    .spacing(space::CONTROL_GAP)
    .align_y(iced::Alignment::Center);
    if let Some(e) = extra {
        r = r.push(e);
    }
    r.push(pick_pill(add, false, Some(m(Msg::New(page))))).into()
}

fn push_form<'a>(
    col: Column<'a, Message, Theme>,
    l: &'a Lists,
    cfg: &apoth_config::Config,
    want: At,
) -> Column<'a, Message, Theme> {
    match &l.edit {
        Some(Edit::Bind(_, at, f)) if *at == want => col.push(bind_form(l, *at, f)),
        Some(Edit::Rule(at, f)) if *at == want => col.push(rule_form(l, *at, f)),
        Some(Edit::Profile(at, f, _)) if *at == want => col.push(profile_form(l, cfg, *at, f)),
        _ => col,
    }
}

pub fn status(app: &App, page: Page) -> (String, String) {
    let l = &app.terminal.lists;
    let n = |c: usize, one: &str| {
        if c == 1 {
            format!("1 {one}")
        } else {
            format!("{c} {one}s")
        }
    };
    match page {
        Page::TermRules => (n(l.rules.len(), "rule"), format!("{} events watched", events(l))),
        Page::TermProfiles => (n(l.profiles.len(), "profile"), "scored by match".to_owned()),
        _ => (n(l.binds.len(), "bind"), n(l.tables.len(), "key table")),
    }
}

fn events(l: &Lists) -> usize {
    let mut e: Vec<&str> = l.rules.iter().map(|r| r.form.event.as_str()).collect();
    e.sort_unstable();
    e.dedup();
    e.len()
}

pub fn blocks(app: &App, page: Page) -> Vec<Element<'_, Message, Theme>> {
    let t = &app.terminal;
    let l = &t.lists;
    let cfg = t.cfg();
    let mut out: Vec<Element<'_, Message, Theme>> = Vec::new();
    if let Some(e) = &l.error {
        out.push(edge_note("Not written", e, color::danger()));
    }
    match page {
        Page::TermRules => {
            let opted = l
                .rules
                .iter()
                .filter(|r| r.form.allow_input || r.form.allow_spawn)
                .count();
            out.push(status_grid(
                vec![
                    status_cell(
                        "in your file",
                        &l.rules.len().to_string(),
                        "run in file order",
                        false,
                    ),
                    status_cell("events", &events(l).to_string(), "of 30 a rule can watch", false),
                    status_cell(
                        "opted in",
                        &opted.to_string(),
                        "may send input or spawn",
                        opted > 0,
                    ),
                ],
                3,
            ));
            let mut col = Column::new().spacing(space::PILL_GAP).push(list_head(
                "Rules",
                l.rules.len(),
                page,
                "Add rule",
                None,
            ));
            col = push_form(col, l, &cfg, At::New);
            if l.rules.is_empty() && l.edit.is_none() {
                col = col.push(empty("No rules of your own", "A rule runs actions when something happens in a pane. Add one, such as a notification when a long command finishes."));
            }
            for (i, e) in l.rules.iter().enumerate() {
                let open = open_index(&l.edit) == Some(i) && matches!(l.edit, Some(Edit::Rule(..)));
                let f = &e.form;
                let mut head = row![
                    text(f.name.clone())
                        .font(font::UI_MEDIUM)
                        .size(size::BODY)
                        .style(theme::text_primary),
                    badge(&f.event)
                ]
                .spacing(space::CONTROL_GAP)
                .align_y(iced::Alignment::Center);
                if f.allow_input {
                    head = head.push(badge("input"));
                }
                if f.allow_spawn {
                    head = head.push(badge("spawn"));
                }
                if e.opaque {
                    head = head.push(badge("edit in the file"));
                }
                let mut body = Column::new().spacing(space::BADGE_Y).push(head);
                if !f.when.is_empty() {
                    body = body.push(mono_dim(&format!("if {}", f.when)));
                }
                body = body.push(mono_text(&summary(&f.actions)));
                col = col.push(hairline());
                col = col.push(item_row(
                    page,
                    i,
                    open,
                    body.into(),
                    !e.opaque,
                    (i > 0, i + 1 < l.rules.len()),
                ));
                col = push_form(col, l, &cfg, At::Item(i));
            }
            let sys: Vec<_> = cfg
                .rules
                .iter()
                .filter(|r| !r.value.name.is_empty() && r.origin.layer != Layer::User)
                .collect();
            if !sys.is_empty() {
                col = col.push(hairline()).push(micro_label("From the system"));
                for r in sys {
                    col = col.push(
                        row![
                            text(r.value.name.clone())
                                .font(font::UI)
                                .size(size::BODY)
                                .style(theme::text_secondary),
                            badge(&r.value.event),
                            badge(r.origin.layer.name()),
                        ]
                        .spacing(space::CONTROL_GAP)
                        .padding(tight()),
                    );
                }
            }
            out.push(panel(app.glass_radius, app.hairline_cards, col));
        }
        Page::TermProfiles => {
            let weights = [16.0, 8.0, 4.0, 2.0, 1.0];
            let names = ["exact host", "wildcard host", "process", "user", "exact folder"];
            let mut labels = iced::widget::Row::new().spacing(3.0);
            for (n, w) in names.iter().zip(weights) {
                labels = labels.push(
                    column![
                        text(format!("{w}"))
                            .font(font::DATA_MEDIUM)
                            .size(size::MONO)
                            .style(theme::text_primary),
                        text(*n)
                            .font(font::DATA)
                            .size(size::MICRO)
                            .style(theme::text_tertiary),
                    ]
                    .width(Length::FillPortion(1)),
                );
            }
            out.push(panel(
                app.glass_radius,
                app.hairline_cards,
                column![
                    micro_label("How a profile is picked"),
                    BarChart::new(weights).max(16.0).highlight(Highlight::None),
                    labels,
                    row_caption("Each match block adds its parts; the best score wins, ties go to the earlier profile. Switching applies colours and binds only, never shell or env."),
                ]
                .spacing(space::ROW_Y),
            ));
            let mut col = Column::new().spacing(space::PILL_GAP).push(list_head(
                "Profiles",
                l.profiles.len(),
                page,
                "Add profile",
                None,
            ));
            col = push_form(col, l, &cfg, At::New);
            if l.profiles.is_empty() && l.edit.is_none() {
                col = col.push(empty("No profiles yet", "A profile carries settings for one kind of session, such as a remote host or a project folder, and can pick itself by what runs there."));
            }
            for (i, e) in l.profiles.iter().enumerate() {
                let open = open_index(&l.edit) == Some(i) && matches!(l.edit, Some(Edit::Profile(..)));
                let f = &e.form;
                let mut head = row![text(f.name.clone())
                    .font(font::UI_MEDIUM)
                    .size(size::BODY)
                    .style(theme::text_primary)]
                .spacing(space::CONTROL_GAP)
                .align_y(iced::Alignment::Center);
                if !f.inherits.is_empty() {
                    head = head.push(badge(&format!("inherits {}", f.inherits)));
                }
                if f.sticky {
                    head = head.push(badge("sticky"));
                }
                if !f.keys.is_empty() {
                    head = head.push(badge(&format!("{} set", f.keys.len())));
                }
                if e.binds > 0 {
                    head = head.push(badge(&format!("{} binds", e.binds)));
                }
                if e.security > 0 {
                    head = head.push(badge("security"));
                }
                let mut body = Column::new().spacing(space::BADGE_Y).push(head);
                let when: Vec<String> = f
                    .matches
                    .iter()
                    .map(|mf| {
                        MATCH_FIELDS
                            .iter()
                            .filter(|n| !mf.field(n).is_empty())
                            .map(|n| format!("{n}={}", mf.field(n)))
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .collect();
                body = body.push(mono_dim(&if when.is_empty() {
                    "chosen by name only".to_owned()
                } else {
                    when.join("  or  ")
                }));
                col = col.push(hairline());
                col = col.push(item_row(
                    page,
                    i,
                    open,
                    body.into(),
                    true,
                    (l.can_move_profile(i, false), l.can_move_profile(i, true)),
                ));
                col = push_form(col, l, &cfg, At::Item(i));
            }
            out.push(panel(app.glass_radius, app.hairline_cards, col));
        }
        _ => {
            let scope = l.scope().clone();
            let sys_binds = cfg.binds.iter().filter(|b| b.origin.layer != Layer::User).count();
            out.push(status_grid(
                vec![
                    status_cell(
                        "your binds",
                        &l.binds.len().to_string(),
                        "in the scope shown below",
                        false,
                    ),
                    status_cell(
                        "key tables",
                        &l.tables.len().to_string(),
                        "modes and leader keys",
                        false,
                    ),
                    status_cell(
                        "from the system",
                        &sys_binds.to_string(),
                        "binds you did not write",
                        false,
                    ),
                ],
                3,
            ));
            let mut tabs = vec![pick_pill(
                "Global",
                scope == Scope::Global,
                Some(m(Msg::Tab(Scope::Global))),
            )];
            for n in &l.tables {
                let s = Scope::Table(n.clone());
                tabs.push(pick_pill(n, scope == s, Some(m(Msg::Tab(s)))));
            }
            out.push(
                row![
                    container(ec_ui::widget::pill_group(tabs)).width(Length::Fill),
                    container(
                        text_input("New key table", &l.new_table)
                            .on_input(|s| m(Msg::TableName(s)))
                            .on_submit(m(Msg::AddTable))
                            .font(font::UI)
                            .size(size::BODY)
                            .style(theme::eclipse_input)
                    )
                    .width(Length::Fixed(space::FIELD_W)),
                    pill("Add table", false, m(Msg::AddTable)),
                ]
                .spacing(space::CONTROL_GAP)
                .align_y(iced::Alignment::Center)
                .into(),
            );
            let (heading, extra) = match &scope {
                Scope::Global => ("Binds".to_owned(), None),
                Scope::Table(n) => (
                    format!("Key table \u{b7} {n}"),
                    Some(verb("Remove table", Some(m(Msg::RemoveTable(n.clone()))))),
                ),
            };
            let mut col = Column::new().spacing(space::PILL_GAP).push(list_head(
                &heading,
                l.binds.len(),
                page,
                "Add bind",
                extra,
            ));
            col = push_form(col, l, &cfg, At::New);
            if l.binds.is_empty() && l.edit.is_none() {
                col = col.push(empty(
                    if scope == Scope::Global { "No binds of your own" } else { "This table is empty" },
                    if scope == Scope::Global {
                        "The built-in binds still work. Add a bind to change one, or unbind a default."
                    } else {
                        "Enter it with key-table.push from a bind. Keys in a table apply only while it is active."
                    },
                ));
            }
            for (i, e) in l.binds.iter().enumerate() {
                let open = open_index(&l.edit) == Some(i) && matches!(l.edit, Some(Edit::Bind(..)));
                let f = &e.form;
                let what = if f.unbind {
                    "unbound".to_owned()
                } else if e.opaque {
                    "edit in the file".to_owned()
                } else {
                    summary(&f.actions)
                };
                let mut head = row![
                    container(keycaps(&f.chord)).width(Length::Fixed(space::FIELD_W)),
                    mono_text(&what)
                ]
                .spacing(space::CONTROL_GAP)
                .align_y(iced::Alignment::Center);
                head = head.push(Space::new().width(Length::Fill));
                if !f.mode.is_empty() {
                    head = head.push(badge(&f.mode));
                }
                if !f.repeat {
                    head = head.push(badge("no repeat"));
                }
                if f.release {
                    head = head.push(badge("release"));
                }
                if f.performable {
                    head = head.push(badge("performable"));
                }
                col = col.push(hairline());
                col = col.push(item_row(
                    page,
                    i,
                    open,
                    head.into(),
                    !e.opaque,
                    (i > 0, i + 1 < l.binds.len()),
                ));
                col = push_form(col, l, &cfg, At::Item(i));
            }
            if scope == Scope::Global && sys_binds > 0 {
                col = col.push(hairline()).push(micro_label("From the system"));
                for b in cfg
                    .binds
                    .iter()
                    .filter(|b| b.origin.layer != Layer::User)
                    .take(12)
                {
                    let ids: Vec<String> = b.value.actions.iter().map(|a| a.id.clone()).collect();
                    col = col.push(
                        row![
                            container(keycaps(&format_chord_seq(&b.value.chords)))
                                .width(Length::Fixed(space::FIELD_W)),
                            mono_dim(&summary(&ids)),
                            Space::new().width(Length::Fill),
                            badge(b.origin.layer.name()),
                        ]
                        .spacing(space::CONTROL_GAP)
                        .align_y(iced::Alignment::Center)
                        .padding(tight()),
                    );
                }
            }
            out.push(panel(app.glass_radius, app.hairline_cards, col));
        }
    }
    let _ = check_action;
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::keyboard::{key::Named, Key, Modifiers};

    fn chord(k: Key, m: Modifiers) -> Heard {
        hear(&k, m)
    }

    #[test]
    fn a_pressed_chord_is_written_in_keysym_names_the_loader_reads() {
        let ctrl_shift = Modifiers::CTRL | Modifiers::SHIFT;
        let cases = [
            (Key::Character("t".into()), ctrl_shift, "ctrl+shift+t"),
            (Key::Named(Named::PageUp), Modifiers::SHIFT, "shift+Prior"),
            (Key::Named(Named::Enter), Modifiers::ALT, "alt+Return"),
            (Key::Character(",".into()), Modifiers::LOGO, "super+comma"),
            (Key::Named(Named::F5), Modifiers::empty(), "F5"),
            (Key::Character("`".into()), Modifiers::CTRL, "ctrl+grave"),
        ];
        for (k, m, want) in cases {
            assert_eq!(chord(k, m), Heard::Chord(want.to_owned()));
            assert!(apoth_config::chord::parse_chord_seq(want).is_ok(), "{want}");
        }
    }

    #[test]
    fn modifiers_alone_wait_and_escape_cancels() {
        assert_eq!(chord(Key::Named(Named::Shift), Modifiers::SHIFT), Heard::Ignore);
        assert_eq!(chord(Key::Named(Named::Control), Modifiers::CTRL), Heard::Ignore);
        assert_eq!(
            chord(Key::Named(Named::Escape), Modifiers::empty()),
            Heard::Cancel
        );
        assert_eq!(chord(Key::Unidentified, Modifiers::empty()), Heard::Ignore);
    }

    #[test]
    fn a_profile_does_not_move_past_its_parent() {
        let text = "profile \"plain\" {\n}\n\nprofile \"work\" {\n    inherits \"plain\"\n}\n\nprofile \"solo\" {\n}\n";
        let l = Lists {
            profiles: crate::term_edit::list_profiles(text),
            ..Lists::default()
        };
        // plain (0) cannot go below work (1): work would then precede its parent.
        assert!(!l.can_move_profile(0, true));
        assert!(!l.can_move_profile(1, false));
        assert!(l.can_move_profile(1, true));
        assert!(l.can_move_profile(2, false));
        assert!(!l.can_move_profile(0, false) && !l.can_move_profile(2, true));
    }

    #[test]
    fn the_window_search_is_deaf_while_a_chord_is_recorded() {
        let mut l = Lists::default();
        assert!(!l.recording());
        l.recording = Some(true);
        assert!(l.recording());
        // `subscription_inner` leaves the type-to-search listener out whenever
        // `recording()` holds, so a plain key is the recorder's alone.
        l.close();
        assert!(!l.recording());
    }

    #[test]
    fn a_rule_name_taken_by_another_rule_is_named() {
        let l = Lists {
            rules: crate::term_edit::list_rules(
                "rule \"a\" {\n    on \"bell\"\n    do {\n        tab.new\n    }\n}\n",
            ),
            ..Lists::default()
        };
        assert_eq!(l.rule_names(None), vec!["a".to_owned()]);
        assert!(l.rule_names(Some(0)).is_empty());
    }
}
