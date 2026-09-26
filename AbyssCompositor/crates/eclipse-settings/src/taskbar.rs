// SPDX-License-Identifier: AGPL-3.0-only
//! The Taskbar pane (ADR 0065, D-05 §4): a live picture of the bar, the
//! widget lane under it, one sheet for whichever widget is selected, the bar's
//! motion, and the bar's own appearance and folding.
//!
//! Every `bar.*` key has a control here, and every one is still a schema row:
//! the grouping below is the one place this crate names keys by hand, and a
//! key it does not name lands in the "other" column rather than vanishing.
//!
//! The picture is solved by [`crate::bar_preview`] and drawn from the bar's
//! own primitives. Its cells glide with [`Animated`] under the configured
//! `bar.motion`, and the frame clock runs only while one of them moves.

use std::time::{Duration, Instant};

use iced::widget::{
    button, column, container, pick_list, row, sensor, text, text_input, Column, Row, Space, Stack,
};
use iced::{Alignment, Element, Length, Size, Task, Theme};
use serde_json::{json, Value};

use eclipse_ipc::{Widget, WidgetOp};
use eclipse_ui::motion::{Animated, Curve, Motion};
use eclipse_ui::theme;
use eclipse_ui::tokens::{bar, canvas, color, font, size, space};
use eclipse_ui::widget::{
    arg_chip, art_thumb, bar_cell_frame, bar_sheet, battery_gauge, big_value, chip, config_error, drag_bar,
    elide, glide_track, hairline, inset, list_row, mark, micro_label, mini_meter, outline, panel, pill, pin,
    placed, ring, track_label, value as mono, viz_bars, widget_shell, widget_tile, Grip, NumericSlider,
    Toggle,
};

use crate::app::{App, Message};
use crate::bar_preview::{self as bp, Cell, Knobs, Rung, Solved};
use crate::editor::{Editor, Field, Kind, Slot, SOURCES};
use crate::schema::Row as Key;
use crate::tray::Lane;

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
    (
        match o.len() {
            1 => "1 widget".to_owned(),
            n => format!("{n} widgets"),
        },
        format!("{pinned} never compress · {motion}"),
    )
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
    /// A lane tile's grip was pressed. The drag that follows carries no id:
    /// iced keeps the grip's press by tree position, so the id is taken once.
    Grab(String),
    Drag(f32),
    Drop,
    Shift(bool),
    Remove,
    Important(bool),
    Picker,
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
}

#[derive(Debug, Clone)]
struct Drag {
    id: String,
    from: usize,
    to: usize,
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
    picker: bool,
    pub(crate) customs: Vec<Widget>,
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
            picker: false,
            customs: Vec::new(),
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

    /// The width of a lane tile when the lane holds `n`.
    fn tile_w(&self, n: usize) -> f32 {
        let n = n.max(1) as f32;
        let room = self.inner() - 2.0 * canvas::DROP_MARK_W;
        ((room - (n - 1.0) * canvas::TILE_GAP) / n).clamp(canvas::TILE_MIN_W, canvas::TILE_W)
    }
}

/// The order the picture draws: the configured one, or the one a drag in
/// progress would write.
fn drawn_order(app: &App) -> Vec<String> {
    let mut o = order(app);
    if let Some(d) = &app.bar.drag {
        if let Some(at) = o.iter().position(|x| *x == d.id) {
            let id = o.remove(at);
            o.insert(d.to.min(o.len()), id);
        }
    }
    o
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
    let order = drawn_order(app);
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
    app.bar.picker = std::env::var_os("SETTINGS_PREVIEW_PICKER").is_some();
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
}

