// SPDX-License-Identifier: AGPL-3.0-only

//! The window's behaviour: `ec-fogd` replies routed to the tabs, keys resolved
//! through fog.kdl's bindings, mouse input, and custom actions
//! (FOG §UI and navigation). Drawing is in [`crate::view`].
//!
//! Keys are read with `keyboard::listen`, so every key reaches [`on_key`],
//! which gives them to the innermost mode first: a pending conflict, then
//! the overlay (palette, path editor, inline name, delete confirmation),
//! then the places sidebar or the job tray, then the list (where a filter,
//! once typed, takes printable keys before the bindings do).
//!
//! File operations (FOG §File operations) are jobs sent to `ec-fogd` through
//! [`Tray`]; the app never touches the filesystem.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::process::Stdio;
use std::time::{Duration, Instant};

use ec_fog_config::{Action, Chord, Config, CustomAction, Key as K, Mods, Target};
use ec_fog_proto::{
    ConflictPolicy, JobId, JobSpec, JobStatus, Kind, Place, PlaceKind, Reply, Request, Resolution,
    SortKey, TrashItem,
};
use ec_fog_widgets::scroll_into_view;
use iced::keyboard::{self, key::Named, Key, Modifiers};
use iced::widget::operation::{scroll_to, AbsoluteOffset};
use iced::widget::Id;
use iced::window::Screenshot;
use iced::{window, Subscription, Task};
use jiff::tz::TimeZone;

use crate::bench::Bench;
use crate::clip::{Clip, ClipOp};
use crate::conn::{self, Link};
use crate::edit::{Edit, PathEdit};
use crate::motion::{self, Motion};
use crate::ops::{Confirm, Conflict, JobKind, NameEntry, Tray, TrayEvent};
use crate::palette::{Item, Palette};
use crate::state::{join, Browser, Click, Effect, Notice, Tabs};
use crate::theme::{self, size};

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
    /// What Copy or Cut took. A paste with this empty reads the Wayland
    /// clipboard instead.
    pub clip: Option<Clip>,
    /// The paths of the last cut pasted. The Wayland clipboard still lists
    /// them after the move, and a paste of that is refused, not a copy of
    /// sources that are gone.
    pasted_cut: Option<Vec<Vec<u8>>>,
    pub tray: Tray,
    /// Jobs waiting on a conflict answer, oldest first; the first is shown.
    pub conflicts: Vec<Conflict>,
    /// `ListTrash`, by id (the item's path under `Trash/files`), for the
    /// trash view's original locations and deletion dates.
    pub trash: HashMap<Vec<u8>, TrashItem>,
    /// Keys replayed one per [`SCRIPT_TICK`], for screenshots and probing
    /// on hosts without input injection. Filled from `FOG_UI_SCRIPT` in
    /// debug builds only; always empty in release.
    script: VecDeque<Scripted>,
    /// Springs and the sheet's snapshot (FOG §Visual design).
    pub motion: Motion,
    /// `FOG_UI_BENCH_SCROLL`: a timed scroll for `ec-fog-bench frames`.
    bench: Option<Bench>,
    /// Rows in hand, dragged toward a folder or a place.
    pub drag: Option<Drag>,
    /// Escape dropped the drag in hand: the pointer's travel is ignored
    /// until the button comes up (D-05 §2).
    drag_escaped: bool,
    /// The sidebar place under the pointer.
    pub place_hover: Option<usize>,
}

/// A drag of rows: what they are, and what the ghost says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drag {
    pub paths: Vec<Vec<u8>>,
    /// The first row's name, and whether it is a folder.
    pub name: String,
    pub dir: bool,
}

/// Where a drag would land if released now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropAt {
    Place(usize),
    Row(usize),
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
    /// The job tray.
    Jobs,
}

/// What floats over the list and takes the keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    None,
    Path(PathEdit),
    Palette(Palette),
    /// Renaming a row, or naming a new folder or file.
    Name(NameEntry),
    /// The permanent-delete confirmation.
    Confirm(Confirm),
}

/// A tray button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobCtl {
    /// Pause if running, resume if paused.
    Pause,
    Cancel,
    Dismiss,
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
    /// The Wayland clipboard, read for a file paste.
    ClipRead(Option<String>),
    /// Clears finished jobs and refreshes ETAs while the tray is shown.
    Tick(Instant),
    Job(JobId, JobCtl),
    /// A conflict dialog button.
    Resolve(Resolution),
    ApplyAll,
    /// The delete confirmation's buttons: `true` deletes.
    Confirm(bool),
    /// A clickable hint that runs an action (the trash bar).
    Act(Action),
    /// A frame is being drawn: step the springs.
    Frame(Instant),
    /// The window, snapshotted for the sheet that just opened.
    Backdrop(Screenshot),
    /// The pointer entered a row, or left row `usize`.
    Hover(usize),
    Unhover(usize),
    /// Row `usize` is being dragged: the first of these starts the drag.
    Lift(usize),
    /// The drag's button came up.
    Release,
    /// The button went down again during a drag: its release was lost.
    DragLost,
    /// The pointer entered a place, or left place `usize`.
    PlaceHover(usize),
    PlaceUnhover(usize),
}

