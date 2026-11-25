// SPDX-License-Identifier: AGPL-3.0-only
//! Stack state: which notifications are on screen, and for how long.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use iced::window::Id;
use iced::{Subscription, Task};
use iced_layershell::to_layer_message;

use ec_services::notifications::{Class, CloseReason, Event, Notification, Notifications, Share, Silence};
use ec_ui::ipc::EventAnim;
use ec_ui::motion::{Curve, Motion, Tween};

use crate::art::{self, Art};
use ec_ui::markup::{self, Run};

/// How long the silence drawer takes to open or close, ease-out.
const DRAWER_MS: u64 = 180;

/// The drawer's duration in ms. Debug builds only: `EC_TOASTS_SLOW=10` slows it
/// tenfold so a headless session can capture frames from the middle of it.
fn drawer_ms() -> u64 {
    #[cfg(debug_assertions)]
    if let Some(k) = std::env::var("EC_TOASTS_SLOW")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        return DRAWER_MS * k.max(1);
    }
    DRAWER_MS
}

/// The freedesktop action a click on the card itself means.
const DEFAULT_ACTION: &str = "default";

/// Why a write failed, in words short enough for a drawer row. Never the
/// path or the text of an error from elsewhere.
fn reason(e: &std::io::Error) -> &'static str {
    use std::io::ErrorKind::*;
    match e.kind() {
        PermissionDenied => "permission denied",
        NotFound => "no config folder",
        StorageFull | QuotaExceeded => "disk full",
        ReadOnlyFilesystem => "read-only disk",
        _ => "the rules file cannot be edited",
    }
}

/// Start the app that sent a toast, by its desktop entry. The launch runs off
/// the UI thread: scanning the entries is the slow part.
fn open_source(desktop_entry: &str) {
    let want = desktop_entry.trim().trim_end_matches(".desktop").to_lowercase();
    if want.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        let entry = ec_services::apps::scan(None)
            .into_iter()
            .find(|e| e.id.trim_end_matches(".desktop").to_lowercase() == want);
        if let Some(entry) = entry {
            let _ = ec_services::apps::launch(&entry, None);
        }
    });
}

/// How often the stack wakes to drain the bus and retire whatever has run out
/// of time. Shorter than the bar's tick: a toast that lingers a visible beat
/// after its lifetime reads as a stuck window.
const TICK: Duration = Duration::from_millis(200);

/// Gap between the bar's edge and the first card, on the edge the bar is on.
const BAR_MARGIN: i32 = ec_ui::tokens::bar::SHEET_MARGIN_TOP;
/// Gap between the cards and the side edge of the output: the bar
/// capsule's own, so the stack lines up under its end. It is also the gap to
/// the screen edge the bar is not on.
const EDGE_MARGIN: i32 = ec_ui::tokens::bar::SHEET_MARGIN_X;
/// The narrowest a card is drawn, however small the output.
const MIN_WIDTH: u32 = 220;

/// Which corner of the output the stack is anchored to. Everything that has a
/// direction in the stack derives from it: growth is away from the edge, the
/// silence drawer opens toward the centre, a card slides in from the nearest
/// side. Button order does not: it reads the same in every corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Corner {
    TopLeft,
    #[default]
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    pub const ALL: [Corner; 4] = [
        Corner::TopLeft,
        Corner::TopRight,
        Corner::BottomLeft,
        Corner::BottomRight,
    ];

    pub fn parse(s: &str) -> Option<Corner> {
        match s.trim() {
            "top-left" => Some(Corner::TopLeft),
            "top-right" => Some(Corner::TopRight),
            "bottom-left" => Some(Corner::BottomLeft),
            "bottom-right" => Some(Corner::BottomRight),
            _ => None,
        }
    }

    /// The corner this process was asked to use: `EC_TOASTS_CORNER`, else the
    /// default. There is no compositor setting for it yet, so this is the one
    /// way to choose (a session unit sets the variable).
    pub fn from_env() -> Corner {
        std::env::var("EC_TOASTS_CORNER")
            .ok()
            .and_then(|v| Corner::parse(&v))
            .unwrap_or_default()
    }

    pub fn right(self) -> bool {
        matches!(self, Corner::TopRight | Corner::BottomRight)
    }

    pub fn bottom(self) -> bool {
        matches!(self, Corner::BottomLeft | Corner::BottomRight)
    }

    /// `+1` when a card leaves toward the right edge, `-1` toward the left.
    pub fn side(self) -> f32 {
        if self.right() {
            1.0
        } else {
            -1.0
        }
    }

    /// Layer-shell margins `(top, right, bottom, left)`: the bar's gap on the
    /// bar's edge, the screen-edge gap on the other, the side gap on the
    /// anchored side only.
    pub fn margins(self, bar_bottom: bool) -> (i32, i32, i32, i32) {
        let v = if self.bottom() == bar_bottom {
            BAR_MARGIN
        } else {
            EDGE_MARGIN
        };
        match (self.bottom(), self.right()) {
            (false, true) => (v, EDGE_MARGIN, 0, 0),
            (false, false) => (v, 0, 0, EDGE_MARGIN),
            (true, true) => (0, EDGE_MARGIN, v, 0),
            (true, false) => (0, 0, v, EDGE_MARGIN),
        }
    }
}

/// The card width for an output `output` logical px wide: the full
/// [`crate::WIDTH`] where it fits with its margins, narrower where it does not.
pub fn fit_width(output: Option<f32>) -> u32 {
    match output {
        Some(w) if w.is_finite() => {
            (w - 2.0 * EDGE_MARGIN as f32).clamp(MIN_WIDTH as f32, crate::WIDTH as f32) as u32
        }
        _ => crate::WIDTH,
    }
}

/// How many cards are drawn at once. The rest wait their turn rather than
/// being dropped — a queued notification the human never saw must not be
/// reported to its sender as expired — and the stack says how many wait.
pub const VISIBLE: usize = 4;

