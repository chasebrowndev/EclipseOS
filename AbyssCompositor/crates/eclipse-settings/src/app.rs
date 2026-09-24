// SPDX-License-Identifier: AGPL-3.0-only
//! State, messages and view.
//!
//! The whole pane body is generated: `rows` is whatever the compositor said
//! its schema is, `pane_for` sorts it, and `Control` picks the widget. The one
//! bespoke pane is Display, which is not schema keys at all but outputs
//! (COMP-03 §1.1) — and even there the calibration overlay is the
//! compositor's; this pane only sends the verbs and shows the numbers.
//! Network is the other: the status service's readings, not config
//! (`network.rs`). Taskbar is schema keys plus one hero — the tray lanes,
//! which are the control for the two `bar.tray.*` lists.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use iced::widget::{column, pick_list, row, scrollable, text_input, Column, Row, Space};
use iced::{Alignment, Element, Length, Subscription, Task, Theme};
use serde_json::{json, Value};

use eclipse_ipc::EventKind;
use eclipse_ui::theme;
use eclipse_ui::tokens::space;
use eclipse_ui::widget::{
    big_value, chip, content, hairline, header, lane, list_row, micro_label, panel, pill, sidebar,
    status_chip, subtitle, value as mono, NumericSlider, Toggle,
};

use crate::conn::{Conn, Problem};
use crate::network::{self, Net};
use crate::output::{Edge, Inset, Output};
use crate::pane::{group_for, pane_for, Pane};
use crate::schema::{Control, Row as Key};
use crate::tray::{Lane, Tray};

const TRAY_PINNED: &str = "bar.tray.pinned";
const TRAY_HIDDEN: &str = "bar.tray.hidden";

/// How often the event thread checks the socket. `poll_event` never blocks and
/// we have no futures timer backend (only `thread-pool` is enabled), so the
/// wait is an ordinary sleep on an ordinary thread.
const POLL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone)]
pub enum Message {
    Select(Pane),
    Toggled(String, bool),
    SliderMoved(String, f64),
    SliderReleased(String),
    Chose(String, String),
    Edited(String, String),
    Committed(String),
    Reload,
    Dismiss,
    OutputEnabled(u64, bool),
    OutputScale(u64, f64),
    OutputScaleReleased(u64),
    OutputTransform(u64, String),
    InsetMoved(u64, Edge, f64),
    InsetReleased(u64),
    Calibrate(u64, &'static str),
    NumberTyped(Num, String),
    NumberCommitted(Num),
    /// A click landed somewhere else: every open draft commits or reverts.
    /// iced 0.14's `text_input` has no blur hook, so focus loss is observed
    /// at the application level instead.
    NumberBlur,
    Wire(EventKind, Value),
    /// The Network pane's feed connected; its action handle.
    NetReady(network::Handle),
    NetDown(String),
    Net(eclipse_services::status::Event),
    ForgetWifi(String),
    ForgetDevice(String),
    /// Select a tray entry, or clear the selection if it is the one selected.
    TraySelect(String),
    TrayMove(String, Lane),
    /// Move a pinned entry one place: `true` later, `false` earlier.
    TrayShift(String, bool),
    /// The live tray ids from `tray::feed`; `None` when it could not start.
    TrayLive(Option<Vec<String>>),
}

/// Which draggable number a typed draft belongs to. The three sites are not
/// one keyspace — a schema key is a path, an output control is an id — so the
/// draft map is keyed by the union rather than by a stringly-typed hybrid.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Num {
    Key(String),
    Inset(u64, Edge),
    Scale(u64),
}

/// The value a [`Num`] is allowed to take: its slider's own range, and
/// whether it reads as an integer.
struct Span {
    min: f64,
    max: f64,
    integral: bool,
}

/// Overscan is bounded here and nowhere else — the compositor's own clamp is
/// a fraction of the mode (`outputs/overscan.rs`), so this is a UI bound.
const INSET_MAX: f64 = 120.0;
const SCALE_MIN: f64 = 0.5;
const SCALE_MAX: f64 = 3.0;
const SCALE_STEP: f64 = 0.05;

impl Span {
    fn format(&self, v: f64) -> String {
        if self.integral {
            format!("{}", v.round() as i64)
        } else {
            format!("{v:.2}")
        }
    }

