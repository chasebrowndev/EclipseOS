// SPDX-License-Identifier: AGPL-3.0-only

//! The window's behaviour: `fogd` replies routed to the tabs, keys resolved
//! through fog.kdl's bindings, mouse input, and custom actions
//! (FOG §UI and navigation). Drawing is in [`crate::view`].
//!
//! Keys are read with `keyboard::listen`, so every key reaches [`on_key`],
//! which gives them to the innermost mode first: the palette, then the path
//! editor, then the places sidebar, then the list (where a filter, once
//! typed, takes printable keys before the bindings do).

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::process::Stdio;
use std::time::Duration;

use fog_config::{Action, Chord, Config, CustomAction, Key as K, Mods, Target};
use fog_proto::{Place, Reply, Request, SortKey};
use fog_widgets::scroll_into_view;
use iced::keyboard::{self, key::Named, Key, Modifiers};
use iced::widget::operation::{scroll_to, AbsoluteOffset};
use iced::widget::Id;
use iced::{window, Subscription, Task};
use jiff::tz::TimeZone;

use crate::conn::{self, Link};
use crate::edit::{Edit, PathEdit};
use crate::palette::{Item, Palette};
use crate::state::{Browser, Click, Effect, Notice, Tabs};
use crate::theme::size;

pub fn list_id() -> Id {
    Id::new("fog-list")
}

pub struct App {
    pub tabs: Tabs,
    /// `keys { bind … }` from fog.kdl, loaded before iced starts.
    pub keys: BTreeMap<Chord, Target>,
    /// `action "…" { run … }` from fog.kdl.
    pub actions: Vec<CustomAction>,
    pub places: Vec<Place>,
    /// Which region has the keyboard, and so the accent.
    pub focus: Focus,
    /// Cursor in `places`, while the sidebar has focus.
    pub place_sel: usize,
    pub sidebar: bool,
    pub overlay: Overlay,
    /// For the modified column; read before iced starts.
    pub tz: TimeZone,
    /// `$HOME`, for `~` in the path bar.
    home: Vec<u8>,
    link: Option<Link>,
    pub fogd: Fogd,
    /// Held modifiers, for Ctrl+ and Shift+click.
    mods: Modifiers,
    /// Keys replayed one per [`SCRIPT_TICK`], for screenshots and probing
    /// on hosts without input injection. Filled from `FOG_UI_SCRIPT` in
    /// debug builds only; always empty in release.
    script: VecDeque<Scripted>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fogd {
    Connecting,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Places,
}

/// What floats over the list and takes the keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    None,
    Path(PathEdit),
    Palette(Palette),
}

#[derive(Debug, Clone)]
pub enum Message {
    Key(keyboard::Event),
    Conn(conn::Event),
    Resized,
    Script,
    /// A row was pressed.
    Row(usize),
    /// A row was double-clicked.
    OpenRow(usize),
    /// A breadcrumb or place: go there.
    Go(Vec<u8>),
    Header(SortKey),
    Tab(usize),
    /// A palette row was clicked.
    Run(Item),
    /// A click outside the overlay.
    Dismiss,
    Paste(Option<String>),
    /// A custom action was spawned, or could not be.
    Ran(String, Result<(), String>),
}

const SCRIPT_TICK: Duration = Duration::from_millis(900);

#[derive(Debug, Clone)]
struct Scripted {
    key: Key,
    mods: Modifiers,
    text: Option<String>,
}

/// `FOG_UI_SCRIPT="j j l . ctrl+p s i z e Enter"`: space-separated keys. One
/// character is that key; `Enter`, `Backspace`, `Tab`, `Esc`, `Space`,
/// `Up`, `Down` are named keys; `ctrl+`, `alt+`, `shift+` prefix modifiers;
/// `.` waits a tick. Debug builds only.
fn script() -> VecDeque<Scripted> {
    if !cfg!(debug_assertions) {
        return VecDeque::new();
    }
    let raw = std::env::var("FOG_UI_SCRIPT").unwrap_or_default();
    raw.split_whitespace().filter_map(scripted).collect()
}

