// SPDX-License-Identifier: AGPL-3.0-only
//! Notification history, mode and read state: the newest [`CAP`]
//! notifications, kept by the toasts process, persisted to
//! `$XDG_STATE_HOME/eclipse/notifications.json`, and readable from the taskbar
//! and the notification widget through a private session-bus interface,
//! `org.eclipse.NotificationHistory`.
//!
//! * **Persisted.** Written atomically (temp file, rename, 0600) after every
//!   change, restored on start. Restored entries are never `live`. Images
//!   (`image-data`) are not persisted; the icon name is.
//! * **Transient-hinted notifications are never stored.** No hint marks a
//!   notification private or sensitive in this server today, so there is
//!   nothing else to exclude; do not invent one here without an ADR.
//! * **Mode** (`normal` / `quiet` / `dnd`) lives here too, and is persisted
//!   with the history. `dnd` stores nothing; `quiet` stores unread without a
//!   popup. Per-app [`Rules`](super::rules) can mute or quieten one app.
//! * **Unread** is every stored entry not yet marked read: quiet arrivals and
//!   toasts nobody opened the widget for. Marking read keeps the entry; it
//!   moves from the widget's quick view to its history drawer.
//! * **Dismissing a toast keeps its entry.** Only [`Store::remove`] and
//!   [`Store::clear`] delete one.
//! * **Bodies are never logged**, same as the rest of this module.
//! * `InvokeAction` works only while the source notification is still live:
//!   received by this running service (shown, quiet or viewing-suppressed)
//!   and not since closed by the sender, dismissed/expired at the toast,
//!   replaced or acted on. Restored entries are never live; they are read-only.
//! * **Only D-Bus notifications.** Compositor-originated notices (the
//!   persistent config-error notice, `compositor.rs`) go straight to the toast
//!   stack, bypass mode and rules, and are not recorded here.
//!
//! The bus interface is served on the toasts connection under its own name,
//! `org.eclipse.NotificationHistory`, never `org.freedesktop.Notifications`:
//! a sandbox granted the latter to post notices gets no access to it
//! (`xdg-dbus-proxy` filters by name). [`HistoryClient`] is the
//! blocking-zbus client the bar and widget use, with no async runtime.
//!
//! Wire, at `/org/eclipse/NotificationHistory`: see
//! `docs/internal/notifications-contract.md`. Any process that can reach that
//! name on the session bus can use it, like a click by the same human;
//! sandboxes are not given it.

use std::collections::{HashSet, VecDeque};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use zbus::blocking::{Connection, MessageIterator};
use zbus::MatchRule;

use super::rules::Rules;
use super::{Mode, Notification, Urgency};

/// How many entries are kept; the oldest falls off first.
pub const CAP: usize = 200;

/// The history's own bus name. Not the notification name: sandboxes are
/// granted that one to post, and must not be able to read or drive history.
pub(crate) const HISTORY_BUS_NAME: &str = "org.eclipse.NotificationHistory";
pub(crate) const HISTORY_PATH: &str = "/org/eclipse/NotificationHistory";
pub(crate) const HISTORY_IFACE: &str = "org.eclipse.NotificationHistory";

const STATE_FILE: &str = "notifications.json";

/// One remembered notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub id: u32,
    pub app_name: String,
    /// Icon name or path, as the notification carried it. Empty if none.
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    pub urgency: Urgency,
    /// Seconds since the Unix epoch when it was posted.
    pub timestamp: u64,
    /// `(key, label)` buttons.
    pub actions: Vec<(String, String)>,
    /// Still on screen (or queued), so `InvokeAction` will be honoured.
    pub live: bool,
}

/// A history entry with the widget's extra fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageEntry {
    pub entry: HistoryEntry,
    /// Seen in the widget (or marked read). Unread entries are the quick view.
    pub read: bool,
    pub category: String,
    pub desktop_entry: String,
    /// Grouping key; see [`Notification::group`].
    pub group: String,
    pub progress: Option<u8>,
    /// The `resident` hint. Not persisted.
    pub resident: bool,
}

/// Which entries a page lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageFilter {
    All,
    Unread,
    Read,
}

impl PageFilter {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Unread => "unread",
            Self::Read => "read",
        }
    }

    /// Unknown strings are `All`.
    pub fn parse(s: &str) -> Self {
        match s {
            "unread" => Self::Unread,
            "read" => Self::Read,
            _ => Self::All,
        }
    }

    fn keeps(self, e: &PageEntry) -> bool {
        match self {
            Self::All => true,
            Self::Unread => !e.read,
            Self::Read => e.read,
        }
    }
}

/// One page of history, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Page {
    /// Entries matching the filter, across all pages.
    pub total: u32,
    /// Unread entries overall, whatever the filter.
    pub unread: u32,
    pub mode: Mode,
    pub entries: Vec<PageEntry>,
}

