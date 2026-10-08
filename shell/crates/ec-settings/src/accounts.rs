// SPDX-License-Identifier: AGPL-3.0-only
//! The Accounts pane (ADR 0077): the owner's Claude accounts, managed with
//! no terminal.
//!
//! Not schema keys and not the control socket's data. The list is
//! `ec-secret account list --json`, run on a worker thread every
//! [`POLL_EVERY`] while the pane shows; a sign-in is `ec-secret account login
//! <name> --json`, its JSON-line events read on another. Settings sees
//! account names, kinds and rotation counts — never a value. The two things
//! that do take a secret are never drawn here: the store's passphrase is the
//! compositor's prompt (`secrets_unlock_prompt`), and an API key is typed into
//! `ec-secret-prompt`, the `secret`-class window (ADR 0053). The one thing this
//! pane does hold is the sign-in code the browser shows, for as long as it
//! takes to write it to the child's stdin; that copy is wiped.
//!
//! Shape: header (status chip) → hero (status grid: the store, and how many
//! accounts of each kind) → the accounts as an inset list of hairlined rows →
//! the add form as a glass panel. Panel, inset, panel: no two adjacent blocks
//! share a silhouette.
//!
//! Accent ledger: one live yellow. While a sign-in runs it is the step the
//! sign-in is waiting on, in the add panel's step line — that is the thing
//! happening now. Otherwise it is the store's state in the hero when it is
//! unlocked: an open store is what lets a task authenticate at all. Never
//! both: the hero gives its yellow up while a sign-in runs. Every pill is
//! unaccented; the kind choice lifts white (`pick_pill`).

use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::Duration;

use iced::widget::{button, column, container, row, text, text_input, Column, Space};
use iced::{Alignment, Element, Length, Subscription, Task, Theme};
use serde_json::{json, Value};

use ec_ui::theme;
use ec_ui::tokens::{color, font, size, space};
use ec_ui::widget::{
    badge, edge_note, hairline, inset, micro_label, panel, pick_pill, pill, prompt_cell, status_cell,
    status_grid, value as mono,
};

use crate::app::{App, Message};
use crate::conn::Problem;

/// The secret CLI, resolved from `PATH` like every other DE spawn.
const EC_SECRET: &str = "ec-secret";
/// The policy viewer's sibling: the `secret`-class prompt an API key is
/// typed into.
const SECRET_PROMPT: &str = "ec-secret-prompt";
/// How often the list is re-read while the pane shows. Unlocking and adding a
/// key both happen in another window, so this is how their effect arrives.
pub const POLL_EVERY: Duration = Duration::from_secs(2);
/// Longest account name (ADR 0077).
const NAME_MAX: usize = 32;
/// The name field's and the code field's widget ids.
const NAME_FIELD: &str = "accounts:name";
const CODE_FIELD: &str = "accounts:code";

/// What an account's secret is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A Claude Code OAuth token, `claude-code-token[.account]`.
    ClaudeCode,
    /// An API key, `anthropic-api-key[.account]`.
    ApiKey,
}

impl Kind {
    /// The CLI's spelling: `--kind claude-code|api-key`.
    pub fn as_arg(self) -> &'static str {
        match self {
            Kind::ClaudeCode => "claude-code",
            Kind::ApiKey => "api-key",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::ClaudeCode => "Claude Code",
            Kind::ApiKey => "API key",
        }
    }

    fn from_arg(s: &str) -> Option<Kind> {
        match s {
            "claude-code" => Some(Kind::ClaudeCode),
            "api-key" => Some(Kind::ApiKey),
            _ => None,
        }
    }
}

/// One account as the list reports it: a name, never a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub kind: Kind,
    pub rotations: u64,
}

/// What brokerd's store is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Store {
    /// Not heard from yet.
    Unread,
    /// `ec-secret` could not reach brokerd, or is not installed. The string
    /// is a hint for the human, not the CLI's words.
    Unreachable(String),
    Uninitialised,
    Locked,
    Unlocked,
}

/// One `account list` answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub store: Store,
    pub accounts: Vec<Account>,
}

/// The hint under "Secret broker isn't running".
const START_BROKER: &str =
    "Start it with systemctl --user start ec-brokerd. This pane notices within a few seconds.";