fn scripted(tok: &str) -> Option<Scripted> {
    let mut mods = Modifiers::empty();
    let mut rest = tok;
    while let Some((m, r)) = rest.split_once('+').filter(|(_, r)| !r.is_empty()) {
        mods |= match m {
            "ctrl" => Modifiers::CTRL,
            "alt" => Modifiers::ALT,
            "shift" => Modifiers::SHIFT,
            _ => break,
        };
        rest = r;
    }
    let (key, text) = match rest {
        "." => return None,
        "Enter" => (Key::Named(Named::Enter), None),
        "Backspace" => (Key::Named(Named::Backspace), None),
        "Tab" => (Key::Named(Named::Tab), None),
        "Esc" => (Key::Named(Named::Escape), None),
        "Space" => (Key::Named(Named::Space), Some(" ".to_owned())),
        "Up" => (Key::Named(Named::ArrowUp), None),
        "Down" => (Key::Named(Named::ArrowDown), None),
        t => (Key::Character(t.into()), Some(t.to_owned())),
    };
    Some(Scripted { key, mods, text })
}

impl App {
    pub fn new(path: Vec<u8>, config: Config, tz: TimeZone, home: Vec<u8>) -> Self {
        let mut first = Browser::new(path);
        first.show_hidden = config.view.show_hidden;
        Self {
            tabs: Tabs::new(first),
            keys: config.keys,
            actions: config.actions,
            places: Vec::new(),
            focus: Focus::List,
            place_sel: 0,
            sidebar: true,
            overlay: Overlay::None,
            tz,
            home,
            link: None,
            fogd: Fogd::Connecting,
            mods: Modifiers::empty(),
            script: script(),
        }
    }

    fn request(&mut self, req: Request) {
        if let Some(link) = &self.link {
            if link.send(req).is_err() {
                self.link = None;
            }
        }
        // Offline: every tab's target is re-requested when the link comes
        // up, and a new connection holds no subscriptions.
    }

    /// Carry out tab `tab`'s effect. Scrolling only ever touches the active
    /// tab: the list widget is shared.
    fn apply(&mut self, tab: usize, fx: Effect) -> Task<Message> {
        let active = tab == self.tabs.index();
        match fx {
            Effect::None => Task::none(),
            Effect::Reveal(i) if active => scroll_into_view(list_id(), i, size::ROW_H),
            Effect::Entered(i) => {
                if let Some(b) = self.tabs.get(tab) {
                    let path = b.path.clone();
                    self.request(Request::FsInfo { path });
                }
                if active {
                    rescroll(i)
                } else {
                    Task::none()
                }
            }
            Effect::Reveal(_) => Task::none(),
            Effect::List(path) => {
                self.request(Request::Subscribe { path });
                Task::none()
            }
            Effect::Unsubscribe(dir) => {
                self.request(Request::Unsubscribe { dir });
                Task::none()
            }
            Effect::Open(path) => {
                self.request(Request::Open { path, app: None });
                Task::none()
            }
            Effect::SetSort(dir, sort) => {
                self.request(Request::SetSort { dir, sort });
                Task::none()
            }
        }
    }

    fn apply_active(&mut self, fx: Effect) -> Task<Message> {
        self.apply(self.tabs.index(), fx)
    }

    /// Another tab became active: its scroll position is not kept, so show
    /// it from the top with its cursor revealed, and refresh free space.
    fn switched(&mut self) -> Task<Message> {
        let b = self.tabs.active();
        let (path, sel) = (b.path.clone(), b.selected);
        self.request(Request::FsInfo { path });
        rescroll(sel)
    }

    /// Ask for completion's listing if the typed folder changed.
    fn complete(&mut self) {
        if let Overlay::Path(p) = &mut self.overlay {
            if let Some(path) = p.want() {
                self.request(Request::ListDir { path });
            }
        }
    }

    fn edit_path(&mut self, text: String) {
        let cwd = self.tabs.active().path.clone();
        self.overlay = Overlay::Path(PathEdit::new(text, self.home.clone(), cwd));
        self.complete();
    }
}