impl PageEntry {
    fn to_json(&self) -> Value {
        let e = &self.entry;
        json!({
            "id": e.id,
            "app": e.app_name,
            "icon": e.app_icon,
            "summary": e.summary,
            "body": e.body,
            "urgency": e.urgency.as_str(),
            "timestamp": e.timestamp,
            "actions": e.actions.iter()
                .map(|(k, l)| json!({"key": k, "label": l}))
                .collect::<Vec<_>>(),
            "live": e.live,
            "read": self.read,
            "category": self.category,
            "desktop_entry": self.desktop_entry,
            "group": self.group,
            "progress": self.progress,
        })
    }

    /// Anything missing or mistyped that is not essential gets its default;
    /// without an id there is no entry.
    fn from_json(v: &Value) -> Option<Self> {
        let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
        let app = text("app");
        let desktop_entry = text("desktop_entry");
        let group = match text("group") {
            g if g.is_empty() => super::hints::group_key(&desktop_entry, &app),
            g => g,
        };
        Some(Self {
            entry: HistoryEntry {
                id: u32::try_from(v.get("id")?.as_u64()?).ok()?,
                app_name: app,
                app_icon: text("icon"),
                summary: text("summary"),
                body: text("body"),
                urgency: Urgency::parse(&text("urgency")).unwrap_or_default(),
                timestamp: v.get("timestamp").and_then(Value::as_u64).unwrap_or(0),
                actions: v
                    .get("actions")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| {
                                Some((
                                    x.get("key")?.as_str()?.to_owned(),
                                    x.get("label")?.as_str()?.to_owned(),
                                ))
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                live: v.get("live").and_then(Value::as_bool).unwrap_or(false),
            },
            read: v.get("read").and_then(Value::as_bool).unwrap_or(false),
            category: text("category"),
            desktop_entry,
            group,
            progress: v
                .get("progress")
                .and_then(Value::as_u64)
                .map(|p| p.min(100) as u8),
            resident: false,
        })
    }
}

impl Page {
    pub fn to_json(&self) -> String {
        json!({
            "total": self.total,
            "unread": self.unread,
            "mode": self.mode.as_str(),
            "entries": self.entries.iter().map(PageEntry::to_json).collect::<Vec<_>>(),
        })
        .to_string()
    }

    pub fn from_json(text: &str) -> Option<Self> {
        let v: Value = serde_json::from_str(text).ok()?;
        Some(Self {
            total: u32::try_from(v.get("total")?.as_u64()?).ok()?,
            unread: u32::try_from(v.get("unread")?.as_u64()?).ok()?,
            mode: Mode::parse(v.get("mode")?.as_str()?).unwrap_or_default(),
            entries: v
                .get("entries")?
                .as_array()?
                .iter()
                .filter_map(PageEntry::from_json)
                .collect(),
        })
    }
}

/// `(id, app_name, app_icon, summary, body, urgency, timestamp, actions, live)`
/// — the D-Bus signature `(usssssyta(ss)b)`.
pub(crate) type WireEntry = (
    u32,
    String,
    String,
    String,
    String,
    u8,
    u64,
    Vec<(String, String)>,
    bool,
);

fn to_wire(e: HistoryEntry) -> WireEntry {
    let urgency = match e.urgency {
        Urgency::Low => 0,
        Urgency::Normal => 1,
        Urgency::Critical => 2,
    };
    (
        e.id,
        e.app_name,
        e.app_icon,
        e.summary,
        e.body,
        urgency,
        e.timestamp,
        e.actions,
        e.live,
    )
}

fn from_wire(w: WireEntry) -> HistoryEntry {
    HistoryEntry {
        id: w.0,
        app_name: w.1,
        app_icon: w.2,
        summary: w.3,
        body: w.4,
        urgency: Urgency::from_hint(w.5),
        timestamp: w.6,
        actions: w.7,
        live: w.8,
    }
}

/// What the server does with an arrival.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Popup (and any sound), stored.
    Show,
    /// Stored unread, no popup.
    Quiet,
    /// Ignored: not shown, not stored.
    Drop,
}

/// The bounded store. Oldest at the front.
#[derive(Debug, Default)]
pub(crate) struct Store {
    entries: VecDeque<PageEntry>,
    /// Ids invokable this session: every arrival this process admitted
    /// (shown, quiet or viewing-suppressed) until closed. Stored or not.
    live: HashSet<u32>,
    /// Ids with a toast on screen or queued (a subset of `live`).
    shown: HashSet<u32>,
    mode: Mode,
    rules: Rules,
    /// Where to persist; `None` keeps everything in memory (tests).
    path: Option<PathBuf>,
    dirty: bool,
    /// The drawer is open (set by `SetViewing`): when it was set, in
    /// `now_secs` time. Never persisted; lapses after [`VIEWING_TIMEOUT`].
    viewing_since: Option<u64>,
    /// Unique name of the client a name-owner watch is already running for.
    viewing_watch: Option<String>,
}

/// Safety net: a viewing flag nobody cleared stops suppressing popups after this.
pub(crate) const VIEWING_TIMEOUT: u64 = 15 * 60;

