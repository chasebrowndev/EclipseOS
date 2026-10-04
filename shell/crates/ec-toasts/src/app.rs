// SPDX-License-Identifier: AGPL-3.0-only
//! Stack state: which notifications are on screen, and for how long.

use std::time::{Duration, Instant};

use iced::window::Id;
use iced::{Subscription, Task};
use iced_layershell::to_layer_message;

use ec_services::notifications::{CloseReason, Event, Notification, Notifications};
use ec_ui::ipc::EventAnim;
use ec_ui::motion::{Motion, Tween};

/// How often the stack wakes to drain the bus and retire whatever has run out
/// of time. Shorter than the bar's tick: a toast that lingers a visible beat
/// after its lifetime reads as a stuck window.
const TICK: Duration = Duration::from_millis(200);

/// Gap between the bar's bottom edge and the first card.
const TOP_MARGIN: i32 = ec_ui::tokens::bar::SHEET_MARGIN_TOP;
/// Gap between the cards and the right edge of the output: the bar
/// capsule's own, so the stack lines up under its right end.
const RIGHT_MARGIN: i32 = ec_ui::tokens::bar::SHEET_MARGIN_X;

/// How many notifications are drawn at once. The rest wait their turn rather
/// than being dropped — a queued notification the human never saw must not be
/// reported to its sender as expired.
pub const VISIBLE: usize = 4;

/// The style a card moves in (`animations.toast`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// In from the anchored (right) edge, and back out through it.
    Slide,
    /// Alpha only.
    Fade,
    /// Lands and leaves at once.
    None,
}

impl Style {
    /// An add-on's style, which this stack does not know, is the built-in.
    pub fn parse(s: &str) -> Style {
        match s {
            "fade" => Style::Fade,
            "none" => Style::None,
            _ => Style::Slide,
        }
    }
}

/// What a card is doing at an instant: how far in it is (`0.0` is not there,
/// `1.0` is at rest) and the style it moves in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose {
    pub style: Style,
    pub presence: f32,
}

/// The stack's `animations.toast`, no motion until the compositor says so.
pub fn still() -> EventAnim {
    EventAnim {
        style: "none".to_owned(),
        motion: Motion::SNAP,
    }
}

/// One entry in the stack.
#[derive(Debug, Clone)]
pub struct Toast {
    pub notification: Notification,
    /// When it first reached a drawn slot. `None` while queued: the clock
    /// starts when the human could actually have read it.
    pub shown: Option<Instant>,
    /// When it was dismissed, expired or withdrawn and began to leave. It is
    /// still in the stack, holding its slot, until its exit has played.
    pub leaving: Option<Instant>,
}

impl Toast {
    /// Where the card is in its entrance or exit. The exit runs the same
    /// curve backwards from wherever the entrance had got to — a card
    /// dismissed mid-arrival does not jump to full first.
    pub fn pose(&self, now: Instant, anim: &EventAnim) -> Pose {
        let style = Style::parse(&anim.style);
        let m = anim.motion;
        let presence = if style == Style::None || m.snaps() {
            1.0
        } else if let Some(left) = self.leaving {
            let arrived = self.shown.map_or(1.0, |s| {
                Tween::new(0.0, 1.0, s, m.duration, m.curve).value_at(left)
            });
            Tween::new(arrived.min(1.0), 0.0, left, m.duration, m.curve).value_at(now)
        } else {
            self.shown.map_or(0.0, |s| {
                Tween::new(0.0, 1.0, s, m.duration, m.curve).value_at(now)
            })
        };
        Pose { style, presence }
    }

    /// Whether the card is mid-entrance or mid-exit, so the frame clock is
    /// needed.
    fn moving(&self, now: Instant, anim: &EventAnim) -> bool {
        self.leaving
            .or(self.shown)
            .is_some_and(|t| self.moving_from(t, now, anim))
    }

    /// Whether its exit has played out and it can go.
    fn left(&self, now: Instant, anim: &EventAnim) -> bool {
        self.leaving.is_some_and(|t| !self.moving_from(t, now, anim))
    }

