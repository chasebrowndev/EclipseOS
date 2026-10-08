// SPDX-License-Identifier: AGPL-3.0-only
//! State, messages and `update`. The console holds no authority (A-08 §1): the
//! only things it can ask agentd for are in [`crate::net::Cmd`], and a task
//! starts or resumes only through the compositor's card ([`crate::shield`]).

use std::collections::HashSet;

use iced::widget::text_editor;
use iced::{window, Element, Point, Size, Subscription, Task, Theme};

use ec_console_client::console::{CancelMode, MsgId};
use ec_ui::tokens::console as metrics;

use crate::accounts::{self, Kind, Picks, Store};
use crate::model::{self, Deadline, Model, Outcome, Phase, Selection, Want};
use crate::net::{self, Cmd, Handle, Update};
use crate::shield::{self, Guard, Outcome as Slot, Shield};
use crate::view;

/// How long to wait for the window's handles before asking again.
const HANDLES_RETRY_MS: u64 = 1500;

#[derive(Debug, Clone)]
pub enum Message {
    /// The worker thread started; here is its command channel.
    Ready(Handle),
    /// A heartbeat from the worker, a few times a second.
    Tick,
    Net(Update),
    /// The window's Wayland handles, or why there are none.
    Handles(Result<(usize, usize), String>),
    /// The hole was laid out at a new place.
    HoleAt(Point),
    Resized(Size),
    CloseRequested,

    Select(String),
    NewTask,
    Back,
    ContinueFrom(String),
    StartFresh(String),
    DeleteSession(String),
    ToggleHistory,
    ToggleProvenance(String, MsgId),

    EditDraft(text_editor::Action),
    PickPackage(String),
    PickDeadline(Deadline),
    ClearFollow,
    /// Resume this closed task (A-08 §5.4): fills the commit slot's draft.
    Resume(String),
    ClearResume,
    EditInstruction(String),
    /// A fresh read of the secret store's account names.
    Accounts(Store),
    PickAccount(String),
    /// Ask the compositor for its unlock (or set-up) prompt.
    Unlock,
    /// Open Settings at Accounts.
    OpenAccounts,

    EditReply(text_editor::Action),
    SendReply,

    Pause,
    ToggleCancel,
    Cancel(CancelMode),
    ShowDecisions,
    Retry,

    /// Debug builds: the screenshot loop's picture.
    #[cfg(debug_assertions)]
    Shot(window::Screenshot),
}

pub struct App {
    pub m: Model,
    pub shield: Shield,
    net: Option<Handle>,
    /// Wall clock, refreshed on every heartbeat.
    pub now: u64,
    /// The window's logical width, for the narrow layout.
    pub width: f32,
    pub radius: f32,
    pub blur: bool,

    /// The composer: what the human is typing. It leaves this process only
    /// through the compositor's card (`Shield::reconcile`).
    pub draft: text_editor::Content,
    pub package: Option<String>,
    pub deadline: Deadline,
    /// The task this one continues (A-08 §5.3).
    pub follow: Option<String>,
    /// The session being resumed (A-08 §5.4) as (task id, original
    /// statement): the statement is the draft, verbatim, and `resumes` names
    /// the id. Exclusive with `follow`.
    pub resume: Option<(String, String)>,
    /// An optional new instruction for a resume. Never part of the statement:
    /// it is posted as a conversation message on the new task once the
    /// compositor reports `committed`.
    pub instruction: String,
    /// The account names the secret store holds, as last read. Names only:
    /// no value ever reaches this process.
    pub accounts: Store,
    /// The account picked per kind, for this session.
    pub picks: Picks,
    /// The unlock prompt was asked for and the store is still locked.
    /// Cleared by a failure, or by a read that is no longer locked.
    pub unlocking: bool,

