// SPDX-License-Identifier: AGPL-3.0-only
//! Open with (Apotheosis T-14 §11, T-10 §3): the `open.handlers` rules, edited
//! in place on Terminal > Paste & security, in the "Open with" group beside
//! `open.placement` (a generic pill row).
//!
//! A rule maps a MIME type, `type/*` or `x-scheme-handler/<scheme>` to a
//! handler (`apoth-icat`, `apoth-play`, `<id>.desktop` or `opener`) and, for
//! the two terminal handlers, an optional placement. Every write goes through
//! `apoth_config::edit::Editor` (`map_set` / `map_remove`, byte-range splices
//! on the user file), then the same load gate the M27 editors use, then an
//! atomic write. The built-in defaults are listed read-only beneath the rules,
//! so an empty list says what it means.
//!
//! A Security-class key (T-10 §1.1): like the other Security rows on the
//! Terminal pages it carries a `security` badge and says what it controls; the
//! form shows the exact entry it writes before it is saved.
//!
//! Shape: rows inside the page's one panel, the open form a single inset.
//! Accent ledger: none. Every choice here is a white lift (`pick_pill`,
//! `fleet_row`); the page keeps its one yellow where `terminal.rs` puts it.

use std::path::{Path, PathBuf};

use apoth_config::edit::Editor;
use apoth_config::lang::{validate_open_handler, OPEN_PLACEMENTS, OPEN_TERMINAL_HANDLERS};
use apoth_config::model::{Config, Layer};
use apoth_config::types::Value;
use apoth_config::value::kdl_str;
use iced::widget::{button, column, container, row, scrollable, text, text_input, Column, Space};
use iced::{Element, Length, Subscription, Task, Theme};

use ec_ui::theme;
use ec_ui::tokens::{font, size, space};
use ec_ui::widget::{badge, glass_inset, hairline, micro_label, pick_pill, pill, pill_group, row_caption};

use crate::app::{App, Message};
use crate::term_edit::{gate, read, write};
use crate::term_lists::{field, mono_dim, mono_text, tight, verb};
use crate::terminal::Terminal;

/// The key this editor owns.
pub const HANDLERS: &str = "open.handlers";
const PLACEMENT: &str = "open.placement";

/// Types the MIME field offers while it is typed into, most asked-for first.
pub const COMMON_TYPES: &[&str] = &[
    "image/*",
    "video/*",
    "audio/*",
    "application/pdf",
    "text/html",
    "text/plain",
    "text/markdown",
    "inode/directory",
    "image/png",
    "image/jpeg",
    "image/svg+xml",
    "video/mp4",
    "audio/mpeg",
    "application/zip",
    "application/json",
    "x-scheme-handler/https",
    "x-scheme-handler/http",
    "x-scheme-handler/mailto",
    "x-scheme-handler/ssh",
];

/// How many suggestions show under the MIME field at once.
const SUGGESTIONS: usize = 6;

/// What an empty rule list means (T-14 §11, "Default handler"), in the order
/// the opener tries it.
pub const BUILT_INS: &[(&str, &str)] = &[
    ("image/*", "apoth icat"),
    ("video/*, audio/*", "apoth play"),
    ("everything else", "desktop default, then browser"),
];

/// An installed desktop app and the types it declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopApp {
    pub id: String,
    pub name: String,
    pub mimes: Vec<String>,
}

/// One rule as the form edits it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rule {
    pub mime: String,
    pub handler: String,
    /// Empty: `open.placement`.
    pub placement: String,
}

impl Rule {
    fn from_entry(mime: &str, args: &[String]) -> Rule {
        Rule {
            mime: mime.to_owned(),
            handler: args.first().cloned().unwrap_or_default(),
            placement: args.get(1).cloned().unwrap_or_default(),
        }
    }

    /// The map entry's arguments: handler, then the placement if any.
    pub fn args(&self) -> Vec<String> {
        let mut a = vec![self.handler.trim().to_owned()];
        if is_terminal(&self.handler) && !self.placement.is_empty() {
            a.push(self.placement.clone());
        }
        a
    }

    /// The type as it is written: MIME types are case-insensitive, and the
    /// opener compares them so (`eq_ignore_ascii_case`), so a rule is saved in
    /// lower case and `Image/PNG` is the same rule as `image/png`.
    pub fn mime_key(&self) -> String {
        self.mime.trim().to_ascii_lowercase()
    }

    /// What is wrong with the type alone, or `None` (an empty type is not yet
    /// wrong). `taken` are the other rules' types, lower case.
    pub fn mime_problem(&self, taken: &[String]) -> Option<String> {
        let mime = self.mime_key();
        if mime.is_empty() {
            return None;
        }
        if taken.contains(&mime) {
            return Some(format!("{mime} already has a rule. Edit that one instead."));
        }
        validate_open_handler(&mime, &["opener".to_owned()]).err()
    }

