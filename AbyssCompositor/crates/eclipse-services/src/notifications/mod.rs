// SPDX-License-Identifier: AGPL-3.0-only
//! The `org.freedesktop.Notifications` server (ADR 0038 — in-house, no dunst,
//! no mako).
//!
//! Shape: [`Notifications::spawn`] takes the well-known name on the session
//! bus and moves the whole D-Bus side onto its own thread. The GUI keeps the
//! returned handle, drains [`Event`]s from it each frame, and sends the human's
//! decisions back with [`Notifications::dismiss`] and
//! [`Notifications::invoke`]. iced runs no async runtime, so the boundary is a
//! plain `std::sync::mpsc` channel — the same idiom the settings app already
//! uses for its config watcher.
//!
//! Two behaviours are deliberate and tested:
//!
//! * **The body is never logged.** A notification body is other people's mail;
//!   it goes to the screen and nowhere else. The tracing calls in here name
//!   ids and app names only.
//! * **`expire_timeout` is advice, and a client cannot pin a notification on
//!   screen forever by asking.** A negative timeout means "server decides",
//!   zero means "never expire", and the second is clamped for everything below
//!   [`Urgency::Critical`] — only a critical notification may sit until the
//!   human dismisses it.

mod server;

use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

pub use server::spawn;

/// How loudly the client asked to be heard. The `urgency` hint, as a byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Urgency {
    Low,
    #[default]
    Normal,
    Critical,
}

impl Urgency {
    fn from_hint(byte: u8) -> Self {
        match byte {
            0 => Self::Low,
            2 => Self::Critical,
            _ => Self::Normal,
        }
    }
}

/// One button. `key` goes back over the bus in `ActionInvoked`; `label` is
/// what the human reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub key: String,
    pub label: String,
}

/// Why a notification left the screen. The numbering is the wire protocol's,
/// not ours — `NotificationClosed` carries exactly these values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    Expired = 1,
    Dismissed = 2,
    ByCall = 3,
    Undetermined = 4,
}

/// A notification as the GUI needs it: parsed, clamped, and with the hint bag
/// already reduced to the few things we render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub id: u32,
    pub app_name: String,
    pub summary: String,
    pub body: String,
    /// Icon name or path, as given. Empty when the client sent none.
    pub icon: String,
    pub urgency: Urgency,
    pub actions: Vec<Action>,
    /// `None` means it stays until the human dismisses it. Only a
    /// [`Urgency::Critical`] notification can reach that state.
    pub expires_in: Option<Duration>,
    /// The `transient` hint: show it, but keep no history entry.
    pub transient: bool,
}

/// The server's default lifetime when a client passes `-1`.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
/// The longest a non-critical notification may sit on screen, however politely
/// it asked.
const MAX_TIMEOUT: Duration = Duration::from_secs(30);

impl Notification {
    /// Turn the wire's `expire_timeout` (milliseconds, `0` = never, `-1` =
    /// server decides) into a lifetime this server is willing to honour.
    fn lifetime(expire_timeout: i32, urgency: Urgency) -> Option<Duration> {
        match expire_timeout {
            ..=-1 => Some(DEFAULT_TIMEOUT),
            0 if urgency == Urgency::Critical => None,
            0 => Some(MAX_TIMEOUT),
            ms => Some(Duration::from_millis(ms as u64).min(MAX_TIMEOUT)),
        }
    }
}

/// What the GUI learns from the bus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A new notification, or a replacement for one already on screen — the
    /// id is the same in that case, and the old one is gone.
    Posted(Box<Notification>),
    /// The sending application withdrew it (`CloseNotification`).
    Closed { id: u32, reason: CloseReason },
}

/// What the GUI tells the bus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    /// The human closed it, or it timed out.
    Close { id: u32, reason: CloseReason },
    /// The human clicked a button. Also closes it, per the spec's usual
    /// `ActionInvoked` + `NotificationClosed` pairing.
    Invoke { id: u32, key: String },
}

/// The GUI's end of the notification service.
///
/// Dropping this stops the server: the command channel closes and the thread
/// releases the well-known name.
#[derive(Debug)]
pub struct Notifications {
    events: Receiver<Event>,
    commands: Sender<Command>,
}

impl Notifications {
    pub(crate) fn new(events: Receiver<Event>, commands: Sender<Command>) -> Self {
        Self { events, commands }
    }

    /// Next event, or `None` when there is nothing waiting. Never blocks —
    /// call it from the GUI thread.
    ///
    /// A dead server is reported as no events rather than as an error: the
    /// human's desktop keeps working without notifications, which is the right
    /// failure for a non-TCB service.
    pub fn try_recv(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    /// The human dismissed it, or its lifetime ran out.
    pub fn close(&self, id: u32, reason: CloseReason) {
        let _ = self.commands.send(Command::Close { id, reason });
    }

    /// The human clicked one of the notification's buttons.
    pub fn invoke(&self, id: u32, key: &str) {
        let _ = self.commands.send(Command::Invoke {
            id,
            key: key.to_owned(),
        });
    }
}

/// Pair the flat `["key", "label", …]` array the wire uses into buttons.
/// An odd trailing element is a malformed client and is dropped, not an error:
/// this server never fails a `Notify` it can partially understand.
fn parse_actions(flat: &[String]) -> Vec<Action> {
    flat.chunks_exact(2)
        .map(|pair| Action {
            key: pair[0].clone(),
            label: pair[1].clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_pair_up_and_an_odd_one_is_dropped() {
        let flat = ["ok".into(), "OK".into(), "later".into()];
        assert_eq!(
            parse_actions(&flat),
            vec![Action {
                key: "ok".into(),
                label: "OK".into()
            }]
        );
        assert!(parse_actions(&[]).is_empty());
    }

    #[test]
    fn a_client_cannot_pin_a_normal_notification_on_screen() {
        // Zero means "never expire" on the wire. Only critical gets it.
        assert_eq!(Notification::lifetime(0, Urgency::Normal), Some(MAX_TIMEOUT));
        assert_eq!(Notification::lifetime(0, Urgency::Critical), None);
        // Nor by asking for an hour.
        assert_eq!(Notification::lifetime(3_600_000, Urgency::Low), Some(MAX_TIMEOUT));
    }

    #[test]
    fn a_negative_timeout_means_the_server_decides() {
        assert_eq!(Notification::lifetime(-1, Urgency::Normal), Some(DEFAULT_TIMEOUT));
        assert_eq!(
            Notification::lifetime(-99, Urgency::Critical),
            Some(DEFAULT_TIMEOUT)
        );
    }

    #[test]
    fn an_ordinary_timeout_is_honoured() {
        assert_eq!(
            Notification::lifetime(2_000, Urgency::Normal),
            Some(Duration::from_secs(2))
        );
    }

    #[test]
    fn urgency_defaults_to_normal_for_anything_unrecognised() {
        assert_eq!(Urgency::from_hint(0), Urgency::Low);
        assert_eq!(Urgency::from_hint(1), Urgency::Normal);
        assert_eq!(Urgency::from_hint(2), Urgency::Critical);
        assert_eq!(Urgency::from_hint(200), Urgency::Normal);
        assert_eq!(Urgency::default(), Urgency::Normal);
    }
}
