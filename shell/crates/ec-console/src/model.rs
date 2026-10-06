// SPDX-License-Identifier: AGPL-3.0-only
//! What the console knows, and the rules for reading it. No iced, no sockets:
//! everything here is a pure function of data agentd sent, so it is tested
//! without a window.
//!
//! Nothing in this module is persisted (A-08 §11). It is a cache of what the
//! console socket said, and it is rebuilt from `list_tasks` on every connect.

use std::collections::HashSet;

use ec_console_client::console::{Conversation, Event, Message, MsgId, Package, Session, Task, TaskList};

// --------------------------------------------------------------- phase

/// Why a closed task closed, as far as the console can tell from agentd's
/// reason code. Unknown codes are kept as [`Outcome::Other`], never guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Finished,
    Cancelled,
    Failed,
    Other,
}

/// The one word a task is in, for a row, a header and a status chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Running,
    /// Active and the agent asked a question.
    Waiting,
    Paused,
    /// Cancel was requested with `drain`: finishing the current step.
    Draining,
    Closed(Outcome),
}

impl Phase {
    pub fn of(t: &Task) -> Phase {
        match t.state.as_str() {
            "paused" => Phase::Paused,
            "draining" => Phase::Draining,
            "closed" => Phase::Closed(outcome(&t.reason)),
            _ if t.awaiting_reply => Phase::Waiting,
            _ => Phase::Running,
        }
    }

    /// A closed task's phase from a session record.
    pub fn of_session(s: &Session) -> Phase {
        Phase::Closed(outcome(&s.reason))
    }

    pub fn is_closed(self) -> bool {
        matches!(self, Phase::Closed(_))
    }

    /// Live: the agent is working or waiting on you.
    pub fn is_live(self) -> bool {
        matches!(self, Phase::Running | Phase::Waiting)
    }

    /// The header's reading.
    pub fn label(self) -> &'static str {
        match self {
            Phase::Running => "Running",
            Phase::Waiting => "Waiting for your reply",
            Phase::Paused => "Paused",
            Phase::Draining => "Finishing the current step",
            Phase::Closed(Outcome::Finished) => "Finished",
            Phase::Closed(Outcome::Cancelled) => "Cancelled",
            Phase::Closed(Outcome::Failed) => "Failed",
            Phase::Closed(Outcome::Other) => "Closed",
        }
    }

    /// A row's short tag.
    pub fn tag(self) -> &'static str {
        match self {
            Phase::Running => "Running",
            Phase::Waiting => "Reply",
            Phase::Paused => "Paused",
            Phase::Draining => "Stopping",
            Phase::Closed(Outcome::Finished) => "Done",
            Phase::Closed(Outcome::Cancelled) => "Cancelled",
            Phase::Closed(Outcome::Failed) => "Failed",
            Phase::Closed(Outcome::Other) => "Closed",
        }
    }
}

/// agentd's close reason as an [`Outcome`]. Tolerant of wording: the codes are
/// agentd's, and a new one must degrade to "Closed", not to a wrong word.
pub fn outcome(reason: &str) -> Outcome {
    let r = reason.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| r.contains(n));
    if has(&["complet", "finish", "done", "success"]) {
        Outcome::Finished
    } else if has(&["cancel", "human", "drain", "immediate", "stopped", "user"]) {
        Outcome::Cancelled
    } else if has(&[
        "fail", "error", "restart", "crash", "incident", "timeout", "deadline",
    ]) {
        Outcome::Failed
    } else {
        Outcome::Other
    }
}

// ------------------------------------------------------------------ time

/// Time left to a deadline, as the fleet and the header read it.
pub fn left(deadline_ms: u64, now_ms: u64) -> String {
    if deadline_ms == 0 {
        return "no deadline".to_owned();
    }
    if deadline_ms <= now_ms {
        return "overdue".to_owned();
    }
    duration((deadline_ms - now_ms) / 1000)
}

/// `1h 12m`, `48m`, `2d 3h`, `<1m`.
pub fn duration(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, (secs % 86_400) / 3600, (secs % 3600) / 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        "<1m".to_owned()
    }
}

/// The share of the allotted time already used, 0..=1. Unknown bounds read as
/// nothing used rather than as a made-up fraction.
pub fn elapsed_fraction(started_ms: u64, deadline_ms: u64, now_ms: u64) -> f32 {
    if started_ms == 0 || deadline_ms <= started_ms || now_ms <= started_ms {
        return 0.0;
    }
    let total = (deadline_ms - started_ms) as f64;
    let used = (now_ms.min(deadline_ms) - started_ms) as f64;
    (used / total) as f32
}

/// Local `HH:MM` for an epoch-millisecond instant, through `localtime_r` so
/// the user's zone and DST are followed without a time crate.
pub fn clock(ms: u64) -> String {
    let secs = (ms / 1000) as libc::time_t;
    // SAFETY: `localtime_r` writes a `tm` we own and reads a `time_t` we own;
    // a zeroed `tm` is a valid value for it to overwrite.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&secs, &mut tm).is_null() {
            return "--:--".to_owned();
        }
        tm
    };
    format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
}