/// Read `account list --json`'s stdout. `ok` is the exit status; a non-zero
/// exit with `{"error":"unavailable"}` (or anything unparseable) is a broker
/// that cannot be reached.
pub fn parse_list(stdout: &str, ok: bool) -> Listing {
    let unreachable = || Listing {
        store: Store::Unreachable(START_BROKER.to_owned()),
        accounts: Vec::new(),
    };
    let Ok(v) = serde_json::from_str::<Value>(stdout.trim()) else {
        return unreachable();
    };
    if !ok || v.get("error").is_some() {
        return unreachable();
    }
    let flag = |k: &str| v.get(k).and_then(Value::as_bool);
    let store = match (flag("initialised"), flag("locked")) {
        (Some(false), _) => Store::Uninitialised,
        (Some(true), Some(true)) => Store::Locked,
        (Some(true), Some(false)) => Store::Unlocked,
        _ => return unreachable(),
    };
    let accounts = v
        .get("accounts")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    Some(Account {
                        name: x.get("name")?.as_str()?.to_owned(),
                        kind: Kind::from_arg(x.get("kind")?.as_str()?)?,
                        rotations: x.get("rotations").and_then(Value::as_u64).unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Listing { store, accounts }
}

/// Run `ec-secret account list --json` once. Blocks: call it off the UI
/// thread.
fn list() -> Listing {
    match Command::new(EC_SECRET)
        .args(["account", "list", "--json"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    {
        Ok(out) => parse_list(&String::from_utf8_lossy(&out.stdout), out.status.success()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Listing {
            store: Store::Unreachable(
                "ec-secret is not installed, so Settings cannot reach the store.".to_owned(),
            ),
            accounts: Vec::new(),
        },
        Err(_) => parse_list("", false),
    }
}

/// Whether `name` is an account name: 1–32 of `[A-Za-z0-9_-]`. `default` is
/// one by this rule, and is the default account.
pub fn valid_name(name: &str) -> bool {
    (1..=NAME_MAX).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// One JSON line from `account login --json`.
#[derive(Clone, PartialEq, Eq)]
pub enum LoginEvent {
    Url(String),
    CodeNeeded,
    Stored(String),
    Error {
        code: String,
        message: String,
    },
    /// Stdout closed: the child is gone or going.
    Ended,
}

/// The URL is an OAuth authorize link, not a secret, but it carries a state
/// nonce that has no business in a log.
impl fmt::Debug for LoginEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoginEvent::Url(_) => f.write_str("Url(<redacted>)"),
            LoginEvent::CodeNeeded => f.write_str("CodeNeeded"),
            LoginEvent::Stored(a) => write!(f, "Stored({a:?})"),
            LoginEvent::Error { code, .. } => write!(f, "Error({code:?})"),
            LoginEvent::Ended => f.write_str("Ended"),
        }
    }
}

/// One stdout line as an event; anything else (a blank line, an event this
/// build does not know) is `None` and ignored.
pub fn parse_event(line: &str) -> Option<LoginEvent> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
    Some(match v.get("event")?.as_str()? {
        "url" => LoginEvent::Url(s("url")),
        "code_needed" => LoginEvent::CodeNeeded,
        "stored" => LoginEvent::Stored(s("account")),
        "error" => LoginEvent::Error {
            code: s("code"),
            message: s("message"),
        },
        _ => return None,
    })
}

/// A sign-in error code in words a human can act on.
pub fn friendly(code: &str) -> &'static str {
    match code {
        "locked" => "The secret store is locked. Unlock it above, then try again.",
        "uninitialised" => "The secret store isn't set up yet. Set it up above, then try again.",
        "no_claude" => "Claude Code isn't installed on this machine, so there is nothing to sign in with.",
        "no_token" => "The sign-in finished but no token came back. Try again.",
        "timeout" => "The sign-in took too long and was stopped. Try again.",
        "cancelled" => "The sign-in was cancelled.",
        "refused" => "The secret store refused to keep the token.",
        "bad_account" => "That name can't be used. Use letters, digits, - and _, up to 32.",
        _ => "The sign-in stopped before it finished.",
    }
}

/// The sign-in code, as the code field carries it through a [`Message`].
/// `Debug` never shows it.
#[derive(Clone)]
pub struct Pasted(pub String);

impl fmt::Debug for Pasted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Pasted(<redacted>)")
    }
}

/// Overwrite every byte a string owns, spare capacity included; the same
/// best-effort wipe `ec-secret-prompt` does (no `zeroize` in this workspace).
fn wipe(value: String) {
    let mut bytes = value.into_bytes();
    let capacity = bytes.capacity();
    bytes.clear();
    bytes.resize(capacity, 0);
    std::hint::black_box(&mut bytes);
}

#[derive(Debug, Clone)]
pub enum Msg {
    /// A list answer from the poller or a one-off re-read.
    Listed(Listing),
    /// "Unlock" or "Set up secret store": the compositor's prompt.
    Unlock,
    AskRemove(String, Kind),
    CancelRemove,
    Remove(String, Kind),
    Removed(Result<(), String>),
    /// The add form's kind choice.
    Kind(Kind),
    Name(String),
    StartLogin,
    /// One event from the sign-in numbered `.0`.
    Login(u64, LoginEvent),
    OpenUrl,
    Code(Pasted),
    Continue,
    CancelLogin,
    /// Back to an empty form after a sign-in stored or failed.
    Again,
    AddKey,
}

/// Where a sign-in is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    /// Started; waiting on the authorize URL, then on the browser.
    Browser,
    /// The CLI wants the code the browser shows.
    Code,
    /// The code went to the child; waiting on `stored`.
    Checking,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Flow {
    Idle,
    Signing { url: Option<String>, step: Step },
    Stored(String),
    Failed { copy: &'static str, detail: String },
}

/// The running `account login` child. Dropping it kills the child: a
/// cancelled or abandoned sign-in leaves nothing behind.
struct Login {
    child: Child,
    stdin: Option<ChildStdin>,
    name: String,
}

impl Drop for Login {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The pane's state.
pub struct Accounts {
    store: Store,
    list: Vec<Account>,
    /// The row whose Remove is asking "are you sure".
    confirm: Option<(String, Kind)>,
    /// The row being removed right now.
    removing: Option<(String, Kind)>,
    /// Why the last removal failed, in words.
    remove_error: Option<String>,
    kind: Kind,
    name: String,
    flow: Flow,
    login: Option<Login>,
    /// Numbers each sign-in, so a cancelled one's late events are dropped.
    generation: u64,
    /// Whether this sign-in's URL has been opened for the human already.
    opened: bool,
    /// The code field. Wiped whenever it is replaced, sent or dropped.
    code: String,
    /// The API-key prompt this pane started, kept so it is reaped and a
    /// second click while it is open does not open a second one.
    key_prompt: Option<Child>,
    /// A debug-build fixture is showing: nothing is polled or run.
    fixture: bool,
}

impl Default for Accounts {
    fn default() -> Self {
        Accounts {
            store: Store::Unread,
            list: Vec::new(),
            confirm: None,
            removing: None,
            remove_error: None,
            kind: Kind::ClaudeCode,
            name: String::new(),
            flow: Flow::Idle,
            login: None,
            generation: 0,
            opened: false,
            code: String::new(),
            key_prompt: None,
            fixture: false,
        }
    }
}

impl Drop for Accounts {
    fn drop(&mut self) {
        wipe(std::mem::take(&mut self.code));
    }
}

impl Accounts {
    /// Whether the poller should run: always, bar a fixture.
    pub fn polls(&self) -> bool {
        !self.fixture
    }

    fn signing(&self) -> bool {
        matches!(self.flow, Flow::Signing { .. })
    }

    fn unlocked(&self) -> bool {
        self.store == Store::Unlocked
    }

    fn count(&self, kind: Kind) -> usize {
        self.list.iter().filter(|a| a.kind == kind).count()
    }

    /// The account the form's name and kind would replace, if there is one.
    fn replaces(&self) -> bool {
        self.list
            .iter()
            .any(|a| a.kind == self.kind && a.name == self.name)
    }

    fn set_code(&mut self, new: String) {
        wipe(std::mem::replace(&mut self.code, new));
    }

    /// Forget the running sign-in (killing its child) and go back to `flow`.
    fn end_login(&mut self, flow: Flow) {
        self.login = None;
        self.generation += 1;
        self.set_code(String::new());
        self.flow = flow;
    }
}

/// Re-read the list every [`POLL_EVERY`] while the pane shows. The first
/// read is at once, so the pane fills as soon as it opens.
pub fn poll() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(4, async move |mut sender| {
            std::thread::spawn(move || loop {
                let l = list();
                if sender.try_send(Message::Accounts(Msg::Listed(l))).is_err() && sender.is_closed() {
                    return;
                }
                std::thread::sleep(POLL_EVERY);
            });
        })
    })
}

