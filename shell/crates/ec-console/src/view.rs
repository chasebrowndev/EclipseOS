// SPDX-License-Identifier: AGPL-3.0-only
//! The console, drawn.
//!
//! **Hero shape.** A selected task's hero is *magnitude plus history*: the
//! time left to its deadline as the one big number, over a bar of how much of
//! the allotted time has gone. A closed task swaps it for a *status grid*
//! (outcome, provenance, agent), because nothing about a closed task is still
//! moving. The composer's hero is the statement itself at prompt scale. Under
//! the hero is the thread, which is bubbles and no list at all, and under the
//! thread is a rounded editor or the compositor's hole: four silhouettes, none
//! of them a stack of panels.
//!
//! **The accent ledger.** The one yellow in this pane is the selected task's
//! live state, spoken once, in the header's subtitle ("Waiting for your
//! reply", "Running"). In the composer, where no task is selected, it is the
//! chosen deadline. A selected fleet row is lifted white, a question carries a
//! white edge, a closed task has no yellow at all.
//!
//! **What this never draws.** No Allow or Deny, no taxonomy, no card that
//! looks like a prompt (A-08 §4.1): a count of waiting decisions and a button
//! that asks the compositor to open its queue. Inside the commit slot's hole it
//! draws nothing, and nothing around it that could be mistaken for it
//! (A-08 §8).

use iced::widget::{
    button, column, container, row, scrollable, text, text_editor, text_input, Column, Row, Space,
};
use iced::{Alignment, Element, Length, Size, Theme};

use ec_console_client::console::{CancelMode, Message as Post, Task};
use ec_ui::theme;
use ec_ui::tokens::{color, console as metrics, font, radius, size, space};
use ec_ui::widget::{
    badge, big_value, edge_note, edge_quad, hairline, header, inset, inset_list, micro_label, panel,
    pick_pill, pill, progress_bar, status_cell, status_chip, status_grid, subtitle, Hole,
};

use crate::app::{outcome_word, App, Message};
use crate::model::{self, Entry, HistoryItem, Link, Outcome, Phase, Selection, Voice};
use crate::shield::Guard;

type El<'a> = Element<'a, Message, Theme>;

pub fn view(app: &App) -> El<'_> {
    let m = &app.m;
    let narrow = app.narrow();
    let no_agents = m.listed && m.packages.is_empty() && m.link == Link::Online;

    let body: El<'_> = if no_agents {
        no_agents_pane(app)
    } else if narrow {
        if app.pane_open {
            container(pane(app))
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        } else {
            container(fleet(app, Length::Fill))
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        }
    } else {
        row![
            fleet(app, Length::Fixed(metrics::FLEET_W)),
            iced::widget::rule::vertical(space::HAIRLINE).style(theme::hairline),
            container(pane(app))
                .width(Length::Fill)
                .height(Length::Fill)
                .style(theme::content_ground(app.blur)),
        ]
        .height(Length::Fill)
        .into()
    };
    container(body).width(Length::Fill).height(Length::Fill).into()
}

// ----------------------------------------------------------------- text

fn body<'a>(s: impl text::IntoFragment<'a>) -> iced::widget::Text<'a, Theme> {
    text(s)
        .font(font::UI)
        .size(size::BODY)
        .line_height(metrics::LINE)
        .style(theme::text_primary)
}

fn small<'a>(s: impl text::IntoFragment<'a>) -> iced::widget::Text<'a, Theme> {
    text(s)
        .font(font::UI)
        .size(size::BODY_SMALL)
        .style(theme::text_secondary)
}

fn mono<'a>(s: impl text::IntoFragment<'a>) -> iced::widget::Text<'a, Theme> {
    text(s)
        .font(font::DATA)
        .size(size::MICRO)
        .style(theme::text_tertiary)
}

/// A pill that acts, or the same pill switched off.
fn act<'a>(label: &str, on: Option<Message>) -> El<'a> {
    let b = button(
        text(label.to_owned())
            .font(font::UI_MEDIUM)
            .size(size::BODY_SMALL),
    )
    .padding([space::PILL_Y, space::PILL_X]);
    match on {
        Some(msg) => b.on_press(msg).style(theme::pill(false)).into(),
        None => b.style(theme::pill_disabled).into(),
    }
}

/// A quiet verb: "Provenance", "Delete".
fn link<'a>(label: &str, on: Option<Message>) -> El<'a> {
    let b = button(
        text(label.to_owned())
            .font(font::UI_MEDIUM)
            .size(size::BODY_SMALL),
    )
    .padding(0);
    match on {
        Some(msg) => b.on_press(msg).style(theme::link).into(),
        None => b.style(theme::link).into(),
    }
}

// ----------------------------------------------------------------- fleet