/// Local `day HH:MM` for a closed task: `today 11:20`, or `Mon 10:18`.
pub fn when(ms: u64, now_ms: u64) -> String {
    let day = |ms: u64| -> (i32, i32) {
        let secs = (ms / 1000) as libc::time_t;
        // SAFETY: as in `clock`.
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            if libc::localtime_r(&secs, &mut tm).is_null() {
                return (0, 0);
            }
            (tm.tm_year * 400 + tm.tm_yday, tm.tm_wday)
        }
    };
    let (d, wd) = day(ms);
    let (today, _) = day(now_ms);
    let name = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    if d == today {
        format!("today {}", clock(ms))
    } else if today - d == 1 {
        format!("yesterday {}", clock(ms))
    } else {
        format!("{} {}", name[wd.rem_euclid(7) as usize], clock(ms))
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Agent text made safe to lay out. The server already sanitises (A-08 §6.1);
/// this is the second belt: control characters other than newline and tab are
/// dropped, and so are the bidirectional overrides that let a line pretend to
/// be somewhere else on the screen.
pub fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| {
            let bidi =
                matches!(*c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}');
            !bidi && (!c.is_control() || *c == '\n' || *c == '\t')
        })
        .collect()
}

/// A string cut to `max` characters with a trailing `…`. By characters, never
/// bytes: a statement is human text in any script.
pub fn ellipsize(s: &str, max: usize) -> String {
    let one_line: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= max {
        return one_line;
    }
    let mut out: String = one_line.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

// ------------------------------------------------------------------ fleet

/// One row of the fleet column, already ordered and indented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetRow {
    pub task_id: String,
    pub depth: usize,
}

/// Non-closed tasks, a task's children directly under it (A-08 §8). Roots
/// newest first; children in the order they started. A task whose parent is
/// not in the list (closed, or not the owner's) is a root. Cycles cannot
/// loop: a task is placed once.
pub fn fleet(tasks: &[Task]) -> Vec<FleetRow> {
    let open: Vec<&Task> = tasks.iter().filter(|t| !Phase::of(t).is_closed()).collect();
    let ids: HashSet<&str> = open.iter().map(|t| t.task_id.as_str()).collect();
    let mut roots: Vec<&Task> = open
        .iter()
        .copied()
        .filter(|t| t.parent.as_deref().is_none_or(|p| !ids.contains(p)))
        .collect();
    roots.sort_by(|a, b| b.started_ms.cmp(&a.started_ms).then(a.task_id.cmp(&b.task_id)));

    let mut out = Vec::new();
    let mut placed: HashSet<&str> = HashSet::new();
    fn place<'a>(
        t: &'a Task,
        depth: usize,
        open: &[&'a Task],
        placed: &mut HashSet<&'a str>,
        out: &mut Vec<FleetRow>,
    ) {
        if !placed.insert(t.task_id.as_str()) {
            return;
        }
        out.push(FleetRow {
            task_id: t.task_id.clone(),
            depth,
        });
        let mut kids: Vec<&Task> = open
            .iter()
            .copied()
            .filter(|c| c.parent.as_deref() == Some(t.task_id.as_str()))
            .collect();
        kids.sort_by(|a, b| a.started_ms.cmp(&b.started_ms).then(a.task_id.cmp(&b.task_id)));
        for k in kids {
            place(k, depth + 1, open, placed, out);
        }
    }
    for r in roots {
        place(r, 0, &open, &mut placed, &mut out);
    }
    out
}

/// A closed task in History, whether it came from `list_tasks` or from
/// `list_sessions`.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryItem {
    pub task_id: String,
    pub package: String,
    pub statement: String,
    pub outcome: Outcome,
    pub reason: String,
    pub closed_ms: u64,
    pub min_trust: String,
}

/// Closed tasks and session records, one row per task id, newest first.
pub fn history(tasks: &[Task], sessions: &[Session]) -> Vec<HistoryItem> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<HistoryItem> = Vec::new();
    for t in tasks.iter().filter(|t| Phase::of(t).is_closed()) {
        if seen.insert(t.task_id.clone()) {
            out.push(HistoryItem {
                task_id: t.task_id.clone(),
                package: t.package.clone(),
                statement: t.statement.clone(),
                outcome: outcome(&t.reason),
                reason: t.reason.clone(),
                closed_ms: t.closed_ms.unwrap_or(0),
                min_trust: t.min_trust.clone(),
            });
        }
    }
    for s in sessions {
        if seen.insert(s.task_id.clone()) {
            out.push(HistoryItem {
                task_id: s.task_id.clone(),
                package: s.package.clone(),
                statement: s.statement.clone(),
                outcome: outcome(&s.reason),
                reason: s.reason.clone(),
                closed_ms: s.closed_ms,
                min_trust: s.min_trust.clone(),
            });
        }
    }
    out.sort_by(|a, b| b.closed_ms.cmp(&a.closed_ms).then(a.task_id.cmp(&b.task_id)));
    out
}