/// Run `f` on its own thread and hand its answer back as a message.
fn off_thread<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
    to: impl Fn(T) -> Msg + Send + 'static,
) -> Task<Message> {
    let (tx, rx) = iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    Task::perform(async move { rx.await.ok() }, move |r| match r {
        Some(v) => Message::Accounts(to(v)),
        // The thread died; the next poll says what is true.
        None => Message::Accounts(Msg::CancelRemove),
    })
}

/// A list read now, rather than at the next tick.
fn reread() -> Task<Message> {
    off_thread(list, Msg::Listed)
}

/// `ec-secret account remove <name> --kind <kind> --json`.
fn remove(name: &str, kind: Kind) -> Result<(), String> {
    let out = Command::new(EC_SECRET)
        .args(["account", "remove", name, "--kind", kind.as_arg(), "--json"])
        .stdin(Stdio::null())
        .output()
        .map_err(|_| "ec-secret could not be started.".to_owned())?;
    if out.status.success() {
        return Ok(());
    }
    let said = serde_json::from_slice::<Value>(&out.stdout)
        .ok()
        .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_owned));
    Err(said.unwrap_or_else(|| {
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .next()
            .map(|l| l.trim_start_matches("ec-secret:").trim().to_owned())
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| "the secret store refused".to_owned())
    }))
}

/// Open the authorize URL in the browser. Only an https URL: the line came
/// from a child process, and `xdg-open` will open anything.
fn open_url(url: &str) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err("The sign-in link was not an https address, so it was not opened.".to_owned());
    }
    let mut child = Command::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "Could not open the browser (xdg-open).".to_owned())?;
    // Reaped off the UI thread; it exits as soon as it has handed off.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Spawn `account login <name> --json` and stream its events back.
