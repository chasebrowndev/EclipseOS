// SPDX-License-Identifier: AGPL-3.0-only
//! The Taskbar pane (ADR 0065, D-05 §4): a live picture of the bar that is
//! also where its widgets are arranged — dragged along it to reorder, in from
//! the picker under it to add, back out onto the picker to remove — one sheet
//! for whichever widget is selected, the bar's motion, and the bar's own
//! appearance and folding.
//!
//! Every `bar.*` key has a control here, and every one is still a schema row:
//! the grouping below is the one place this crate names keys by hand, and a
//! key it does not name lands in the "other" column rather than vanishing.
//!
//! The picture is solved by [`crate::bar_preview`] and drawn from the bar's
//! own primitives. Its cells glide with [`Animated`] under the configured
//! `bar.motion`, and the frame clock runs only while one of them moves.

use std::time::{Duration, Instant};

use iced::advanced::widget::operation::{Focusable, Operation, Outcome};
use iced::advanced::widget::Id;
use iced::widget::{
    button, column, container, pick_list, row, sensor, text, text_input, Column, Row, Space, Stack,
};
use iced::{Alignment, Element, Length, Point, Rectangle, Size, Task, Theme};
use serde_json::{json, Value};

use eclipse_ipc::{Approval, Widget, WidgetOp, WidgetStatus};
use eclipse_ui::motion::{Animated, Curve, Motion};
use eclipse_ui::theme;
use eclipse_ui::tokens::{bar, canvas, color, font, radius, size, space};
use eclipse_ui::widget::{
    arg_chip, art_thumb, badge, bar_cell_frame, bar_sheet, battery_gauge, big_value, chip, config_error,
    drag_bar, drag_ghost, draggable, drop_well, drop_zone, edge_note, edge_quad, elide, glide_track,
    hairline, inset, list_row, mark, micro_label, mini_meter, outline, panel, pill, pin, placed, ring,
    track_label, viz_bars, widget_shell, Grip, NumericSlider, Toggle, Well,
};

use crate::app::{App, Message};
use crate::bar_preview::{self as bp, Cell, Knobs, Rung, Solved};
use crate::conn::Problem;
use crate::editor::{Editor, Field, Kind, Slot, SOURCES};
use crate::schema::Row as Key;
use crate::tray::{Lane, Tray, Writes};

pub const ORDER: &str = "bar.widgets.order";
pub const IMPORTANT: &str = "bar.widgets.important";
const MOTION_ENABLED: &str = "bar.motion.enabled";
const MOTION_DURATION: &str = "bar.motion.duration-ms";
const MOTION_CURVE: &str = "bar.motion.curve";
/// B2b's key. Not in every compositor's schema yet, so it is stood in for by
/// path (see [`stand_in`]) and a write to it fails into the banner until the
/// compositor knows it.
pub const REMOTE_ART: &str = "bar.widgets.now-playing.remote-art";

/// The compositor's defaults for the two lists, for a pane with no socket.
const DEFAULT_ORDER: [&str; 7] = [
    "now-playing",
    "volume",
    "network",
    "bluetooth",
    "battery",
    "tray",
    "clock",
];
const DEFAULT_IMPORTANT: [&str; 2] = ["clock", "battery"];

/// The bar's appearance, and how it folds away. Everything else under `bar`
/// belongs to a widget's sheet, the motion band, or the tray.
const APPEARANCE: [&str; 4] = ["bar.position", "bar.rounding", "bar.eye", "bar.popup-anchor"];
const FOLDING: [&str; 6] = [
    "bar.fold-when-inactive",
    "bar.fold-when-idle",
    "bar.idle-seconds",
    "bar.fold-height",
    "bar.fold-duration-ms",
    "bar.fold-curve",
];

/// The keys each built-in's sheet shows, in order.
fn widget_keys(id: &str) -> &'static [&'static str] {
    match id {
        "now-playing" => &[
            "bar.widgets.now-playing.art",
            "bar.widgets.now-playing.visualizer",
            REMOTE_ART,
        ],
        "system-usage" => &[
            "bar.widgets.system-usage.interval-ms",
            "bar.widgets.system-usage.gpu",
            "bar.widgets.system-usage.disk",
            "bar.widgets.system-usage.disk-path",
        ],
        "volume" => &[
            "bar.widgets.volume.step",
            "bar.widgets.volume.scroll",
            "bar.widgets.volume.max-percent",
        ],
        "clock" => &["bar.clock.hour-12", "bar.clock.date-mdy"],
        _ => &[],
    }
}

/// Whether some block of this pane draws `path` already.
fn claimed(path: &str) -> bool {
    path.starts_with("bar.tray.")
        || path.starts_with("bar.motion.")
        || path == ORDER
        || path == IMPORTANT
        || APPEARANCE.contains(&path)
        || FOLDING.contains(&path)
        || bp::BUILTINS.iter().any(|id| widget_keys(id).contains(&path))
}

/// A key as a person reads it on this pane.
fn label(key: &Key) -> &str {
    match key.path.as_str() {
        "bar.position" => "Position",
        "bar.rounding" => "Corner radius",
        "bar.popup-anchor" => "Popups open",
        "bar.fold-when-inactive" => "Fold when inactive",
        "bar.fold-when-idle" => "Fold when idle",
        "bar.idle-seconds" => "Idle after, s",
        "bar.fold-height" => "Folded height",
        "bar.fold-duration-ms" => "Fold duration, ms",
        "bar.fold-curve" => "Fold curve",
        "bar.widgets.now-playing.art" => "Album art",
        "bar.widgets.now-playing.visualizer" => "Visualizer",
        REMOTE_ART => "Fetch remote art",
        "bar.widgets.system-usage.interval-ms" => "Refresh, ms",
        "bar.widgets.system-usage.gpu" => "GPU meter",
        "bar.widgets.system-usage.disk" => "Disk meter",
        "bar.widgets.system-usage.disk-path" => "Disk to measure",
        "bar.widgets.volume.step" => "Step, %",
        "bar.widgets.volume.scroll" => "Scroll to adjust",
        "bar.widgets.volume.max-percent" => "Loudest, %",
        "bar.clock.hour-12" => "12-hour time",
        "bar.clock.date-mdy" => "Month / day / year",
        MOTION_ENABLED => "Animate",
        MOTION_DURATION => "Duration, ms",
        MOTION_CURVE => "Curve",
        _ => key.label(),
    }
}

/// One line under a widget's rows: what it does, or what it reads.
fn note(id: &str) -> &'static str {
    match id {
        "now-playing" => {
            "The visualizer reads the audio output on this machine and never stores it. \
             Remote art asks the player's image host for the cover; off, only art already \
             on this machine is shown."
        }
        "system-usage" => "The bar shows the CPU; memory, then GPU and disk, pull out from the grip.",
        "volume" => "Pull the grip to reveal the slider. The loudest setting lets the output go past 100%.",
        "network" | "bluetooth" | "battery" => "No settings of its own: it shows what the system reports.",
        "tray" => {
            "Apps' status icons. Pinned ones sit on the bar in this order; the rest wait in the drawer."
        }
        "clock" => "Time over date, on the bar itself: these set the hour and the date's order.",
        _ => "",
    }
}

// ------------------------------------------------------------------ keys

/// Rows for the `bar` keys this compositor did not report, so the pane has
/// every control even with no socket, and so B2b's `remote-art` has a toggle
/// before the compositor that knows it lands. The compositor's own rows
/// always win; these fill gaps only.
pub fn stand_in(rows: &mut Vec<Key>) {
    let b = |p: &str, d: bool| json!({ "path": p, "type": "bool", "default": d, "value": d });
    let i = |p: &str, d: i64, min: i64, max: i64| json!({ "path": p, "type": "int", "default": d, "value": d, "constraints": { "min": min, "max": max } });
    let e = |p: &str, d: &str, values: &[&str]| json!({ "path": p, "type": "enum", "default": d, "value": d, "constraints": { "values": values } });
    let s = |p: &str, d: &str| json!({ "path": p, "type": "string", "default": d, "value": d });
    let l = |p: &str, d: Value| json!({ "path": p, "type": "string-list", "default": d.clone(), "value": d });
    const FOLD_CURVES: [&str; 4] = ["linear", "ease-in", "ease-out", "ease-in-out"];
    let wanted = [
        b("bar.fold-when-inactive", false),
        i("bar.fold-height", 4, 2, 16),
        b("bar.fold-when-idle", false),
        i("bar.idle-seconds", 30, 5, 600),
        i("bar.fold-duration-ms", 150, 0, 1000),
        e("bar.fold-curve", "ease-out", &FOLD_CURVES),
        e("bar.position", "top", &["top", "bottom"]),
        l("bar.tray.pinned", Value::Null),
        l("bar.tray.hidden", json!([])),
        i("bar.rounding", 20, 0, 64),
        b("bar.clock.hour-12", true),
        b("bar.clock.date-mdy", true),
        e("bar.popup-anchor", "cell", &["cell", "pointer"]),
        b("bar.eye", true),
        l(ORDER, json!(DEFAULT_ORDER)),
        l(IMPORTANT, json!(DEFAULT_IMPORTANT)),
        b("bar.widgets.now-playing.art", true),
        b("bar.widgets.now-playing.visualizer", true),
        b(REMOTE_ART, false),
        i("bar.widgets.system-usage.interval-ms", 1000, 250, 10_000),
        b("bar.widgets.system-usage.gpu", true),
        b("bar.widgets.system-usage.disk", true),
        s("bar.widgets.system-usage.disk-path", "/"),
        i("bar.widgets.volume.step", 5, 1, 25),
        b("bar.widgets.volume.scroll", true),
        i("bar.widgets.volume.max-percent", 100, 100, 150),
        b(MOTION_ENABLED, true),
        i(MOTION_DURATION, 220, 0, 2000),
        e(MOTION_CURVE, "spring", &Curve::ALL.map(Curve::as_str)),
    ];
    for mut v in wanted {
        let path = v["path"].as_str().unwrap_or_default().to_owned();
        if rows.iter().any(|r| r.path == path) {
            continue;
        }
        v["file"] = json!("abyss");
        v["readable"] = json!(true);
        v["writable"] = json!(true);
        v["source"] = json!("stand-in");
        if let Some(row) = Key::parse(&v) {
            rows.push(row);
        }
    }
}

fn list(app: &App, path: &str, fallback: &[&str]) -> Vec<String> {
    match app.key(path).map(|k| &k.value) {
        Some(Value::Array(a)) => a.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect(),
        _ => fallback.iter().map(|s| (*s).to_owned()).collect(),
    }
}

/// `bar.widgets.order` as configured.
pub fn order(app: &App) -> Vec<String> {
    list(app, ORDER, &DEFAULT_ORDER)
}

pub fn important(app: &App) -> Vec<String> {
    list(app, IMPORTANT, &DEFAULT_IMPORTANT)
}

fn flag(app: &App, path: &str, default: bool) -> bool {
    app.key(path).and_then(|k| k.value.as_bool()).unwrap_or(default)
}

fn knobs(app: &App) -> Knobs {
    Knobs {
        art: flag(app, "bar.widgets.now-playing.art", true),
        visualizer: flag(app, "bar.widgets.now-playing.visualizer", true),
        gpu: flag(app, "bar.widgets.system-usage.gpu", true),
        disk: flag(app, "bar.widgets.system-usage.disk", true),
    }
}

/// `bar.motion` as written. The live drag of the duration slider is not
/// motion yet: the demo moves once the value lands.
pub fn motion(app: &App) -> Motion {
    let ms = app.key(MOTION_DURATION).and_then(|k| k.value.as_u64());
    Motion {
        enabled: flag(app, MOTION_ENABLED, Motion::DEFAULT.enabled),
        curve: app
            .key(MOTION_CURVE)
            .and_then(|k| k.value.as_str())
            .and_then(Curve::parse)
            .unwrap_or(Motion::DEFAULT.curve),
        duration: ms.map(Duration::from_millis).unwrap_or(Motion::DEFAULT.duration),
    }
}

/// The two lines of the header's status chip.
pub fn status(app: &App) -> (String, String) {
    let o = order(app);
    let imp = important(app);
    let pinned = o.iter().filter(|id| imp.contains(id)).count();
    let m = motion(app);
    let motion = if m.snaps() {
        "motion off".to_owned()
    } else {
        format!("{} · {} ms", m.curve.as_str(), m.duration.as_millis())
    };
    let widgets = match o.len() {
        1 => "1 widget".to_owned(),
        n => format!("{n} widgets"),
    };
    let waiting = app
        .bar
        .statuses
        .iter()
        .filter(|s| s.approval == Approval::Pending)
        .count();
    (
        if waiting == 0 {
            widgets
        } else {
            format!("{widgets} · {waiting} waiting")
        },
        format!("{pinned} never compress · {motion}"),
    )
}

