// SPDX-License-Identifier: AGPL-3.0-only
//! Machine status for the bar: network, bluetooth, battery (ADR 0038).
//!
//! All three come off the *system* bus from daemons that already run on any
//! ordinary Linux desktop — NetworkManager, BlueZ and UPower. We speak to them
//! over zbus rather than pulling in `bluer` or `network-manager` wrappers; the
//! handful of properties a bar needs does not justify another dependency tree.
//!
//! Readings are passive: the watchers below only ever read. Changing anything
//! — joining a network, powering a radio, pairing — goes through [`Actions`],
//! a separate handle that only the human-driven UI holds and only calls from
//! a click or a key, never from a draw path. Every action runs on its own
//! short-lived thread and reports back over [`Events`], so a slow daemon never
//! costs the bar a frame. A secret (a wifi passphrase, a PIN) arrives as a
//! [`Secret`], is never logged or put in `Debug`, and is overwritten when it
//! is dropped (ADR 0053).
//!
//! Like the notification server, each watcher owns a thread and reports over a
//! channel, because iced runs no async runtime.

mod actions;
mod agent;
mod battery;
mod bluetooth;
mod network;
mod secret;
mod wifi;

use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use zbus::blocking::{Connection, MessageIterator};
use zbus::MatchRule;

pub use actions::{actions, Actions, BtPrompt, ConnectError, Event, Events, PairingAgent};
pub use battery::{Battery, Charge};
pub use bluetooth::{Bluetooth, BtDevice, BtKind};
pub use network::Network;
pub use secret::Secret;
pub use wifi::{Security, WifiDetail, WifiNetwork};

/// One piece of the picture changed. The GUI folds these into its own state;
/// nothing here keeps a snapshot, so there is one copy of the truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    Network(Network),
    Bluetooth(Bluetooth),
    /// `None` on a machine with no battery — a desktop, or a laptop whose pack
    /// has been removed. The bar draws nothing rather than drawing 0%.
    Battery(Option<Battery>),
}

/// The GUI's end of the three watchers.
pub struct SystemStatus {
    updates: Receiver<Update>,
}

impl SystemStatus {
    /// The next change, if one is waiting. Never blocks.
    pub fn try_recv(&self) -> Option<Update> {
        self.updates.try_recv().ok()
    }
}

/// Start watching. Each source gets its own thread *and its own connection*
/// and reports independently, so a daemon that is missing, slow or chatty
/// costs us that one reading and nothing else — a machine without BlueZ
/// still gets network and battery, and NetworkManager's signal storm during a
/// scan cannot hold up the battery's replies.
///
/// Fails only if the system bus itself is unreachable.
pub fn spawn() -> zbus::Result<SystemStatus> {
    // Fail early and honestly if there is no system bus at all; after this the
    // watchers reconnect on their own.
    drop(Connection::system()?);
    let (tx, updates) = mpsc::channel();

    network::watch(tx.clone());
    bluetooth::watch(tx.clone());
    battery::watch(tx);

    Ok(SystemStatus { updates })
}

/// How often a watcher looks, whatever the daemon says.
#[derive(Debug, Clone, Copy)]
struct Cadence {
    /// Re-read at least this often even with no signal at all. The backstop:
    /// a signal we never see (a daemon that only emits on some paths, a bus
    /// that dropped one) costs at most this much staleness, not forever.
    floor: Duration,
    /// After a signal, wait this long and swallow the rest of the burst
    /// before reading. NetworkManager emits dozens of signals per roam; one
    /// read at the end is the same picture.
    debounce: Duration,
    /// How long to wait before reconnecting after the bus hangs up. Long
    /// enough not to spin on a bus that is down for good, short enough that a
    /// restart is picked up while the human is still looking at the bar.
    retry: Duration,
}

const CADENCE: Cadence = Cadence {
    floor: Duration::from_secs(30),
    debounce: Duration::from_millis(100),
    retry: Duration::from_secs(5),
};

/// Start a system-bus watcher thread with the production cadence.
fn watch_system<T, Read, Wrap>(
    thread_name: &'static str,
    rule: zbus::Result<MatchRule<'static>>,
    read: Read,
    wrap: Wrap,
    updates: mpsc::Sender<Update>,
) where
    T: Clone + PartialEq + Send + 'static,
    Read: Fn(&Connection) -> T + Send + 'static,
    Wrap: Fn(T) -> Update + Send + 'static,
{
    // A rule we cannot build is a programming error in a constant; without it
    // the floor still keeps the reading fresh, so degrade rather than panic.
    let rule = rule.ok();
    watch_service(
        thread_name,
        Connection::system,
        rule,
        CADENCE,
        read,
        wrap,
        updates,
    );
}