    fn moving_from(&self, since: Instant, now: Instant, anim: &EventAnim) -> bool {
        Style::parse(&anim.style) != Style::None
            && !anim.motion.snaps()
            && now.saturating_duration_since(since) < anim.motion.duration
    }

    /// Whether its lifetime has run out. A notification with no lifetime — only
    /// a critical one can have that — never expires on its own.
    fn expired(&self, now: Instant) -> bool {
        if self.leaving.is_some() {
            return false;
        }
        match (self.shown, self.notification.expires_in) {
            (Some(shown), Some(lifetime)) => now.duration_since(shown) >= lifetime,
            _ => false,
        }
    }
}

#[to_layer_message(multi)]
#[derive(Debug, Clone)]
pub enum Message {
    /// Drain the bus and retire anything out of time.
    Tick,
    /// One frame of a card's entrance or exit. Sent only while one plays.
    Frame,
    /// A surface of ours went away, whether we closed it or the compositor
    /// did (its output was unplugged).
    Closed(Id),
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
    /// The stack's layer surface, while it has one. There is none while
    /// nothing is drawn: an empty surface, however small, is still a region
    /// the compositor blurs (BLUR-02).
    surface: Option<Id>,
    /// The surface height last asked for, so a tick that changes nothing does
    /// not ask the compositor to resize to the size it already has.
    height: u32,
    /// The card's glass radius, read once from `decoration.rounding` at
    /// startup (BLUR-06). `crate::conn::fetch_glass` is fail-soft, so
    /// this falls back to the compile-time token when nothing answers.
    pub glass_radius: f32,
    /// Whether the compositor's blur is behind the cards, read with the
    /// radius; `false` when nothing answers, so the cards stay opaque.
    pub blur: bool,
    /// `animations.toast`, re-read whenever a card arrives or leaves.
    pub anim: EventAnim,
    /// The instant the view draws at: the last tick or frame.
    pub now: Instant,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        let (radius, blur) = crate::conn::fetch_glass();
        App {
            service: ec_services::notifications::spawn().ok(),
            toasts: Vec::new(),
            surface: None,
            height: 0,
            glass_radius: radius.unwrap_or(ec_ui::tokens::radius::CARD),
            blur: blur.unwrap_or(false),
            anim: crate::conn::fetch_anim().unwrap_or_else(still),
            now: Instant::now(),
        }
    }

    /// Whether another frame is needed.
    pub fn animating(&self) -> bool {
        self.drawn().iter().any(|t| t.moving(self.now, &self.anim))
    }

    /// Pick up a changed `animations.toast`; keep the old one when nothing
    /// answers.
    ///
    /// Only while this stack serves the bus: with no service nothing arrives
    /// to animate, and a test must not read the host's socket.
    fn refresh_anim(&mut self) {
        if self.service.is_none() {
            return;
        }
        if let Some(a) = crate::conn::fetch_anim() {
            self.anim = a;
        }
    }

    /// Start `id`'s exit (or end it at once, with no motion). `true` when
    /// there was a card to send.
    fn leave(&mut self, id: u32) -> bool {
        let Some(i) = self
            .toasts
            .iter()
            .position(|t| t.notification.id == id && t.leaving.is_none())
        else {
            return false;
        };
        self.refresh_anim();
        let t = &mut self.toasts[i];
        // A card that never reached a slot has nothing to animate out.
        if t.shown.is_none() || Style::parse(&self.anim.style) == Style::None || self.anim.motion.snaps() {
            self.toasts.remove(i);
        } else {
            t.leaving = Some(self.now);
        }
        true
    }

    /// The notifications that are actually on screen.
    pub fn drawn(&self) -> &[Toast] {
        let end = self.toasts.len().min(VISIBLE);
        &self.toasts[..end]
    }

    fn remove(&mut self, id: u32, reason: CloseReason) {
        if self.leave(id) {
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
    match toasts
        .iter_mut()
        .find(|t| t.notification.id == notification.id && t.leaving.is_none())
    {
        // The replacement gets a fresh lifetime but keeps the moment it was
        // first shown, so a client cannot hold a slot forever by replacing.
        Some(existing) => existing.notification = notification,
        None => toasts.push(Toast {
            notification,
            shown: None,
            leaving: None,
        }),
    }
}

pub fn update(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::Frame => {
            app.now = Instant::now();
            let anim = app.anim.clone();
            let now = app.now;
            app.toasts.retain(|t| !t.left(now, &anim));
        }
        Message::Tick => {
            app.now = Instant::now();
            while let Some(event) = app.service.as_ref().and_then(Notifications::try_recv) {
                match event {
                    Event::Posted(notification) => {
                        app.refresh_anim();
                        post(&mut app.toasts, *notification);
                    }
                    // The sender withdrew it; it is already gone as far as the
                    // bus is concerned, so we do not report the close back.
                    Event::Closed { id, .. } => {
                        app.leave(id);
                    }
                }
            }
            let now = app.now;
            let anim = app.anim.clone();
            app.toasts.retain(|t| !t.left(now, &anim));
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
            app.leave(id);
        }
        Message::Closed(id) => {
            // Forget it, so the next card opens a fresh one rather than
            // resizing a surface that is gone.
            if app.surface == Some(id) {
                app.surface = None;
                app.height = 0;
            }
        }
        // `to_layer_message` injects the layer-control variants. We send
        // them ourselves below; they never arrive here.
        _ => return Task::none(),
    }
    sync_surface(app)
}

