// SPDX-License-Identifier: AGPL-3.0-only
//! Stack state: which notifications are on screen, and for how long.

use std::time::{Duration, Instant};

use iced::{Subscription, Task};
use iced_layershell::to_layer_message;

use eclipse_services::notifications::{CloseReason, Event, Notification, Notifications};

/// How often the stack wakes to drain the bus and retire whatever has run out
/// of time. Shorter than the bar's tick: a toast that lingers a visible beat
/// after its lifetime reads as a stuck window.
const TICK: Duration = Duration::from_millis(200);

/// How many notifications are drawn at once. The rest wait their turn rather
/// than being dropped — a queued notification the human never saw must not be
/// reported to its sender as expired.
pub const VISIBLE: usize = 4;

/// One entry in the stack.
#[derive(Debug, Clone)]
pub struct Toast {
    pub notification: Notification,
    /// When it first reached a drawn slot. `None` while queued: the clock
    /// starts when the human could actually have read it.
    pub shown: Option<Instant>,
}

impl Toast {
    /// Whether its lifetime has run out. A notification with no lifetime — only
    /// a critical one can have that — never expires on its own.
    fn expired(&self, now: Instant) -> bool {
        match (self.shown, self.notification.expires_in) {
            (Some(shown), Some(lifetime)) => now.duration_since(shown) >= lifetime,
            _ => false,
        }
    }
}

#[to_layer_message]
#[derive(Debug, Clone)]
pub enum Message {
    /// Drain the bus and retire anything out of time.
    Tick,
    /// The human clicked the body of a notification.
    Dismiss(u32),
    /// The human clicked one of its buttons.
    Invoke(u32, String),
}

pub struct App {
    /// `None` when something else already owns the notification name — another
    /// daemon is running, and we must not steal it. The surface then stays
    /// empty for the rest of its life rather than half-serving the session.
    pub service: Option<Notifications>,
    pub toasts: Vec<Toast>,
    /// The surface height last asked for, so a tick that changes nothing does
    /// not ask the compositor to resize to the size it already has.
    height: u32,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        App {
            service: eclipse_services::notifications::spawn().ok(),
            toasts: Vec::new(),
            height: 1,
        }
    }

    /// The notifications that are actually on screen.
    pub fn drawn(&self) -> &[Toast] {
        let end = self.toasts.len().min(VISIBLE);
        &self.toasts[..end]
    }

    fn remove(&mut self, id: u32, reason: CloseReason) {
        if let Some(i) = self.toasts.iter().position(|t| t.notification.id == id) {
            self.toasts.remove(i);
            if let Some(service) = self.service.as_ref() {
                service.close(id, reason);
            }
        }
    }
}

/// Fold one bus event into the stack.
///
/// A `Posted` carrying an id already on screen replaces it in place and keeps
/// its slot — that is what `replaces_id` is for, and a progress notification
/// that jumped to the bottom of the stack on every update would be unreadable.
fn post(toasts: &mut Vec<Toast>, notification: Notification) {
    match toasts.iter_mut().find(|t| t.notification.id == notification.id) {
        // The replacement gets a fresh lifetime but keeps the moment it was
        // first shown, so a client cannot hold a slot forever by replacing.
        Some(existing) => existing.notification = notification,
        None => toasts.push(Toast {
            notification,
            shown: None,
        }),
    }
}

pub fn update(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::Tick => {
            while let Some(event) = app.service.as_ref().and_then(Notifications::try_recv) {
                match event {
                    Event::Posted(notification) => post(&mut app.toasts, *notification),
                    // The sender withdrew it; it is already gone as far as the
                    // bus is concerned, so we do not report the close back.
                    Event::Closed { id, .. } => {
                        app.toasts.retain(|t| t.notification.id != id);
                    }
                }
            }
            let now = Instant::now();
            let expired: Vec<u32> = app
                .drawn()
                .iter()
                .filter(|t| t.expired(now))
                .map(|t| t.notification.id)
                .collect();
            for id in expired {
                app.remove(id, CloseReason::Expired);
            }
            // Whatever is drawn now starts its clock now. A queued notification
            // promoted by an expiry gets its full lifetime from this moment.
            let end = app.toasts.len().min(VISIBLE);
            for toast in &mut app.toasts[..end] {
                toast.shown.get_or_insert(now);
            }
        }
        Message::Dismiss(id) => app.remove(id, CloseReason::Dismissed),
        Message::Invoke(id, key) => {
            if let Some(service) = app.service.as_ref() {
                // `invoke` closes it on the bus too, so the local removal must
                // not send a second `NotificationClosed` behind it.
                service.invoke(id, &key);
            }
            app.toasts.retain(|t| t.notification.id != id);
        }
        // `to_layer_message` injects the layer-control variants. We send
        // `SizeChange` ourselves below; the rest never arrive.
        _ => return Task::none(),
    }

    let wanted = crate::toasts::view::height(app.drawn());
    if wanted == app.height {
        return Task::none();
    }
    app.height = wanted;
    // The surface is only as tall as what it holds. An empty stack shrinks to a
    // pixel rather than leaving an invisible sheet over the corner of the
    // screen swallowing the human's clicks.
    Task::done(Message::SizeChange((crate::toasts::WIDTH, wanted)))
}