    /// Why this rule cannot be saved, or `None`.
    pub fn problem(&self, taken: &[String]) -> Option<String> {
        if self.mime_key().is_empty() {
            return Some("Name a type, such as image/png or video/*.".to_owned());
        }
        if let Some(why) = self.mime_problem(taken) {
            return Some(why);
        }
        if self.handler.trim().is_empty() {
            return Some("Pick what opens it.".to_owned());
        }
        validate_open_handler(&self.mime_key(), &self.args()).err()
    }

    /// The entry exactly as the editor writes it into the file.
    pub fn kdl(&self) -> String {
        let mime = self.mime_key();
        let mime = mime.as_str();
        edited("", None, Some(self))
            .ok()
            .and_then(|t| {
                t.lines()
                    .map(str::trim)
                    .find(|l| l.starts_with(&kdl_str(mime)) || l.starts_with(mime))
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| {
                std::iter::once(mime.to_owned())
                    .chain(self.args())
                    .map(|s| kdl_str(&s))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
    }
}

fn is_terminal(handler: &str) -> bool {
    OPEN_TERMINAL_HANDLERS.contains(&handler)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum At {
    New,
    Item(usize),
}

#[derive(Debug, Clone)]
pub enum Msg {
    New,
    Edit(usize),
    Remove(usize),
    Cancel,
    Save,
    Mime(String),
    Handler(String),
    Placement(String),
    Picker,
    Query(String),
    /// Enter in the picker's search: take the first handler it lists.
    PickFirst,
    Undo,
    Dismiss,
    Key(Key),
}

/// A key the open form answers (`keys`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Tab; `true` with Shift.
    Tab(bool),
    Enter,
    Escape,
}

/// The form's text fields, in Tab order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Field {
    #[default]
    Type,
    Find,
}

const TYPE_FIELD: &str = "open-with:type";
const FIND_FIELD: &str = "open-with:find";

/// A rule just removed: what Undo puts back. `before` is the file as it was;
/// while the file is still exactly `after`, Undo restores it byte for byte
/// (the rule, its place and its comments), otherwise it adds the rule again.
#[derive(Debug, Clone)]
struct Removed {
    rule: Rule,
    at: usize,
    before: String,
    after: String,
}

/// The editor's state: the user file's rules and the open form.
#[derive(Debug, Default)]
pub struct Open {
    rules: Vec<Rule>,
    edit: Option<(At, Rule)>,
    picker: bool,
    query: String,
    /// Read the first time a name or the picker needs them.
    apps: Option<Vec<DesktopApp>>,
    error: Option<String>,
    user: PathBuf,
    focus: Field,
    removed: Option<Removed>,
}

/// The `open.handlers` entries in a file's text, in file order.
pub fn list(text: &str) -> Vec<Rule> {
    match Editor::parse(text)
        .ok()
        .and_then(|e| e.get(HANDLERS).ok().flatten())
    {
        Some(Value::Map(m)) => m.iter().map(|(k, v)| Rule::from_entry(k, v)).collect(),
        _ => Vec::new(),
    }
}

/// `text` with the entry `old` replaced in place by `rule` (both given), `rule`
/// appended (no `old`), or `old` removed (no `rule`). Only that entry's bytes
/// change; an unchanged rule leaves the text identical.
pub fn edited(text: &str, old: Option<&str>, rule: Option<&Rule>) -> Result<String, String> {
    let mut ed = Editor::parse(text).map_err(|e| e.to_string())?;
    match (old, rule) {
        (Some(o), Some(r)) => ed.map_replace(HANDLERS, o, &r.mime_key(), &r.args()),
        (None, Some(r)) => ed.map_set(HANDLERS, &r.mime_key(), &r.args()),
        (Some(o), None) => ed.map_remove(HANDLERS, o).map(|_| ()),
        (None, None) => Ok(()),
    }
    .map_err(|e| e.to_string())?;
    Ok(ed.text())
}

/// The installed, shown desktop apps with the types they declare, read with
/// the launcher's scanner; `MimeType=` comes from the file itself, as
/// `maximize.rs` reads `StartupWMClass=`.
fn installed() -> Vec<DesktopApp> {
    #[cfg(not(test))]
    let entries = ec_services::apps::scan(None);
    #[cfg(test)]
    let entries: Vec<ec_services::apps::Entry> = Vec::new();
    let dirs = ec_services::apps::search_path();
    let mut out: Vec<DesktopApp> = Vec::new();
    for e in entries.iter().filter(|e| !ec_services::apps::is_path_binary(e)) {
        if e.id.contains('/') || !e.id.ends_with(".desktop") || out.iter().any(|a| a.id == e.id) {
            continue;
        }
        let mimes = dirs
            .iter()
            .find_map(|d| std::fs::read_to_string(d.join(&e.id)).ok())
            .map(|t| mime_types(&t))
            .unwrap_or_default();
        out.push(DesktopApp {
            id: e.id.clone(),
            name: e.name.clone(),
            mimes,
        });
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

/// `MimeType=` of the `[Desktop Entry]` group.
pub fn mime_types(text: &str) -> Vec<String> {
    let mut in_entry = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
        } else if in_entry {
            if let Some(v) = line.strip_prefix("MimeType=") {
                return v
                    .split(';')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect();
            }
        }
    }
    Vec::new()
}

/// Does an app that declares `declared` handle `wanted` (which may be `type/*`)?
pub fn declares(declared: &[String], wanted: &str) -> bool {
    let wanted = wanted.trim();
    if wanted.is_empty() {
        return false;
    }
    let family = |m: &str| m.split_once('/').map(|(t, _)| t.to_owned());
    declared.iter().any(|d| {
        d == wanted
            || (wanted.ends_with("/*") && family(d) == family(wanted))
            || (d.ends_with("/*") && family(d) == family(wanted))
    })
}

impl Open {
    pub fn refresh(&mut self, user: &Path) {
        self.user = user.to_path_buf();
        self.rules = list(&read(user));
        if self.apps.is_none() && self.rules.iter().any(|r| r.handler.ends_with(".desktop")) {
            self.apps = Some(installed());
        }
    }

    fn apps(&mut self) -> &[DesktopApp] {
        self.apps.get_or_insert_with(installed)
    }

    fn close(&mut self) {
        self.edit = None;
        self.picker = false;
        self.query.clear();
        self.focus = Field::Type;
    }

    /// Is a form open? The window's type-to-search is off while it is, and
    /// the form hears Tab, Enter and Escape (`keys`).
    pub fn editing(&self) -> bool {
        self.edit.is_some()
    }

    /// The other rules' types, lower case.
    fn taken(&self, except: At) -> Vec<String> {
        self.rules
            .iter()
            .enumerate()
            .filter(|(i, _)| except != At::Item(*i))
            .map(|(_, r)| r.mime.trim().to_ascii_lowercase())
            .collect()
    }

    /// A handler's name: the app's for a desktop id, the built-in's otherwise.
    fn name_of(&self, handler: &str) -> String {
        match handler {
            "apoth-icat" => "apoth icat".to_owned(),
            "apoth-play" => "apoth play".to_owned(),
            "opener" => "Link open command".to_owned(),
            id => self
                .apps
                .as_deref()
                .and_then(|a| a.iter().find(|a| a.id == id))
                .map_or_else(|| id.to_owned(), |a| a.name.clone()),
        }
    }
}

fn m(x: Msg) -> Message {
    Message::Terminal(crate::terminal::Msg::Open(x))
}

/// Check `new` loads, write it, re-read; `false` leaves the file as it was.
fn put(t: &mut Terminal, old: &str, new: Result<String, String>) -> bool {
    let user = t.user_main.clone();
    let result = new
        .and_then(|new| gate(&user, old, &new).map(|()| new))
        .and_then(|new| if new == old { Ok(()) } else { write(&user, &new) });
    match result {
        Ok(()) => {
            t.open.error = None;
            t.read();
            true
        }
        Err(why) => {
            t.open.error = Some(why);
            false
        }
    }
}

/// Apply one edit (see [`edited`]) to the user file.
/// The pane does not watch the file, so the edit is checked against the text
/// as it is now: a rule removed outside Settings is not "removed" again, and
/// one added outside is not silently overwritten. Either way the list reloads.
fn commit(t: &mut Terminal, old: Option<&str>, rule: Option<&Rule>) -> bool {
    let text = read(&t.user_main);
    let fresh = list(&text);
    let has = |m: &str| fresh.iter().any(|r| r.mime.trim().eq_ignore_ascii_case(m.trim()));
    let stale = match (old, rule) {
        (Some(o), _) if !has(o) => Some(format!("{o} was changed outside Settings.")),
        (o, Some(r)) if o.is_none_or(|o| !o.eq_ignore_ascii_case(&r.mime)) && has(&r.mime) => {
            Some(format!("{} already has a rule in the file.", r.mime))
        }
        _ => None,
    };
    if let Some(why) = stale {
        t.read();
        if old.is_some() {
            // Row indices may have shifted; an open edit form would point at the wrong rule.
            t.open.close();
        }
        t.open.error = Some(format!("{why} The list has been reloaded."));
        return false;
    }
    let ok = put(t, &text, edited(&text, old, rule));
    if ok {
        t.open.removed = None;
    }
    ok
}

/// Focus a form field.
fn focus(o: &mut Open, f: Field) -> Task<Message> {
    o.focus = f;
    iced::widget::operation::focus(match f {
        Field::Type => TYPE_FIELD,
        Field::Find => FIND_FIELD,
    })
}

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    let links = links_command(&app.terminal.cfg());
    let t = &mut app.terminal;
    let o = &mut t.open;
    match msg {
        Msg::New => {
            // Add rule toggles the new-rule form; it never resets a draft.
            if matches!(o.edit, Some((At::New, _))) {
                o.close();
                return Task::none();
            }
            if o.edit.is_some() {
                return Task::none();
            }
            o.error = None;
            o.edit = Some((At::New, Rule::default()));
            return focus(o, Field::Type);
        }
        Msg::Edit(i) => {
            let was = matches!(o.edit, Some((At::Item(j), _)) if j == i);
            if was {
                o.close();
                return Task::none();
            }
            if o.edit.is_some() {
                return Task::none();
            }
            o.error = None;
            o.edit = o.rules.get(i).cloned().map(|r| (At::Item(i), r));
            return focus(o, Field::Type);
        }
        Msg::Remove(i) => {
            if o.edit.is_some() {
                return Task::none();
            }
            let Some(r) = o.rules.get(i).cloned() else {
                return Task::none();
            };
            let before = read(&t.user_main);
            if commit(t, Some(&r.mime), None) {
                t.open.removed = Some(Removed {
                    rule: r,
                    at: i,
                    before,
                    after: read(&t.user_main),
                });
            }
        }
        Msg::Undo => {
            let Some(gone) = o.removed.take() else {
                return Task::none();
            };
            let now = read(&t.user_main);
            if now == gone.after {
                put(t, &now, Ok(gone.before));
            } else {
                commit(t, None, Some(&gone.rule));
            }
        }
        Msg::Dismiss => o.removed = None,
        Msg::Cancel => {
            o.close();
            o.error = None;
        }
        Msg::Save => {
            let Some((at, rule)) = o.edit.clone() else {
                return Task::none();
            };
            if rule.problem(&o.taken(at)).is_some() {
                return Task::none();
            }
            let old = match at {
                At::Item(i) => o.rules.get(i).map(|r| r.mime.clone()),
                At::New => None,
            };
            if commit(t, old.as_deref(), Some(&rule)) {
                t.open.close();
            }
        }
        Msg::Picker => {
            o.picker = !o.picker;
            o.query.clear();
            if o.picker {
                o.apps();
                return focus(o, Field::Find);
            }
            return focus(o, Field::Type);
        }
        Msg::Query(q) => {
            o.query = q;
            o.focus = Field::Find;
        }
        Msg::PickFirst => {
            let mime = o.edit.as_ref().map(|(_, r)| r.mime_key()).unwrap_or_default();
            if let Some(c) = choices(o, &mime, &links).into_iter().next() {
                return update(app, Msg::Handler(c.id));
            }
        }
        Msg::Mime(s) => {
            if let Some((_, r)) = o.edit.as_mut() {
                r.mime = s;
            }
            o.focus = Field::Type;
        }
        Msg::Handler(h) => {
            if let Some((_, r)) = o.edit.as_mut() {
                if !is_terminal(&h) {
                    r.placement.clear();
                }
                r.handler = h;
            }
            o.picker = false;
            o.query.clear();
            return focus(o, Field::Type);
        }
        Msg::Placement(p) => {
            if let Some((_, r)) = o.edit.as_mut() {
                r.placement = p;
            }
        }
        Msg::Key(k) => {
            match k {
                Key::Escape if o.picker => {
                    o.picker = false;
                    o.query.clear();
                    return focus(o, Field::Type);
                }
                Key::Escape => {
                    o.close();
                    o.error = None;
                }
                // Type, then the handler's search (opening the picker), and round.
                Key::Tab(_) => {
                    let next = match o.focus {
                        Field::Type => Field::Find,
                        Field::Find => Field::Type,
                    };
                    if next == Field::Find && !o.picker {
                        o.picker = true;
                        o.query.clear();
                        o.apps();
                    }
                    return focus(o, next);
                }
                Key::Enter => return update(app, Msg::Save),
            }
        }
    }
    Task::none()
}

/// Tab, Shift+Tab, Enter and Escape while a form is open. Enter inside a field
/// is that field's own submit, so only an Enter no widget took counts here.
pub fn keys() -> Subscription<Message> {
    use iced::keyboard::{key::Named, Event, Key as K};
    iced::event::listen_with(|event, status, _window| {
        let iced::Event::Keyboard(Event::KeyPressed { key, modifiers, .. }) = event else {
            return None;
        };
        let k = match key {
            K::Named(Named::Tab) if !modifiers.control() && !modifiers.alt() => Key::Tab(modifiers.shift()),
            K::Named(Named::Escape) if modifiers.is_empty() => Key::Escape,
            K::Named(Named::Enter) if modifiers.is_empty() && status == iced::event::Status::Ignored => {
                Key::Enter
            }
            _ => return None,
        };
        Some(m(Msg::Key(k)))
    })
}

/// Debug builds: `SETTINGS_PREVIEW_OPEN=new|picker|edit|dup|bad|remove` opens
/// the form (with the picker, a duplicate or a malformed type) or removes the
/// first rule, so each state can be screenshotted without injected input.
#[cfg(debug_assertions)]
pub fn preview_env(app: &mut App) {
    let Some(v) = std::env::var("SETTINGS_PREVIEW_OPEN")
        .ok()
        .filter(|v| !v.is_empty())
    else {
        return;
    };
    match v.as_str() {
        "remove" => {
            let _ = update(app, Msg::Remove(0));
        }
        "edit" => {
            let _ = update(app, Msg::Edit(0));
        }
        _ => {
            let _ = update(app, Msg::New);
        }
    }
    match v.as_str() {
        "picker" => {
            let _ = update(app, Msg::Mime("image/png".to_owned()));
            let _ = update(app, Msg::Picker);
        }
        "dup" => {
            let _ = update(app, Msg::Mime("Image/*".to_owned()));
        }
        "bad" => {
            let _ = update(app, Msg::Mime("image".to_owned()));
        }
        _ => {}
    }
}

// ---- view -----------------------------------------------------------------

fn ui_text<'a>(s: &str) -> Element<'a, Message, Theme> {
    text(s.to_owned())
        .font(font::UI_MEDIUM)
        .size(size::BODY)
        .style(theme::text_primary)
        .into()
}

fn placement_of(cfg: &Config) -> String {
    cfg.get(PLACEMENT)
        .and_then(Value::as_str)
        .unwrap_or("float")
        .to_owned()
}

/// The "Open with" row and everything under it.
pub fn view<'a>(t: &'a Terminal, cfg: &Config, label: &str) -> Element<'a, Message, Theme> {
    let o = &t.open;
    let n = o.rules.len();
    let head = row![
        text(label.to_owned())
            .font(font::UI)
            .size(size::BODY)
            .style(theme::text_primary),
        badge("security"),
        Space::new().width(Length::Fill),
        mono_dim(&match n {
            0 => "no rules".to_owned(),
            1 => "1 rule".to_owned(),
            n => format!("{n} rules"),
        }),
        // A toggle for the new-rule form; inert while another rule is open,
        // so a draft is never thrown away by a stray click.
        pick_pill(
            "Add rule",
            matches!(o.edit, Some((At::New, _))),
            (!matches!(o.edit, Some((At::Item(_), _)))).then(|| m(Msg::New))
        ),
    ]
    .spacing(space::CONTROL_GAP)
    .align_y(iced::Alignment::Center);
    let mut col = Column::new()
        .spacing(space::PILL_GAP)
        .push(container(head).padding(tight()))
        .push(row_caption(
            "Chooses the program that runs when you open a path or link from the terminal. Nothing opens on its own; a rule only answers Open.",
        ));
    if let Some(e) = &o.error {
        col = col.push(
            container(
                text(e.clone())
                    .font(font::UI)
                    .size(size::BODY_SMALL)
                    .style(theme::text_danger),
            )
            .padding([0.0, space::CARD]),
        );
    }
    if let Some((At::New, r)) = &o.edit {
        col = col.push(form(o, cfg, At::New, r));
    }
    let busy = o.edit.is_some();
    for (i, r) in o.rules.iter().enumerate() {
        if let Some(gone) = o.removed.as_ref().filter(|g| g.at == i) {
            col = col.push(hairline()).push(removed_row(o, gone));
        }
        let open = matches!(o.edit, Some((At::Item(j), _)) if j == i);
        let mut line = row![
            container(mono_text(&r.mime)).width(Length::Fixed(space::FIELD_W)),
            mono_dim("\u{2192}"),
            ui_text(&o.name_of(&r.handler)),
        ]
        .spacing(space::CONTROL_GAP)
        .align_y(iced::Alignment::Center);
        if r.handler.ends_with(".desktop") {
            line = line.push(mono_dim(&r.handler));
        }
        line = line.push(Space::new().width(Length::Fill));
        if is_terminal(&r.handler) {
            line = line.push(badge(if r.placement.is_empty() {
                "default placement"
            } else {
                &r.placement
            }));
        }
        // While a form is open only its own row answers: a draft is never
        // dropped by opening or removing another rule.
        let live = open || !busy;
        let body = button(container(line).width(Length::Fill))
            .width(Length::Fill)
            .padding(tight())
            .on_press_maybe(live.then(|| m(Msg::Edit(i))))
            .style(theme::fleet_row(open));
        col = col.push(hairline()).push(
            row![
                body,
                // One slot for Edit and Close, so the placement badge stays put.
                container(verb(
                    if open { "Close" } else { "Edit" },
                    live.then(|| m(Msg::Edit(i)))
                ))
                .width(Length::Fixed(space::NUMBER_W))
                .align_x(iced::Alignment::Center),
                verb("Remove", (!busy).then(|| m(Msg::Remove(i)))),
            ]
            .spacing(space::PILL_GAP)
            .align_y(iced::Alignment::Center),
        );
        if open {
            col = col.push(form(o, cfg, At::Item(i), r_open(o).unwrap_or(r)));
        }
    }
    if let Some(gone) = o.removed.as_ref().filter(|g| g.at >= o.rules.len()) {
        col = col.push(hairline()).push(removed_row(o, gone));
    }
    // Entries from /etc or an include the user does not own.
    let user_types: Vec<&str> = o.rules.iter().map(|r| r.mime.as_str()).collect();
    if cfg
        .origin(HANDLERS)
        .is_some_and(|o| o.layer != Layer::User && o.layer != Layer::Default)
    {
        if let Some(Value::Map(sys)) = cfg.get(HANDLERS) {
            let sys: Vec<_> = sys
                .iter()
                .filter(|(k, _)| !user_types.contains(&k.as_str()))
                .collect();
            if !sys.is_empty() {
                col = col.push(hairline()).push(micro_label("From the system"));
                for (k, v) in sys {
                    let r = Rule::from_entry(k, v);
                    col = col.push(
                        container(
                            row![
                                container(mono_dim(&r.mime)).width(Length::Fixed(space::FIELD_W)),
                                mono_dim("\u{2192}"),
                                mono_dim(&o.name_of(&r.handler)),
                            ]
                            .spacing(space::CONTROL_GAP),
                        )
                        .padding(tight()),
                    );
                }
            }
        }
    }
    let placement = placement_of(cfg);
    col = col.push(hairline()).push(
        container(micro_label(if n == 0 {
            "Built in \u{b7} what opens now"
        } else {
            "Built in \u{b7} for types without a rule"
        }))
        .padding([0.0, space::CARD]),
    );
    for (ty, what) in BUILT_INS {
        let mut line = row![
            container(mono_dim(ty)).width(Length::Fixed(space::FIELD_W)),
            mono_dim("\u{2192}"),
            text((*what).to_owned())
                .font(font::UI)
                .size(size::BODY)
                .style(theme::text_secondary),
            Space::new().width(Length::Fill),
        ]
        .spacing(space::CONTROL_GAP)
        .align_y(iced::Alignment::Center);
        if what.starts_with("apoth") {
            line = line.push(mono_dim(&format!("in a {placement}")));
        }
        col = col.push(container(line).padding(tight()));
    }
    container(col)
        .id(crate::app::row_id(HANDLERS))
        .width(Length::Fill)
        .into()
}

