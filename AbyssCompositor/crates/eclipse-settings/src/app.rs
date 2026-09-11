// SPDX-License-Identifier: AGPL-3.0-only
//! State, messages and view.
//!
//! The whole pane body is generated: `rows` is whatever the compositor said
//! its schema is, `pane_for` sorts it, and `Control` picks the widget. The one
//! bespoke pane is Display, which is not schema keys at all but outputs
//! (COMP-03 §1.1) — and even there the calibration overlay is the
//! compositor's; this pane only sends the verbs and shows the numbers.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use iced::widget::{column, pick_list, row, scrollable, slider, text_input, Column, Row, Space};
use iced::{Element, Length, Subscription, Task, Theme};
use serde_json::{json, Value};

use eclipse_ipc::EventKind;
use eclipse_ui::theme;
use eclipse_ui::tokens::space;
use eclipse_ui::widget::{
    big_value, content, hairline, header, list_row, micro_label, panel, pill, sidebar, subtitle,
    value as mono, Toggle,
};

use crate::conn::{Conn, Problem};
use crate::output::{Edge, Inset, Output};
use crate::pane::{group_for, pane_for, Pane};
use crate::schema::{Control, Row as Key};

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
    Wire(EventKind, Value),
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
    /// Slider position while the knob is held. The write happens on release —
    /// a drag must not splice the KDL file once per pixel.
    live: HashMap<String, f64>,
    outputs: Vec<Output>,
    insets: HashMap<u64, Inset>,
    scales: HashMap<u64, f64>,
    calibrating: Option<u64>,
    banner: Option<Problem>,
    restart_pending: bool,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        let mut app = App {
            conn: Conn::new(),
            rows: Vec::new(),
            pane: Pane::Appearance,
            drafts: HashMap::new(),
            invalid: HashSet::new(),
            live: HashMap::new(),
            outputs: Vec::new(),
            insets: HashMap::new(),
            scales: HashMap::new(),
            calibrating: None,
            banner: None,
            restart_pending: false,
        };
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

    fn set_output(&mut self, id: u64, field: &str, v: Value) {
        let Some(name) = self.outputs.iter().find(|o| o.id == id).map(|o| o.name.clone()) else {
            return;
        };
        match self.conn.call("set_output", json!({ "output": name, field: v })) {
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
            }
        }
        Message::Calibrate(id, action) => {
            let Some(name) = app.outputs.iter().find(|o| o.id == id).map(|o| o.name.clone()) else {
                return Task::none();
            };
            match app
                .conn
                .call("calibrate_output", json!({ "output": name, "action": action }))
            {
                Ok(_) => {
                    app.calibrating = (action == "start").then_some(id);
                    app.banner = None;
                }
                Err(e) => app.banner = Some(e),
            }
        }

        Message::Wire(kind, data) => match kind {
            // Hot-plug: the output list is the compositor's, never cached
            // across an event.
            EventKind::Output => app.reload(),
            EventKind::ConfigError => app.banner = Some(Problem::from_config_error(&data)),
            _ => {}
        },
    }
    Task::none()
}

/// A second connection on its own thread, forwarding events into the runtime.
/// iced's `time::every` needs a tokio or smol backend and we enable neither,
/// so the wait lives in a thread rather than in a futures timer.
pub fn subscription(_app: &App) -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(32, async move |mut sender| {
            std::thread::spawn(move || {
                let mut client = None;
                loop {
                    if client.is_none() {
                        if let Ok(mut c) = eclipse_ipc::Client::connect() {
                            if c.subscribe(&[EventKind::Output, EventKind::ConfigError]).is_ok() {
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

    let mut blocks = vec![header(
        app.pane.title(),
        subtitle(app.pane.subtitle(), "", ""),
        vec![pill("Reload", false, Message::Reload)],
    )];
    if let Some(problem) = &app.banner {
        blocks.push(banner(problem));
    }
    blocks.extend(if app.pane.is_bespoke() {
        display_pane(app)
    } else {
        schema_pane(app)
    });

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
fn banner(problem: &Problem) -> Element<'_, Message, Theme> {
    panel(
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
    for key in app.rows.iter().filter(|k| pane_for(&k.path) == Some(app.pane)) {
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
            panel(col).into()
        })
        .collect()
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
            let shown = if *integral {
                format!("{}", current.round() as i64)
            } else {
                format!("{current:.2}")
            };
            let released = path.clone();
            row![
                slider(*min..=*max, current, move |v| Message::SliderMoved(
                    path.clone(),
                    v
                ))
                .step(if *integral { 1.0 } else { 0.01 })
                .on_release(Message::SliderReleased(released))
                .style(theme::eclipse_slider)
                .width(Length::Fixed(180.0)),
                mono(&shown),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center)
            .into()
        }

        Control::Segmented(values) => {
            let current = key.value.as_str().unwrap_or_default().to_string();
            let mut r = Row::new().spacing(6);
            for v in values {
                r = r.push(pill(v, *v == current, Message::Chose(path.clone(), v.clone())));
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
                .width(Length::Fixed(220.0))
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

/// Outputs. Modes are read-only — `get_outputs` reports the current mode, not
/// the list of available ones — and the calibration overlay is drawn by the
/// compositor (COMP-03 §1.1), so this pane sends verbs and shows numbers.
fn display_pane(app: &App) -> Vec<Element<'_, Message, Theme>> {
    const TRANSFORMS: &[&str] = &["normal", "90", "180", "270"];

    if app.outputs.is_empty() {
        return vec![panel(mono("no outputs")).into()];
    }

    app.outputs
        .iter()
        .map(|o| {
            let scale = app.scales.get(&o.id).copied().unwrap_or(o.scale);
            let inset = app.insets.get(&o.id).copied().unwrap_or_default();

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
                edges = edges.push(list_row(
                    edge.label(),
                    row![
                        slider(0.0..=200.0, inset.get(edge) as f64, move |v| {
                            Message::InsetMoved(id, edge, v)
                        })
                        .step(1.0)
                        .on_release(Message::InsetReleased(id))
                        .style(theme::eclipse_slider)
                        .width(Length::Fixed(180.0)),
                        mono(&inset.get(edge).to_string()),
                    ]
                    .spacing(10)
                    .align_y(iced::Alignment::Center),
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
                    list_row(
                        "scale",
                        slider(0.5..=3.0, scale, {
                            let id = o.id;
                            move |v| Message::OutputScale(id, v)
                        })
                        .step(0.05)
                        .on_release(Message::OutputScaleReleased(o.id))
                        .style(theme::eclipse_slider)
                        .width(Length::Fixed(180.0)),
                    ),
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