    /// Out of range is clamped, like a drag to the end of the track.
    /// Unparseable is refused, and `None` is what refusal looks like.
    fn parse(&self, text: &str) -> Option<f64> {
        let v: f64 = text.trim().parse().ok()?;
        v.is_finite().then(|| v.clamp(self.min, self.max))
    }
}

pub struct App {
    conn: Conn,
    rows: Vec<Key>,
    pane: Pane,
    /// Text in flight, per path. Absent means "show the committed value".
    drafts: HashMap<String, String>,
    /// Drafts `validate_config` has rejected. Written on every keystroke so
    /// the field can say no before the user commits.
    invalid: HashSet<String>,
    /// Text typed into a numeric entry, per control. Absent means "show the
    /// value the slider is at".
    nums: HashMap<Num, String>,
    /// Slider position while the knob is held. The write happens on release —
    /// a drag must not splice the KDL file once per pixel.
    live: HashMap<String, f64>,
    outputs: Vec<Output>,
    insets: HashMap<u64, Inset>,
    scales: HashMap<u64, f64>,
    calibrating: Option<u64>,
    banner: Option<Problem>,
    restart_pending: bool,
    net: Net,
    /// The tray entry the move pills act on.
    tray_sel: Option<String>,
    /// The running status-notifier items, while the Taskbar pane shows.
    /// `None` is not heard from yet; `Some(None)` is the feed failing.
    tray_live: Option<Option<Vec<String>>>,
    /// Every panel's glass radius, live-synced to `decoration.rounding`
    /// (BLUR-06): read once at startup and refetched on every `Config` event,
    /// so a live-reload can never leave this pane's glass drifted from the
    /// compositor's blur backdrop behind it.
    glass_radius: f32,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self::with_pane(Pane::Appearance)
    }

    /// Open on `pane` — `eclipse-settings network` from the taskbar.
    pub fn with_pane(pane: Pane) -> Self {
        let mut conn = Conn::new();
        let glass_radius = conn.glass_radius().unwrap_or(eclipse_ui::tokens::radius::CARD);
        let mut app = App {
            conn,
            rows: Vec::new(),
            pane,
            drafts: HashMap::new(),
            invalid: HashSet::new(),
            live: HashMap::new(),
            nums: HashMap::new(),
            outputs: Vec::new(),
            insets: HashMap::new(),
            scales: HashMap::new(),
            calibrating: None,
            banner: None,
            restart_pending: false,
            net: Net::default(),
            tray_sel: None,
            tray_live: None,
            glass_radius,
        };
        // Debug builds only: open with a tray entry selected, so the selected
        // state can be screenshotted without pointer injection.
        #[cfg(debug_assertions)]
        {
            app.tray_sel = std::env::var("SETTINGS_PREVIEW_TRAY_SEL").ok();
        }
        app.reload();
        app
    }

    /// Re-read everything. Cheaper than tracking which key a write touched,
    /// and it is also how the app recovers from someone editing the file.
    fn reload(&mut self) {
        match self.conn.load_schema() {
            Ok(rows) => {
                self.rows = rows;
                self.drafts.clear();
                self.invalid.clear();
                self.live.clear();
                self.nums.clear();
            }
            Err(e) => self.banner = Some(e),
        }
        match self.conn.call("get_outputs", json!({ "all": true })) {
            Ok(reply) => {
                self.outputs = crate::output::parse_all(&reply);
                self.scales.clear();
            }
            Err(e) => self.banner = Some(e),
        }
    }

    fn key(&self, path: &str) -> Option<&Key> {
        self.rows.iter().find(|r| r.path == path)
    }

    /// The two tray lists as the compositor last reported them.
    fn tray(&self) -> Tray {
        let v = |p| self.key(p).map(|k| k.value.clone()).unwrap_or(Value::Null);
        let mut t = Tray::from_values(&v(TRAY_PINNED), &v(TRAY_HIDDEN));
        if let Some(Some(live)) = &self.tray_live {
            t.live = live.clone();
        }
        t
    }

    /// Write one scalar and fold the outcome into the banner.
    fn write(&mut self, path: &str, v: Value) {
        match self.conn.set(path, v) {
            Ok(restart) => {
                self.restart_pending |= restart;
                self.banner = None;
                self.reload();
            }
            Err(e) => self.banner = Some(e),
        }
    }

    /// The range a typed draft is checked against: the same one its slider
    /// spans, so typing and dragging cannot disagree.
    fn span(&self, id: &Num) -> Option<Span> {
        match id {
            Num::Key(path) => match self.key(path).map(|k| &k.control) {
                Some(Control::Slider { min, max, integral }) => Some(Span {
                    min: *min,
                    max: *max,
                    integral: *integral,
                }),
                _ => None,
            },
            Num::Inset(..) => Some(Span {
                min: 0.0,
                max: INSET_MAX,
                integral: true,
            }),
            Num::Scale(..) => Some(Span {
                min: SCALE_MIN,
                max: SCALE_MAX,
                integral: false,
            }),
        }
    }

    /// What the control reads right now: the in-flight drag if there is one,
    /// otherwise the compositor's value.
    fn current(&self, id: &Num) -> Option<f64> {
        match id {
            Num::Key(path) => self
                .live
                .get(path)
                .copied()
                .or_else(|| self.key(path).map(Key::as_f64)),
            Num::Inset(out, edge) => {
                let o = self.outputs.iter().find(|o| o.id == *out)?;
                Some(self.insets.get(out).copied().unwrap_or(o.overscan).get(*edge) as f64)
            }
            Num::Scale(out) => {
                let o = self.outputs.iter().find(|o| o.id == *out)?;
                Some(self.scales.get(out).copied().unwrap_or(o.scale))
            }
        }
    }

    /// Apply a committed draft. The write is the same one the slider's
    /// release performs — a typed number is not a second code path.
    fn commit_num(&mut self, id: &Num, value: f64) {
        match id {
            Num::Key(path) => {
                let integral = matches!(
                    self.key(path).map(|k| &k.control),
                    Some(Control::Slider { integral: true, .. })
                );
                self.live.remove(path);
                let json = if integral {
                    json!(value.round() as i64)
                } else {
                    json!(value)
                };
                let path = path.clone();
                self.write(&path, json);
            }
            Num::Inset(out, edge) => {
                let base = self
                    .outputs
                    .iter()
                    .find(|o| o.id == *out)
                    .map(|o| o.overscan)
                    .unwrap_or_default();
                let inset = self.insets.entry(*out).or_insert(base);
                inset.set(*edge, value.round() as i64);
                let inset = *inset;
                self.set_output(*out, "overscan", inset.to_json());
                self.insets.remove(out);
            }
            Num::Scale(out) => {
                self.scales.remove(out);
                self.set_output(*out, "scale", json!(value));
            }
        }
    }

    fn set_output(&mut self, id: u64, field: &str, v: Value) {
        // `output` is the numeric id from `get_outputs`, not the connector
        // name: every handler parses it with `u64_param`.
        if !self.outputs.iter().any(|o| o.id == id) {
            return;
        }
        match self.conn.call("set_output", json!({ "output": id, field: v })) {
            Ok(_) => {
                self.banner = None;
                self.reload();
            }
            Err(e) => self.banner = Some(e),
        }
    }
}