impl Store {
    /// The store at the standard state path, restored from disk, with the
    /// user's rules.
    pub(crate) fn open() -> Self {
        let path = crate::frecency::path_for(STATE_FILE);
        let mut s = path.as_deref().map(Self::load_from).unwrap_or_default();
        s.path = path;
        s.rules = Rules::load_default();
        s
    }

    /// Missing, unreadable or corrupt is empty, never an error.
    pub(crate) fn load_from(path: &Path) -> Self {
        let mut s = Self::default();
        let Ok(text) = std::fs::read_to_string(path) else {
            return s;
        };
        let Ok(top) = serde_json::from_str::<Value>(&text) else {
            return s;
        };
        s.mode = top
            .get("mode")
            .and_then(Value::as_str)
            .and_then(Mode::parse)
            .unwrap_or_default();
        if let Some(a) = top.get("entries").and_then(Value::as_array) {
            let mut seen = HashSet::new();
            s.entries = a
                .iter()
                .filter_map(PageEntry::from_json)
                .filter(|e| seen.insert(e.entry.id))
                .map(|mut e| {
                    e.entry.live = false;
                    e
                })
                .collect();
            while s.entries.len() > CAP {
                s.entries.pop_front();
            }
        }
        s
    }

    /// Point the store at a file (tests, and `open`).
    #[cfg(test)]
    pub(crate) fn at(mut self, path: PathBuf) -> Self {
        self.path = Some(path);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_rules(mut self, rules: Rules) -> Self {
        self.rules = rules;
        self
    }

    /// Write to disk if anything changed since the last write. Atomic: a temp
    /// file in the same directory, then a rename.
    pub(crate) fn save(&mut self) {
        if !self.dirty {
            return;
        }
        let Some(path) = self.path.clone() else {
            return;
        };
        self.dirty = false;
        if let Err(e) = self.write_to(&path) {
            warn_save(&e);
        }
    }

    fn write_to(&self, path: &Path) -> std::io::Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let dir = path
            .parent()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no parent"))?;
        std::fs::create_dir_all(dir)?;
        let doc = json!({
            "version": 1,
            "mode": self.mode.as_str(),
            "entries": self.entries.iter().map(PageEntry::to_json).collect::<Vec<_>>(),
        });
        let tmp = dir.join(format!(".notifications.{}.tmp", std::process::id()));
        let written = (|| {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(doc.to_string().as_bytes())?;
            f.sync_all()?;
            std::fs::rename(&tmp, path)
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        written
    }

    pub(crate) fn mode(&self) -> Mode {
        self.mode
    }

    /// True if the mode changed.
    pub(crate) fn set_mode(&mut self, mode: Mode) -> bool {
        let changed = self.mode != mode;
        self.mode = mode;
        self.dirty |= changed;
        changed
    }

    /// The drawer opened or closed. Presentation only; not persisted.
    pub(crate) fn set_viewing(&mut self, on: bool, now: u64) {
        self.viewing_since = on.then_some(now);
    }

    /// True while the drawer is open and the flag has not lapsed.
    pub(crate) fn viewing(&self, now: u64) -> bool {
        self.viewing_since
            .is_some_and(|t| now.saturating_sub(t) < VIEWING_TIMEOUT)
    }

    /// Claim the name-owner watch for `owner`; false if one already runs.
    pub(crate) fn claim_viewing_watch(&mut self, owner: &str) -> bool {
        if self.viewing_watch.as_deref() == Some(owner) {
            return false;
        }
        self.viewing_watch = Some(owner.to_owned());
        true
    }

    /// The watched client vanished: viewing ends, the watch is spent.
    pub(crate) fn viewing_owner_gone(&mut self, owner: &str) {
        if self.viewing_watch.as_deref() == Some(owner) {
            self.viewing_watch = None;
            self.viewing_since = None;
        }
    }

    /// Highest stored id, so fresh ids never collide with restored ones.
    pub(crate) fn max_id(&self) -> u32 {
        self.entries
            .iter()
            .map(|e| e.entry.id)
            .filter(|&id| id != super::compositor::CONFIG_ID)
            .max()
            .unwrap_or(0)
    }

    pub(crate) fn unread(&self) -> u32 {
        self.entries.iter().filter(|e| !e.read).count() as u32
    }

    /// Apply the user's rules and the mode to an arrival, then record it.
    /// `n.urgency` and `n.expires_in` are final afterwards. The verdict says
    /// whether to pop it up; after anything but `Drop` the history changed.
    pub(crate) fn admit(&mut self, n: &mut Notification, expire_timeout: i32, now: u64) -> Verdict {
        self.rules.refresh();
        let rule = self.rules.find(&n.desktop_entry, &n.app_name).cloned();
        let mut timeout = expire_timeout;
        if let Some(r) = &rule {
            if let Some(u) = r.urgency {
                n.urgency = u;
            }
            if let Some(ms) = r.timeout_ms {
                timeout = ms;
            }
        }
        n.expires_in = Notification::lifetime(timeout, n.urgency);

        let verdict = if self.mode == Mode::Dnd || rule.as_ref().is_some_and(|r| r.mute) {
            Verdict::Drop
        } else if self.mode == Mode::Quiet || rule.as_ref().is_some_and(|r| r.quiet) {
            Verdict::Quiet
        } else if self.viewing(now) && n.urgency != Urgency::Critical && n.id != super::compositor::CONFIG_ID
        {
            // The open drawer already shows it: stored, no popup, no sound.
            Verdict::Quiet
        } else {
            Verdict::Show
        };
        match verdict {
            Verdict::Drop => {
                // A replacement of something stored still supersedes it, so
                // stale actions do not answer for new content.
                if n.transient {
                    self.take(n.id);
                }
            }
            Verdict::Show => {
                self.shown.insert(n.id);
                self.posted(n, now);
            }
            Verdict::Quiet => {
                self.shown.remove(&n.id);
                self.posted(n, now);
            }
        }
        verdict
    }

    /// Record an arrival; it is invokable this session. True if the history changed.
    fn posted(&mut self, n: &Notification, now: u64) -> bool {
        self.live.insert(n.id);
        let live = true;
        // A replacement supersedes the old entry, even when the new one is
        // transient: the old actions must not answer for the new content.
        let had = self.take(n.id);
        if n.transient {
            return had;
        }
        if self.entries.len() == CAP {
            self.entries.pop_front();
        }
        self.entries.push_back(PageEntry {
            entry: HistoryEntry {
                id: n.id,
                app_name: n.app_name.clone(),
                app_icon: n.icon.clone(),
                summary: n.summary.clone(),
                body: n.body.clone(),
                urgency: n.urgency,
                timestamp: now,
                actions: n
                    .actions
                    .iter()
                    .map(|a| (a.key.clone(), a.label.clone()))
                    .collect(),
                live,
            },
            read: false,
            category: n.category.clone(),
            desktop_entry: n.desktop_entry.clone(),
            group: n.group.clone(),
            progress: n.progress,
            resident: n.resident,
        });
        self.dirty = true;
        true
    }

    /// The notification was closed (sender, toast dismissal/expiry, or an
    /// action). True if an entry's `live` flag changed.
    pub(crate) fn closed(&mut self, id: u32) -> bool {
        self.live.remove(&id);
        self.shown.remove(&id);
        match self.entries.iter_mut().find(|e| e.entry.id == id && e.entry.live) {
            Some(e) => {
                e.entry.live = false;
                self.dirty = true;
                true
            }
            None => false,
        }
    }

    /// Whether a toast for `id` is on screen or queued.
    pub(crate) fn is_shown(&self, id: u32) -> bool {
        self.shown.contains(&id)
    }

    /// Whether a stored notification asked to stay after an action.
    pub(crate) fn is_resident(&self, id: u32) -> bool {
        self.entries.iter().any(|e| e.entry.id == id && e.resident)
    }

    /// Newest first.
    pub(crate) fn list(&self) -> Vec<HistoryEntry> {
        self.entries.iter().rev().map(|e| e.entry.clone()).collect()
    }

    /// Newest first, filtered, then `offset`/`limit` applied. `limit` is
    /// capped at [`CAP`].
    pub(crate) fn page(&self, filter: PageFilter, offset: usize, limit: usize) -> Page {
        let matching = || self.entries.iter().rev().filter(|e| filter.keeps(e));
        Page {
            total: matching().count() as u32,
            unread: self.unread(),
            mode: self.mode,
            entries: matching().skip(offset).take(limit.min(CAP)).cloned().collect(),
        }
    }

    pub(crate) fn remove(&mut self, id: u32) -> bool {
        self.take(id)
    }

    pub(crate) fn clear(&mut self) -> bool {
        let had = !self.entries.is_empty();
        self.entries.clear();
        self.dirty |= had;
        had
    }

    /// True if anything was unread.
    pub(crate) fn mark_all_read(&mut self) -> bool {
        let mut any = false;
        for e in self.entries.iter_mut().filter(|e| !e.read) {
            e.read = true;
            any = true;
        }
        self.dirty |= any;
        any
    }

    /// True if `id` was stored and unread.
    pub(crate) fn mark_read(&mut self, id: u32) -> bool {
        match self.entries.iter_mut().find(|e| e.entry.id == id && !e.read) {
            Some(e) => {
                e.read = true;
                self.dirty = true;
                true
            }
            None => false,
        }
    }

    /// True (and the notification is marked closed) only if `id` is a stored,
    /// still-live notification that really has an action called `key`.
    pub(crate) fn invoke(&mut self, id: u32, key: &str) -> bool {
        let ok = self.live.contains(&id)
            && self
                .entries
                .iter()
                .any(|e| e.entry.id == id && e.entry.live && e.entry.actions.iter().any(|(k, _)| k == key));
        if ok && !self.is_resident(id) {
            self.closed(id);
        }
        ok
    }

    fn take(&mut self, id: u32) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.entry.id != id);
        let changed = self.entries.len() != before;
        self.dirty |= changed;
        changed
    }
}