/// Where a removed rule was: what went, and the way back.
fn removed_row<'a>(o: &Open, gone: &Removed) -> Element<'a, Message, Theme> {
    container(
        row![
            mono_dim(&format!(
                "Removed {} \u{2192} {}",
                gone.rule.mime,
                o.name_of(&gone.rule.handler)
            )),
            Space::new().width(Length::Fill),
            verb("Undo", Some(m(Msg::Undo))),
            verb("Dismiss", Some(m(Msg::Dismiss))),
        ]
        .spacing(space::PILL_GAP)
        .align_y(iced::Alignment::Center),
    )
    .padding(tight())
    .into()
}

fn r_open(o: &Open) -> Option<&Rule> {
    o.edit.as_ref().map(|(_, r)| r)
}

/// The add / edit form: type, handler (with its picker), placement, the entry
/// it writes, and the verbs.
fn form<'a>(o: &'a Open, cfg: &Config, at: At, r: &'a Rule) -> Element<'a, Message, Theme> {
    let taken = o.taken(at);
    let why = r.problem(&taken);
    // The type is judged as soon as it is typed; the handler once one is chosen.
    let mime_why = r.mime_problem(&taken);
    let mime_bad = mime_why.is_some();
    let mime = text_input("image/png, video/* or x-scheme-handler/https", &r.mime)
        .id(TYPE_FIELD)
        .on_input(|s| m(Msg::Mime(s)))
        .on_submit(m(Msg::Save))
        .font(font::DATA)
        .size(size::MONO)
        .style(if mime_bad {
            theme::eclipse_input_invalid
        } else {
            theme::eclipse_input
        });
    let mut col = Column::new().spacing(space::PILL_GAP);
    col = col.push(field("Type", mime.into(), mime_why.as_deref()));
    let typed = r.mime.trim().to_lowercase();
    let hints: Vec<Element<'a, Message, Theme>> = COMMON_TYPES
        .iter()
        .filter(|c| **c != typed && !taken.iter().any(|t| t == *c) && c.contains(typed.as_str()))
        .take(SUGGESTIONS)
        .map(|c| pick_pill(c, false, Some(m(Msg::Mime((*c).to_owned())))))
        .collect();
    if !hints.is_empty() {
        col = col.push(field("", pill_group(hints), None));
    }
    let chosen: Element<'a, Message, Theme> = if r.handler.is_empty() {
        text("Nothing chosen")
            .font(font::UI)
            .size(size::BODY)
            .style(theme::text_tertiary)
            .into()
    } else {
        ui_text(&o.name_of(&r.handler))
    };
    col = col.push(field(
        "Open with",
        row![
            chosen,
            mono_dim(if r.handler.ends_with(".desktop") {
                r.handler.as_str()
            } else {
                ""
            }),
            Space::new().width(Length::Fill),
            pick_pill("Choose\u{2026}", o.picker, Some(m(Msg::Picker))),
        ]
        .spacing(space::CONTROL_GAP)
        .align_y(iced::Alignment::Center)
        .into(),
        None,
    ));
    if o.picker {
        col = col.push(picker(o, cfg, r));
    }
    if is_terminal(&r.handler) {
        let mut pills = vec![pick_pill(
            &format!("Default \u{b7} {}", placement_of(cfg)),
            r.placement.is_empty(),
            Some(m(Msg::Placement(String::new()))),
        )];
        for p in OPEN_PLACEMENTS {
            let label = match *p {
                "inline" => "Split pane",
                "window" => "Own window",
                _ => "Float",
            };
            pills.push(pick_pill(
                label,
                r.placement == *p,
                Some(m(Msg::Placement((*p).to_owned()))),
            ));
        }
        col = col.push(field("Shows in", pill_group(pills), None));
    }
    // A Security key: the entry is shown exactly as it will be written, and
    // only when it can be.
    if why.is_none() {
        col = col.push(hairline());
        col = col.push(
            row![
                micro_label("Writes"),
                mono_text(&format!("open {{ handlers {{ {} }} }}", r.kdl())),
            ]
            .spacing(space::CONTROL_GAP)
            .align_y(iced::Alignment::Center),
        );
    }
    // A handler that does not fit the rule (a placement on a desktop app);
    // the type's own problems are under its field, and an unchosen handler
    // only leaves Add inert.
    if let Some(w) = why
        .as_deref()
        .filter(|_| mime_why.is_none() && !r.mime_key().is_empty() && !r.handler.is_empty())
    {
        col = col.push(
            text(w.to_owned())
                .font(font::UI)
                .size(size::BODY_SMALL)
                .style(theme::text_danger),
        );
    }
    let ok = why.is_none();
    col = col.push(
        row![
            pick_pill(
                if at == At::New { "Add" } else { "Save" },
                ok,
                ok.then(|| m(Msg::Save))
            ),
            pill("Cancel", false, m(Msg::Cancel)),
        ]
        .spacing(space::PILL_GAP),
    );
    glass_inset(container(col).padding(space::CARD))
}