pub fn update(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::Select(p) => app.pane = p,
        Message::Dismiss => app.banner = None,
        Message::Reload => app.reload(),

        Message::Toggled(path, on) => app.write(&path, Value::Bool(on)),

        Message::SliderMoved(path, v) => {
            app.live.insert(path, v);
        }
        Message::SliderReleased(path) => {
            if let Some(v) = app.live.remove(&path) {
                let integral = matches!(
                    app.key(&path).map(|k| &k.control),
                    Some(Control::Slider { integral: true, .. })
                );
                let json = if integral {
                    json!(v.round() as i64)
                } else {
                    json!(v)
                };
                app.write(&path, json);
            }
        }

        Message::Chose(path, choice) => app.write(&path, Value::String(choice)),

        Message::Edited(path, text) => {
            // Validation is per keystroke so the field can refuse before the
            // write. It is a `dry_run`-shaped call: nothing is spliced.
            match app.conn.validate(&path, Value::String(text.clone())) {
                Ok(()) => {
                    app.invalid.remove(&path);
                }
                Err(_) => {
                    app.invalid.insert(path.clone());
                }
            }
            app.drafts.insert(path, text);
        }
        Message::Committed(path) => {
            if app.invalid.contains(&path) {
                return Task::none();
            }
            if let Some(text) = app.drafts.remove(&path) {
                app.write(&path, Value::String(text));
            }
        }

        Message::NumberTyped(id, text) => {
            app.nums.insert(id, text);
        }
        Message::NumberCommitted(id) => {
            if let Some(text) = app.nums.remove(&id) {
                if let Some(v) = app.span(&id).and_then(|s| s.parse(&text)) {
                    app.commit_num(&id, v);
                }
            }
        }
        Message::NumberBlur => {
            // Take first: a write reloads, and a reload clears the map out
            // from under an iteration over it.
            for (id, text) in std::mem::take(&mut app.nums) {
                let Some(span) = app.span(&id) else { continue };
                // Unchanged text is not a write — a click inside the field
                // must not splice the file.
                match (span.parse(&text), app.current(&id)) {
                    (Some(v), Some(now)) if (v - now).abs() > f64::EPSILON => app.commit_num(&id, v),
                    _ => {}
                }
            }
        }

        Message::OutputEnabled(id, on) => app.set_output(id, "enabled", Value::Bool(on)),
        Message::OutputScale(id, v) => {
            app.scales.insert(id, v);
        }
        Message::OutputScaleReleased(id) => {
            if let Some(v) = app.scales.remove(&id) {
                app.set_output(id, "scale", json!(v));
            }
        }
        Message::OutputTransform(id, t) => app.set_output(id, "transform", Value::String(t)),
        Message::InsetMoved(id, edge, v) => {
            let inset = app.insets.entry(id).or_default();
            inset.set(edge, v.round() as i64);
        }
        Message::InsetReleased(id) => {
            if let Some(inset) = app.insets.get(&id).copied() {
                app.set_output(id, "overscan", inset.to_json());
                // The compositor's reading is the truth from here on; the
                // draft would otherwise pin the row to a stale number.
                app.insets.remove(&id);
            }
        }
        Message::Calibrate(id, action) => {
            if !app.outputs.iter().any(|o| o.id == id) {
                return Task::none();
            }
            match app
                .conn
                .call("calibrate_output", json!({ "output": id, "action": action }))
            {
                Ok(_) => {
                    app.calibrating = (action == "start").then_some(id);
                    app.banner = None;
                    // Commit and cancel both leave the compositor holding
                    // overscan numbers this pane has never seen.
                    if action != "start" {
                        app.insets.remove(&id);
                        app.reload();
                    }
                }
                Err(e) => app.banner = Some(e),
            }
        }

        Message::Wire(kind, data) => match kind {
            // Hot-plug: the output list is the compositor's, never cached
            // across an event.
            EventKind::Output => app.reload(),
            EventKind::ConfigError => app.banner = Some(Problem::from_config_error(&data)),
            // A reload succeeded; re-read the one key this pane keeps live
            // between fetches rather than re-deriving from `rows` (BLUR-06).
            EventKind::Config => {
                if let Some(radius) = app.conn.glass_radius() {
                    app.glass_radius = radius;
                }
            }
            _ => {}
        },

        Message::NetReady(h) => app.net.ready(h),
        Message::NetDown(why) => app.net.down(why),
        Message::Net(ev) => app.net.apply(ev),
        Message::ForgetWifi(ssid) => app.net.forget_wifi(ssid),
        Message::ForgetDevice(addr) => app.net.forget_device(addr),

        Message::TraySelect(id) => {
            app.tray_sel = (app.tray_sel.as_deref() != Some(id.as_str())).then_some(id);
        }
        Message::TrayMove(id, to) => {
            // The selection stays on the entry, so a second move is one click.
            let w = app.tray().moved(&id, to);
            if let Some(p) = w.pinned {
                app.write(TRAY_PINNED, json!(p));
            }
            if let Some(h) = w.hidden {
                app.write(TRAY_HIDDEN, json!(h));
            }
        }
        Message::TrayShift(id, later) => {
            if let Some(p) = app.tray().shifted(&id, later) {
                app.write(TRAY_PINNED, json!(p));
            }
        }
        Message::TrayLive(live) => app.tray_live = Some(live),
    }
    Task::none()
}