/// The shape every watcher has: read once, report, then re-read whenever the
/// daemon says anything matching `rule` — and at least every `floor`.
///
/// The signals are drained by a second thread that does nothing else. That
/// split is the fix for the bar going stale: zbus parks the whole socket
/// reader when one match queue is full, and a thread that read properties in
/// between signals on the same connection (the old shape, with a queue of 8)
/// could wait forever on a reply stuck behind the signals it had not yet
/// taken. The pump never makes a call, so it always drains; the reader never
/// waits on a signal queue, so its replies always arrive.
fn watch_service<T, Connect, Read, Wrap>(
    thread_name: &'static str,
    connect: Connect,
    rule: Option<MatchRule<'static>>,
    cadence: Cadence,
    read: Read,
    wrap: Wrap,
    updates: mpsc::Sender<Update>,
) where
    T: Clone + PartialEq + Send + 'static,
    Connect: Fn() -> zbus::Result<Connection> + Send + 'static,
    Read: Fn(&Connection) -> T + Send + 'static,
    Wrap: Fn(T) -> Update + Send + 'static,
{
    let spawned = std::thread::Builder::new()
        .name(thread_name.to_owned())
        .spawn(move || {
            let mut last: Option<T> = None;
            let mut report = |value: T| {
                if last.as_ref() == Some(&value) {
                    return true; // Nothing the human would see.
                }
                last = Some(value.clone());
                updates.send(wrap(value)).is_ok()
            };
            loop {
                let Ok(connection) = connect() else {
                    std::thread::sleep(cadence.retry);
                    continue;
                };
                let signals = rule.clone().map(|rule| pump(thread_name, rule, &connection));

                if !report(read(&connection)) {
                    return; // The GUI is gone.
                }
                loop {
                    let heard = match &signals {
                        Some(Ok(signals)) => signals.recv_timeout(cadence.floor),
                        // No subscription: the floor alone keeps us honest.
                        _ => {
                            std::thread::sleep(cadence.floor);
                            Err(RecvTimeoutError::Timeout)
                        }
                    };
                    match heard {
                        Ok(()) => {
                            std::thread::sleep(cadence.debounce);
                            if let Some(Ok(signals)) = &signals {
                                while signals.try_recv().is_ok() {}
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        // The pump ended: the bus hung up on us.
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                    if !report(read(&connection)) {
                        return;
                    }
                }
                std::thread::sleep(cadence.retry);
            }
        });
    // A thread we cannot start means that reading is simply unavailable; the
    // other two still work, and the bar draws one fewer cell.
    drop(spawned);
}

/// Subscribe to `rule` and forward a tick per signal on a thread that does
/// nothing else. The receiver disconnects when the connection closes.
fn pump(
    thread_name: &'static str,
    rule: MatchRule<'static>,
    connection: &Connection,
) -> zbus::Result<Receiver<()>> {
    let signals = MessageIterator::for_match_rule(rule, connection, Some(64))?;
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name(format!("{thread_name}-signals"))
        .spawn(move || {
            for message in signals {
                if message.is_err() || tx.send(()).is_err() {
                    return;
                }
            }
        })
        .map_err(|error| zbus::Error::Failure(error.to_string()))?;
    Ok(rx)
}

/// A proxy that asks the daemon every time. zbus's default caches lazily,
/// which costs a match rule, a background task and a `GetAll` per proxy — and
/// every read here builds a fresh proxy, so the cache would never be reused.
pub(crate) fn uncached<'a>(
    connection: &Connection,
    destination: &'a str,
    path: &'a str,
    interface: &'a str,
) -> Option<zbus::blocking::Proxy<'a>> {
    zbus::blocking::proxy::Builder::new(connection)
        .destination(destination)
        .ok()?
        .path(path)
        .ok()?
        .interface(interface)
        .ok()?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .ok()
}

/// A signal rule on one sender, optionally narrowed to a path and interface.
fn signal_rule(
    sender: &'static str,
    path: Option<&'static str>,
    interface: Option<&'static str>,
) -> zbus::Result<MatchRule<'static>> {
    let mut rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(sender)?;
    if let Some(path) = path {
        rule = rule.path(path)?;
    }
    if let Some(interface) = interface {
        rule = rule.interface(interface)?;
    }
    Ok(rule.build())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A machine with no battery reports `Battery(None)`, and that has to be a
    /// value the GUI can hold and compare — the bar decides to draw nothing
    /// from this, so it must not be confused with "no update yet".
    #[test]
    fn an_absent_battery_is_a_real_update() {
        assert_ne!(
            Update::Battery(None),
            Update::Battery(Some(Battery {
                percent: 0,
                state: Charge::Unknown,
                remaining: None,
            }))
        );
    }

    /// Offline is the default because every read failure funnels into it. A bar
    /// that cannot reach NetworkManager must not claim the machine is online.
    #[test]
    fn the_default_network_is_offline() {
        assert_eq!(Network::default(), Network::Offline);
    }
}
