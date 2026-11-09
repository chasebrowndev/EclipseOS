// SPDX-License-Identifier: AGPL-3.0-only
//! The Taskbar section (ADR 0065, D-05 §4), one page at a time: the bar's
//! appearance; its widgets — a live picture of the bar that is also where
//! they are arranged, dragged along it to reorder, in from the picker under
//! it to add, back out onto the picker to remove, and one sheet for
//! whichever is selected; how it folds; the tray's lanes; and its motion.
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

use ec_ipc::{Approval, Widget, WidgetOp, WidgetStatus};
use ec_ui::motion::{Animated, Curve, Motion};
use ec_ui::theme;
use ec_ui::tokens::{bar, canvas, color, font, motion as pace, size, space};
use ec_ui::widget::{
    arg_chip, art_mark, art_thumb, badge, bar_cell_frame, bar_sheet, battery_gauge, big_value, config_error,
    drag_bar, drag_ghost, draggable, drop_well, drop_zone, edge_note, edge_quad, elide, glass_inset as inset,
    glass_panel as panel, glide_track, hairline, list_row, mark, micro_label, mini_meter, outline, pill, pin,
    placed, ring, track_label, viz_bars, widget_shell_rounded, Art, Grip, NumericSlider, Toggle, Well,
};

use crate::app::{App, Message};
use crate::bar_preview::{self as bp, Cell, Knobs, Mode, Solved};
use crate::conn::Problem;
use crate::editor::{Editor, Field, Kind, Slot, SOURCES};
use crate::icons::Glyphs;
use crate::pane::Page;
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
const APPEARANCE: [&str; 5] = [
    "components.bar",
    "bar.position",
    "bar.rounding",
    "bar.eye",
    "bar.popup-anchor",
];
const FOLDING: [&str; 6] = [
    "bar.fold-when-inactive",
    "bar.fold-when-idle",
    "bar.idle-seconds",
    "bar.fold-height",
    "bar.fold-duration-ms",
    "bar.fold-curve",
];

/// The corner radius every widget on the bar wears; 17 is the full capsule.
const WIDGET_ROUNDING: &str = "bar.widgets.rounding";

/// The widgets' live corner radius, for the previews: the row's value as it
/// is edited, the capsule when the compositor does not report one.
fn widget_radius(app: &App) -> f32 {
    app.key(WIDGET_ROUNDING)
        .and_then(|k| k.value.as_u64())
        .map_or(bar::RADIUS_WIDGET, |r| r as f32)
}

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
        "clock" => &[
            "bar.clock.hour-12",
            "bar.clock.date-mdy",
            "bar.clock.week-start",
            "bar.clock.week-numbers",
        ],
        _ => &[],
    }
}

/// Whether some block of this pane draws `path` already.
fn claimed(path: &str) -> bool {
    path.starts_with("bar.tray.")
        || path.starts_with("bar.motion.")
        || path == ORDER
        || path == WIDGET_ROUNDING
        || path == IMPORTANT
        || path == crate::pinned::PATH
        || APPEARANCE.contains(&path)
        || FOLDING.contains(&path)
        || bp::BUILTINS.iter().any(|id| widget_keys(id).contains(&path))
}

/// A key as a person reads it on this pane.
fn label(key: &Key) -> std::borrow::Cow<'_, str> {
    std::borrow::Cow::Borrowed(match key.path.as_str() {
        "components.bar" => "Show the bar",
        "bar.position" => "Position",
        "bar.rounding" => "Corner radius",
        WIDGET_ROUNDING => "Corner radius",
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
        "bar.clock.week-start" => "Week starts on",
        "bar.clock.week-numbers" => "Week numbers",
        MOTION_ENABLED => "Animate",
        MOTION_DURATION => "Duration, ms",
        MOTION_CURVE => "Curve",
        _ => return key.label(),
    })
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
        i(WIDGET_ROUNDING, 17, 0, 17),
        b("bar.clock.hour-12", true),
        b("bar.clock.date-mdy", true),
        e("bar.clock.week-start", "auto", &["auto", "monday", "sunday"]),
        b("bar.clock.week-numbers", false),
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
        // Reduce motion (`animations.reduce-motion`) stills the picture
        // too: cells land where they go, and a missed drop does not fly.
        enabled: flag(app, MOTION_ENABLED, Motion::DEFAULT.enabled)
            && !flag(app, "animations.reduce-motion", false),
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
    /// somewhere it can land; fly it home if not.
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
    /// The inspector's move controls: one place later (`true`) or earlier.
    Step(bool),
    /// The inspector's Remove.
    Remove,
    Important(bool),
    Add(String),
    New,
    /// Open the selected custom widget's editor.
    Edit,
    Name(String),
    Kind(Kind),
    ArgDraft(Slot, String),
    ArgPush(Slot),
    ArgRemove(Slot, usize),
    Interval(String),
    Source(String),
    Format(String),
    Icon(String),
    /// Show or fold the editor's less-used fields.
    More(bool),
    Save,
    /// First press asks, second press removes (the confirm row's Delete).
    Delete,
    /// The confirm row's Keep: back out of a delete.
    Keep,
    Close,
    /// The unsaved-draft ask: keep editing it…
    Stay,
    /// …or throw it away and go where the owner was going.
    Discard,
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
    /// A widget from the shelf, not on the bar yet.
    Pick(String),
    /// A tray entry, from any of its three lanes.
    Tray(String),
}

/// Where a drag lands if it is released now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Between two cells: the slot, `0` before the first cell.
    Bar(usize),
    /// The shelf: off the bar.
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
    /// Select the next widget (tray entry); [`Stroke::BackTab`] the one
    /// before.
    Tab,
    BackTab,
    /// Lift the selection to move it several places, or drop it there.
    Space,
    /// Drop a lifted selection.
    Enter,
    /// Ctrl+Z.
    Undo,
}

/// What a drop or a key does, as the pane's writes see it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Landing {
    /// The new order, and the widget it moved or added.
    Order(Vec<String>, String),
    Remove(String),
    Tray(String, Lane, usize),
}

#[derive(Debug, Clone)]
struct Drag {
    what: Payload,
    at: Point,
}