/// A second connection on its own thread, forwarding events into the runtime.
/// iced's `time::every` needs a tokio or smol backend and we enable neither,
/// so the wait lives in a thread rather than in a futures timer.
pub fn subscription(app: &App) -> Subscription<Message> {
    let mut subs = vec![blur(), events()];
    // The status feed runs only while its pane is showing.
    if app.pane == Pane::Network {
        subs.push(network::feed());
    }
    if app.pane == Pane::Taskbar {
        subs.push(crate::tray::feed());
    }
    Subscription::batch(subs)
}

/// A press anywhere is the only focus-loss signal available: iced 0.14's
/// `text_input` has no blur hook. `NumberBlur` is a no-op unless a draft is
/// open and actually differs from the live value.
fn blur() -> Subscription<Message> {
    iced::event::listen_with(|event, _status, _window| match event {
        iced::Event::Mouse(iced::mouse::Event::ButtonPressed(_))
        | iced::Event::Touch(iced::touch::Event::FingerPressed { .. }) => Some(Message::NumberBlur),
        _ => None,
    })
}

fn events() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(32, async move |mut sender| {
            std::thread::spawn(move || {
                let mut client = None;
                loop {
                    if client.is_none() {
                        if let Ok(mut c) = eclipse_ipc::Client::connect() {
                            if c.subscribe(&[EventKind::Output, EventKind::ConfigError, EventKind::Config])
                                .is_ok()
                            {
                                client = Some(c);
                            }
                        }
                    }
                    if let Some(c) = client.as_mut() {
                        loop {
                            match c.poll_event() {
                                Ok(Some(ev)) => {
                                    if sender.try_send(Message::Wire(ev.kind, ev.data)).is_err() {
                                        return;
                                    }
                                }
                                Ok(None) => break,
                                Err(_) => {
                                    client = None;
                                    break;
                                }
                            }
                        }
                    }
                    std::thread::sleep(POLL);
                }
            });
        })
    })
}