    pub reply: text_editor::Content,
    pub cancel_open: bool,
    pub provenance: HashSet<(String, MsgId)>,
    pub history_open: bool,
    /// Narrow layout: the thread (or composer) is showing, not the fleet.
    pub pane_open: bool,
    /// The last thing that went wrong, in one line. Cleared by the next
    /// action.
    pub notice: Option<String>,
    /// When the handles were last asked for: `window::oldest` answers nothing
    /// until the window exists, so the ask is repeated.
    handles_asked: Option<u64>,
    #[cfg(debug_assertions)]
    pub fixture: bool,
}

impl App {
    fn new() -> Self {
        let (radius, blur) = ec_ui::ipc::fetch_glass();
        Self {
            m: Model::default(),
            shield: Shield::default(),
            net: None,
            now: model::now_ms(),
            width: ec_ui::tokens::console::WINDOW_W,
            radius: radius.unwrap_or(ec_ui::tokens::radius::CARD),
            blur: blur.unwrap_or(true),
            draft: text_editor::Content::new(),
            package: None,
            deadline: Deadline::Default,
            follow: None,
            resume: None,
            instruction: String::new(),
            accounts: Store::Unknown,
            picks: Picks::default(),
            unlocking: false,
            reply: text_editor::Content::new(),
            cancel_open: false,
            provenance: HashSet::new(),
            history_open: true,
            pane_open: false,
            notice: None,
            handles_asked: None,
            #[cfg(debug_assertions)]
            fixture: false,
        }
    }

    pub fn narrow(&self) -> bool {
        self.width < metrics::NARROW
    }

    fn send(&self, c: Cmd) {
        if let Some(h) = &self.net {
            h.send(c);
        }
    }

    /// The account kind the chosen agent runs on. `None` when the agent
    /// declares no inference backend, which hides the picker.
    pub fn account_kind(&self) -> Option<Kind> {
        let id = self.package.as_deref()?;
        let p = self.m.packages.iter().find(|p| p.id == id)?;
        Kind::for_backend(p.backend.as_deref()?)
    }

    /// The account the task would run as, if the store has one to offer.
    pub fn chosen_account(&self) -> Option<&str> {
        self.picks.chosen(&self.accounts, self.account_kind()?)
    }

    /// The statement as typed.
    pub fn statement(&self) -> String {
        self.draft.text()
    }

    /// The reply as typed, trimmed.
    pub fn reply_text(&self) -> String {
        self.reply.text().trim().to_owned()
    }

    /// Which slot the screen needs now. A hole that is not on screen (the
    /// narrow layout showing the fleet) wants no slot.
    pub fn want(&self) -> Want {
        if self.narrow() && !self.pane_open {
            return Want::Nothing;
        }
        match &self.m.selection {
            Selection::Compose => Want::Commit,
            Selection::Task(id) => match self.m.task(id).map(Phase::of) {
                Some(Phase::Paused) if self.m.usable() => Want::Unpause,
                _ => Want::Nothing,
            },
        }
    }

    /// The hole is about to be rebuilt somewhere else, or not at all: forget
    /// where it was, so a stale position never reaches the compositor.
    fn screen_changed(&mut self) {
        self.shield.hole = None;
        self.cancel_open = false;
    }

    fn open_composer(&mut self, follow: Option<String>, package: Option<String>, statement: &str) {
        self.m.selection = Selection::Compose;
        self.follow = follow;
        self.resume = None;
        self.instruction.clear();
        if package.is_some() {
            self.package = package;
        }
        self.draft = content_with(statement);
        self.pane_open = true;
        self.notice = None;
        self.screen_changed();
    }

    fn select(&mut self, id: String) {
        self.m.selection = Selection::Task(id.clone());
        self.reply = text_editor::Content::new();
        self.pane_open = true;
        self.notice = None;
        self.screen_changed();
        self.send(Cmd::Read(id));
    }

