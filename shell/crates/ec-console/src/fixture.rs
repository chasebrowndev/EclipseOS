// SPDX-License-Identifier: AGPL-3.0-only
//! Debug builds only: canned states for the screenshot loop.
//!
//! `EC_CONSOLE_FIXTURE=<name>` starts the console with a made-up model and no
//! connection to agentd, so every state in the design can be looked at without
//! a running agent. `EC_CONSOLE_SHOT=<path>` then writes the window's pixels
//! (raw RGBA, `W H` on the first line) and exits, and `EC_CONSOLE_MARK_HOLE=1`
//! outlines the commit slot's hole so its size and place can be checked. None
//! of this exists in a release build, and none of it can reach a compositor:
//! the fixture's "protected" window is a flag, not a protected surface.

use std::sync::atomic::{AtomicU32, Ordering};

use ec_console_client::console::{Message as Post, Package, Session, Task, TaskList, Trust};
use ec_console_client::protected::{SlotReason, SlotState};
use iced::widget::{container, stack, Space};
use iced::{Element, Length, Subscription, Task as Cmd, Theme};

use crate::app::{App, Message};
use crate::model::{Deadline, Link, Selection};
use crate::shield::Guard;

const MIN: u64 = 60_000;

pub fn name() -> Option<String> {
    std::env::var("EC_CONSOLE_FIXTURE").ok().filter(|s| !s.is_empty())
}