fn rescroll(i: usize) -> Task<Message> {
    scroll_to(list_id(), AbsoluteOffset { x: 0.0, y: 0.0 }).chain(scroll_into_view(
        list_id(),
        i,
        size::ROW_H,
    ))
}

pub fn update(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::Conn(conn::Event::Up(link)) => {
            app.link = Some(link);
            app.fogd = Fogd::Up;
            let targets: Vec<Vec<u8>> = app
                .tabs
                .iter_mut()
                .map(|b| {
                    b.reconnected();
                    b.target().to_vec()
                })
                .collect();
            for path in targets {
                app.request(Request::Subscribe { path });
            }
            app.request(Request::Places);
            if let Overlay::Path(p) = &mut app.overlay {
                p.forget();
            }
            app.complete();
            Task::none()
        }
        Message::Conn(conn::Event::Down) => {
            app.link = None;
            app.fogd = Fogd::Down;
            Task::none()
        }
        Message::Conn(conn::Event::Reply(Reply::PlacesList(places))) => {
            app.place_sel = app.place_sel.min(places.len().saturating_sub(1));
            app.places = places;
            Task::none()
        }
        Message::Conn(conn::Event::Reply(r)) => {
            if let Overlay::Path(p) = &mut app.overlay {
                p.on_reply(&r);
            }
            let fxs = app.tabs.on_reply(r);
            Task::batch(fxs.into_iter().map(|(t, fx)| app.apply(t, fx)))
        }
        Message::Key(keyboard::Event::ModifiersChanged(m)) => {
            app.mods = m;
            Task::none()
        }
        Message::Key(keyboard::Event::KeyPressed {
            key,
            modified_key,
            modifiers,
            text,
            ..
        }) => on_key(app, &key, &modified_key, text.as_deref(), modifiers),
        Message::Key(_) => Task::none(),
        // A shrinking window can leave the selection below the fold.
        Message::Resized if app.tabs.active().len() > 0 => {
            let sel = app.tabs.active().selected;
            app.apply_active(Effect::Reveal(sel))
        }
        Message::Resized => Task::none(),
        Message::Script => match app.script.pop_front() {
            Some(s) => on_key(app, &s.key, &s.key, s.text.as_deref(), s.mods),
            None => Task::none(),
        },
        Message::Row(i) => {
            app.overlay = Overlay::None;
            app.focus = Focus::List;
            let how = if app.mods.control() {
                Click::Toggle
            } else if app.mods.shift() {
                Click::Extend
            } else {
                Click::Only
            };
            app.tabs.active_mut().click(i, how);
            Task::none()
        }
        Message::OpenRow(i) => {
            let b = app.tabs.active_mut();
            b.click(i, Click::Only);
            let fx = b.open();
            app.apply_active(fx)
        }
        Message::Go(path) => {
            app.overlay = Overlay::None;
            let fx = app.tabs.active_mut().go(path);
            app.apply_active(fx)
        }
        Message::Header(key) => {
            let fx = app.tabs.active_mut().sort_by(key);
            app.apply_active(fx)
        }
        Message::Tab(i) => {
            app.tabs.select(i);
            app.switched()
        }
        Message::Run(item) => {
            app.overlay = Overlay::None;
            run_item(app, item)
        }
        Message::Dismiss => {
            app.overlay = Overlay::None;
            Task::none()
        }
        Message::Paste(Some(s)) => {
            let line: String = s.lines().next().unwrap_or_default().to_owned();
            match &mut app.overlay {
                Overlay::Path(p) => {
                    p.edit(Edit::Insert(line));
                    app.complete();
                }
                Overlay::Palette(p) => p.edit(Edit::Insert(line)),
                Overlay::None => {}
            }
            Task::none()
        }
        Message::Paste(None) => Task::none(),
        Message::Ran(name, res) => {
            app.tabs.active_mut().notice = Some(match res {
                Ok(()) => Notice::Ran(name),
                Err(e) => Notice::RunFailed(name, e),
            });
            Task::none()
        }
    }
}