fn fleet(app: &App, width: Length) -> El<'_> {
    let m = &app.m;
    let usable = m.usable();

    let new = button(
        row![
            text("New task").font(font::UI_MEDIUM).size(size::BODY),
            Space::new().width(Length::Fill),
            badge("Super /"),
        ]
        .align_y(Alignment::Center)
        .spacing(space::HEADER_GAP),
    )
    .width(Length::Fill)
    .padding([space::NAV_Y, space::NAV_X])
    .on_press(Message::NewTask)
    .style(theme::glass_cell(false, radius::INSET));

    let mut list = Column::new().spacing(space::NAV_SUB_GAP);

    let rows = model::fleet(&m.tasks);
    list = list.push(section("Running", &rows.len().to_string()));
    if rows.is_empty() {
        list = list.push(
            container(small(if m.listed {
                "Nothing running yet"
            } else {
                "Looking for tasks"
            }))
            .padding([space::NAV_Y, space::NAV_X]),
        );
    }
    for r in &rows {
        if let Some(t) = m.task(&r.task_id) {
            list = list.push(fleet_row(app, t, r.depth));
        }
    }

    let hist = m.history();
    if !hist.is_empty() {
        let open = app.history_open;
        list = list.push(
            button(
                row![
                    micro_label("History"),
                    Space::new().width(Length::Fill),
                    mono(format!("{} {}", if open { "Hide" } else { "Show" }, hist.len())),
                ]
                .align_y(Alignment::Center),
            )
            .width(Length::Fill)
            .padding([space::ROW_Y, space::NAV_X])
            .on_press(Message::ToggleHistory)
            .style(theme::link),
        );
        if open {
            for h in hist {
                list = list.push(history_row(app, h));
            }
        }
    }

    let mut top = Column::new().spacing(space::ROW_Y).push(new);
    if m.decisions > 0 {
        top = top.push(decisions(m.decisions, usable));
    }

    let foot = column![
        foot_line("agentd", link_word(&m.link, m.available)),
        foot_line(
            "window",
            match &app.shield.guard {
                Guard::On => "protected".to_owned(),
                Guard::Waiting => "checking".to_owned(),
                Guard::Off(_) => "not protected".to_owned(),
            }
        ),
    ]
    .spacing(space::LINE_GAP);

    container(
        column![
            top,
            scrollable(list)
                .style(theme::eclipse_scrollable)
                .height(Length::Fill)
                .width(Length::Fill),
            foot,
        ]
        .spacing(space::ROW_Y),
    )
    .width(width)
    .height(Length::Fill)
    .padding([space::PANE_Y, space::SIDEBAR_X])
    .style(theme::sidebar(app.blur))
    .into()
}

fn link_word(l: &Link, available: bool) -> String {
    match l {
        Link::Connecting => "connecting".to_owned(),
        Link::Offline(_) => "unavailable".to_owned(),
        Link::Online if !available => "policy away".to_owned(),
        Link::Online => "connected".to_owned(),
    }
}

fn foot_line<'a>(k: &str, v: String) -> El<'a> {
    row![
        mono(k.to_owned()),
        Space::new().width(Length::Fill),
        small(v).size(size::MICRO).font(font::DATA)
    ]
    .align_y(Alignment::Center)
    .into()
}

fn section<'a>(label: &str, count: &str) -> El<'a> {
    container(
        row![
            micro_label(label),
            Space::new().width(Length::Fill),
            mono(count.to_owned())
        ]
        .align_y(Alignment::Center),
    )
    .padding([space::ROW_Y, space::NAV_X])
    .into()
}

/// The count of waiting permission decisions and the one control that asks the
/// compositor to open its queue. A number and a button: not the decisions, not
/// a way to answer them (A-08 §4.1).
fn decisions<'a>(count: u32, usable: bool) -> El<'a> {
    inset(
        column![
            row![
                text(count.to_string())
                    .font(font::DATA_MEDIUM)
                    .size(size::BODY)
                    .style(theme::text_primary),
                small(if count == 1 {
                    "permission decision waiting"
                } else {
                    "permission decisions waiting"
                }),
            ]
            .spacing(space::HEADER_GAP)
            .align_y(Alignment::Center),
            act("Show decisions", usable.then_some(Message::ShowDecisions)),
        ]
        .spacing(space::ROW_Y),
    )
    .padding([space::ROW_Y, space::CARD])
    .into()
}

fn fleet_row<'a>(app: &'a App, t: &'a Task, depth: usize) -> El<'a> {
    let m = &app.m;
    let phase = Phase::of(t);
    let selected = m.selection == Selection::Task(t.task_id.clone());
    let who = model::package_name(&m.packages, &t.package);

    let mut right = Column::new()
        .spacing(space::LINE_GAP)
        .align_x(Alignment::End)
        .push(tag(phase));
    if t.pending_decisions > 0 {
        right = right.push(mono(format!(
            "{} decision{}",
            t.pending_decisions,
            if t.pending_decisions == 1 { "" } else { "s" }
        )));
    }

    let mut r = Row::new();
    if depth > 0 {
        r = r.push(Space::new().width(Length::Fixed(metrics::SUB_INDENT * depth as f32)));
        r = r.push(edge_quad(
            Length::Fixed(space::HAIRLINE),
            Length::Fill,
            color::BORDER_STRONG,
        ));
    }
    r = r.push(
        column![
            text(model::ellipsize(&t.statement, metrics::ROW_CHARS))
                .font(font::UI_MEDIUM)
                .size(size::BODY)
                .style(theme::text_primary),
            row![
                small(model::ellipsize(&who, 18)),
                mono(model::left(t.deadline_ms, app.now))
            ]
            .spacing(space::HEADER_GAP)
            .align_y(Alignment::Center),
        ]
        .spacing(space::LINE_GAP)
        .width(Length::Fill),
    );
    r = r.push(right);

    button(
        r.spacing(space::HEADER_GAP)
            .align_y(Alignment::Center)
            .height(Length::Shrink),
    )
    .width(Length::Fill)
    .padding([space::NAV_Y, space::NAV_X])
    .on_press(Message::Select(t.task_id.clone()))
    .style(theme::fleet_row(selected))
    .into()
}