fn select(app: &mut App, id: String) {
    app.bar.editor = id
        .strip_prefix(bp::CUSTOM)
        .and_then(|name| app.bar.customs.iter().find(|w| w.name == name))
        .map(Editor::from_widget);
    app.bar.selected = Some(id);
    app.bar.offer = None;
    app.bar.picker = false;
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
    if app.bar.editor.as_ref().is_some_and(Editor::is_new) {
        return None;
    }
    app.bar.selected.clone().or_else(|| order(app).first().cloned())
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
            app.bar.selected = None;
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
    let mut fresh = Editor::from_widget(&w);
    fresh.checked = true;
    app.bar.editor = Some(fresh);
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
        Msg::Grab(id) => {
            let from = order(app).iter().position(|x| *x == id);
            if let Some(from) = from {
                app.bar.drag = Some(Drag {
                    id: id.clone(),
                    from,
                    to: from,
                });
                select(app, id);
            }
        }
        Msg::Drag(dx) => {
            let n = order(app).len();
            let pitch = app.bar.tile_w(n) + canvas::TILE_GAP;
            if let Some(d) = app.bar.drag.as_mut() {
                let at = d.from as f32 + (dx / pitch).round();
                d.to = at.clamp(0.0, n.saturating_sub(1) as f32) as usize;
            }
        }
        Msg::Drop => {
            if let Some(d) = app.bar.drag.take() {
                if d.to != d.from {
                    let mut o = order(app);
                    let id = o.remove(d.from);
                    o.insert(d.to.min(o.len()), id);
                    app.write(ORDER, json!(o));
                }
            }
        }
        Msg::Shift(later) => {
            let Some(id) = selected(app) else {
                return Task::none();
            };
            let mut o = order(app);
            if let Some(at) = o.iter().position(|x| *x == id) {
                let to = if later { at + 1 } else { at.wrapping_sub(1) };
                if to < o.len() {
                    o.swap(at, to);
                    app.write(ORDER, json!(o));
                }
            }
        }
        Msg::Remove => {
            let Some(id) = selected(app) else {
                return Task::none();
            };
            let mut imp = important(app);
            if imp.contains(&id) {
                imp.retain(|x| *x != id);
                app.write(IMPORTANT, json!(imp));
            }
            let mut o = order(app);
            let at = o.iter().position(|x| *x == id).unwrap_or(0);
            o.retain(|x| *x != id);
            app.write(ORDER, json!(o));
            // The neighbour takes the selection, so a run of removals is a
            // run of clicks on one place.
            let next = o.get(at.min(o.len().saturating_sub(1))).cloned();
            app.bar.editor = None;
            app.bar.selected = None;
            if let Some(next) = next {
                select(app, next);
            }
        }
        Msg::Important(on) => {
            let Some(id) = selected(app) else {
                return Task::none();
            };
            let mut imp = important(app);
            imp.retain(|x| *x != id);
            if on {
                imp.push(id);
            }
            app.write(IMPORTANT, json!(imp));
        }
        Msg::Picker => app.bar.picker = !app.bar.picker,
        Msg::Add(id) => {
            let mut o = order(app);
            if !o.contains(&id) {
                o.push(id.clone());
                app.write(ORDER, json!(o));
            }
            select(app, id);
        }
        Msg::New => {
            app.bar.editor = Some(Editor::new());
            app.bar.picker = false;
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
    }
    Task::none()
}

// ------------------------------------------------------------------ view
//
// Accent ledger: the one live yellow is the selected widget — its tile in the
// lane and its outline on the bar picture, both inside the hero. The picture
// itself is neutral: meters unaccented, the visualizer at rest, the launcher
// ring in secondary ink. Controls keep their own on-state (toggles, sliders).

fn bar_msg(m: Msg) -> Message {
    Message::Bar(m)
}