/// Make the surface match the stack: open it with the first card, resize it
/// as cards come and go, close it with the last.
///
/// The surface is only as tall as what it holds, and an empty stack has no
/// surface at all — not an invisible sheet over the corner swallowing the
/// human's clicks, and not a transparent pixel for the compositor to blur.
fn sync_surface(app: &mut App) -> Task<Message> {
    use iced_layershell::reexport::{
        Anchor, KeyboardInteractivity, Layer, NewLayerShellSettings, OutputOption,
    };
    let wanted = crate::view::height(app.drawn());
    match app.surface {
        Some(id) if wanted == 0 => {
            app.surface = None;
            app.height = 0;
            Task::done(Message::RemoveWindow(id))
        }
        Some(id) if wanted != app.height => {
            app.height = wanted;
            Task::done(Message::SizeChange {
                id,
                size: (crate::WIDTH, wanted),
            })
        }
        None if wanted > 0 => {
            let id = Id::unique();
            app.surface = Some(id);
            app.height = wanted;
            Task::done(Message::NewLayerShell {
                settings: NewLayerShellSettings {
                    size: Some((crate::WIDTH, wanted)),
                    anchor: Anchor::Top | Anchor::Right,
                    // Above the bar and above fullscreen windows: a notification
                    // the human cannot see is a notification that did not happen.
                    layer: Layer::Overlay,
                    // Toasts never push windows around. The stack comes and goes
                    // several times a minute, and a reflow each time would be
                    // unusable.
                    exclusive_zone: Some(0),
                    margin: Some((TOP_MARGIN, RIGHT_MARGIN, 0, 0)),
                    keyboard_interactivity: KeyboardInteractivity::None,
                    // Wherever the compositor puts new things, as before.
                    output_option: OutputOption::Active,
                    // The action buttons need the pointer.
                    events_transparent: false,
                    // The process namespace, which is what layer rules match.
                    namespace: None,
                },
                id,
            })
        }
        _ => Task::none(),
    }
}