fn history_row<'a>(app: &'a App, h: &'a HistoryItem) -> El<'a> {
    let selected = app.m.selection == Selection::Task(h.task_id.clone());
    button(
        row![
            column![
                text(model::ellipsize(&h.statement, metrics::HISTORY_CHARS))
                    .font(font::UI)
                    .size(size::BODY)
                    .style(theme::text_secondary),
                mono(model::when(h.closed_ms, app.now)),
            ]
            .spacing(space::LINE_GAP)
            .width(Length::Fill),
            tag(Phase::Closed(h.outcome)),
        ]
        .spacing(space::HEADER_GAP)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([space::NAV_Y, space::NAV_X])
    .on_press(Message::Select(h.task_id.clone()))
    .style(theme::fleet_row(selected))
    .into()
}

/// A row's state word. Failure reads red, a question reads brighter than the
/// rest, everything else sits back.
fn tag<'a>(p: Phase) -> El<'a> {
    let t = text(p.tag()).font(font::DATA_MEDIUM).size(size::MICRO);
    match p {
        Phase::Closed(Outcome::Failed) => t.style(theme::text_danger).into(),
        Phase::Waiting => t.style(theme::text_primary).into(),
        _ => t.style(theme::text_tertiary).into(),
    }
}

// ------------------------------------------------------------------ pane

fn pane(app: &App) -> El<'_> {
    let inner: El<'_> = match &app.m.selection {
        Selection::Compose => composer(app),
        Selection::Task(id) => thread_pane(app, id),
    };
    let pad = if app.narrow() {
        [space::PANE_Y_COMPACT, space::PANE_X_COMPACT]
    } else {
        [space::PANE_Y, space::PANE_X]
    };
    let mut col = Column::new().spacing(space::BLOCK).max_width(metrics::THREAD_MAX);
    if app.narrow() {
        col = col.push(row![act("Fleet", Some(Message::Back))].align_y(Alignment::Center));
    }
    if let Some(s) = service(app) {
        col = col.push(s);
    }
    col = col.push(inner);
    container(col)
        .padding(pad)
        .center_x(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// The notice when agentd or policyd is away (A-08 §12). Everything stays
/// visible; controls are inert.
fn service(app: &App) -> Option<El<'_>> {
    let m = &app.m;
    let (head, text_) = match &m.link {
        Link::Connecting => (
            "Connecting to the agent service",
            "Tasks appear here once it answers.",
        ),
        Link::Offline(_) => (
            "Agent service unavailable",
            "Everything is read-only until it reconnects. Pausing still works from the emergency panel.",
        ),
        Link::Online if !m.available => (
            "Policy service unavailable",
            "Tasks are shown as last known. Nothing can start or change until it returns.",
        ),
        Link::Online => return None,
    };
    let retry = matches!(m.link, Link::Offline(_));
    Some(
        row![
            edge_note(head, text_, color::NEUTRAL),
            if retry {
                act("Retry", Some(Message::Retry))
            } else {
                Space::new().into()
            },
        ]
        .spacing(space::HEADER_GAP)
        .align_y(Alignment::Center)
        .into(),
    )
}

fn no_agents_pane(app: &App) -> El<'_> {
    container(
        column![
            text("No agents yet")
                .font(font::UI_SEMIBOLD)
                .size(size::PANE_TITLE)
                .style(theme::text_primary),
            small("Install an agent to dispatch tasks from here."),
            text("ec-ctl agent install <path>")
                .font(font::DATA)
                .size(size::MONO)
                .style(theme::text_secondary),
        ]
        .spacing(space::ROW_Y)
        .align_x(Alignment::Center),
    )
    .center(Length::Fill)
    .style(theme::content_ground(app.blur))
    .into()
}

// ---------------------------------------------------------------- thread

/// What the header and the hero need to know about the selected task, whether
/// it is open (a [`Task`]) or only a record in History.
struct Subject<'a> {
    id: &'a str,
    statement: &'a str,
    package: String,
    version: &'a str,
    phase: Phase,
    state: &'a str,
    reason: &'a str,
    started_ms: u64,
    deadline_ms: u64,
    closed_ms: u64,
    depth: u64,
    min_trust: &'a str,
    pending: u64,
    parent: Option<String>,
}

fn subject<'a>(app: &'a App, id: &'a str) -> Option<Subject<'a>> {
    let m = &app.m;
    if let Some(t) = m.task(id) {
        return Some(Subject {
            id,
            statement: &t.statement,
            package: model::package_name(&m.packages, &t.package),
            version: &t.version,
            phase: Phase::of(t),
            state: &t.state,
            reason: &t.reason,
            started_ms: t.started_ms,
            deadline_ms: t.deadline_ms,
            closed_ms: t.closed_ms.unwrap_or(0),
            depth: t.depth,
            min_trust: &t.min_trust,
            pending: t.pending_decisions,
            parent: t
                .parent
                .as_deref()
                .and_then(|p| m.task(p))
                .map(|p| model::ellipsize(&p.statement, metrics::ROW_CHARS)),
        });
    }
    let h = m.history().iter().find(|h| h.task_id == id)?;
    Some(Subject {
        id,
        statement: &h.statement,
        package: model::package_name(&m.packages, &h.package),
        version: "",
        phase: Phase::Closed(h.outcome),
        state: "closed",
        reason: &h.reason,
        started_ms: 0,
        deadline_ms: 0,
        closed_ms: h.closed_ms,
        depth: 0,
        min_trust: &h.min_trust,
        pending: 0,
        parent: None,
    })
}