/// How far right, as a share of the card's width, a drag must carry a card
/// for letting go to dismiss it. Short of that it springs back.
pub const SWIPE_DISMISS: f32 = 0.25;

/// The style a card moves in (`animations.toast`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// In from the nearest side edge, and back out through it.
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
    /// How long the pointer has held its clock still: time on screen that
    /// does not count against its lifetime.
    pub held: Duration,
    /// How far right a swipe had carried it when it was let go, so its exit
    /// carries on from there rather than from home.
    pub fling: f32,
    /// Its picture, resolved on arrival.
    pub art: Art,
    /// Its body, parsed on arrival.
    pub runs: Vec<Run>,
    /// How many lines the body wraps to at a card width, measured once per
    /// width by the view: `(card width, lines)`.
    pub body_lines: std::cell::Cell<Option<(u32, usize)>>,
}

impl Toast {
    pub fn new(notification: Notification) -> Self {
        Toast {
            art: art::resolve(&notification),
            runs: markup::parse(&notification.body),
            body_lines: std::cell::Cell::new(None),
            notification,
            shown: None,
            leaving: None,
            held: Duration::ZERO,
            fling: 0.0,
        }
    }

    /// Swap in a replacement of the same id. The picture is looked up again
    /// only if it could have changed: a progress notification replaces itself
    /// many times a second and must not walk the icon theme each time.
    fn replace(&mut self, new: Notification) {
        let old = &self.notification;
        if old.icon != new.icon || old.desktop_entry != new.desktop_entry || old.image != new.image {
            self.art = art::resolve(&new);
        }
        if old.body != new.body {
            self.runs = markup::parse(&new.body);
            self.body_lines.set(None);
        }
        self.notification = new;
    }

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

    /// A system yes/no question: it never times out, and its buttons answer
    /// through `Message::Invoke` with the action id (`yes` / `no`). The view
    /// decides how to draw it; this is the whole of the model.
    pub fn is_question(&self) -> bool {
        self.notification.question && self.notification.class == Class::System
    }

    /// Whether its lifetime has run out. A notification with no lifetime — only
    /// a critical one can have that — never expires on its own. Time the
    /// pointer held it does not count.
    fn expired(&self, now: Instant) -> bool {
        // A question waits for its answer however its lifetime was set
        // (ADR 0127); only a system toast can be one.
        if self.leaving.is_some() || self.is_question() {
            return false;
        }
        match (self.shown, self.notification.expires_in) {
            (Some(shown), Some(lifetime)) => {
                now.saturating_duration_since(shown).saturating_sub(self.held) >= lifetime
            }
            _ => false,
        }
    }
}

/// A card being dragged right.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Drag {
    pub id: u32,
    /// Where the pointer was when it first moved after the press; `None`
    /// until it has.
    from: Option<f32>,
    /// How far right the card is carried.
    pub dx: f32,
}

/// One column slot of the stack: a single card, or all the cards of one app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    /// The group key (or a per-notification key when the app sent none).
    pub key: String,
    /// Every notification of the unit, as indices into `App::toasts`, oldest
    /// first.
    pub all: Vec<usize>,
    /// The ones drawn as cards, top down. A folded group draws its newest
    /// alone; an open one draws newest first, so the card under the pointer
    /// when it opens stays where it was.
    pub cards: Vec<usize>,
    pub expanded: bool,
}

impl Unit {
    /// A group of several, folded to its newest card with the rest peeking.
    pub fn folded(&self) -> bool {
        !self.expanded && self.all.len() > 1
    }
}

/// What the stack draws: its units, and how many notifications wait beyond
/// [`VISIBLE`] cards.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    pub units: Vec<Unit>,
    pub hidden: usize,
}

impl Plan {
    /// Every notification that has a place on screen, folded or not.
    pub fn shown(&self) -> impl Iterator<Item = usize> + '_ {
        self.units.iter().flat_map(|u| u.all.iter().copied())
    }
}

#[to_layer_message(multi)]
#[derive(Debug, Clone)]
pub enum Message {
    /// The compositor's theme moved and the palette was re-applied (ADR 0084);
    /// nothing to do but let the views rebuild.
    ThemeChanged,
    /// Drain the bus and retire anything out of time.
    Tick,
    /// One frame of a card's entrance or exit. Sent only while one plays.
    Frame,
    /// A surface of ours went away, whether we closed it or the compositor
    /// did (its output was unplugged).
    Closed(Id),
    /// The human dismissed a notification (its close button, or a click).
    Dismiss(u32),
    /// The human clicked one of its buttons.
    Invoke(u32, String),
    /// The human asked to open what sent it.
    Open(u32),
    /// Open or close the silence drawer of a card.
    Drawer(u32),
    /// The human chose how to silence a card's source.
    Silence(u32, Silence),
    /// The pointer entered (`true`) or left a card. It holds the card's clock.
    HoverCard(u32, bool),
    /// The pointer entered or left a group's whole column. It opens the group
    /// and holds every clock in it.
    HoverGroup(String, bool),
    /// The human clicked a group's strips: hold it open until the pointer
    /// leaves.
    Pin(String),
    /// A press landed on a card's body; a drag or a click follows.
    Press(u32),
    /// The pointer moved over a card that is being pressed (x within it).
    Pointer(u32, f32),
    /// The press ended over the card.
    Release(u32),
}

