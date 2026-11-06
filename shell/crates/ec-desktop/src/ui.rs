// SPDX-License-Identifier: AGPL-3.0-only
//! What the pointer and keyboard do to the desktop: selection, the rubber
//! band, drags, the context menu, inline rename, the launch prompt and the
//! failure note.
//!
//! Pure: no widgets and no disk. A [`Scene`] says where every icon is; an
//! event goes into [`Ui`]; what comes out is a list of [`Act`]s for
//! `app::update` to run through `Desktop::run` (or a spawn). The selection is
//! by file name, so it survives a rescan and is pruned by [`Ui::prune`].
//!
//! Nothing here is destructive. Delete is `Op::Trash`; a launcher without the
//! exec bit asks the human first (`prompt`) and is only allowed by a click on
//! the prompt's button or the menu's "Allow Launching".

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use ec_ui::tokens::{desktop, menu};
use iced::{Point, Rectangle, Size};

use crate::desktop::{Desktop, Done, Failure, Op};
use crate::grid;
use crate::launch::{self, Plan};
use crate::layout::{Cell, Grid, Sort};
use crate::scan::{Entry, Kind};

/// What the desktop asks `app::update` to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Act {
    Run(Op),
    /// A terminal in the desktop folder.
    Terminal,
    /// `ec-settings` on the Desktop pane.
    Settings,
}

/// One icon on one surface.
#[derive(Debug, Clone)]
pub struct Item<'a> {
    pub entry: &'a Entry,
    pub cell: Cell,
    pub rect: Rectangle,
}

/// Where everything is on one output's surface.
#[derive(Debug, Clone)]
pub struct Scene<'a> {
    pub output: &'a str,
    pub size: Size,
    pub grid: Grid,
    pub sort: Sort,
    /// The icons inside the grid, in directory order. One placed past the
    /// right edge is kept by the layout but is not here.
    pub items: Vec<Item<'a>>,
}

impl<'a> Scene<'a> {
    pub fn new(desk: &'a Desktop, output: &'a str, size: Size) -> Scene<'a> {
        let grid = grid::grid_for(size);
        let cells = desk.cells(output, grid);
        Scene::from_parts(
            output,
            size,
            &desk.entries,
            &cells,
            desk.layout.output(output).sort,
        )
    }

    pub fn from_parts(
        output: &'a str,
        size: Size,
        entries: &'a [Entry],
        cells: &[Cell],
        sort: Sort,
    ) -> Scene<'a> {
        let grid = grid::grid_for(size);
        let items = entries
            .iter()
            .zip(cells)
            .filter(|(_, c)| c.col < grid.cols && c.row < grid.rows)
            .map(|(entry, cell)| Item {
                entry,
                cell: *cell,
                rect: grid::cell_rect(*cell),
            })
            .collect();
        Scene {
            output,
            size,
            grid,
            sort,
            items,
        }
    }

    pub fn at(&self, p: Point) -> Option<&Item<'a>> {
        self.items.iter().find(|i| i.rect.contains(p))
    }

    pub fn find(&self, name: &str) -> Option<&Item<'a>> {
        self.items.iter().find(|i| i.entry.name == name)
    }

    fn entry(&self, name: &str) -> Option<&'a Entry> {
        self.find(name).map(|i| i.entry)
    }
}

/// A key the desktop acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Left,
    Right,
    Up,
    Down,
    Enter,
    Escape,
    Delete,
    F2,
}

/// A pointer event on a surface, in its logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pointer {
    Moved(Point),
    Exited,
    Pressed,
    Released,
    RightPressed,
}

/// What a context menu is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Empty,
    Icon(String),
}

/// One verb of a context menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Open,
    Rename,
    Copy,
    Trash,
    AllowLaunching,
    NewFolder,
    NewFile,
    Terminal,
    Sort(Sort),
    Settings,
}

impl Verb {
    pub fn label(self) -> &'static str {
        match self {
            Verb::Open => "Open",
            Verb::Rename => "Rename",
            Verb::Copy => "Copy",
            Verb::Trash => "Move to Trash",
            Verb::AllowLaunching => "Allow Launching",
            Verb::NewFolder => "New Folder",
            Verb::NewFile => "New File",
            Verb::Terminal => "Open Terminal Here",
            Verb::Sort(Sort::Name) => "Name",
            Verb::Sort(Sort::Kind) => "Kind",
            Verb::Sort(Sort::Modified) => "Modified",
            Verb::Settings => "Desktop Settings",
        }
    }
}

/// A line of the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Verb(Verb),
    Rule,
    Heading(&'static str),
}

/// The menu's rows. `entry` is the icon it was opened on; `many` is whether
/// the selection holds more than that one.
pub fn rows(target: &Target, entry: Option<&Entry>, many: bool) -> Vec<Row> {
    match target {
        Target::Empty => vec![
            Row::Verb(Verb::NewFolder),
            Row::Verb(Verb::NewFile),
            Row::Verb(Verb::Terminal),
            Row::Rule,
            Row::Heading("Sort by"),
            Row::Verb(Verb::Sort(Sort::Name)),
            Row::Verb(Verb::Sort(Sort::Kind)),
            Row::Verb(Verb::Sort(Sort::Modified)),
            Row::Rule,
            Row::Verb(Verb::Settings),
        ],
        Target::Icon(_) => {
            let mut v = vec![Row::Verb(Verb::Open)];
            if !many {
                v.push(Row::Verb(Verb::Rename));
            }
            v.push(Row::Verb(Verb::Copy));
            let locked = entry
                .is_some_and(|e| e.kind == Kind::Launcher && e.launcher.as_ref().is_some_and(|l| !l.trusted));
            if locked && !many {
                v.push(Row::Verb(Verb::AllowLaunching));
            }
            v.push(Row::Rule);
            v.push(Row::Verb(Verb::Trash));
            v
        }
    }
}

/// The menu's pixel height: the rows, the rules and the padding round them.
pub fn menu_height(rows: &[Row]) -> f32 {
    let body: f32 = rows
        .iter()
        .map(|r| match r {
            Row::Verb(_) => menu::ROW_H,
            Row::Rule => menu::SEP_H,
            Row::Heading(_) => desktop::MENU_HEADING_H,
        })
        .sum();
    body + 2.0 * menu::PAD
}