/// A persistence failure is not worth more than a line, and never the body.
fn warn_save(e: &std::io::Error) {
    eprintln!("ec-notifications: could not save history: {e}");
}

/// The store shared between the bus methods and the service thread.
#[derive(Debug, Clone, Default)]
pub(crate) struct Shared(Arc<Mutex<Store>>);

impl Shared {
    pub(crate) fn new(store: Store) -> Self {
        Self(Arc::new(Mutex::new(store)))
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Store> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub(crate) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub(crate) fn wire_list(store: &Store) -> Vec<WireEntry> {
    store.list().into_iter().map(to_wire).collect()
}

/// The bar's end of the history. Cheap to create; connects lazily and
/// reconnects on every call while there is no bus, so it can start before the
/// toasts process. With nothing on the bus every read is empty, not an error.
#[derive(Debug, Default)]
pub struct HistoryClient {
    connection: Option<Connection>,
    /// Bus address to use instead of the session bus (tests, private buses).
    address: Option<String>,
}

impl HistoryClient {
    pub fn new() -> Self {
        Self::default()
    }

    /// A client for the bus at `address` rather than the session bus.
    pub fn at(address: &str) -> Self {
        Self {
            connection: None,
            address: Some(address.to_owned()),
        }
    }

    fn conn(&mut self) -> Option<&Connection> {
        if self.connection.is_none() {
            self.connection = match &self.address {
                Some(a) => zbus::blocking::connection::Builder::address(a.as_str())
                    .and_then(zbus::blocking::connection::Builder::build)
                    .ok(),
                None => Connection::session().ok(),
            };
        }
        self.connection.as_ref()
    }

    fn call<B>(&mut self, method: &str, body: &B) -> Option<zbus::Message>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        let conn = self.conn()?;
        match conn.call_method(
            Some(HISTORY_BUS_NAME),
            HISTORY_PATH,
            Some(HISTORY_IFACE),
            method,
            body,
        ) {
            Ok(m) => Some(m),
            Err(zbus::Error::InputOutput(_)) => {
                self.connection = None;
                None
            }
            Err(_) => None,
        }
    }

    /// Newest first. Empty when the toasts process is not on the bus.
    pub fn list(&mut self) -> Vec<HistoryEntry> {
        self.call("List", &())
            .and_then(|m| m.body().deserialize::<Vec<WireEntry>>().ok())
            .map_or_else(Vec::new, |v| v.into_iter().map(from_wire).collect())
    }

    /// Delete one entry. A no-op without a service.
    pub fn remove(&mut self, id: u32) {
        let _ = self.call("Remove", &(id,));
    }

    /// Delete every entry. A no-op without a service.
    pub fn clear(&mut self) {
        let _ = self.call("Clear", &());
    }

    /// Press an action button on a still-live notification. False if it was
    /// refused (expired, dismissed, unknown key) or there is no service.
    pub fn invoke_action(&mut self, id: u32, key: &str) -> bool {
        self.call("InvokeAction", &(id, key))
            .and_then(|m| m.body().deserialize::<bool>().ok())
            .unwrap_or(false)
    }

    /// A channel that receives `()` whenever the history changed (a
    /// `Changed` signal), fed by a thread of its own. Re-`list` on each tick.
    /// The channel closes when the bus connection goes away; the receiver
    /// dropping stops the thread at the next signal.
    pub fn changes(&mut self) -> Receiver<()> {
        self.signals("Changed", |_| Some(()))
    }

    /// The unread count, sent whenever it may have changed (a new arrival,
    /// mark-read, remove, clear).
    pub fn unread_changes(&mut self) -> Receiver<u32> {
        self.signals("UnreadChanged", |m| m.body().deserialize::<u32>().ok())
    }

    /// The new mode, sent when anyone sets it.
    pub fn mode_changes(&mut self) -> Receiver<Mode> {
        self.signals("ModeChanged", |m| {
            m.body()
                .deserialize::<String>()
                .ok()
                .and_then(|s| Mode::parse(&s))
        })
    }

    fn signals<T: Send + 'static>(
        &mut self,
        member: &'static str,
        decode: fn(&zbus::Message) -> Option<T>,
    ) -> Receiver<T> {
        let (tx, rx) = mpsc::channel();
        if let Some(conn) = self.conn().cloned() {
            let rule = MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .path(HISTORY_PATH)
                .and_then(|b| b.interface(HISTORY_IFACE))
                .and_then(|b| b.member(member))
                .map(zbus::match_rule::Builder::build);
            if let Ok(rule) = rule {
                if let Ok(signals) = MessageIterator::for_match_rule(rule, &conn, Some(16)) {
                    let _ = std::thread::Builder::new()
                        .name("eclipse-notif-history".into())
                        .spawn(move || {
                            for message in signals {
                                let Ok(message) = message else { return };
                                if let Some(v) = decode(&message) {
                                    if tx.send(v).is_err() {
                                        return;
                                    }
                                }
                            }
                        });
                }
            }
        }
        rx
    }