pub struct App {
    /// `None` when something else already owns the notification name — another
    /// daemon is running, and we must not steal it. The surface then stays
    /// empty for the rest of its life rather than half-serving the session.
    pub service: Option<Notifications>,
    /// Whether a screen share is running and what toasts do then
    /// (`ui.capture-toasts`, ADR 0106).
    pub share: Share,
    /// Arrivals kept back by `hold` while a share ran, oldest first. They go
    /// to the stack the tick after it ends; a withdrawal removes them here.
    pub held: Vec<Notification>,
    pub toasts: Vec<Toast>,
    /// The stack's layer surface, while it has one. There is none while
    /// nothing is drawn: an empty surface, however small, is still a region
    /// the compositor blurs (BLUR-02).
    surface: Option<Id>,
    /// The surface height last asked for, so a tick that changes nothing does
    /// not ask the compositor to resize to the size it already has.
    height: u32,
    /// `decoration.rounding`, read once at startup (BLUR-06);
    /// `crate::conn::fetch_glass` is fail-soft, so this is `radius::CARD`
    /// when nothing answers. Not the card's radius by itself: see
    /// [`App::card_radius`].
    pub glass_radius: f32,
    /// Whether the compositor's blur is behind the cards, read with the
    /// radius; `false` when nothing answers, so the cards stay opaque.
    pub blur: bool,
    /// `animations.toast`, re-read whenever a card arrives or leaves.
    pub anim: EventAnim,
    /// The instant the view draws at: the last tick or frame.
    pub now: Instant,
    /// The card under the pointer.
    pub hover: Option<u32>,
    /// The group whose column is under the pointer.
    pub hover_group: Option<String>,
    /// Groups opened by a click, until the pointer leaves them.
    pub pinned: HashSet<String>,
    /// The card being pressed or dragged.
    pub drag: Option<Drag>,
    /// Where the stack sits.
    pub corner: Corner,
    /// Whether the bar is on the bottom edge, so the stack's offset from it
    /// is on the bottom edge.
    pub bar_bottom: bool,
    /// The card width in px, fitted to the output when the surface opens.
    pub width: u32,
    /// The card whose silence drawer is open. It holds the card's clock.
    pub drawer: Option<u32>,
    /// Why the last silence did not take, shown in the drawer.
    pub drawer_error: Option<String>,
    /// The card the drawer is drawn on: `drawer` while it is open, and still
    /// the card while the drawer closes.
    pub drawer_card: Option<u32>,
    /// The drawer's open amount (`0.0` shut, `1.0` open), while it moves.
    pub drawer_tween: Option<Tween>,
    /// `animations.reduce-motion`: the drawer opens and closes at once.
    pub reduce_motion: bool,
}