// ------------------------------------------------------------- approval
//
// ADR 0067. Settings shows what abyss decided and can ask for its prompt
// again; it never answers one. There is no Accept or Allow anywhere in this
// crate: the answer is given on the compositor's own surface.

/// Shown for every command widget that is not premade: the prompt's own
/// words (COMP-10 §3.11), verbatim.
pub const DISCLAIMER: &str =
    "This is not a premade widget. It runs this command as you. EclipseOS is not responsible for what it does.";
/// The risk, in one paragraph, under [`DISCLAIMER`].
pub const RISK: &str = "A command widget runs with your full authority as a user, every time it \
     refreshes: it can read, change or send anything you can. Only add commands you understand.";
/// The altered-premade prompt's own words (ADR 0067), verbatim.
pub const ALTERED: &str = "This widget has been altered! Altered widgets are not guaranteed to be safe!";

/// A widget's approval state, if the compositor reported one.
fn status_of<'a>(app: &'a App, name: &str) -> Option<&'a WidgetStatus> {
    app.bar.statuses.iter().find(|s| s.name == name)
}

fn pending(app: &App, name: &str) -> bool {
    status_of(app, name).is_some_and(|s| s.approval == Approval::Pending)
}

fn premade(app: &App, name: &str) -> bool {
    status_of(app, name).is_some_and(|s| s.premade)
}

// ----------------------------------------------------------------- state

#[derive(Debug, Clone)]
pub enum Msg {
    /// The preview sheet's size, first shown or resized.
    Measured(Size),
    Frame(Instant),
    Windows(f64),
    WindowsTyped(String),
    WindowsCommitted,
    Select(String),
    /// Something is in hand, with the pointer here. The first one starts
    /// the drag.
    Move(Payload, Point),
    /// The drag ended with the pointer here: land it, if it is over
    /// somewhere it can land.
    Release(Point),
    /// The drag ended without a release.
    Cancel,
    /// Where a drop target was laid out.
    Zone(Zone, Rectangle),
    /// A key the pane did not otherwise take: the keyboard's way to move.
    Key(Stroke),
    /// A non-Escape key, once the tree has said whether a text field has
    /// the keyboard (`true`): then it moves nothing.
    Keyed(Stroke, bool),
    Important(bool),
    Add(String),
    New,
    Name(String),
    Kind(Kind),
    ArgDraft(Slot, String),
    ArgPush(Slot),
    ArgRemove(Slot, usize),
    Interval(String),
    Source(String),
    Format(String),
    Icon(String),
    Save,
    /// First press asks, second press removes (the confirm row's Delete).
    Delete,
    /// The confirm row's Keep: back out of a delete.
    Keep,
    Close,
    /// After a new widget is saved: put it on the bar, or not.
    Offer(bool),
    /// Bring a withheld widget's approval prompt back. Only re-queues it: the
    /// answer is the owner's, on the compositor's surface.
    Review(String),
}

/// What a drag carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Payload {
    /// A widget on the bar, by id.
    Cell(String),
    /// A widget from the picker, not on the bar yet.
    Pick(String),
    /// A tray entry, from any of its three lanes.
    Tray(String),
}

/// Where a drag lands if it is released now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Between two cells: the slot, `0` before the first cell.
    Bar(usize),
    /// The picker: off the bar.
    Picker,
    /// A tray lane, before the entry now at that index in it.
    Tray(Lane, usize),
}

/// A laid-out region a drop is aimed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Zone {
    Sheet,
    Picker,
    Lane(Lane),
    Chip(String),
}

/// The keys the pane moves things with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stroke {
    Left,
    Right,
    Up,
    Down,
    Delete,
    Escape,
}

/// What a drop or a key does, as the pane's writes see it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Landing {
    Order(Vec<String>),
    Remove(String),
    Tray(String, Lane, usize),
}

#[derive(Debug, Clone)]
struct Drag {
    what: Payload,
    at: Point,
}

/// The drop targets, as last laid out.
#[derive(Debug, Clone, Default)]
struct Zones {
    sheet: Option<Rectangle>,
    picker: Option<Rectangle>,
    lanes: Vec<(Lane, Rectangle)>,
    chips: Vec<(String, Rectangle)>,
}

fn keep<K: PartialEq>(v: &mut Vec<(K, Rectangle)>, k: K, r: Rectangle) {
    match v.iter_mut().find(|(x, _)| *x == k) {
        Some(e) => e.1 = r,
        None => v.push((k, r)),
    }
}

/// One widget cell in flight.
#[derive(Debug, Clone)]
struct CellAnim {
    id: String,
    cell: Cell,
    x: Animated,
    extent: Animated,
    presence: Animated,
}

/// One window chip in flight. A chip that is leaving narrows to nothing.
/// Chips carry only a width: each sits right after the one before it, so a
/// chip arriving while its neighbours still shrink can never overlap them.
#[derive(Debug, Clone)]
struct ChipAnim {
    w: Animated,
}

pub struct Bar {
    windows: usize,
    windows_text: Option<String>,
    pub(crate) selected: Option<String>,
    drag: Option<Drag>,
    /// Escape dropped the drag in hand: the pointer's travel is ignored
    /// until the button comes up.
    escaped: bool,
    zones: Zones,
    /// Debug builds: a drag to hold mid-flight once the targets are laid
    /// out (`SETTINGS_PREVIEW_DRAG`).
    preview: Option<(Payload, Target)>,
    pub(crate) customs: Vec<Widget>,
    /// Every widget's approval state, withheld ones included: a withheld
    /// widget is absent from `customs` (abyss serves no command for it).
    pub(crate) statuses: Vec<WidgetStatus>,
    /// The last Review answer per widget: `Ok(true)` queued, `Ok(false)`
    /// already queued or on screen, `Err` refused. Kept per widget so a
    /// refusal is drawn on the row whose Review was pressed, not in the
    /// pane's banner scrolled out of view above it.
    reviewed: Vec<(String, Result<bool, Problem>)>,
    pub(crate) editor: Option<Editor>,
    offer: Option<String>,
    width: Option<f32>,
    motion: Option<Motion>,
    cells: Vec<CellAnim>,
    chips: Vec<ChipAnim>,
    plus_w: Animated,
    plus_count: usize,
    solved: Option<Solved>,
    glide: Animated,
    glide_right: bool,
    /// Debug builds: flip the window count on a timer, for a frame capture.
    sweep: Option<Instant>,
}

impl Default for Bar {
    fn default() -> Self {
        Bar {
            windows: canvas::WINDOWS_DEFAULT,
            windows_text: None,
            selected: None,
            drag: None,
            escaped: false,
            zones: Zones::default(),
            preview: None,
            customs: Vec::new(),
            statuses: Vec::new(),
            reviewed: Vec::new(),
            editor: None,
            offer: None,
            width: None,
            motion: None,
            cells: Vec::new(),
            chips: Vec::new(),
            plus_w: Animated::new(0.0, Motion::DEFAULT),
            plus_count: 0,
            solved: None,
            glide: Animated::new(0.0, Motion::DEFAULT),
            glide_right: false,
            sweep: None,
        }
    }
}

/// Move `a` toward `t`, or put it there. Retargets only on a real change, so
/// a sync every frame never restarts a leg.
fn aim(a: &mut Animated, t: f32, now: Instant, snap: bool) {
    if snap {
        a.snap(t);
    } else if a.target() != t {
        a.set_target(t, now);
    }
}

impl Bar {
    pub fn animating(&self) -> bool {
        self.glide.animating()
            || self.plus_w.animating()
            || self
                .cells
                .iter()
                .any(|c| c.x.animating() || c.extent.animating() || c.presence.animating())
            || self.chips.iter().any(|c| c.w.animating())
            || self.sweep.is_some()
            || self.editor.as_ref().is_some_and(|e| e.check_at.is_some())
    }

    fn every(&mut self) -> impl Iterator<Item = &mut Animated> {
        self.cells
            .iter_mut()
            .flat_map(|c| [&mut c.x, &mut c.extent, &mut c.presence])
            .chain(self.chips.iter_mut().map(|c| &mut c.w))
            .chain([&mut self.plus_w, &mut self.glide])
    }

    fn tick(&mut self, now: Instant) {
        for a in self.every() {
            a.tick(now);
        }
        self.cells
            .retain(|c| c.presence.target() > 0.0 || c.presence.animating());
        while self
            .chips
            .last()
            .is_some_and(|c| c.w.target() == 0.0 && !c.w.animating())
        {
            self.chips.pop();
        }
    }

    fn inner(&self) -> f32 {
        self.width.unwrap_or(canvas::SHEET_FALLBACK_W) - 2.0 * bar::EDGE
    }

    /// Each widget cell's left edge and width inside the sheet, in order.
    fn spans(&self, order: &[String]) -> Vec<(f32, f32)> {
        order
            .iter()
            .filter_map(|id| self.cells.iter().find(|c| c.id == *id))
            .map(|c| (c.x.value(), cell_w(c)))
            .collect()
    }

    /// The cells' centres in layout space: the picture's hit test.
    fn centres(&self, order: &[String], sheet: Rectangle) -> Vec<f32> {
        self.spans(order)
            .into_iter()
            .map(|(x, w)| sheet.x + bar::EDGE + x + w / 2.0)
            .collect()
    }
}

/// The slot a pointer at `x` names among cells centred at `centres` (left to
/// right): how many of them it has passed. A cell is passed at its middle,
/// so the slot changes where the drop would change the order.
fn slot_at(centres: &[f32], x: f32) -> usize {
    centres.iter().filter(|c| **c < x).count()
}

/// Where in a lane of chips a pointer at `at` falls, in reading order: the
/// chips on rows above it, and those left of it on its own row. A pointer
/// above or below every row is read as on the nearest one, so the lane's
/// padding still aims.
fn index_in(chips: &[Rectangle], at: Point) -> usize {
    let (Some(top), Some(bottom)) = (
        chips.iter().map(|r| r.y).reduce(f32::min),
        chips.iter().map(|r| r.y + r.height).reduce(f32::max),
    ) else {
        return 0;
    };
    let y = at.y.clamp(top, bottom - 1.0);
    chips
        .iter()
        .filter(|r| r.y + r.height <= y || (r.y <= y && r.center_x() < at.x))
        .count()
}

/// What releasing `what` over `to` would do, or `None` for nothing: a drop
/// back onto its own place, a pick onto the picker, anything onto a target
/// that does not take it.
fn landing(order: &[String], what: &Payload, to: Option<Target>) -> Option<Landing> {
    match (what, to?) {
        (Payload::Cell(id), Target::Bar(s)) => {
            let from = order.iter().position(|x| x == id)?;
            if s == from || s == from + 1 {
                return None;
            }
            let mut o = order.to_vec();
            o.remove(from);
            o.insert(if s > from { s - 1 } else { s }, id.clone());
            Some(Landing::Order(o))
        }
        (Payload::Cell(id), Target::Picker) => Some(Landing::Remove(id.clone())),
        (Payload::Pick(id), Target::Bar(s)) if !order.contains(id) => {
            let mut o = order.to_vec();
            o.insert(s.min(o.len()), id.clone());
            Some(Landing::Order(o))
        }
        (Payload::Tray(id), Target::Tray(lane, at)) => Some(Landing::Tray(id.clone(), lane, at)),
        _ => None,
    }
}

/// Where a drag with the pointer at `at` would land.
fn target(app: &App, what: &Payload, at: Point) -> Option<Target> {
    let z = &app.bar.zones;
    match what {
        Payload::Cell(_) | Payload::Pick(_) => {
            if let Some(sheet) = z.sheet {
                // A little above and below the picture still aims at it: the
                // bar is thin, and a drop that misses it by a hair is a drop
                // that did nothing.
                let reach = Rectangle {
                    y: sheet.y - space::ROW_Y,
                    height: sheet.height + 2.0 * space::ROW_Y,
                    ..sheet
                };
                if reach.contains(at) {
                    let s = slot_at(&app.bar.centres(&order(app), sheet), at.x);
                    return Some(Target::Bar(s));
                }
            }
            let over_picker = z.picker.is_some_and(|r| r.contains(at));
            (matches!(what, Payload::Cell(_)) && over_picker).then_some(Target::Picker)
        }
        Payload::Tray(_) => {
            let (lane, _) = z.lanes.iter().find(|(_, r)| r.contains(at))?;
            let t = app.tray();
            let chips: Vec<Rectangle> = t
                .in_lane(*lane)
                .iter()
                .filter_map(|id| z.chips.iter().find(|(c, _)| c == id).map(|(_, r)| *r))
                .collect();
            Some(Target::Tray(*lane, index_in(&chips, at)))
        }
    }
}