fn thread_pane<'a>(app: &'a App, id: &'a str) -> El<'a> {
    let Some(s) = subject(app, id) else {
        return container(small("This task is not in the list.")).into();
    };
    let usable = app.m.usable();

    let mut col = Column::new().spacing(space::BLOCK).height(Length::Fill);
    col = col.push(task_header(app, &s, usable));
    if let Some(pred) = app.m.task(id).and_then(model::resumed_from) {
        let label = app
            .m
            .history()
            .iter()
            .find(|h| h.task_id == pred)
            .map(|h| model::ellipsize(&h.statement, metrics::ROW_CHARS))
            .or_else(|| {
                app.m
                    .task(&pred)
                    .map(|t| model::ellipsize(&t.statement, metrics::ROW_CHARS))
            })
            .unwrap_or_else(|| "the previous task".to_owned());
        col = col.push(link(
            &format!("Resumed from “{label}” ▸"),
            Some(Message::Select(pred)),
        ));
    }
    if app.cancel_open && matches!(s.phase, Phase::Running | Phase::Waiting | Phase::Paused) {
        col = col.push(cancel_menu(usable));
    }
    col = col.push(hero(app, &s));
    col = col.push(transcript(app, &s));
    if let Some(n) = &app.notice {
        col = col.push(small(n.clone()));
    }
    col = col.push(foot(app, &s, usable));
    col.into()
}

fn task_header<'a>(app: &'a App, s: &Subject<'a>, usable: bool) -> El<'a> {
    let live = !s.phase.is_closed();
    // Pieces in a row rather than one string: the type has no useful
    // non-breaking space, and a trailing space in a text run is trimmed away.
    let part = |t: String| {
        text(t)
            .font(font::UI)
            .size(size::BODY)
            .style(theme::text_secondary)
    };
    let mut sub_row = Row::new()
        .spacing(space::PILL_GAP)
        .push(part(s.package.clone()))
        .push(part("·".to_owned()));
    sub_row = if live {
        sub_row.push(
            text(s.phase.label())
                .font(font::UI)
                .size(size::BODY)
                .style(theme::text_accent),
        )
    } else {
        sub_row.push(part(s.phase.label().to_owned()))
    };
    if let Some(p) = &s.parent {
        sub_row = sub_row
            .push(part("·".to_owned()))
            .push(part(format!("subtask of “{p}”")));
    }
    let sub: El<'a> = sub_row.into();

    let mut controls: Vec<El<'a>> = Vec::new();
    match s.phase {
        Phase::Running | Phase::Waiting => {
            controls.push(act("Pause", usable.then_some(Message::Pause)));
            controls.push(act(
                if app.cancel_open {
                    "Cancel ▴"
                } else {
                    "Cancel ▾"
                },
                usable.then_some(Message::ToggleCancel),
            ));
        }
        Phase::Paused => controls.push(act(
            if app.cancel_open {
                "Cancel ▴"
            } else {
                "Cancel ▾"
            },
            usable.then_some(Message::ToggleCancel),
        )),
        Phase::Draining => controls.push(act(
            "Stop now",
            usable.then_some(Message::Cancel(CancelMode::Immediate)),
        )),
        Phase::Closed(_) => {}
    }
    if app.m.decisions > 0 {
        controls.push(act("Show decisions", usable.then_some(Message::ShowDecisions)));
    }
    let (state, measure) = if live {
        (
            format!("{} · depth {}", s.state, s.depth),
            if s.deadline_ms == 0 {
                "no deadline".to_owned()
            } else {
                format!("due {}", model::clock(s.deadline_ms))
            },
        )
    } else {
        (
            if s.reason.is_empty() {
                "closed".to_owned()
            } else {
                format!("closed · {}", s.reason)
            },
            format!("ended {}", model::when(s.closed_ms, app.now)),
        )
    };
    // Title and reading on one line, the controls on their own beneath. Not
    // `header`: a statement can be long, and a title that wraps beside a
    // controls row that also wraps leaves neither readable.
    let title = text(s.statement)
        .font(font::UI_SEMIBOLD)
        .size(size::PANE_TITLE)
        .style(theme::text_primary);
    let mut col = Column::new().spacing(space::HEADER_GAP).push(
        row![
            column![title, sub].spacing(space::TITLE_GAP).width(Length::Fill),
            status_chip(&state, &measure),
        ]
        .spacing(space::FOLD_X),
    );
    if !controls.is_empty() {
        col = col.push(ec_ui::widget::pill_group(controls));
    }
    col.into()
}

/// Finish the current step, or stop now. Two choices, no more: A-04 §8.
fn cancel_menu<'a>(usable: bool) -> El<'a> {
    let item = |title: &str, note: &str, mode: CancelMode| -> El<'a> {
        let b = button(
            column![
                text(title.to_owned())
                    .font(font::UI_MEDIUM)
                    .size(size::BODY)
                    .style(theme::text_primary),
                small(note.to_owned()).style(theme::text_tertiary),
            ]
            .spacing(space::LINE_GAP),
        )
        .width(Length::FillPortion(1))
        .padding([space::ROW_Y, space::CARD])
        .style(theme::glass_cell(false, radius::INSET));
        if usable {
            b.on_press(Message::Cancel(mode)).into()
        } else {
            b.into()
        }
    };
    row![
        item(
            "Finish current step",
            "Stops once this step is done",
            CancelMode::Drain
        ),
        item("Stop now", "Discards work in progress", CancelMode::Immediate),
    ]
    .spacing(space::GRID_GAP)
    .into()
}