/// Every block of the pane after the header.
pub fn blocks(app: &App) -> Vec<Element<'_, Message, Theme>> {
    let mut out = vec![hero(app)];
    if let Some(s) = sheet(app) {
        out.push(s);
    }
    out.push(motion_band(app));
    out.push(settings(app));
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
    for c in &b.cells {
        let presence = c.presence.value().clamp(0.0, 1.0);
        let x = c.x.value();
        let body: Element<'_, Message, Theme> = if c.cell.grip() {
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
        layers.push(placed(x, body));
        if sel.as_deref() == Some(c.id.as_str()) && presence > 0.0 {
            layers.push(placed(
                x,
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
    let sheet = bar_sheet(
        Stack::with_children(layers)
            .width(Length::Fill)
            .height(Length::Fill),
        Length::Fill,
    );
    sensor(sheet)
        .on_show(|s| bar_msg(Msg::Measured(s)))
        .on_resize(|s| bar_msg(Msg::Measured(s)))
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

/// The lane: one tile per widget, in order, with a drop mark while one is
/// dragged.
fn lane_row(app: &App) -> Element<'_, Message, Theme> {
    let o = order(app);
    let imp = important(app);
    let sel = selected(app);
    let tile_w = app.bar.tile_w(o.len());
    let drop_at = app
        .bar
        .drag
        .as_ref()
        .and_then(|d| (d.to != d.from).then_some(if d.to > d.from { d.to + 1 } else { d.to }));
    let slot = |k: usize| -> Element<'_, Message, Theme> {
        let w = if k == 0 || k == o.len() {
            canvas::DROP_MARK_W
        } else {
            canvas::TILE_GAP
        };
        if drop_at == Some(k) {
            container(eclipse_ui::widget::edge_quad(
                Length::Fixed(canvas::DROP_MARK_W),
                Length::Fixed(canvas::TILE_H),
                color::TEXT,
            ))
            .width(Length::Fixed(w))
            .align_x(Alignment::Center)
            .into()
        } else {
            Space::new().width(Length::Fixed(w)).into()
        }
    };
    let mut r = Row::new().align_y(Alignment::Center);
    for (i, id) in o.iter().enumerate() {
        r = r.push(slot(i));
        let dragging = app.bar.drag.as_ref().is_some_and(|d| d.id == *id);
        let grip = drag_bar()
            .state(if dragging { Grip::Active } else { Grip::Rest })
            .on_press(bar_msg(Msg::Grab(id.clone())))
            .on_drag(|dx| bar_msg(Msg::Drag(dx)))
            .on_release(bar_msg(Msg::Drop));
        r = r.push(widget_tile(
            grip,
            &bp::title(id),
            tile_w,
            imp.contains(id),
            sel.as_deref() == Some(id.as_str()),
            bar_msg(Msg::Select(id.clone())),
        ));
    }
    r = r.push(slot(o.len()));
    r.into()
}

fn picker(app: &App) -> Element<'_, Message, Theme> {
    let o = order(app);
    let mut chips: Vec<Element<'_, Message, Theme>> = Vec::new();
    for id in bp::BUILTINS.iter().filter(|id| !o.iter().any(|x| x == *id)) {
        chips.push(chip(
            None,
            &bp::title(id),
            false,
            bar_msg(Msg::Add((*id).to_owned())),
        ));
    }
    for w in &app.bar.customs {
        let id = format!("{}{}", bp::CUSTOM, w.name);
        if !o.contains(&id) {
            chips.push(chip(None, &w.name, false, bar_msg(Msg::Add(id))));
        }
    }
    let lead: Element<'_, Message, Theme> = if chips.is_empty() {
        caption("every widget is on the bar".into())
    } else {
        Row::with_children(chips).spacing(space::CHIP_GAP).wrap().into()
    };
    row![
        container(lead).width(Length::Fill),
        pill("New custom widget", false, bar_msg(Msg::New)),
    ]
    .spacing(space::CONTROL_GAP)
    .align_y(Alignment::Center)
    .into()
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

    let o = order(app);
    let lane_note = match &b.drag {
        Some(d) => format!("moving to {} of {}", d.to + 1, o.len()),
        None => "left to right · drag a grip to reorder".to_owned(),
    };
    let mut col = column![
        row![
            micro_label("live bar"),
            Space::new().width(Length::Fill),
            caption(readout(app)),
        ]
        .align_y(Alignment::Center),
        picture(app),
        row![prose("Open windows"), Space::new().width(Length::Fill), windows,]
            .spacing(space::CONTROL_GAP)
            .align_y(Alignment::Center),
        hairline(),
        row![
            micro_label("widgets"),
            caption(lane_note),
            Space::new().width(Length::Fill),
            pill(
                if b.picker { "Done" } else { "Add widget" },
                false,
                bar_msg(Msg::Picker)
            ),
        ]
        .spacing(space::CONTROL_GAP)
        .align_y(Alignment::Center),
        lane_row(app),
    ]
    .spacing(space::ROW_Y);
    if b.picker {
        col = col.push(picker(app));
    }
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
            editor_view(ed),
        ];
        return Some(inset(col).into());
    }

    let id = selected(app)?;
    let o = order(app);
    let at = o.iter().position(|x| *x == id);
    let imp = important(app).contains(&id);

    let mut head = Row::new()
        .push(
            text(bp::title(&id))
                .font(font::UI)
                .size(size::CARD_TITLE)
                .style(theme::text_primary),
        )
        .push(caption(match at {
            Some(i) => format!("{id} · {} of {}", i + 1, o.len()),
            None => id.clone(),
        }))
        .push(Space::new().width(Length::Fill))
        .spacing(space::CONTROL_GAP)
        .align_y(Alignment::Center);
    if let Some(i) = at {
        if i > 0 {
            head = head.push(pill("Earlier", false, bar_msg(Msg::Shift(false))));
        }
        if i + 1 < o.len() {
            head = head.push(pill("Later", false, bar_msg(Msg::Shift(true))));
        }
        head = head.push(pill("Remove", false, bar_msg(Msg::Remove)));
    }

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
    if let Some(name) = id.strip_prefix(bp::CUSTOM) {
        col = col.push(hairline());
        match &app.bar.editor {
            Some(ed) => col = col.push(editor_view(ed)),
            None => {
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

/// The tray's three lanes, flat inside the tray's sheet.
fn tray_rows(app: &App) -> Vec<Element<'_, Message, Theme>> {
    let t = app.tray();
    let sel = app.tray_sel.as_deref();
    let chips = |ids: &[String], ordinal: bool| -> Element<'_, Message, Theme> {
        if ids.is_empty() {
            return caption("none".into());
        }
        Row::with_children(ids.iter().enumerate().map(|(i, id)| {
            chip(
                ordinal.then_some(i + 1),
                id,
                sel == Some(id.as_str()),
                Message::TraySelect(id.clone()),
            )
        }))
        .spacing(space::CHIP_GAP)
        .wrap()
        .into()
    };
    let live = match &app.tray_live {
        None => "reading the tray".to_owned(),
        Some(None) => "tray unreachable".to_owned(),
        Some(Some(l)) if l.len() == 1 => "1 app running".to_owned(),
        Some(Some(l)) => format!("{} apps running", l.len()),
    };
    let lane = |name: &str, ids: Vec<String>, ordinal: bool| {
        padded(
            row![
                container(micro_label(name)).width(Length::Fixed(space::FIELD_W / 2.0)),
                chips(&ids, ordinal),
            ]
            .spacing(space::CONTROL_GAP)
            .align_y(Alignment::Center),
        )
    };
    let mut out = vec![
        padded(row![
            micro_label("status icons"),
            Space::new().width(Length::Fill),
            caption(live)
        ]),
        lane("on the bar", t.taskbar(), true),
        lane("drawer", t.in_lane(Lane::Overflow), false),
        lane("hidden", t.in_lane(Lane::Hidden), false),
    ];
    if let Some(id) = sel {
        let at = t.lane_of(id);
        let mut r = Row::new()
            .push(mono(id))
            .push(Space::new().width(Length::Fill))
            .spacing(space::PILL_GAP)
            .align_y(Alignment::Center);
        if at == Lane::Taskbar {
            if t.shifted(id, false).is_some() {
                r = r.push(pill("Earlier", false, Message::TrayShift(id.to_owned(), false)));
            }
            if t.shifted(id, true).is_some() {
                r = r.push(pill("Later", false, Message::TrayShift(id.to_owned(), true)));
            }
        }
        for (to, name) in [
            (Lane::Taskbar, "To bar"),
            (Lane::Overflow, "To drawer"),
            (Lane::Hidden, "Hide"),
        ] {
            if at != to {
                r = r.push(pill(name, false, Message::TrayMove(id.to_owned(), to)));
            }
        }
        out.push(padded(r));
    }
    out
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
fn editor_view(ed: &Editor) -> Element<'_, Message, Theme> {
    let bad = |f| ed.errors_for(f).next().is_some();
    let mut kinds = Row::new().spacing(space::PILL_GAP);
    for k in Kind::ALL {
        kinds = kinds.push(pill(k.as_str(), ed.kind == k, bar_msg(Msg::Kind(k))));
    }
    let mut col = column![
        padded(caption_prose(
            "Commands run as you, without a shell: each chip is one argument, passed exactly as written."
        )),
        list_row("Name", field("weather", &ed.name, bad(Field::Name), Msg::Name)),
        errors_for(ed, Field::Name),
        list_row("Kind", kinds),
        padded(caption_prose(ed.kind.gist())),
    ];
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
    fn an_explicit_selection_wins_over_the_first_widget() {
        let mut b = bar_with(&["weather", "load"]);
        b.selected = Some("custom:load".into());
        adopt(&mut b, &ids(&["custom:weather", "custom:load"]));
        assert_eq!(open(&b), Some("load"));
    }
}