/// An open context menu.
#[derive(Debug, Clone, PartialEq)]
pub struct Menu {
    /// Where it opens, clamped to the surface by [`Menu::origin`].
    pub at: Point,
    pub target: Target,
}

impl Menu {
    /// The menu's top-left corner: at the pointer, pushed back inside the
    /// surface when it would run off the right or bottom edge.
    pub fn origin(&self, size: Size, rows: &[Row]) -> Point {
        let h = menu_height(rows);
        Point::new(
            self.at.x.min(size.width - menu::W).max(0.0),
            self.at.y.min(size.height - h).max(0.0),
        )
    }
}

/// An inline rename in progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rename {
    /// The entry's current file name.
    pub from: String,
    pub text: String,
}

impl Rename {
    pub fn valid(&self) -> bool {
        crate::valid_name(&self.text)
    }
}

/// A drag of icons.
#[derive(Debug, Clone, PartialEq)]
pub struct Drag {
    /// What moves with the pointer, and the icon the pointer holds.
    pub names: Vec<String>,
    pub primary: String,
    pub grab: Point,
    pub at: Point,
}

/// A rubber band, with the selection it started from.
#[derive(Debug, Clone, PartialEq)]
pub struct Band {
    pub from: Point,
    pub to: Point,
    base: BTreeSet<String>,
}

impl Band {
    pub fn rect(&self) -> Rectangle {
        grid::band_rect(self.from, self.to)
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Press {
    Icon { name: String, at: Point, ctrl: bool },
    Empty { at: Point },
}

/// The failure note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub text: String,
    pub seq: u64,
}

/// The words for a failed operation. No name or path is in it.
pub fn failure_text(f: Failure) -> String {
    let what = match f.op {
        "new-folder" => "create the folder",
        "new-file" => "create the file",
        "rename" => "rename that",
        "copy" => "copy that",
        "move" => "move that",
        "trash" => "move that to the trash",
        "activate" => "open that",
        "allow-launching" => "allow that launcher",
        "sort" => "sort the desktop",
        "place" => "move that icon",
        "terminal" => "open a terminal",
        "settings" => "open Settings",
        _ => "do that",
    };
    let why = match f.kind {
        "not-allowed" => "it is not allowed to launch yet",
        "not-launchable" => "it is not an application",
        "broken-link" => "the link points nowhere",
        "no-opener" => "nothing is installed to open it",
        "spawn" => "it would not start",
        "invalid-name" => "that name is not allowed",
        "missing" => "it is gone",
        "cross-device" => "it is on another disk",
        "inside-itself" => "a folder cannot go inside itself",
        "unsupported" => "that kind of file is not supported",
        "permission-denied" => "permission denied",
        "storage-full" => "the disk is full",
        "outside" => "it is not on the desktop",
        "no-desktop" => "there is no desktop folder",
        "no-trash" => "there is no trash",
        _ => "something went wrong",
    };
    format!("Could not {what}: {why}.")
}

#[derive(Debug, Default)]
pub struct Ui {
    pub selected: BTreeSet<String>,
    /// The keyboard's icon: where the arrows start, what F2 renames.
    pub cursor: Option<String>,
    pub hover: Option<String>,
    pub point: Option<Point>,
    pub ctrl: bool,
    pub menu: Option<Menu>,
    pub rename: Option<Rename>,
    /// A launcher (by file name) waiting on "Allow launching?".
    pub prompt: Option<String>,
    pub drag: Option<Drag>,
    pub band: Option<Band>,
    pub toast: Option<Toast>,
    press: Option<Press>,
    last_click: Option<(String, Instant)>,
    toasts: u64,
}

impl Ui {
    /// Does anything here need the keyboard? Then the surface asks for it.
    pub fn wants_keys(&self) -> bool {
        !self.selected.is_empty() || self.rename.is_some() || self.menu.is_some() || self.prompt.is_some()
    }

    /// The surface the pointer was acting on is gone: drop every half-done
    /// gesture so the next press elsewhere cannot replay it.
    pub fn abandon_gesture(&mut self) {
        self.drag = None;
        self.band = None;
        self.press = None;
        self.menu = None;
        self.rename = None;
    }

    fn select_only(&mut self, name: &str) {
        self.selected.clear();
        self.selected.insert(name.to_owned());
        self.cursor = Some(name.to_owned());
    }