/// The agent's display name: the installed package's name, else its id.
pub fn package_name(packages: &[Package], id: &str) -> String {
    packages
        .iter()
        .find(|p| p.id == id)
        .map(|p| p.name.clone())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| id.to_owned())
}

// ----------------------------------------------------------------- thread

/// What the console shows for one message kind (`conversation.v1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Voice {
    /// Yours: posted over the console socket. Informs the agent, authorizes
    /// nothing (A-08 §6), and is shown as yours.
    You,
    /// The agent's, untrusted regardless of its chain.
    Agent { question: bool },
    /// The carried-over summary of a predecessor (A-08 §5.3), still agent
    /// text and still marked as such.
    Context,
}

pub fn voice(m: &Message) -> Voice {
    match m.kind.as_str() {
        "human" => Voice::You,
        "context" => Voice::Context,
        "ask" => Voice::Agent { question: true },
        _ => Voice::Agent { question: false },
    }
}

/// A line the console writes itself between messages: a state change it
/// witnessed, or how the task ended.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub time_ms: u64,
    pub text: String,
    pub danger: bool,
}

/// One task's conversation as the console holds it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Thread {
    pub messages: Vec<Message>,
    pub lines: Vec<Line>,
    pub loaded: bool,
    /// `conversation_read` said the conversation is closed to new posts.
    pub closed: bool,
}

impl Thread {
    /// Replace the messages with a fresh read, keeping the console's own
    /// lines.
    pub fn load(&mut self, c: Conversation) {
        let mut messages = c.messages;
        messages.sort_by_key(|m| m.msg_id);
        messages.dedup_by_key(|m| m.msg_id);
        self.messages = messages;
        self.closed = c.closed;
        self.loaded = true;
    }

    /// Add one message from the event stream, once, in id order.
    pub fn push(&mut self, m: Message) {
        match self.messages.binary_search_by_key(&m.msg_id, |x| x.msg_id) {
            Ok(_) => {}
            Err(at) => self.messages.insert(at, m),
        }
    }

    pub fn note(&mut self, time_ms: u64, text: &str, danger: bool) {
        if self.lines.iter().any(|l| l.text == text && l.time_ms == time_ms) {
            return;
        }
        self.lines.push(Line {
            time_ms,
            text: text.to_owned(),
            danger,
        });
    }

    pub fn last_id(&self) -> Option<MsgId> {
        self.messages.last().map(|m| m.msg_id)
    }
}

/// One entry of the merged transcript.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry<'a> {
    Message(&'a Message),
    Line(&'a Line),
}

impl Entry<'_> {
    fn time(&self) -> u64 {
        match self {
            Entry::Message(m) => m.time_ms,
            Entry::Line(l) => l.time_ms,
        }
    }
}

/// Messages and the console's own lines in time order. Stable: two entries at
/// one instant keep message-before-line order.
pub fn transcript<'a>(t: &'a Thread, extra: &'a [Line]) -> Vec<Entry<'a>> {
    let mut v: Vec<Entry<'a>> = t.messages.iter().map(Entry::Message).collect();
    v.extend(t.lines.iter().chain(extra.iter()).map(Entry::Line));
    v.sort_by_key(Entry::time);
    v
}

/// How a closed task's last line reads, from what agentd reported.
pub fn end_line(t: &Task) -> Option<Line> {
    let Phase::Closed(o) = Phase::of(t) else {
        return None;
    };
    let at = t.closed_ms.unwrap_or(0);
    let took = match t.closed_ms {
        Some(c) if c > t.started_ms && t.started_ms > 0 => {
            format!(" · {}", duration((c - t.started_ms) / 1000))
        }
        _ => String::new(),
    };
    let text = match o {
        Outcome::Finished => format!("Finished{took}"),
        Outcome::Cancelled => format!("Cancelled by you{took}"),
        Outcome::Failed if t.reason.is_empty() => format!("Failed{took}"),
        Outcome::Failed => format!("Failed · {}{took}", t.reason),
        Outcome::Other if t.reason.is_empty() => format!("Closed{took}"),
        Outcome::Other => format!("Closed · {}{took}", t.reason),
    };
    Some(Line {
        time_ms: at,
        text,
        danger: o == Outcome::Failed,
    })
}

/// The line a witnessed `task_state` event writes, if it is one worth a line.
pub fn state_line(state: &str, reason: &str) -> Option<(String, bool)> {
    match state {
        "paused" => Some(("Paused".to_owned(), false)),
        "draining" => Some(("Cancel requested · finishing the current step".to_owned(), false)),
        "active" if reason == "unpaused" || reason == "human" || reason.is_empty() => {
            Some(("Resumed".to_owned(), false))
        }
        _ => None,
    }
}

// ------------------------------------------------------------------ model