fn start_login(app: &mut App) -> Task<Message> {
    let a = &mut app.accounts;
    let name = a.name.clone();
    let spawned = Command::new(EC_SECRET)
        .args(["account", "login", name.as_str(), "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            a.flow = Flow::Failed {
                copy: if e.kind() == std::io::ErrorKind::NotFound {
                    "ec-secret is not installed, so there is nothing to sign in with."
                } else {
                    "The sign-in could not be started."
                },
                detail: String::new(),
            };
            return Task::none();
        }
    };
    let stdout = child.stdout.take();
    let stdin = child.stdin.take();
    a.generation += 1;
    a.opened = false;
    a.login = Some(Login { child, stdin, name });
    a.flow = Flow::Signing {
        url: None,
        step: Step::Browser,
    };
    let Some(stdout) = stdout else {
        return Task::none();
    };
    let generation = a.generation;
    let (tx, rx) = iced::futures::channel::mpsc::unbounded();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Some(ev) = parse_event(&line) {
                if tx.unbounded_send(ev).is_err() {
                    return;
                }
            }
        }
        let _ = tx.unbounded_send(LoginEvent::Ended);
    });
    Task::run(rx, move |ev| Message::Accounts(Msg::Login(generation, ev)))
}

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    match msg {
        Msg::Listed(l) => {
            let a = &mut app.accounts;
            if a.fixture {
                return Task::none();
            }
            a.store = l.store;
            a.list = l.accounts;
            // A row that is gone has nothing left to confirm.
            if let Some((n, k)) = &a.confirm {
                if !a.list.iter().any(|x| &x.name == n && x.kind == *k) {
                    a.confirm = None;
                }
            }
            // The prompt this pane opened has closed: reap it.
            if a.key_prompt
                .as_mut()
                .is_some_and(|c| !matches!(c.try_wait(), Ok(None)))
            {
                a.key_prompt = None;
            }
        }
        Msg::Unlock => match app.conn.call("secrets_unlock_prompt", json!({})) {
            Ok(_) => app.banner = None,
            Err(e) => app.banner = Some(e),
        },
        Msg::AskRemove(n, k) => {
            app.accounts.remove_error = None;
            app.accounts.confirm = Some((n, k));
        }
        Msg::CancelRemove => app.accounts.confirm = None,
        Msg::Remove(n, k) => {
            let a = &mut app.accounts;
            a.confirm = None;
            if a.fixture || a.removing.is_some() {
                return Task::none();
            }
            a.removing = Some((n.clone(), k));
            return off_thread(move || remove(&n, k), Msg::Removed);
        }
        Msg::Removed(r) => {
            let a = &mut app.accounts;
            if let (Err(why), Some((n, k))) = (&r, &a.removing) {
                a.remove_error = Some(format!("Could not remove {n} ({}): {why}", k.label()));
            }
            a.removing = None;
            return reread();
        }
        Msg::Kind(k) => {
            if !app.accounts.signing() {
                app.accounts.kind = k;
            }
        }
        Msg::Name(n) => {
            if !app.accounts.signing() {
                app.accounts.name = n;
                if matches!(app.accounts.flow, Flow::Stored(_) | Flow::Failed { .. }) {
                    app.accounts.flow = Flow::Idle;
                }
            }
        }
        Msg::StartLogin => {
            let a = &app.accounts;
            if a.fixture || a.signing() || !valid_name(&a.name) || !a.unlocked() {
                return Task::none();
            }
            return start_login(app);
        }
        Msg::Login(generation, ev) => {
            let a = &mut app.accounts;
            if generation != a.generation {
                return Task::none();
            }
            match ev {
                LoginEvent::Url(u) => {
                    if !a.opened {
                        a.opened = true;
                        if let Err(why) = open_url(&u) {
                            app.banner = Some(Problem::Other(why));
                        }
                    }
                    if let Flow::Signing { url, .. } = &mut a.flow {
                        *url = Some(u);
                    }
                }
                LoginEvent::CodeNeeded => {
                    if let Flow::Signing { step, .. } = &mut a.flow {
                        *step = Step::Code;
                    }
                    return iced::widget::operation::focus(CODE_FIELD);
                }
                LoginEvent::Stored(account) => {
                    let name = if account.is_empty() {
                        a.login.as_ref().map(|l| l.name.clone()).unwrap_or_default()
                    } else {
                        account
                    };
                    a.end_login(Flow::Stored(name));
                    a.name.clear();
                    return reread();
                }
                LoginEvent::Error { code, message } => {
                    let copy = friendly(&code);
                    // The CLI's own words only where the code is not one we know.
                    let detail = if copy == friendly("") {
                        message
                    } else {
                        String::new()
                    };
                    a.end_login(Flow::Failed { copy, detail });
                }
                LoginEvent::Ended => {
                    if a.signing() {
                        a.end_login(Flow::Failed {
                            copy: friendly(""),
                            detail: String::new(),
                        });
                    }
                }
            }
        }
        Msg::OpenUrl => {
            if let Flow::Signing { url: Some(u), .. } = &app.accounts.flow {
                if let Err(why) = open_url(u) {
                    app.banner = Some(Problem::Other(why));
                }
            }
        }
        Msg::Code(Pasted(c)) => {
            let a = &mut app.accounts;
            if matches!(a.flow, Flow::Signing { step: Step::Code, .. }) {
                a.set_code(c);
            } else {
                wipe(c);
            }
        }
        Msg::Continue => {
            let a = &mut app.accounts;
            if !matches!(a.flow, Flow::Signing { step: Step::Code, .. }) || a.code.trim().is_empty() {
                return Task::none();
            }
            // Written from a slice of the one copy, then that copy is wiped;
            // stdin closes, since the CLI reads exactly one line.
            let wrote = a.login.as_mut().and_then(|l| l.stdin.take()).map(|mut s| {
                s.write_all(a.code.trim().as_bytes())
                    .and_then(|()| s.write_all(b"\n"))
                    .and_then(|()| s.flush())
            });
            a.set_code(String::new());
            match wrote {
                Some(Ok(())) => {
                    if let Flow::Signing { step, .. } = &mut a.flow {
                        *step = Step::Checking;
                    }
                }
                _ => a.end_login(Flow::Failed {
                    copy: friendly(""),
                    detail: String::new(),
                }),
            }
        }
        Msg::CancelLogin => app.accounts.end_login(Flow::Idle),
        Msg::Again => {
            let a = &mut app.accounts;
            if !a.signing() {
                a.flow = Flow::Idle;
                return iced::widget::operation::focus(NAME_FIELD);
            }
        }
        Msg::AddKey => {
            let a = &mut app.accounts;
            if a.fixture || !valid_name(&a.name) || !a.unlocked() {
                return Task::none();
            }
            let open = a
                .key_prompt
                .as_mut()
                .is_some_and(|c| matches!(c.try_wait(), Ok(None)));
            if open {
                return Task::none();
            }
            match Command::new(SECRET_PROMPT)
                .args(["api-key", a.name.as_str()])
                .spawn()
            {
                Ok(child) => a.key_prompt = Some(child),
                Err(e) => {
                    a.key_prompt = None;
                    app.banner = Some(Problem::Other(format!("could not start {SECRET_PROMPT}: {e}")));
                }
            }
        }
    }
    Task::none()
}

/// The header's two-line status chip.
pub fn status(app: &App) -> (String, String) {
    let a = &app.accounts;
    let state = match &a.store {
        Store::Unread => "store not read",
        Store::Unreachable(_) => "broker not running",
        Store::Uninitialised => "store not set up",
        Store::Locked => "store locked",
        Store::Unlocked => "store unlocked",
    };
    let measure = if a.unlocked() {
        let n = a.list.len();
        let rotated: u64 = a.list.iter().map(|x| x.rotations).sum();
        format!(
            "{n} account{} \u{b7} {rotated} rotation{}",
            if n == 1 { "" } else { "s" },
            if rotated == 1 { "" } else { "s" }
        )
    } else {
        match &a.store {
            Store::Locked => "names hidden until unlocked",
            Store::Uninitialised => "no passphrase chosen yet",
            _ => "no answer from ec-brokerd",
        }
        .to_owned()
    };
    (state.to_owned(), measure)
}

/// Every block after the header.
pub fn blocks(app: &App) -> Vec<Element<'_, Message, Theme>> {
    vec![hero(app), listing(app), add(app)]
}

fn msg(m: Msg) -> Message {
    Message::Accounts(m)
}