/// One thread, one tick. The bus handle lives on `App` because the human's
/// clicks have to reach it, so the thread here carries nothing but the beat.
pub fn subscription(app: &App) -> Subscription<Message> {
    let tick = Subscription::run(|| {
        iced::stream::channel(32, async move |mut sender| {
            std::thread::spawn(move || loop {
                if sender.try_send(Message::Tick).is_err() {
                    return;
                }
                std::thread::sleep(TICK);
            });
        })
    });
    // The frame clock runs only while a card moves.
    let frames = if app.animating() {
        Subscription::run(|| {
            iced::stream::channel(8, async move |mut sender| {
                std::thread::spawn(move || loop {
                    std::thread::sleep(Duration::from_millis(ec_ui::tokens::motion::FRAME_MS));
                    if sender.try_send(Message::Frame).is_err() {
                        return;
                    }
                });
            })
        })
    } else {
        Subscription::none()
    };
    Subscription::batch([tick, frames, iced::window::close_events().map(Message::Closed)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_services::notifications::{Action, Urgency};

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
            surface: None,
            height: 0,
            glass_radius: ec_ui::tokens::radius::CARD,
            blur: false,
            anim: still(),
            now: Instant::now(),
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
            leaving: None,
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
            leaving: None,
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

    /// An empty stack has no surface at all: even a transparent pixel of one
    /// is a region the compositor blurs (BLUR-02). The first card opens it and
    /// the last one closes it.
    #[test]
    fn the_surface_exists_only_while_something_is_drawn() {
        let mut a = app();
        let _ = update(&mut a, Message::Tick);
        assert!(a.surface.is_none());
        post(&mut a.toasts, notification(7, None));
        let _ = update(&mut a, Message::Tick);
        let opened = a.surface.expect("the first card opens the surface");
        post(&mut a.toasts, notification(8, None));
        let _ = update(&mut a, Message::Tick);
        assert_eq!(a.surface, Some(opened), "a second card resizes, not reopens");
        let _ = update(&mut a, Message::Dismiss(7));
        let _ = update(&mut a, Message::Dismiss(8));
        assert!(a.surface.is_none());
    }

    fn slide() -> EventAnim {
        EventAnim {
            style: "slide".into(),
            motion: Motion {
                enabled: true,
                curve: ec_ui::motion::Curve::Linear,
                duration: Duration::from_millis(200),
            },
        }
    }

    /// A card arrives over the duration, rests, and a dismissed one stays in
    /// its slot until its exit has played, then goes.
    #[test]
    fn a_dismissed_card_slides_out_and_is_then_forgotten() {
        let mut a = app();
        a.anim = slide();
        // `Frame` reads the real clock, so the story ends 300ms ago.
        let t0 = Instant::now() - Duration::from_millis(600);
        a.now = t0;
        post(&mut a.toasts, notification(7, None));
        a.toasts[0].shown = Some(t0);
        assert_eq!(a.toasts[0].pose(t0, &a.anim).presence, 0.0);
        let half = t0 + Duration::from_millis(100);
        assert!((a.toasts[0].pose(half, &a.anim).presence - 0.5).abs() < 1e-3);
        let rest = t0 + Duration::from_millis(300);
        assert_eq!(a.toasts[0].pose(rest, &a.anim).presence, 1.0);

        a.now = rest;
        a.remove(7, CloseReason::Dismissed);
        assert_eq!(a.toasts.len(), 1, "it holds its slot while it leaves");
        assert!(a.toasts[0].leaving.is_some());
        assert!(a.animating());
        let mid = rest + Duration::from_millis(100);
        assert!((a.toasts[0].pose(mid, &a.anim).presence - 0.5).abs() < 1e-3);
        let _ = update(&mut a, Message::Frame);
        assert!(a.toasts.is_empty());
    }

    /// With no motion a card lands and leaves at once, as it always did.
    #[test]
    fn without_motion_a_dismissed_card_goes_at_once() {
        let mut a = app();
        post(&mut a.toasts, notification(7, None));
        a.toasts[0].shown = Some(Instant::now());
        assert_eq!(a.toasts[0].pose(Instant::now(), &a.anim).presence, 1.0);
        assert!(!a.animating());
        a.remove(7, CloseReason::Dismissed);
        assert!(a.toasts.is_empty());
    }

    /// A new card in an open stack animates alone; the ones already there
    /// are at rest.
    #[test]
    fn only_the_new_card_moves() {
        let mut a = app();
        a.anim = slide();
        let t0 = Instant::now();
        post(&mut a.toasts, notification(1, None));
        a.toasts[0].shown = Some(t0 - Duration::from_secs(5));
        post(&mut a.toasts, notification(2, None));
        a.toasts[1].shown = Some(t0);
        a.now = t0 + Duration::from_millis(50);
        assert_eq!(a.toasts[0].pose(a.now, &a.anim).presence, 1.0);
        assert!(a.toasts[1].pose(a.now, &a.anim).presence < 1.0);
        assert!(a.animating());
    }
}