    /// The current mode; `Normal` when there is no service.
    pub fn mode(&mut self) -> Mode {
        self.call("GetMode", &())
            .and_then(|m| m.body().deserialize::<String>().ok())
            .and_then(|s| Mode::parse(&s))
            .unwrap_or_default()
    }

    /// Switch mode. False if there is no service.
    pub fn set_mode(&mut self, mode: Mode) -> bool {
        self.call("SetMode", &(mode.as_str(),)).is_some()
    }

    /// Tell the service the notification drawer is open (`true`) or closed.
    /// While open, Normal-mode arrivals are stored without a popup or sound
    /// (Critical and the config-error notice still pop). Not persisted; the
    /// service resets it on start, when this client leaves the bus, and after
    /// 15 minutes. False if there is no service.
    pub fn set_viewing(&mut self, viewing: bool) -> bool {
        self.call("SetViewing", &(viewing,)).is_some()
    }

    /// One page, newest first. Empty without a service.
    pub fn page(&mut self, filter: PageFilter, offset: u32, limit: u32) -> Page {
        self.call("ListPage", &(filter.as_str(), offset, limit))
            .and_then(|m| m.body().deserialize::<String>().ok())
            .and_then(|s| Page::from_json(&s))
            .unwrap_or_default()
    }

    /// Entries not yet marked read; zero without a service.
    pub fn unread_count(&mut self) -> u32 {
        self.call("UnreadCount", &())
            .and_then(|m| m.body().deserialize::<u32>().ok())
            .unwrap_or(0)
    }