/// One thread, one tick. The bus handle lives on `App` because the human's
/// clicks have to reach it, so the thread here carries nothing but the beat.
pub fn subscription(_app: &App) -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(32, async move |mut sender| {
            std::thread::spawn(move || loop {
                if sender.try_send(Message::Tick).is_err() {
                    return;
                }
                std::thread::sleep(TICK);
            });
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use eclipse_services::notifications::{Action, Urgency};

    fn notification(id: u32, expires_in: Option<Duration>) -> Notification {
        Notification {
            id,
            app_name: "mail".into(),
            summary: "Summary".into(),
            body: "Body".into(),
            icon: String::new(),
            urgency: Urgency::Normal,
            actions: vec![Action {
                key: "read".into(),
                label: "Read".into(),
            }],
            expires_in,
            transient: false,
        }
    }

    /// No bus in the test environment, so the service is `None` and every path
    /// through `update` has to survive that.
    fn app() -> App {
        App {
            service: None,
            toasts: Vec::new(),
            height: 1,
        }
    }

    /// `replaces_id` is a replacement, not a second notification.
    #[test]
    fn a_replacement_keeps_its_place_in_the_stack() {
        let mut a = app();
        post(&mut a.toasts, notification(1, None));
        post(&mut a.toasts, notification(2, None));
        let mut third = notification(1, None);
        third.summary = "Replaced".into();
        post(&mut a.toasts, third);
        assert_eq!(a.toasts.len(), 2);
        assert_eq!(a.toasts[0].notification.summary, "Replaced");
        assert_eq!(a.toasts[1].notification.id, 2);
    }

    /// A notification that has never been on screen must not be retired for
    /// running out of a lifetime it never started spending.
    #[test]
    fn a_queued_notification_does_not_expire_unseen() {
        let queued = Toast {
            notification: notification(1, Some(Duration::from_millis(1))),
            shown: None,
        };
        assert!(!queued.expired(Instant::now()));
    }

    /// Only critical notifications reach `expires_in: None`, and those sit
    /// until the human deals with them.
    #[test]
    fn a_notification_without_a_lifetime_never_times_out() {
        let pinned = Toast {
            notification: notification(1, None),
            shown: Some(Instant::now() - Duration::from_secs(3600)),
        };
        assert!(!pinned.expired(Instant::now()));
    }

    /// Past the visible count the stack queues rather than drops: a
    /// notification the human never saw is still owed its moment.
    #[test]
    fn the_stack_queues_what_it_cannot_draw() {
        let mut a = app();
        for id in 1..=(VISIBLE as u32 + 2) {
            post(&mut a.toasts, notification(id, None));
        }
        assert_eq!(a.toasts.len(), VISIBLE + 2);
        assert_eq!(a.drawn().len(), VISIBLE);
    }

    /// Clicking a button closes it on the bus through `invoke`; removing it
    /// here must not send a second close behind that.
    #[test]
    fn invoking_an_action_removes_it_without_a_second_close() {
        let mut a = app();
        post(&mut a.toasts, notification(7, None));
        let _ = update(&mut a, Message::Invoke(7, "read".into()));
        assert!(a.toasts.is_empty());
    }

    /// An empty stack must not leave a sheet over the corner of the screen.
    #[test]
    fn an_empty_stack_asks_for_no_room() {
        let a = app();
        assert_eq!(crate::toasts::view::height(a.drawn()), 1);
    }
}