fn hero<'a>(app: &'a App, s: &Subject<'a>) -> El<'a> {
    match s.phase {
        Phase::Closed(_) => closed_hero(app, s),
        _ => live_hero(app, s),
    }
}

/// Magnitude plus history: the time left, and how much of the time has gone.
fn live_hero<'a>(app: &'a App, s: &Subject<'a>) -> El<'a> {
    let left = model::left(s.deadline_ms, app.now);
    let (num, unit) = match left.as_str() {
        "no deadline" => ("none".to_owned(), "deadline"),
        "overdue" => ("overdue".to_owned(), ""),
        _ => (left, "left"),
    };
    let frac = model::elapsed_fraction(s.started_ms, s.deadline_ms, app.now);
    let readings = column![
        mono(format!("started {}", model::clock(s.started_ms))),
        mono(if s.deadline_ms == 0 {
            "no deadline set".to_owned()
        } else {
            format!("due {}", model::clock(s.deadline_ms))
        }),
    ]
    .spacing(space::LINE_GAP)
    .align_x(Alignment::End);

    let mut top = Row::new()
        .push(big_value(&num, unit, false))
        .push(Space::new().width(Length::Fill));
    if s.pending > 0 {
        top = top.push(
            column![
                text(s.pending.to_string())
                    .font(font::DATA_MEDIUM)
                    .size(size::BODY)
                    .style(theme::text_primary),
                mono("decisions waiting".to_owned()),
            ]
            .spacing(space::LINE_GAP)
            .align_x(Alignment::End),
        );
        top = top.push(Space::new().width(Length::Fixed(space::BLOCK)));
    }
    top = top.push(readings).align_y(Alignment::End);

    panel(
        app.radius,
        column![top, progress_bar(frac, color::TEXT_SECONDARY)].spacing(space::ROW_Y),
    )
    .into()
}

/// A closed task is not moving, so its hero is a status grid.
fn closed_hero<'a>(app: &'a App, s: &Subject<'a>) -> El<'a> {
    let outcome = s.phase.label();
    let trust = if s.min_trust.is_empty() {
        "unknown"
    } else {
        s.min_trust
    };
    let cells = vec![
        status_cell("outcome", outcome, &model::when(s.closed_ms, app.now), false),
        status_cell("provenance", trust, "least-trusted source", false),
        status_cell(
            "agent",
            &s.package,
            if s.version.is_empty() { "closed" } else { s.version },
            false,
        ),
    ];
    status_grid(cells, if app.narrow() { 1 } else { 3 })
}

fn transcript<'a>(app: &App, s: &Subject<'_>) -> El<'a> {
    let empty = model::Thread::default();
    let t = app.m.thread(s.id).unwrap_or(&empty);
    let mut end: Vec<model::Line> = app.m.task(s.id).and_then(model::end_line).into_iter().collect();
    // Where the restored history ends and this task's own begins (A-08 §8).
    if app.m.task(s.id).and_then(model::resumed_from).is_some() {
        end.push(model::Line {
            time_ms: s.started_ms,
            text: "Restored history ends here. This task starts below.".to_owned(),
            danger: false,
        });
    }
    let mut col = Column::new().spacing(space::BLOCK).width(Length::Fill);
    let entries = model::transcript(t, &end);
    if entries.is_empty() {
        col = col.push(
            container(small(if t.loaded {
                "Nothing has been said yet."
            } else {
                "Reading the conversation"
            }))
            .center_x(Length::Fill),
        );
    }
    for e in entries {
        col = col.push(match e {
            Entry::Message(msg) => message(app, s, msg),
            Entry::Line(l) => event_line(l),
        });
    }
    scrollable(col)
        .style(theme::eclipse_scrollable)
        .anchor_bottom()
        .height(Length::Fill)
        .width(Length::Fill)
        .into()
}

/// A line the console wrote itself: a state it witnessed, how a task ended.
fn event_line<'a>(l: &model::Line) -> El<'a> {
    let t = text(l.text.clone()).font(font::UI).size(size::BODY_SMALL);
    let t = if l.danger {
        t.style(theme::text_danger)
    } else {
        t.style(theme::text_secondary)
    };
    container(
        container(
            row![
                t,
                mono(if l.time_ms > 0 {
                    model::clock(l.time_ms)
                } else {
                    String::new()
                })
            ]
            .spacing(space::HEADER_GAP)
            .align_y(Alignment::Center),
        )
        .padding([space::BADGE_Y * 2.0, space::ROW_Y])
        .style(|_t: &Theme| container::Style {
            border: iced::Border {
                color: color::HAIRLINE,
                width: space::HAIRLINE,
                radius: radius::PILL.into(),
            },
            ..container::Style::default()
        }),
    )
    .center_x(Length::Fill)
    .into()
}

fn message<'a>(app: &App, s: &Subject<'_>, m: &Post) -> El<'a> {
    let when = model::clock(m.time_ms);
    match model::voice(m) {
        Voice::You => {
            let bubble = container(body(model::clean(&m.text)))
                .padding([space::ROW_Y, space::CARD])
                .style(theme::bubble_human);
            row![
                Space::new().width(Length::FillPortion(4 - metrics::BUBBLE_PORTION)),
                column![bubble, mono(format!("You · {when}"))]
                    .spacing(space::LINE_GAP)
                    .align_x(Alignment::End)
                    .width(Length::FillPortion(metrics::BUBBLE_PORTION)),
            ]
            .into()
        }
        Voice::Agent { question } => agent_post(app, s, m, question, false),
        Voice::Context => agent_post(app, s, m, false, true),
    }
}

