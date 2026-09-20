// SPDX-License-Identifier: AGPL-3.0-only
//! `org.freedesktop.ScreenSaver`: the idle-inhibit door for clients that have
//! no Wayland surface to hang a `zwp_idle_inhibitor_v1` on (ADR 0051).
//!
//! Firefox, Chromium and `xdg-desktop-portal-gtk` all ask the session bus to
//! keep the screen awake while a video plays. With nobody owning this name the
//! request fails silently and the display powers off mid-film. This service
//! owns the name, keeps one cookie per request, and tells the compositor over
//! the control socket whether *any* cookie is outstanding.
//!
//! It is an ordinary userland service, outside the TCB (ADR 0038). All it can
//! do to the compositor is keep the screen on, which any Wayland client can
//! already do, and the hold lapses with the socket connection.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use zbus::blocking::{connection, fdo::DBusProxy};
use zbus::fdo::{self, RequestNameFlags, RequestNameReply};
use zbus::interface;
use zbus::message::Header;

const BUS_NAME: &str = "org.freedesktop.ScreenSaver";
/// Chromium and Firefox use the first path; older KDE-flavoured clients the
/// second. Same object behind both.
const OBJECT_PATHS: [&str; 2] = ["/org/freedesktop/ScreenSaver", "/ScreenSaver"];

/// No honest client holds more than a handful. The cap is here so a
/// misbehaving one cannot grow this table without bound.
const MAX_HOLDS: usize = 4096;

/// How often a held inhibit is re-stated to the compositor, and how soon a
/// failed connect is retried. The call is idempotent, so re-asserting is what
/// carries the hold across a compositor restart.
const REASSERT: Duration = Duration::from_secs(10);
const RETRY: Duration = Duration::from_secs(3);

/// The outstanding inhibit cookies and who holds each. Plain data, so the
/// rules are testable without a bus.
#[derive(Debug, Default)]
pub struct Holds {
    by_cookie: HashMap<u32, String>,
    last: u32,
}

impl Holds {
    /// Take a hold for `owner` (a unique bus name). `None` when the table is full.
    pub fn add(&mut self, owner: &str) -> Option<u32> {
        if self.by_cookie.len() >= MAX_HOLDS {
            return None;
        }
        // Zero reads as "no cookie" to a lot of client code, so it is never
        // handed out, and a live cookie is never handed out twice.
        loop {
            self.last = self.last.wrapping_add(1);
            if self.last != 0 && !self.by_cookie.contains_key(&self.last) {
                break;
            }
        }
        self.by_cookie.insert(self.last, owner.to_owned());
        Some(self.last)
    }

    /// Release a cookie, but only for the connection that took it: one client
    /// must not be able to end another's inhibit.
    pub fn remove(&mut self, cookie: u32, owner: &str) -> bool {
        if self.by_cookie.get(&cookie).is_some_and(|o| o == owner) {
            self.by_cookie.remove(&cookie);
            true
        } else {
            false
        }
    }

    /// A connection left the bus; whatever it held goes with it.
    pub fn drop_owner(&mut self, owner: &str) -> usize {
        let before = self.by_cookie.len();
        self.by_cookie.retain(|_, o| o != owner);
        before - self.by_cookie.len()
    }

    pub fn held(&self) -> bool {
        !self.by_cookie.is_empty()
    }
}

/// State shared by the interface objects and the bus-name watcher.
struct Shared {
    holds: Mutex<Holds>,
    /// The level last sent, so a change of cookie count that leaves the level
    /// alone says nothing. Guarded by the same lock as `holds` (see `apply`).
    levels: Sender<bool>,
}

impl Shared {
    /// Run `change` on the table and, if it flipped whether anything is held,
    /// announce the new level. Sending under the lock keeps levels in order.
    fn apply<R>(&self, change: impl FnOnce(&mut Holds) -> R) -> R {
        let mut holds = self.holds.lock().unwrap_or_else(|p| p.into_inner());
        let was = holds.held();
        let out = change(&mut holds);
        let now = holds.held();
        if now != was {
            // A gone receiver means the bridge is gone; there is nobody to tell.
            let _ = self.levels.send(now);
        }
        out
    }
}

struct Server {
    shared: Arc<Shared>,
}