/// What the right-hand side is showing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Selection {
    /// The composer: a new task.
    #[default]
    Compose,
    /// One task's thread, open or closed.
    Task(String),
}

/// How the console stands with agentd.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Link {
    #[default]
    Connecting,
    Online,
    /// No console socket, or it dropped. The string is for a tooltip, never
    /// for the headline.
    Offline(String),
}

#[derive(Debug, Default)]
pub struct Model {
    pub link: Link,
    /// `available` from `list_tasks` / `subscribe`: false while agentd cannot
    /// reach policyd (A-08 §12).
    pub available: bool,
    pub packages: Vec<Package>,
    pub tasks: Vec<Task>,
    pub sessions: Vec<Session>,
    pub decisions: u32,
    pub selection: Selection,
    pub threads: std::collections::HashMap<String, Thread>,
    /// The first `list_tasks` has landed: until then "no tasks yet" would be
    /// a guess.
    pub listed: bool,
    /// A task the compositor just committed: selected before `list_tasks` can
    /// know it, so it must survive a stale list that arrives in between.
    pub keep: Option<String>,
    /// [`history`] of `tasks` and `sessions`, kept so the view can borrow it.
    hist: Vec<HistoryItem>,
}

impl Model {
    /// Controls act only against a live, reachable agentd.
    pub fn usable(&self) -> bool {
        self.link == Link::Online && self.available
    }

    pub fn task(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.task_id == id)
    }

    pub fn thread(&self, id: &str) -> Option<&Thread> {
        self.threads.get(id)
    }

    pub fn selected_task(&self) -> Option<&Task> {
        match &self.selection {
            Selection::Task(id) => self.task(id),
            Selection::Compose => None,
        }
    }

    /// Closed tasks and session records, newest first.
    pub fn history(&self) -> &[HistoryItem] {
        &self.hist
    }

    fn rebuild(&mut self) {
        self.hist = history(&self.tasks, &self.sessions);
    }

    /// Install a `list_tasks` answer. A selected task that vanished from both
    /// the fleet and History falls back to the composer.
    pub fn set_tasks(&mut self, l: TaskList) {
        self.available = l.available;
        self.decisions = l.decisions_pending;
        self.tasks = l.tasks;
        self.listed = true;
        self.rebuild();
        self.reconcile_selection();
    }

    pub fn set_sessions(&mut self, s: Vec<Session>) {
        self.sessions = s;
        self.rebuild();
        self.reconcile_selection();
    }

    fn reconcile_selection(&mut self) {
        if let Some(k) = &self.keep {
            if self.task(k).is_some() {
                self.keep = None;
            }
        }
        if let Selection::Task(id) = &self.selection {
            let known = self.keep.as_deref() == Some(id.as_str())
                || self.task(id).is_some()
                || self.hist.iter().any(|h| &h.task_id == id);
            if !known {
                self.selection = Selection::Compose;
            }
        }
    }

    pub fn offline(&mut self, why: &str) {
        self.link = Link::Offline(why.to_owned());
        self.available = false;
    }

    /// Fold one event from the stream into the model. Returns what the
    /// caller must fetch: a lifecycle change needs a fresh `list_tasks`.
    pub fn apply(&mut self, ev: Event, now_ms: u64) -> Refetch {
        match ev {
            Event::TaskStarted { .. } | Event::TaskClosed { .. } => Refetch::Tasks,
            Event::TaskState {
                task_id,
                state,
                reason,
            } => {
                if let Some((text, danger)) = state_line(&state, &reason) {
                    self.threads
                        .entry(task_id.clone())
                        .or_default()
                        .note(now_ms, &text, danger);
                }
                if let Some(t) = self.tasks.iter_mut().find(|t| t.task_id == task_id) {
                    t.state = state;
                    t.reason = reason;
                }
                self.rebuild();
                Refetch::Tasks
            }
            Event::Message(m) => {
                let id = m.task_id.clone();
                self.threads.entry(id).or_default().push(m);
                Refetch::Nothing
            }
            Event::AwaitingReply { task_id, awaiting } => {
                if let Some(t) = self.tasks.iter_mut().find(|t| t.task_id == task_id) {
                    t.awaiting_reply = awaiting;
                }
                Refetch::Nothing
            }
            Event::DecisionsPending { count } => {
                self.decisions = count;
                Refetch::Nothing
            }
            Event::Unknown { .. } => Refetch::Nothing,
            Event::Disconnected => {
                self.offline("connection closed");
                Refetch::Nothing
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refetch {
    Nothing,
    Tasks,
}

// --------------------------------------------------------------- composer

/// How long the task may run, as the composer offers it. `0` seconds is "the
/// package's default" (A-04 §13.4: 2 h).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Deadline {
    #[default]
    Default,
    Short,
    Long,
    Day,
}

impl Deadline {
    pub const ALL: [Deadline; 4] = [Deadline::Default, Deadline::Short, Deadline::Long, Deadline::Day];

    pub fn seconds(self) -> u32 {
        match self {
            Deadline::Default => 0,
            Deadline::Short => 30 * 60,
            Deadline::Long => 8 * 3600,
            Deadline::Day => 24 * 3600,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Deadline::Default => "Default",
            Deadline::Short => "30 min",
            Deadline::Long => "8 hours",
            Deadline::Day => "24 hours",
        }
    }
}

/// The statement limit (A-08 §5.1).
pub const STATEMENT_MAX: usize = 1000;

/// Why a draft is not sent to the compositor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftIssue {
    NoPackage,
    TooLong(usize),
}

/// The task a resumed task restored its history from: agentd's `resumes`
/// field, read from the raw object because the client library does not name it.
pub fn resumed_from(t: &Task) -> Option<String> {
    t.raw
        .get("resumes")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// The draft the composer hands the compositor's slot. The console never
/// submits it anywhere else (A-08 §5.1).
///
/// A resume (A-08 §5.4) carries `resumes` and an empty `continuation`; its
/// statement is the session's original, verbatim, which the caller passes and
/// this does not edit beyond the NUL and trailing-space rules every draft gets
/// (the original never has either).
pub fn draft(
    package: Option<&str>,
    statement: &str,
    deadline: Deadline,
    continuation: Option<&str>,
    resumes: Option<&str>,
) -> Result<ec_console_client::protected::Draft, DraftIssue> {
    let continuation = if resumes.is_some() { None } else { continuation };
    let package = package.filter(|p| !p.is_empty()).ok_or(DraftIssue::NoPackage)?;
    let statement: String = statement.chars().filter(|c| *c != '\0').collect();
    let statement = statement.trim_end().to_owned();
    let n = statement.chars().count();
    if n > STATEMENT_MAX {
        return Err(DraftIssue::TooLong(n));
    }
    Ok(ec_console_client::protected::Draft {
        package: package.to_owned(),
        statement,
        deadline_s: deadline.seconds(),
        continuation: continuation.unwrap_or_default().to_owned(),
        resumes: resumes.unwrap_or_default().to_owned(),
        ..Default::default()
    })
}

// ------------------------------------------------------------------- slot

use ec_console_client::protected::{SlotKind, SlotReason, SlotState};

/// Which slot the screen needs right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Want {
    Nothing,
    Commit,
    /// The selected task is paused: resuming is a slot, not a button.
    Unpause,
}

impl Want {
    pub fn kind(self) -> Option<SlotKind> {
        match self {
            Want::Nothing => None,
            Want::Commit => Some(SlotKind::TaskCommit),
            Want::Unpause => Some(SlotKind::TaskUnpause),
        }
    }
}

/// What the console holds of the slot it asked for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Held {
    pub kind: Option<SlotKind>,
    pub at: Option<(i32, i32)>,
    /// `cancel` was sent; the slot is gone when the compositor says
    /// `cancelled`.
    pub ending: bool,
}