fn body<'a>(t: impl Into<String>) -> iced::widget::Text<'a, Theme> {
    text(t.into()).font(font::UI).size(size::BODY)
}

fn small<'a>(t: impl Into<String>) -> iced::widget::Text<'a, Theme> {
    text(t.into()).font(font::UI).size(size::BODY_SMALL)
}

fn padded<'a>(e: impl Into<Element<'a, Message, Theme>>) -> Element<'a, Message, Theme> {
    container(e)
        .padding([space::ROW_Y, space::CARD])
        .width(Length::Fill)
        .into()
}

/// A pill that does nothing right now, drawn as such.
fn off_pill<'a>(label: &str) -> Element<'a, Message, Theme> {
    pick_pill(label, false, None)
}

/// The pill that ends something for good: a removal, once confirmed.
fn danger_pill<'a>(label: &str, on_press: Message) -> Element<'a, Message, Theme> {
    button(
        text(label.to_owned())
            .font(font::UI_MEDIUM)
            .size(size::BODY_SMALL),
    )
    .padding([space::PILL_Y, space::PILL_X])
    .on_press(on_press)
    .style(theme::danger)
    .into()
}

fn plural(n: usize, one: &str) -> String {
    match n {
        0 => "None".to_owned(),
        1 => format!("1 {one}"),
        n => format!("{n} {one}s"),
    }
}

/// The hero: a status grid of the store and the two kinds, with the store's
/// one verb in the heading row.
fn hero(app: &App) -> Element<'_, Message, Theme> {
    let a = &app.accounts;
    let (state, measure) = match &a.store {
        Store::Unread => ("Reading\u{2026}", "ec-brokerd"),
        Store::Unreachable(_) => ("Not running", "ec-brokerd \u{b7} no answer"),
        Store::Uninitialised => ("Not set up", "ec-brokerd \u{b7} no passphrase yet"),
        Store::Locked => ("Locked", "ec-brokerd \u{b7} values sealed"),
        Store::Unlocked => ("Unlocked", "ec-brokerd \u{b7} read every 2 s"),
    };
    let known = |n: usize, one: &str| match &a.store {
        Store::Unlocked => plural(n, one),
        Store::Uninitialised => "None".to_owned(),
        Store::Locked => "Hidden".to_owned(),
        Store::Unread | Store::Unreachable(_) => "Unknown".to_owned(),
    };
    let cells = vec![
        status_cell("secret store", state, measure, a.unlocked() && !a.signing()),
        status_cell(
            "claude code",
            &known(a.count(Kind::ClaudeCode), "account"),
            "sign-in tokens \u{b7} api.anthropic.com",
            false,
        ),
        status_cell(
            "api keys",
            &known(a.count(Kind::ApiKey), "account"),
            "typed in a system prompt",
            false,
        ),
    ];
    let verb: Element<'_, Message, Theme> = match &a.store {
        Store::Locked => pill("Unlock\u{2026}", false, msg(Msg::Unlock)),
        Store::Uninitialised => pill("Set up secret store\u{2026}", false, msg(Msg::Unlock)),
        _ => Space::new().into(),
    };
    let foot: Element<'_, Message, Theme> = match &a.store {
        Store::Unreachable(hint) => edge_note("Secret broker isn't running", hint, color::NEUTRAL),
        Store::Locked => {
            small("Unlock to add or remove accounts. The compositor asks for the passphrase, not Settings.")
                .style(theme::text_tertiary)
                .into()
        }
        Store::Uninitialised => {
            small("Choose a passphrase to create the store. The compositor asks for it, not Settings.")
                .style(theme::text_tertiary)
                .into()
        }
        _ => small("Values never leave the broker. Settings sees names, kinds and rotation counts only.")
            .style(theme::text_tertiary)
            .into(),
    };
    panel(
        app.glass_radius,
        column![
            row![
                micro_label("secret store"),
                Space::new().width(Length::Fill),
                verb
            ]
            .align_y(Alignment::Center),
            status_grid(cells, 3),
            foot,
        ]
        .spacing(space::ROW_Y),
    )
    .into()
}

/// The accounts, one hairlined row each.
fn listing(app: &App) -> Element<'_, Message, Theme> {
    let a = &app.accounts;
    let mut col = Column::new().push(padded(micro_label("accounts")));
    let empty = |head: &str, sub: &str| {
        padded(
            column![
                body(head.to_owned()).style(theme::text_primary),
                small(sub.to_owned()).style(theme::text_tertiary),
            ]
            .spacing(space::LINE_GAP),
        )
    };
    match &a.store {
        Store::Unlocked if a.list.is_empty() => {
            col = col.push(hairline()).push(empty(
                "No accounts yet.",
                "Add one below. Each task picks its account in the console's New task composer.",
            ));
        }
        Store::Unlocked => {
            for x in &a.list {
                col = col.push(hairline()).push(account_row(a, x));
            }
        }
        Store::Unread => col = col.push(hairline()).push(empty("Reading the store\u{2026}", "")),
        Store::Unreachable(_) => {
            col = col.push(hairline()).push(empty(
                "No accounts to show.",
                "The list comes from the secret broker, and it isn't answering.",
            ));
        }
        Store::Locked => {
            col = col.push(hairline()).push(empty(
                "Account names are sealed.",
                "They show here once the store is unlocked.",
            ));
        }
        Store::Uninitialised => {
            col = col.push(hairline()).push(empty(
                "There is no store yet.",
                "Set it up above, then add the first account below.",
            ));
        }
    }
    if let Some(why) = &a.remove_error {
        col = col
            .push(hairline())
            .push(padded(small(why.clone()).style(theme::text_danger)));
    }
    inset(col).into()
}

