// SPDX-License-Identifier: AGPL-3.0-only
//! Notification history: the newest [`CAP`] notifications, kept in memory by
//! the toasts process and readable from the taskbar through a private
//! session-bus interface, `org.eclipse.NotificationHistory`.
//!
//! * **Memory only.** Entries die with the process; nothing touches disk.
//! * **Transient-hinted notifications are never stored.** No hint marks a
//!   notification private or sensitive in this server today, so there is
//!   nothing else to exclude; do not invent one here without an ADR.
//! * **Dismissing a toast keeps its entry.** Only [`Store::remove`] and
//!   [`Store::clear`] (the bar's buttons) delete one.
//! * **Bodies are never logged**, same as the rest of this module.
//! * `InvokeAction` works only while the source notification is still live
//!   (on screen, not expired, dismissed or withdrawn). After that the entry
//!   is read-only.
//!
//! * **Only D-Bus notifications.** Compositor-originated notices (the
//!   persistent config-error notice, `compositor.rs`) go straight to the toast
//!   stack and are not recorded here.
//!
//! The bus interface is served on the toasts connection under its own name,
//! `org.eclipse.NotificationHistory`, never `org.freedesktop.Notifications`:
//! a sandbox granted the latter to post notices gets no access to it
//! (`xdg-dbus-proxy` filters by name). [`HistoryClient`] is the
//! blocking-zbus client the bar uses, with no async runtime.
//!
//! Wire, at `/org/eclipse/NotificationHistory` on the name
//! `org.eclipse.NotificationHistory`: `List() -> a(usssssyta(ss)b)` (see
//! [`WireEntry`]), `Remove(u)`, `Clear()`, `InvokeAction(u, s) -> b`, signal
//! `Changed()`. Any process that can reach that name on the session bus
//! can use it, like a click by the same human; sandboxes are not given it.

use std::collections::{HashSet, VecDeque};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use zbus::blocking::{Connection, MessageIterator};
use zbus::MatchRule;

use super::{Notification, Urgency};

/// How many entries are kept; the oldest falls off first.
pub const CAP: usize = 100;

/// The history's own bus name. Not the notification name: sandboxes are
/// granted that one to post, and must not be able to read or drive history.
pub(crate) const HISTORY_BUS_NAME: &str = "org.eclipse.NotificationHistory";
pub(crate) const HISTORY_PATH: &str = "/org/eclipse/NotificationHistory";
pub(crate) const HISTORY_IFACE: &str = "org.eclipse.NotificationHistory";

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

/// The bounded store. Oldest at the front.
#[derive(Debug, Default)]
pub(crate) struct Store {
    entries: VecDeque<HistoryEntry>,
    /// Ids currently on screen or queued, stored or not.
    live: HashSet<u32>,
}

impl Store {
    /// A notification arrived (or replaced one). True if the history changed.
    pub(crate) fn posted(&mut self, n: &Notification, now: u64) -> bool {
        self.live.insert(n.id);
        // A replacement supersedes the old entry, even when the new one is
        // transient: the old actions must not answer for the new content.
        let had = self.take(n.id);
        if n.transient {
            return had;
        }
        if self.entries.len() == CAP {
            self.entries.pop_front();
        }
        self.entries.push_back(HistoryEntry {
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
            live: true,
        });
        true
    }

    /// The toast left the screen (any reason). True if an entry's `live`
    /// flag changed.
    pub(crate) fn closed(&mut self, id: u32) -> bool {
        self.live.remove(&id);
        match self.entries.iter_mut().find(|e| e.id == id && e.live) {
            Some(e) => {
                e.live = false;
                true
            }
            None => false,
        }
    }

    /// Newest first.
    pub(crate) fn list(&self) -> Vec<HistoryEntry> {
        self.entries.iter().rev().cloned().collect()
    }

    pub(crate) fn remove(&mut self, id: u32) -> bool {
        self.take(id)
    }

    pub(crate) fn clear(&mut self) -> bool {
        let had = !self.entries.is_empty();
        self.entries.clear();
        had
    }