    /// Keep the chosen agent valid as the installed set changes.
    fn settle_package(&mut self) {
        let valid = self
            .package
            .as_ref()
            .is_some_and(|p| self.m.packages.iter().any(|k| &k.id == p));
        if !valid {
            self.package = self.m.packages.first().map(|p| p.id.clone());
        }
    }

    fn net_update(&mut self, u: Update) {
        match u {
            Update::Online { available, decisions } => {
                self.m.link = model::Link::Online;
                self.m.available = available;
                self.m.decisions = decisions;
                self.notice = None;
            }
            Update::Offline(why) => self.m.offline(&why),
            Update::Packages(p) => {
                self.m.packages = p;
                self.settle_package();
            }
            Update::Tasks(l) => {
                let first = !self.m.listed;
                self.m.set_tasks(l);
                if first && self.m.selection == Selection::Compose {
                    let waiting = model::fleet(&self.m.tasks)
                        .into_iter()
                        .find(|r| self.m.task(&r.task_id).map(Phase::of) == Some(Phase::Waiting));
                    if let Some(r) = waiting {
                        self.select(r.task_id);
                    }
                }
                if let Selection::Task(id) = &self.m.selection {
                    if self.m.thread(id).is_none() {
                        self.send(Cmd::Read(id.clone()));
                    }
                }
            }
            Update::Sessions(s) => self.m.set_sessions(s),
            Update::Thread(id, conv) => self.m.threads.entry(id).or_default().load(conv),
            Update::Event(ev) => {
                let closed = match &ev {
                    ec_console_client::console::Event::TaskClosed { task_id, .. } => Some(task_id.clone()),
                    _ => None,
                };
                if self.m.apply(ev, self.now) == model::Refetch::Tasks {
                    self.send(Cmd::Refresh);
                }
                if let Some(id) = closed {
                    self.send(Cmd::Read(id));
                }
            }
            Update::Posted(id, _) => self.send(Cmd::Read(id)),
            Update::Failed {
                what,
                text,
                unavailable,
            } => {
                if unavailable {
                    self.m.available = false;
                }
                self.notice = Some(match what {
                    "decisions" => "The decision queue could not be opened.".to_owned(),
                    "unlock" => {
                        self.unlocking = false;
                        "The unlock prompt did not open. Try again in a moment.".to_owned()
                    }
                    "reply" => format!("Your reply was not sent. {text}"),
                    "pause" | "cancel" => format!("That did not go through. {text}"),
                    _ => text,
                });
            }
        }
    }

    fn slot_outcome(&mut self, o: Slot) {
        match o {
            Slot::Committed(id) => {
                // The compositor started the task. The thread is the answer.
                self.draft = text_editor::Content::new();
                self.follow = None;
                self.resume = None;
                self.deadline = Deadline::Default;
                // A resume's optional new instruction is the first human
                // message of the new task, never part of the statement.
                let note = std::mem::take(&mut self.instruction);
                let note = note.trim();
                if !note.is_empty() {
                    self.send(Cmd::Post {
                        task_id: id.clone(),
                        text: note.to_owned(),
                        reply_to: None,
                    });
                }
                self.send(Cmd::Refresh);
                self.m.keep = Some(id.clone());
                self.select(id);
            }
            Slot::Cancelled => {}
            Slot::InputRefused(_) => {
                self.notice =
                    Some("Input that did not come from your own keyboard or mouse was dropped.".to_owned());
            }
        }
    }