/// The toast card radius. With compositor glass the card is a pane the
/// client sizes itself, so it is rounder than a window by default:
/// `max(rounding, radius::TOAST)`. Without panes (frost/blur mode, or no glass
/// global) the compositor draws a sheet masked with `decoration.rounding`
/// that it cannot resize to the client, so the card uses `rounding` and the
/// two agree: a larger card radius would leave a grey wedge at each corner.
fn toast_radius(rounding: f32, glass: bool) -> f32 {
    if glass {
        rounding.max(ec_ui::tokens::radius::TOAST)
    } else {
        rounding
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    /// The radius the cards are drawn and declared at, now: it depends on
    /// whether glass panes are live, which is only known after startup.
    pub fn card_radius(&self) -> f32 {
        toast_radius(self.glass_radius, ec_ui::glass::available())
    }

    pub fn new() -> Self {
        let (radius, blur) = crate::conn::fetch_glass();
        let service = ec_services::notifications::spawn().ok();
        let share = service.as_ref().map(|s| s.share().clone()).unwrap_or_default();
        let mut app = App {
            service,
            share,
            held: Vec::new(),
            toasts: Vec::new(),
            surface: None,
            height: 0,
            glass_radius: radius.unwrap_or(ec_ui::tokens::radius::CARD),
            blur: blur.unwrap_or(false),
            anim: crate::conn::fetch_anim().unwrap_or_else(still),
            now: Instant::now(),
            hover: None,
            hover_group: None,
            pinned: HashSet::new(),
            drag: None,
            corner: Corner::from_env(),
            bar_bottom: crate::conn::fetch_bar_bottom().unwrap_or(false),
            width: fit_width(crate::conn::fetch_output_width()),
            drawer: None,
            drawer_error: None,
            drawer_card: None,
            drawer_tween: None,
            reduce_motion: ec_ui::ipc::fetch_reduce_motion().unwrap_or(false),
        };
        #[cfg(debug_assertions)]
        crate::preview::apply(&mut app);
        app
    }

    /// The open drawer's card and whether it carries an error row, for the
    /// surface height.
    pub fn drawer_extent(&self) -> Option<(u32, bool)> {
        self.drawer_card.map(|id| (id, self.drawer_error.is_some()))
    }

    /// How open the drawer is now, `0.0..=1.0`.
    pub fn drawer_t(&self) -> f32 {
        match (self.drawer_card, self.drawer_tween) {
            (None, _) => 0.0,
            (Some(_), Some(t)) => t.value_at(self.now),
            (Some(_), None) => 1.0,
        }
    }

    /// Open the drawer on `id` or shut it (`None`). It eases from where it is,
    /// so a click in mid-motion turns it round without a jump; with reduced
    /// motion, or `animate` false (the card is leaving), it is at once.
    fn move_drawer(&mut self, open: Option<u32>, animate: bool) {
        let mut from = self.drawer_t();
        self.drawer = open;
        let to = match open {
            Some(id) => {
                if self.drawer_card != Some(id) {
                    from = 0.0;
                }
                self.drawer_card = Some(id);
                1.0
            }
            None => 0.0,
        };
        if !animate || self.reduce_motion {
            self.drawer_tween = None;
            if open.is_none() {
                self.drawer_card = None;
            }
        } else {
            let d = Duration::from_millis(drawer_ms());
            self.drawer_tween = Some(Tween::new(from, to, Instant::now(), d, Curve::EaseOut));
        }
    }

    /// Retire a drawer tween that has landed, and the card of a shut drawer.
    fn settle_drawer(&mut self) {
        if self.drawer_tween.is_some_and(|t| t.done(self.now)) {
            self.drawer_tween = None;
            if self.drawer.is_none() {
                self.drawer_card = None;
            }
        }
    }

    /// Whether another frame is needed.
    pub fn animating(&self) -> bool {
        self.drawer_tween.is_some_and(|t| !t.done(self.now))
            || self
                .drawn()
                .into_iter()
                .any(|i| self.toasts[i].moving(self.now, &self.anim))
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

    /// Whether the group's column is open: under the pointer, or clicked open.
    fn open(&self, key: &str) -> bool {
        self.hover_group.as_deref() == Some(key) || self.pinned.contains(key)
    }

    /// What is on screen: notifications of one app share a unit, folded
    /// unless open, and at most [`VISIBLE`] cards are drawn.
    pub fn plan(&self) -> Plan {
        let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
        for (i, t) in self.toasts.iter().enumerate() {
            let g = &t.notification.group;
            let found = if g.is_empty() {
                None
            } else {
                groups.iter_mut().find(|(k, _)| k == g)
            };
            match found {
                Some((_, members)) => members.push(i),
                None if g.is_empty() => groups.push((format!("#{}", t.notification.id), vec![i])),
                None => groups.push((g.clone(), vec![i])),
            }
        }
        let mut plan = Plan::default();
        let mut used = 0;
        for (key, all) in groups {
            let expanded = all.len() > 1 && self.open(&key);
            let want = if expanded { all.len() } else { 1 };
            if used >= VISIBLE {
                plan.hidden += all.len();
                continue;
            }
            let take = want.min(VISIBLE - used);
            let cards: Vec<usize> = all.iter().rev().take(take).copied().collect();
            used += cards.len();
            plan.hidden += want - cards.len();
            plan.units.push(Unit {
                key,
                all,
                cards,
                expanded,
            });
        }
        plan
    }

    /// Indices of the notifications that have a place on screen.
    pub fn drawn(&self) -> Vec<usize> {
        self.plan().shown().collect()
    }

    fn remove(&mut self, id: u32, reason: CloseReason) {
        if self.leave(id) {
            if let Some(service) = self.service.as_ref() {
                service.close(id, reason);
            }
        }
    }

    /// Move the clock to `now`, charging the span since the last move to every
    /// shown card the pointer is holding.
    fn advance(&mut self, now: Instant) {
        let dt = now.saturating_duration_since(self.now);
        let (hover, group, drawer) = (self.hover, self.hover_group.clone(), self.drawer);
        for t in &mut self.toasts {
            let n = &t.notification;
            // The pointer holds a card's clock, and so does its open drawer:
            // a human choosing how long to silence it is not done reading.
            let holding = hover == Some(n.id)
                || drawer == Some(n.id)
                || (!n.group.is_empty() && group.as_deref() == Some(n.group.as_str()));
            if holding && t.shown.is_some() {
                t.held += dt;
            }
        }
        self.now = now;
    }

    /// End a press: dismiss when carried far enough, spring back when carried
    /// less, and treat as a click when it never moved (`click`).
    fn finish_drag(&mut self, id: u32, click: bool) {
        let Some(drag) = self.drag.take_if(|d| d.id == id) else {
            return;
        };
        if drag.dx >= SWIPE_DISMISS * self.width as f32 {
            if let Some(t) = self.toasts.iter_mut().find(|t| t.notification.id == id) {
                t.fling = drag.dx;
            }
            self.remove(id, CloseReason::Dismissed);
        } else if drag.dx <= ec_ui::tokens::motion::TAP_SLOP && click {
            self.click(id);
        }
    }

    /// A click on a card's body. A card of a group never dismisses: hovering
    /// already opens the group, so a click lands on a face the human is only
    /// reading, and it pins the group open instead. A lone card is dismissed,
    /// as it always was. Dismissal of a group's card is the cross or a swipe.
    fn click(&mut self, id: u32) {
        let plan = self.plan();
        let group = plan
            .units
            .iter()
            .find(|u| u.all.len() > 1 && u.all.iter().any(|&i| self.toasts[i].notification.id == id));
        match group {
            Some(unit) => {
                self.pinned.insert(unit.key.clone());
            }
            None => self.remove(id, CloseReason::Dismissed),
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
        // The replacement keeps the moment it was first shown, so a client
        // cannot hold a slot forever by replacing.
        Some(existing) => existing.replace(notification),
        None => toasts.push(Toast::new(notification)),
    }
}

/// Fold one bus event into the app, keeping arrivals back while `hold` is on
/// and a share runs (ADR 0106). A replacement of a held toast replaces it in
/// the queue, so the stack never sees the stale text.
fn admit(app: &mut App, event: Event) {
    match event {
        Event::Posted(notification) if app.share.holding() => {
            let id = notification.id;
            match app.held.iter_mut().find(|n| n.id == id) {
                Some(slot) => *slot = *notification,
                None => app.held.push(*notification),
            }
            // It replaces a toast already on screen: that one goes now.
            app.leave(id);
        }
        Event::Posted(notification) => {
            app.refresh_anim();
            post(&mut app.toasts, *notification);
        }
        // The sender withdrew it; it is already gone as far as the
        // bus is concerned, so we do not report the close back.
        Event::Closed { id, .. } => {
            app.held.retain(|n| n.id != id);
            app.leave(id);
        }
    }
}

/// Show what `hold` kept back, once the share has ended (or the mode changed).
fn release_held(app: &mut App) {
    if app.held.is_empty() || app.share.holding() {
        return;
    }
    app.refresh_anim();
    for n in std::mem::take(&mut app.held) {
        post(&mut app.toasts, n);
    }
}

pub fn update(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::ThemeChanged => return Task::none(),
        Message::Frame => {
            app.advance(Instant::now());
            app.settle_drawer();
            let anim = app.anim.clone();
            let now = app.now;
            app.toasts.retain(|t| !t.left(now, &anim));
        }
        Message::Tick => {
            app.advance(Instant::now());
            // `Closed` for an id not on screen is a quiet arrival replacing
            // nothing, or a withdrawal that lost a race: `leave` finds no
            // card and there is nothing to do.
            release_held(app);
            while let Some(event) = app.service.as_ref().and_then(Notifications::try_recv) {
                admit(app, event);
            }
            let now = app.now;
            let anim = app.anim.clone();
            app.toasts.retain(|t| !t.left(now, &anim));
            let drawn = app.drawn();
            let drawer = app.drawer;
            let expired: Vec<u32> = drawn
                .iter()
                .map(|&i| &app.toasts[i])
                .filter(|t| drawer != Some(t.notification.id) && t.expired(now))
                .map(|t| t.notification.id)
                .collect();
            for id in expired {
                app.remove(id, CloseReason::Expired);
            }
            // Whatever is drawn now starts its clock now. A queued notification
            // promoted by an expiry gets its full lifetime from this moment.
            for i in app.drawn() {
                app.toasts[i].shown.get_or_insert(now);
            }
            // A pinned group that has shrunk to one card is just a card.
            let live: HashSet<String> = app
                .plan()
                .units
                .iter()
                .filter(|u| u.all.len() > 1)
                .map(|u| u.key.clone())
                .collect();
            app.pinned.retain(|k| live.contains(k));
            // A drawer whose card has gone has nothing to be about.
            if let Some(d) = app.drawer {
                if !app
                    .toasts
                    .iter()
                    .any(|t| t.notification.id == d && t.leaving.is_none())
                {
                    app.move_drawer(None, false);
                    app.drawer_error = None;
                }
            }
            app.settle_drawer();
        }
        Message::Dismiss(id) => app.remove(id, CloseReason::Dismissed),
        Message::Open(id) => {
            let Some(t) = app.toasts.iter().find(|t| t.notification.id == id) else {
                return Task::none();
            };
            // Open belongs to apps; a system toast has no source to open.
            if t.notification.class != Class::App {
                return Task::none();
            }
            if t.notification.actions.iter().any(|a| a.key == DEFAULT_ACTION) {
                return update(app, Message::Invoke(id, DEFAULT_ACTION.to_owned()));
            }
            open_source(&t.notification.desktop_entry);
            app.remove(id, CloseReason::Dismissed);
        }
        Message::Drawer(id) => {
            app.advance(Instant::now());
            app.drawer_error = None;
            let app_toast = app.toasts.iter().any(|t| {
                t.notification.id == id && t.notification.class == Class::App && t.leaving.is_none()
            });
            if app.drawer == Some(id) || !app_toast {
                app.move_drawer(None, true);
            } else {
                // A card carried by a drag does not take a drawer with it.
                app.drag = None;
                app.move_drawer(Some(id), true);
            }
        }
        Message::Silence(id, how) => {
            let Some(t) = app.toasts.iter().find(|t| t.notification.id == id) else {
                return Task::none();
            };
            // Only an app can be silenced. A system source is not ours to mute:
            // it is how the human hears about the session itself.
            if t.notification.class != Class::App {
                return Task::none();
            }
            let (entry, name, group) = (
                t.notification.desktop_entry.clone(),
                t.notification.app_name.clone(),
                t.notification.group.clone(),
            );
            match ec_services::notifications::silence(&entry, &name, how) {
                Ok(()) => {
                    app.move_drawer(None, false);
                    app.drawer_error = None;
                    // Every card of the source leaves with it: the human
                    // asked not to see more of them.
                    let ids: Vec<u32> = app
                        .toasts
                        .iter()
                        .filter(|t| {
                            t.notification.class == Class::App
                                && (t.notification.id == id
                                    || (!group.is_empty() && t.notification.group == group))
                        })
                        .map(|t| t.notification.id)
                        .collect();
                    for id in ids {
                        app.remove(id, CloseReason::Dismissed);
                    }
                }
                Err(e) => {
                    // Never silently: say why, and keep the drawer open.
                    app.drawer_error = Some(format!("Could not save: {}", reason(&e)));
                }
            }
        }
        Message::Invoke(id, key) => {
            let resident = app
                .toasts
                .iter()
                .any(|t| t.notification.id == id && t.notification.resident);
            if let Some(service) = app.service.as_ref() {
                // `invoke` closes it on the bus too (unless resident), so the
                // local removal must not send a second `NotificationClosed`
                // behind it.
                service.invoke(id, &key);
            }
            // A resident card stays: the action did something, the
            // notification is still true.
            if !resident {
                app.leave(id);
            }
            if app.drawer_card == Some(id) && !resident {
                app.move_drawer(None, false);
            }
        }
        Message::HoverCard(id, over) => {
            app.advance(Instant::now());
            if over {
                app.hover = Some(id);
            } else {
                if app.hover == Some(id) {
                    app.hover = None;
                }
                // A drag that left the card ends where it was.
                app.finish_drag(id, false);
            }
        }
        Message::HoverGroup(key, over) => {
            app.advance(Instant::now());
            if over {
                app.hover_group = Some(key);
            } else if app.hover_group.as_deref() == Some(key.as_str()) {
                app.hover_group = None;
                app.pinned.remove(&key);
            }
        }
        Message::Pin(key) => {
            app.pinned.insert(key);
        }
        Message::Press(id) => {
            // An open drawer pins its card where it is.
            if app.drawer == Some(id) {
                return Task::none();
            }
            app.drag = Some(Drag {
                id,
                from: None,
                dx: 0.0,
            });
        }
        Message::Pointer(id, x) => {
            if let Some(d) = app.drag.as_mut().filter(|d| d.id == id) {
                let from = *d.from.get_or_insert(x);
                // Toward the nearest side edge is the way a card is carried off.
                d.dx = ((x - from) * app.corner.side()).max(0.0);
            }
        }
        Message::Release(id) => app.finish_drag(id, true),
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
    let wanted = crate::view::height(
        &app.toasts,
        &app.plan(),
        app.corner.bottom(),
        app.width,
        app.drawer_extent(),
    );
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
                size: (app.width, wanted),
            })
        }
        None if wanted > 0 => {
            // The output may have changed since the last stack: fit the cards
            // to it each time the surface opens.
            app.width = fit_width(crate::conn::fetch_output_width());
            app.bar_bottom = crate::conn::fetch_bar_bottom().unwrap_or(app.bar_bottom);
            let id = Id::unique();
            app.surface = Some(id);
            app.height = wanted;
            Task::done(Message::NewLayerShell {
                settings: NewLayerShellSettings {
                    size: Some((app.width, wanted)),
                    anchor: match (app.corner.bottom(), app.corner.right()) {
                        (false, true) => Anchor::Top | Anchor::Right,
                        (false, false) => Anchor::Top | Anchor::Left,
                        (true, true) => Anchor::Bottom | Anchor::Right,
                        (true, false) => Anchor::Bottom | Anchor::Left,
                    },
                    // Above the bar and above fullscreen windows: a notification
                    // the human cannot see is a notification that did not happen.
                    layer: Layer::Overlay,
                    // Toasts never push windows around. The stack comes and goes
                    // several times a minute, and a reflow each time would be
                    // unusable.
                    exclusive_zone: Some(0),
                    margin: Some(app.corner.margins(app.bar_bottom)),
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
    // Keep the palette on the compositor's theme (ADR 0084).
    Subscription::batch([
        subscription_inner(app),
        ec_ui::ipc::theme_subscription(|| Message::ThemeChanged),
    ])
}

fn subscription_inner(app: &App) -> Subscription<Message> {
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
    #[test]
    fn toast_radius_is_a_floor_over_the_rounding_only_with_glass() {
        use super::toast_radius;
        let t = ec_ui::tokens::radius::TOAST;
        assert_eq!(toast_radius(9.0, true), t);
        assert_eq!(toast_radius(t + 10.0, true), t + 10.0);
        // No panes: the compositor's sheet is masked with the rounding.
        assert_eq!(toast_radius(9.0, false), 9.0);
        assert_eq!(toast_radius(t + 10.0, false), t + 10.0);
    }

    use super::*;
    use crate::WIDTH;
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
            ..Notification::default()
        }
    }

    /// No bus in the test environment, so the service is `None` and every path
    /// through `update` has to survive that.
    fn app() -> App {
        App {
            service: None,
            share: Share::default(),
            held: Vec::new(),
            toasts: Vec::new(),
            surface: None,
            height: 0,
            glass_radius: ec_ui::tokens::radius::CARD,
            blur: false,
            anim: still(),
            now: Instant::now(),
            hover: None,
            hover_group: None,
            pinned: HashSet::new(),
            drag: None,
            corner: Corner::TopRight,
            bar_bottom: false,
            width: WIDTH,
            drawer: None,
            drawer_error: None,
            drawer_card: None,
            drawer_tween: None,
            reduce_motion: false,
        }
    }

    fn posted(id: u32) -> Event {
        Event::Posted(Box::new(notification(id, None)))
    }

    /// ADR 0106: `hold` keeps arrivals back for as long as the share runs, then
    /// shows them in order.
    #[test]
    fn hold_keeps_toasts_back_until_the_share_ends() {
        let mut a = app();
        a.share.set_mode(ec_services::notifications::ToastMode::Hold);
        a.share.set_active(true);
        admit(&mut a, posted(1));
        admit(&mut a, posted(2));
        assert!(a.toasts.is_empty());
        assert_eq!(a.held.len(), 2);
        release_held(&mut a);
        assert!(a.toasts.is_empty(), "still sharing");
        a.share.set_active(false);
        release_held(&mut a);
        assert!(a.held.is_empty());
        let ids: Vec<u32> = a.toasts.iter().map(|t| t.notification.id).collect();
        assert_eq!(ids, [1, 2]);
    }

    #[test]
    fn a_held_toast_can_be_replaced_or_withdrawn() {
        let mut a = app();
        a.share.set_mode(ec_services::notifications::ToastMode::Hold);
        a.share.set_active(true);
        admit(&mut a, posted(1));
        let mut newer = notification(1, None);
        newer.summary = "Newer".into();
        admit(&mut a, Event::Posted(Box::new(newer)));
        assert_eq!(a.held.len(), 1);
        assert_eq!(a.held[0].summary, "Newer");
        admit(&mut a, posted(2));
        admit(
            &mut a,
            Event::Closed {
                id: 2,
                reason: CloseReason::ByCall,
            },
        );
        assert_eq!(a.held.len(), 1);
    }

    /// The other modes, and `hold` with no share, show a toast at once.
    #[test]
    fn nothing_is_held_outside_hold_mode_or_a_share() {
        use ec_services::notifications::ToastMode;
        for (mode, active) in [
            (ToastMode::Hold, false),
            (ToastMode::Show, true),
            (ToastMode::Redact, true),
            (ToastMode::History, true),
        ] {
            let mut a = app();
            a.share.set_mode(mode);
            a.share.set_active(active);
            admit(&mut a, posted(1));
            assert!(a.held.is_empty(), "{mode:?} {active}");
            assert_eq!(a.toasts.len(), 1);
        }
    }

    /// Ending hold mode mid-share releases what it held.
    #[test]
    fn leaving_hold_mode_releases_the_queue() {
        use ec_services::notifications::ToastMode;
        let mut a = app();
        a.share.set_mode(ToastMode::Hold);
        a.share.set_active(true);
        admit(&mut a, posted(1));
        a.share.set_mode(ToastMode::Show);
        release_held(&mut a);
        assert_eq!(a.toasts.len(), 1);
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
        let queued = Toast::new(notification(1, Some(Duration::from_millis(1))));
        assert!(!queued.expired(Instant::now()));
    }

    /// Only critical notifications reach `expires_in: None`, and those sit
    /// until the human deals with them.
    #[test]
    fn a_notification_without_a_lifetime_never_times_out() {
        let pinned = Toast {
            shown: Some(Instant::now() - Duration::from_secs(3600)),
            ..Toast::new(notification(1, None))
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
        assert_eq!(a.plan().hidden, 2);
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

    fn grouped(id: u32, group: &str) -> Notification {
        Notification {
            group: group.into(),
            ..notification(id, Some(Duration::from_secs(5)))
        }
    }

    /// The pointer holds a card's clock: time spent under it does not count
    /// against its lifetime, and the clock runs again when it leaves.
    fn question(id: u32) -> Notification {
        Notification {
            class: Class::System,
            question: true,
            // Even a lifetime on the struct cannot make one expire.
            expires_in: Some(Duration::from_secs(1)),
            actions: vec![
                ec_services::notifications::Action {
                    key: "yes".into(),
                    label: "Reopen".into(),
                },
                ec_services::notifications::Action {
                    key: "no".into(),
                    label: "Start fresh".into(),
                },
            ],
            ..notification(id, None)
        }
    }

    #[test]
    fn a_question_never_expires_and_an_app_cannot_make_itself_one() {
        let t0 = Instant::now();
        let mut q = Toast::new(question(1));
        q.shown = Some(t0);
        assert!(q.is_question());
        assert!(!q.expired(t0 + Duration::from_secs(3600)));
        // The flag alone, from an app, is not enough: the class must be system.
        let mut fake = Toast::new(Notification {
            class: Class::App,
            expires_in: Some(Duration::from_secs(1)),
            ..question(2)
        });
        fake.shown = Some(t0);
        assert!(!fake.is_question());
        assert!(fake.expired(t0 + Duration::from_secs(2)));
    }

    #[test]
    fn a_question_holds_its_clock_under_the_pointer_and_stays_until_answered() {
        let mut a = app();
        post(&mut a.toasts, question(1));
        a.toasts[0].shown = Some(a.now);
        a.hover = Some(1);
        a.advance(a.now + Duration::from_secs(60));
        assert_eq!(a.toasts[0].held, Duration::from_secs(60));
        a.hover = None;
        assert!(!a.toasts[0].expired(a.now + Duration::from_secs(600)));
        // Answering goes through the same reply as any action: the id of the
        // button, and the card leaves.
        let _ = update(&mut a, Message::Invoke(1, "yes".into()));
        assert!(a.toasts.is_empty());
    }

    #[test]
    fn hovering_a_card_pauses_its_timeout() {
        let mut a = app();
        let t0 = Instant::now();
        a.now = t0;
        post(&mut a.toasts, grouped(1, ""));
        a.toasts[0].shown = Some(t0);
        a.hover = Some(1);
        a.advance(t0 + Duration::from_secs(10));
        assert!(!a.toasts[0].expired(t0 + Duration::from_secs(10)));
        a.hover = None;
        a.advance(t0 + Duration::from_secs(14));
        assert!(!a.toasts[0].expired(t0 + Duration::from_secs(14)));
        a.advance(t0 + Duration::from_secs(16));
        assert!(a.toasts[0].expired(t0 + Duration::from_secs(16)));
    }

    /// Several of one app share a unit: folded to the newest card until the
    /// pointer is over the column, then open, newest first.
    #[test]
    fn one_apps_cards_fold_and_open() {
        let mut a = app();
        for id in 1..=3 {
            post(&mut a.toasts, grouped(id, "mail"));
        }
        post(&mut a.toasts, grouped(4, "chat"));
        let plan = a.plan();
        assert_eq!(plan.units.len(), 2);
        assert!(plan.units[0].folded());
        assert_eq!(plan.units[0].cards, vec![2]);
        assert_eq!(plan.units[0].all.len(), 3);
        a.hover_group = Some("mail".into());
        let plan = a.plan();
        assert_eq!(plan.units[0].cards, vec![2, 1, 0]);
        assert!(!plan.units[0].folded());
    }

    /// Cards past the cap are counted, not lost.
    #[test]
    fn overflow_is_counted() {
        let mut a = app();
        for id in 1..=(VISIBLE as u32 + 3) {
            post(&mut a.toasts, grouped(id, &format!("app{id}")));
        }
        assert_eq!(a.plan().hidden, 3);
    }

    /// A close for something never shown (a quiet arrival replacing nothing)
    /// is not an error and changes nothing.
    #[test]
    fn a_close_for_an_unknown_id_is_ignored() {
        let mut a = app();
        post(&mut a.toasts, grouped(1, "mail"));
        assert!(!a.leave(99));
        assert_eq!(a.toasts.len(), 1);
    }

    /// A resident card survives its action.
    #[test]
    fn a_resident_card_stays_after_an_action() {
        let mut a = app();
        let mut n = grouped(5, "player");
        n.resident = true;
        post(&mut a.toasts, n);
        let _ = update(&mut a, Message::Invoke(5, "read".into()));
        assert_eq!(a.toasts.len(), 1);
    }

    /// A swipe past the threshold dismisses; a short one springs back; a press
    /// that never moves is a click.
    #[test]
    fn swiping_right_dismisses() {
        let mut a = app();
        post(&mut a.toasts, grouped(1, ""));
        post(&mut a.toasts, grouped(2, ""));
        let _ = update(&mut a, Message::Press(1));
        let _ = update(&mut a, Message::Pointer(1, 20.0));
        let _ = update(&mut a, Message::Pointer(1, 40.0));
        let _ = update(&mut a, Message::Release(1));
        assert_eq!(a.toasts.len(), 2, "short drag springs back");
        let _ = update(&mut a, Message::Press(1));
        let _ = update(&mut a, Message::Pointer(1, 20.0));
        let _ = update(&mut a, Message::Pointer(1, 20.0 + WIDTH as f32 * 0.5));
        let _ = update(&mut a, Message::Release(1));
        assert_eq!(a.toasts.len(), 1);
        let _ = update(&mut a, Message::Press(2));
        let _ = update(&mut a, Message::Release(2));
        assert!(a.toasts.is_empty(), "a click dismisses");
    }

    /// Clicking a group's card, folded or already opened by the pointer, pins
    /// the group and never dismisses.
    #[test]
    fn clicking_a_group_card_pins_and_never_dismisses() {
        let mut a = app();
        post(&mut a.toasts, grouped(1, "mail"));
        post(&mut a.toasts, grouped(2, "mail"));
        let _ = update(&mut a, Message::Press(2));
        let _ = update(&mut a, Message::Release(2));
        assert_eq!(a.toasts.len(), 2);
        assert!(a.pinned.contains("mail"));
        let _ = update(&mut a, Message::HoverGroup("mail".into(), true));
        // Opened by the pointer: the card under it is a member of an open group.
        let _ = update(&mut a, Message::Press(2));
        let _ = update(&mut a, Message::Release(2));
        assert_eq!(a.toasts.len(), 2, "a click on an open group's card keeps it");
        let _ = update(&mut a, Message::HoverGroup("mail".into(), false));
        assert!(a.pinned.is_empty());
        let _ = update(&mut a, Message::Pin("mail".into()));
        assert!(a.pinned.contains("mail"), "the strips pin the group");
    }

    /// An open silence drawer holds its card's clock even when the pointer is
    /// elsewhere, and the timeout cannot fire under it.
    #[test]
    fn an_open_drawer_holds_the_timeout() {
        let mut a = app();
        let t0 = Instant::now();
        a.now = t0;
        post(&mut a.toasts, grouped(1, ""));
        a.toasts[0].shown = Some(t0);
        let _ = update(&mut a, Message::Drawer(1));
        assert_eq!(a.drawer, Some(1));
        a.advance(t0 + Duration::from_secs(60));
        assert!(a.toasts[0].held >= Duration::from_secs(59));
        assert!(!a.toasts[0].expired(a.now));
        let _ = update(&mut a, Message::Drawer(1));
        assert_eq!(a.drawer, None, "the same button closes it");
    }

    /// A system card has no drawer and cannot be silenced or opened: only an
    /// app is the human's to quiet.
    #[test]
    fn a_system_card_takes_no_drawer_silence_or_open() {
        let mut a = app();
        post(&mut a.toasts, question(1));
        let _ = update(&mut a, Message::Drawer(1));
        assert_eq!(a.drawer, None);
        let _ = update(&mut a, Message::Silence(1, Silence::Forever));
        let _ = update(&mut a, Message::Open(1));
        assert_eq!(a.toasts.len(), 1);
    }

    #[test]
    fn the_corners_mirror_each_other() {
        assert_eq!(Corner::parse("bottom-left"), Some(Corner::BottomLeft));
        assert_eq!(Corner::parse("middle"), None);
        for c in Corner::ALL {
            let (t, r, b, l) = c.margins(false);
            assert!((t > 0) != (b > 0), "{c:?}: one vertical margin");
            assert!((r > 0) != (l > 0), "{c:?}: one side margin");
            assert_eq!(c.side(), if c.right() { 1.0 } else { -1.0 });
        }
        // The bar's gap is on the bar's edge only.
        assert_eq!(Corner::TopRight.margins(false).0, BAR_MARGIN);
        assert_eq!(Corner::TopRight.margins(true).0, EDGE_MARGIN);
        assert_eq!(Corner::BottomLeft.margins(true).2, BAR_MARGIN);
    }

    #[test]
    fn cards_narrow_with_the_output() {
        assert_eq!(fit_width(None), WIDTH);
        assert_eq!(fit_width(Some(1920.0)), WIDTH);
        assert!(fit_width(Some(300.0)) < WIDTH);
        assert_eq!(fit_width(Some(100.0)), MIN_WIDTH);
    }

    /// Swiping carries a card toward the nearest side edge.
    #[test]
    fn a_left_corner_swipes_left() {
        let mut a = app();
        a.corner = Corner::TopLeft;
        post(&mut a.toasts, grouped(1, ""));
        let _ = update(&mut a, Message::Press(1));
        let _ = update(&mut a, Message::Pointer(1, 200.0));
        let _ = update(&mut a, Message::Pointer(1, 100.0));
        assert_eq!(a.drag.map(|d| d.dx), Some(100.0));
    }

    /// The drawer eases open from where it is and shuts at once under reduced
    /// motion; a card's drawer stays drawn while it closes.
    #[test]
    fn the_drawer_eases_and_reduced_motion_snaps() {
        let mut a = app();
        post(&mut a.toasts, grouped(1, ""));
        a.toasts[0].shown = Some(a.now);
        let _ = update(&mut a, Message::Drawer(1));
        assert!(a.animating(), "opening plays");
        assert_eq!(a.drawer_card, Some(1));
        assert!(a.drawer_t() < 1.0);
        let _ = update(&mut a, Message::Drawer(1));
        assert_eq!(a.drawer, None);
        assert_eq!(a.drawer_card, Some(1), "still drawn while it closes");
        a.now += Duration::from_secs(1);
        a.settle_drawer();
        assert_eq!(a.drawer_card, None);
        a.reduce_motion = true;
        let _ = update(&mut a, Message::Drawer(1));
        assert_eq!(a.drawer_t(), 1.0);
        assert!(!a.animating());
        let _ = update(&mut a, Message::Drawer(1));
        assert_eq!(a.drawer_card, None);
    }
}
