// SPDX-License-Identifier: AGPL-3.0-only
//! The console socket, on one thread.
//!
//! `Console::call` blocks for up to ten seconds, so it never runs on the UI
//! thread. One worker owns the connection: it takes commands from the UI over
//! a channel, drains the event stream between them, and forwards both kinds of
//! answer to iced as messages. The wait is a thread, not a futures timer,
//! because iced's `time::every` needs a tokio or smol backend and this feature
//! set enables neither (the same reasoning as `ec-settings`).
//!
//! This is the console's only route to agentd. It has no create, unpause,
//! resume or answer-a-prompt call, because `ec_console_client::console` has
//! none and the server has none (A-08 §7).

use std::{
    sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender},
    time::{Duration, Instant},
};

use ec_console_client::console::{
    CancelMode, Console, Conversation, Error, Event, MsgId, Package, Posted, Session, TaskList,
};
use iced::Subscription;

use crate::app::Message;

/// How often the worker wakes with nothing to do: it drains events and, for
/// the UI, ticks. Short enough that a slot's `geometry` shows within a blink.
const POLL: Duration = Duration::from_millis(120);
/// Between reconnect attempts while agentd is away.
const RETRY: Duration = Duration::from_secs(2);

/// A request from the UI to the worker.
#[derive(Debug, Clone)]
pub enum Cmd {
    /// Connect again now (the Retry control).
    Reconnect,
    Refresh,
    Read(String),
    Post {
        task_id: String,
        text: String,
        reply_to: Option<MsgId>,
    },
    Pause(String),
    Cancel(String, CancelMode),
    ShowDecisions,
    /// Ask for the compositor's secret-store unlock (or set-up) prompt,
    /// which agentd forwards. Its result is not returned: the account list's
    /// next read shows it.
    UnlockSecrets,
    DeleteSession(String),
}

/// What the worker learned.
#[derive(Debug, Clone)]
pub enum Update {
    Online {
        available: bool,
        decisions: u32,
    },
    Offline(String),
    Packages(Vec<Package>),
    Tasks(TaskList),
    Sessions(Vec<Session>),
    Thread(String, Conversation),
    Event(Event),
    Posted(String, Posted),
    /// A call failed. `unavailable` is the server saying policyd or the
    /// compositor is away, which is not the same as a refusal.
    Failed {
        what: &'static str,
        text: String,
        unavailable: bool,
    },
}

/// The UI's end of the command channel.
#[derive(Clone)]
pub struct Handle(Sender<Cmd>);

impl std::fmt::Debug for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Handle")
    }
}

impl Handle {
    pub fn send(&self, c: Cmd) {
        let _ = self.0.send(c);
    }
}

/// The subscription: starts the worker, hands the UI its [`Handle`] first, then
/// forwards updates and a heartbeat. The heartbeat is what lets the UI thread
/// pump the protected-surface queue and move the clock without a timer of its
/// own.
pub fn feed() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(64, async move |mut sender| {
            let (tx, rx) = channel::<Cmd>();
            if sender.try_send(Message::Ready(Handle(tx))).is_err() {
                return;
            }
            std::thread::spawn(move || {
                let mut out = Out(sender);
                work(&rx, &mut out);
            });
        })
    })
}

struct Out(iced::futures::channel::mpsc::Sender<Message>);

impl Out {
    /// False once the UI has gone away.
    fn update(&mut self, u: Update) -> bool {
        // A full queue drops the heartbeat but never an update: updates block
        // briefly instead, because losing one would leave the view wrong.
        loop {
            match self.0.try_send(Message::Net(u.clone())) {
                Ok(()) => return true,
                Err(e) if e.is_full() => std::thread::sleep(Duration::from_millis(5)),
                Err(_) => return false,
            }
        }
    }

    fn tick(&mut self) -> bool {
        match self.0.try_send(Message::Tick) {
            Ok(()) => true,
            Err(e) => e.is_full(),
        }
    }
}