fn account_row<'a>(a: &Accounts, x: &Account) -> Element<'a, Message, Theme> {
    let key = (x.name.clone(), x.kind);
    if a.confirm.as_ref() == Some(&key) {
        return padded(
            row![
                column![
                    body(format!("Remove {}?", x.name)).style(theme::text_primary),
                    small(format!(
                        "Its {} is revoked. Tasks set to {} stop signing in.",
                        if x.kind == Kind::ClaudeCode {
                            "token"
                        } else {
                            "key"
                        },
                        x.name
                    ))
                    .style(theme::text_tertiary),
                ]
                .spacing(space::LINE_GAP)
                .width(Length::Fill),
                pill("Keep", false, msg(Msg::CancelRemove)),
                danger_pill("Remove", msg(Msg::Remove(x.name.clone(), x.kind))),
            ]
            .spacing(space::CONTROL_GAP)
            .align_y(Alignment::Center),
        );
    }
    let mut who = column![row![
        body(x.name.clone()).style(theme::text_primary),
        badge(x.kind.label())
    ]
    .spacing(space::CONTROL_GAP)
    .align_y(Alignment::Center)]
    .spacing(space::LINE_GAP);
    if x.name == "default" {
        who = who.push(small("Used when a task names no account").style(theme::text_tertiary));
    }
    let rotated = match x.rotations {
        0 => "never rotated".to_owned(),
        1 => "rotated 1\u{d7}".to_owned(),
        n => format!("rotated {n}\u{d7}"),
    };
    let verb = if a.removing.as_ref() == Some(&key) {
        mono("removing\u{2026}")
    } else if a.removing.is_some() {
        off_pill("Remove")
    } else {
        pill("Remove", false, msg(Msg::AskRemove(x.name.clone(), x.kind)))
    };
    padded(
        row![
            container(who).width(Length::Fill),
            text(rotated)
                .font(font::DATA)
                .size(size::MONO)
                .style(theme::text_tertiary),
            verb,
        ]
        .spacing(space::BLOCK)
        .align_y(Alignment::Center),
    )
}

/// The three steps of a sign-in, as one mono line: done ones secondary, the
/// one waited on in the accent, the rest tertiary.
fn steps<'a>(at: Option<usize>, done: bool) -> Element<'a, Message, Theme> {
    const STEPS: [&str; 3] = ["1  browser sign-in", "2  paste the code", "3  stored"];
    let mut r = iced::widget::Row::new()
        .spacing(space::CONTROL_GAP)
        .align_y(Alignment::Center);
    for (i, s) in STEPS.iter().enumerate() {
        if i > 0 {
            r = r.push(
                text("\u{203a}")
                    .font(font::DATA)
                    .size(size::MONO)
                    .style(theme::text_tertiary),
            );
        }
        let style: fn(&Theme) -> iced::widget::text::Style = match at {
            _ if done => theme::text_secondary,
            Some(n) if n == i => theme::text_accent,
            Some(n) if n > i => theme::text_secondary,
            _ => theme::text_tertiary,
        };
        r = r.push(text(*s).font(font::DATA_MEDIUM).size(size::MONO).style(style));
    }
    r.into()
}

/// The add form: a kind, a name, then that kind's way in.
fn add(app: &App) -> Element<'_, Message, Theme> {
    let a = &app.accounts;
    let signing = a.signing();
    let pick = |k: Kind| pick_pill(k.label(), a.kind == k, (!signing).then_some(msg(Msg::Kind(k))));

    let head = row![
        micro_label("add an account"),
        Space::new().width(Length::Fill),
        pick(Kind::ClaudeCode),
        pick(Kind::ApiKey),
    ]
    .spacing(space::PILL_GAP)
    .align_y(Alignment::Center);

    // The name and what it will do: refused, replacing, or the rule.
    let valid = valid_name(&a.name);
    let mut field = text_input("e.g. research", &a.name)
        .id(NAME_FIELD)
        .font(font::DATA)
        .size(size::BODY)
        .width(Length::Fixed(space::FIELD_W))
        .style(if a.name.is_empty() || valid {
            theme::eclipse_input
        } else {
            theme::eclipse_input_invalid
        });
    if !signing {
        field = field.on_input(|n| msg(Msg::Name(n)));
        if valid {
            field = field.on_submit(msg(match a.kind {
                Kind::ClaudeCode => Msg::StartLogin,
                Kind::ApiKey => Msg::AddKey,
            }));
        }
    }
    let verdict = if !a.name.is_empty() && !valid {
        small("Only letters, digits, - and _, up to 32 characters.").style(theme::text_danger)
    } else if valid && a.replaces() {
        small(match a.kind {
            Kind::ClaudeCode => format!(
                "{} already has a Claude Code sign-in. Signing in again replaces it.",
                a.name
            ),
            Kind::ApiKey => format!("{} already has an API key. A new key replaces it.", a.name),
        })
        .style(theme::text_secondary)
    } else {
        small("Letters, digits, - and _. \"default\" is the account a task gets when it names none.")
            .style(theme::text_tertiary)
    };
    let name_row = row![
        column![body("Name").style(theme::text_secondary), verdict]
            .spacing(space::LINE_GAP)
            .width(Length::Fill),
        field,
    ]
    .spacing(space::BLOCK)
    .align_y(Alignment::Center);

    let ready = valid && a.unlocked();
    let gate = match &a.store {
        Store::Unlocked => "Name the account to continue.",
        Store::Locked => "Unlock the secret store first.",
        Store::Uninitialised => "Set up the secret store first.",
        Store::Unread => "Reading the secret store\u{2026}",
        Store::Unreachable(_) => "The secret broker isn't running.",
    };

    let way: Element<'_, Message, Theme> = match a.kind {
        Kind::ApiKey => {
            let open = a.key_prompt.is_some();
            let (line, verb): (String, Element<'_, Message, Theme>) = if open {
                (
                    "Waiting on the key prompt. This list updates once it is saved.".to_owned(),
                    off_pill("Prompt open"),
                )
            } else if ready {
                (
                    "The key is typed into a system prompt, never into Settings.".to_owned(),
                    pill("Enter API key\u{2026}", false, msg(Msg::AddKey)),
                )
            } else {
                (
                    "The key is typed into a system prompt, never into Settings.".to_owned(),
                    off_pill("Enter API key\u{2026}"),
                )
            };
            row![small(line).style(theme::text_tertiary).width(Length::Fill), verb]
                .spacing(space::CONTROL_GAP)
                .align_y(Alignment::Center)
                .into()
        }
        Kind::ClaudeCode => login_way(a, ready, gate),
    };

    panel(
        app.glass_radius,
        column![head, hairline(), name_row, hairline(), way].spacing(space::ROW_Y),
    )
    .into()
}

