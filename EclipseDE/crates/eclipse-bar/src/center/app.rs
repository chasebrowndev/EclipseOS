// SPDX-License-Identifier: AGPL-3.0-only
//! Control-center state.
//!
//! The panel is a menu, not a window: it is spawned by a keybind, it answers
//! one question, and it goes away. So there is no "stay open" state to keep —
//! every action either exits the runtime or, if logind refused, leaves a
//! sentence on screen saying why.

use iced::{Subscription, Task};
use iced_layershell::to_layer_message;

use eclipse_services::session::{Action, Availability, Session};
use eclipse_services::status::{Battery, Bluetooth, Network, Update};

/// How long the status thread sleeps between passes. The panel is on screen
/// for seconds at a time, so this only has to be faster than a human.
const POLL: std::time::Duration = std::time::Duration::from_millis(500);

/// Every action, in the order the panel lists them: the reversible ones first,
/// and the two that take the machine away from the human last.
pub const ACTIONS: [Action; 6] = [
    Action::Lock,
    Action::LogOut,
    Action::Suspend,
    Action::Hibernate,
    Action::Reboot,
    Action::PowerOff,
];

#[to_layer_message]
#[derive(Debug, Clone)]
pub enum Message {
    /// The system bus said something about network, bluetooth or battery.
    Status(Update),
    /// A row was clicked.
    Perform(Action),
    /// Dismiss without doing anything.
    Close,
}

pub struct App {
    /// `None` on a machine with no system bus. The status rows still draw; the
    /// session rows say the session cannot be reached rather than offering
    /// buttons that would do nothing.
    pub session: Option<Session>,
    /// Every action's availability, asked once at startup. logind's answers
    /// do not change while a menu is open, and re-asking on every frame would
    /// put a polkit round trip in the draw path. Refused actions stay in the
    /// list and are drawn greyed out: a menu whose rows move around depending
    /// on what the machine supports is a menu you cannot learn.
    pub offered: Vec<(Action, Availability)>,
    pub network: Network,
    pub bluetooth: Bluetooth,
    pub battery: Option<Battery>,
    /// logind's own refusal, shown verbatim. We do not translate it: a policy
    /// decision explained in our words instead of its own is a worse answer.
    pub problem: Option<String>,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        let session = Session::connect().ok();
        let offered = session
            .as_ref()
            .map(|s| ACTIONS.iter().map(|&a| (a, s.availability(a))).collect())
            .unwrap_or_default();
        App {
            session,
            offered,
            network: Network::default(),
            bluetooth: Bluetooth::default(),
            battery: None,
            problem: None,
        }
    }
}

pub fn update(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::Status(update) => match update {
            Update::Network(network) => app.network = network,
            Update::Bluetooth(bluetooth) => app.bluetooth = bluetooth,
            Update::Battery(battery) => app.battery = battery,
        },
        Message::Close => return iced::exit(),
        Message::Perform(action) => {
            let Some(session) = app.session.as_ref() else {
                app.problem = Some("no session bus".to_owned());
                return Task::none();
            };
            // `true`: the human is standing right there, having just clicked.
            // polkit may put an authentication prompt in front of them, which
            // is the whole point of showing a `Challenge` action at all.
            match session.perform(action, true) {
                // The machine is on its way out, or the session is. Either way
                // this panel has nothing left to say.
                Ok(()) => return iced::exit(),
                Err(error) => app.problem = Some(error.to_string()),
            }
        }
        // `to_layer_message` injects the layer-control variants. The panel is
        // a fixed size for its whole short life and sends none of them.
        _ => {}
    }
    Task::none()
}

/// The same system-bus watchers the bar runs, on the panel's own thread.
pub fn subscription(_app: &App) -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(32, async move |mut sender| {
            std::thread::spawn(move || {
                let Ok(status) = eclipse_services::status::spawn() else {
                    return;
                };
                loop {
                    while let Some(update) = status.try_recv() {
                        if sender.try_send(Message::Status(update)).is_err() {
                            return;
                        }
                    }
                    std::thread::sleep(POLL);
                }
            });
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Without a bus there is nothing to offer, and clicking anyway must not
    /// panic — it must say so.
    #[test]
    fn a_missing_session_bus_is_a_sentence_not_a_crash() {
        let mut app = App {
            session: None,
            offered: Vec::new(),
            network: Network::default(),
            bluetooth: Bluetooth::default(),
            battery: None,
            problem: None,
        };
        let _ = update(&mut app, Message::Perform(Action::PowerOff));
        assert!(app.problem.is_some());
    }

    /// An injected layer variant must be a no-op, not a panic.
    #[test]
    fn an_injected_layer_variant_is_a_no_op() {
        let mut app = App::new();
        let _ = update(&mut app, Message::SizeChange((crate::center::WIDTH, 100)));
        assert!(app.problem.is_none());
    }

    /// Power off is last. A menu that puts it next to "lock" is a menu that
    /// eventually powers the machine off by accident.
    #[test]
    fn the_irreversible_actions_are_at_the_bottom() {
        assert_eq!(ACTIONS[0], Action::Lock);
        assert_eq!(ACTIONS[ACTIONS.len() - 1], Action::PowerOff);
    }
}