/// The drag in flight and where it would land, if it would land anywhere.
fn aimed(app: &App) -> Option<(&Drag, Target)> {
    let d = app.bar.drag.as_ref()?;
    let t = target(app, &d.what, d.at)?;
    landing(&order(app), &d.what, Some(t)).map(|_| (d, t))
}

/// The keyboard on the widget lane: Left and Right move the selection one
/// place, Delete takes it off the bar.
fn keyed(b: &mut Bar, order: &[String], key: Stroke) -> Option<Landing> {
    match key {
        Stroke::Left | Stroke::Right => shift(b, order, key == Stroke::Right).map(Landing::Order),
        Stroke::Delete => pick(b, order)
            .filter(|id| order.contains(id))
            .map(Landing::Remove),
        _ => None,
    }
}

/// The keyboard on the selected tray entry: Left and Right along the bar
/// lane, Up and Down between lanes, Delete hides it.
fn tray_keyed(t: &Tray, id: &str, key: Stroke) -> Option<Writes> {
    match key {
        Stroke::Left | Stroke::Right => t.shifted(id, key == Stroke::Right).map(|p| Writes {
            pinned: Some(p),
            hidden: None,
        }),
        Stroke::Up | Stroke::Down => t.stepped(id, key == Stroke::Down),
        Stroke::Delete => Some(t.moved(id, Lane::Hidden)),
        Stroke::Escape => None,
    }
}

/// Whether any text field in the tree has the keyboard. Unnamed fields
/// count too, which `find_focused` would skip.
struct Typing(bool);

impl Operation<bool> for Typing {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<bool>)) {
        operate(self);
    }

    fn focusable(&mut self, _id: Option<&Id>, _bounds: Rectangle, state: &mut dyn Focusable) {
        self.0 |= state.is_focused();
    }

    fn finish(&self) -> Outcome<bool> {
        Outcome::Some(self.0)
    }
}

/// A selection key, unless a text field has the keyboard: then arrows,
/// Delete and Backspace are the field's, in either lane.
fn gated(key: Stroke, typing: bool) -> Option<Stroke> {
    (!typing).then_some(key)
}

/// Do what a drop or a key decided.
fn apply(app: &mut App, l: Landing) {
    match l {
        Landing::Order(o) => app.write(ORDER, json!(o)),
        Landing::Remove(id) => remove(app, &id),
        Landing::Tray(id, lane, at) => {
            let w = app.tray().dropped(&id, lane, at);
            app.write_tray(w);
            app.tray_sel = Some(id);
        }
    }
}

/// Take `id` off the bar, and out of the important list with it. When it
/// was the selection, its neighbour takes it, so a run of removals is a run
/// of presses on one place.
fn remove(app: &mut App, id: &str) {
    let mut imp = important(app);
    if imp.iter().any(|x| x == id) {
        imp.retain(|x| x != id);
        app.write(IMPORTANT, json!(imp));
    }
    let before = order(app);
    let was_selected = pick(&app.bar, &before).as_deref() == Some(id);
    let at = before.iter().position(|x| x == id).unwrap_or(0);
    let mut o = before;
    o.retain(|x| x != id);
    app.write(ORDER, json!(o));
    if was_selected {
        let next = o.get(at.min(o.len().saturating_sub(1))).cloned();
        app.bar.editor = None;
        app.bar.selected = None;
        if let Some(next) = next {
            select(app, next);
        }
    }
}

/// Bring every animated value in line with the config and the knobs. Called
/// after every message while the pane shows; cheap when nothing changed.
pub fn sync(app: &mut App, now: Instant) {
    sync_with(app, now, false);
}

fn sync_with(app: &mut App, now: Instant, snap: bool) {
    adopt_selection(app);
    let m = motion(app);
    if app.bar.motion != Some(m) {
        let first = app.bar.motion.is_none();
        app.bar.motion = Some(m);
        for a in app.bar.every() {
            a.set_motion(m);
        }
        // The demo moves on each change, not on the first read.
        if !first {
            app.bar.glide_right = !app.bar.glide_right;
            let t = if app.bar.glide_right { 1.0 } else { 0.0 };
            app.bar.glide.set_target(t, now);
        }
    }
    let order = order(app);
    let imp = important(app);
    let k = knobs(app);
    let cells: Vec<Cell> = order
        .iter()
        .map(|id| Cell {
            span: bp::span(id, &k),
            important: imp.contains(id),
        })
        .collect();
    let s = bp::solve(app.bar.inner(), app.bar.windows, &cells);
    let b = &mut app.bar;

    for (i, id) in order.iter().enumerate() {
        let (x, e) = (s.xs[i], s.extents[i]);
        match b.cells.iter_mut().find(|c| c.id == *id) {
            Some(c) => {
                c.cell = cells[i];
                aim(&mut c.x, x, now, snap);
                aim(&mut c.extent, e, now, snap);
                aim(&mut c.presence, 1.0, now, snap);
            }
            None => {
                let mut c = CellAnim {
                    id: id.clone(),
                    cell: cells[i],
                    x: Animated::new(x, m),
                    extent: Animated::new(e, m),
                    presence: Animated::new(0.0, m),
                };
                aim(&mut c.presence, 1.0, now, snap);
                b.cells.push(c);
            }
        }
    }
    for c in b.cells.iter_mut().filter(|c| !order.contains(&c.id)) {
        aim(&mut c.presence, 0.0, now, snap);
    }

    for i in 0..s.shown {
        match b.chips.get_mut(i) {
            Some(c) => aim(&mut c.w, s.chip_w, now, snap),
            None => {
                let mut c = ChipAnim {
                    w: Animated::new(0.0, m),
                };
                aim(&mut c.w, s.chip_w, now, snap);
                b.chips.push(c);
            }
        }
    }
    for c in b.chips.iter_mut().skip(s.shown) {
        aim(&mut c.w, 0.0, now, snap);
    }
    if s.overflow > 0 {
        b.plus_count = s.overflow;
        aim(&mut b.plus_w, bar::OVERFLOW_W, now, snap);
    } else {
        aim(&mut b.plus_w, 0.0, now, snap);
    }
    b.solved = Some(s);
    if snap {
        b.tick(now);
    }
    // A held preview follows the cells it is aimed between as they settle.
    place_preview(app);
}

/// Open the pane's debug states: selection, window count, an open editor.
#[cfg(debug_assertions)]
pub fn preview_env(app: &mut App) {
    if let Ok(id) = std::env::var("SETTINGS_PREVIEW_WIDGET") {
        select(app, id);
    }
    if let Some(n) = std::env::var("SETTINGS_PREVIEW_WINDOWS")
        .ok()
        .and_then(|n| n.parse().ok())
    {
        app.bar.windows = canvas::WINDOWS_MAX.min(n);
    }
    // A drag held mid-flight: `<id>@<slot>` a bar cell over a slot,
    // `+<id>@<slot>` a picker widget over one, `-<id>` a bar cell over the
    // picker, `tray:<id>@<bar|drawer|hidden>` a tray entry over a lane. The
    // pointer is placed once the targets are laid out, so the real hit test
    // decides what is drawn.
    if let Ok(v) = std::env::var("SETTINGS_PREVIEW_DRAG") {
        let slot = |s: &str| s.parse::<usize>().ok();
        app.bar.preview = if let Some(id) = v.strip_prefix('-') {
            Some((Payload::Cell(id.into()), Target::Picker))
        } else if let Some((id, lane)) = v.strip_prefix("tray:").and_then(|r| r.split_once('@')) {
            let lane = match lane {
                "bar" => Lane::Taskbar,
                "drawer" => Lane::Overflow,
                _ => Lane::Hidden,
            };
            Some((Payload::Tray(id.into()), Target::Tray(lane, 0)))
        } else if let Some((id, s)) = v.strip_prefix('+').and_then(|r| r.split_once('@')) {
            slot(s).map(|s| (Payload::Pick(id.into()), Target::Bar(s)))
        } else {
            v.split_once('@')
                .and_then(|(id, s)| slot(s).map(|s| (Payload::Cell(id.into()), Target::Bar(s))))
        };
    }
    match std::env::var("SETTINGS_PREVIEW_EDITOR").as_deref() {
        Ok("new") => {
            let _ = update(app, Msg::New);
        }
        // An argument longer than the panel is wide, and a delete waiting
        // on its confirm.
        Ok("long") => {
            let w = Widget {
                name: "probe".into(),
                kind: eclipse_ipc::WidgetKind::Exec {
                    argv: vec![
                        "sh".into(),
                        "-c".into(),
                        "curl -s https://example.org/a/very/long/path/that/keeps/going?and=more&query=strings | head -n1"
                            .into(),
                    ],
                    interval_ms: 5000,
                },
                icon: None,
                on_click: None,
                on_scroll_up: None,
                on_scroll_down: None,
            };
            let mut e = Editor::from_widget(&w);
            e.checked = true;
            e.confirm_delete = true;
            app.bar.editor = Some(e);
            app.bar.selected = Some(format!("{}probe", bp::CUSTOM));
        }
        // A block the compositor refuses: an interval under its floor.
        Ok("invalid") => {
            let _ = update(app, Msg::New);
            if let Some(ed) = app.bar.editor.as_mut() {
                ed.name = "weather".into();
                ed.argv_mut(Slot::Command).args =
                    vec!["curl".into(), "-s".into(), "wttr.in/?format=%t".into()];
                ed.argv_mut(Slot::Click).args = vec!["xdg-open".into(), "https://wttr.in".into()];
            }
            let _ = update(app, Msg::Interval("10".into()));
        }
        _ => {}
    }
    if std::env::var_os("SETTINGS_PREVIEW_SWEEP").is_some() {
        app.bar.sweep = Some(Instant::now());
    }
    // A withheld command widget, as abyss would report it: the approval
    // states can be seen without a compositor that has the hook on.
    if let Ok(name) = std::env::var("SETTINGS_PREVIEW_PENDING") {
        if !app.bar.statuses.iter().any(|s| s.name == name) {
            app.bar.statuses.push(WidgetStatus {
                name,
                approval: Approval::Pending,
                premade: false,
                altered: false,
            });
        }
    }
    // A press, through the real update path, so what it leaves behind can be
    // captured with no input injected.
    match std::env::var("SETTINGS_PREVIEW_PRESS").as_deref() {
        Ok("review") => {
            let pending = app.bar.statuses.iter().find(|s| s.approval == Approval::Pending);
            if let Some(name) = pending.map(|s| s.name.clone()) {
                let _ = update(app, Msg::Review(name));
            }
        }
        Ok("review-denied") => {
            let pending = app.bar.statuses.iter().find(|s| s.approval == Approval::Pending);
            if let Some(name) = pending.map(|s| s.name.clone()) {
                app.bar.reviewed.push((
                    name,
                    Err(Problem::Denied {
                        path: None,
                        reason: "add-on hook `taskbar-widgets` is off".into(),
                    }),
                ));
            }
        }
        // Shift's own selection logic, with the order it would write landed
        // locally: `bar.widgets.order` exists only while the taskbar hook is
        // on, which a dev host with no add-on manifest cannot give.
        Ok("later") => {
            let o = order(app);
            if let Some(o) = shift(&mut app.bar, &o, true) {
                if let Some(k) = app.rows.iter_mut().find(|r| r.path == ORDER) {
                    k.value = json!(o);
                }
            }
        }
        _ => {}
    }
}

fn select(app: &mut App, id: String) {
    app.bar.editor = id
        .strip_prefix(bp::CUSTOM)
        .and_then(|name| app.bar.customs.iter().find(|w| w.name == name))
        .map(Editor::from_widget);
    app.bar.selected = Some(id);
    app.bar.offer = None;
}