pub fn view(app: &App) -> Element<'_, Message, Theme> {
    let nav: Vec<Element<'_, Message, Theme>> = Pane::ALL
        .iter()
        .map(|p| eclipse_ui::widget::nav_item(p.title(), *p == app.pane, Message::Select(*p)))
        .collect();

    let mut footer = vec![
        (
            "socket",
            if app.conn.is_connected() {
                "connected".to_string()
            } else {
                "offline".to_string()
            },
        ),
        ("keys", app.rows.len().to_string()),
    ];
    if app.restart_pending {
        footer.push(("restart", "required".into()));
    }

    let mut controls = vec![pill("Reload", false, Message::Reload)];
    match app.pane {
        Pane::Network => {
            let (state, measure) = app.net.chip();
            controls.push(status_chip(&state, &measure));
        }
        Pane::Taskbar => {
            let t = app.tray();
            controls.push(status_chip(
                &format!("{} in taskbar", t.taskbar().len()),
                &format!(
                    "{} overflow · {} hidden",
                    t.in_lane(Lane::Overflow).len(),
                    t.in_lane(Lane::Hidden).len()
                ),
            ));
        }
        _ => {}
    }
    let mut blocks = vec![header(
        app.pane.title(),
        subtitle(app.pane.subtitle(), "", ""),
        controls,
    )];
    if let Some(problem) = &app.banner {
        blocks.push(banner(problem, app.glass_radius));
    }
    match app.pane {
        Pane::Display => blocks.extend(display_pane(app)),
        Pane::Network => blocks.extend(network::blocks(&app.net, app.glass_radius)),
        Pane::Taskbar => {
            blocks.push(tray_hero(app));
            blocks.extend(schema_pane(app));
        }
        _ => blocks.extend(schema_pane(app)),
    }

    row![
        sidebar(nav, footer),
        scrollable(content(blocks))
            .style(theme::eclipse_scrollable)
            .height(Length::Fill),
    ]
    .into()
}

/// The two error shapes every DE app renders identically: a denial and a
/// config error. Neither is ever swallowed.
fn banner(problem: &Problem, radius: f32) -> Element<'_, Message, Theme> {
    panel(
        radius,
        column![
            row![
                mono(&problem.headline()),
                Space::new().width(Length::Fill),
                pill("Dismiss", false, Message::Dismiss),
            ]
            .align_y(iced::Alignment::Center),
            hairline(),
            iced::widget::text(problem.detail().to_string())
                .font(eclipse_ui::tokens::font::UI)
                .size(eclipse_ui::tokens::size::BODY_SMALL)
                .style(theme::text_secondary),
        ]
        .spacing(space::ROW_Y),
    )
    .into()
}