/// An agent's words. They are untrusted whatever their chain says, so every
/// one wears the same two marks: a darker ground, and the "agent · untrusted"
/// badge. Provenance (`min_trust`, head) is one press away and never in the
/// way.
fn agent_post<'a>(app: &App, s: &Subject<'_>, m: &Post, question: bool, context: bool) -> El<'a> {
    let when = model::clock(m.time_ms);
    let who = if context {
        "Carried over from the previous task".to_owned()
    } else {
        s.package.clone()
    };
    let head = row![
        text(who)
            .font(font::UI_SEMIBOLD)
            .size(size::BODY)
            .style(theme::text_primary),
        badge("agent · untrusted"),
        mono(when),
    ]
    .spacing(space::HEADER_GAP)
    .align_y(Alignment::Center);

    let words = body(model::clean(&m.text));
    let mut inner = Column::new().spacing(space::ROW_Y).push(words);
    if question {
        inner = inner.push(
            text("Waiting for your reply")
                .font(font::UI_MEDIUM)
                .size(size::BODY_SMALL)
                .style(theme::text_primary),
        );
    }
    let card = container(inner)
        .padding([space::ROW_Y, space::CARD])
        .width(Length::Fill)
        .style(theme::bubble_agent);
    let card: El<'a> = if question {
        container(
            row![
                edge_quad(Length::Fixed(space::BAR_W), Length::Fill, color::TEXT_SECONDARY),
                card,
            ]
            .height(Length::Shrink),
        )
        .into()
    } else {
        card.into()
    };

    let open = app.provenance.contains(&(m.task_id.clone(), m.msg_id));
    let mut col = Column::new()
        .spacing(space::LINE_GAP)
        .push(head)
        .push(card)
        .push(link(
            if open { "Provenance ▾" } else { "Provenance ▸" },
            Some(Message::ToggleProvenance(m.task_id.clone(), m.msg_id)),
        ));
    if open {
        let trust = if m.trust.min_trust.is_empty() {
            "unknown".to_owned()
        } else {
            m.trust.min_trust.clone()
        };
        let head_src = if m.trust.head.is_empty() {
            "unknown".to_owned()
        } else {
            model::clean(&m.trust.head)
        };
        col = col.push(
            inset(
                column![prov_row("min_trust", trust), prov_row("head", head_src),].spacing(space::LINE_GAP),
            )
            .padding([space::ROW_Y, space::CARD]),
        );
    }
    row![
        col.width(Length::FillPortion(metrics::BUBBLE_PORTION)),
        Space::new().width(Length::FillPortion(4 - metrics::BUBBLE_PORTION)),
    ]
    .into()
}

fn prov_row<'a>(k: &str, v: String) -> El<'a> {
    row![
        container(mono(k.to_owned())).width(Length::Fixed(space::SWATCH * 4.0)),
        text(v)
            .font(font::DATA)
            .size(size::MONO)
            .style(theme::text_primary),
    ]
    .spacing(space::HEADER_GAP)
    .into()
}

// ------------------------------------------------------------------ foot

fn foot<'a>(app: &'a App, s: &Subject<'a>, usable: bool) -> El<'a> {
    if !usable {
        return notice_bar("Read-only while the agent service is unavailable.");
    }
    match s.phase {
        Phase::Running | Phase::Waiting => reply_bar(app, s),
        Phase::Draining => notice_bar("Finishing the current step. Replies are closed."),
        Phase::Paused => resume_slot(app),
        Phase::Closed(_) => closed_bar(s),
    }
}

fn notice_bar<'a>(t: &str) -> El<'a> {
    container(small(t.to_owned()))
        .padding([space::ROW_Y, space::CARD])
        .width(Length::Fill)
        .style(theme::editor_ground(false))
        .into()
}

fn reply_bar<'a>(app: &'a App, s: &Subject<'a>) -> El<'a> {
    let waiting = s.phase == Phase::Waiting;
    let ready = !app.reply_text().is_empty();
    let editor = text_editor(&app.reply)
        .placeholder(if waiting {
            format!("Reply to {}", s.package)
        } else {
            format!("Message {}", s.package)
        })
        .on_action(Message::EditReply)
        .key_binding(|kp| {
            use iced::keyboard::{key::Named, Key};
            let enter = matches!(kp.key.as_ref(), Key::Named(Named::Enter));
            if enter && !kp.modifiers.shift() && matches!(kp.status, text_editor::Status::Focused { .. }) {
                Some(text_editor::Binding::Custom(Message::SendReply))
            } else {
                text_editor::Binding::from_key_press(kp)
            }
        })
        .style(theme::bare_editor)
        .font(font::UI)
        .size(size::BODY)
        .padding(0)
        .min_height(metrics::EDITOR_MIN)
        .max_height(metrics::EDITOR_MAX);

    let send = button(text("Send").font(font::UI_MEDIUM).size(size::BODY_SMALL))
        .padding([space::PILL_Y, space::PILL_X])
        .on_press_maybe(ready.then_some(Message::SendReply))
        .style(theme::send(ready));

    column![
        container(
            row![container(editor).width(Length::Fill), send]
                .spacing(space::HEADER_GAP)
                .align_y(Alignment::End),
        )
        .padding([space::ROW_Y, space::CARD])
        .width(Length::Fill)
        .style(theme::editor_ground(waiting)),
        mono("Enter to send · Shift+Enter for a new line".to_owned()),
    ]
    .spacing(space::LINE_GAP)
    .into()
}