/// Put a previewed drag's pointer where it names, once the targets it names
/// are laid out.
fn place_preview(app: &mut App) {
    let Some((what, to)) = app.bar.preview.clone() else {
        return;
    };
    let z = &app.bar.zones;
    let at = match to {
        Target::Bar(s) => z.sheet.map(|sheet| {
            let c = app.bar.centres(&order(app), sheet);
            let x = match (s.checked_sub(1).and_then(|i| c.get(i)), c.get(s)) {
                (Some(a), Some(b)) => (a + b) / 2.0,
                (None, Some(b)) => b - bar::WIDGET_H,
                (Some(a), None) => a + bar::WIDGET_H,
                (None, None) => sheet.center_x(),
            };
            Point::new(x, sheet.center_y())
        }),
        Target::Picker => z.picker.map(|r| r.center()),
        Target::Tray(lane, _) => z
            .lanes
            .iter()
            .find(|(l, _)| *l == lane)
            .map(|(_, r)| Point::new(r.x + r.width - bar::WIDGET_H, r.center_y())),
    };
    if let Some(at) = at {
        app.bar.drag = Some(Drag { what, at });
    }
}

/// Give the selection its editor when nobody clicked it. The first widget
/// stands selected on open, after a reload and after its neighbour goes; a
/// custom one there must open its block like a click would, not read as
/// missing. Runs with every sync, so any change to the fallback is caught.
pub fn adopt_selection(app: &mut App) {
    let o = order(app);
    adopt(&mut app.bar, &o);
}

/// [`adopt_selection`] on the pane's state alone. A draft (edited, or a new
/// block) is never replaced, and an editor already on the widget stays: a
/// config change reaches it through [`follow_config`].
fn adopt(b: &mut Bar, order: &[String]) {
    if b.editor.as_ref().is_some_and(|e| e.dirty || e.is_new()) {
        return;
    }
    let id = b.selected.clone().or_else(|| order.first().cloned());
    let Some(name) = id.as_deref().and_then(|i| i.strip_prefix(bp::CUSTOM)) else {
        return;
    };
    if b.editor.as_ref().and_then(|e| e.original.as_deref()) == Some(name) {
        return;
    }
    b.editor = b.customs.iter().find(|w| w.name == name).map(Editor::from_widget);
}

/// The selection, or the first widget when nothing is selected yet.
fn selected(app: &App) -> Option<String> {
    pick(&app.bar, &order(app))
}

/// [`selected`] on the pane's state alone.
fn pick(b: &Bar, order: &[String]) -> Option<String> {
    if b.editor.as_ref().is_some_and(Editor::is_new) {
        return None;
    }
    b.selected.clone().or_else(|| order.first().cloned())
}

/// Move the selected widget one place, and the order it leaves behind.
/// The selection is pinned to the widget by id first: the fallback
/// selection is "whatever is first", which would otherwise stay at the old
/// index while the widget the owner was moving walks away from it.
fn shift(b: &mut Bar, order: &[String], later: bool) -> Option<Vec<String>> {
    let id = pick(b, order)?;
    b.selected = Some(id.clone());
    let at = order.iter().position(|x| *x == id)?;
    let to = if later { at + 1 } else { at.checked_sub(1)? };
    if to >= order.len() {
        return None;
    }
    let mut o = order.to_vec();
    o.swap(at, to);
    Some(o)
}

/// The dry run: what the compositor would say to the block as it stands.
fn check(app: &mut App) {
    let Some(ed) = app.bar.editor.as_ref() else {
        return;
    };
    let verdict = ed
        .widget()
        .map(|w| app.conn.widget_write(&WidgetOp::Upsert(&w), true));
    let Some(ed) = app.bar.editor.as_mut() else {
        return;
    };
    ed.refused = None;
    ed.check_at = None;
    match verdict {
        Err(local) => {
            ed.errors = vec![local];
            ed.checked = true;
        }
        Ok(Ok(r)) => ed.take_result(&r),
        Ok(Err(p)) => ed.take_problem(&p),
    }
}

/// How long the fields must sit still before the dry run asks the
/// compositor. Each ask re-parses the whole config; a keystroke is not worth
/// one, a pause is.
const CHECK_QUIET: Duration = Duration::from_millis(300);

fn edit(app: &mut App, f: impl FnOnce(&mut Editor)) {
    if let Some(ed) = app.bar.editor.as_mut() {
        f(ed);
        ed.dirty = true;
        ed.confirm_delete = false;
        ed.refused = None;
        ed.check_at = Some(Instant::now() + CHECK_QUIET);
    }
}

/// Run the dry run now if one is waiting. The ask is synchronous, so there
/// is never more than one in flight.
fn flush(app: &mut App, now: Instant, force: bool) {
    let due = app
        .bar
        .editor
        .as_ref()
        .and_then(|e| e.check_at)
        .is_some_and(|at| force || now >= at);
    if due {
        check(app);
    }
}

/// The config changed underneath (a reload, another client, our own write).
/// An editor with nothing unsaved follows it; one with a draft keeps the
/// draft, and its next save answers against what is there now. The draft's
/// verdict was given against the old config, so it is asked again after the
/// usual pause.
pub fn follow_config(app: &mut App) {
    let Some(ed) = app.bar.editor.as_mut() else {
        return;
    };
    if ed.dirty {
        ed.check_at = Some(Instant::now() + CHECK_QUIET);
        return;
    }
    if ed.is_new() {
        return;
    }
    let name = ed.original.clone();
    let fresh = name
        .as_deref()
        .and_then(|n| app.bar.customs.iter().find(|w| w.name == n));
    match fresh {
        Some(w) => {
            if ed.widget().ok().as_ref() != Some(w) {
                let mut e = Editor::from_widget(w);
                e.checked = true;
                app.bar.editor = Some(e);
            }
        }
        None => {
            app.bar.editor = None;
            if !name.as_deref().is_some_and(|n| pending(app, n)) {
                app.bar.selected = None;
            }
        }
    }
}

/// One real write on the collection. `false` when it was refused; the
/// editor then carries why.
fn commit(app: &mut App, op: WidgetOp<'_>) -> bool {
    let r = app.conn.widget_write(&op, false);
    let Some(ed) = app.bar.editor.as_mut() else {
        return r.is_ok();
    };
    match r {
        Ok(r) if r.applied => true,
        Ok(r) => {
            ed.take_result(&r);
            ed.refused = Some("The compositor did not write it.".into());
            false
        }
        Err(p) => {
            ed.take_problem(&p);
            ed.refused = Some(p.headline());
            false
        }
    }
}

fn save(app: &mut App) {
    // A dry run still waiting on the pause answers first: saving must never
    // outrun the verdict on the text it saves.
    flush(app, Instant::now(), true);
    let Some(ed) = app.bar.editor.clone() else {
        return;
    };
    // The dry run already named a problem: writing now would store a value
    // the compositor has said it will ignore.
    if !ed.errors.is_empty() {
        return;
    }
    let w = match ed.widget() {
        Ok(w) => w,
        Err(local) => {
            if let Some(e) = app.bar.editor.as_mut() {
                e.errors = vec![local];
                e.checked = true;
            }
            return;
        }
    };
    if let (Some(orig), true) = (ed.original.as_deref(), ed.renamed()) {
        // Rename first: the compositor rewrites `custom:<old>` in both lists.
        if !commit(
            app,
            WidgetOp::Rename {
                name: orig,
                new_name: &w.name,
            },
        ) {
            return;
        }
    }
    if !commit(app, WidgetOp::Upsert(&w)) {
        return;
    }
    app.reload();
    let id = format!("{}{}", bp::CUSTOM, w.name);
    // A command abyss has not approved is withheld: it serves no command for
    // it, so there is nothing to edit until the owner answers the prompt.
    app.bar.editor = (!pending(app, &w.name)).then(|| {
        let mut fresh = Editor::from_widget(&w);
        fresh.checked = true;
        fresh
    });
    app.bar.selected = Some(id.clone());
    if ed.is_new() && !order(app).contains(&id) {
        app.bar.offer = Some(w.name);
    }
}

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    let now = Instant::now();
    match msg {
        Msg::Measured(s) => {
            let first = app.bar.width.is_none();
            app.bar.width = Some(s.width);
            if first {
                sync_with(app, now, true);
            }
        }
        Msg::Frame(t) => {
            app.bar.tick(t);
            flush(app, t, false);
            if let Some(at) = app.bar.sweep {
                const SWEEP: Duration = Duration::from_millis(1500);
                if t.saturating_duration_since(at) >= SWEEP {
                    app.bar.sweep = Some(t);
                    app.bar.windows = if app.bar.windows > canvas::WINDOWS_DEFAULT {
                        canvas::WINDOWS_DEFAULT
                    } else {
                        canvas::WINDOWS_MAX / 2
                    };
                }
            }
        }
        Msg::Windows(v) => {
            app.bar.windows = (v.round().max(0.0) as usize).min(canvas::WINDOWS_MAX);
            app.bar.windows_text = None;
        }
        Msg::WindowsTyped(t) => app.bar.windows_text = Some(t),
        Msg::WindowsCommitted => {
            if let Some(n) = app
                .bar
                .windows_text
                .take()
                .and_then(|t| t.trim().parse::<usize>().ok())
            {
                app.bar.windows = n.min(canvas::WINDOWS_MAX);
            }
        }
        Msg::Select(id) => select(app, id),
        Msg::Move(what, at) => {
            app.bar.preview = None;
            if !app.bar.escaped {
                app.bar.drag = Some(Drag { what, at });
            }
        }
        Msg::Release(at) => {
            app.bar.escaped = false;
            let Some(d) = app.bar.drag.take() else {
                return Task::none();
            };
            let to = target(app, &d.what, at);
            if let Some(l) = landing(&order(app), &d.what, to) {
                apply(app, l);
                if let Payload::Pick(id) = d.what {
                    select(app, id);
                }
            }
        }
        Msg::Cancel => {
            app.bar.drag = None;
            app.bar.escaped = false;
        }
        Msg::Zone(z, r) => {
            let zs = &mut app.bar.zones;
            match z {
                Zone::Sheet => zs.sheet = Some(r),
                Zone::Picker => zs.picker = Some(r),
                Zone::Lane(l) => keep(&mut zs.lanes, l, r),
                Zone::Chip(id) => keep(&mut zs.chips, id, r),
            }
            place_preview(app);
        }
        Msg::Key(Stroke::Escape) => {
            app.bar.escaped = app.bar.drag.take().is_some();
        }
        // iced 0.14's `text_input` leaves Up and Down uncaptured, so a
        // focused field would let them through: ask the tree first.
        Msg::Key(k) => {
            return iced::advanced::widget::operate(Typing(false)).map(move |t| bar_msg(Msg::Keyed(k, t)));
        }
        Msg::Keyed(k, typing) => {
            let Some(k) = gated(k, typing) else {
                return Task::none();
            };
            let o = order(app);
            let tray = pick(&app.bar, &o).as_deref() == Some("tray");
            match app.tray_sel.clone().filter(|_| tray) {
                Some(id) => {
                    if let Some(w) = tray_keyed(&app.tray(), &id, k) {
                        app.write_tray(w);
                    }
                }
                None => {
                    if let Some(l) = keyed(&mut app.bar, &o, k) {
                        apply(app, l);
                    }
                }
            }
        }
        Msg::Important(on) => {
            let Some(id) = selected(app) else {
                return Task::none();
            };
            app.bar.selected = Some(id.clone());
            let mut imp = important(app);
            imp.retain(|x| *x != id);
            if on {
                imp.push(id);
            }
            app.write(IMPORTANT, json!(imp));
        }
        Msg::Add(id) => {
            let mut o = order(app);
            if !o.contains(&id) {
                o.push(id.clone());
                app.write(ORDER, json!(o));
            }
            select(app, id);
        }
        // Widget writes are refused while the hook is off; the controls that
        // start one are not drawn, and these are the ones a debug env reaches.
        Msg::New | Msg::Save | Msg::Delete | Msg::Review(_) if !crate::addons::taskbar_widgets_on(app) => {}
        Msg::New => {
            app.bar.editor = Some(Editor::new());
            app.bar.offer = None;
        }
        Msg::Name(t) => edit(app, |e| e.name = t),
        Msg::Kind(k) => edit(app, |e| e.kind = k),
        Msg::ArgDraft(s, t) => edit(app, |e| e.argv_mut(s).draft = t),
        // A pushed chip lands before the input in the same row, and iced
        // keys widget state by position: the input is rebuilt and loses the
        // caret. Hand it back, so the next argument types straight in.
        Msg::ArgPush(s) => {
            edit(app, |e| e.argv_mut(s).push());
            return iced::widget::operation::focus(s.input_id());
        }
        Msg::ArgRemove(s, i) => {
            edit(app, |e| {
                let a = e.argv_mut(s);
                if i < a.args.len() {
                    a.args.remove(i);
                }
            });
            return iced::widget::operation::focus(s.input_id());
        }
        Msg::Interval(t) => edit(app, |e| e.interval = t),
        Msg::Source(t) => edit(app, |e| e.source = t),
        Msg::Format(t) => edit(app, |e| e.format = t),
        Msg::Icon(t) => edit(app, |e| e.icon = t),
        Msg::Save => save(app),
        Msg::Delete => {
            let Some(ed) = app.bar.editor.as_mut() else {
                return Task::none();
            };
            let Some(name) = ed.original.clone() else {
                return Task::none();
            };
            if !ed.confirm_delete {
                ed.confirm_delete = true;
                return Task::none();
            }
            if commit(app, WidgetOp::Remove { name: &name }) {
                app.reload();
                app.bar.editor = None;
                app.bar.selected = None;
            }
        }
        Msg::Keep => {
            if let Some(ed) = app.bar.editor.as_mut() {
                ed.confirm_delete = false;
            }
        }
        Msg::Close => {
            app.bar.editor = None;
        }
        Msg::Offer(yes) => {
            if let (true, Some(name)) = (yes, app.bar.offer.take()) {
                let id = format!("{}{}", bp::CUSTOM, name);
                let mut o = order(app);
                if !o.contains(&id) {
                    o.push(id);
                    app.write(ORDER, json!(o));
                }
            }
            app.bar.offer = None;
        }
        Msg::Review(name) => {
            let answer = app.conn.review_widget(&name);
            app.bar.reviewed.retain(|(n, _)| *n != name);
            app.bar.reviewed.push((name, answer));
        }
    }
    Task::none()
}