    /// The icons a verb applies to: the selection, or the one it was opened on.
    fn subjects<'a>(&self, scene: &Scene<'a>) -> Vec<&'a Entry> {
        scene
            .items
            .iter()
            .filter(|i| self.selected.contains(&i.entry.name))
            .map(|i| i.entry)
            .collect()
    }

    /// Close the menu and the prompt (a press on the ground around them).
    pub fn dismiss_floating(&mut self) {
        self.close_floating();
    }

    /// Close whatever is floating over the icons.
    fn close_floating(&mut self) {
        self.menu = None;
        self.prompt = None;
    }

    pub fn begin_rename(&mut self, name: &str) {
        self.close_floating();
        self.rename = Some(Rename {
            from: name.to_owned(),
            text: name.to_owned(),
        });
    }

    pub fn rename_input(&mut self, text: String) {
        if let Some(r) = &mut self.rename {
            r.text = text;
        }
    }

    /// Enter in the rename field. A name the desktop would refuse keeps the
    /// field open; an unchanged one just closes it.
    pub fn rename_commit(&mut self, scene: &Scene) -> Vec<Act> {
        let Some(r) = self.rename.clone() else {
            return Vec::new();
        };
        if !r.valid() {
            return Vec::new();
        }
        self.rename = None;
        if r.text == r.from {
            return Vec::new();
        }
        match scene.entry(&r.from) {
            Some(e) => vec![Act::Run(Op::Rename {
                path: e.path.clone(),
                name: r.text,
            })],
            None => Vec::new(),
        }
    }

    pub fn rename_cancel(&mut self) {
        self.rename = None;
    }

    /// Activate `entries`: a launcher that is not allowed yet asks first.
    fn activate(&mut self, entries: &[&Entry]) -> Vec<Act> {
        let mut acts = Vec::new();
        for e in entries {
            if launch::plan(e) == Plan::Refuse(launch::Error::NotAllowed) {
                if self.prompt.is_none() {
                    self.prompt = Some(e.name.clone());
                }
                continue;
            }
            acts.push(Act::Run(Op::Activate { path: e.path.clone() }));
        }
        acts
    }

    /// The prompt's "Allow launching" button.
    pub fn allow(&mut self, scene: &Scene) -> Vec<Act> {
        let Some(name) = self.prompt.take() else {
            return Vec::new();
        };
        match scene.entry(&name) {
            Some(e) => vec![Act::Run(Op::AllowLaunching { path: e.path.clone() })],
            None => Vec::new(),
        }
    }

    pub fn dismiss_prompt(&mut self) {
        self.prompt = None;
    }

    // --- the pointer -------------------------------------------------------

    pub fn pointer(&mut self, scene: &Scene, event: Pointer, now: Instant) -> Vec<Act> {
        match event {
            Pointer::Moved(p) => {
                self.point = Some(p);
                self.hover = scene.at(p).map(|i| i.entry.name.clone());
                self.moved(scene, p);
                Vec::new()
            }
            Pointer::Exited => {
                self.hover = None;
                Vec::new()
            }
            Pointer::Pressed => match self.point {
                Some(p) => self.pressed(scene, p),
                None => Vec::new(),
            },
            Pointer::Released => self.released(scene, now),
            Pointer::RightPressed => match self.point {
                Some(p) => self.right_pressed(scene, p),
                None => Vec::new(),
            },
        }
    }

    fn moved(&mut self, scene: &Scene, p: Point) {
        if let Some(d) = &mut self.drag {
            d.at = p;
            return;
        }
        if let Some(b) = &mut self.band {
            b.to = p;
            let rects: Vec<Rectangle> = scene.items.iter().map(|i| i.rect).collect();
            let mut now = b.base.clone();
            for i in grid::band_hits(b.rect(), &rects) {
                now.insert(scene.items[i].entry.name.clone());
            }
            self.selected = now;
            return;
        }
        let far = |a: Point| (a.x - p.x).hypot(a.y - p.y) > desktop::DRAG_SLOP;
        match self.press.clone() {
            Some(Press::Icon { name, at, .. }) if far(at) => {
                // A press on an unselected icon with no Ctrl has already
                // selected it; a drag carries the whole selection.
                let names: Vec<String> = if self.selected.contains(&name) {
                    scene
                        .items
                        .iter()
                        .filter(|i| self.selected.contains(&i.entry.name))
                        .map(|i| i.entry.name.clone())
                        .collect()
                } else {
                    vec![name.clone()]
                };
                self.drag = Some(Drag {
                    names,
                    primary: name,
                    grab: at,
                    at: p,
                });
                self.last_click = None;
            }
            Some(Press::Empty { at }) if far(at) => {
                let base = if self.ctrl {
                    self.selected.clone()
                } else {
                    BTreeSet::new()
                };
                self.band = Some(Band {
                    from: at,
                    to: p,
                    base,
                });
                self.moved(scene, p);
            }
            _ => {}
        }
    }

    fn pressed(&mut self, scene: &Scene, p: Point) -> Vec<Act> {
        // A press away from a floating thing closes it; an open rename is
        // committed, as a file manager does.
        let acts = if self.rename.is_some() {
            self.rename_commit(scene)
        } else {
            Vec::new()
        };
        self.rename = None;
        self.close_floating();
        self.toast = None;
        match scene.at(p).map(|i| i.entry.name.clone()) {
            Some(name) => {
                if self.ctrl {
                    if !self.selected.remove(&name) {
                        self.selected.insert(name.clone());
                        self.cursor = Some(name.clone());
                    }
                } else if !self.selected.contains(&name) {
                    self.select_only(&name);
                } else {
                    self.cursor = Some(name.clone());
                }
                self.press = Some(Press::Icon {
                    name,
                    at: p,
                    ctrl: self.ctrl,
                });
            }
            None => {
                if !self.ctrl {
                    self.selected.clear();
                    self.cursor = None;
                }
                self.press = Some(Press::Empty { at: p });
            }
        }
        acts
    }

    fn released(&mut self, scene: &Scene, now: Instant) -> Vec<Act> {
        let press = self.press.take();
        if let Some(d) = self.drag.take() {
            return self.drop(scene, d);
        }
        if self.band.take().is_some() {
            return Vec::new();
        }
        let Some(Press::Icon { name, ctrl, .. }) = press else {
            return Vec::new();
        };
        if ctrl {
            self.last_click = None;
            return Vec::new();
        }
        // A plain click on one of several selected icons selects just it.
        if self.selected.len() > 1 {
            self.select_only(&name);
        }
        let double = matches!(
            &self.last_click,
            Some((n, t)) if *n == name
                && now.saturating_duration_since(*t) <= Duration::from_millis(desktop::DOUBLE_CLICK_MS)
        );
        if double {
            self.last_click = None;
            return match scene.entry(&name) {
                Some(e) => self.activate(&[e]),
                None => Vec::new(),
            };
        }
        self.last_click = Some((name, now));
        Vec::new()
    }

    fn drop(&mut self, scene: &Scene, d: Drag) -> Vec<Act> {
        // On a real folder: move into it.
        if let Some(target) = scene.at(d.at) {
            let folder = target.entry;
            if folder.kind == Kind::Dir && !d.names.contains(&folder.name) {
                return d
                    .names
                    .iter()
                    .filter_map(|n| scene.entry(n))
                    .map(|e| {
                        Act::Run(Op::Move {
                            src: e.path.clone(),
                            into: Some(folder.path.clone()),
                        })
                    })
                    .collect();
            }
        }
        let Some(held) = scene.find(&d.primary) else {
            return Vec::new();
        };
        let want = grid::cell_at(d.at, scene.grid);
        let (mut dc, mut dr) = (
            want.col as i64 - held.cell.col as i64,
            want.row as i64 - held.cell.row as i64,
        );
        let moving: Vec<&Item> = d.names.iter().filter_map(|n| scene.find(n)).collect();
        // Nothing may leave the top-left of the grid.
        if let Some(least) = moving.iter().map(|i| i.cell.col as i64).min() {
            dc = dc.max(-least);
        }
        if let Some(least) = moving.iter().map(|i| i.cell.row as i64).min() {
            dr = dr.max(-least);
        }
        if dc == 0 && dr == 0 {
            return Vec::new();
        }
        // Fill order is down a column then across, so go far end first: an
        // icon never lands on a cell another dragged icon has yet to leave.
        let key = |i: &Item| i.cell.col as i64 * scene.grid.rows as i64 + i.cell.row as i64;
        let mut order = moving;
        order.sort_by_key(|i| key(i));
        if dc * scene.grid.rows as i64 + dr > 0 {
            order.reverse();
        }
        order
            .into_iter()
            .map(|i| {
                Act::Run(Op::Place {
                    output: scene.output.to_owned(),
                    name: i.entry.name.clone(),
                    cell: Cell {
                        col: (i.cell.col as i64 + dc).max(0) as u32,
                        row: (i.cell.row as i64 + dr).max(0) as u32,
                    },
                    grid: scene.grid,
                })
            })
            .collect()
    }

    fn right_pressed(&mut self, scene: &Scene, p: Point) -> Vec<Act> {
        let acts = if self.rename.is_some() {
            self.rename_commit(scene)
        } else {
            Vec::new()
        };
        self.rename = None;
        self.close_floating();
        self.toast = None;
        self.press = None;
        match scene.at(p).map(|i| i.entry.name.clone()) {
            Some(name) => {
                if !self.selected.contains(&name) {
                    self.select_only(&name);
                } else {
                    self.cursor = Some(name.clone());
                }
                self.menu = Some(Menu {
                    at: p,
                    target: Target::Icon(name),
                });
            }
            None => {
                self.selected.clear();
                self.cursor = None;
                self.menu = Some(Menu {
                    at: p,
                    target: Target::Empty,
                });
            }
        }
        acts
    }

    // --- the menu ----------------------------------------------------------

    /// The rows of the open menu.
    pub fn menu_rows(&self, scene: &Scene) -> Vec<Row> {
        let Some(m) = &self.menu else {
            return Vec::new();
        };
        let entry = match &m.target {
            Target::Icon(n) => scene.entry(n),
            Target::Empty => None,
        };
        rows(&m.target, entry, self.selected.len() > 1)
    }

    /// A row was clicked.
    pub fn choose(&mut self, scene: &Scene, verb: Verb) -> Vec<Act> {
        let menu = self.menu.take();
        let subjects = self.subjects(scene);
        match verb {
            Verb::Open => self.activate(&subjects),
            Verb::Rename => {
                let name = match menu.map(|m| m.target) {
                    Some(Target::Icon(n)) => Some(n),
                    _ => self.cursor.clone(),
                };
                if let Some(n) = name {
                    self.begin_rename(&n);
                }
                Vec::new()
            }
            Verb::Copy => subjects
                .iter()
                .map(|e| {
                    Act::Run(Op::Copy {
                        src: e.path.clone(),
                        into: None,
                    })
                })
                .collect(),
            Verb::Trash => trash(&subjects),
            Verb::AllowLaunching => match menu.map(|m| m.target) {
                Some(Target::Icon(n)) => scene
                    .entry(&n)
                    .map(|e| vec![Act::Run(Op::AllowLaunching { path: e.path.clone() })])
                    .unwrap_or_default(),
                _ => Vec::new(),
            },
            Verb::NewFolder => vec![Act::Run(Op::NewFolder {
                name: "New Folder".to_owned(),
            })],
            Verb::NewFile => vec![Act::Run(Op::NewFile {
                name: "New File".to_owned(),
            })],
            Verb::Terminal => vec![Act::Terminal],
            Verb::Sort(sort) => vec![Act::Run(Op::Sort {
                output: scene.output.to_owned(),
                sort,
                grid: scene.grid,
            })],
            Verb::Settings => vec![Act::Settings],
        }
    }

    // --- the keyboard ------------------------------------------------------

    pub fn key(&mut self, scene: &Scene, key: Key) -> Vec<Act> {
        if self.rename.is_some() {
            // Typing and Enter belong to the text field; Escape gives it up.
            if key == Key::Escape {
                self.rename_cancel();
            }
            return Vec::new();
        }
        if self.menu.is_some() || self.prompt.is_some() {
            if key == Key::Escape {
                self.close_floating();
            }
            return Vec::new();
        }
        match key {
            Key::Left | Key::Right | Key::Up | Key::Down => {
                if let Some(n) = step(scene, self.cursor.as_deref(), key) {
                    self.select_only(&n);
                }
                Vec::new()
            }
            Key::Enter => {
                let subjects = self.subjects(scene);
                self.activate(&subjects)
            }
            Key::F2 => {
                if let Some(n) = self.cursor.clone().filter(|n| self.selected.contains(n)) {
                    self.begin_rename(&n);
                }
                Vec::new()
            }
            Key::Delete => trash(&self.subjects(scene)),
            Key::Escape => {
                self.selected.clear();
                self.cursor = None;
                self.toast = None;
                Vec::new()
            }
        }
    }

    // --- results -----------------------------------------------------------

    /// Forget names that are no longer on the desktop.
    pub fn prune(&mut self, entries: &[Entry]) {
        let here = |n: &String| entries.iter().any(|e| &e.name == n);
        self.selected.retain(here);
        if self.cursor.as_ref().is_some_and(|n| !here(n)) {
            self.cursor = None;
        }
        if self.hover.as_ref().is_some_and(|n| !here(n)) {
            self.hover = None;
        }
        if self.prompt.as_ref().is_some_and(|n| !here(n)) {
            self.prompt = None;
        }
        if self.rename.as_ref().is_some_and(|r| !here(&r.from)) {
            self.rename = None;
        }
        if self.last_click.as_ref().is_some_and(|(n, _)| !here(n)) {
            self.last_click = None;
        }
    }

    /// An operation finished. Returns the failure note's sequence number when
    /// one was raised, so the caller can schedule its expiry.
    pub fn settle(&mut self, op: &Op, result: &Result<Done, Failure>, entries: &[Entry]) -> Option<u64> {
        self.prune(entries);
        match result {
            Ok(Done::Path(p)) => {
                let name = p.file_name().and_then(|n| n.to_str()).map(str::to_owned);
                if let Some(name) = name.filter(|n| entries.iter().any(|e| &e.name == n)) {
                    match op {
                        Op::NewFolder { .. } | Op::NewFile { .. } => {
                            self.select_only(&name);
                            self.begin_rename(&name);
                        }
                        // A rename committed by a press elsewhere keeps what
                        // that press selected.
                        Op::Rename { .. } if !self.selected.is_empty() => {}
                        Op::Rename { .. } | Op::Copy { .. } => self.select_only(&name),
                        _ => {}
                    }
                }
                None
            }
            Ok(_) => None,
            Err(f) => Some(self.notify(*f)),
        }
    }

    /// Raise the failure note. Returns its sequence number, for [`Ui::expire`].
    pub fn notify(&mut self, f: Failure) -> u64 {
        self.toasts += 1;
        self.toast = Some(Toast {
            text: failure_text(f),
            seq: self.toasts,
        });
        self.toasts
    }

    /// The note's time is up (or it was clicked).
    pub fn expire(&mut self, seq: u64) {
        if self.toast.as_ref().is_some_and(|t| t.seq == seq) {
            self.toast = None;
        }
    }

    pub fn dismiss_toast(&mut self) {
        self.toast = None;
    }
}