/// A selection lifted by Space: the order it is being walked through, not
/// written until it is dropped.
#[derive(Debug, Clone, PartialEq)]
struct Lift {
    id: String,
    order: Vec<String>,
    from: Vec<String>,
}

/// A missed drop flying back to where it came from.
#[derive(Debug, Clone)]
struct Homing {
    what: Payload,
    t: Animated,
    /// Where it let go, for a draggable that has no release of its own to
    /// fly from (the tree was rebuilt, or a debug preview).
    from: Option<Point>,
}

/// Where a confirmed discard of the open draft goes next.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Leave {
    To(String),
    New,
    Close,
}

/// What undoing one change writes back: every key it touched as it was,
/// in one commit, and the selections that went with it.
#[derive(Debug, Clone)]
pub struct Restore {
    edits: Vec<(String, Value)>,
    select: Option<String>,
    tray: Option<String>,
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
    /// The last Review answer per widget.
    reviewed: Vec<(String, Result<bool, Problem>)>,
    pub(crate) editor: Option<Editor>,
    /// The editor is open over the inspector (a custom widget's Edit, or
    /// New custom widget).
    editing: bool,
    /// The editor's less-used fields are unfolded.
    more: bool,
    /// The owner asked to leave a draft with unsaved changes: the inspector
    /// asks before it goes.
    leaving: Option<Leave>,
    offer: Option<String>,
    lift: Option<Lift>,
    homing: Option<Homing>,
    /// Per frame, how far an edge-held drag scrolls the pane.
    autoscroll: f32,
    /// The status line: what the last move did, or why it did nothing.
    said: Option<String>,
    /// Debug builds: the mode to draw, over the config's.
    mode: Option<Mode>,
    /// Debug builds: the tray's live apps are the preview's, not the feed's.
    pub(crate) tray_preview: bool,
    glyphs: Glyphs,
    width: Option<f32>,
    /// The narrowest the bar can be drawn (`bp::min_inner`), as last solved.
    min_inner: f32,
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
            editing: false,
            more: false,
            leaving: None,
            offer: None,
            lift: None,
            homing: None,
            autoscroll: 0.0,
            said: None,
            mode: None,
            tray_preview: false,
            glyphs: Glyphs::default(),
            width: None,
            min_inner: 0.0,
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

impl Bar {
    /// The bar's own icons, looked up once. Not in `Default`: the lookups
    /// touch the filesystem, and tests build bars by the hundred.
    pub fn with_glyphs() -> Bar {
        Bar {
            glyphs: Glyphs::resolve(),
            ..Bar::default()
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
            || self.homing.as_ref().is_some_and(|h| h.t.animating())
            || (self.drag.is_some() && self.autoscroll != 0.0)
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
        if let Some(h) = self.homing.as_mut() {
            h.t.tick(now);
            if !h.t.animating() && h.t.target() >= 1.0 {
                self.homing = None;
            }
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

    /// The width the picture solves for: the pane's, or the bar's
    /// narrowest when the pane is narrower (the picture then scrolls).
    fn inner(&self) -> f32 {
        (self.width.unwrap_or(canvas::SHEET_FALLBACK_W) - 2.0 * bar::EDGE).max(self.min_inner)
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

    /// Whether `what` is in hand or flying home: its place stays a hole.
    fn away(&self, what: &Payload) -> bool {
        self.drag.as_ref().is_some_and(|d| d.what == *what)
            || self.homing.as_ref().is_some_and(|h| h.what == *what)
    }

    /// The `.lifted` and `.homing` a draggable carrying `what` is given.
    fn ghost_of(&self, what: &Payload) -> (Option<Point>, Option<f32>) {
        if let Some(d) = self.drag.as_ref().filter(|d| d.what == *what) {
            return (Some(d.at), None);
        }
        match self.homing.as_ref().filter(|h| h.what == *what) {
            Some(h) => (h.from, Some(h.t.value())),
            None => (None, None),
        }
    }
}

/// The mode the preview draws: the config's `mode`, or a debug override.
fn mode(app: &App) -> Mode {
    app.bar.mode.unwrap_or_else(|| {
        app.key("mode")
            .and_then(|k| k.value.as_str())
            .map_or(Mode::default(), Mode::parse)
    })
}

/// The order the picture draws: a lifted selection's walk, else the config.
fn shown_order(app: &App) -> Vec<String> {
    match &app.bar.lift {
        Some(l) => l.order.clone(),
        None => order(app),
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
/// back onto its own place, a pick onto the shelf, anything onto a target
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
            Some(Landing::Order(o, id.clone()))
        }
        (Payload::Cell(id), Target::Picker) => Some(Landing::Remove(id.clone())),
        (Payload::Pick(id), Target::Bar(s)) if !order.contains(id) => {
            let mut o = order.to_vec();
            o.insert(s.min(o.len()), id.clone());
            Some(Landing::Order(o, id.clone()))
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
/// Only a target that would take the drop is ever returned, so a cue is
/// drawn only where a release would do something.
fn aimed(app: &App) -> Option<(&Drag, Target)> {
    let d = app.bar.drag.as_ref()?;
    let t = target(app, &d.what, d.at)?;
    landing(&order(app), &d.what, Some(t)).map(|_| (d, t))
}

/// `order` with `id` swapped one place later or earlier; `None` past
/// either end.
fn swapped(order: &[String], id: &str, later: bool) -> Option<Vec<String>> {
    let at = order.iter().position(|x| x == id)?;
    let to = if later { at + 1 } else { at.checked_sub(1)? };
    if to >= order.len() {
        return None;
    }
    let mut o = order.to_vec();
    o.swap(at, to);
    Some(o)
}

/// The next (or, `back`, previous) of `ids` after `sel`, wrapping; the first
/// (last) when nothing is selected.
fn cycle(ids: &[String], sel: Option<&str>, back: bool) -> Option<String> {
    let n = ids.len();
    if n == 0 {
        return None;
    }
    let i = match sel.and_then(|s| ids.iter().position(|x| x == s)) {
        Some(i) if back => (i + n - 1) % n,
        Some(i) => (i + 1) % n,
        None if back => n - 1,
        None => 0,
    };
    ids.get(i).cloned()
}

/// The keyboard on the widget lane, unlifted: Left and Right move the
/// selection one place, Delete takes it off the bar.
fn keyed(b: &mut Bar, order: &[String], key: Stroke) -> Option<Landing> {
    match key {
        Stroke::Left | Stroke::Right => {
            let id = pick(b)?;
            swapped(order, &id, key == Stroke::Right).map(|o| Landing::Order(o, id))
        }
        Stroke::Delete => pick(b).filter(|id| order.contains(id)).map(Landing::Remove),
        _ => None,
    }
}

/// The keyboard on a lifted selection: Left and Right walk it, and the
/// walk is the lift's own until it is dropped. `true` when it moved.
fn walk(l: &mut Lift, key: Stroke) -> bool {
    match key {
        Stroke::Left | Stroke::Right => match swapped(&l.order, &l.id, key == Stroke::Right) {
            Some(o) => {
                l.order = o;
                true
            }
            None => false,
        },
        _ => false,
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
        _ => None,
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
/// Delete, Space, Tab and Ctrl+Z are the field's, in either lane.
fn gated(key: Stroke, typing: bool) -> Option<Stroke> {
    (!typing).then_some(key)
}

/// Why `edits` cannot be written here, in words for the status line, or
/// `None` when they can. A refusal is explained where the move was made,
/// never silently dropped: the bar's widget lists exist only while the
/// taskbar add-on is on, and a key this compositor did not report is a
/// stand-in that no write reaches.
fn refusal(app: &App, edits: &[(String, Value)]) -> Option<String> {
    let widgets = edits.iter().any(|(p, _)| p.starts_with("bar.widgets."));
    if widgets && !crate::addons::taskbar_widgets_on(app) {
        return Some(
            "Not saved: widgets come with the taskbar add-on, which is off. Turn it on under Add-ons."
                .to_owned(),
        );
    }
    edits
        .iter()
        .find(|(p, _)| {
            app.key(p)
                .is_some_and(|k| k.source.as_deref() == Some("stand-in"))
        })
        .map(|(p, _)| format!("Not saved: this compositor does not know {p} yet."))
}

/// Write `edits` as one commit, and remember how to take them back. On a
/// refusal the status line and the toast say why; `false` then, so a drag
/// can fly home.
fn land(app: &mut App, edits: Vec<(String, Value)>, note: String) -> bool {
    if let Some(why) = refusal(app, &edits) {
        app.bar.said = Some(why.clone());
        app.undo.say(why);
        return false;
    }
    let before: Vec<(String, Value)> = edits
        .iter()
        .map(|(p, _)| (p.clone(), app.key(p).map_or(Value::Null, |k| k.value.clone())))
        .collect();
    let restore = Restore {
        edits: before,
        select: app.bar.selected.clone(),
        tray: app.tray_sel.clone(),
    };
    match app.write_batch(&edits) {
        Ok(()) => {
            app.bar.said = Some(note.clone());
            app.undo.push(note, restore);
            true
        }
        Err(p) => {
            let why = format!("Not saved: {}", p.headline());
            app.bar.said = Some(why.clone());
            app.undo.say(why);
            false
        }
    }
}

/// Take the newest change back, in one commit.
fn undo(app: &mut App) {
    let Some(e) = app.undo.pop() else {
        app.undo.say("Nothing to undo");
        return;
    };
    match app.write_batch(&e.restore.edits) {
        Ok(()) => {
            app.bar.lift = None;
            app.bar.selected = e.restore.select;
            app.tray_sel = e.restore.tray;
            let note = format!("Undone: {}", e.note);
            app.bar.said = Some(note.clone());
            app.undo.say(note);
        }
        Err(p) => {
            let why = format!("Could not undo: {}", p.headline());
            app.bar.said = Some(why.clone());
            app.undo.say(why);
        }
    }
}

fn lane_name(l: Lane) -> &'static str {
    match l {
        Lane::Taskbar => "the bar",
        Lane::Overflow => "the drawer",
        Lane::Hidden => "hidden",
    }
}

/// Do what a drop or a key decided. `false` when nothing was written.
fn apply(app: &mut App, l: Landing) -> bool {
    match l {
        Landing::Order(o, id) => {
            let added = !order(app).contains(&id);
            let note = if added {
                format!("Added {}", bp::title(&id))
            } else {
                let at = o.iter().position(|x| *x == id).unwrap_or(0);
                format!("Moved {} to {} of {}", bp::title(&id), at + 1, o.len())
            };
            land(app, vec![(ORDER.to_owned(), json!(o))], note)
        }
        Landing::Remove(id) => remove(app, &id),
        Landing::Tray(id, lane, at) => {
            let w = app.tray().dropped(&id, lane, at);
            let edits = app.tray_edits(w);
            let ok = land(app, edits, format!("Moved {id} to {}", lane_name(lane)));
            if ok {
                app.tray_sel = Some(id);
            }
            ok
        }
    }
}

/// Take `id` off the bar, and out of the important list with it, in one
/// commit. When it was the selection, its neighbour takes it, so a run of
/// removals is a run of presses on one place.
fn remove(app: &mut App, id: &str) -> bool {
    let before = order(app);
    let at = before.iter().position(|x| x == id).unwrap_or(0);
    let o: Vec<String> = before.iter().filter(|x| *x != id).cloned().collect();
    let edits = removal(&before, &important(app), id);
    let was_selected = app.bar.selected.as_deref() == Some(id);
    if !land(app, edits, format!("Removed {}", bp::title(id))) {
        return false;
    }
    if was_selected {
        app.bar.editor = None;
        app.bar.editing = false;
        app.bar.selected = o.get(at.min(o.len().saturating_sub(1))).cloned();
        adopt_selection(app);
    }
    true
}

/// The one commit that takes `id` off the bar: the order without it, and
/// the important list without it when it was there.
fn removal(order: &[String], important: &[String], id: &str) -> Vec<(String, Value)> {
    let o: Vec<&String> = order.iter().filter(|x| *x != id).collect();
    let mut edits = vec![(ORDER.to_owned(), json!(o))];
    if important.iter().any(|x| x == id) {
        let imp: Vec<&String> = important.iter().filter(|x| *x != id).collect();
        edits.push((IMPORTANT.to_owned(), json!(imp)));
    }
    edits
}

/// Fly `what` back to its place from `from`, unless motion is off.
fn home(app: &mut App, what: Payload, from: Point, now: Instant) {
    if motion(app).snaps() {
        app.bar.homing = None;
        return;
    }
    let mut t = Animated::new(
        0.0,
        Motion {
            enabled: true,
            curve: Curve::EaseOut,
            duration: Duration::from_millis(pace::HOMING_MS),
        },
    );
    t.set_target(1.0, now);
    app.bar.homing = Some(Homing {
        what,
        t,
        from: Some(from),
    });
}

/// How far a drag held at `at` (layout space) scrolls the pane per frame:
/// nothing in the middle, up to [`canvas::AUTOSCROLL_STEP`] at an edge.
fn edge_speed(viewport: Option<(Rectangle, f32, f32)>, at: Point) -> f32 {
    let Some((b, offset, _)) = viewport else {
        return 0.0;
    };
    let y = at.y - offset;
    let edge = canvas::AUTOSCROLL_EDGE;
    let top = b.y + edge;
    let bottom = b.y + b.height - edge;
    if y < top {
        -canvas::AUTOSCROLL_STEP * ((top - y) / edge).min(1.0)
    } else if y > bottom {
        canvas::AUTOSCROLL_STEP * ((y - bottom) / edge).min(1.0)
    } else {
        0.0
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
    let mode = mode(app);
    let mut ids = shown_order(app);
    // `de` puts the history bell before the clock; it is not in the order
    // and cannot be moved, but it takes its room.
    if mode == Mode::De {
        let at = ids.iter().position(|x| x == "clock").unwrap_or(ids.len());
        ids.insert(at, bp::HISTORY.to_owned());
    }
    let imp = important(app);
    let k = knobs(app);
    let cells: Vec<Cell> = ids
        .iter()
        .map(|id| match id.as_str() {
            bp::HISTORY => Cell {
                span: ec_ui::widget::ShellSpan {
                    core: bar::MARK,
                    revealed: 0.0,
                },
                important: true,
            },
            _ => Cell {
                span: bp::span(id, &k),
                important: imp.contains(id),
            },
        })
        .collect();
    app.bar.min_inner = bp::min_inner(mode, app.bar.windows, &cells);
    let s = bp::solve_in(mode, app.bar.inner(), app.bar.windows, &cells);
    let b = &mut app.bar;

    for (i, id) in ids.iter().enumerate() {
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
    for c in b.cells.iter_mut().filter(|c| !ids.contains(&c.id)) {
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

/// Open the pane's debug states: selection, window count, an open editor,
/// a drag held mid-flight, a lift, a missed drop flying home, a toast.
#[cfg(debug_assertions)]
pub fn preview_env(app: &mut App) {
    let var = |k: &str| std::env::var(k).ok();
    if let Some(m) = var("SETTINGS_PREVIEW_MODE") {
        app.bar.mode = Some(Mode::parse(&m));
    }
    if let Some(id) = var("SETTINGS_PREVIEW_WIDGET") {
        go_to(app, id);
    }
    if let Some(n) = var("SETTINGS_PREVIEW_WINDOWS").and_then(|n| n.parse().ok()) {
        app.bar.windows = canvas::WINDOWS_MAX.min(n);
    }
    // Live tray apps with an icon name each, as the feed would report them:
    // `id=icon,id=icon`.
    if let Some(v) = var("SETTINGS_PREVIEW_TRAY") {
        let mut ids = Vec::new();
        for pair in v.split(',') {
            let (id, icon) = pair.split_once('=').unwrap_or((pair, pair));
            let art = match crate::icons::symbolic(icon) {
                Some(p) => ec_ui::widget::Art::Svg(p),
                None => crate::icons::tray(icon, "", None),
            };
            ids.push(id.to_owned());
            app.tray_art.push((id.to_owned(), art));
        }
        app.tray_live = Some(Some(ids));
        app.bar.tray_preview = true;
    }
    // A drag held mid-flight: `<id>@<slot>` a bar cell over a slot,
    // `+<id>@<slot>` a shelf widget over one, `-<id>` a bar cell over the
    // shelf, `tray:<id>@<bar|drawer|hidden>` a tray entry over a lane. The
    // pointer is placed once the targets are laid out, so the real hit test
    // decides what is drawn.
    if let Some(v) = var("SETTINGS_PREVIEW_DRAG") {
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
    // A missed drop halfway home: `<id>` a bar cell, `+<id>` a shelf widget,
    // let go of below the shelf.
    if let Some(v) = var("SETTINGS_PREVIEW_HOMING") {
        let what = match v.strip_prefix('+') {
            Some(id) => Payload::Pick(id.into()),
            None => Payload::Cell(v.clone()),
        };
        let mut t = Animated::new(0.0, Motion::DEFAULT);
        t.snap(0.5);
        app.bar.homing = Some(Homing { what, t, from: None });
        app.bar.said = Some("Dropped where nothing takes it: it went back.".into());
    }
    // A lifted selection walked `n` places: `<id>@<n>`, negative is left.
    if let Some((id, n)) = var("SETTINGS_PREVIEW_LIFT").and_then(|v| {
        let (id, n) = v.split_once('@')?;
        Some((id.to_owned(), n.parse::<i32>().ok()?))
    }) {
        go_to(app, id.clone());
        let o = order(app);
        let mut l = Lift {
            id: id.clone(),
            order: o.clone(),
            from: o,
        };
        let key = if n < 0 { Stroke::Left } else { Stroke::Right };
        for _ in 0..n.unsigned_abs() {
            walk(&mut l, key);
        }
        app.bar.lift = Some(l);
        app.bar.said = Some(lift_note(&id));
    }
    if let Some(note) = var("SETTINGS_PREVIEW_TOAST") {
        app.undo.push(
            note.clone(),
            Restore {
                edits: Vec::new(),
                select: None,
                tray: None,
            },
        );
        app.bar.said = Some(note);
    }
    match var("SETTINGS_PREVIEW_EDITOR").as_deref() {
        Some("new") => {
            let _ = update(app, Msg::New);
        }
        // An argument longer than the panel is wide, and a delete waiting
        // on its confirm.
        Some("long") => {
            let w = Widget {
                name: "probe".into(),
                kind: ec_ipc::WidgetKind::Exec {
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
            app.bar.editing = true;
            app.bar.selected = Some(format!("{}probe", bp::CUSTOM));
        }
        // A block the compositor refuses: an interval under its floor.
        Some("invalid") => {
            let _ = update(app, Msg::New);
            if let Some(ed) = app.bar.editor.as_mut() {
                ed.name = "weather".into();
                ed.argv_mut(Slot::Command).args =
                    vec!["curl".into(), "-s".into(), "wttr.in/?format=%t".into()];
                ed.argv_mut(Slot::Click).args = vec!["xdg-open".into(), "https://wttr.in".into()];
            }
            let _ = update(app, Msg::Interval("10".into()));
        }
        // A draft with changes, and a click on another widget: the ask.
        Some("leaving") => {
            let _ = update(app, Msg::New);
            let _ = update(app, Msg::Name("weather".into()));
            select(app, "clock".into());
        }
        _ => {}
    }
    if var("SETTINGS_PREVIEW_SWEEP").is_some() {
        app.bar.sweep = Some(Instant::now());
    }
    // A withheld command widget, as abyss would report it.
    if let Some(name) = var("SETTINGS_PREVIEW_PENDING") {
        if !app.bar.statuses.iter().any(|s| s.name == name) {
            app.bar.statuses.push(WidgetStatus {
                name,
                approval: Approval::Pending,
                premade: false,
                altered: false,
            });
        }
    }
    match var("SETTINGS_PREVIEW_PRESS").as_deref() {
        Some("review") => {
            let pending = app.bar.statuses.iter().find(|s| s.approval == Approval::Pending);
            if let Some(name) = pending.map(|s| s.name.clone()) {
                let _ = update(app, Msg::Review(name));
            }
        }
        Some("review-denied") => {
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
        _ => {}
    }
}

fn lift_note(id: &str) -> String {
    format!(
        "{} lifted: \u{2190} \u{2192} to move it, Space to drop, Esc to put it back",
        bp::title(id)
    )
}

/// Whether the editor holds work a switch would lose.
fn draft_open(b: &Bar) -> bool {
    b.editor.as_ref().is_some_and(|e| e.dirty || e.is_new())
}

/// Select `id`, unless that would throw away a draft: then the inspector
/// asks first ([`Leave`]).
fn select(app: &mut App, id: String) {
    if draft_open(&app.bar) && app.bar.selected.as_deref() != Some(id.as_str()) {
        app.bar.leaving = Some(Leave::To(id));
        return;
    }
    go_to(app, id);
}

/// Select `id` with no questions.
fn go_to(app: &mut App, id: String) {
    app.bar.editor = id
        .strip_prefix(bp::CUSTOM)
        .and_then(|name| app.bar.customs.iter().find(|w| w.name == name))
        .map(Editor::from_widget);
    app.bar.selected = Some(id);
    app.bar.offer = None;
    app.bar.editing = false;
    app.bar.leaving = None;
}

/// Put a previewed drag's pointer where it names, once the targets it names
/// are laid out.
fn place_preview(app: &mut App) {
    // A previewed homing flies from under the shelf.
    if let (Some(h), Some(r)) = (app.bar.homing.as_mut(), app.bar.zones.picker) {
        if h.from.is_none() {
            h.from = Some(Point::new(r.center_x(), r.y + r.height + bar::PILL_H));
        }
    }
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

/// Keep the selection's editor in line with it. Nothing is selected until
/// the owner selects something (a click, Tab); a custom widget selected must
/// open its block like a click would. Runs with every sync.
pub fn adopt_selection(app: &mut App) {
    adopt(&mut app.bar);
}

/// [`adopt_selection`] on the pane's state alone. A draft (edited, or a new
/// block) is never replaced, and an editor already on the widget stays: a
/// config change reaches it through [`follow_config`].
fn adopt(b: &mut Bar) {
    if draft_open(b) {
        return;
    }
    let Some(name) = b.selected.as_deref().and_then(|i| i.strip_prefix(bp::CUSTOM)) else {
        return;
    };
    if b.editor.as_ref().and_then(|e| e.original.as_deref()) == Some(name) {
        return;
    }
    b.editor = b.customs.iter().find(|w| w.name == name).map(Editor::from_widget);
}

/// The selection, if there is one and no new block is being drafted.
fn selected(app: &App) -> Option<String> {
    pick(&app.bar)
}

/// [`selected`] on the pane's state alone.
fn pick(b: &Bar) -> Option<String> {
    if b.editor.as_ref().is_some_and(Editor::is_new) {
        return None;
    }
    b.selected.clone()
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
    app.bar.editing = false;
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
            // A drag held near the pane's edge scrolls it, and the point in
            // hand moves with the page under a pointer that stays put.
            let speed = app.bar.autoscroll;
            if let (Some(d), Some((_, offset, max))) = (app.bar.drag.as_mut(), app.viewport.as_mut()) {
                let next = (*offset + speed).clamp(0.0, max.max(0.0));
                let dy = next - *offset;
                if dy != 0.0 {
                    *offset = next;
                    d.at.y += dy;
                    return iced::widget::operation::scroll_by(
                        crate::app::SCROLL,
                        iced::widget::scrollable::AbsoluteOffset { x: 0.0, y: dy },
                    );
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
        Msg::Select(id) => {
            app.bar.lift = None;
            select(app, id);
        }
        Msg::Move(what, at) => {
            app.bar.preview = None;
            if !app.bar.escaped {
                // Picking something up puts a keyboard lift back.
                app.bar.lift = None;
                app.bar.homing = None;
                app.bar.autoscroll = edge_speed(app.viewport, at);
                app.bar.drag = Some(Drag { what, at });
            }
        }
        Msg::Release(_) => {
            app.bar.escaped = false;
            app.bar.autoscroll = 0.0;
            let Some(d) = app.bar.drag.take() else {
                return Task::none();
            };
            // The drag's own point, not the release's: an edge scroll moved
            // the page under a pointer that did not move.
            let to = target(app, &d.what, d.at);
            let landed = match landing(&order(app), &d.what, to) {
                Some(l) => apply(app, l),
                None => {
                    if to.is_none() {
                        app.bar.said = Some("Dropped where nothing takes it: it went back.".into());
                    }
                    false
                }
            };
            if landed {
                // What moved stays selected, so the next key moves it again.
                match &d.what {
                    Payload::Cell(id) => app.bar.selected = Some(id.clone()),
                    Payload::Pick(id) => select(app, id.clone()),
                    Payload::Tray(_) => {}
                }
            } else {
                home(app, d.what, d.at, now);
            }
        }
        Msg::Cancel => {
            if let Some(d) = app.bar.drag.take() {
                home(app, d.what, d.at, now);
            }
            app.bar.escaped = false;
            app.bar.autoscroll = 0.0;
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
            if let Some(d) = app.bar.drag.take() {
                app.bar.escaped = true;
                home(app, d.what, d.at, now);
            } else if let Some(l) = app.bar.lift.take() {
                app.bar.said = Some(format!("{} put back", bp::title(&l.id)));
            } else if app.bar.leaving.take().is_none() && app.bar.editing && !draft_open(&app.bar) {
                app.bar.editing = false;
            }
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
            if k == Stroke::Undo {
                undo(app);
            } else if app.page == Page::BarTray {
                tray_key(app, k);
            } else {
                widget_key(app, k);
            }
        }
        Msg::Step(later) => {
            let o = order(app);
            if let Some(l) = keyed(&mut app.bar, &o, if later { Stroke::Right } else { Stroke::Left }) {
                apply(app, l);
            }
        }
        Msg::Remove => {
            if let Some(id) = selected(app).filter(|id| order(app).contains(id)) {
                remove(app, &id);
            }
        }
        Msg::Important(on) => {
            let Some(id) = selected(app) else {
                return Task::none();
            };
            let mut imp = important(app);
            imp.retain(|x| *x != id);
            if on {
                imp.push(id.clone());
            }
            let note = if on {
                format!("{} always keeps its full size", bp::title(&id))
            } else {
                format!("{} gives way when the bar is crowded", bp::title(&id))
            };
            land(app, vec![(IMPORTANT.to_owned(), json!(imp))], note);
        }
        Msg::Add(id) => {
            let mut o = order(app);
            if !o.contains(&id) {
                o.push(id.clone());
                if !apply(app, Landing::Order(o, id.clone())) {
                    return Task::none();
                }
            }
            select(app, id);
        }
        // Widget writes are refused while the hook is off; the controls that
        // start one say so, and these are the ones a debug env reaches.
        Msg::New | Msg::Save | Msg::Delete | Msg::Review(_) if !crate::addons::taskbar_widgets_on(app) => {
            app.bar.said = refusal(app, &[(ORDER.to_owned(), Value::Null)]);
        }
        Msg::New => {
            if draft_open(&app.bar) {
                app.bar.leaving = Some(Leave::New);
                return Task::none();
            }
            app.bar.editor = Some(Editor::new());
            app.bar.editing = true;
            app.bar.more = false;
            app.bar.offer = None;
        }
        Msg::Edit => {
            adopt_selection(app);
            app.bar.editing = app.bar.editor.is_some();
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
        Msg::More(on) => app.bar.more = on,
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
                app.bar.editing = false;
                app.bar.selected = None;
            }
        }
        Msg::Keep => {
            if let Some(ed) = app.bar.editor.as_mut() {
                ed.confirm_delete = false;
            }
        }
        Msg::Close => {
            if draft_open(&app.bar) {
                app.bar.leaving = Some(Leave::Close);
                return Task::none();
            }
            app.bar.editing = false;
        }
        Msg::Stay => app.bar.leaving = None,
        Msg::Discard => {
            let to = app.bar.leaving.take();
            app.bar.editor = None;
            app.bar.editing = false;
            match to {
                Some(Leave::To(id)) => go_to(app, id),
                Some(Leave::New) => {
                    app.bar.editor = Some(Editor::new());
                    app.bar.editing = true;
                }
                Some(Leave::Close) | None => adopt_selection(app),
            }
        }
        Msg::Offer(yes) => {
            if let (true, Some(name)) = (yes, app.bar.offer.take()) {
                let id = format!("{}{}", bp::CUSTOM, name);
                let mut o = order(app);
                if !o.contains(&id) {
                    o.push(id.clone());
                    apply(app, Landing::Order(o, id));
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

/// A key on the Widgets page.
fn widget_key(app: &mut App, k: Stroke) {
    let o = order(app);
    if let Some(l) = app.bar.lift.as_mut() {
        match k {
            Stroke::Left | Stroke::Right => {
                if walk(l, k) {
                    let at = l.order.iter().position(|x| *x == l.id).unwrap_or(0);
                    app.bar.said = Some(format!("{}: {} of {}", bp::title(&l.id), at + 1, l.order.len()));
                }
            }
            Stroke::Space | Stroke::Enter => {
                let Some(l) = app.bar.lift.take() else {
                    return;
                };
                if l.order == l.from {
                    app.bar.said = Some(format!("{} dropped where it was", bp::title(&l.id)));
                } else {
                    apply(app, Landing::Order(l.order, l.id));
                }
            }
            _ => {}
        }
        return;
    }
    match k {
        Stroke::Tab | Stroke::BackTab => {
            if let Some(id) = cycle(&o, app.bar.selected.as_deref(), k == Stroke::BackTab) {
                select(app, id);
            }
        }
        Stroke::Space => {
            if let Some(id) = selected(app).filter(|id| o.contains(id)) {
                app.bar.said = Some(lift_note(&id));
                app.bar.lift = Some(Lift {
                    id,
                    order: o.clone(),
                    from: o,
                });
            }
        }
        _ => {
            if let Some(l) = keyed(&mut app.bar, &o, k) {
                apply(app, l);
            }
        }
    }
}

/// A key on the Tray page: Tab walks every entry, the rest move it.
fn tray_key(app: &mut App, k: Stroke) {
    let t = app.tray();
    match k {
        Stroke::Tab | Stroke::BackTab => {
            let mut ids = t.taskbar();
            ids.extend(t.in_lane(Lane::Overflow));
            ids.extend(t.in_lane(Lane::Hidden));
            app.tray_sel = cycle(&ids, app.tray_sel.as_deref(), k == Stroke::BackTab);
        }
        _ => {
            let Some(id) = app.tray_sel.clone() else {
                return;
            };
            if let Some(w) = tray_keyed(&t, &id, k) {
                let edits = app.tray_edits(w);
                land(app, edits, format!("Moved {id}"));
            }
        }
    }
}

// ------------------------------------------------------------------ view
//
// Accent ledger: the one live yellow on the Widgets page is the selected
// widget's name in the inspector. The bar picture is neutral: the selection
// on it is the focused cell's brighter glass and crisp rim, never gold; the
// drop mark is white ink; a drop well answers in ground and edge only. On
// the Tray page the one yellow is the selected entry's label.

mod canvas_view;
mod inspector;
mod tray_view;

fn bar_msg(m: Msg) -> Message {
    Message::Bar(m)
}

/// Every block of `page` after the header.
pub fn blocks(app: &App, page: Page) -> Vec<Element<'_, Message, Theme>> {
    match page {
        Page::BarAppearance => vec![appearance(app), crate::pinned::view(app)],
        Page::BarFolding => vec![keys_panel(app, FOLDING.to_vec())],
        Page::BarTray => vec![tray_view::tray(app)],
        Page::BarMotion => vec![motion_band(app)],
        _ => widgets(app),
    }
}

/// The Widgets page: the bar and the shelf it is built from, then the
/// inspector for whatever is selected. Two silhouettes, a canvas and a
/// sheet; never a stack of panels.
fn widgets(app: &App) -> Vec<Element<'_, Message, Theme>> {
    let mut out = Vec::new();
    if !crate::addons::taskbar_widgets_on(app) {
        out.push(edge_note(
            "Taskbar add-on is off",
            "Widgets come with the taskbar add-on. Without it the compositor has no widget list,              so moves here cannot be saved; the bar's own settings still apply.",
            color::neutral(),
        ));
    }
    out.push(canvas_view::hero(app));
    out.push(inspector::inspector(app));
    out
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

fn caption_prose<'a>(t: &str) -> Element<'a, Message, Theme> {
    text(t.to_owned())
        .font(font::UI)
        .size(size::BODY_SMALL)
        .style(theme::text_tertiary)
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

fn padded<'a>(e: impl Into<Element<'a, Message, Theme>>) -> Element<'a, Message, Theme> {
    container(e)
        .padding([space::ROW_Y, space::CARD])
        .width(Length::Fill)
        .into()
}

/// [`ec_ui::widget::key_hints`], wrapped: a narrow pane drops the last
/// hints to a second line rather than clip them.
fn wrapped_hints<'a>(hints: &[(&str, &str)]) -> Element<'a, Message, Theme> {
    hints
        .iter()
        .fold(
            Row::new().spacing(space::HINT_GAP).align_y(Alignment::Center),
            |line, &(key, verb)| line.push(ec_ui::widget::key_hint(key, verb)),
        )
        .wrap()
        .vertical_spacing(space::PILL_GAP)
        .into()
}

fn key_row<'a>(app: &'a App, path: &str) -> Option<Element<'a, Message, Theme>> {
    let key = app.key(path)?;
    Some(crate::app::setting_row(app, &label(key), key))
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

/// The span the picture draws: the core alone. Nothing is ever revealed in
/// the picture, and the shell anchors its body right, so a revealed width it
/// is not given would show as empty room where the core should be.
fn drawn_span(c: &CellAnim) -> ec_ui::widget::ShellSpan {
    ec_ui::widget::ShellSpan {
        core: c.cell.span.core,
        revealed: 0.0,
    }
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
            container(controls).width(Length::Fixed(space::CONTROL_COL_W)),
            column![
                glide_track(app.bar.glide.value(), "demo"),
                caption("slides on each change".into()),
            ]
            .spacing(space::PILL_GAP)
            .align_x(Alignment::Center),
        ]
        .spacing(space::BLOCK)
        .align_y(Alignment::Center)
        .wrap()
        .vertical_spacing(space::BLOCK),
    )
    .padding([0.0, space::CARD])
    .width(Length::Fill)
    .into()
}

/// One panel of `paths`' rows, in that order.
fn keys_panel<'a>(app: &'a App, paths: Vec<&str>) -> Element<'a, Message, Theme> {
    let mut c = Column::new();
    for p in paths {
        if let Some(r) = key_row(app, p) {
            c = c.push(r);
        }
    }
    panel(app.glass_radius, app.hairline_cards, c.width(Length::Fill))
}

/// How the bar looks — and, under a hairline, any key the compositor puts on
/// this page that no block here names, so a new one is never unreachable.
fn appearance(app: &App) -> Element<'_, Message, Theme> {
    let mut body = Column::new();
    for p in APPEARANCE {
        if let Some(r) = key_row(app, p) {
            body = body.push(r);
        }
    }
    let rest: Vec<&str> = app
        .rows
        .iter()
        .filter(|k| crate::pane::page_of(&k.path) == Some(Page::BarAppearance) && !claimed(&k.path))
        .map(|k| k.path.as_str())
        .collect();
    if !rest.is_empty() {
        body = body.push(hairline()).push(padded(micro_label("other")));
        for p in rest {
            if let Some(r) = key_row(app, p) {
                body = body.push(r);
            }
        }
    }
    panel(app.glass_radius, app.hairline_cards, body.width(Length::Fill))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn widget(name: &str) -> Widget {
        Widget {
            name: name.into(),
            kind: ec_ipc::WidgetKind::Exec {
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
    fn nothing_is_selected_until_the_owner_selects() {
        let mut b = bar_with(&["weather"]);
        adopt(&mut b);
        assert_eq!(pick(&b), None);
        assert!(b.editor.is_none());
    }

    #[test]
    fn a_selected_custom_widget_opens_its_block() {
        let mut b = bar_with(&["weather"]);
        b.selected = Some("custom:weather".into());
        adopt(&mut b);
        assert_eq!(open(&b), Some("weather"));
    }

    #[test]
    fn a_selected_builtin_opens_no_editor() {
        let mut b = bar_with(&["weather"]);
        b.selected = Some("clock".into());
        adopt(&mut b);
        assert!(b.editor.is_none());
    }

    #[test]
    fn a_draft_is_never_replaced() {
        let mut b = bar_with(&["weather", "load"]);
        b.selected = Some("custom:weather".into());
        adopt(&mut b);
        if let Some(e) = b.editor.as_mut() {
            e.name = "weather2".into();
            e.dirty = true;
        }
        assert!(draft_open(&b));
        b.selected = Some("custom:load".into());
        adopt(&mut b);
        assert_eq!(open(&b), Some("weather"));
        assert_eq!(b.editor.as_ref().map(|e| e.name.as_str()), Some("weather2"));

        // A new block hides the selection and is a draft too.
        b.editor = Some(Editor::new());
        assert!(draft_open(&b));
        assert_eq!(pick(&b), None);
        adopt(&mut b);
        assert!(b.editor.as_ref().is_some_and(Editor::is_new));
    }

    #[test]
    fn a_move_past_either_end_writes_nothing() {
        let o = ids(&["clock", "tray"]);
        assert_eq!(swapped(&o, "clock", false), None);
        assert_eq!(swapped(&o, "tray", true), None);
        assert_eq!(swapped(&o, "clock", true), Some(ids(&["tray", "clock"])));
        assert_eq!(swapped(&o, "absent", true), None);
    }

    #[test]
    fn tab_cycles_and_wraps_both_ways() {
        let o = ids(&["a", "b", "c"]);
        assert_eq!(cycle(&o, None, false).as_deref(), Some("a"));
        assert_eq!(cycle(&o, None, true).as_deref(), Some("c"));
        assert_eq!(cycle(&o, Some("a"), false).as_deref(), Some("b"));
        assert_eq!(cycle(&o, Some("c"), false).as_deref(), Some("a"));
        assert_eq!(cycle(&o, Some("a"), true).as_deref(), Some("c"));
        // A selection no longer on the bar starts over.
        assert_eq!(cycle(&o, Some("gone"), false).as_deref(), Some("a"));
        assert_eq!(cycle(&[], None, false), None);
    }

    #[test]
    fn a_lift_walks_its_own_order_and_stops_at_the_ends() {
        let o = ids(&["a", "b", "c"]);
        let mut l = Lift {
            id: "b".into(),
            order: o.clone(),
            from: o.clone(),
        };
        assert!(walk(&mut l, Stroke::Right));
        assert_eq!(l.order, ids(&["a", "c", "b"]));
        assert!(!walk(&mut l, Stroke::Right));
        assert!(walk(&mut l, Stroke::Left));
        assert!(walk(&mut l, Stroke::Left));
        assert_eq!(l.order, ids(&["b", "a", "c"]));
        assert!(!walk(&mut l, Stroke::Left));
        assert!(!walk(&mut l, Stroke::Up));
        // The walk never touches where it came from: Esc puts that back.
        assert_eq!(l.from, o);
    }

    #[test]
    fn a_removal_is_one_commit_of_order_and_importance() {
        let o = ids(&["a", "b", "c"]);
        let e = removal(&o, &ids(&["b"]), "b");
        assert_eq!(
            e,
            vec![
                (ORDER.to_owned(), json!(["a", "c"])),
                (IMPORTANT.to_owned(), json!([])),
            ]
        );
        // Not important: the order alone.
        let e = removal(&o, &ids(&["c"]), "a");
        assert_eq!(e, vec![(ORDER.to_owned(), json!(["b", "c"]))]);
    }

    #[test]
    fn a_drag_scrolls_only_near_an_edge() {
        let vp = Some((
            Rectangle {
                x: 0.0,
                y: 100.0,
                width: 400.0,
                height: 500.0,
            },
            200.0,
            1000.0,
        ));
        // The middle of the viewport, in content space (offset 200).
        assert_eq!(edge_speed(vp, at(10.0, 550.0)), 0.0);
        // Right at the top edge: full speed up.
        assert_eq!(edge_speed(vp, at(10.0, 300.0)), -canvas::AUTOSCROLL_STEP);
        // Halfway into the bottom band: half speed down.
        let half = 100.0 + 500.0 - canvas::AUTOSCROLL_EDGE / 2.0 + 200.0;
        let v = edge_speed(vp, at(10.0, half));
        assert!((v - canvas::AUTOSCROLL_STEP / 2.0).abs() < 0.01, "{v}");
        assert_eq!(edge_speed(None, at(10.0, 0.0)), 0.0);
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
            Some(Landing::Order(ids(&["b", "a", "c", "d"]), "b".into()))
        );
        assert_eq!(
            landing(&o, &cell, Some(Target::Bar(4))),
            Some(Landing::Order(ids(&["a", "c", "d", "b"]), "b".into()))
        );
        assert_eq!(
            landing(&o, &cell, Some(Target::Bar(3))),
            Some(Landing::Order(ids(&["a", "c", "b", "d"]), "b".into()))
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
            Some(Landing::Order(ids(&["a", "x", "b"]), "x".into()))
        );
        assert_eq!(
            landing(&o, &Payload::Pick("x".into()), Some(Target::Bar(9))),
            Some(Landing::Order(ids(&["a", "b", "x"]), "x".into()))
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
            Some(Landing::Order(ids(&["b", "a", "c"]), "b".into()))
        );
        assert_eq!(
            keyed(&mut b, &o, Stroke::Right),
            Some(Landing::Order(ids(&["a", "c", "b"]), "b".into()))
        );
        assert_eq!(
            keyed(&mut b, &o, Stroke::Delete),
            Some(Landing::Remove("b".into()))
        );
        assert_eq!(keyed(&mut b, &o, Stroke::Up), None);
        // With nothing chosen, keys do nothing: no hidden default.
        b.selected = None;
        assert_eq!(keyed(&mut b, &o, Stroke::Left), None);
        assert_eq!(keyed(&mut b, &o, Stroke::Delete), None);
        // A selection on the shelf, not the bar, cannot be removed.
        b.selected = Some("load".into());
        assert_eq!(keyed(&mut b, &o, Stroke::Delete), None);
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