/// Text a key typed, if it is printable and no Ctrl, Alt or Super is held.
fn typed(text: Option<&str>, m: Modifiers) -> Option<&str> {
    text.filter(|t| {
        !(m.control() || m.alt() || m.logo()) && !t.is_empty() && !t.chars().any(char::is_control)
    })
}

/// The line-editing meaning of a key, if it has one.
fn edit_of(key: &Key, text: Option<&str>, m: Modifiers) -> Option<Edit> {
    if let Some(t) = typed(text, m) {
        return Some(Edit::Insert(t.to_owned()));
    }
    Some(match key.as_ref() {
        Key::Named(Named::Backspace) if m.control() => Edit::DeleteWord,
        Key::Named(Named::Backspace) => Edit::Backspace,
        Key::Named(Named::Delete) => Edit::Delete,
        Key::Named(Named::ArrowLeft) => Edit::Left,
        Key::Named(Named::ArrowRight) => Edit::Right,
        Key::Named(Named::Home) => Edit::Home,
        Key::Named(Named::End) => Edit::End,
        Key::Character("w") if m.control() => Edit::DeleteWord,
        Key::Character("a") if m.control() => Edit::Home,
        Key::Character("e") if m.control() => Edit::End,
        _ => return None,
    })
}

fn is_paste(key: &Key, m: Modifiers) -> bool {
    m.control() && matches!(key.as_ref(), Key::Character("v"))
}

/// Up or Down (or Shift+Tab / Tab in the palette) as a step.
fn step_of(key: &Key, m: Modifiers) -> Option<isize> {
    match key.as_ref() {
        Key::Named(Named::ArrowUp) => Some(-1),
        Key::Named(Named::ArrowDown) => Some(1),
        Key::Character("p") if m.control() => Some(-1),
        Key::Character("n") if m.control() => Some(1),
        _ => None,
    }
}

pub fn on_key(
    app: &mut App,
    key: &Key,
    modified: &Key,
    text: Option<&str>,
    m: Modifiers,
) -> Task<Message> {
    let esc = matches!(key, Key::Named(Named::Escape));
    let enter = matches!(key, Key::Named(Named::Enter));
    let tab = matches!(key, Key::Named(Named::Tab));
    match &mut app.overlay {
        Overlay::Palette(p) => {
            let n = p.matches(&app.actions).len();
            if esc {
                app.overlay = Overlay::None;
            } else if enter {
                let chosen = p.chosen(&app.actions);
                app.overlay = Overlay::None;
                if let Some(item) = chosen {
                    return run_item(app, item);
                }
            } else if let Some(d) = step_of(key, m) {
                p.step(d, n);
            } else if tab {
                p.step(if m.shift() { -1 } else { 1 }, n);
            } else if is_paste(key, m) {
                return iced::clipboard::read().map(Message::Paste);
            } else if let Some(e) = edit_of(key, text, m) {
                p.edit(e);
            }
            return Task::none();
        }
        Overlay::Path(p) => {
            let hidden = app.tabs.active().show_hidden;
            if esc {
                app.overlay = Overlay::None;
            } else if enter {
                let path = p.target();
                app.overlay = Overlay::None;
                let fx = app.tabs.active_mut().go(path);
                return app.apply_active(fx);
            } else if tab {
                p.accept(hidden);
                app.complete();
            } else if let Some(d) = step_of(key, m) {
                let n = p.candidates(hidden).len();
                p.step(d, n);
            } else if is_paste(key, m) {
                return iced::clipboard::read().map(Message::Paste);
            } else if let Some(e) = edit_of(key, text, m) {
                p.edit(e);
                app.complete();
            }
            return Task::none();
        }
        Overlay::None => {}
    }

    let bound = chord(key, modified, m).and_then(|c| app.keys.get(&c).cloned());

    if app.focus == Focus::Places {
        if esc {
            app.focus = Focus::List;
            return Task::none();
        }
        let n = app.places.len();
        match bound {
            Some(Target::Action(Action::Down)) => {
                app.place_sel = (app.place_sel + 1).min(n.saturating_sub(1))
            }
            Some(Target::Action(Action::Up)) => app.place_sel = app.place_sel.saturating_sub(1),
            Some(Target::Action(Action::Top)) => app.place_sel = 0,
            Some(Target::Action(Action::Bottom)) => app.place_sel = n.saturating_sub(1),
            Some(Target::Action(Action::Open)) => {
                if let Some(p) = app.places.get(app.place_sel) {
                    let path = p.path.clone();
                    app.focus = Focus::List;
                    let fx = app.tabs.active_mut().go(path);
                    return app.apply_active(fx);
                }
            }
            Some(t) => return run(app, t),
            None => {}
        }
        return Task::none();
    }

    if esc {
        let fx = app.tabs.active_mut().escape();
        return app.apply_active(fx);
    }
    let typed = typed(text, m);
    if !app.tabs.active().filter.is_empty() {
        // While filtering, typing extends the filter and Backspace edits it;
        // arrows, Enter and chorded bindings still work.
        if matches!(key, Key::Named(Named::Backspace)) && !m.control() {
            let fx = app.tabs.active_mut().pop_filter();
            return app.apply_active(fx);
        }
        if let Some(t) = typed {
            let fx = app.tabs.active_mut().push_filter(t);
            return app.apply_active(fx);
        }
    } else if let Some(t @ ("/" | "~")) = typed {
        let start = if t == "~" { "~/" } else { "/" };
        app.edit_path(start.to_owned());
        return Task::none();
    }
    match (bound, typed) {
        (Some(t), _) => run(app, t),
        // An unbound printable key starts the filter. Space never does: a
        // leading space is never what was meant.
        (None, Some(t)) if t != " " => {
            let fx = app.tabs.active_mut().push_filter(t);
            app.apply_active(fx)
        }
        (None, _) => Task::none(),
    }
}