/// Every schema key this pane claims, grouped by node, in schema order.
fn schema_pane(app: &App) -> Vec<Element<'_, Message, Theme>> {
    let mut groups: Vec<(&str, Vec<Element<'_, Message, Theme>>)> = Vec::new();
    // The tray lists are drawn by `tray_hero`, their control; listing them
    // again here as read-only text would be the same setting twice.
    let shown = |k: &&Key| pane_for(&k.path) == Some(app.pane) && !k.path.starts_with("bar.tray.");
    for key in app.rows.iter().filter(shown) {
        let group = group_for(&key.path);
        let rows = match groups.iter_mut().find(|(g, _)| *g == group) {
            Some((_, rows)) => rows,
            None => {
                groups.push((group, Vec::new()));
                &mut groups.last_mut().expect("just pushed").1
            }
        };
        rows.push(list_row(key.label(), control(app, key)));
    }

    groups
        .into_iter()
        .map(|(group, rows)| {
            let mut col = Column::new().push(micro_label(group)).spacing(space::ROW_Y);
            for r in rows {
                col = col.push(r);
            }
            panel(app.glass_radius, col).into()
        })
        .collect()
}

/// The tray, as three lanes of chips: the hero of the Taskbar pane.
///
/// Select a chip, then move it. Pinned chips carry their position, and the
/// taskbar lane is the order the bar draws them in. Accent ledger: the one
/// yellow is the selected chip; every move pill is unaccented.
///
/// The chips are the built-ins, the status-notifier items running now, and
/// any id the two lists name whose app is not running. The caption counts
/// the running ones, so an app that is missing from the lanes is plainly
/// not running rather than silently dropped.
fn tray_hero(app: &App) -> Element<'_, Message, Theme> {
    let t = app.tray();
    let sel = app.tray_sel.as_deref();
    let chips = |ids: &[String], ordinal: bool| -> Vec<Element<'_, Message, Theme>> {
        ids.iter()
            .enumerate()
            .map(|(i, id)| {
                chip(
                    ordinal.then_some(i + 1),
                    id,
                    sel == Some(id.as_str()),
                    Message::TraySelect(id.clone()),
                )
            })
            .collect()
    };
    let pinned = t.taskbar();
    let overflow = t.in_lane(Lane::Overflow);
    let hidden = t.in_lane(Lane::Hidden);

    let caption = row![
        micro_label("tray"),
        Space::new().width(Length::Fill),
        iced::widget::text(match &app.tray_live {
            None => "reading tray".to_owned(),
            Some(None) => "tray unreachable".to_owned(),
            Some(Some(live)) => match live.len() {
                1 => "1 app running".to_owned(),
                n => format!("{n} apps running"),
            },
        })
        .font(eclipse_ui::tokens::font::DATA)
        .size(eclipse_ui::tokens::size::MICRO)
        .style(theme::text_tertiary),
    ]
    .align_y(Alignment::Center);

    let actions: Element<'_, Message, Theme> = match sel {
        None => iced::widget::text("nothing selected · select an entry, then move it")
            .size(eclipse_ui::tokens::size::BODY_SMALL)
            .style(theme::text_tertiary)
            .into(),
        Some(id) => {
            let at = t.lane_of(id);
            // Where the entry comes from, so a pinned app that is not
            // running reads as that, not as a chip that does nothing.
            let origin = if crate::tray::BUILTINS.contains(&id) {
                "built-in"
            } else if t.live.iter().any(|l| l == id) {
                "running"
            } else {
                "not running"
            };
            let mut r = Row::new()
                .push(mono(id))
                .push(
                    iced::widget::text(origin)
                        .font(eclipse_ui::tokens::font::DATA)
                        .size(eclipse_ui::tokens::size::MICRO)
                        .style(theme::text_tertiary),
                )
                .push(Space::new().width(Length::Fill))
                .spacing(space::CONTROL_GAP)
                .align_y(Alignment::Center);
            if at == Lane::Taskbar {
                if t.shifted(id, false).is_some() {
                    r = r.push(pill("Earlier", false, Message::TrayShift(id.to_owned(), false)));
                }
                if t.shifted(id, true).is_some() {
                    r = r.push(pill("Later", false, Message::TrayShift(id.to_owned(), true)));
                }
            }
            for (to, label) in [
                (Lane::Taskbar, "To taskbar"),
                (Lane::Overflow, "To overflow"),
                (Lane::Hidden, "Hide"),
            ] {
                if at != to {
                    r = r.push(pill(label, false, Message::TrayMove(id.to_owned(), to)));
                }
            }
            r.into()
        }
    };

    panel(
        app.glass_radius,
        column![
            caption,
            lane(
                "taskbar",
                &format!("{} pinned", pinned.len()),
                chips(&pinned, true)
            ),
            row![
                lane(
                    "overflow",
                    &format!("{} in drawer", overflow.len()),
                    chips(&overflow, false)
                ),
                lane(
                    "hidden",
                    &format!("{} hidden", hidden.len()),
                    chips(&hidden, false)
                ),
            ]
            .spacing(space::BLOCK),
            hairline(),
            actions,
        ]
        .spacing(space::ROW_Y),
    )
    .into()
}