fn work(rx: &Receiver<Cmd>, out: &mut Out) {
    let mut console: Option<Console> = None;
    let mut next_try = Instant::now();
    loop {
        if console.is_none() && Instant::now() >= next_try {
            match connect(out) {
                Ok(c) => console = Some(c),
                Err(why) => {
                    if !out.update(Update::Offline(why)) {
                        return;
                    }
                    next_try = Instant::now() + RETRY;
                }
            }
        }

        let mut dropped = false;
        if let Some(c) = &console {
            while let Ok(ev) = c.events().try_recv() {
                dropped |= ev == Event::Disconnected;
                if !out.update(Update::Event(ev)) {
                    return;
                }
            }
        }
        if dropped {
            console = None;
            next_try = Instant::now() + RETRY;
        }

        match rx.recv_timeout(POLL) {
            Ok(Cmd::Reconnect) => {
                console = None;
                next_try = Instant::now();
            }
            Ok(cmd) => {
                if let Some(c) = &console {
                    if !run(c, cmd, out) {
                        return;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if !out.tick() {
            return;
        }
    }
}

/// Connect, subscribe, and fetch the first picture.
fn connect(out: &mut Out) -> Result<Console, String> {
    let c = Console::connect().map_err(|e| e.to_string())?;
    let sub = c.subscribe().map_err(|e| e.to_string())?;
    if !out.update(Update::Online {
        available: sub.available,
        decisions: sub.decisions_pending,
    }) {
        return Err("closed".into());
    }
    refresh(&c, out);
    Ok(c)
}

fn fail(out: &mut Out, what: &'static str, e: &Error) -> bool {
    out.update(Update::Failed {
        what,
        text: e.to_string(),
        unavailable: e.is_unavailable() || matches!(e, Error::Disconnected | Error::Timeout),
    })
}

/// Everything the fleet and History need.
fn refresh(c: &Console, out: &mut Out) -> bool {
    match c.list_packages() {
        Ok(p) => {
            if !out.update(Update::Packages(p)) {
                return false;
            }
        }
        Err(e) => {
            if !fail(out, "packages", &e) {
                return false;
            }
        }
    }
    match c.list_tasks(None) {
        Ok(t) => {
            if !out.update(Update::Tasks(t)) {
                return false;
            }
        }
        Err(e) => {
            if !fail(out, "tasks", &e) {
                return false;
            }
        }
    }
    match c.list_sessions(None) {
        Ok(s) => out.update(Update::Sessions(s)),
        Err(e) => fail(out, "history", &e),
    }
}

/// One command. False once the UI has gone.
fn run(c: &Console, cmd: Cmd, out: &mut Out) -> bool {
    match cmd {
        Cmd::Reconnect => true,
        Cmd::Refresh => refresh(c, out),
        Cmd::Read(id) => match c.conversation_read(&id, None) {
            Ok(conv) => out.update(Update::Thread(id, conv)),
            Err(e) => fail(out, "conversation", &e),
        },
        Cmd::Post {
            task_id,
            text,
            reply_to,
        } => match c.conversation_post(&task_id, &text, reply_to) {
            Ok(p) => out.update(Update::Posted(task_id, p)),
            Err(e) if e.is_closed() => out.update(Update::Failed {
                what: "reply",
                text: "This conversation is closed.".into(),
                unavailable: false,
            }),
            Err(e) => fail(out, "reply", &e),
        },
        Cmd::Pause(id) => match c.pause_task(&id) {
            Ok(()) => refresh(c, out),
            Err(e) => fail(out, "pause", &e),
        },
        Cmd::Cancel(id, mode) => match c.cancel_task(&id, mode) {
            Ok(()) => refresh(c, out),
            Err(e) => fail(out, "cancel", &e),
        },
        Cmd::ShowDecisions => match c.show_decisions() {
            Ok(()) => true,
            // 1/s: pressing twice is not an error worth a notice.
            Err(e) if e.is_rate_limited() => true,
            Err(e) => fail(out, "decisions", &e),
        },
        Cmd::UnlockSecrets => match c.unlock_secrets() {
            Ok(()) => true,
            // Pressing twice is not an error worth a notice.
            Err(e) if e.is_rate_limited() => true,
            Err(e) => fail(out, "unlock", &e),
        },
        Cmd::DeleteSession(id) => match c.delete_session(&id) {
            Ok(()) => refresh(c, out),
            // Gone already: the refresh shows it.
            Err(e) if e.is_not_found() => refresh(c, out),
            Err(e) => fail(out, "delete", &e),
        },
    }
}