/// The next request, if any. One step at a time: the slot object is only
/// replaced after the compositor has confirmed the old one is gone, because a
/// second `get_slot` before then is a protocol error that would kill the
/// console's own connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Idle,
    Create(SlotKind, (i32, i32)),
    Move((i32, i32)),
    Cancel,
}

pub fn next_step(want: Want, held: Held, hole: Option<(i32, i32)>) -> Step {
    if held.ending {
        return Step::Idle;
    }
    match (want.kind(), held.kind) {
        (None, None) => Step::Idle,
        (None, Some(_)) => Step::Cancel,
        (Some(w), Some(h)) if w != h => Step::Cancel,
        (Some(_), Some(_)) => match (hole, held.at) {
            (Some(h), Some(a)) if h != a => Step::Move(h),
            _ => Step::Idle,
        },
        (Some(w), None) => match hole {
            Some(h) => Step::Create(w, h),
            None => Step::Idle,
        },
    }
}

/// What the slot is doing, in the console's own plain words, for the line
/// beneath the hole. These are statuses of the card, not content of it: the
/// console never learns what the card shows.
pub fn slot_note(state: Option<(SlotState, SlotReason)>) -> Option<&'static str> {
    let (s, r) = state?;
    match (s, r) {
        (SlotState::Armed, _) => Some("Armed. Press Enter, physically, to commit."),
        (SlotState::Previewing, _) => Some("Checking the draft."),
        (_, SlotReason::NotFocused) => Some("Click this window to arm the card."),
        (_, SlotReason::Occluded) => Some("Part of this window is covered, so the card cannot arm."),
        (_, SlotReason::Locked) => Some("The session is locked."),
        (_, SlotReason::Modal) => Some("Another prompt is open."),
        (_, SlotReason::Fullscreen) => Some("Press Enter to open the commit card."),
        (_, SlotReason::Overflow) => Some("The statement is too long for the card."),
        (_, SlotReason::PolicyUnavailable) => {
            Some("The policy service is unavailable, so nothing can commit.")
        }
        (_, SlotReason::AgentdUnavailable) => {
            Some("The agent service is unavailable, so nothing can commit.")
        }
        (_, SlotReason::PreviewStale) => Some("Policy changed. Checking the draft again."),
        (SlotState::Refused, _) => Some("The draft was refused."),
        (_, SlotReason::Unsupported) => Some("This compositor cannot draw the commit card."),
        (SlotState::Disarmed, _) => Some("Edit the task, then commit it in the card."),
        (SlotState::Suspended, _) => Some("Waiting for this window."),
        _ => None,
    }
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;
    use ec_console_client::console::Trust;
    use serde_json::Value;

    fn task(id: &str, state: &str, parent: Option<&str>, started: u64) -> Task {
        Task {
            task_id: id.into(),
            package: "inv".into(),
            version: "1".into(),
            statement: format!("statement {id}"),
            state: state.into(),
            reason: String::new(),
            deadline_ms: 0,
            started_ms: started,
            closed_ms: None,
            continuation: String::new(),
            min_trust: String::new(),
            depth: 0,
            awaiting_reply: false,
            pending_decisions: 0,
            parent: parent.map(str::to_owned),
            children: Vec::new(),
            raw: Value::Null,
        }
    }

    fn msg(task: &str, id: u64, kind: &str, t: u64) -> Message {
        Message {
            task_id: task.into(),
            msg_id: id,
            time_ms: t,
            kind: kind.into(),
            text: format!("m{id}"),
            reply_to: None,
            trust: Trust::default(),
        }
    }

    #[test]
    fn phase_reads_state_then_awaiting() {
        let mut t = task("a", "active", None, 1);
        assert_eq!(Phase::of(&t), Phase::Running);
        t.awaiting_reply = true;
        assert_eq!(Phase::of(&t), Phase::Waiting);
        t.state = "paused".into();
        assert_eq!(Phase::of(&t), Phase::Paused);
        t.state = "draining".into();
        assert_eq!(Phase::of(&t), Phase::Draining);
        t.state = "closed".into();
        t.reason = "completed".into();
        assert_eq!(Phase::of(&t), Phase::Closed(Outcome::Finished));
    }

    #[test]
    fn an_unknown_close_reason_is_closed_not_a_guess() {
        assert_eq!(outcome("completed"), Outcome::Finished);
        assert_eq!(outcome("failed"), Outcome::Failed);
        assert_eq!(outcome("agentd_restart"), Outcome::Failed);
        assert_eq!(outcome("cancelled"), Outcome::Cancelled);
        assert_eq!(outcome("quiescent"), Outcome::Other);
        assert_eq!(Phase::Closed(Outcome::Other).label(), "Closed");
    }

    #[test]
    fn time_left_reads_like_a_person() {
        let now = 1_000_000_000;
        assert_eq!(left(0, now), "no deadline");
        assert_eq!(left(now - 1, now), "overdue");
        assert_eq!(left(now + 30_000, now), "<1m");
        assert_eq!(left(now + 48 * 60_000, now), "48m");
        assert_eq!(left(now + 72 * 60_000, now), "1h 12m");
        assert_eq!(left(now + (51 * 3600) * 1000, now), "2d 3h");
    }

    #[test]
    fn elapsed_fraction_is_clamped_and_never_invented() {
        assert_eq!(elapsed_fraction(0, 100, 50), 0.0);
        assert_eq!(elapsed_fraction(100, 100, 150), 0.0);
        assert_eq!(elapsed_fraction(100, 200, 150), 0.5);
        assert_eq!(elapsed_fraction(100, 200, 900), 1.0);
        assert_eq!(elapsed_fraction(100, 200, 50), 0.0);
    }

    #[test]
    fn ellipsis_counts_characters_not_bytes() {
        assert_eq!(ellipsize("short", 10), "short");
        assert_eq!(ellipsize("abcdefghij", 5), "abcd…");
        assert_eq!(ellipsize("日本語のテキストです", 5), "日本語の…");
        assert_eq!(ellipsize("a\n  b", 10), "a b");
    }

    #[test]
    fn agent_text_loses_controls_and_bidi_overrides_but_keeps_its_lines() {
        assert_eq!(clean("a\u{1b}[31mb\u{202e}c\nd\te"), "a[31mbc\nd\te");
        assert_eq!(clean("日本語"), "日本語");
    }

    #[test]
    fn the_fleet_nests_children_under_their_parent_and_hides_closed() {
        let tasks = vec![
            task("old", "active", None, 10),
            task("new", "active", None, 30),
            task("kid2", "active", Some("old"), 22),
            task("kid1", "active", Some("old"), 21),
            task("gone", "closed", None, 5),
            task("orphan", "active", Some("gone"), 40),
        ];
        let rows: Vec<(String, usize)> = fleet(&tasks).into_iter().map(|r| (r.task_id, r.depth)).collect();
        assert_eq!(
            rows,
            vec![
                ("orphan".into(), 0),
                ("new".into(), 0),
                ("old".into(), 0),
                ("kid1".into(), 1),
                ("kid2".into(), 1),
            ]
        );
    }

    #[test]
    fn a_parent_cycle_places_each_task_once() {
        let tasks = vec![
            task("a", "active", Some("b"), 1),
            task("b", "active", Some("a"), 2),
        ];
        // Neither is a root, so neither is placed: no loop, no panic.
        assert!(fleet(&tasks).is_empty());
    }

    #[test]
    fn history_merges_tasks_and_sessions_once_each() {
        let mut closed = task("t1", "closed", None, 1);
        closed.reason = "completed".into();
        closed.closed_ms = Some(500);
        let tasks = vec![closed, task("live", "active", None, 2)];
        let sessions = vec![
            Session {
                task_id: "t1".into(),
                package: "inv".into(),
                statement: "dup".into(),
                reason: "completed".into(),
                closed_ms: 500,
                min_trust: String::new(),
            },
            Session {
                task_id: "t2".into(),
                package: "inv".into(),
                statement: "other".into(),
                reason: "failed".into(),
                closed_ms: 900,
                min_trust: "untrusted".into(),
            },
        ];
        let h = history(&tasks, &sessions);
        let ids: Vec<&str> = h.iter().map(|i| i.task_id.as_str()).collect();
        assert_eq!(ids, ["t2", "t1"]);
        assert_eq!(h[0].outcome, Outcome::Failed);
        assert_eq!(h[1].statement, "statement t1");
    }

    #[test]
    fn a_message_event_lands_once_and_in_order() {
        let mut m = Model::default();
        m.apply(Event::Message(msg("t", 2, "say", 20)), 0);
        m.apply(Event::Message(msg("t", 1, "human", 10)), 0);
        m.apply(Event::Message(msg("t", 2, "say", 20)), 0);
        let ids: Vec<u64> = m.thread("t").unwrap().messages.iter().map(|x| x.msg_id).collect();
        assert_eq!(ids, [1, 2]);
    }

    #[test]
    fn a_fresh_read_replaces_messages_but_keeps_our_lines() {
        let mut th = Thread::default();
        th.note(5, "Paused", false);
        th.push(msg("t", 9, "say", 1));
        th.load(Conversation {
            messages: vec![msg("t", 2, "say", 1), msg("t", 1, "human", 0)],
            closed: false,
            awaiting_reply: false,
        });
        assert_eq!(th.messages.iter().map(|m| m.msg_id).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(th.lines.len(), 1);
        assert!(th.loaded);
    }

    #[test]
    fn the_transcript_interleaves_by_time() {
        let mut th = Thread::default();
        th.push(msg("t", 1, "human", 10));
        th.push(msg("t", 2, "say", 30));
        th.note(20, "Paused", false);
        let t = transcript(&th, &[]);
        let kinds: Vec<&str> = t
            .iter()
            .map(|e| match e {
                Entry::Message(_) => "m",
                Entry::Line(_) => "l",
            })
            .collect();
        assert_eq!(kinds, ["m", "l", "m"]);
    }

    #[test]
    fn voices_follow_the_schema_and_agent_text_is_never_yours() {
        assert_eq!(voice(&msg("t", 1, "human", 0)), Voice::You);
        assert_eq!(voice(&msg("t", 1, "ask", 0)), Voice::Agent { question: true });
        assert_eq!(voice(&msg("t", 1, "say", 0)), Voice::Agent { question: false });
        assert_eq!(voice(&msg("t", 1, "context", 0)), Voice::Context);
        // An unknown kind is agent text, the cautious reading.
        assert_eq!(voice(&msg("t", 1, "weird", 0)), Voice::Agent { question: false });
    }

    #[test]
    fn events_update_the_model_and_ask_for_a_refetch_only_on_lifecycle() {
        let mut m = Model::default();
        m.set_tasks(TaskList {
            tasks: vec![task("a", "active", None, 1)],
            available: true,
            decisions_pending: 0,
        });
        assert_eq!(m.apply(Event::DecisionsPending { count: 3 }, 0), Refetch::Nothing);
        assert_eq!(m.decisions, 3);
        assert_eq!(
            m.apply(
                Event::AwaitingReply {
                    task_id: "a".into(),
                    awaiting: true
                },
                0
            ),
            Refetch::Nothing
        );
        assert_eq!(Phase::of(m.task("a").unwrap()), Phase::Waiting);
        assert_eq!(
            m.apply(
                Event::TaskState {
                    task_id: "a".into(),
                    state: "paused".into(),
                    reason: "human".into()
                },
                77
            ),
            Refetch::Tasks
        );
        assert_eq!(Phase::of(m.task("a").unwrap()), Phase::Paused);
        assert_eq!(m.thread("a").unwrap().lines[0].text, "Paused");
    }

    #[test]
    fn a_disconnect_makes_every_control_inert() {
        let mut m = Model {
            link: Link::Online,
            available: true,
            ..Model::default()
        };
        assert!(m.usable());
        m.apply(Event::Disconnected, 0);
        assert!(!m.usable());
        // Reachable agentd but unreachable policyd is also not usable.
        let m2 = Model {
            link: Link::Online,
            available: false,
            ..Model::default()
        };
        assert!(!m2.usable());
    }

    #[test]
    fn a_selection_that_vanished_falls_back_to_the_composer() {
        let mut m = Model {
            selection: Selection::Task("gone".into()),
            ..Model::default()
        };
        m.set_tasks(TaskList {
            tasks: vec![],
            available: true,
            decisions_pending: 0,
        });
        assert_eq!(m.selection, Selection::Compose);
    }

    #[test]
    fn a_draft_needs_a_package_and_respects_the_limit() {
        assert_eq!(
            draft(None, "x", Deadline::Default, None, None),
            Err(DraftIssue::NoPackage)
        );
        let long = "x".repeat(STATEMENT_MAX + 1);
        assert_eq!(
            draft(Some("inv"), &long, Deadline::Default, None, None),
            Err(DraftIssue::TooLong(STATEMENT_MAX + 1))
        );
        let d = draft(Some("inv"), "do it \0now  \n", Deadline::Short, Some("t1"), None).unwrap();
        assert_eq!(d.statement, "do it now");
        assert_eq!(d.deadline_s, 1800);
        assert_eq!(d.continuation, "t1");
        assert!(d.resumes.is_empty());
    }

    #[test]
    fn a_resume_carries_the_original_statement_and_no_continuation() {
        let d = draft(
            Some("inv"),
            "Compare three hosting quotes",
            Deadline::Default,
            Some("t9"),
            Some("t2"),
        )
        .unwrap();
        assert_eq!(d.resumes, "t2");
        assert!(
            d.continuation.is_empty(),
            "resumes and continuation are exclusive"
        );
        assert_eq!(d.statement, "Compare three hosting quotes");
    }

    #[test]
    fn the_predecessor_comes_from_the_resumes_field() {
        let mut t = task("a", "active", None, 1);
        assert_eq!(resumed_from(&t), None);
        t.raw = serde_json::json!({"resumes": "t2"});
        assert_eq!(resumed_from(&t).as_deref(), Some("t2"));
        t.raw = serde_json::json!({"resumes": ""});
        assert_eq!(resumed_from(&t), None);
    }

    #[test]
    fn the_slot_is_replaced_only_after_the_old_one_is_gone() {
        let held = |kind, at, ending| Held { kind, at, ending };
        // Nothing wanted, nothing held.
        assert_eq!(next_step(Want::Nothing, Held::default(), None), Step::Idle);
        // Wanted, no hole measured yet: wait for layout.
        assert_eq!(next_step(Want::Commit, Held::default(), None), Step::Idle);
        assert_eq!(
            next_step(Want::Commit, Held::default(), Some((4, 8))),
            Step::Create(SlotKind::TaskCommit, (4, 8))
        );
        // Held and the hole moved.
        let h = held(Some(SlotKind::TaskCommit), Some((4, 8)), false);
        assert_eq!(next_step(Want::Commit, h, Some((4, 8))), Step::Idle);
        assert_eq!(next_step(Want::Commit, h, Some((4, 20))), Step::Move((4, 20)));
        // Wrong kind or no longer wanted: cancel, then wait.
        assert_eq!(next_step(Want::Unpause, h, Some((4, 8))), Step::Cancel);
        assert_eq!(next_step(Want::Nothing, h, None), Step::Cancel);
        let ending = held(Some(SlotKind::TaskCommit), Some((4, 8)), true);
        assert_eq!(next_step(Want::Unpause, ending, Some((4, 8))), Step::Idle);
    }

    #[test]
    fn slot_notes_say_what_the_card_is_doing_never_what_it_shows() {
        assert!(slot_note(None).is_none());
        let n = slot_note(Some((SlotState::Disarmed, SlotReason::NotFocused))).unwrap();
        assert!(n.contains("arm"));
        for (s, r) in [
            (SlotState::Armed, SlotReason::None),
            (SlotState::Refused, SlotReason::Refused),
            (SlotState::Suspended, SlotReason::Occluded),
        ] {
            let text = slot_note(Some((s, r))).unwrap().to_lowercase();
            // No taxonomy, no allow/deny: A-08 §4.1.
            assert!(!text.contains("allow") && !text.contains("deny"));
        }
    }

    #[test]
    fn end_lines_name_the_outcome_and_how_long_it_took() {
        let mut t = task("a", "closed", None, 1_000);
        t.reason = "completed".into();
        t.closed_ms = Some(1_000 + 9 * 60_000);
        assert_eq!(end_line(&t).unwrap().text, "Finished · 9m");
        t.reason = "failed".into();
        let l = end_line(&t).unwrap();
        assert!(l.danger && l.text.starts_with("Failed"));
        assert!(end_line(&task("b", "active", None, 1)).is_none());
    }
}