    /// The widget was opened: everything shown is now read.
    pub fn mark_all_read(&mut self) {
        let _ = self.call("MarkAllRead", &());
    }

    pub fn mark_read(&mut self, id: u32) {
        let _ = self.call("MarkRead", &(id,));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::rules::Rules;
    use crate::notifications::Action;

    fn note(id: u32, transient: bool, actions: &[&str]) -> Notification {
        Notification {
            id,
            app_name: "app".into(),
            summary: format!("s{id}"),
            body: "b".into(),
            actions: actions
                .iter()
                .map(|k| Action {
                    key: (*k).into(),
                    label: (*k).into(),
                })
                .collect(),
            transient,
            group: "app".into(),
            ..Notification::default()
        }
    }

    /// Arrive through the front door, as the server does.
    fn arrive(s: &mut Store, mut n: Notification) -> Verdict {
        s.admit(&mut n, -1, 0)
    }

    #[test]
    fn keeps_only_the_newest_cap_newest_first() {
        let mut s = Store::default();
        for id in 1..=(CAP as u32 + 50) {
            arrive(&mut s, note(id, false, &[]));
        }
        let l = s.list();
        assert_eq!(l.len(), CAP);
        assert_eq!(l[0].id, CAP as u32 + 50);
        assert_eq!(l[CAP - 1].id, 51);
    }

    #[test]
    fn transient_is_never_stored() {
        let mut s = Store::default();
        assert_eq!(arrive(&mut s, note(1, true, &["a"])), Verdict::Show);
        assert!(s.list().is_empty());
        assert!(!s.invoke(1, "a"));
    }

    #[test]
    fn replacement_supersedes_and_moves_to_newest() {
        let mut s = Store::default();
        arrive(&mut s, note(1, false, &[]));
        arrive(&mut s, note(2, false, &[]));
        arrive(&mut s, note(1, false, &[]));
        let ids: Vec<u32> = s.list().iter().map(|e| e.id).collect();
        assert_eq!(ids, [1, 2]);
        // A transient replacement drops the stale entry.
        arrive(&mut s, note(1, true, &[]));
        assert_eq!(s.list().len(), 1);
    }

    #[test]
    fn dismissal_keeps_the_entry_remove_and_clear_do_not() {
        let mut s = Store::default();
        for id in 1..=3 {
            arrive(&mut s, note(id, false, &[]));
        }
        assert!(s.closed(1));
        assert_eq!(s.list().len(), 3);
        assert!(!s.list().iter().find(|e| e.id == 1).unwrap().live);
        assert!(s.remove(2));
        assert!(!s.remove(2));
        assert_eq!(s.list().len(), 2);
        assert!(s.clear());
        assert!(s.list().is_empty());
        assert!(!s.clear());
    }

    #[test]
    fn invoke_is_refused_after_expiry_and_for_unknown_keys() {
        let mut s = Store::default();
        arrive(&mut s, note(1, false, &["ok"]));
        assert!(!s.invoke(1, "nope"));
        assert!(s.invoke(1, "ok"));
        // It is closed now: a second press is refused.
        assert!(!s.invoke(1, "ok"));

        arrive(&mut s, note(2, false, &["ok"]));
        s.closed(2); // expired
        assert!(!s.invoke(2, "ok"));
        assert!(!s.invoke(99, "ok"));
        // The entry itself is still there.
        assert!(s.list().iter().any(|e| e.id == 2));
    }

    #[test]
    fn resident_notifications_stay_live_after_an_action() {
        let mut s = Store::default();
        let mut n = note(1, false, &["ok"]);
        n.resident = true;
        arrive(&mut s, n);
        assert!(s.invoke(1, "ok"));
        assert!(s.invoke(1, "ok"));
    }

    #[test]
    fn normal_shows_and_stores_unread() {
        let mut s = Store::default();
        assert_eq!(arrive(&mut s, note(1, false, &[])), Verdict::Show);
        assert_eq!(s.unread(), 1);
        assert!(s.list()[0].live);
    }

    #[test]
    fn quiet_stores_unread_without_a_popup() {
        let mut s = Store::default();
        s.set_mode(Mode::Quiet);
        assert_eq!(arrive(&mut s, note(1, false, &[])), Verdict::Quiet);
        assert_eq!(s.unread(), 1);
        assert!(s.list()[0].live);
        assert!(!s.is_shown(1));
    }

    #[test]
    fn quiet_entry_with_actions_is_invokable_without_a_toast() {
        let mut s = Store::default();
        s.set_mode(Mode::Quiet);
        arrive(&mut s, note(1, false, &["ok"]));
        assert!(!s.is_shown(1));
        assert!(s.invoke(1, "ok"));
        assert!(!s.list()[0].live);
        assert!(!s.invoke(1, "ok"));
    }

    #[test]
    fn closed_by_app_is_not_live() {
        let mut s = Store::default();
        s.set_mode(Mode::Quiet);
        arrive(&mut s, note(1, false, &["ok"]));
        assert!(s.closed(1));
        assert!(!s.list()[0].live);
        assert!(!s.invoke(1, "ok"));
    }

    #[test]
    fn dnd_ignores_everything_but_still_has_an_id() {
        let mut s = Store::default();
        s.set_mode(Mode::Dnd);
        let mut n = note(7, false, &[]);
        n.urgency = Urgency::Critical;
        assert_eq!(arrive(&mut s, n), Verdict::Drop);
        assert!(s.list().is_empty());
        assert_eq!(s.unread(), 0);
    }

    #[test]
    fn mark_read_moves_entries_out_of_the_unread_view() {
        let mut s = Store::default();
        for id in 1..=3 {
            arrive(&mut s, note(id, false, &[]));
        }
        assert!(s.mark_read(2));
        assert!(!s.mark_read(2));
        assert_eq!(s.unread(), 2);
        let p = s.page(PageFilter::Unread, 0, 10);
        assert_eq!(p.entries.iter().map(|e| e.entry.id).collect::<Vec<_>>(), [3, 1]);
        assert_eq!((p.total, p.unread), (2, 2));
        assert!(s.mark_all_read());
        assert!(!s.mark_all_read());
        assert_eq!(s.unread(), 0);
        let p = s.page(PageFilter::Read, 0, 10);
        assert_eq!(p.total, 3);
        assert!(s.page(PageFilter::Unread, 0, 10).entries.is_empty());
    }

    #[test]
    fn paging_is_newest_first_and_bounded() {
        let mut s = Store::default();
        for id in 1..=10 {
            arrive(&mut s, note(id, false, &[]));
        }
        let p = s.page(PageFilter::All, 3, 4);
        assert_eq!(p.total, 10);
        assert_eq!(
            p.entries.iter().map(|e| e.entry.id).collect::<Vec<_>>(),
            [7, 6, 5, 4]
        );
        assert!(s.page(PageFilter::All, 50, 4).entries.is_empty());
        assert!(s.page(PageFilter::All, 0, 100_000).entries.len() <= CAP);
    }

    #[test]
    fn rules_mute_quieten_and_override() {
        let rules = Rules::from_rules(Rules::parse(
            r#"{"rules":[
                {"app":"Muted","mute":true},
                {"desktop-entry":"chat","quiet":true},
                {"app":"Mail","urgency":"critical","timeout-ms":1500}
            ]}"#,
        ));
        let mut s = Store::default().with_rules(rules);