#[interface(name = "org.freedesktop.ScreenSaver")]
impl Server {
    /// Keep the screen on until `un_inhibit`, or until the caller leaves the bus.
    fn inhibit(
        &self,
        _application_name: String,
        _reason_for_inhibit: String,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<u32> {
        let sender = header
            .sender()
            .ok_or_else(|| fdo::Error::Failed("no sender on the call".into()))?
            .to_string();
        self.shared
            .apply(|h| h.add(&sender))
            .ok_or_else(|| fdo::Error::LimitsExceeded("too many inhibits".into()))
    }

    fn un_inhibit(&self, cookie: u32, #[zbus(header)] header: Header<'_>) -> fdo::Result<()> {
        let sender = header
            .sender()
            .ok_or_else(|| fdo::Error::Failed("no sender on the call".into()))?
            .to_string();
        if self.shared.apply(|h| h.remove(cookie, &sender)) {
            Ok(())
        } else {
            Err(fdo::Error::InvalidArgs("no such inhibit".into()))
        }
    }

    /// Nothing here locks the screen, so it is never "active".
    fn get_active(&self) -> bool {
        false
    }
}

/// The running service. Dropping it leaves the bus and releases every hold.
pub struct ScreenSaver {
    _connection: zbus::blocking::Connection,
}

/// Own `org.freedesktop.ScreenSaver` and report every change of "is anything
/// holding the screen on" down `levels`.
///
/// Fails if the name is taken -- we never steal it from a running daemon.
pub fn spawn_with(levels: Sender<bool>) -> zbus::Result<ScreenSaver> {
    let shared = Arc::new(Shared {
        holds: Mutex::new(Holds::default()),
        levels,
    });

    let mut builder = connection::Builder::session()?;
    for path in OBJECT_PATHS {
        builder = builder.serve_at(
            path,
            Server {
                shared: Arc::clone(&shared),
            },
        )?;
    }
    let connection = builder.build()?;

    // Subscribe before the name is requested, so no client can arrive, take a
    // cookie and vanish in a gap the watcher would miss.
    let bus = DBusProxy::new(&connection)?;
    let vanished = bus.receive_name_owner_changed()?;
    thread::Builder::new()
        .name("eclipse-screensaver-names".into())
        .spawn(move || {
            for signal in vanished {
                let Ok(args) = signal.args() else { continue };
                // A unique name that lost its owner is a connection closing.
                let Some(old) = args.old_owner().as_ref() else {
                    continue;
                };
                if args.new_owner().is_none() && args.name().as_str() == old.as_str() {
                    shared.apply(|h| h.drop_owner(old.as_str()));
                }
            }
        })
        .map_err(|e| zbus::Error::InputOutput(Arc::new(e)))?;

    match connection.request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into())? {
        RequestNameReply::PrimaryOwner => {}
        _ => return Err(zbus::Error::NameTaken),
    }
    Ok(ScreenSaver {
        _connection: connection,
    })
}

/// [`spawn_with`], wired to the compositor's control socket.
pub fn spawn() -> zbus::Result<ScreenSaver> {
    spawn_with(spawn_bridge())
}

/// A thread that owns the control-socket connection and keeps the compositor's
/// idea of the level equal to the last one it was handed.
fn spawn_bridge() -> Sender<bool> {
    let (tx, rx) = mpsc::channel();
    let _ = thread::Builder::new()
        .name("eclipse-screensaver-ipc".into())
        .spawn(move || bridge(&rx));
    tx
}

fn bridge(levels: &Receiver<bool>) {
    let mut wanted = false;
    let mut client: Option<eclipse_ipc::Client> = None;
    let mut wait = REASSERT;
    loop {
        match levels.recv_timeout(wait) {
            Ok(level) => wanted = level,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        // Only the newest level matters.
        while let Ok(level) = levels.try_recv() {
            wanted = level;
        }

        if client.is_none() && wanted {
            client = eclipse_ipc::Client::connect().ok();
        }
        wait = REASSERT;
        if let Some(c) = client.as_mut() {
            let sent = c.call("set_idle_inhibit", serde_json::json!({ "inhibit": wanted }));
            if sent.is_err() {
                // The compositor went away; its side of the hold went with it.
                client = None;
            }
        }
        if wanted && client.is_none() {
            wait = RETRY;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookies_are_nonzero_and_distinct() {
        let mut h = Holds::default();
        let a = h.add(":1.5").unwrap();
        let b = h.add(":1.5").unwrap();
        assert!(a != 0 && b != 0 && a != b);
    }

    #[test]
    fn a_cookie_wraps_past_zero_and_past_live_ones() {
        let mut h = Holds {
            last: u32::MAX,
            ..Holds::default()
        };
        assert_eq!(h.add(":1.1"), Some(1), "zero is skipped on wrap");
        h.last = 0;
        assert_eq!(h.add(":1.2"), Some(2), "a live cookie is skipped");
    }

    #[test]
    fn only_the_holder_can_release() {
        let mut h = Holds::default();
        let c = h.add(":1.5").unwrap();
        assert!(!h.remove(c, ":1.6"), "another client cannot release it");
        assert!(h.held());
        assert!(h.remove(c, ":1.5"));
        assert!(!h.held());
        assert!(!h.remove(c, ":1.5"), "a cookie releases once");
    }

    #[test]
    fn a_client_leaving_the_bus_releases_all_its_holds_and_only_those() {
        let mut h = Holds::default();
        h.add(":1.5");
        h.add(":1.5");
        h.add(":1.6");
        assert_eq!(h.drop_owner(":1.5"), 2);
        assert!(h.held(), "the other client's hold stays");
        assert_eq!(h.drop_owner(":1.6"), 1);
        assert!(!h.held());
    }

    #[test]
    fn the_table_is_bounded() {
        let mut h = Holds::default();
        for _ in 0..MAX_HOLDS {
            assert!(h.add(":1.5").is_some());
        }
        assert_eq!(h.add(":1.5"), None);
    }

    #[test]
    fn the_level_is_announced_only_when_it_flips() {
        let (tx, rx) = mpsc::channel();
        let shared = Shared {
            holds: Mutex::new(Holds::default()),
            levels: tx,
        };
        let a = shared.apply(|h| h.add(":1.5")).unwrap();
        let b = shared.apply(|h| h.add(":1.5")).unwrap();
        shared.apply(|h| h.remove(a, ":1.5"));
        shared.apply(|h| h.remove(b, ":1.5"));
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), [true, false]);
    }
}