/// The one place a schema type becomes a widget.
fn control<'a>(app: &'a App, key: &'a Key) -> Element<'a, Message, Theme> {
    let path = key.path.clone();
    if key.locked() {
        // Policy-owned, or the compositor says not writable. Shown, never
        // editable by any path in this app: the policy editor owns it.
        return match &key.control {
            Control::Toggle => Toggle::locked(key.as_bool()).into(),
            _ => mono(&key.display()),
        };
    }

    match &key.control {
        Control::Toggle => Toggle::new(key.as_bool(), move |on| Message::Toggled(path.clone(), on)).into(),

        Control::Slider { min, max, integral } => {
            let current = app.live.get(&key.path).copied().unwrap_or_else(|| key.as_f64());
            let span = Span {
                min: *min,
                max: *max,
                integral: *integral,
            };
            let id = Num::Key(key.path.clone());
            let draft = app.nums.get(&id);
            let shown = draft.cloned().unwrap_or_else(|| span.format(current));
            let invalid = draft.is_some_and(|d| span.parse(d).is_none());
            let typed = id.clone();
            let released = path.clone();
            NumericSlider::new(
                *min..=*max,
                current,
                shown,
                move |v| Message::SliderMoved(path.clone(), v),
                move |t| Message::NumberTyped(typed.clone(), t),
            )
            .step(if *integral { 1.0 } else { 0.01 })
            .on_release(Message::SliderReleased(released))
            .on_commit(Message::NumberCommitted(id))
            .invalid(invalid)
            .into()
        }

        Control::Segmented(values) => {
            let current = key.value.as_str().unwrap_or_default().to_string();
            let mut r = Row::new().spacing(space::PILL_GAP);
            for v in values {
                r = r.push(pill(
                    crate::schema::value_label(v),
                    *v == current,
                    Message::Chose(path.clone(), v.clone()),
                ));
            }
            r.into()
        }

        Control::Dropdown(values) => {
            pick_list(values.clone(), key.value.as_str().map(str::to_owned), move |v| {
                Message::Chose(path.clone(), v)
            })
            .into()
        }

        Control::Text { .. } => {
            let draft = app.drafts.get(&key.path);
            let shown = draft.cloned().unwrap_or_else(|| key.display());
            let submit = path.clone();
            let input = text_input(key.default.as_str().unwrap_or(""), &shown)
                .on_input(move |t| Message::Edited(path.clone(), t))
                .width(Length::Fixed(space::FIELD_W))
                .style(theme::eclipse_input);
            // A rejected draft has no commit path at all, rather than a
            // commit that fails after the fact.
            if app.invalid.contains(&key.path) {
                row![input, mono("invalid")].spacing(10).into()
            } else {
                input.on_submit(Message::Committed(submit)).into()
            }
        }

        // `set_config_value` writes one scalar at a dotted path; a list needs
        // the node editor. Shown so the setting is never hidden.
        Control::List => mono(&key.display()),
    }
}

const INSET_SPAN: Span = Span {
    min: 0.0,
    max: INSET_MAX,
    integral: true,
};

/// The scale control: one row's worth, lifted out because the output card's
/// `column!` is already the widest expression in this file.
fn scale_control(app: &App, id: u64, scale: f64) -> Element<'_, Message, Theme> {
    let span = Span {
        min: SCALE_MIN,
        max: SCALE_MAX,
        integral: false,
    };
    let num = Num::Scale(id);
    let draft = app.nums.get(&num);
    let shown = draft.cloned().unwrap_or_else(|| span.format(scale));
    let invalid = draft.is_some_and(|d| span.parse(d).is_none());
    let typed = num.clone();
    NumericSlider::new(
        SCALE_MIN..=SCALE_MAX,
        scale,
        shown,
        move |v| Message::OutputScale(id, v),
        move |t| Message::NumberTyped(typed.clone(), t),
    )
    .step(SCALE_STEP)
    .on_release(Message::OutputScaleReleased(id))
    .on_commit(Message::NumberCommitted(num))
    .invalid(invalid)
    .into()
}