// ------------------------------------------------------------------ view
//
// Accent ledger: the one live yellow is the selected widget's outline on the
// bar picture, inside the hero. The picture itself is neutral: meters
// unaccented, the visualizer at rest, the launcher ring in secondary ink.
// A drag adds no yellow: the drop mark is white ink, the hole a cell leaves
// is a strong hairline, and a drop well answers in ground and edge only.
// Controls keep their own on-state (toggles, sliders).

fn bar_msg(m: Msg) -> Message {
    Message::Bar(m)
}

/// Every block of the pane after the header.
pub fn blocks(app: &App) -> Vec<Element<'_, Message, Theme>> {
    let on = crate::addons::taskbar_widgets_on(app);
    let mut out = Vec::new();
    if !on {
        out.push(edge_note(
            "Taskbar add-on not installed",
            "Custom and premade widgets come with the taskbar add-on. Without it the compositor \
             ignores widget blocks, so there is nothing here to edit; the bar's own settings \
             still apply.",
            color::NEUTRAL,
        ));
    }
    out.push(hero(app));
    if let Some(s) = sheet(app) {
        out.push(s);
    }
    out.push(motion_band(app));
    if on && !app.bar.statuses.is_empty() {
        out.push(approvals(app));
    }
    out.push(settings(app));
    out
}

/// One command widget's approval, as abyss reports it: the state line and,
/// for a withheld one, the Review control. Review only brings the
/// compositor's prompt back; Settings never answers it.
/// `named` is false inside the widget's own sheet, whose head already says it.
fn approval_row<'a>(app: &App, st: &WidgetStatus, named: bool) -> Element<'a, Message, Theme> {
    let (state, style): (&str, fn(&Theme) -> text::Style) = match st.approval {
        Approval::Approved => ("live", theme::text_secondary),
        Approval::Pending if st.altered => ("Waiting for approval \u{b7} altered", theme::text_danger),
        Approval::Pending => ("Waiting for approval", theme::text_secondary),
    };
    let mut r = Row::new().spacing(space::CONTROL_GAP).align_y(Alignment::Center);
    if named {
        r = r.push(
            text(st.name.clone())
                .font(font::UI)
                .size(size::BODY)
                .style(theme::text_primary),
        );
        if st.premade {
            r = r.push(badge("Premade"));
        }
        r = r.push(Space::new().width(Length::Fill)).push(tag(state, style));
    } else {
        r = r.push(tag(state, style)).push(Space::new().width(Length::Fill));
    }
    let answer = app
        .bar
        .reviewed
        .iter()
        .find(|(n, _)| *n == st.name)
        .map(|(_, a)| a)
        .filter(|_| st.approval == Approval::Pending);
    if st.approval == Approval::Pending {
        match answer {
            Some(Ok(true)) => r = r.push(caption("prompt queued".into())),
            Some(Ok(false)) => r = r.push(caption("prompt already open".into())),
            Some(Err(_)) => r = r.push(tag("prompt not shown", theme::text_danger)),
            None => {}
        }
        r = r.push(pill("Review", false, bar_msg(Msg::Review(st.name.clone()))));
    }
    let mut c = Column::new().push(r).spacing(space::LINE_GAP);
    if st.approval == Approval::Pending {
        c = c.push(caption_prose(if st.altered {
            ALTERED
        } else {
            "It does not run until you approve it in the compositor's prompt."
        }));
    }
    // A refused Review is drawn here, under the control that asked, with the
    // same rendering every refusal gets: DENIED and a config error alike.
    if let Some(Err(p)) = answer {
        c = c.push(config_error(&p.headline(), p.detail(), "", 0, 0));
    }
    padded(c)
}

/// Every widget block and whether it runs. Withheld ones are here even when
/// they are not on the bar, since abyss serves nothing else for them.
fn approvals(app: &App) -> Element<'_, Message, Theme> {
    let waiting = app
        .bar
        .statuses
        .iter()
        .filter(|s| s.approval == Approval::Pending)
        .count();
    let mut col = Column::new().push(padded(
        row![
            micro_label("custom widgets"),
            Space::new().width(Length::Fill),
            caption(match waiting {
                0 => "every command approved".to_owned(),
                n => format!("{n} waiting \u{b7} approve in the compositor's prompt"),
            }),
        ]
        .align_y(Alignment::Center),
    ));
    for st in &app.bar.statuses {
        col = col.push(hairline()).push(approval_row(app, st, true));
    }
    inset(col).into()
}

fn caption<'a>(t: String) -> Element<'a, Message, Theme> {
    text(t)
        .font(font::DATA)
        .size(size::MICRO)
        .style(theme::text_tertiary)
        .wrapping(text::Wrapping::None)
        .into()
}

fn prose<'a>(t: &str) -> Element<'a, Message, Theme> {
    text(t.to_owned())
        .font(font::UI)
        .size(size::BODY_SMALL)
        .style(theme::text_secondary)
        .into()
}

fn tag<'a>(t: &str, style: fn(&Theme) -> text::Style) -> Element<'a, Message, Theme> {
    text(t.to_owned())
        .font(font::DATA_MEDIUM)
        .size(size::MONO)
        .style(style)
        .wrapping(text::Wrapping::None)
        .into()
}

/// A still reading for a widget's core, drawn from the bar's primitives.
fn core_of<'a>(app: &App, id: &str) -> Element<'a, Message, Theme> {
    const LEVELS: [f32; bar::VIZ_BANDS] = [
        0.35, 0.6, 0.8, 0.55, 0.7, 0.9, 0.5, 0.65, 0.4, 0.75, 0.55, 0.3, 0.45, 0.6, 0.35, 0.25,
    ];
    let k = knobs(app);
    match id {
        "now-playing" => {
            let mut r = Row::new().spacing(bar::GAP).align_y(Alignment::Center);
            if k.art {
                r = r.push(art_thumb(None, false));
            }
            r = r.push(track_label("Midnight City", "M83", bar::MEDIA_TEXT_W));
            if k.visualizer {
                r = r.push(viz_bars(&LEVELS, false));
            }
            r.into()
        }
        "system-usage" => mini_meter("CPU", 0.42, false),
        "volume" => tag(bp::VOLUME_TAG, theme::text_primary),
        "network" => tag(bp::NETWORK_TAG, theme::text_primary),
        "bluetooth" => tag(bp::BLUETOOTH_TAG, theme::text_primary),
        "battery" => row![
            battery_gauge(84, color::TEXT_SECONDARY),
            tag(bp::BATTERY_READING, theme::text_primary)
        ]
        .spacing(bar::GAP)
        .align_y(Alignment::Center)
        .into(),
        "tray" => {
            let slot = || {
                container(mark(None, bar::MARK, color::TEXT_TERTIARY))
                    .width(Length::Fixed(bar::TRAY_MARK_W))
                    .align_x(Alignment::Center)
            };
            row![slot(), slot()].align_y(Alignment::Center).into()
        }
        "clock" => container(tag(
            if flag(app, "bar.clock.hour-12", true) {
                "9:41 PM"
            } else {
                "21:41"
            },
            theme::text_primary,
        ))
        .width(Length::Fixed(bar::CLOCK_W))
        .align_x(Alignment::Center)
        .into(),
        custom => row![
            mark(None, bar::MARK, color::TEXT_SECONDARY),
            tag(&elide(&bp::title(custom), bp::CUSTOM_CHARS), theme::text_primary),
        ]
        .spacing(bar::GAP)
        .align_y(Alignment::Center)
        .into(),
    }
}

/// The span the picture draws: the core alone. Nothing is ever revealed in
/// the picture, and the shell anchors its body right, so a revealed width it
/// is not given would show as empty room where the core should be.
fn drawn_span(c: &CellAnim) -> eclipse_ui::widget::ShellSpan {
    eclipse_ui::widget::ShellSpan {
        core: c.cell.span.core,
        revealed: 0.0,
    }
}

/// The on-screen width of a cell in flight.
fn cell_w(c: &CellAnim) -> f32 {
    let presence = c.presence.value().clamp(0.0, 1.0);
    if c.cell.grip() {
        drawn_span(c).width_at(c.cell.frame(c.extent.value(), presence))
    } else {
        c.cell.core_run() * presence
    }
}

fn window_chip<'a>(i: usize, w: f32) -> Element<'a, Message, Theme> {
    let (app_name, title) = bp::WINDOWS[i % bp::WINDOWS.len()];
    let glyph = || mark(None, bar::MARK, color::TEXT_SECONDARY);
    let label = |t: &str| {
        text(t.to_owned())
            .font(font::UI)
            .size(size::BODY_SMALL)
            .style(theme::text_primary)
            .wrapping(text::Wrapping::None)
    };
    let body: Element<'a, Message, Theme> = match Rung::of(w) {
        Rung::Full => row![glyph(), label(title)]
            .spacing(bar::GAP * 2.0)
            .align_y(Alignment::Center)
            .into(),
        Rung::Name => row![glyph(), label(app_name)]
            .spacing(bar::GAP * 2.0)
            .align_y(Alignment::Center)
            .into(),
        Rung::Icon => container(glyph())
            .width(Length::Fill)
            .align_x(Alignment::Center)
            .into(),
        Rung::Bare => Space::new().into(),
    };
    bar_cell_frame(body, w, false)
}