fn run_item(app: &mut App, item: Item) -> Task<Message> {
    match item {
        Item::Action(a) => run_action(app, a),
        Item::Custom(i) => run_custom(app, i),
    }
}

fn run(app: &mut App, t: Target) -> Task<Message> {
    match t {
        Target::Action(a) => run_action(app, a),
        Target::Custom(name) => match app.actions.iter().position(|c| c.name == name) {
            Some(i) => run_custom(app, i),
            None => Task::none(),
        },
    }
}

/// Run a built-in action on the active tab. File operations belong to
/// later units: they only say so in the status line.
pub fn run_action(app: &mut App, a: Action) -> Task<Message> {
    let b = app.tabs.active_mut();
    let fx = match a {
        Action::Down => b.step(1),
        Action::Up => b.step(-1),
        Action::Open => b.open(),
        Action::Parent => b.parent(),
        Action::Top => b.first(),
        Action::Bottom => b.last(),
        Action::Visual => {
            b.visual();
            Effect::None
        }
        Action::SelectAll => {
            b.select_all();
            Effect::None
        }
        Action::ToggleHidden => b.toggle_hidden(),
        Action::SortName => b.sort_by(SortKey::Name),
        Action::SortSize => b.sort_by(SortKey::Size),
        Action::SortModified => b.sort_by(SortKey::Modified),
        Action::SortType => b.sort_by(SortKey::Type),
        Action::SortReverse => b.sort_reverse(),
        Action::SortDirsFirst => b.sort_dirs_first(),
        Action::QuickLook
        | Action::Copy
        | Action::Cut
        | Action::Paste
        | Action::Rename
        | Action::Trash
        | Action::Delete
        | Action::Undo
        | Action::NewFolder
        | Action::SplitToggle => {
            b.notice = Some(Notice::Unavailable(a.name()));
            Effect::None
        }
        Action::Palette => {
            app.overlay = Overlay::Palette(Palette::default());
            return Task::none();
        }
        Action::PathEdit => {
            let mut text = String::from_utf8_lossy(&b.path).into_owned();
            if !text.ends_with('/') {
                text.push('/');
            }
            app.edit_path(text);
            return Task::none();
        }
        Action::NewTab => {
            let fx = app.tabs.open();
            let t = app.apply_active(fx);
            return t.chain(rescroll(0));
        }
        Action::CloseTab => {
            return match app.tabs.close() {
                Some(fx) => {
                    let t = app.apply_active(fx);
                    t.chain(app.switched())
                }
                None => iced::exit(),
            };
        }
        Action::NextTab | Action::PrevTab => {
            app.tabs.cycle(if a == Action::NextTab { 1 } else { -1 });
            return app.switched();
        }
        Action::ToggleSidebar => {
            app.sidebar = !app.sidebar;
            if !app.sidebar {
                app.focus = Focus::List;
            }
            return Task::none();
        }
        Action::FocusPlaces => {
            app.focus = match app.focus {
                Focus::List if !app.places.is_empty() => {
                    app.sidebar = true;
                    Focus::Places
                }
                _ => Focus::List,
            };
            return Task::none();
        }
    };
    app.apply_active(fx)
}