/// Outputs. Modes are read-only — `get_outputs` reports the current mode, not
/// the list of available ones — and the calibration overlay is drawn by the
/// compositor (COMP-03 §1.1), so this pane sends verbs and shows numbers.
fn display_pane(app: &App) -> Vec<Element<'_, Message, Theme>> {
    const TRANSFORMS: &[&str] = &["normal", "90", "180", "270"];

    if app.outputs.is_empty() {
        return vec![panel(app.glass_radius, mono("no outputs")).into()];
    }

    app.outputs
        .iter()
        .map(|o| {
            let scale = app.scales.get(&o.id).copied().unwrap_or(o.scale);
            let inset = app.insets.get(&o.id).copied().unwrap_or(o.overscan);

            let mut transforms = Row::new().spacing(6);
            for t in TRANSFORMS {
                transforms = transforms.push(pill(
                    t,
                    o.transform == *t,
                    Message::OutputTransform(o.id, (*t).to_string()),
                ));
            }

            let mut edges = Column::new().spacing(space::ROW_Y);
            for edge in Edge::ALL {
                let edge = *edge;
                let id = o.id;
                let num = Num::Inset(id, edge);
                let draft = app.nums.get(&num);
                let current = inset.get(edge) as f64;
                let shown = draft.cloned().unwrap_or_else(|| INSET_SPAN.format(current));
                let invalid = draft.is_some_and(|d| INSET_SPAN.parse(d).is_none());
                let typed = num.clone();
                edges = edges.push(list_row(
                    edge.label(),
                    NumericSlider::new(
                        0.0..=INSET_MAX,
                        current,
                        shown,
                        move |v| Message::InsetMoved(id, edge, v),
                        move |t| Message::NumberTyped(typed.clone(), t),
                    )
                    .step(1.0)
                    .on_release(Message::InsetReleased(id))
                    .on_commit(Message::NumberCommitted(num))
                    .invalid(invalid),
                ));
            }

            let calibrating = app.calibrating == Some(o.id);
            let actions: Row<'_, Message, Theme> = if calibrating {
                row![
                    pill("Commit", true, Message::Calibrate(o.id, "commit")),
                    pill("Cancel", false, Message::Calibrate(o.id, "cancel")),
                ]
            } else {
                row![pill("Calibrate", false, Message::Calibrate(o.id, "start"))]
            }
            .spacing(6);

            panel(
                app.glass_radius,
                column![
                    row![
                        micro_label(&o.name),
                        Space::new().width(Length::Fill),
                        big_value(&scale.to_string(), "x", o.focused),
                    ]
                    .align_y(iced::Alignment::Center),
                    list_row("identity", mono(&o.identity)),
                    list_row("mode", mono(&o.mode_display())),
                    list_row("position", mono(&o.position_display())),
                    list_row(
                        "enabled",
                        Toggle::new(o.enabled, {
                            let id = o.id;
                            move |on| Message::OutputEnabled(id, on)
                        }),
                    ),
                    list_row("transform", transforms),
                    list_row("scale", scale_control(app, o.id, scale)),
                    hairline(),
                    micro_label("overscan"),
                    edges,
                    actions,
                ]
                .spacing(space::ROW_Y),
            )
            .into()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCALE: Span = Span {
        min: SCALE_MIN,
        max: SCALE_MAX,
        integral: false,
    };

    #[test]
    fn a_typed_number_is_clamped_to_the_sliders_range() {
        assert_eq!(INSET_SPAN.parse("999"), Some(INSET_MAX));
        assert_eq!(INSET_SPAN.parse("-4"), Some(0.0));
        assert_eq!(SCALE.parse("9"), Some(SCALE_MAX));
    }

    #[test]
    fn a_typed_number_that_does_not_parse_is_refused() {
        for bad in ["", "  ", "2x", "nan", "inf", "--1"] {
            assert_eq!(INSET_SPAN.parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn surrounding_space_is_not_a_refusal() {
        assert_eq!(INSET_SPAN.parse(" 42 "), Some(42.0));
    }

    #[test]
    fn a_reading_round_trips_through_the_entry() {
        for v in [0.0, 1.0, 37.0, INSET_MAX] {
            assert_eq!(INSET_SPAN.parse(&INSET_SPAN.format(v)), Some(v));
        }
        for v in [SCALE_MIN, 1.0, 1.25, SCALE_MAX] {
            assert_eq!(SCALE.parse(&SCALE.format(v)), Some(v));
        }
    }

    #[test]
    fn the_overscan_bound_is_the_uis_own() {
        assert_eq!(INSET_MAX, 120.0);
    }
}