/// One handler the picker lists.
#[derive(Debug, Clone)]
struct Choice {
    section: String,
    name: String,
    id: String,
    note: String,
}

/// `links.open-command` as one line, for the picker's note.
fn links_command(cfg: &Config) -> String {
    match cfg.get("links.open-command") {
        Some(Value::List(l)) => l.join(" "),
        _ => "xdg-open {url}".to_owned(),
    }
}

/// The handlers the picker lists, in T-14 §11 order: the terminal handlers,
/// the apps that declare the type, the link command, then every other app;
/// filtered by the picker's search.
fn choices(o: &Open, mime: &str, links: &str) -> Vec<Choice> {
    let q = o.query.trim().to_lowercase();
    let hit = |name: &str, id: &str| {
        q.is_empty() || name.to_lowercase().contains(&q) || id.to_lowercase().contains(&q)
    };
    let mut out = Vec::new();
    let mut push = |section: &str, name: &str, id: &str, note: &str| {
        if hit(name, id) {
            out.push(Choice {
                section: section.to_owned(),
                name: name.to_owned(),
                id: id.to_owned(),
                note: note.to_owned(),
            });
        }
    };
    push(
        "In the terminal",
        "apoth icat",
        "apoth-icat",
        "images, in the terminal",
    );
    push(
        "In the terminal",
        "apoth play",
        "apoth-play",
        "video and audio, in the terminal",
    );
    let apps = o.apps.as_deref().unwrap_or_default();
    let fits = format!("Apps for {mime}");
    for a in apps.iter().filter(|a| declares(&a.mimes, mime)) {
        push(&fits, &a.name, &a.id, &a.id);
    }
    push("Link command", "Link open command", "opener", links);
    for a in apps.iter().filter(|a| !declares(&a.mimes, mime)) {
        push("Other apps", &a.name, &a.id, &a.id);
    }
    out
}