/// The Claude Code half of the add form: the step line, then whatever the
/// sign-in is waiting on.
fn login_way<'a>(a: &'a Accounts, ready: bool, gate: &'static str) -> Element<'a, Message, Theme> {
    let line =
        |t: String, style: fn(&Theme) -> iced::widget::text::Style| small(t).style(style).width(Length::Fill);
    let actions = |lead: Element<'a, Message, Theme>, pills: Vec<Element<'a, Message, Theme>>| {
        let mut r = row![lead].spacing(space::CONTROL_GAP).align_y(Alignment::Center);
        for p in pills {
            r = r.push(p);
        }
        r
    };
    let sign_in = |url: &Option<String>| -> Element<'a, Message, Theme> {
        if url.is_some() {
            pill("Sign in with Claude", false, msg(Msg::OpenUrl))
        } else {
            off_pill("Sign in with Claude")
        }
    };
    match &a.flow {
        Flow::Idle => column![
            steps(None, false),
            actions(
                line(
                    if ready {
                        "Opens your browser to sign in. Nothing is typed into a terminal.".to_owned()
                    } else {
                        gate.to_owned()
                    },
                    theme::text_tertiary
                )
                .into(),
                vec![if ready {
                    pill("Start sign-in", false, msg(Msg::StartLogin))
                } else {
                    off_pill("Start sign-in")
                }],
            ),
        ]
        .spacing(space::ROW_Y)
        .into(),
        Flow::Signing {
            url,
            step: Step::Browser,
        } => column![
            steps(Some(0), false),
            actions(
                line(
                    if url.is_some() {
                        "Finish signing in in the browser window that opened.".to_owned()
                    } else {
                        "Starting Claude Code\u{2026}".to_owned()
                    },
                    theme::text_secondary
                )
                .into(),
                vec![pill("Cancel", false, msg(Msg::CancelLogin)), sign_in(url)],
            ),
        ]
        .spacing(space::ROW_Y)
        .into(),
        Flow::Signing { url, step } => {
            let checking = *step == Step::Checking;
            let mut field = text_input(
                if checking {
                    "checking\u{2026}"
                } else {
                    "Paste the code shown after you sign in"
                },
                &a.code,
            )
            .id(CODE_FIELD)
            .secure(true)
            .font(font::DATA)
            .size(size::BODY)
            .style(theme::prompt_input);
            if !checking {
                field = field
                    .on_input(|c| msg(Msg::Code(Pasted(c))))
                    .on_submit(msg(Msg::Continue));
            }
            let go = if a.code.trim().is_empty() {
                off_pill("Continue")
            } else {
                pill("Continue", false, msg(Msg::Continue))
            };
            column![
                steps(Some(if checking { 2 } else { 1 }), false),
                prompt_cell("\u{203a}", field, None),
                actions(
                    line(
                        if checking {
                            "Checking the code with Claude Code\u{2026}".to_owned()
                        } else {
                            "The code is passed to Claude Code once, then forgotten.".to_owned()
                        },
                        theme::text_tertiary
                    )
                    .into(),
                    if checking {
                        vec![pill("Cancel", false, msg(Msg::CancelLogin))]
                    } else {
                        vec![sign_in(url), pill("Cancel", false, msg(Msg::CancelLogin)), go]
                    },
                ),
            ]
            .spacing(space::ROW_Y)
            .into()
        }
        Flow::Stored(name) => column![
            steps(None, true),
            actions(
                column![
                    body(format!("{name} is signed in.")).style(theme::text_primary),
                    small("Tasks can pick it in the console's New task composer.")
                        .style(theme::text_tertiary),
                ]
                .spacing(space::LINE_GAP)
                .width(Length::Fill)
                .into(),
                vec![pill("Add another", false, msg(Msg::Again))],
            ),
        ]
        .spacing(space::ROW_Y)
        .into(),
        Flow::Failed { copy, detail } => {
            let body = if detail.is_empty() {
                (*copy).to_owned()
            } else {
                format!("{copy} ({detail})")
            };
            actions(
                container(edge_note("Sign-in didn't finish", &body, color::DANGER))
                    .width(Length::Fill)
                    .into(),
                vec![pill("Try again", false, msg(Msg::Again))],
            )
            .into()
        }
    }
}