/// The window size the fixture wants, `WxH`.
pub fn size() -> Option<(f32, f32)> {
    let v = std::env::var("EC_CONSOLE_SIZE").ok()?;
    let (w, h) = v.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

fn pkg(id: &str, name: &str) -> Package {
    Package {
        id: id.into(),
        name: name.into(),
        publisher: "fixture".into(),
        version: "1.0".into(),
    }
}

#[allow(clippy::too_many_arguments)]
fn task(
    id: &str,
    package: &str,
    statement: &str,
    state: &str,
    now: u64,
    started_ago: u64,
    left: u64,
) -> Task {
    Task {
        task_id: id.into(),
        package: package.into(),
        version: "1.0".into(),
        statement: statement.into(),
        state: state.into(),
        reason: String::new(),
        deadline_ms: now + left * MIN,
        started_ms: now - started_ago * MIN,
        closed_ms: None,
        continuation: String::new(),
        min_trust: String::new(),
        depth: 0,
        awaiting_reply: false,
        pending_decisions: 0,
        parent: None,
        children: Vec::new(),
        raw: serde_json::Value::Null,
    }
}

fn closed(mut t: Task, reason: &str, ended_ago: u64, now: u64, trust: &str) -> Task {
    t.state = "closed".into();
    t.reason = reason.into();
    t.closed_ms = Some(now - ended_ago * MIN);
    t.min_trust = trust.into();
    t
}

fn say(task: &str, id: u64, kind: &str, text: &str, at: u64, trust: &str, head: &str) -> Post {
    Post {
        task_id: task.into(),
        msg_id: id,
        time_ms: at,
        kind: kind.into(),
        text: text.into(),
        reply_to: None,
        trust: Trust {
            min_trust: trust.into(),
            head: head.into(),
        },
    }
}

pub fn apply(app: &mut App) {
    let Some(sc) = name() else {
        return;
    };
    app.fixture = true;
    let now = app.now;
    let m = &mut app.m;
    m.link = Link::Online;
    m.available = true;
    m.packages = vec![
        pkg("inv", "Invoice Triage"),
        pkg("ven", "Vendor Research"),
        pkg("inbox", "Inbox Sweeper"),
        pkg("cal", "Calendar Steward"),
        pkg("notes", "Meeting Notes"),
    ];
    app.package = Some("inv".into());

    let mut t1 = task(
        "t1",
        "inv",
        "Summarize this week's invoices",
        "active",
        now,
        91,
        72,
    );
    let mut t1a = task("t1a", "inv", "Extract totals from 14 PDFs", "active", now, 78, 48);
    t1a.parent = Some("t1".into());
    t1a.depth = 1;
    let mut t3 = task(
        "t3",
        "inbox",
        "Draft replies to unread client email",
        "active",
        now,
        120,
        184,
    );
    let t4 = task(
        "t4",
        "cal",
        "Move Thursday's 1:1s to Friday",
        "paused",
        now,
        20,
        22,
    );
    let t9 = task(
        "t9",
        "notes",
        "Transcribe today's design review",
        "draining",
        now,
        40,
        5,
    );
    let t2 = closed(
        task("t2", "ven", "Compare three hosting quotes", "closed", now, 140, 0),
        "completed",
        45,
        now,
        "untrusted",
    );
    let t5 = closed(
        task(
            "t5",
            "notes",
            "Clean up Monday's standup notes",
            "closed",
            now,
            3000,
            0,
        ),
        "completed",
        2900,
        now,
        "standard",
    );
    let t6 = closed(
        task(
            "t6",
            "ven",
            "Find a cheaper domain registrar",
            "closed",
            now,
            2700,
            0,
        ),
        "cancelled",
        2600,
        now,
        "untrusted",
    );
    let t7 = closed(
        task(
            "t7",
            "inbox",
            "Archive newsletters older than 30 days",
            "closed",
            now,
            4300,
            0,
        ),
        "failed",
        4290,
        now,
        "standard",
    );

    let at = |ago: u64| now - ago * MIN;
    let mut threads = vec![
        (
            "t1",
            vec![
                say("t1", 1, "human", "Summarize this week's invoices. Flag anything over $5,000.", at(90), "standard", "human"),
                say("t1", 2, "say", "Found 14 invoices from 9 vendors in Documents > Invoices. Reading the PDFs in a subtask.", at(89), "standard", "fs:Documents/Invoices"),
                say("t1", 3, "say", "Acme Invoicing's portal lists two more invoices that are not in your folder. Their page marks both as overdue.", at(78), "untrusted", "web:acme-invoices.com"),
            ],
        ),
        (
            "t1a",
            vec![say("t1a", 1, "say", "Read 9 of 14 PDFs. Running total so far is $18,420.", at(70), "standard", "fs:Documents/Invoices")],
        ),
        (
            "t3",
            vec![
                say("t3", 1, "human", "Draft replies to unread client email. Ask before sending anything.", at(120), "standard", "human"),
                say("t3", 2, "say", "Drafted 5 replies. Two are ready to send, to Northwind and Halcyon. The other three need details only you have.", at(100), "untrusted", "mail:12 external senders"),
            ],
        ),
        (
            "t4",
            vec![
                say("t4", 1, "human", "Move Thursday's 1:1s to Friday afternoon.", at(20), "standard", "human"),
                say("t4", 2, "say", "Moved 2 of 4. Priya and Sam are busy Friday after 3pm.", at(12), "untrusted", "calendar:shared"),
            ],
        ),
        (
            "t9",
            vec![
                say("t9", 1, "human", "Transcribe today's design review and list the decisions.", at(40), "standard", "human"),
                say("t9", 2, "say", "Transcribed 38 of 52 minutes.", at(8), "standard", "fs:Recordings/Design review.m4a"),
            ],
        ),
        (
            "t2",
            vec![
                say("t2", 1, "human", "Compare three hosting quotes and recommend one.", at(140), "standard", "human"),
                say("t2", 2, "say", "Northwind is cheapest at $84 a month with the same storage. Halcyon includes support hours the others charge for. Northwind, unless you expect to need support.", at(45), "untrusted", "web:northwind.host"),
            ],
        ),
        (
            "t6",
            vec![
                say("t6", 1, "human", "Find a cheaper domain registrar for eclipse-studio.net.", at(2700), "standard", "human"),
                say("t6", 2, "say", "Checked 4 registrars so far. Two list lower renewal prices.", at(2620), "untrusted", "web:namewell.com"),
            ],
        ),
        (
            "t7",
            vec![
                say("t7", 1, "human", "Archive newsletters older than 30 days.", at(4300), "standard", "human"),
                say("t7", 2, "say", "Found 212 newsletters older than 30 days.", at(4295), "standard", "mail"),
            ],
        ),
    ];

    let question = say(
        "t1",
        4,
        "ask",
        "Two invoices appear in both Invoices and Invoices 2025. Which folder should count?",
        at(5),
        "standard",
        "fs:Documents/Invoices, Documents/Invoices 2025",
    );

    let mut sel = Selection::Task("t1".into());
    let mut decisions = 0;
    match sc.as_str() {
        "waiting" => {
            t1.awaiting_reply = true;
            threads[0].1.push(question);
        }
        "decisions" => {
            t3.pending_decisions = 2;
            decisions = 2;
            sel = Selection::Task("t3".into());
        }
        "paused" => sel = Selection::Task("t4".into()),
        "cancelmenu" => app.cancel_open = true,
        "draining" => sel = Selection::Task("t9".into()),
        "finished" => sel = Selection::Task("t2".into()),
        "cancelled" => sel = Selection::Task("t6".into()),
        "failed" => sel = Selection::Task("t7".into()),
        "prov" => {
            app.provenance.insert(("t1".into(), 3));
        }
        "composer" | "composer-ready" | "followup" | "notasks" | "noagents" | "resume" | "slash-resume" => {
            sel = Selection::Compose;
        }
        "resumed" => sel = Selection::Task("t10".into()),
        "down" => {
            m.link = Link::Offline("no socket".into());
            m.available = false;
        }
        "narrow-list" | "narrow-thread" => {
            t1.awaiting_reply = true;
            threads[0].1.push(question);
            decisions = 2;
        }
        _ => {}
    }
    let mut t10 = task(
        "t10",
        "ven",
        "Compare three hosting quotes",
        "active",
        now,
        6,
        110,
    );
    t10.raw = serde_json::json!({"resumes": "t2"});
    threads.push((
        "t10",
        vec![say(
            "t10",
            1,
            "say",
            "Picking up where the last run stopped. Re-reading the three quotes first.",
            at(5),
            "untrusted",
            "web:northwind.host",
        )],
    ));
    let mut tasks = vec![t1, t1a, t3, t4, t9, t2, t5, t6, t7];
    if sc == "resumed" {
        tasks.push(t10);
    }
    if sc == "notasks" || sc == "noagents" {
        tasks.clear();
    }
    if sc == "noagents" {
        m.packages.clear();
        app.package = None;
    }
    m.tasks = tasks;
    m.sessions = Vec::<Session>::new();
    m.decisions = decisions;
    m.selection = sel;
    m.listed = true;
    for (id, msgs) in threads {
        let th = m.threads.entry(id.to_owned()).or_default();
        for x in msgs {
            th.push(x);
        }
        th.loaded = true;
    }
    m.set_tasks(TaskList {
        tasks: m.tasks.clone(),
        available: m.available,
        decisions_pending: decisions,
    });
    if sc == "paused" {
        let th = m.threads.entry("t4".into()).or_default();
        th.note(at(2), "Paused", false);
    }
    if sc == "draining" {
        let th = m.threads.entry("t9".into()).or_default();
        th.note(at(1), "Cancel requested · finishing the current step", false);
    }

    match sc.as_str() {
        "composer-ready" => {
            app.package = Some("inbox".into());
            app.deadline = Deadline::Long;
            app.draft = crate::app::content_with("Draft a reply to Halcyon accepting the revised quote.");
        }
        "resume" => {
            app.package = Some("ven".into());
            app.resume = Some(("t2".into(), "Compare three hosting quotes".into()));
            app.instruction = "Also check support hours".into();
        }
        "slash-resume" => app.draft = crate::app::content_with("/resume"),
        "followup" => {
            app.package = Some("ven".into());
            app.deadline = Deadline::Day;
            app.follow = Some("t2".into());
            app.draft = crate::app::content_with("Book the cheapest of the three hosting plans.");
        }
        _ => {}
    }

    if matches!(sc.as_str(), "narrow-list" | "narrow-thread") {
        app.width = 400.0;
        app.pane_open = sc == "narrow-thread";
    } else {
        app.pane_open = true;
    }
    if let Some((w, _)) = size() {
        app.width = w;
    }

    // Layout only: a flag where the real thing would be a protected surface.
    app.shield.guard = Guard::On;
    if matches!(sc.as_str(), "composer-ready" | "followup") {
        app.shield.state = Some((SlotState::Armed, SlotReason::None));
    } else if sc == "paused" || sc.starts_with("composer") {
        app.shield.state = Some((SlotState::Disarmed, SlotReason::NotFocused));
    }
}

/// Outline the hole, so a screenshot shows where the compositor's card would
/// be. Debug only, and only when asked.
pub fn mark_hole<'a>(hole: Element<'a, Message, Theme>, w: f32, h: f32) -> Element<'a, Message, Theme> {
    if std::env::var_os("EC_CONSOLE_MARK_HOLE").is_none() {
        return hole;
    }
    let outline = container(Space::new())
        .width(Length::Fixed(w))
        .height(Length::Fixed(h))
        .style(|_t: &Theme| container::Style {
            border: iced::Border {
                color: ec_ui::tokens::color::CONNECTED,
                width: 1.0,
                radius: 0.0.into(),
            },
            ..container::Style::default()
        });
    stack![hole, outline].into()
}

static TICKS: AtomicU32 = AtomicU32::new(0);

/// A heartbeat when there is no worker to give one.
pub fn ticks() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(8, async move |mut out| {
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_millis(250));
                if out.try_send(Message::Tick).is_err() && out.is_closed() {
                    return;
                }
            });
        })
    })
}

/// Called on each heartbeat of a fixture: after a few, take the picture.
pub fn on_tick(app: &App) -> Cmd<Message> {
    let _ = app;
    if std::env::var_os("EC_CONSOLE_SHOT").is_none() {
        return Cmd::none();
    }
    if TICKS.fetch_add(1, Ordering::Relaxed) == 12 {
        return iced::window::oldest()
            .and_then(iced::window::screenshot)
            .map(Message::Shot);
    }
    Cmd::none()
}

/// Write the picture and leave.
pub fn save(shot: &iced::window::Screenshot) -> Cmd<Message> {
    if let Some(path) = std::env::var_os("EC_CONSOLE_SHOT") {
        let mut bytes = format!("{} {}\n", shot.size.width, shot.size.height).into_bytes();
        bytes.extend_from_slice(&shot.rgba);
        let _ = std::fs::write(path, bytes);
    }
    iced::exit()
}