/// Spawn custom action `i` on the executor, never the UI thread, with
/// `FOG_CWD` and `FOG_SELECTION` (newline-separated paths) set and `{cwd}`
/// expanded in its arguments.
fn run_custom(app: &mut App, i: usize) -> Task<Message> {
    let Some(a) = app.actions.get(i) else {
        return Task::none();
    };
    let b = app.tabs.active();
    let cwd = b.path.clone();
    let selection = b.targets().join(&b'\n');
    let (name, argv) = (a.name.clone(), a.run.clone());
    Task::perform(spawn(argv, cwd, selection), move |r| {
        Message::Ran(name.clone(), r)
    })
}

async fn spawn(argv: Vec<String>, cwd: Vec<u8>, selection: Vec<u8>) -> Result<(), String> {
    let Some((prog, args)) = argv.split_first() else {
        return Err("empty run".into());
    };
    let mut cmd = tokio::process::Command::new(prog);
    for a in args {
        cmd.arg(expand_cwd(a, &cwd));
    }
    cmd.env("FOG_CWD", OsString::from_vec(cwd.clone()))
        .env("FOG_SELECTION", OsString::from_vec(selection))
        .current_dir(OsString::from_vec(cwd))
        .stdin(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    // Reap it whenever it exits; nothing waits on the result.
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(())
}

/// `{cwd}` in an argument replaced by the raw folder path.
fn expand_cwd(arg: &str, cwd: &[u8]) -> OsString {
    let mut out = Vec::with_capacity(arg.len());
    let mut parts = arg.split("{cwd}");
    if let Some(first) = parts.next() {
        out.extend_from_slice(first.as_bytes());
    }
    for p in parts {
        out.extend_from_slice(cwd);
        out.extend_from_slice(p.as_bytes());
    }
    OsString::from_vec(out)
}

/// The config chord for a key press. With Ctrl, Alt or Super held the
/// unmodified key names it (`ctrl+shift+n`, not a control character);
/// otherwise the produced character does, so `G` and `:` read as typed.
pub fn chord(key: &Key, modified: &Key, m: Modifiers) -> Option<Chord> {
    let mods = Mods {
        ctrl: m.control(),
        alt: m.alt(),
        shift: m.shift(),
        logo: m.logo(),
    };
    let k = match (
        mods.ctrl || mods.alt || mods.logo,
        key.as_ref(),
        modified.as_ref(),
    ) {
        (true, Key::Character(s), _) | (false, _, Key::Character(s)) => {
            let mut cs = s.chars();
            match (cs.next(), cs.next()) {
                (Some(c), None) => K::Char(c),
                _ => return None,
            }
        }
        (_, Key::Named(n), _) | (_, _, Key::Named(n)) => match n {
            Named::Enter => K::Enter,
            Named::Backspace => K::Backspace,
            Named::Tab => K::Tab,
            Named::Space => K::Space,
            Named::Delete => K::Delete,
            Named::Insert => K::Insert,
            Named::Home => K::Home,
            Named::End => K::End,
            Named::PageUp => K::PageUp,
            Named::PageDown => K::PageDown,
            Named::ArrowUp => K::Up,
            Named::ArrowDown => K::Down,
            Named::ArrowLeft => K::Left,
            Named::ArrowRight => K::Right,
            Named::F1 => K::F(1),
            Named::F2 => K::F(2),
            Named::F3 => K::F(3),
            Named::F4 => K::F(4),
            Named::F5 => K::F(5),
            Named::F6 => K::F(6),
            Named::F7 => K::F(7),
            Named::F8 => K::F(8),
            Named::F9 => K::F(9),
            Named::F10 => K::F(10),
            Named::F11 => K::F(11),
            Named::F12 => K::F(12),
            _ => return None,
        },
        _ => return None,
    };
    Some(Chord::new(mods, k))
}

pub fn subscription(app: &App) -> Subscription<Message> {
    let base = [
        keyboard::listen().map(Message::Key),
        conn::subscription().map(Message::Conn),
        window::resize_events().map(|_| Message::Resized),
    ];
    if app.script.is_empty() {
        Subscription::batch(base)
    } else {
        Subscription::batch(
            base.into_iter()
                .chain([iced::time::every(SCRIPT_TICK).map(|_| Message::Script)]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fog_proto::{Entry, Kind};

    fn app() -> App {
        App::new(
            b"/w".to_vec(),
            fog_config::defaults(),
            TimeZone::UTC,
            b"/home/u".to_vec(),
        )
    }

    fn listed(app: &mut App, names: &[&str]) {
        let _ = update(
            app,
            Message::Conn(conn::Event::Reply(Reply::DirSnapshot {
                path: b"/w".to_vec(),
                dir: 1,
                generation: 0,
                entries: names
                    .iter()
                    .map(|n| Entry::new(n.as_bytes().to_vec(), Kind::File))
                    .collect(),
                order: (0..names.len() as u32).collect(),
                complete: true,
            })),
        );
    }

    fn press(app: &mut App, s: &str) {
        let k = scripted(s).unwrap();
        let _ = on_key(app, &k.key, &k.key, k.text.as_deref(), k.mods);
    }

    #[test]
    fn key_presses_resolve_through_the_default_bindings() {
        let keys = fog_config::defaults().keys;
        let hit = |k: Key, m: Key, mods| keys.get(&chord(&k, &m, mods).unwrap()).cloned();
        let act = |a| Some(Target::Action(a));
        let ch = |s: &str| Key::Character(s.into());
        assert_eq!(hit(ch("g"), ch("G"), Modifiers::SHIFT), act(Action::Bottom));
        assert_eq!(hit(ch("g"), ch("g"), Modifiers::empty()), act(Action::Top));
        assert_eq!(
            hit(ch(";"), ch(":"), Modifiers::SHIFT),
            act(Action::Palette)
        );
        let n = Modifiers::CTRL | Modifiers::SHIFT;
        assert_eq!(hit(ch("n"), ch("\u{e}"), n), act(Action::NewFolder));
        let del = Key::Named(Named::Delete);
        assert_eq!(
            hit(del.clone(), del.clone(), Modifiers::SHIFT),
            act(Action::Delete)
        );
        let down = Key::Named(Named::ArrowDown);
        assert_eq!(
            hit(down.clone(), down, Modifiers::empty()),
            act(Action::Down)
        );
        let tab = Key::Named(Named::Tab);
        assert_eq!(
            hit(tab.clone(), tab, Modifiers::CTRL | Modifiers::SHIFT),
            act(Action::PrevTab)
        );
    }

    #[test]
    fn unbound_keys_filter_and_bound_ones_still_act() {
        let mut a = app();
        listed(&mut a, &["alpha", "beta", "delta", "gamma"]);
        // `j` is bound: it moves.
        press(&mut a, "j");
        assert_eq!(a.tabs.active().selected, 1);
        // `e` is not: it starts the filter; then `l` joins it, bound or not.
        press(&mut a, "e");
        press(&mut a, "l");
        assert_eq!(a.tabs.active().filter, "el");
        assert_eq!(a.tabs.active().len(), 1);
        press(&mut a, "Backspace");
        assert_eq!(a.tabs.active().filter, "e");
        press(&mut a, "Down");
        assert_eq!(a.tabs.active().selected, 1);
        press(&mut a, "Esc");
        assert_eq!(a.tabs.active().filter, "");
        assert_eq!(a.tabs.active().len(), 4);
    }

    #[test]
    fn slash_and_ctrl_l_edit_the_path_and_esc_leaves() {
        let mut a = app();
        listed(&mut a, &["x"]);
        press(&mut a, "~");
        let Overlay::Path(p) = &a.overlay else {
            panic!("no path edit")
        };
        assert_eq!(p.line.text(), "~/");
        // Bound keys type while editing.
        press(&mut a, "j");
        let Overlay::Path(p) = &a.overlay else {
            panic!("no path edit")
        };
        assert_eq!(p.target(), b"/home/u/j");
        press(&mut a, "Esc");
        assert_eq!(a.overlay, Overlay::None);
        press(&mut a, "ctrl+l");
        let Overlay::Path(p) = &a.overlay else {
            panic!("no path edit")
        };
        assert_eq!(p.line.text(), "/w/");
        press(&mut a, "Enter");
        assert_eq!(a.overlay, Overlay::None);
        assert_eq!(a.tabs.active().target(), b"/w");
    }

    #[test]
    fn palette_runs_what_it_matches() {
        let mut a = app();
        listed(&mut a, &["a", "b"]);
        press(&mut a, ":");
        assert!(matches!(a.overlay, Overlay::Palette(_)));
        for c in "toggle-hid".chars() {
            press(&mut a, &c.to_string());
        }
        press(&mut a, "Enter");
        assert_eq!(a.overlay, Overlay::None);
        assert!(a.tabs.active().show_hidden);
        press(&mut a, "ctrl+p");
        for c in "botto".chars() {
            press(&mut a, &c.to_string());
        }
        press(&mut a, "Enter");
        assert_eq!(a.tabs.active().selected, 1);
    }

    #[test]
    fn tabs_open_cycle_and_close() {
        let mut a = app();
        listed(&mut a, &["a"]);
        press(&mut a, "ctrl+t");
        assert_eq!((a.tabs.len(), a.tabs.index()), (2, 1));
        press(&mut a, "ctrl+Tab");
        assert_eq!(a.tabs.index(), 0);
        press(&mut a, "ctrl+w");
        assert_eq!(a.tabs.len(), 1);
    }

    #[test]
    fn places_take_focus_and_open() {
        let mut a = app();
        listed(&mut a, &["a"]);
        let _ = update(
            &mut a,
            Message::Conn(conn::Event::Reply(Reply::PlacesList(vec![
                Place {
                    kind: fog_proto::PlaceKind::Home,
                    label: "Home".into(),
                    path: b"/home/u".to_vec(),
                },
                Place {
                    kind: fog_proto::PlaceKind::Trash,
                    label: "Trash".into(),
                    path: b"/home/u/.local/share/Trash/files".to_vec(),
                },
            ]))),
        );
        press(&mut a, "Tab");
        assert_eq!(a.focus, Focus::Places);
        press(&mut a, "j");
        assert_eq!(a.place_sel, 1);
        press(&mut a, "k");
        press(&mut a, "Enter");
        assert_eq!(a.focus, Focus::List);
        assert_eq!(a.tabs.active().target(), b"/home/u");
        press(&mut a, "ctrl+b");
        assert!(!a.sidebar);
    }

    #[test]
    fn custom_action_args_expand_cwd() {
        assert_eq!(expand_cwd("--cwd={cwd}", b"/a b"), "--cwd=/a b");
        assert_eq!(expand_cwd("x", b"/a"), "x");
        assert_eq!(expand_cwd("{cwd}{cwd}", b"/a"), "/a/a");
    }
}