        let mut n = note(1, false, &[]);
        n.app_name = "Muted".into();
        assert_eq!(arrive(&mut s, n), Verdict::Drop);

        let mut n = note(2, false, &[]);
        n.desktop_entry = "chat".into();
        assert_eq!(arrive(&mut s, n), Verdict::Quiet);

        let mut n = note(3, false, &[]);
        n.app_name = "mail".into();
        let v = s.admit(&mut n, 10_000, 0);
        assert_eq!(v, Verdict::Show);
        assert_eq!(n.urgency, Urgency::Critical);
        assert_eq!(n.expires_in, Some(std::time::Duration::from_millis(1500)));
        assert_eq!(s.list().len(), 2);
    }

    #[test]
    fn dnd_beats_a_rule_that_would_show() {
        let rules = Rules::from_rules(Rules::parse(r#"{"rules":[{"app":"app","urgency":"critical"}]}"#));
        let mut s = Store::default().with_rules(rules);
        s.set_mode(Mode::Dnd);
        assert_eq!(arrive(&mut s, note(1, false, &[])), Verdict::Drop);
    }

    #[test]
    fn persists_atomically_and_restores_read_state_and_mode() {
        let dir = std::env::temp_dir().join(format!("ec-notif-test-{}", std::process::id()));
        let path = dir.join("eclipse").join("notifications.json");
        let _ = std::fs::remove_dir_all(&dir);

        let mut s = Store::default().at(path.clone());
        arrive(&mut s, note(1, false, &["ok"]));
        arrive(&mut s, note(2, false, &[]));
        s.mark_read(1);
        s.set_mode(Mode::Quiet);
        s.save();
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .collect();
        assert_eq!(leftovers.len(), 1, "no temp file left behind: {leftovers:?}");

        let r = Store::load_from(&path);
        assert_eq!(r.mode(), Mode::Quiet);
        assert_eq!(r.max_id(), 2);
        assert_eq!(r.unread(), 1);
        // Restored entries are never live, so their buttons are inert.
        assert!(r.list().iter().all(|e| !e.live));
        assert_eq!(r.list()[1].actions, [("ok".to_owned(), "ok".to_owned())]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_state_file_is_an_empty_store() {
        let dir = std::env::temp_dir().join(format!("ec-notif-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("n.json");
        std::fs::write(&path, "{ not json").unwrap();
        let s = Store::load_from(&path);
        assert!(s.list().is_empty());
        assert_eq!(s.mode(), Mode::Normal);
        std::fs::write(
            &path,
            r#"{"mode":"dnd","entries":[{"id":5},{"nope":1},{"id":5}]}"#,
        )
        .unwrap();
        let s = Store::load_from(&path);
        assert_eq!(s.list().len(), 1);
        assert_eq!(s.mode(), Mode::Dnd);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn page_json_round_trips() {
        let mut s = Store::default();
        let mut n = note(1, false, &["ok"]);
        n.progress = Some(40);
        n.category = "email.arrived".into();
        arrive(&mut s, n);
        let p = s.page(PageFilter::All, 0, 10);
        assert_eq!(Page::from_json(&p.to_json()), Some(p));
    }

    #[test]
    fn an_empty_bus_reads_as_empty() {
        // No bus to connect to: no entries, no error, writes are no-ops.
        let mut c = HistoryClient::at("unix:path=/nonexistent/ec-bus");
        assert!(c.list().is_empty());
        c.remove(1);
        c.clear();
        assert!(!c.invoke_action(1, "ok"));
        assert!(c.changes().try_recv().is_err());
        assert_eq!(c.mode(), Mode::Normal);
        assert!(!c.set_mode(Mode::Dnd));
        assert!(!c.set_viewing(true));
        assert_eq!(c.page(PageFilter::All, 0, 10), Page::default());
        assert_eq!(c.unread_count(), 0);
        c.mark_all_read();
        c.mark_read(1);
    }

    #[test]
    fn viewing_verdict_matrix() {
        use Verdict::*;
        for (mode, urg, viewing, want) in [
            (Mode::Normal, Urgency::Normal, false, Show),
            (Mode::Normal, Urgency::Normal, true, Quiet),
            (Mode::Normal, Urgency::Low, true, Quiet),
            (Mode::Normal, Urgency::Critical, true, Show),
            (Mode::Normal, Urgency::Critical, false, Show),
            (Mode::Quiet, Urgency::Normal, true, Quiet),
            (Mode::Quiet, Urgency::Critical, true, Quiet),
            (Mode::Quiet, Urgency::Normal, false, Quiet),
            (Mode::Dnd, Urgency::Normal, true, Drop),
            (Mode::Dnd, Urgency::Critical, true, Drop),
            (Mode::Dnd, Urgency::Normal, false, Drop),
        ] {
            let mut s = Store::default();
            s.set_mode(mode);
            s.set_viewing(viewing, 0);
            let mut n = note(1, false, &[]);
            n.urgency = urg;
            assert_eq!(arrive(&mut s, n), want, "{mode:?} {urg:?} viewing={viewing}");
        }
    }

    #[test]
    fn viewing_quiet_arrival_is_stored_unread_and_live() {
        let mut s = Store::default();
        s.set_viewing(true, 0);
        arrive(&mut s, note(1, false, &["ok"]));
        assert_eq!(s.unread(), 1);
        assert!(s.list()[0].live);
        assert!(s.invoke(1, "ok"));
    }

    #[test]
    fn viewing_spares_the_config_notice_and_lapses() {
        let mut s = Store::default();
        s.set_viewing(true, 0);
        assert_eq!(
            arrive(&mut s, note(super::super::compositor::CONFIG_ID, false, &[])),
            Verdict::Show
        );
        assert!(s.viewing(VIEWING_TIMEOUT - 1));
        assert!(!s.viewing(VIEWING_TIMEOUT));
        s.set_viewing(false, 0);
        assert!(!s.viewing(0));
    }

    #[test]
    fn viewing_ends_when_the_watched_client_leaves() {
        let mut s = Store::default();
        assert!(s.claim_viewing_watch(":1.5"));
        assert!(!s.claim_viewing_watch(":1.5"));
        s.set_viewing(true, 0);
        s.viewing_owner_gone(":1.9");
        assert!(s.viewing(0));
        s.viewing_owner_gone(":1.5");
        assert!(!s.viewing(0));
        assert!(s.claim_viewing_watch(":1.5"));
    }
}