fn trash(subjects: &[&Entry]) -> Vec<Act> {
    subjects
        .iter()
        .map(|e| Act::Run(Op::Trash { path: e.path.clone() }))
        .collect()
}

/// The icon one arrow-key step from `from`: nearest in that direction,
/// preferring the same row or column. With no start, the first icon.
pub fn step(scene: &Scene, from: Option<&str>, key: Key) -> Option<String> {
    let first = || {
        scene
            .items
            .iter()
            .min_by_key(|i| (i.cell.col, i.cell.row))
            .map(|i| i.entry.name.clone())
    };
    let Some(here) = from.and_then(|n| scene.find(n)) else {
        return first();
    };
    let c = here.cell;
    let d = |a: u32, b: u32| a.abs_diff(b);
    scene
        .items
        .iter()
        .filter_map(|i| {
            let o = i.cell;
            let key = match key {
                Key::Left if o.col < c.col => (d(o.row, c.row), c.col - o.col),
                Key::Right if o.col > c.col => (d(o.row, c.row), o.col - c.col),
                Key::Up if o.row < c.row => (d(o.col, c.col), c.row - o.row),
                Key::Down if o.row > c.row => (d(o.col, c.col), o.row - c.row),
                _ => return None,
            };
            Some((key, &i.entry.name))
        })
        .min()
        .map(|(_, n)| n.clone())
        .or_else(|| Some(here.entry.name.clone()))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::layout::Layout;
    use crate::scan::Launcher;

    const SIZE: Size = Size::new(1000.0, 600.0);

    fn entry(name: &str, kind: Kind) -> Entry {
        Entry {
            name: name.to_owned(),
            path: PathBuf::from("/desk").join(name),
            kind,
            mime: None,
            modified: None,
            size: 0,
            to_dir: false,
            broken: false,
            launcher: None,
        }
    }

    fn launcher(name: &str, trusted: bool) -> Entry {
        Entry {
            launcher: Some(Launcher {
                name: name.to_owned(),
                icon: None,
                app: None,
                trusted,
            }),
            ..entry(name, Kind::Launcher)
        }
    }

    /// Entries a, b, c (a folder), d.desktop (locked) in name order down the
    /// first column.
    fn entries() -> Vec<Entry> {
        vec![
            entry("a.txt", Kind::File),
            entry("b.txt", Kind::File),
            entry("c", Kind::Dir),
            launcher("d.desktop", false),
        ]
    }

    fn scene(entries: &[Entry]) -> Scene<'_> {
        let g = grid::grid_for(SIZE);
        let cells = Layout::default().arrange("DP-1", entries, g);
        Scene::from_parts("DP-1", SIZE, entries, &cells, Sort::Name)
    }

    fn centre(s: &Scene, name: &str) -> Point {
        s.find(name).unwrap().rect.center()
    }

    fn click(ui: &mut Ui, s: &Scene, p: Point, t: Instant) -> Vec<Act> {
        ui.pointer(s, Pointer::Moved(p), t);
        let mut acts = ui.pointer(s, Pointer::Pressed, t);
        acts.extend(ui.pointer(s, Pointer::Released, t));
        acts
    }

    fn names(ui: &Ui) -> Vec<&str> {
        ui.selected.iter().map(String::as_str).collect()
    }

    #[test]
    fn click_selects_one_and_empty_space_clears() {
        let es = entries();
        let s = scene(&es);
        let mut ui = Ui::default();
        let t = Instant::now();
        click(&mut ui, &s, centre(&s, "a.txt"), t);
        assert_eq!(names(&ui), ["a.txt"]);
        click(&mut ui, &s, centre(&s, "b.txt"), t);
        assert_eq!(names(&ui), ["b.txt"]);
        click(&mut ui, &s, Point::new(900.0, 500.0), t);
        assert!(ui.selected.is_empty());
    }

    #[test]
    fn ctrl_click_toggles() {
        let es = entries();
        let s = scene(&es);
        let mut ui = Ui::default();
        let t = Instant::now();
        click(&mut ui, &s, centre(&s, "a.txt"), t);
        ui.ctrl = true;
        click(&mut ui, &s, centre(&s, "b.txt"), t);
        assert_eq!(names(&ui), ["a.txt", "b.txt"]);
        click(&mut ui, &s, centre(&s, "a.txt"), t);
        assert_eq!(names(&ui), ["b.txt"]);
        // Ctrl on empty space keeps the selection.
        click(&mut ui, &s, Point::new(900.0, 500.0), t);
        assert_eq!(names(&ui), ["b.txt"]);
    }

    #[test]
    fn a_plain_click_on_one_of_several_selects_only_it() {
        let es = entries();
        let s = scene(&es);
        let mut ui = Ui::default();
        let t = Instant::now();
        click(&mut ui, &s, centre(&s, "a.txt"), t);
        ui.ctrl = true;
        click(&mut ui, &s, centre(&s, "b.txt"), t);
        ui.ctrl = false;
        click(&mut ui, &s, centre(&s, "b.txt"), t);
        assert_eq!(names(&ui), ["b.txt"]);
    }

    #[test]
    fn rubber_band_selects_what_it_covers_and_follows_the_pointer() {
        let es = entries();
        let s = scene(&es);
        let mut ui = Ui::default();
        let t = Instant::now();
        let a = s.find("a.txt").unwrap().rect;
        let b = s.find("b.txt").unwrap().rect;
        // Start in the margin left of the first column, drag over a and b.
        ui.pointer(&s, Pointer::Moved(Point::new(2.0, a.y + 5.0)), t);
        ui.pointer(&s, Pointer::Pressed, t);
        ui.pointer(&s, Pointer::Moved(Point::new(a.x + 30.0, b.y + 30.0)), t);
        assert!(ui.band.is_some());
        assert_eq!(names(&ui), ["a.txt", "b.txt"]);
        // Shrink back: b drops out.
        ui.pointer(&s, Pointer::Moved(Point::new(a.x + 30.0, a.y + 30.0)), t);
        assert_eq!(names(&ui), ["a.txt"]);
        ui.pointer(&s, Pointer::Released, t);
        assert!(ui.band.is_none());
        assert_eq!(names(&ui), ["a.txt"]);
    }

    #[test]
    fn a_ctrl_band_adds_to_the_selection() {
        let es = entries();
        let s = scene(&es);
        let mut ui = Ui::default();
        let t = Instant::now();
        click(&mut ui, &s, centre(&s, "d.desktop"), t);
        ui.ctrl = true;
        let a = s.find("a.txt").unwrap().rect;
        ui.pointer(&s, Pointer::Moved(Point::new(2.0, a.y + 5.0)), t);
        ui.pointer(&s, Pointer::Pressed, t);
        ui.pointer(&s, Pointer::Moved(Point::new(a.x + 30.0, a.y + 30.0)), t);
        ui.pointer(&s, Pointer::Released, t);
        assert_eq!(names(&ui), ["a.txt", "d.desktop"]);
    }

    #[test]
    fn a_double_click_activates_and_two_slow_clicks_do_not() {
        let es = entries();
        let s = scene(&es);
        let mut ui = Ui::default();
        let t = Instant::now();
        let p = centre(&s, "a.txt");
        assert!(click(&mut ui, &s, p, t).is_empty());
        let acts = click(&mut ui, &s, p, t + Duration::from_millis(100));
        assert_eq!(
            acts,
            [Act::Run(Op::Activate {
                path: PathBuf::from("/desk/a.txt")
            })]
        );
        let mut ui = Ui::default();
        click(&mut ui, &s, p, t);
        assert!(click(
            &mut ui,
            &s,
            p,
            t + Duration::from_millis(desktop::DOUBLE_CLICK_MS + 50)
        )
        .is_empty());
    }

    #[test]
    fn a_locked_launcher_asks_and_never_allows_itself() {
        let es = entries();
        let s = scene(&es);
        let mut ui = Ui::default();
        let t = Instant::now();
        let p = centre(&s, "d.desktop");
        click(&mut ui, &s, p, t);
        let acts = click(&mut ui, &s, p, t + Duration::from_millis(50));
        // No Activate, and no AllowLaunching: just the question.
        assert!(acts.is_empty());
        assert_eq!(ui.prompt.as_deref(), Some("d.desktop"));
        // Enter asks again (nothing was allowed).
        ui.prompt = None;
        assert!(ui.key(&s, Key::Enter).is_empty());
        assert_eq!(ui.prompt.as_deref(), Some("d.desktop"));
        // Escape withdraws it with nothing sent.
        assert!(ui.key(&s, Key::Escape).is_empty());
        assert!(ui.prompt.is_none());
        // Only the button sends it.
        ui.prompt = Some("d.desktop".into());
        assert_eq!(
            ui.allow(&s),
            [Act::Run(Op::AllowLaunching {
                path: PathBuf::from("/desk/d.desktop")
            })]
        );
        assert!(ui.prompt.is_none());
    }

    #[test]
    fn an_allowed_launcher_activates_straight_away() {
        let es = vec![launcher("ok.desktop", true)];
        let s = scene(&es);
        let mut ui = Ui::default();
        ui.select_only("ok.desktop");
        assert_eq!(
            ui.key(&s, Key::Enter),
            [Act::Run(Op::Activate {
                path: PathBuf::from("/desk/ok.desktop")
            })]
        );
        assert!(ui.prompt.is_none());
    }

    #[test]
    fn dragging_to_an_empty_cell_places_the_icon() {
        let es = entries();
        let s = scene(&es);
        let mut ui = Ui::default();
        let t = Instant::now();
        let from = centre(&s, "a.txt");
        let to = grid::cell_rect(Cell { col: 3, row: 2 }).center();
        ui.pointer(&s, Pointer::Moved(from), t);
        ui.pointer(&s, Pointer::Pressed, t);
        ui.pointer(&s, Pointer::Moved(to), t);
        assert!(ui.drag.is_some());
        let acts = ui.pointer(&s, Pointer::Released, t);
        assert_eq!(
            acts,
            [Act::Run(Op::Place {
                output: "DP-1".into(),
                name: "a.txt".into(),
                cell: Cell { col: 3, row: 2 },
                grid: s.grid,
            })]
        );
        assert!(ui.drag.is_none());
    }

    #[test]
    fn dropping_on_a_folder_moves_into_it_but_not_a_folder_into_itself() {
        let es = entries();
        let s = scene(&es);
        let mut ui = Ui::default();
        let t = Instant::now();
        ui.pointer(&s, Pointer::Moved(centre(&s, "a.txt")), t);
        ui.pointer(&s, Pointer::Pressed, t);
        ui.pointer(&s, Pointer::Moved(centre(&s, "c")), t);
        let acts = ui.pointer(&s, Pointer::Released, t);
        assert_eq!(
            acts,
            [Act::Run(Op::Move {
                src: PathBuf::from("/desk/a.txt"),
                into: Some(PathBuf::from("/desk/c")),
            })]
        );
        // The folder dropped on itself is a no-op.
        ui.pointer(&s, Pointer::Moved(centre(&s, "c")), t);
        ui.pointer(&s, Pointer::Pressed, t);
        ui.pointer(
            &s,
            Pointer::Moved(centre(&s, "c") + iced::Vector::new(10.0, 10.0)),
            t,
        );
        assert!(ui.pointer(&s, Pointer::Released, t).is_empty());
    }

    #[test]
    fn a_selection_drags_together_keeping_its_shape() {
        let es = entries();
        let s = scene(&es);
        let mut ui = Ui::default();
        let t = Instant::now();
        click(&mut ui, &s, centre(&s, "a.txt"), t);
        ui.ctrl = true;
        click(&mut ui, &s, centre(&s, "b.txt"), t);
        ui.ctrl = false;
        // Hold a (0,0), drop on column 2 row 0: a -> (2,0), b -> (2,1).
        ui.pointer(&s, Pointer::Moved(centre(&s, "a.txt")), t);
        ui.pointer(&s, Pointer::Pressed, t);
        ui.pointer(
            &s,
            Pointer::Moved(grid::cell_rect(Cell { col: 2, row: 0 }).center()),
            t,
        );
        let acts = ui.pointer(&s, Pointer::Released, t);
        let cells: Vec<(String, Cell)> = acts
            .iter()
            .map(|a| match a {
                Act::Run(Op::Place { name, cell, .. }) => (name.clone(), *cell),
                other => panic!("{other:?}"),
            })
            .collect();
        assert!(cells.contains(&("a.txt".into(), Cell { col: 2, row: 0 })));
        assert!(cells.contains(&("b.txt".into(), Cell { col: 2, row: 1 })));
        assert_eq!(cells.len(), 2);
    }

    #[test]
    fn the_empty_space_menu_lists_the_desktop_verbs() {
        let r = rows(&Target::Empty, None, false);
        let verbs: Vec<Verb> = r
            .iter()
            .filter_map(|r| if let Row::Verb(v) = r { Some(*v) } else { None })
            .collect();
        assert_eq!(
            verbs,
            [
                Verb::NewFolder,
                Verb::NewFile,
                Verb::Terminal,
                Verb::Sort(Sort::Name),
                Verb::Sort(Sort::Kind),
                Verb::Sort(Sort::Modified),
                Verb::Settings,
            ]
        );
        assert!(r.contains(&Row::Heading("Sort by")));
    }

    #[test]
    fn the_icon_menu_offers_allow_launching_only_for_a_locked_launcher() {
        let locked = launcher("x.desktop", false);
        let ok = launcher("y.desktop", true);
        let file = entry("f", Kind::File);
        let has = |e: &Entry, many| {
            rows(&Target::Icon(e.name.clone()), Some(e), many).contains(&Row::Verb(Verb::AllowLaunching))
        };
        assert!(has(&locked, false));
        assert!(!has(&ok, false));
        assert!(!has(&file, false));
        assert!(!has(&locked, true));
        // Trash is always there, and Rename only for one icon.
        let r = rows(&Target::Icon("f".into()), Some(&file), true);
        assert!(r.contains(&Row::Verb(Verb::Trash)));
        assert!(!r.contains(&Row::Verb(Verb::Rename)));
        assert!(rows(&Target::Icon("f".into()), Some(&file), false).contains(&Row::Verb(Verb::Rename)));
    }

    #[test]
    fn right_click_targets_the_icon_or_the_empty_space_and_the_menu_stays_on_screen() {
        let es = entries();
        let s = scene(&es);
        let mut ui = Ui::default();
        let t = Instant::now();
        ui.pointer(&s, Pointer::Moved(centre(&s, "b.txt")), t);
        ui.pointer(&s, Pointer::RightPressed, t);
        assert_eq!(ui.menu.as_ref().unwrap().target, Target::Icon("b.txt".into()));
        assert_eq!(names(&ui), ["b.txt"]);
        ui.pointer(&s, Pointer::Moved(Point::new(990.0, 590.0)), t);
        ui.pointer(&s, Pointer::RightPressed, t);
        let m = ui.menu.clone().unwrap();
        assert_eq!(m.target, Target::Empty);
        assert!(ui.selected.is_empty());
        let rows = ui.menu_rows(&s);
        let o = m.origin(SIZE, &rows);
        assert!(o.x + menu::W <= SIZE.width && o.y + menu_height(&rows) <= SIZE.height);
    }

    #[test]
    fn menu_verbs_become_operations() {
        let es = entries();
        let s = scene(&es);
        let mut ui = Ui::default();
        let t = Instant::now();
        click(&mut ui, &s, centre(&s, "b.txt"), t);
        ui.menu = Some(Menu {
            at: Point::ORIGIN,
            target: Target::Icon("b.txt".into()),
        });
        let acts = ui.choose(&s, Verb::Trash);
        assert_eq!(
            acts,
            [Act::Run(Op::Trash {
                path: PathBuf::from("/desk/b.txt")
            })]
        );
        assert!(ui.menu.is_none());
        assert_eq!(
            ui.choose(&s, Verb::Sort(Sort::Kind)),
            [Act::Run(Op::Sort {
                output: "DP-1".into(),
                sort: Sort::Kind,
                grid: s.grid
            })]
        );
        assert_eq!(ui.choose(&s, Verb::Terminal), [Act::Terminal]);
        assert_eq!(ui.choose(&s, Verb::Settings), [Act::Settings]);
        assert_eq!(
            ui.choose(&s, Verb::Copy),
            [Act::Run(Op::Copy {
                src: PathBuf::from("/desk/b.txt"),
                into: None
            })]
        );
    }

    #[test]
    fn f2_edits_the_name_enter_commits_and_escape_cancels() {
        let es = entries();
        let s = scene(&es);
        let mut ui = Ui::default();
        let t = Instant::now();
        click(&mut ui, &s, centre(&s, "a.txt"), t);
        assert!(ui.key(&s, Key::F2).is_empty());
        assert_eq!(ui.rename.as_ref().unwrap().text, "a.txt");
        // Keys other than Escape are the text field's, not the desktop's.
        assert!(ui.key(&s, Key::Delete).is_empty());
        assert!(ui.rename.is_some());
        ui.rename_input("renamed.txt".into());
        let acts = ui.rename_commit(&s);
        assert_eq!(
            acts,
            [Act::Run(Op::Rename {
                path: PathBuf::from("/desk/a.txt"),
                name: "renamed.txt".into()
            })]
        );
        assert!(ui.rename.is_none());
        // Escape throws it away.
        ui.begin_rename("a.txt");
        ui.rename_input("zzz".into());
        ui.key(&s, Key::Escape);
        assert!(ui.rename.is_none());
        // Unchanged commits nothing.
        ui.begin_rename("a.txt");
        assert!(ui.rename_commit(&s).is_empty());
        assert!(ui.rename.is_none());
    }

    #[test]
    fn a_name_the_desktop_would_refuse_keeps_the_field_open() {
        let es = entries();
        let s = scene(&es);
        let mut ui = Ui::default();
        ui.begin_rename("a.txt");
        for bad in ["", "a/b", ".."] {
            ui.rename_input(bad.into());
            assert!(!ui.rename.as_ref().unwrap().valid());
            assert!(ui.rename_commit(&s).is_empty());
            assert!(ui.rename.is_some(), "{bad:?}");
        }
    }

    #[test]
    fn clicking_away_commits_a_rename() {
        let es = entries();
        let s = scene(&es);
        let mut ui = Ui::default();
        let t = Instant::now();
        ui.begin_rename("a.txt");
        ui.rename_input("b2.txt".into());
        let acts = click(&mut ui, &s, Point::new(900.0, 500.0), t);
        assert_eq!(acts.len(), 1);
        assert!(ui.rename.is_none());
    }

    #[test]
    fn arrows_walk_the_grid_and_delete_trashes_and_escape_clears() {
        let es = entries();
        let s = scene(&es);
        let mut ui = Ui::default();
        // From nothing: the first icon.
        ui.key(&s, Key::Down);
        assert_eq!(names(&ui), ["a.txt"]);
        ui.key(&s, Key::Down);
        assert_eq!(names(&ui), ["b.txt"]);
        // Nothing to the left: stays.
        ui.key(&s, Key::Left);
        assert_eq!(names(&ui), ["b.txt"]);
        ui.key(&s, Key::Up);
        assert_eq!(names(&ui), ["a.txt"]);
        ui.key(&s, Key::Down);
        let acts = ui.key(&s, Key::Delete);
        assert_eq!(
            acts,
            [Act::Run(Op::Trash {
                path: PathBuf::from("/desk/b.txt")
            })]
        );
        ui.key(&s, Key::Escape);
        assert!(ui.selected.is_empty());
        // Delete with nothing selected does nothing.
        assert!(ui.key(&s, Key::Delete).is_empty());
    }

    #[test]
    fn right_arrow_finds_the_next_column() {
        let es = entries();
        let g = grid::grid_for(SIZE);
        let mut layout = Layout::default();
        layout.place("DP-1", "b.txt", Cell { col: 2, row: 0 }, &es, g);
        let cells = layout.arrange("DP-1", &es, g);
        let s = Scene::from_parts("DP-1", SIZE, &es, &cells, Sort::Name);
        let mut ui = Ui::default();
        ui.select_only("a.txt");
        ui.key(&s, Key::Right);
        assert_eq!(names(&ui), ["b.txt"]);
    }

    #[test]
    fn a_surface_closing_mid_drag_leaves_no_gesture_behind() {
        let es = entries();
        let s = scene(&es);
        let mut ui = Ui {
            drag: Some(Drag {
                names: vec!["a.txt".into()],
                primary: "a.txt".into(),
                grab: Point::ORIGIN,
                at: Point::ORIGIN,
            }),
            ..Ui::default()
        };
        ui.abandon_gesture();
        click(&mut ui, &s, centre(&s, "b.txt"), Instant::now());
        assert!(ui.drag.is_none() && ui.band.is_none());
    }

    #[test]
    fn a_rename_committed_by_a_press_keeps_that_presss_selection() {
        let es = entries();
        let mut ui = Ui::default();
        ui.select_only("b.txt");
        let op = Op::Rename {
            path: PathBuf::from("/desk/a.txt"),
            name: "z.txt".into(),
        };
        let mut after = es.clone();
        after.push(es[0].clone());
        after.last_mut().unwrap().name = "z.txt".into();
        ui.settle(&op, &Ok(Done::Path(PathBuf::from("/desk/z.txt"))), &after);
        assert_eq!(names(&ui), ["b.txt"]);
    }

    #[test]
    fn the_keyboard_is_wanted_only_while_something_needs_it() {
        let mut ui = Ui::default();
        assert!(!ui.wants_keys());
        ui.select_only("a");
        assert!(ui.wants_keys());
        ui.selected.clear();
        ui.menu = Some(Menu {
            at: Point::ORIGIN,
            target: Target::Empty,
        });
        assert!(ui.wants_keys());
    }

    #[test]
    fn a_failure_shows_once_and_expires_by_sequence() {
        let es = entries();
        let mut ui = Ui::default();
        let op = Op::Trash {
            path: PathBuf::from("/desk/a.txt"),
        };
        let f = Failure {
            op: "trash",
            kind: "cross-device",
        };
        let seq = ui.settle(&op, &Err(f), &es).unwrap();
        assert!(ui.toast.as_ref().unwrap().text.contains("another disk"));
        // A newer note is not cleared by the older one's timer.
        let seq2 = ui.settle(&op, &Err(f), &es).unwrap();
        ui.expire(seq);
        assert!(ui.toast.is_some());
        ui.expire(seq2);
        assert!(ui.toast.is_none());
    }

    #[test]
    fn a_new_folder_is_selected_and_opens_for_rename() {
        let mut es = entries();
        es.push(entry("New Folder", Kind::Dir));
        let mut ui = Ui::default();
        let op = Op::NewFolder {
            name: "New Folder".into(),
        };
        ui.settle(&op, &Ok(Done::Path(PathBuf::from("/desk/New Folder"))), &es);
        assert_eq!(names(&ui), ["New Folder"]);
        assert_eq!(ui.rename.as_ref().unwrap().from, "New Folder");
    }

    #[test]
    fn pruning_forgets_what_left_the_desktop() {
        let es = entries();
        let mut ui = Ui::default();
        ui.select_only("gone");
        ui.hover = Some("gone".into());
        ui.prompt = Some("gone".into());
        ui.prune(&es);
        assert!(ui.selected.is_empty() && ui.cursor.is_none() && ui.hover.is_none() && ui.prompt.is_none());
    }

    #[test]
    fn failure_words_never_carry_a_name() {
        let t = failure_text(Failure {
            op: "rename",
            kind: "invalid-name",
        });
        assert_eq!(t, "Could not rename that: that name is not allowed.");
    }

    #[test]
    fn a_press_on_the_ground_around_the_sheet_closes_menu_and_prompt() {
        let mut ui = Ui {
            menu: Some(Menu {
                at: Point::new(1.0, 1.0),
                target: Target::Empty,
            }),
            prompt: Some("a.desktop".to_owned()),
            ..Ui::default()
        };
        ui.dismiss_floating();
        assert!(ui.menu.is_none() && ui.prompt.is_none());
    }
}