    fn heartbeat(&mut self) -> Task<Message> {
        self.now = model::now_ms();
        for o in self.shield.pump() {
            self.slot_outcome(o);
        }
        let draft = match (self.want(), &self.m.selection) {
            (Want::Commit, _) => {
                let (statement, resumes) = match &self.resume {
                    Some((id, original)) => (original.clone(), Some(id.as_str())),
                    None => (self.statement(), None),
                };
                model::draft(
                    self.package.as_deref(),
                    &statement,
                    self.deadline,
                    self.follow.as_deref(),
                    resumes,
                )
                .ok()
            }
            _ => None,
        };
        let unpause = match &self.m.selection {
            Selection::Task(id) => Some(id.clone()),
            Selection::Compose => None,
        };
        let want = self.want();
        // The empty string is the default account, and is what goes out when
        // the store cannot be read: the card then says nothing about it.
        let account = accounts::wire(self.chosen_account());
        self.shield
            .reconcile(want, unpause.as_deref(), draft.as_ref(), &account);
        if self.shield.guard == Guard::Waiting
            && self
                .handles_asked
                .is_none_or(|t| self.now.saturating_sub(t) > HANDLES_RETRY_MS)
        {
            return self.ask_handles();
        }
        Task::none()
    }

    /// Fetch the raw Wayland handles of the (only) window.
    fn ask_handles(&mut self) -> Task<Message> {
        self.handles_asked = Some(self.now);
        window::oldest()
            .and_then(|id| window::run(id, |w| shield::raw_handles(w)))
            .map(Message::Handles)
    }
}

pub fn boot() -> (App, Task<Message>) {
    #[allow(unused_mut)]
    let mut app = App::new();
    #[cfg(debug_assertions)]
    crate::fixture::apply(&mut app);
    let handles = app.ask_handles();
    let size = window::oldest().and_then(window::size).map(Message::Resized);
    (app, Task::batch([handles, size]))
}

pub fn update(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::Ready(h) => app.net = Some(h),
        Message::Tick => {
            let beat = app.heartbeat();
            #[cfg(debug_assertions)]
            if app.fixture {
                return Task::batch([beat, crate::fixture::on_tick(app)]);
            }
            return beat;
        }
        #[cfg(debug_assertions)]
        Message::Shot(s) => return crate::fixture::save(&s),
        Message::Net(u) => app.net_update(u),
        Message::Handles(Ok(h)) => app.shield.attach(Ok(h)),
        Message::Handles(Err(e)) => app.shield.attach(Err(e)),
        Message::HoleAt(p) => {
            app.shield.hole = Some((p.x.round() as i32, p.y.round() as i32));
        }
        Message::Resized(s) => {
            let was = app.narrow();
            app.width = s.width;
            if was != app.narrow() {
                app.screen_changed();
            }
        }
        Message::CloseRequested => {
            // The protected surface goes before the window does.
            app.shield.detach();
            return iced::exit();
        }

        Message::Select(id) => app.select(id),
        Message::NewTask => {
            let pkg = app.package.clone();
            app.open_composer(None, pkg, "");
        }
        Message::Back => {
            app.pane_open = false;
            app.screen_changed();
        }
        Message::ContinueFrom(id) => {
            let pkg = app
                .m
                .history()
                .iter()
                .find(|h| h.task_id == id)
                .map(|h| h.package.clone());
            let pkg = pkg.or_else(|| app.m.task(&id).map(|t| t.package.clone()));
            app.open_composer(Some(id), pkg, "");
        }
        Message::StartFresh(id) => {
            let h = app.m.history().iter().find(|h| h.task_id == id);
            let (pkg, statement) = match h {
                Some(h) => (Some(h.package.clone()), h.statement.clone()),
                None => (None, String::new()),
            };
            app.open_composer(None, pkg, &statement);
        }
        Message::DeleteSession(id) => {
            app.send(Cmd::DeleteSession(id.clone()));
            app.m.threads.remove(&id);
            if app.m.selection == Selection::Task(id) {
                app.m.selection = Selection::Compose;
                app.screen_changed();
            }
        }
        Message::ToggleHistory => app.history_open = !app.history_open,
        Message::ToggleProvenance(t, id) => {
            if !app.provenance.remove(&(t.clone(), id)) {
                app.provenance.insert((t, id));
            }
        }

        Message::EditDraft(a) => {
            if app.m.usable() {
                app.draft.perform(a);
            }
        }
        Message::PickPackage(p) => app.package = Some(p),
        Message::PickDeadline(d) => app.deadline = d,
        Message::ClearFollow => app.follow = None,
        Message::ClearResume => {
            app.resume = None;
            app.instruction.clear();
        }
        Message::EditInstruction(s) => app.instruction = s,
        Message::Accounts(store) => {
            if !matches!(store, Store::Locked | Store::Uninitialised) {
                app.unlocking = false;
            }
            app.accounts = store;
        }
        Message::PickAccount(name) => {
            if let Some(kind) = app.account_kind() {
                app.picks.set(kind, &name);
            }
        }
        Message::Unlock => {
            app.notice = None;
            app.unlocking = true;
            app.send(Cmd::UnlockSecrets);
        }
        Message::OpenAccounts => {
            if accounts::open_settings().is_err() {
                app.notice = Some("Settings could not be opened.".to_owned());
            }
        }
        Message::Resume(id) => {
            let h = app.m.history().iter().find(|h| h.task_id == id).cloned();
            if let Some(h) = h {
                app.open_composer(None, Some(h.package), "");
                app.resume = Some((id, h.statement));
            }
        }

        Message::EditReply(a) => {
            if app.m.usable() {
                app.reply.perform(a);
            }
        }
        Message::SendReply => {
            let text = app.reply_text();
            if let (false, Selection::Task(id)) = (text.is_empty(), &app.m.selection) {
                if app.m.usable() {
                    app.send(Cmd::Post {
                        task_id: id.clone(),
                        text,
                        reply_to: None,
                    });
                    app.reply = text_editor::Content::new();
                    app.notice = None;
                }
            }
        }

        Message::Pause => {
            if let Selection::Task(id) = &app.m.selection {
                app.send(Cmd::Pause(id.clone()));
            }
        }
        Message::ToggleCancel => app.cancel_open = !app.cancel_open,
        Message::Cancel(mode) => {
            if let Selection::Task(id) = &app.m.selection {
                app.send(Cmd::Cancel(id.clone(), mode));
            }
            app.cancel_open = false;
        }
        Message::ShowDecisions => app.send(Cmd::ShowDecisions),
        Message::Retry => {
            app.m.link = model::Link::Connecting;
            app.send(Cmd::Reconnect);
        }
    }
    Task::none()
}