/// A paused task is resumed in the compositor's card, not by a button here.
fn resume_slot(app: &App) -> El<'_> {
    let mut col = Column::new()
        .spacing(space::ROW_Y)
        .align_x(Alignment::Center)
        .width(Length::Fill);
    col = col.push(small(
        "Paused by you. Resuming is done in the card below, which the compositor draws.",
    ));
    col = col.push(slot_hole(app));
    if let Some(note) = model::slot_note(app.shield.state) {
        col = col.push(mono(note.to_owned()));
    }
    col.into()
}

fn closed_bar<'a>(s: &Subject<'a>) -> El<'a> {
    let id = s.id.to_owned();
    container(
        row![
            small("Resume continues with its history. Reply or Start fresh begin a new task.")
                .width(Length::Fill),
            act("Resume", Some(Message::Resume(id.clone()))),
            act("Reply as new task", Some(Message::ContinueFrom(id.clone()))),
            act("Start fresh", Some(Message::StartFresh(id.clone()))),
            link("Delete", Some(Message::DeleteSession(id))),
        ]
        .spacing(space::HEADER_GAP)
        .align_y(Alignment::Center),
    )
    .padding([space::ROW_Y, space::CARD])
    .width(Length::Fill)
    .style(theme::editor_ground(false))
    .into()
}

// -------------------------------------------------------------- composer

/// The composer. Its yellow is the chosen deadline; the statement, the agent
/// and the follow-up are white. What is typed here is a *draft*: it goes to the
/// compositor's card and nowhere else, and the card, not this screen, commits
/// it.
fn composer(app: &App) -> El<'_> {
    let m = &app.m;
    let usable = m.usable();
    let first = m.listed && model::fleet(&m.tasks).is_empty() && m.history().is_empty();
    let title = if first {
        "What should get done?"
    } else {
        "New task"
    };

    let head = header(
        title,
        subtitle(
            "Pick an agent and say what you want. You commit it in the card below, with the keyboard or the pointer. ",
            "",
            "",
        ),
        vec![status_chip(
            &link_word(&m.link, m.available),
            &format!("{} agent{}", m.packages.len(), if m.packages.len() == 1 { "" } else { "s" }),
        )],
    );

    let mut col = Column::new()
        .spacing(space::BLOCK)
        .height(Length::Fill)
        .push(head);

    if let Some(id) = &app.follow {
        let (stmt, trust) = m
            .task(id)
            .map(|t| (t.statement.clone(), t.min_trust.clone()))
            .or_else(|| {
                m.history()
                    .iter()
                    .find(|h| &h.task_id == id)
                    .map(|h| (h.statement.clone(), h.min_trust.clone()))
            })
            .unwrap_or_default();
        let word = m
            .history()
            .iter()
            .find(|h| &h.task_id == id)
            .map(|h| outcome_word(h.outcome))
            .unwrap_or("open");
        let mut lines = Column::new().spacing(space::LINE_GAP).push(
            text(format!(
                "Follows “{}”",
                model::ellipsize(&stmt, metrics::ROW_CHARS * 2)
            ))
            .font(font::UI_MEDIUM)
            .size(size::BODY)
            .style(theme::text_primary),
        );
        lines = lines.push(mono(format!(
            "{word} · provenance {}",
            if trust.is_empty() { "unknown" } else { &trust }
        )));
        col = col.push(
            inset(
                row![
                    lines.width(Length::Fill),
                    link("Remove", Some(Message::ClearFollow))
                ]
                .spacing(space::HEADER_GAP)
                .align_y(Alignment::Center),
            )
            .padding([space::ROW_Y, space::CARD]),
        );
    }

    // `/resume` opens the same list History offers (A-08 §8).
    if app.resume.is_none() && app.statement().trim() == "/resume" {
        col = col.push(resume_picker(app));
    }

    // A resume shows the session's statement, verbatim and not editable (the
    // compositor will only commit it unchanged), and an optional separate
    // instruction that becomes the first message of the new task.
    if let Some((_, original)) = &app.resume {
        col = col.push(
            container(
                column![
                    micro_label("Resuming, statement unchanged"),
                    text(model::clean(original))
                        .font(font::UI)
                        .size(size::PROMPT)
                        .style(theme::text_primary),
                    row![
                        text_input("Add an instruction (optional)", &app.instruction)
                            .on_input_maybe(usable.then_some(Message::EditInstruction))
                            .style(theme::eclipse_input)
                            .font(font::UI)
                            .size(size::BODY)
                            .width(Length::Fill),
                        link("Cancel resume", Some(Message::ClearResume)),
                    ]
                    .spacing(space::HEADER_GAP)
                    .align_y(Alignment::Center),
                ]
                .spacing(space::ROW_Y),
            )
            .padding(space::CARD)
            .width(Length::Fill)
            .style(theme::editor_ground(false)),
        );
    }

    // The hero: the statement, at prompt scale.
    let chars = app.statement().chars().count();
    let over = chars > model::STATEMENT_MAX;
    let mut editor = text_editor(&app.draft)
        .placeholder("Describe the task")
        .style(theme::bare_editor)
        .font(font::UI)
        .size(size::PROMPT)
        .padding(0)
        .min_height(metrics::COMPOSER_MIN)
        .max_height(metrics::COMPOSER_MAX);
    if usable {
        editor = editor.on_action(Message::EditDraft);
    }
    let editor: El<'_> = editor.into();
    // iced's tiny-skia screenshot path draws an editor from a weak reference
    // that any later layout invalidates, so a fixture's prefilled statement is
    // drawn as plain text there. Debug builds only.
    #[cfg(debug_assertions)]
    let editor: El<'_> = if app.fixture && chars > 0 {
        container(
            text(app.statement())
                .font(font::UI)
                .size(size::PROMPT)
                .style(theme::text_primary),
        )
        .height(Length::Fixed(metrics::COMPOSER_MIN))
        .into()
    } else {
        editor
    };
    let counter = text(format!("{chars} / {}", model::STATEMENT_MAX))
        .font(font::DATA)
        .size(size::MICRO)
        .style(if over {
            theme::text_danger
        } else {
            theme::text_tertiary
        });
    if app.resume.is_none() {
        col = col.push(
            container(column![editor, row![Space::new().width(Length::Fill), counter]].spacing(space::ROW_Y))
                .padding([space::CARD, space::CARD])
                .width(Length::Fill)
                .style(theme::editor_ground(false)),
        );
    }

    // The agent (neutral) and the deadline (the pane's yellow).
    let mut agents: Vec<El<'_>> = Vec::new();
    // A resume keeps its package: the compositor previews the session against
    // the installed version of that agent.
    for p in m
        .packages
        .iter()
        .filter(|p| app.resume.is_none() || app.package.as_deref() == Some(p.id.as_str()))
    {
        let name = if p.name.is_empty() {
            p.id.clone()
        } else {
            p.name.clone()
        };
        agents.push(pick_pill(
            &name,
            app.package.as_deref() == Some(p.id.as_str()),
            (usable && app.resume.is_none()).then(|| Message::PickPackage(p.id.clone())),
        ));
    }
    let deadlines: Vec<El<'_>> = model::Deadline::ALL
        .iter()
        .map(|d| {
            if usable {
                pill(d.label(), app.deadline == *d, Message::PickDeadline(*d))
            } else {
                act(d.label(), None)
            }
        })
        .collect();
    col = col.push(
        column![
            row![micro_label("Agent"), ec_ui::widget::pill_group(agents)]
                .spacing(space::BLOCK)
                .align_y(Alignment::Center),
            row![micro_label("Deadline"), ec_ui::widget::pill_group(deadlines)]
                .spacing(space::BLOCK)
                .align_y(Alignment::Center),
        ]
        .spacing(space::ROW_Y),
    );

    if over && app.resume.is_none() {
        col = col.push(small(format!(
            "A statement is at most {} characters. The card will not arm until it is shorter.",
            model::STATEMENT_MAX
        )));
    }

    // The compositor's card goes here.
    col = col.push(slot_area(app));
    if let Some(n) = &app.notice {
        col = col.push(small(n.clone()));
    }
    scrollable(col)
        .style(theme::eclipse_scrollable)
        .height(Length::Fill)
        .into()
}