const SCRIPT_TICK: Duration = Duration::from_millis(900);
/// Tray refresh while jobs are shown.
const TRAY_TICK: Duration = Duration::from_millis(500);

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
        "Left" => (Key::Named(Named::ArrowLeft), None),
        "Right" => (Key::Named(Named::ArrowRight), None),
        "Delete" => (Key::Named(Named::Delete), None),
        "F2" => (Key::Named(Named::F2), None),
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
            clip: None,
            pasted_cut: None,
            tray: Tray::default(),
            conflicts: Vec::new(),
            trash: HashMap::new(),
            motion: Motion::new(true),
            bench: Bench::from_env(),
            drag: None,
            drag_escaped: false,
            place_hover: None,
        }
    }

    /// Where the drag in hand would land now: the place under the pointer,
    /// else the folder row under it. Never one of the dragged items, nor a
    /// folder inside one of them, nor the folder they are already in.
    pub fn drop_at(&self) -> Option<DropAt> {
        let d = self.drag.as_ref()?;
        let ok = |dest: &[u8]| {
            dest != self.tabs.active().path.as_slice()
                && !d.paths.iter().any(|p| {
                    dest == p.as_slice()
                        || (dest.starts_with(p) && dest.get(p.len()) == Some(&b'/'))
                })
        };
        if let Some(i) = self.place_hover {
            let p = self.places.get(i)?;
            return (!matches!(p.kind, PlaceKind::Trash | PlaceKind::Recent) && ok(&p.path))
                .then_some(DropAt::Place(i));
        }
        let i = self.motion.hovered?;
        let b = self.tabs.active();
        let e = b.row(i).filter(|e| e.kind == Kind::Dir)?;
        ok(&join(&b.path, &e.name)).then_some(DropAt::Row(i))
    }

    /// The folder a [`DropAt`] names.
    fn drop_dest(&self, at: &DropAt) -> Option<Vec<u8>> {
        match at {
            DropAt::Place(i) => self.places.get(*i).map(|p| p.path.clone()),
            DropAt::Row(i) => {
                let b = self.tabs.active();
                b.row(*i).map(|e| join(&b.path, &e.name))
            }
        }
    }

    /// Row `i` is dragged: the first call picks up the selection if the row
    /// is in it, else the row alone, selecting it.
    fn lift(&mut self, i: usize) {
        if self.drag.is_some() || self.drag_escaped {
            return;
        }
        let b = self.tabs.active_mut();
        if !b.is_marked(i) {
            b.click(i, Click::Only);
        }
        let paths = b.targets();
        let Some(first) = b.row(i) else {
            return;
        };
        let mut name = first.display().into_owned();
        if paths.len() > 1 {
            name.push_str(&format!(" +{}", paths.len() - 1));
        }
        self.overlay = Overlay::None;
        self.focus = Focus::List;
        self.drag = Some(Drag {
            paths,
            name,
            dir: first.kind == Kind::Dir,
        });
    }

    /// The button came up: a drag over a target becomes a job there, a
    /// move, or a copy with Ctrl held (D-05 §2; FOG §File operations: every
    /// mutation is a job).
    fn release(&mut self) {
        self.drag_escaped = false;
        let at = self.drop_at();
        let Some(d) = self.drag.take() else {
            return;
        };
        let Some(dest) = at.and_then(|a| self.drop_dest(&a)) else {
            return;
        };
        let op = if self.mods.control() {
            ClipOp::Copy
        } else {
            ClipOp::Cut
        };
        let clip = Clip { op, paths: d.paths };
        if let Some(job) = clip.job(dest) {
            self.submit(job);
        }
    }

    /// The Trash place's folder (`…/Trash/files`), once places are known.
    pub fn trash_dir(&self) -> Option<&[u8]> {
        self.places
            .iter()
            .find(|p| p.kind == PlaceKind::Trash)
            .map(|p| p.path.as_slice())
    }

    /// Whether the active tab shows the trash.
    pub fn in_trash(&self) -> bool {
        self.trash_dir() == Some(self.tabs.active().path.as_slice())
    }

    /// fog.kdl changed under a running window (`ec-fogd` pushed the text it
    /// accepted): take its bindings, custom actions and `appearance` at
    /// once. Parsing is CPU only; the file was read by `ec-fogd`.
    pub fn reconfigure(&mut self, text: &str) {
        let c = match ec_fog_config::parse(text) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("ec-fog-ui: reloaded {e}; keeping the current config");
                return;
            }
        };
        self.keys = c.keys;
        self.actions = c.actions;
        if theme::set_appearance(c.appearance) {
            self.motion.restyled();
        }
    }

    /// Hand `spec` to the tray, which sends it when nothing is in flight.
    fn submit(&mut self, spec: JobSpec) {
        if let Some(req) = self.tray.submit(spec) {
            self.request(req);
        }
    }

    fn notify(&mut self, n: Notice) {
        self.tabs.active_mut().notice = Some(n);
    }

    /// Carry out what the tray made of a reply.
    fn on_tray(&mut self, ev: TrayEvent) {
        match ev {
            TrayEvent::None => {}
            TrayEvent::Send(req) => self.request(req),
            TrayEvent::Conflict { id, src, dest } => {
                self.conflicts.retain(|c| c.id != id);
                let c = Conflict::new(id, src, dest);
                for req in c.stats() {
                    self.request(req);
                }
                self.conflicts.push(c);
            }
            TrayEvent::Ended(job) => {
                self.conflicts.retain(|c| c.id != job.id);
                // An undone paste of a cut put the files back: that cut
                // can be pasted again.
                if job.kind == JobKind::Undo
                    && job.undoes == Some(JobKind::Move)
                    && job.status == JobStatus::Done
                    && self
                        .pasted_cut
                        .as_ref()
                        .is_some_and(|p| p.first() == Some(&job.subject))
                {
                    self.pasted_cut = None;
                }
                let n = match (&job.status, job.kind) {
                    (_, JobKind::Other | JobKind::Undo) => None,
                    (JobStatus::Failed { msg, .. }, k) => Some(Notice::Failed(k, msg.clone())),
                    (_, k) if job.skipped_all() => Some(Notice::Skipped(k)),
                    (
                        JobStatus::Done,
                        k @ (JobKind::Copy
                        | JobKind::Move
                        | JobKind::Trash
                        | JobKind::Delete
                        | JobKind::Restore),
                    ) => Some(Notice::Done(k, job.count)),
                    _ => None,
                };
                if let Some(n) = n {
                    self.notify(n);
                }
                if self.in_trash() {
                    self.request(Request::ListTrash);
                }
            }
            TrayEvent::Undone { ok, reason } => {
                self.notify(if ok {
                    Notice::Undone
                } else {
                    Notice::UndoRefused(reason.unwrap_or_else(|| "refused".into()))
                });
            }
        }
    }

    /// Answer the shown conflict.
    fn resolve(&mut self, choice: Resolution) {
        let Some(req) = self.conflicts.first().and_then(|c| c.resolve(choice)) else {
            return;
        };
        self.conflicts.remove(0);
        self.request(req);
    }

    fn job_ctl(&mut self, id: JobId, ctl: JobCtl) {
        let req = match ctl {
            JobCtl::Pause => self.tray.toggle_pause(id),
            JobCtl::Cancel => self.tray.cancel(id),
            JobCtl::Dismiss => {
                self.tray.dismiss(id);
                None
            }
        };
        if let Some(req) = req {
            self.request(req);
        }
        if self.tray.jobs.is_empty() && self.focus == Focus::Jobs {
            self.focus = Focus::List;
        }
    }

    /// Ask to delete the picked rows permanently. In the trash they are
    /// named by where they came from.
    fn confirm_delete(&mut self) {
        let paths = self.tabs.active().targets();
        if paths.is_empty() {
            return;
        }
        let names = paths
            .iter()
            .map(|p| {
                let orig = self.trash.get(p).map(|t| t.original_path.as_slice());
                crate::view::basename(orig.unwrap_or(p))
            })
            .collect();
        self.overlay = Overlay::Confirm(Confirm::new(paths, names));
    }

    /// Paste `clip` into the active folder. A cut is pasted once; a cut
    /// into the folder it is already in is no paste at all.
    fn paste(&mut self, clip: Clip) {
        let dest = self.tabs.active().path.clone();
        let Some(job) = clip.job(dest) else {
            self.notify(Notice::Hint("already in this folder"));
            return;
        };
        self.submit(job);
        if clip.op == ClipOp::Cut {
            self.clip = None;
            self.pasted_cut = Some(clip.paths);
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
                    if Some(path.as_slice()) == self.trash_dir() {
                        self.request(Request::ListTrash);
                    }
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

/// The sidebar's sections, top to bottom: a title and the kinds of place
/// in it. [`App::places`] is kept sorted by section (stable within one),
/// so drawing and the places cursor walk the same sequence.
pub const SECTIONS: [(&str, &[PlaceKind]); 4] = [
    (
        "places",
        &[PlaceKind::Home, PlaceKind::UserDir, PlaceKind::Recent],
    ),
    ("bookmarks", &[PlaceKind::Bookmark]),
    ("devices", &[PlaceKind::Mount]),
    ("", &[PlaceKind::Trash]),
];

fn section_of(kind: PlaceKind) -> usize {
    SECTIONS
        .iter()
        .position(|(_, kinds)| kinds.contains(&kind))
        .unwrap_or(SECTIONS.len())
}

fn rescroll(i: usize) -> Task<Message> {
    scroll_to(list_id(), AbsoluteOffset { x: 0.0, y: 0.0 }).chain(scroll_into_view(
        list_id(),
        i,
        size::ROW_H,
    ))
}

pub fn update(app: &mut App, message: Message) -> Task<Message> {
    let task = match message {
        Message::Frame(now) => return frame(app, now),
        Message::Backdrop(shot) => {
            app.motion.backdrop(shot);
            return Task::none();
        }
        Message::Hover(i) => {
            app.motion.hover(Some(i));
            return Task::none();
        }
        Message::Unhover(i) => {
            if app.motion.hovered == Some(i) {
                app.motion.hover(None);
            }
            return Task::none();
        }
        Message::PlaceHover(i) => {
            app.place_hover = Some(i);
            return Task::none();
        }
        Message::PlaceUnhover(i) => {
            if app.place_hover == Some(i) {
                app.place_hover = None;
            }
            return Task::none();
        }
        Message::Lift(i) => {
            app.lift(i);
            Task::none()
        }
        Message::Release => {
            app.release();
            Task::none()
        }
        Message::DragLost => {
            app.drag = None;
            app.drag_escaped = false;
            Task::none()
        }
        Message::Resized => {
            app.motion.resized();
            handle(app, Message::Resized)
        }
        m => handle(app, m),
    };
    let motion = motion::sync(app);
    Task::batch([task, motion])
}

/// Step the springs, and the benchmark's scroll.
fn frame(app: &mut App, now: Instant) -> Task<Message> {
    app.motion.frame(now);
    let rows = app.tabs.active().len();
    let Some(b) = &mut app.bench else {
        return Task::none();
    };
    if rows == 0 {
        return Task::none();
    }
    if b.sheet && matches!(app.overlay, Overlay::None) {
        app.overlay = Overlay::Palette(Palette::default());
        let _ = motion::sync(app);
        return Task::none();
    }
    let Some(b) = &mut app.bench else {
        return Task::none();
    };
    match b.frame(now, rows, size::ROW_H) {
        Some(y) => scroll_to(list_id(), AbsoluteOffset { x: 0.0, y }),
        None => {
            println!("{}", b.report());
            iced::exit()
        }
    }
}

fn handle(app: &mut App, message: Message) -> Task<Message> {
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
            // A new ec-fogd numbers jobs afresh; these can no longer be asked.
            app.tray.reset();
            app.conflicts.clear();
            if app.focus == Focus::Jobs {
                app.focus = Focus::List;
            }
            Task::none()
        }
        Message::Conn(conn::Event::Reply(Reply::PlacesList(mut places))) => {
            // Kept in drawn order, so the cursor walks what is on screen.
            places.sort_by_key(|p| section_of(p.kind));
            app.place_sel = app.place_sel.min(places.len().saturating_sub(1));
            app.places = places;
            if app.in_trash() {
                app.request(Request::ListTrash);
            }
            Task::none()
        }
        Message::Conn(conn::Event::Reply(Reply::ConfigReloaded { text })) => {
            app.reconfigure(&text);
            Task::none()
        }
        Message::Conn(conn::Event::Reply(Reply::TrashList(items))) => {
            app.trash = items.into_iter().map(|t| (t.id.clone(), t)).collect();
            Task::none()
        }
        Message::Conn(conn::Event::Reply(r)) => {
            let ev = app.tray.on_reply(&r, Instant::now());
            app.on_tray(ev);
            if let Reply::Stat(st) = &r {
                for c in &mut app.conflicts {
                    c.on_stat(st);
                }
            }
            // The trash folder changed: its items' origins may have too.
            let trash_diff = matches!(&r, Reply::DirDiff { dir, .. }
                if app.in_trash() && app.tabs.active().dir == Some(*dir));
            if let Overlay::Path(p) = &mut app.overlay {
                p.on_reply(&r);
            }
            let fxs = app.tabs.on_reply(r);
            if trash_diff {
                app.request(Request::ListTrash);
            }
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
                Overlay::Name(n) => n.edit(Edit::Insert(line)),
                Overlay::None | Overlay::Confirm(_) => {}
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
        Message::ClipRead(text) => {
            match text.as_deref().and_then(Clip::parse) {
                Some(clip) if app.pasted_cut.as_ref() == Some(&clip.paths) => {
                    app.notify(Notice::Hint("that cut was already pasted"));
                }
                Some(clip) => app.paste(clip),
                None => app.notify(Notice::NothingToPaste),
            }
            Task::none()
        }
        Message::Tick(now) => {
            app.tray.tick(now);
            if app.tray.jobs.is_empty() && app.focus == Focus::Jobs {
                app.focus = Focus::List;
            }
            Task::none()
        }
        Message::Job(id, ctl) => {
            app.job_ctl(id, ctl);
            Task::none()
        }
        Message::Resolve(choice) => {
            app.resolve(choice);
            Task::none()
        }
        Message::ApplyAll => {
            if let Some(c) = app.conflicts.first_mut() {
                c.apply_all = !c.apply_all;
            }
            Task::none()
        }
        Message::Confirm(delete) => {
            if let Overlay::Confirm(c) = std::mem::replace(&mut app.overlay, Overlay::None) {
                if delete {
                    app.submit(c.job());
                }
            }
            Task::none()
        }
        Message::Act(a) => {
            app.overlay = Overlay::None;
            run_action(app, a)
        }
        // Handled in `update`.
        Message::Frame(_)
        | Message::Backdrop(_)
        | Message::Hover(_)
        | Message::Unhover(_)
        | Message::PlaceHover(_)
        | Message::PlaceUnhover(_)
        | Message::Lift(_)
        | Message::Release
        | Message::DragLost => Task::none(),
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
    // Escape drops a drag in hand, before anything else sees the key.
    if esc && app.drag.take().is_some() {
        app.drag_escaped = true;
        return Task::none();
    }
    let enter = matches!(key, Key::Named(Named::Enter));
    let tab = matches!(key, Key::Named(Named::Tab));
    let sideways = match key.as_ref() {
        Key::Named(Named::ArrowLeft) => Some(-1),
        Key::Named(Named::ArrowRight) => Some(1),
        Key::Named(Named::Tab) => Some(if m.shift() { -1 } else { 1 }),
        _ => step_of(key, m),
    };

    // A job waiting on a conflict comes before everything else.
    if let Some(c) = app.conflicts.first_mut() {
        let answer = if esc {
            c.resolve(Resolution::Skip)
        } else if enter {
            c.resolve(c.pick)
        } else if let Some(d) = sideways {
            c.step(d);
            None
        } else {
            typed(text, m).and_then(|t| c.key(t))
        };
        if let Some(req) = answer {
            app.conflicts.remove(0);
            app.request(req);
        }
        return Task::none();
    }

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
        Overlay::Name(n) => {
            if esc {
                app.overlay = Overlay::None;
            } else if enter {
                match n.submit() {
                    Ok(None) => app.overlay = Overlay::None,
                    Ok(Some((spec, name))) => {
                        app.overlay = Overlay::None;
                        app.tabs.active_mut().want = Some(name);
                        app.submit(spec);
                    }
                    // Refused: the entry stays open and says why.
                    Err(_) => {}
                }
            } else if is_paste(key, m) {
                return iced::clipboard::read().map(Message::Paste);
            } else if let Some(e) = edit_of(key, text, m) {
                n.edit(e);
            }
            return Task::none();
        }
        Overlay::Confirm(c) => {
            if esc {
                app.overlay = Overlay::None;
            } else if enter {
                let spec = c.enter();
                app.overlay = Overlay::None;
                if let Some(spec) = spec {
                    app.submit(spec);
                }
            } else if sideways.is_some() {
                c.toggle();
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

    if app.focus == Focus::Jobs {
        if esc {
            app.focus = Focus::List;
            return Task::none();
        }
        // The tray's own letters come first: they are shown next to it.
        let ctl = match typed(text, m) {
            Some("p" | " ") => Some(JobCtl::Pause),
            Some("c") => Some(JobCtl::Cancel),
            Some("x") => Some(JobCtl::Dismiss),
            _ => None,
        };
        if let (Some(ctl), Some(id)) = (ctl, app.tray.at_cursor()) {
            app.job_ctl(id, ctl);
            return Task::none();
        }
        let n = app.tray.jobs.len() as isize;
        match bound {
            Some(Target::Action(Action::Down)) => app.tray.step(1),
            Some(Target::Action(Action::Up)) => app.tray.step(-1),
            Some(Target::Action(Action::Top)) => app.tray.step(-n),
            Some(Target::Action(Action::Bottom)) => app.tray.step(n),
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

/// Run a built-in action on the active tab.
pub fn run_action(app: &mut App, a: Action) -> Task<Message> {
    if let Some(t) = file_action(app, a) {
        return t;
    }
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
        Action::QuickLook | Action::SplitToggle => {
            b.notice = Some(Notice::Unavailable(a.name()));
            Effect::None
        }
        // Handled by `file_action`.
        Action::Copy
        | Action::Cut
        | Action::Paste
        | Action::Rename
        | Action::Trash
        | Action::Delete
        | Action::Undo
        | Action::NewFolder
        | Action::NewFile
        | Action::Restore
        | Action::FocusJobs => Effect::None,
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

/// The file operations (FOG §File operations): each one ends as a job for
/// `ec-fogd`, a prompt that leads to one, or a clipboard write. `None` for
/// every other action.
fn file_action(app: &mut App, a: Action) -> Option<Task<Message>> {
    let trash = app.in_trash();
    let b = app.tabs.active_mut();
    match a {
        Action::Copy | Action::Cut => {
            let paths = b.targets();
            if paths.is_empty() {
                return Some(Task::none());
            }
            let op = if a == Action::Copy {
                ClipOp::Copy
            } else {
                ClipOp::Cut
            };
            b.notice = Some(Notice::Clipboard(op, paths.len()));
            let clip = Clip { op, paths };
            let text = clip.uri_list();
            app.clip = Some(clip);
            app.pasted_cut = None;
            return Some(iced::clipboard::write(text));
        }
        Action::Paste if trash => b.notice = Some(Notice::Hint("paste does not go into the trash")),
        Action::Paste => match app.clip.clone() {
            Some(clip) => app.paste(clip),
            None => return Some(iced::clipboard::read().map(Message::ClipRead)),
        },
        Action::Rename | Action::NewFolder | Action::NewFile if trash => {
            b.notice = Some(Notice::Hint(
                "the trash is changed by restore and delete only",
            ));
        }
        Action::Rename => {
            // One name is edited: the first picked row's.
            let first = b.targets().into_iter().next();
            if let Some(n) = first.and_then(NameEntry::rename) {
                app.overlay = Overlay::Name(n);
            }
        }
        Action::NewFolder | Action::NewFile => {
            let dir = b.path.clone();
            app.overlay = Overlay::Name(NameEntry::create(dir, a == Action::NewFolder));
        }
        // Trash is undoable, so it asks nothing. In the trash itself there
        // is nowhere further to go: it becomes the confirmed delete.
        Action::Trash if !trash => {
            let paths = b.targets();
            if !paths.is_empty() {
                app.submit(JobSpec::Trash {
                    paths,
                    on_conflict: ConflictPolicy::Fail,
                });
            }
        }
        Action::Trash | Action::Delete => app.confirm_delete(),
        Action::Restore if trash => {
            let trash_ids = b.targets();
            if !trash_ids.is_empty() {
                app.submit(JobSpec::Restore {
                    trash_ids,
                    on_conflict: ConflictPolicy::Ask,
                });
            }
        }
        Action::Restore => b.notice = Some(Notice::Hint("restore works in the trash")),
        Action::Undo => {
            let req = app.tray.undo();
            app.request(req);
        }
        Action::FocusJobs => {
            app.focus = match app.focus {
                Focus::Jobs => Focus::List,
                _ if !app.tray.jobs.is_empty() => Focus::Jobs,
                f => f,
            };
        }
        _ => return None,
    }
    Some(Task::none())
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
    let mut subs = vec![
        keyboard::listen().map(Message::Key),
        conn::subscription().map(Message::Conn),
        window::resize_events().map(|_| Message::Resized),
    ];
    if !app.tray.jobs.is_empty() {
        subs.push(iced::time::every(TRAY_TICK).map(Message::Tick));
    }
    if !app.script.is_empty() {
        subs.push(iced::time::every(SCRIPT_TICK).map(|_| Message::Script));
    }
    if app.drag.is_some() || app.drag_escaped {
        // The button may come up outside the row that took the press. A
        // new press means its release was lost: the drag is dropped, not
        // landed.
        subs.push(iced::event::listen_with(|e, _, _| match e {
            iced::Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left)) => {
                Some(Message::Release)
            }
            iced::Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left)) => {
                Some(Message::DragLost)
            }
            _ => None,
        }));
    }
    if app.motion.busy() || app.bench.is_some() {
        subs.push(window::frames().map(Message::Frame));
    }
    Subscription::batch(subs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_fog_proto::{Entry, Kind};

    fn app() -> App {
        App::new(
            b"/w".to_vec(),
            ec_fog_config::defaults(),
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
        let keys = ec_fog_config::defaults().keys;
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
                    kind: ec_fog_proto::PlaceKind::Home,
                    label: "Home".into(),
                    path: b"/home/u".to_vec(),
                },
                Place {
                    kind: ec_fog_proto::PlaceKind::Trash,
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

    fn reply(app: &mut App, r: Reply) {
        let _ = update(app, Message::Conn(conn::Event::Reply(r)));
    }

    #[test]
    fn a_reloaded_config_rebinds_keys_live() {
        let mut a = app();
        listed(&mut a, &["a", "b", "c"]);
        press(&mut a, "ctrl+y");
        assert_eq!(a.tabs.active().selected, 0, "ctrl+y is unbound by default");
        // What ec-fogd pushes after a valid edit of fog.kdl.
        reply(
            &mut a,
            Reply::ConfigReloaded {
                text: "keys {\n    bind \"ctrl+y\" \"down\"\n}\n".into(),
            },
        );
        press(&mut a, "ctrl+y");
        assert_eq!(a.tabs.active().selected, 1);
        // Text that does not parse changes nothing.
        reply(
            &mut a,
            Reply::ConfigReloaded {
                text: "bogus\n".into(),
            },
        );
        press(&mut a, "ctrl+y");
        assert_eq!(a.tabs.active().selected, 2);
    }

    fn kinds(app: &App) -> Vec<JobKind> {
        app.tray.jobs.iter().map(|j| j.kind).collect()
    }

    #[test]
    fn trash_goes_at_once_and_delete_only_after_a_confirm() {
        let mut a = app();
        listed(&mut a, &["a", "b"]);
        press(&mut a, "Delete");
        reply(&mut a, Reply::JobAccepted { id: 1 });
        assert_eq!(kinds(&a), [JobKind::Trash]);
        // Shift+Delete asks; Enter on the default is cancel.
        press(&mut a, "shift+Delete");
        assert!(matches!(&a.overlay, Overlay::Confirm(c) if !c.delete));
        press(&mut a, "Enter");
        assert!(matches!(a.overlay, Overlay::None));
        // Esc cancels too; only moving to "delete" and Enter deletes.
        press(&mut a, "shift+Delete");
        press(&mut a, "Esc");
        press(&mut a, "shift+Delete");
        press(&mut a, "Right");
        press(&mut a, "Enter");
        reply(&mut a, Reply::JobAccepted { id: 2 });
        assert_eq!(kinds(&a), [JobKind::Trash, JobKind::Delete]);
    }

    #[test]
    fn f2_renames_inline_and_refuses_a_slash() {
        let mut a = app();
        listed(&mut a, &["a", "b"]);
        press(&mut a, "F2");
        assert!(matches!(&a.overlay, Overlay::Name(n) if n.line.text() == "a"));
        press(&mut a, "/");
        press(&mut a, "Enter");
        assert!(matches!(&a.overlay, Overlay::Name(n) if n.error.is_some()));
        press(&mut a, "Backspace");
        press(&mut a, "x");
        press(&mut a, "Enter");
        assert!(matches!(a.overlay, Overlay::None));
        assert_eq!(a.tabs.active().want.as_deref(), Some(&b"ax"[..]));
        reply(&mut a, Reply::JobAccepted { id: 1 });
        assert_eq!(kinds(&a), [JobKind::Rename]);
        // Esc leaves without a job.
        press(&mut a, "ctrl+shift+n");
        press(&mut a, "d");
        press(&mut a, "Esc");
        assert!(matches!(a.overlay, Overlay::None));
    }

    /// `/w` holding folders `d`, `dd` and a file `f`.
    fn listed_with_dirs(app: &mut App) {
        let e = |n: &str, k| Entry::new(n.as_bytes().to_vec(), k);
        let _ = update(
            app,
            Message::Conn(conn::Event::Reply(Reply::DirSnapshot {
                path: b"/w".to_vec(),
                dir: 1,
                generation: 0,
                entries: vec![e("d", Kind::Dir), e("dd", Kind::Dir), e("f", Kind::File)],
                order: vec![0, 1, 2],
                complete: true,
            })),
        );
    }

    #[test]
    fn a_row_dropped_on_a_folder_moves_and_with_ctrl_copies() {
        let mut a = app();
        listed_with_dirs(&mut a);
        // Drag `f` over `d`: a move, the default.
        let _ = update(&mut a, Message::Lift(2));
        assert_eq!(a.drag.as_ref().unwrap().paths, [b"/w/f".to_vec()]);
        let _ = update(&mut a, Message::Hover(0));
        assert_eq!(a.drop_at(), Some(DropAt::Row(0)));
        let _ = update(&mut a, Message::Release);
        reply(&mut a, Reply::JobAccepted { id: 1 });
        assert_eq!(kinds(&a), [JobKind::Move]);
        assert!(a.drag.is_none());
        // With Ctrl held, onto a place: a copy.
        a.places = vec![Place {
            kind: PlaceKind::UserDir,
            label: "Docs".into(),
            path: b"/docs".to_vec(),
        }];
        a.mods = Modifiers::CTRL;
        let _ = update(&mut a, Message::Lift(2));
        let _ = update(&mut a, Message::PlaceHover(0));
        assert_eq!(a.drop_at(), Some(DropAt::Place(0)));
        let _ = update(&mut a, Message::Release);
        reply(&mut a, Reply::JobAccepted { id: 2 });
        assert_eq!(kinds(&a), [JobKind::Move, JobKind::Copy]);
    }

    #[test]
    fn a_drag_never_lands_on_itself_a_file_or_its_own_folder() {
        let mut a = app();
        listed_with_dirs(&mut a);
        let _ = update(&mut a, Message::Lift(0));
        // Over itself, over a file: nowhere.
        let _ = update(&mut a, Message::Hover(0));
        assert_eq!(a.drop_at(), None);
        let _ = update(&mut a, Message::Hover(2));
        assert_eq!(a.drop_at(), None);
        // A place inside the dragged folder, or the folder it is in.
        a.places = vec![
            Place {
                kind: PlaceKind::Bookmark,
                label: "in".into(),
                path: b"/w/d/sub".to_vec(),
            },
            Place {
                kind: PlaceKind::Bookmark,
                label: "here".into(),
                path: b"/w".to_vec(),
            },
        ];
        for i in 0..2 {
            let _ = update(&mut a, Message::PlaceHover(i));
            assert_eq!(a.drop_at(), None, "place {i}");
            let _ = update(&mut a, Message::PlaceUnhover(i));
        }
        // A sibling whose name only starts the same is fine.
        let _ = update(&mut a, Message::Hover(1));
        assert_eq!(a.drop_at(), Some(DropAt::Row(1)));
        let _ = update(&mut a, Message::Hover(2));
        let _ = update(&mut a, Message::Release);
        assert!(kinds(&a).is_empty(), "no job was sent");
    }

    #[test]
    fn escape_drops_the_drag_until_the_button_comes_up() {
        let mut a = app();
        listed_with_dirs(&mut a);
        let _ = update(&mut a, Message::Lift(2));
        let _ = update(&mut a, Message::Hover(0));
        press(&mut a, "Esc");
        assert!(a.drag.is_none());
        // Travel with the button still down does not pick it up again.
        let _ = update(&mut a, Message::Lift(2));
        assert!(a.drag.is_none());
        let _ = update(&mut a, Message::Release);
        assert!(kinds(&a).is_empty(), "no job was sent");
        // The next press can drag again.
        let _ = update(&mut a, Message::Lift(2));
        assert!(a.drag.is_some());
    }

    #[test]
    fn a_lost_release_drops_the_drag_instead_of_landing_it() {
        let mut a = app();
        listed_with_dirs(&mut a);
        let _ = update(&mut a, Message::Lift(2));
        let _ = update(&mut a, Message::Hover(0));
        // The button went down again: the drag's release never came.
        let _ = update(&mut a, Message::DragLost);
        let _ = update(&mut a, Message::Release);
        assert!(kinds(&a).is_empty(), "no job was sent");
    }

    #[test]
    fn copy_then_paste_asks_fogd_to_copy_here() {
        let mut a = app();
        listed(&mut a, &["a", "b"]);
        press(&mut a, "ctrl+c");
        assert_eq!(
            a.clip,
            Some(Clip {
                op: ClipOp::Copy,
                paths: vec![b"/w/a".to_vec()]
            })
        );
        press(&mut a, "ctrl+v");
        reply(&mut a, Reply::JobAccepted { id: 1 });
        assert_eq!(kinds(&a), [JobKind::Copy]);
        // A cut is pasted once.
        press(&mut a, "ctrl+x");
        a.clip.as_mut().unwrap().paths = vec![b"/x/a".to_vec()];
        press(&mut a, "ctrl+v");
        assert_eq!(a.clip, None);
    }

    #[test]
    fn a_cut_into_its_own_folder_sends_no_job_and_stays_cut() {
        let mut a = app();
        listed(&mut a, &["a", "b"]);
        press(&mut a, "ctrl+x");
        press(&mut a, "ctrl+v");
        reply(&mut a, Reply::JobAccepted { id: 1 });
        assert!(kinds(&a).is_empty(), "no job was sent");
        assert_eq!(
            a.tabs.active().notice,
            Some(Notice::Hint("already in this folder"))
        );
        assert_eq!(
            a.clip,
            Some(Clip {
                op: ClipOp::Cut,
                paths: vec![b"/w/a".to_vec()]
            })
        );
    }

    #[test]
    fn a_pasted_cut_is_not_pasted_again_from_the_system_clipboard() {
        let mut a = app();
        listed(&mut a, &["a"]);
        press(&mut a, "ctrl+x");
        a.clip.as_mut().unwrap().paths = vec![b"/x/a".to_vec()];
        let text = a.clip.as_ref().unwrap().uri_list();
        press(&mut a, "ctrl+v");
        reply(&mut a, Reply::JobAccepted { id: 1 });
        assert_eq!(kinds(&a), [JobKind::Move]);
        // Fog's clip is spent; the system clipboard still lists /x/a.
        let _ = update(&mut a, Message::ClipRead(Some(text)));
        assert_eq!(
            a.tabs.active().notice,
            Some(Notice::Hint("that cut was already pasted"))
        );
        assert_eq!(kinds(&a), [JobKind::Move]);
        // Anything else on it still pastes.
        let _ = update(&mut a, Message::ClipRead(Some("/y/b\n".into())));
        reply(&mut a, Reply::JobAccepted { id: 2 });
        assert_eq!(kinds(&a), [JobKind::Move, JobKind::Copy]);
    }

    #[test]
    fn a_new_listing_leaves_no_hover_behind() {
        let mut a = app();
        listed_with_dirs(&mut a);
        let _ = update(&mut a, Message::Hover(2));
        // The folder shrank past the hovered row.
        listed(&mut a, &["a"]);
        assert_eq!(a.motion.hovered, None);
        listed_with_dirs(&mut a);
        let _ = update(&mut a, Message::Hover(0));
        let _ = update(&mut a, Message::OpenRow(0));
        reply(
            &mut a,
            Reply::DirSnapshot {
                path: b"/w/d".to_vec(),
                dir: 2,
                generation: 0,
                entries: vec![Entry::new(b"x".to_vec(), Kind::File)],
                order: vec![0],
                complete: true,
            },
        );
        assert_eq!(a.tabs.active().path, b"/w/d");
        assert_eq!(a.motion.hovered, None);
    }

    #[test]
    fn an_undone_cut_paste_can_be_pasted_again() {
        let mut a = app();
        listed(&mut a, &["a"]);
        press(&mut a, "ctrl+x");
        a.clip.as_mut().unwrap().paths = vec![b"/x/a".to_vec()];
        let text = a.clip.as_ref().unwrap().uri_list();
        press(&mut a, "ctrl+v");
        reply(&mut a, Reply::JobAccepted { id: 1 });
        reply(
            &mut a,
            Reply::JobState {
                id: 1,
                state: JobStatus::Done,
            },
        );
        press(&mut a, "ctrl+z");
        for state in [JobStatus::Running, JobStatus::Done] {
            reply(&mut a, Reply::JobState { id: 2, state });
        }
        // The files are back at /x: the cut pastes again.
        let _ = update(&mut a, Message::ClipRead(Some(text)));
        reply(&mut a, Reply::JobAccepted { id: 3 });
        assert_eq!(kinds(&a).len(), 3);
        assert_ne!(
            a.tabs.active().notice,
            Some(Notice::Hint("that cut was already pasted"))
        );
    }

    #[test]
    fn a_conflict_takes_the_keys_until_answered() {
        let mut a = app();
        listed(&mut a, &["a"]);
        press(&mut a, "ctrl+c");
        press(&mut a, "ctrl+v");
        reply(&mut a, Reply::JobAccepted { id: 7 });
        let conflict = |a: &mut App| {
            reply(
                a,
                Reply::JobState {
                    id: 7,
                    state: JobStatus::Conflict {
                        src: b"/w/a".to_vec(),
                        dest: b"/w/a".to_vec(),
                    },
                },
            )
        };
        conflict(&mut a);
        assert_eq!(a.conflicts.len(), 1);
        // `j` is not a list move while the dialog is up.
        press(&mut a, "j");
        press(&mut a, "a");
        assert!(a.conflicts[0].apply_all);
        press(&mut a, "k");
        assert!(a.conflicts.is_empty());
        conflict(&mut a);
        press(&mut a, "Esc");
        assert!(a.conflicts.is_empty());
    }

    #[test]
    fn places_are_walked_in_the_order_they_are_drawn() {
        let mut a = app();
        listed(&mut a, &["a"]);
        let place = |kind, label: &str| Place {
            kind,
            label: label.into(),
            path: format!("/{label}").into_bytes(),
        };
        // ec-fogd's order: bookmarks before trash before mounts.
        let _ = update(
            &mut a,
            Message::Conn(conn::Event::Reply(Reply::PlacesList(vec![
                place(PlaceKind::Home, "home"),
                place(PlaceKind::Bookmark, "mark"),
                place(PlaceKind::Trash, "trash"),
                place(PlaceKind::Mount, "usb"),
                place(PlaceKind::UserDir, "docs"),
            ]))),
        );
        let labels: Vec<&str> = a.places.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(labels, ["home", "docs", "mark", "usb", "trash"]);
        press(&mut a, "Tab");
        for _ in 0..3 {
            press(&mut a, "j");
        }
        press(&mut a, "Enter");
        assert_eq!(a.tabs.active().target(), b"/usb");
    }

    #[test]
    fn custom_action_args_expand_cwd() {
        assert_eq!(expand_cwd("--cwd={cwd}", b"/a b"), "--cwd=/a b");
        assert_eq!(expand_cwd("x", b"/a"), "x");
        assert_eq!(expand_cwd("{cwd}{cwd}", b"/a"), "/a/a");
    }
}