/// The bar, drawn. Every cell is a layer at an animated x.
fn picture(app: &App) -> Element<'_, Message, Theme> {
    let b = &app.bar;
    let mut layers: Vec<Element<'_, Message, Theme>> = vec![placed(
        0.0,
        bar_cell_frame(
            container(ring(bar::MARK, bar::RING, color::TEXT_SECONDARY))
                .width(Length::Fill)
                .align_x(Alignment::Center),
            bar::TASK_MIN,
            false,
        ),
    )];
    let mut x = bp::chip_x(0, 0.0);
    for (i, c) in b.chips.iter().enumerate() {
        let w = c.w.value();
        if w >= 1.0 {
            layers.push(placed(x, window_chip(i, w)));
            x += w + bar::GAP;
        }
    }
    let plus_w = b.plus_w.value();
    if plus_w >= 1.0 {
        layers.push(placed(
            x,
            bar_cell_frame(
                container(tag(&format!("+{}", b.plus_count), theme::text_secondary))
                    .width(Length::Fill)
                    .align_x(Alignment::Center),
                plus_w,
                false,
            ),
        ));
    }
    let sel = selected(app);
    let held = |id: &str| matches!(&b.drag, Some(Drag { what: Payload::Cell(x), .. }) if x == id);
    for c in &b.cells {
        let presence = c.presence.value().clamp(0.0, 1.0);
        let x = c.x.value();
        // The cell in hand leaves its place empty: an outline where it was,
        // so the drop reads as moving it out of that hole. The draggable
        // stays in the same layer, so iced keeps the press it is holding.
        let body: Element<'_, Message, Theme> = if held(&c.id) {
            outline(
                cell_w(c),
                bar::WIDGET_H,
                bar::HAIRLINE,
                color::BORDER_STRONG,
                bar::RADIUS_CELL,
            )
        } else if c.cell.grip() {
            widget_shell(
                drag_bar::<Message>().state(Grip::Rest),
                core_of(app, &c.id),
                None,
                drawn_span(c),
                c.cell.frame(c.extent.value(), presence),
            )
        } else {
            container(
                container(core_of(app, &c.id))
                    .width(Length::Fixed(c.cell.span.core))
                    .align_y(Alignment::Center),
            )
            .padding([0.0, bar::WIDGET_X])
            .width(Length::Fixed(cell_w(c)))
            .height(Length::Fixed(bar::WIDGET_H))
            .align_y(Alignment::Center)
            .clip(true)
            .into()
        };
        let what = Payload::Cell(c.id.clone());
        let lifted = b.drag.as_ref().filter(|d| d.what == what).map(|d| d.at);
        layers.push(placed(
            x,
            draggable(body)
                .ghost(drag_ghost(core_of(app, &c.id), bar::RADIUS_CELL))
                .lifted(lifted)
                .on_move(move |p| bar_msg(Msg::Move(what.clone(), p)))
                .on_release(|p| bar_msg(Msg::Release(p)))
                .on_cancel(bar_msg(Msg::Cancel))
                .on_click(bar_msg(Msg::Select(c.id.clone()))),
        ));
    }
    // The selection's outline, and the drop mark, above every cell and
    // after them in the stack: a layer that comes and goes ahead of a cell
    // would move that cell's place in the tree, and with it the press iced
    // keeps there.
    if let Some(c) = b
        .cells
        .iter()
        .find(|c| sel.as_deref() == Some(c.id.as_str()) && !held(&c.id))
    {
        if c.presence.value() > 0.0 {
            layers.push(placed(
                c.x.value(),
                outline(
                    cell_w(c),
                    bar::WIDGET_H,
                    bar::HAIRLINE,
                    color::ACCENT,
                    bar::RADIUS_CELL,
                ),
            ));
        }
    }
    if let Some((_, Target::Bar(slot))) = aimed(app) {
        let spans = b.spans(&order(app));
        let gap = |i: usize| match (i.checked_sub(1).and_then(|j| spans.get(j)), spans.get(i)) {
            (_, Some((x, _))) if i == 0 => x - bar::GAP / 2.0,
            (Some((x, w)), _) => x + w + bar::GAP / 2.0,
            (None, _) => 0.0,
        };
        layers.push(placed(
            gap(slot) - canvas::DROP_MARK_W / 2.0,
            edge_quad(
                Length::Fixed(canvas::DROP_MARK_W),
                Length::Fixed(bar::WIDGET_H),
                color::TEXT,
            ),
        ));
    }
    let sheet = bar_sheet(
        Stack::with_children(layers)
            .width(Length::Fill)
            .height(Length::Fill),
        Length::Fill,
    );
    drop_zone(
        sensor(sheet)
            .on_show(|s| bar_msg(Msg::Measured(s)))
            .on_resize(|s| bar_msg(Msg::Measured(s))),
        |r| bar_msg(Msg::Zone(Zone::Sheet, r)),
    )
    .into()
}

/// What the picture is showing, in words.
fn readout(app: &App) -> String {
    let b = &app.bar;
    let Some(s) = &b.solved else {
        return String::new();
    };
    let mut parts = vec![match b.windows {
        1 => "1 window".to_owned(),
        n => format!("{n} windows"),
    }];
    if b.windows > 0 {
        parts.push(format!("chips {}", Rung::of(s.chip_w).as_str()));
    }
    parts.push(match s.compressed {
        0 => "every widget open".to_owned(),
        1 => "1 widget folded to its grip".to_owned(),
        n => format!("{n} widgets folded to their grips"),
    });
    if s.overflow > 0 {
        parts.push(format!("+{} in overflow", s.overflow));
    }
    parts.join(" · ")
}

/// A widget off the bar, as something to pick up. Its ghost is the widget
/// as the bar would draw it, so what lands is seen before it does.
fn pickable<'a>(app: &'a App, id: String, name: &str) -> Element<'a, Message, Theme> {
    let what = Payload::Pick(id.clone());
    let lifted = app.bar.drag.as_ref().filter(|d| d.what == what).map(|d| d.at);
    draggable(chip(None, name, false, bar_msg(Msg::Add(id.clone()))))
        .ghost(drag_ghost(core_of(app, &id), bar::RADIUS_CELL))
        .lifted(lifted)
        .on_move(move |p| bar_msg(Msg::Move(what.clone(), p)))
        .on_release(|p| bar_msg(Msg::Release(p)))
        .on_cancel(bar_msg(Msg::Cancel))
        .on_click(bar_msg(Msg::Add(id)))
        .into()
}

/// The widgets that are not on the bar, and the place a bar cell is dropped
/// to take it off. Always under the picture: adding is a drag up into it.
fn picker(app: &App) -> Element<'_, Message, Theme> {
    let o = order(app);
    let mut chips: Vec<Element<'_, Message, Theme>> = Vec::new();
    for id in bp::BUILTINS.iter().filter(|id| !o.iter().any(|x| x == *id)) {
        chips.push(pickable(app, (*id).to_owned(), &bp::title(id)));
    }
    let on = crate::addons::taskbar_widgets_on(app);
    let customs = app.bar.customs.iter().map(|w| w.name.as_str());
    let withheld = app
        .bar
        .statuses
        .iter()
        .filter(|s| s.approval == Approval::Pending)
        .map(|s| s.name.as_str());
    for name in customs.chain(withheld).filter(|_| on) {
        let id = format!("{}{}", bp::CUSTOM, name);
        if !o.contains(&id) {
            chips.push(pickable(app, id, name));
        }
    }
    let held = match &app.bar.drag {
        Some(Drag {
            what: Payload::Cell(id),
            ..
        }) => Some(id.as_str()),
        _ => None,
    };
    let state = match (held, aimed(app)) {
        (Some(_), Some((_, Target::Picker))) => Well::Hot,
        (Some(_), _) => Well::Armed,
        (None, _) => Well::Idle,
    };
    let hint = match held {
        Some(id) => format!("drop here to take {} off the bar", bp::title(id)),
        None if chips.is_empty() => "every widget is on the bar".to_owned(),
        None => "drag onto the bar to add · click to add at the end".to_owned(),
    };
    let mut head = row![
        micro_label("off the bar"),
        caption(hint),
        Space::new().width(Length::Fill)
    ]
    .spacing(space::CONTROL_GAP)
    .align_y(Alignment::Center);
    if on {
        head = head.push(pill("New custom widget", false, bar_msg(Msg::New)));
    }
    let body = column![head, Row::with_children(chips).spacing(space::CHIP_GAP).wrap()].spacing(space::ROW_Y);
    drop_zone(drop_well(body, state), |r| bar_msg(Msg::Zone(Zone::Picker, r))).into()
}

/// The hero: the bar as it will draw, the windows that squeeze it, and the
/// lane that orders it.
fn hero(app: &App) -> Element<'_, Message, Theme> {
    let b = &app.bar;
    let shown = b.windows_text.clone().unwrap_or_else(|| b.windows.to_string());
    let invalid = b
        .windows_text
        .as_ref()
        .is_some_and(|t| t.trim().parse::<usize>().is_err());
    let windows = NumericSlider::new(
        0.0..=canvas::WINDOWS_MAX as f64,
        b.windows as f64,
        shown,
        |v| bar_msg(Msg::Windows(v)),
        |t| bar_msg(Msg::WindowsTyped(t)),
    )
    .step(1.0)
    .on_commit(bar_msg(Msg::WindowsCommitted))
    .invalid(invalid);

    let col = column![
        row![
            micro_label("live bar"),
            Space::new().width(Length::Fill),
            caption(readout(app)),
        ]
        .align_y(Alignment::Center),
        picture(app),
        row![
            caption("drag a widget along the bar to move it · click it to open its sheet".into()),
            Space::new().width(Length::Fill),
            prose("Open windows"),
            windows,
        ]
        .spacing(space::CONTROL_GAP)
        .align_y(Alignment::Center),
        picker(app),
    ]
    .spacing(space::ROW_Y);
    panel(app.glass_radius, col).into()
}

fn padded<'a>(e: impl Into<Element<'a, Message, Theme>>) -> Element<'a, Message, Theme> {
    container(e)
        .padding([space::ROW_Y, space::CARD])
        .width(Length::Fill)
        .into()
}

fn key_row<'a>(app: &'a App, path: &str) -> Option<Element<'a, Message, Theme>> {
    let key = app.key(path)?;
    Some(list_row(label(key), crate::app::control(app, key)))
}

/// The selected widget's sheet: where it sits, whether it gives way, and its
/// own settings. A custom widget's sheet is its editor.
fn sheet(app: &App) -> Option<Element<'_, Message, Theme>> {
    let new = app.bar.editor.as_ref().filter(|e| e.is_new());
    if let Some(ed) = new {
        let col = column![
            padded(
                row![
                    text("New custom widget")
                        .font(font::UI)
                        .size(size::CARD_TITLE)
                        .style(theme::text_primary),
                    Space::new().width(Length::Fill),
                    pill("Cancel", false, bar_msg(Msg::Close)),
                ]
                .align_y(Alignment::Center)
            ),
            hairline(),
            editor_view(app, ed),
        ];
        return Some(inset(col).into());
    }

    let id = selected(app)?;
    let o = order(app);
    let at = o.iter().position(|x| *x == id);
    let imp = important(app).contains(&id);

    let custom = id.strip_prefix(bp::CUSTOM);
    let mut head = Row::new().push(
        text(bp::title(&id))
            .font(font::UI)
            .size(size::CARD_TITLE)
            .style(theme::text_primary),
    );
    if custom.is_some_and(|n| premade(app, n)) {
        head = head.push(badge("Premade"));
    }
    let head = head
        .push(caption(match at {
            Some(i) => format!("{id} · {} of {}", i + 1, o.len()),
            None => id.clone(),
        }))
        .push(Space::new().width(Length::Fill))
        .spacing(space::CONTROL_GAP)
        .align_y(Alignment::Center);
    let head = match at {
        Some(_) => head.push(caption("← → move · delete removes".into())),
        None => head,
    };

    let mut col = Column::new().push(padded(head)).push(hairline());
    if at.is_some() {
        col = col.push(padded(
            row![
                pin(color::TEXT_SECONDARY),
                column![
                    text("Important")
                        .font(font::UI)
                        .size(size::BODY)
                        .style(theme::text_primary),
                    caption("never compresses: keeps its full size while everything else gives way".into()),
                ]
                .spacing(space::HAIRLINE),
                Space::new().width(Length::Fill),
                Toggle::new(imp, |on| bar_msg(Msg::Important(on))),
            ]
            .spacing(space::CONTROL_GAP)
            .align_y(Alignment::Center),
        ));
    }
    for path in widget_keys(&id) {
        if let Some(r) = key_row(app, path) {
            col = col.push(r);
        }
    }
    if id == "tray" {
        col = col.push(hairline());
        for r in tray_rows(app) {
            col = col.push(r);
        }
    }
    if let Some(name) = custom {
        col = col.push(hairline());
        let st = status_of(app, name).filter(|s| s.approval == Approval::Pending);
        match (&app.bar.editor, st) {
            _ if !crate::addons::taskbar_widgets_on(app) => {
                col = col.push(padded(prose(
                    "The taskbar add-on is not installed: the compositor ignores widget blocks.",
                )))
            }
            (_, Some(st)) => col = col.push(approval_row(app, st, false)),
            (Some(ed), None) => col = col.push(editor_view(app, ed)),
            (None, None) => {
                col = col.push(padded(prose(&format!(
                    "No widget block named \u{201c}{name}\u{201d} in abyss.kdl: the bar draws nothing here."
                ))))
            }
        }
    } else if !note(&id).is_empty() {
        col = col.push(padded(caption_prose(note(&id))));
    }
    if let Some(name) = &app.bar.offer {
        col = col.push(hairline()).push(padded(
            row![
                prose(&format!("Put \u{201c}{name}\u{201d} on the bar?")),
                Space::new().width(Length::Fill),
                pill("Add to bar", false, bar_msg(Msg::Offer(true))),
                pill("Not now", false, bar_msg(Msg::Offer(false))),
            ]
            .spacing(space::PILL_GAP)
            .align_y(Alignment::Center),
        ));
    }
    Some(inset(col).into())
}

fn caption_prose<'a>(t: &str) -> Element<'a, Message, Theme> {
    text(t.to_owned())
        .font(font::UI)
        .size(size::BODY_SMALL)
        .style(theme::text_tertiary)
        .into()
}