/// The handler picker: a search (Enter takes the first hit), then the
/// choices under their section labels.
fn picker<'a>(o: &'a Open, cfg: &Config, r: &'a Rule) -> Element<'a, Message, Theme> {
    let all = choices(o, &r.mime_key(), &links_command(cfg));
    let mut list = Column::new();
    let mut section = String::new();
    for c in &all {
        if c.section != section {
            section = c.section.clone();
            list = list.push(
                container(micro_label(&section))
                    .padding(iced::Padding::ZERO.left(space::CARD).top(space::PILL_GAP)),
            );
        }
        list = list.push(
            button(
                row![
                    text(c.name.clone())
                        .font(font::UI)
                        .size(size::BODY_SMALL)
                        .style(theme::text_primary),
                    Space::new().width(Length::Fill),
                    text(c.note.clone())
                        .font(font::DATA)
                        .size(size::MONO)
                        .style(theme::text_tertiary),
                ]
                .spacing(space::CONTROL_GAP),
            )
            .width(Length::Fill)
            .padding([space::BADGE_Y, space::CARD])
            .on_press(m(Msg::Handler(c.id.clone())))
            .style(theme::fleet_row(r.handler == c.id)),
        );
    }
    let apps = o.apps.as_deref().map_or(0, <[_]>::len);
    column![
        text_input("Search handlers and apps", &o.query)
            .id(FIND_FIELD)
            .on_input(|s| m(Msg::Query(s)))
            .on_submit(m(Msg::PickFirst))
            .font(font::UI)
            .size(size::BODY)
            .style(theme::eclipse_input),
        container(scrollable(list).style(theme::eclipse_scrollable)).height(Length::Fixed(space::FIELD_W)),
        row_caption(&format!(
            "{} handlers \u{b7} {apps} desktop apps installed \u{b7} Enter takes the first, Escape closes",
            all.len()
        )),
    ]
    .spacing(space::PILL_GAP)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(mime: &str, handler: &str, placement: &str) -> Rule {
        Rule {
            mime: mime.into(),
            handler: handler.into(),
            placement: placement.into(),
        }
    }

    #[test]
    fn rules_are_added_edited_and_removed_through_the_editor() {
        let start = "// mine\nfont { size 13.0; }\n";
        let a = edited(start, None, Some(&rule("image/*", "apoth-icat", "inline"))).unwrap();
        assert!(a.starts_with("// mine\nfont { size 13.0; }\n"), "{a}");
        let b = edited(&a, None, Some(&rule("text/html", "firefox.desktop", ""))).unwrap();
        assert_eq!(
            list(&b),
            vec![
                rule("image/*", "apoth-icat", "inline"),
                rule("text/html", "firefox.desktop", "")
            ]
        );
        let c = edited(&b, Some("image/*"), Some(&rule("image/png", "apoth-icat", ""))).unwrap();
        assert_eq!(list(&c).len(), 2);
        assert!(list(&c)
            .iter()
            .any(|r| r.mime == "image/png" && r.placement.is_empty()));
        let d = edited(&c, Some("image/png"), None).unwrap();
        let e = edited(&d, Some("text/html"), None).unwrap();
        assert!(list(&e).is_empty());
        assert!(e.starts_with("// mine\n"));
        assert_eq!(
            rule("image/*", "apoth-icat", "float")
                .kdl()
                .split_whitespace()
                .count(),
            3
        );
        let user = std::env::temp_dir().join("ec-settings-open-handlers.kdl");
        gate(&user, "", &b).unwrap();
    }

    const SEED: &str = "// mine\nopen {\n    handlers {\n        // images in term\n        \"image/*\" \"apoth-icat\" \"inline\"\n        // docs\n        \"application/pdf\" \"org.gnome.Evince.desktop\"\n    }\n}\n";

    #[test]
    fn saves_keep_comments_quotes_and_order() {
        // An unchanged save is a byte-for-byte no-op.
        let same = edited(
            SEED,
            Some("image/*"),
            Some(&rule("image/*", "apoth-icat", "inline")),
        )
        .unwrap();
        assert_eq!(same, SEED);
        // An edit rewrites only that entry, in its place, under its comment.
        let e = edited(SEED, Some("image/*"), Some(&rule("Image/PNG", "apoth-icat", ""))).unwrap();
        assert_eq!(
            e,
            SEED.replace(
                "\"image/*\" \"apoth-icat\" \"inline\"",
                "\"image/png\" \"apoth-icat\""
            )
        );
        assert_eq!(list(&e)[0].mime, "image/png");
        // An add appends and leaves the others alone.
        let a = edited(SEED, None, Some(&rule("x-scheme-handler/https", "opener", ""))).unwrap();
        assert!(a.starts_with(&SEED[..SEED.find("    }\n}").unwrap()]), "{a}");
        assert_eq!(list(&a).len(), 3);
        assert_eq!(list(&a)[2].mime, "x-scheme-handler/https");
        // A remove cuts that entry's line only.
        let r = edited(SEED, Some("application/pdf"), None).unwrap();
        assert!(r.contains("// images in term") && r.contains("// docs") && !r.contains("Evince"));
    }

    #[test]
    fn types_are_compared_without_case() {
        let taken = vec!["image/png".to_owned()];
        assert!(rule("Image/PNG", "apoth-icat", "").mime_problem(&taken).is_some());
        assert_eq!(rule(" Video/MP4 ", "apoth-play", "").mime_key(), "video/mp4");
        assert!(rule("image", "", "").mime_problem(&[]).is_some());
        assert!(rule("", "", "").mime_problem(&[]).is_none());
    }

    #[test]
    fn a_form_names_what_is_wrong() {
        assert!(rule("", "apoth-icat", "").problem(&[]).is_some());
        assert!(rule("image", "apoth-icat", "").problem(&[]).is_some());
        assert!(rule("image/png", "", "").problem(&[]).is_some());
        assert!(rule("image/png", "apoth-icat", "")
            .problem(&["image/png".into()])
            .is_some());
        assert!(rule("image/png", "apoth-icat", "float").problem(&[]).is_none());
        assert!(rule("x-scheme-handler/https", "opener", "")
            .problem(&[])
            .is_none());
        // A placement only rides on a terminal handler.
        assert_eq!(
            rule("text/html", "firefox.desktop", "float").args(),
            vec!["firefox.desktop"]
        );
        for c in COMMON_TYPES {
            assert!(rule(c, "opener", "").problem(&[]).is_none(), "{c}");
        }
    }

    #[test]
    fn desktop_types_match_exact_and_family() {
        let text = "[Desktop Entry]\nName=Viewer\nMimeType=image/png;image/jpeg;\n[Desktop Action x]\nMimeType=text/plain;\n";
        let m = mime_types(text);
        assert_eq!(m, vec!["image/png", "image/jpeg"]);
        assert!(declares(&m, "image/png"));
        assert!(declares(&m, "image/*"));
        assert!(!declares(&m, "image/gif"));
        assert!(!declares(&m, "text/plain"));
        assert!(declares(&["video/*".into()], "video/mp4"));
    }
}
