// SPDX-License-Identifier: AGPL-3.0-only
//! Machine status for the bar: network, bluetooth, battery (ADR 0038).
//!
//! All three come off the *system* bus from daemons that already run on any
//! ordinary Linux desktop — NetworkManager, BlueZ and UPower. We speak to them
//! over zbus rather than pulling in `bluer` or `network-manager` wrappers; the
//! handful of properties a bar needs does not justify another dependency tree.
//!
//! Everything here is **read-only**. This module never enables an adapter,
//! joins a network or changes a power profile. Those are actions with real
//! consequences and they belong behind a capability check, not behind a widget
//! that a compromised bar process could click on the human's behalf.
//!
//! Like the notification server, each watcher owns a thread and reports over a
//! channel, because iced runs no async runtime.

mod battery;
mod bluetooth;
mod network;

use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

pub use battery::{Battery, Charge};
pub use bluetooth::Bluetooth;
pub use network::Network;

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

/// Start watching. Each source gets its own thread and reports independently,
/// so a daemon that is missing or slow costs us that one reading and nothing
/// else — a machine without BlueZ still gets network and battery.
///
/// Fails only if the system bus itself is unreachable.
pub fn spawn() -> zbus::Result<SystemStatus> {
    let connection = zbus::blocking::Connection::system()?;
    let (tx, updates) = mpsc::channel();

    network::watch(&connection, tx.clone());
    bluetooth::watch(&connection, tx.clone());
    battery::watch(&connection, tx);

    Ok(SystemStatus { updates })
}

/// How long a watcher waits before trying again after the daemon hangs up.
/// Long enough not to spin on a service that is down for good, short enough
/// that a restarted NetworkManager is picked up while the human is still
/// looking at the bar.
const RETRY: Duration = Duration::from_secs(5);

/// The shape every watcher has: read once, report, then re-read whenever the
/// daemon says anything at all.
///
/// Matching on the sender rather than on a specific property is deliberate.
/// The interesting changes are scattered — NetworkManager moves the primary
/// connection on one object and the signal strength on another, BlueZ adds and
/// removes whole device objects — and a bar that missed those would quietly go
/// stale. A re-read is two or three round trips on a socket we already hold.
fn watch_service<T, Read, Wrap>(
    thread_name: &'static str,
    connection: &zbus::blocking::Connection,
    sender: &'static str,
    read: Read,
    wrap: Wrap,
    updates: mpsc::Sender<Update>,
) where
    T: Clone + PartialEq + Send + 'static,
    Read: Fn(&zbus::blocking::Connection) -> T + Send + 'static,
    Wrap: Fn(T) -> Update + Send + 'static,
{
    let connection = connection.clone();
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
                let signals = zbus::MatchRule::builder()
                    .msg_type(zbus::message::Type::Signal)
                    .sender(sender)
                    .map(zbus::match_rule::Builder::build)
                    .and_then(|rule| {
                        zbus::blocking::MessageIterator::for_match_rule(rule, &connection, Some(8))
                    });
                let Ok(mut signals) = signals else {
                    std::thread::sleep(RETRY);
                    continue;
                };

                if !report(read(&connection)) {
                    return; // The GUI is gone.
                }
                while signals.next().is_some() {
                    if !report(read(&connection)) {
                        return;
                    }
                }
                // The daemon hung up. Wait, then re-establish and re-read.
                std::thread::sleep(RETRY);
            }
        });
    // A thread we cannot start means that reading is simply unavailable; the
    // other two still work, and the bar draws one fewer cell.
    drop(spawned);
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