/// The tray's three lanes, flat inside the tray's sheet. Each entry is
/// dragged within its lane or into another; a click selects it for the
/// keyboard.
fn tray_rows(app: &App) -> Vec<Element<'_, Message, Theme>> {
    let t = app.tray();
    let sel = app.tray_sel.as_deref();
    let aim = aimed(app);
    let held = match &app.bar.drag {
        Some(Drag {
            what: Payload::Tray(id),
            ..
        }) => Some(id.clone()),
        _ => None,
    };
    let entry = |i: usize, id: &str, ordinal: bool| -> Element<'_, Message, Theme> {
        let what = Payload::Tray(id.to_owned());
        let lifted = app.bar.drag.as_ref().filter(|d| d.what == what).map(|d| d.at);
        let at_rest = chip(
            ordinal.then_some(i + 1),
            id,
            sel == Some(id) && held.as_deref() != Some(id),
            Message::TraySelect(id.to_owned()),
        );
        drop_zone(
            draggable(at_rest)
                .ghost(drag_ghost(tag(id, theme::text_primary), radius::CHIP))
                .lifted(lifted)
                .on_move(move |p| bar_msg(Msg::Move(what.clone(), p)))
                .on_release(|p| bar_msg(Msg::Release(p)))
                .on_cancel(bar_msg(Msg::Cancel))
                .on_click(Message::TraySelect(id.to_owned())),
            {
                let id = id.to_owned();
                move |r| bar_msg(Msg::Zone(Zone::Chip(id.clone()), r))
            },
        )
        .into()
    };
    let chips = |lane: Lane, ids: &[String], ordinal: bool| -> Element<'_, Message, Theme> {
        let mark = match aim {
            Some((_, Target::Tray(l, at))) if l == lane => Some(at),
            _ => None,
        };
        // Only the bar lane keeps an order, so only it shows where in the
        // order a drop lands. Every gap has its mark's room, lit or not: a
        // mark that came and went between chips would move the chips after
        // it in the tree, and the press iced keeps on the one in hand with
        // them.
        let gap = |k: usize| -> Element<'_, Message, Theme> {
            let lit = lane == Lane::Taskbar && mark == Some(k);
            let ink = if lit {
                color::TEXT
            } else {
                iced::Color::TRANSPARENT
            };
            container(edge_quad(
                Length::Fixed(canvas::DROP_MARK_W),
                Length::Fixed(space::CHIP_H),
                ink,
            ))
            .width(Length::Fixed(space::CHIP_GAP))
            .align_x(Alignment::Center)
            .into()
        };
        let mut r: Vec<Element<'_, Message, Theme>> = Vec::new();
        for (i, id) in ids.iter().enumerate() {
            r.push(gap(i));
            r.push(entry(i, id, ordinal));
        }
        r.push(gap(ids.len()));
        if ids.is_empty() && mark.is_none() {
            r.push(caption("none".into()));
        }
        Row::with_children(r)
            .align_y(Alignment::Center)
            .wrap()
            .vertical_spacing(space::CHIP_GAP)
            .into()
    };
    let live = match &app.tray_live {
        None => "reading the tray".to_owned(),
        Some(None) => "tray unreachable".to_owned(),
        Some(Some(l)) if l.len() == 1 => "1 app running".to_owned(),
        Some(Some(l)) => format!("{} apps running", l.len()),
    };
    let lane = |name: &str, lane: Lane, ids: Vec<String>, ordinal: bool| {
        let state = match (&held, aim) {
            (Some(_), Some((_, Target::Tray(l, _)))) if l == lane => Well::Hot,
            (Some(_), _) => Well::Armed,
            _ => Well::Idle,
        };
        drop_zone(
            drop_well(
                row![
                    container(micro_label(name)).width(Length::Fixed(space::FIELD_W / 2.0)),
                    chips(lane, &ids, ordinal),
                ]
                .spacing(space::CONTROL_GAP)
                .align_y(Alignment::Center),
                state,
            ),
            move |r| bar_msg(Msg::Zone(Zone::Lane(lane), r)),
        )
    };
    let hint = match sel {
        Some(id) => format!("{id} · ← → move · ↑ ↓ lane · delete hides"),
        None => "drag an icon between lanes".to_owned(),
    };
    vec![
        padded(
            row![
                micro_label("status icons"),
                caption(live),
                Space::new().width(Length::Fill),
                caption(hint),
            ]
            .spacing(space::CONTROL_GAP),
        ),
        // Each lane is a well with the sheet's own inset, so the lanes are
        // not padded again: their labels line up with the rows above.
        container(
            column![
                lane("on the bar", Lane::Taskbar, t.taskbar(), true),
                lane("drawer", Lane::Overflow, t.in_lane(Lane::Overflow), false),
                lane("hidden", Lane::Hidden, t.in_lane(Lane::Hidden), false),
            ]
            .spacing(space::HAIRLINE),
        )
        .padding([space::ROW_Y, 0.0])
        .width(Length::Fill)
        .into(),
    ]
}

fn errors_for<'a>(ed: &'a Editor, f: Field) -> Column<'a, Message, Theme> {
    Column::with_children(
        ed.errors_for(f)
            .map(|e| config_error(&e.position, &e.message, &e.snippet, e.col, e.span)),
    )
}

fn field<'a>(
    placeholder: &str,
    value: &str,
    bad: bool,
    on_input: impl Fn(String) -> Msg + 'a,
) -> text_input::TextInput<'a, Message, Theme> {
    text_input(placeholder, value)
        .on_input(move |t| bar_msg(on_input(t)))
        .font(font::DATA)
        .size(size::MONO)
        .width(Length::Fixed(space::FIELD_W))
        .style(if bad {
            theme::eclipse_input_invalid
        } else {
            theme::eclipse_input
        })
}

fn argv_row(ed: &Editor, slot: Slot) -> Element<'_, Message, Theme> {
    let a = ed.argv(slot);
    let bad = ed.errors_for(slot.field()).next().is_some();
    let mut r = Row::with_children(
        a.args
            .iter()
            .enumerate()
            .map(|(i, s)| arg_chip(s, bar_msg(Msg::ArgRemove(slot, i)))),
    );
    let hint = if a.args.is_empty() {
        "program, then Enter"
    } else {
        "argument, then Enter"
    };
    r = r.push(
        field(hint, &a.draft, bad, move |t| Msg::ArgDraft(slot, t))
            .id(slot.input_id())
            .on_submit(bar_msg(Msg::ArgPush(slot))),
    );
    column![
        list_row(
            slot.label(),
            r.spacing(canvas::ARG_GAP).align_y(Alignment::Center).wrap()
        ),
        errors_for(ed, slot.field()),
    ]
    .into()
}

/// One custom widget, as the fields its kind has.
fn editor_view<'a>(app: &App, ed: &'a Editor) -> Element<'a, Message, Theme> {
    let bad = |f| ed.errors_for(f).next().is_some();
    let mut kinds = Row::new().spacing(space::PILL_GAP);
    for k in Kind::ALL {
        kinds = kinds.push(pill(k.as_str(), ed.kind == k, bar_msg(Msg::Kind(k))));
    }
    // Premade means the block on file is the catalog's, unedited; a renamed
    // draft is a new block and is not.
    let is_premade = ed
        .original
        .as_deref()
        .is_some_and(|n| !ed.renamed() && premade(app, n));
    let mut col = Column::new();
    match ed.kind {
        Kind::Exec | Kind::Stream if !is_premade => {
            col = col.push(edge_note(DISCLAIMER, RISK, color::DANGER));
        }
        Kind::Exec | Kind::Stream => {
            col = col.push(padded(caption_prose(
                "Premade: shipped with EclipseOS and unedited. Saving a change makes it altered, \
                 and an altered widget does not run until you approve it in the compositor's prompt.",
            )));
        }
        Kind::Source => {}
    }
    let mut col = col.push(column![
        padded(caption_prose(
            "Commands run as you, without a shell: each chip is one argument, passed exactly as written."
        )),
        list_row("Name", field("weather", &ed.name, bad(Field::Name), Msg::Name)),
        errors_for(ed, Field::Name),
        list_row("Kind", kinds),
        padded(caption_prose(ed.kind.gist())),
    ]);
    match ed.kind {
        Kind::Exec | Kind::Stream => {
            col = col.push(argv_row(ed, Slot::Command));
            if ed.kind == Kind::Exec {
                col = col
                    .push(list_row(
                        "Every, ms",
                        field("5000", &ed.interval, bad(Field::Interval), Msg::Interval),
                    ))
                    .push(errors_for(ed, Field::Interval));
            }
        }
        Kind::Source => {
            let current = SOURCES.iter().copied().find(|s| *s == ed.source);
            col = col
                .push(list_row(
                    "Source",
                    pick_list(&SOURCES[..], current, |s: &str| {
                        bar_msg(Msg::Source(s.to_owned()))
                    }),
                ))
                .push(errors_for(ed, Field::Source))
                .push(list_row(
                    "Format",
                    row![
                        caption(format!("\u{2192} {}", ed.format_preview())),
                        field("{}", &ed.format, bad(Field::Format), Msg::Format),
                    ]
                    .spacing(space::CONTROL_GAP)
                    .align_y(Alignment::Center),
                ))
                .push(errors_for(ed, Field::Format));
        }
    }
    col = col
        .push(list_row(
            "Icon",
            field("icon name or path", &ed.icon, bad(Field::Icon), Msg::Icon),
        ))
        .push(errors_for(ed, Field::Icon))
        .push(hairline())
        .push(padded(micro_label("when used")));
    for slot in Slot::ACTIONS {
        col = col.push(argv_row(ed, slot));
    }
    col = col.push(errors_for(ed, Field::Block));

    let verdict = if let Some(r) = &ed.refused {
        r.clone()
    } else if ed.check_at.is_some() {
        "checking\u{2026}".to_owned()
    } else if !ed.checked {
        "unchanged".to_owned()
    } else if ed.errors.is_empty() {
        "the compositor would load this".to_owned()
    } else {
        match ed.errors.len() {
            1 => "1 problem".to_owned(),
            n => format!("{n} problems"),
        }
    };
    let mut foot = Row::new()
        .push(caption(verdict))
        .push(Space::new().width(Length::Fill))
        .spacing(space::PILL_GAP)
        .align_y(Alignment::Center);
    let delete = || {
        button(text("Delete").font(font::UI).size(size::BODY_SMALL))
            .padding([space::CHIP_Y, space::CHIP_X])
            .style(theme::danger)
            .on_press(bar_msg(Msg::Delete))
    };
    if ed.confirm_delete {
        // The ask replaces the foot rather than floating over the pane: the
        // question sits where the button was, answered in place.
        let name = ed.original.as_deref().unwrap_or_default();
        let ask = Row::new()
            .push(
                text(format!(
                    "Delete \u{201c}{name}\u{201d}? Its block leaves abyss.kdl and the bar."
                ))
                .font(font::UI)
                .size(size::BODY_SMALL),
            )
            .push(Space::new().width(Length::Fill))
            .push(pill("Keep", false, bar_msg(Msg::Keep)))
            .push(delete())
            .spacing(space::PILL_GAP)
            .align_y(Alignment::Center);
        return col.push(hairline()).push(padded(ask)).into();
    }
    if !ed.is_new() {
        foot = foot.push(delete());
    }
    foot = foot.push(pill(
        if ed.is_new() { "Create" } else { "Save" },
        false,
        bar_msg(Msg::Save),
    ));
    col.push(hairline()).push(padded(foot)).into()
}

/// Motion: a magnitude, its three controls, and the chip that shows it.
fn motion_band(app: &App) -> Element<'_, Message, Theme> {
    let m = motion(app);
    let magnitude: Element<'_, Message, Theme> = if m.snaps() {
        big_value("off", "", false)
    } else {
        big_value(&m.duration.as_millis().to_string(), "ms", false)
    };
    let mut controls = Column::new();
    for path in [MOTION_ENABLED, MOTION_CURVE, MOTION_DURATION] {
        if let Some(r) = key_row(app, path) {
            controls = controls.push(r);
        }
    }
    container(
        row![
            column![
                micro_label("motion"),
                magnitude,
                caption(m.curve.as_str().to_owned())
            ]
            .spacing(space::PILL_GAP)
            .width(Length::Fixed(space::FIELD_W / 2.0)),
            container(controls).width(Length::Fill),
            column![
                glide_track(app.bar.glide.value(), "demo"),
                caption("slides on each change".into()),
            ]
            .spacing(space::PILL_GAP)
            .align_x(Alignment::Center),
        ]
        .spacing(space::BLOCK)
        .align_y(Alignment::Center),
    )
    .padding([0.0, space::CARD])
    .width(Length::Fill)
    .into()
}