/// The `/resume` picker: History's closed tasks, each with Resume.
fn resume_picker(app: &App) -> El<'_> {
    let rows: Vec<El<'_>> = app
        .m
        .history()
        .iter()
        .map(|h| {
            container(
                row![
                    column![
                        text(model::ellipsize(&h.statement, metrics::ROW_CHARS * 2))
                            .font(font::UI)
                            .size(size::BODY)
                            .style(theme::text_primary),
                        mono(format!(
                            "{} · {}",
                            outcome_word(h.outcome),
                            model::when(h.closed_ms, app.now)
                        )),
                    ]
                    .spacing(space::LINE_GAP)
                    .width(Length::Fill),
                    act(
                        "Resume",
                        app.m.usable().then(|| Message::Resume(h.task_id.clone()))
                    ),
                ]
                .spacing(space::HEADER_GAP)
                .align_y(Alignment::Center),
            )
            .padding([space::ROW_Y, space::CARD])
            .into()
        })
        .collect();
    if rows.is_empty() {
        return inset(small("No closed tasks to resume."))
            .padding([space::ROW_Y, space::CARD])
            .into();
    }
    inset_list("RESUME A TASK", rows)
}

fn slot_area(app: &App) -> El<'_> {
    match &app.shield.guard {
        Guard::Off(why) => edge_note(
            "Dispatch needs the compositor's card",
            &format!("This window is not protected, so there is nowhere to commit a task. {why}."),
            color::NEUTRAL,
        ),
        _ => {
            let mut col = Column::new()
                .spacing(space::ROW_Y)
                .align_x(Alignment::Center)
                .width(Length::Fill);
            col = col.push(slot_hole(app));
            let note = model::slot_note(app.shield.state).or(
                if app.shield.state.is_none() && app.shield.protecting() {
                    Some("Waiting for the compositor's card.")
                } else {
                    None
                },
            );
            if let Some(n) = note {
                col = col.push(mono(n.to_owned()));
            }
            col.into()
        }
    }
}

/// The hole the compositor draws its card in: exactly the size it reported
/// (`geometry`), laid out and left empty.
fn slot_hole(app: &App) -> El<'_> {
    let (w, h) = app
        .shield
        .size
        .map_or((metrics::HOLE_W, metrics::HOLE_H), |(w, h)| (w as f32, h as f32));
    let known = app.shield.hole.map(|(x, y)| iced::Point::new(x as f32, y as f32));
    let hole: El<'_> = Hole::new(Size::new(w, h), known, Message::HoleAt).into();
    #[cfg(debug_assertions)]
    let hole = crate::fixture::mark_hole(hole, w, h);
    container(hole).center_x(Length::Fill).into()
}

#[allow(dead_code)]
fn rule<'a>() -> El<'a> {
    hairline()
}