    /// True (and the notification is marked closed) only if `id` is a stored,
    /// still-live notification that really has an action called `key`.
    pub(crate) fn invoke(&mut self, id: u32, key: &str) -> bool {
        let ok = self.live.contains(&id)
            && self
                .entries
                .iter()
                .any(|e| e.id == id && e.live && e.actions.iter().any(|(k, _)| k == key));
        if ok {
            self.closed(id);
        }
        ok
    }

    fn take(&mut self, id: u32) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.id != id);
        self.entries.len() != before
    }
}

/// The store shared between the bus methods and the service thread.
#[derive(Debug, Clone, Default)]
pub(crate) struct Shared(Arc<Mutex<Store>>);

impl Shared {
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
        let (tx, rx) = mpsc::channel();
        if let Some(conn) = self.conn().cloned() {
            let rule = MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .path(HISTORY_PATH)
                .and_then(|b| b.interface(HISTORY_IFACE))
                .and_then(|b| b.member("Changed"))
                .map(zbus::match_rule::Builder::build);
            if let Ok(rule) = rule {
                if let Ok(signals) = MessageIterator::for_match_rule(rule, &conn, Some(16)) {
                    let _ = std::thread::Builder::new()
                        .name("eclipse-notif-history".into())
                        .spawn(move || {
                            for message in signals {
                                if message.is_err() || tx.send(()).is_err() {
                                    return;
                                }
                            }
                        });
                }
            }
        }
        rx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::Action;

    fn note(id: u32, transient: bool, actions: &[&str]) -> Notification {
        Notification {
            id,
            app_name: "app".into(),
            summary: format!("s{id}"),
            body: "b".into(),
            icon: String::new(),
            urgency: Urgency::Normal,
            actions: actions
                .iter()
                .map(|k| Action {
                    key: (*k).into(),
                    label: (*k).into(),
                })
                .collect(),
            expires_in: None,
            transient,
        }
    }

    #[test]
    fn keeps_only_the_newest_hundred_newest_first() {
        let mut s = Store::default();
        for id in 1..=150 {
            s.posted(&note(id, false, &[]), 0);
        }
        let l = s.list();
        assert_eq!(l.len(), CAP);
        assert_eq!(l[0].id, 150);
        assert_eq!(l[CAP - 1].id, 51);
    }

    #[test]
    fn transient_is_never_stored() {
        let mut s = Store::default();
        assert!(!s.posted(&note(1, true, &["a"]), 0));
        assert!(s.list().is_empty());
        assert!(!s.invoke(1, "a"));
    }

    #[test]
    fn replacement_supersedes_and_moves_to_newest() {
        let mut s = Store::default();
        s.posted(&note(1, false, &[]), 0);
        s.posted(&note(2, false, &[]), 0);
        s.posted(&note(1, false, &[]), 0);
        let ids: Vec<u32> = s.list().iter().map(|e| e.id).collect();
        assert_eq!(ids, [1, 2]);
        // A transient replacement drops the stale entry.
        s.posted(&note(1, true, &[]), 0);
        assert_eq!(s.list().len(), 1);
    }

    #[test]
    fn dismissal_keeps_the_entry_remove_and_clear_do_not() {
        let mut s = Store::default();
        s.posted(&note(1, false, &[]), 0);
        s.posted(&note(2, false, &[]), 0);
        s.posted(&note(3, false, &[]), 0);
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
        s.posted(&note(1, false, &["ok"]), 0);
        assert!(!s.invoke(1, "nope"));
        assert!(s.invoke(1, "ok"));
        // It is closed now: a second press is refused.
        assert!(!s.invoke(1, "ok"));

        s.posted(&note(2, false, &["ok"]), 0);
        s.closed(2); // expired
        assert!(!s.invoke(2, "ok"));
        assert!(!s.invoke(99, "ok"));
        // The entry itself is still there.
        assert!(s.list().iter().any(|e| e.id == 2));
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
    }
}