/// The bar itself: how it looks, and how it folds away.
fn settings(app: &App) -> Element<'_, Message, Theme> {
    let group = |name: &'static str, paths: Vec<&str>| {
        let mut c = Column::new().push(padded(micro_label(name)));
        for p in paths {
            if let Some(r) = key_row(app, p) {
                c = c.push(r);
            }
        }
        c.width(Length::Fill)
    };
    let mut body = Column::new().push(
        row![
            group("appearance", APPEARANCE.to_vec()),
            group("folding", FOLDING.to_vec())
        ]
        .spacing(space::BLOCK),
    );
    let rest: Vec<&str> = app
        .rows
        .iter()
        .filter(|k| crate::pane::pane_for(&k.path) == Some(crate::pane::Pane::Taskbar) && !claimed(&k.path))
        .map(|k| k.path.as_str())
        .collect();
    if !rest.is_empty() {
        body = body.push(hairline()).push(group("other", rest));
    }
    panel(app.glass_radius, body).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn widget(name: &str) -> Widget {
        Widget {
            name: name.into(),
            kind: eclipse_ipc::WidgetKind::Exec {
                argv: vec!["date".into()],
                interval_ms: 5000,
            },
            icon: None,
            on_click: None,
            on_scroll_up: None,
            on_scroll_down: None,
        }
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    fn bar_with(customs: &[&str]) -> Bar {
        Bar {
            customs: customs.iter().map(|n| widget(n)).collect(),
            ..Bar::default()
        }
    }

    fn open(b: &Bar) -> Option<&str> {
        b.editor.as_ref().and_then(|e| e.original.as_deref())
    }

    #[test]
    fn a_custom_widget_selected_by_default_opens_its_block() {
        let mut b = bar_with(&["weather"]);
        adopt(&mut b, &ids(&["custom:weather", "clock"]));
        assert_eq!(open(&b), Some("weather"));
    }

    #[test]
    fn a_builtin_selected_by_default_opens_no_editor() {
        let mut b = bar_with(&["weather"]);
        adopt(&mut b, &ids(&["clock", "custom:weather"]));
        assert!(b.editor.is_none());
    }

    #[test]
    fn the_editor_follows_when_the_default_selection_changes() {
        let mut b = bar_with(&["weather", "load"]);
        adopt(&mut b, &ids(&["custom:weather", "custom:load"]));
        adopt(&mut b, &ids(&["custom:load", "custom:weather"]));
        assert_eq!(open(&b), Some("load"));
    }

    #[test]
    fn a_draft_is_never_replaced() {
        let mut b = bar_with(&["weather", "load"]);
        adopt(&mut b, &ids(&["custom:weather"]));
        if let Some(e) = b.editor.as_mut() {
            e.name = "weather2".into();
            e.dirty = true;
        }
        adopt(&mut b, &ids(&["custom:load"]));
        assert_eq!(open(&b), Some("weather"));
        assert_eq!(b.editor.as_ref().map(|e| e.name.as_str()), Some("weather2"));

        b.editor = Some(Editor::new());
        adopt(&mut b, &ids(&["custom:load"]));
        assert!(b.editor.as_ref().is_some_and(Editor::is_new));
    }

    #[test]
    fn later_on_the_default_selection_follows_the_widget_not_the_index() {
        let mut b = bar_with(&["weather", "load"]);
        let o = ids(&["custom:weather", "clock", "custom:load"]);
        let moved = shift(&mut b, &o, true);
        assert_eq!(moved, Some(ids(&["clock", "custom:weather", "custom:load"])));
        let moved = moved.unwrap_or_default();
        assert_eq!(pick(&b, &moved).as_deref(), Some("custom:weather"));
        adopt(&mut b, &moved);
        assert_eq!(open(&b), Some("weather"));
    }

    #[test]
    fn a_shift_past_either_end_writes_nothing_and_keeps_the_selection() {
        let mut b = bar_with(&[]);
        let o = ids(&["clock", "tray"]);
        assert_eq!(shift(&mut b, &o, false), None);
        b.selected = Some("tray".into());
        assert_eq!(shift(&mut b, &o, true), None);
        assert_eq!(pick(&b, &o).as_deref(), Some("tray"));
    }

    #[test]
    fn an_explicit_selection_wins_over_the_first_widget() {
        let mut b = bar_with(&["weather", "load"]);
        b.selected = Some("custom:load".into());
        adopt(&mut b, &ids(&["custom:weather", "custom:load"]));
        assert_eq!(open(&b), Some("load"));
    }

    fn at(x: f32, y: f32) -> Point {
        Point::new(x, y)
    }

    #[test]
    fn the_slot_is_the_number_of_centres_passed() {
        let c = [10.0, 30.0, 50.0];
        assert_eq!(slot_at(&c, 0.0), 0);
        assert_eq!(slot_at(&c, 10.5), 1);
        assert_eq!(slot_at(&c, 29.0), 1);
        assert_eq!(slot_at(&c, 49.0), 2);
        assert_eq!(slot_at(&c, 99.0), 3);
        assert_eq!(slot_at(&[], 5.0), 0);
    }

    #[test]
    fn a_chip_index_reads_rows_then_columns() {
        let r = |x: f32, y: f32| Rectangle {
            x,
            y,
            width: 20.0,
            height: 10.0,
        };
        // Two on the first row, one on the second.
        let chips = [r(0.0, 0.0), r(30.0, 0.0), r(0.0, 20.0)];
        assert_eq!(index_in(&chips, at(5.0, 5.0)), 0);
        assert_eq!(index_in(&chips, at(25.0, 5.0)), 1);
        assert_eq!(index_in(&chips, at(99.0, 5.0)), 2);
        assert_eq!(index_in(&chips, at(99.0, 25.0)), 3);
        // Above every row reads as the first row.
        assert_eq!(index_in(&chips, at(25.0, -8.0)), 1);
        assert_eq!(index_in(&[], at(0.0, 0.0)), 0);
    }

    #[test]
    fn a_cell_dropped_on_the_bar_reorders() {
        let o = ids(&["a", "b", "c", "d"]);
        let cell = Payload::Cell("b".into());
        assert_eq!(
            landing(&o, &cell, Some(Target::Bar(0))),
            Some(Landing::Order(ids(&["b", "a", "c", "d"])))
        );
        assert_eq!(
            landing(&o, &cell, Some(Target::Bar(4))),
            Some(Landing::Order(ids(&["a", "c", "d", "b"])))
        );
        assert_eq!(
            landing(&o, &cell, Some(Target::Bar(3))),
            Some(Landing::Order(ids(&["a", "c", "b", "d"])))
        );
        // Either side of itself is its own place: nothing to write.
        assert_eq!(landing(&o, &cell, Some(Target::Bar(1))), None);
        assert_eq!(landing(&o, &cell, Some(Target::Bar(2))), None);
        assert_eq!(landing(&o, &cell, None), None);
    }

    #[test]
    fn a_pick_dropped_on_the_bar_adds_it_at_the_slot() {
        let o = ids(&["a", "b"]);
        assert_eq!(
            landing(&o, &Payload::Pick("x".into()), Some(Target::Bar(1))),
            Some(Landing::Order(ids(&["a", "x", "b"])))
        );
        assert_eq!(
            landing(&o, &Payload::Pick("x".into()), Some(Target::Bar(9))),
            Some(Landing::Order(ids(&["a", "b", "x"])))
        );
        // Already there, or dropped back on the picker: nothing.
        assert_eq!(
            landing(&o, &Payload::Pick("a".into()), Some(Target::Bar(0))),
            None
        );
        assert_eq!(
            landing(&o, &Payload::Pick("x".into()), Some(Target::Picker)),
            None
        );
    }

    #[test]
    fn a_cell_dropped_on_the_picker_is_removed() {
        let o = ids(&["a", "b"]);
        assert_eq!(
            landing(&o, &Payload::Cell("b".into()), Some(Target::Picker)),
            Some(Landing::Remove("b".into()))
        );
    }

    #[test]
    fn a_tray_entry_lands_only_in_a_tray_lane() {
        let o = ids(&["a"]);
        let t = Payload::Tray("steam".into());
        assert_eq!(
            landing(&o, &t, Some(Target::Tray(Lane::Hidden, 0))),
            Some(Landing::Tray("steam".into(), Lane::Hidden, 0))
        );
        assert_eq!(landing(&o, &t, Some(Target::Bar(0))), None);
        assert_eq!(
            landing(
                &o,
                &Payload::Cell("a".into()),
                Some(Target::Tray(Lane::Taskbar, 0))
            ),
            None
        );
    }

    #[test]
    fn a_tray_drop_writes_the_lane_and_the_place() {
        let tray = Tray::from_values(&serde_json::json!(["a", "b"]), &serde_json::json!([]));
        let w = tray.dropped("x", Lane::Taskbar, 0);
        assert_eq!(w.pinned, Some(ids(&["x", "a", "b"])));
        let w = tray.dropped("a", Lane::Hidden, 0);
        assert_eq!(w.pinned, Some(ids(&["b"])));
        assert_eq!(w.hidden, Some(ids(&["a"])));
    }

    #[test]
    fn arrows_move_the_selection_and_delete_removes_it() {
        let mut b = bar_with(&[]);
        let o = ids(&["a", "b", "c"]);
        b.selected = Some("b".into());
        assert_eq!(
            keyed(&mut b, &o, Stroke::Left),
            Some(Landing::Order(ids(&["b", "a", "c"])))
        );
        assert_eq!(
            keyed(&mut b, &o, Stroke::Right),
            Some(Landing::Order(ids(&["a", "c", "b"])))
        );
        assert_eq!(
            keyed(&mut b, &o, Stroke::Delete),
            Some(Landing::Remove("b".into()))
        );
        assert_eq!(keyed(&mut b, &o, Stroke::Up), None);
        // With nothing chosen, the first widget is the selection.
        b.selected = None;
        assert_eq!(keyed(&mut b, &o, Stroke::Left), None);
        assert_eq!(
            keyed(&mut b, &o, Stroke::Delete),
            Some(Landing::Remove("a".into()))
        );
    }

    #[test]
    fn up_and_down_move_nothing_while_a_field_is_focused() {
        // `a` is pinned on the bar and selected: Down would move it off.
        let t = Tray::from_values(&serde_json::json!(["a", "b"]), &serde_json::json!([]));
        for k in [Stroke::Up, Stroke::Down] {
            assert_eq!(gated(k, true).and_then(|k| tray_keyed(&t, "a", k)), None);
        }
        assert!(gated(Stroke::Down, false)
            .and_then(|k| tray_keyed(&t, "a", k))
            .is_some());
        // The widget lane's Delete is the field's too.
        let mut b = bar_with(&[]);
        b.selected = Some("clock".into());
        let o = ids(&["clock", "tray"]);
        assert_eq!(
            gated(Stroke::Delete, true).and_then(|k| keyed(&mut b, &o, k)),
            None
        );
        assert!(gated(Stroke::Delete, false)
            .and_then(|k| keyed(&mut b, &o, k))
            .is_some());
    }

    #[test]
    fn a_focused_unnamed_field_counts_as_typing() {
        type P = <iced::Renderer as iced::advanced::text::Renderer>::Paragraph;
        let mut field = text_input::State::<P>::new();
        let mut op = Typing(false);
        op.focusable(None, Rectangle::default(), &mut field);
        assert!(matches!(op.finish(), Outcome::Some(false)));
        field.focus();
        op.focusable(None, Rectangle::default(), &mut field);
        assert!(matches!(op.finish(), Outcome::Some(true)));
    }

    #[test]
    fn tray_keys_move_along_and_between_lanes() {
        let t = Tray::from_values(&serde_json::json!(["a", "b"]), &serde_json::json!([]));
        assert_eq!(
            tray_keyed(&t, "a", Stroke::Right).and_then(|w| w.pinned),
            Some(ids(&["b", "a"]))
        );
        assert_eq!(tray_keyed(&t, "a", Stroke::Left), None);
        assert_eq!(
            tray_keyed(&t, "a", Stroke::Down),
            Some(t.moved("a", Lane::Overflow))
        );
        assert_eq!(tray_keyed(&t, "a", Stroke::Up), None);
        assert_eq!(
            tray_keyed(&t, "b", Stroke::Delete),
            Some(t.moved("b", Lane::Hidden))
        );
    }
}