/// Debug builds only: `SETTINGS_PREVIEW_ACCOUNTS=<store>[,<flow>]` opens the
/// pane on a fixture, with nothing polled or run, so every state can be
/// screenshotted. `<store>` is `unlocked`, `locked`, `uninit`, `unreachable`
/// or `empty` (unlocked, no accounts); `<flow>` is `browser`, `url`, `code`,
/// `typed`, `checking`, `stored`, `failed`, `confirm`, `replace`, `apikey` or
/// `bad`.
#[cfg(debug_assertions)]
pub fn preview_env(a: &mut Accounts) {
    let Ok(spec) = std::env::var("SETTINGS_PREVIEW_ACCOUNTS") else {
        return;
    };
    let (store, flow) = spec.split_once(',').unwrap_or((spec.as_str(), ""));
    a.fixture = true;
    let sample = || {
        vec![
            Account {
                name: "default".into(),
                kind: Kind::ClaudeCode,
                rotations: 0,
            },
            Account {
                name: "work".into(),
                kind: Kind::ClaudeCode,
                rotations: 2,
            },
            Account {
                name: "ci-runner".into(),
                kind: Kind::ApiKey,
                rotations: 1,
            },
        ]
    };
    (a.store, a.list) = match store {
        "locked" => (Store::Locked, vec![]),
        "uninit" => (Store::Uninitialised, vec![]),
        "unreachable" => (Store::Unreachable(START_BROKER.to_owned()), vec![]),
        "empty" => (Store::Unlocked, vec![]),
        _ => (Store::Unlocked, sample()),
    };
    let url = Some("https://claude.com/cai/oauth/authorize?fixture".to_owned());
    a.name = "research".into();
    match flow {
        "browser" => {
            a.flow = Flow::Signing {
                url: None,
                step: Step::Browser,
            }
        }
        "url" => {
            a.flow = Flow::Signing {
                url,
                step: Step::Browser,
            }
        }
        "code" => {
            a.flow = Flow::Signing {
                url,
                step: Step::Code,
            }
        }
        "typed" => {
            a.code = "fixture-code".into();
            a.flow = Flow::Signing {
                url,
                step: Step::Code,
            };
        }
        "checking" => {
            a.flow = Flow::Signing {
                url,
                step: Step::Checking,
            }
        }
        "stored" => {
            a.name.clear();
            a.flow = Flow::Stored("research".into());
        }
        "failed" => {
            a.flow = Flow::Failed {
                copy: friendly("timeout"),
                detail: String::new(),
            }
        }
        "confirm" => a.confirm = Some(("work".into(), Kind::ClaudeCode)),
        "replace" => a.name = "work".into(),
        "apikey" => {
            a.kind = Kind::ApiKey;
            a.name = "ci-runner".into();
        }
        "bad" => a.name = "my account!".into(),
        _ => a.name.clear(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_account_rule() {
        assert!(valid_name("default"));
        assert!(valid_name("work"));
        assert!(valid_name("a_b-9"));
        assert!(valid_name(&"a".repeat(32)));
        assert!(!valid_name(""));
        assert!(!valid_name(&"a".repeat(33)));
        assert!(!valid_name("has space"));
        assert!(!valid_name("dot.ted"));
        assert!(!valid_name("ünï"));
    }

    #[test]
    fn the_list_reads_every_store_state() {
        let l = parse_list(
            r#"{"initialised":true,"locked":false,"accounts":[{"name":"default","kind":"claude-code","rotations":0},{"name":"work","kind":"api-key","rotations":1}]}"#,
            true,
        );
        assert_eq!(l.store, Store::Unlocked);
        assert_eq!(
            l.accounts,
            vec![
                Account {
                    name: "default".into(),
                    kind: Kind::ClaudeCode,
                    rotations: 0
                },
                Account {
                    name: "work".into(),
                    kind: Kind::ApiKey,
                    rotations: 1
                },
            ]
        );
        let locked = parse_list(r#"{"initialised":true,"locked":true,"accounts":[]}"#, true);
        assert_eq!(locked.store, Store::Locked);
        let fresh = parse_list(r#"{"initialised":false,"locked":true,"accounts":[]}"#, true);
        assert_eq!(fresh.store, Store::Uninitialised);
        let down = parse_list(r#"{"error":"unavailable","message":"no socket"}"#, false);
        assert!(matches!(down.store, Store::Unreachable(_)));
        assert!(matches!(parse_list("", false).store, Store::Unreachable(_)));
    }

    #[test]
    fn an_unknown_kind_is_left_out_not_misfiled() {
        let l = parse_list(
            r#"{"initialised":true,"locked":false,"accounts":[{"name":"x","kind":"ssh"},{"name":"y","kind":"api-key"}]}"#,
            true,
        );
        assert_eq!(l.accounts.len(), 1);
        assert_eq!(l.accounts[0].rotations, 0);
    }

    #[test]
    fn login_events_parse_and_unknown_ones_are_ignored() {
        assert_eq!(
            parse_event(r#"{"event":"url","url":"https://claude.com/cai/oauth/authorize?x"}"#),
            Some(LoginEvent::Url("https://claude.com/cai/oauth/authorize?x".into()))
        );
        assert_eq!(
            parse_event(r#"{"event":"code_needed"}"#),
            Some(LoginEvent::CodeNeeded)
        );
        assert_eq!(
            parse_event(r#"{"event":"stored","account":"work"}"#),
            Some(LoginEvent::Stored("work".into()))
        );
        assert_eq!(
            parse_event(r#"{"event":"error","code":"timeout","message":"m"}"#),
            Some(LoginEvent::Error {
                code: "timeout".into(),
                message: "m".into()
            })
        );
        assert_eq!(parse_event(r#"{"event":"progress"}"#), None);
        assert_eq!(parse_event("not json"), None);
    }

    #[test]
    fn every_error_code_has_its_own_words() {
        let codes = [
            "locked",
            "uninitialised",
            "no_claude",
            "no_token",
            "timeout",
            "cancelled",
            "refused",
            "bad_account",
        ];
        for c in codes {
            assert_ne!(friendly(c), friendly("something-else"), "{c}");
        }
    }

    #[test]
    fn debug_never_shows_the_code_or_the_url() {
        let m = Msg::Code(Pasted("abc123secret".into()));
        assert!(!format!("{m:?}").contains("abc123secret"));
        let e = Msg::Login(1, LoginEvent::Url("https://x/?state=nonce42".into()));
        assert!(!format!("{e:?}").contains("nonce42"));
    }

    #[test]
    fn only_an_https_link_is_opened() {
        assert!(open_url("file:///etc/passwd").is_err());
        assert!(open_url("javascript:alert(1)").is_err());
    }
}