pub fn view(app: &App) -> Element<'_, Message, Theme> {
    view::view(app)
}

pub fn subscription(app: &App) -> Subscription<Message> {
    let mut subs = vec![
        window::close_requests().map(|_| Message::CloseRequested),
        window::resize_events().map(|(_, s)| Message::Resized(s)),
    ];
    #[cfg(debug_assertions)]
    let live = !app.fixture;
    #[cfg(not(debug_assertions))]
    let live = {
        let _ = app;
        true
    };
    if live {
        subs.push(net::feed());
        // The account list is read only while the composer is on screen.
        if app.want() == Want::Commit {
            subs.push(accounts::feed());
        }
    }
    #[cfg(debug_assertions)]
    if app.fixture {
        subs.push(crate::fixture::ticks());
    }
    Subscription::batch(subs)
}

/// An editor holding `text`. Built by pasting into an empty one: iced's
/// `Content::with_text` lays its buffer out at a 1px font until the first edit,
/// so a prefilled statement would not show.
pub fn content_with(text: &str) -> text_editor::Content {
    let mut c = text_editor::Content::new();
    c.perform(text_editor::Action::Edit(text_editor::Edit::Paste(
        std::sync::Arc::new(text.to_owned()),
    )));
    c
}

/// How a closed task's outcome is worded for the composer's "follows" line.
pub fn outcome_word(o: Outcome) -> &'static str {
    match o {
        Outcome::Finished => "finished",
        Outcome::Cancelled => "cancelled",
        Outcome::Failed => "failed",
        Outcome::Other => "closed",
    }
}
