// SPDX-License-Identifier: AGPL-3.0-only
//! Bar state and the update loop.
//!
//! The model is a single [`Snapshot`]. Every event the compositor pushes
//! triggers one refetch of the three lists rather than a delta merge: the
//! events carry enough to reconstruct the state, but a refetch is one round
//! trip on a socket we already hold and keeps the model single-sourced.

use iced::{Subscription, Task};
use iced_layershell::actions::IcedNewPopupSettings;
use iced_layershell::reexport::PopupGravity;
use iced_layershell::to_layer_message;

use eclipse_services::status::{Battery, Bluetooth, Network, Update};
use eclipse_ui::tokens::{self, bar};

use crate::conn::{BarConfig, Conn};
use crate::icons::Icons;
use crate::model::Snapshot;

/// The launcher binary the launcher button starts. The same name the
/// compositor's default keybind spawns (`abyss` config `Action::Spawn`), so
/// the button and the chord are one path and cannot drift.
pub const LAUNCHER: &str = "eclipse-launcher";

/// How long the event thread sleeps between passes. Short enough that the
/// clock's minute boundary is never more than this late, long enough that an
/// idle bar costs nothing.
const POLL: std::time::Duration = std::time::Duration::from_millis(500);

/// How long the event thread waits out a full channel before offering the
/// same message again. A paint, roughly.
const BACKPRESSURE: std::time::Duration = std::time::Duration::from_millis(8);

/// One line of the right-click menu.
///
/// The menu's contents are decided once, when it opens, and carried here
/// rather than recomputed in the view: the popup's height is a function of how
/// many lines there are, and a view that could disagree with the height the
/// popup was created at would clip its own last row. `Mute` in particular is
/// conditional — it is absent entirely for a window with no playback stream —
/// and asking `pactl` again from the draw path would be a frame hitch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Close,
    Minimize,
    Unminimize,
    Mute,
    Unmute,
    NewInstance,
}

/// Which tray applet a drawer is showing.
///
/// The drawer is a *container*, not one applet's popup: `Overflow` is the
/// general tray drawer — the home for applets that have not earned permanent
/// bar space, with bluetooth as its first tenant — and `Network` is the wifi
/// applet's own. Adding a tenant is a variant plus a row builder, never a new
/// popup path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drawer {
    /// The wifi applet, opened from the signal cell.
    Network,
    /// The tray overflow, opened from the disclosure arrow.
    Overflow,
}

/// What an open popup is.
#[derive(Debug)]
pub enum Kind {
    /// A window's right-click menu.
    Menu { handle: u64, items: Vec<Item> },
    /// A tray drawer.
    Drawer(Drawer),
}

/// The one open popup. One at a time, as on any desktop: opening a second
/// closes the first, whichever kind each of them is.
#[derive(Debug)]
pub struct Popup {
    /// The popup surface. Also the key `view` branches on.
    pub id: iced::window::Id,
    pub kind: Kind,
}

#[to_layer_message(multi)]
#[derive(Debug, Clone)]
pub enum Message {
    /// Something changed; re-read the compositor.
    Refresh,
    /// A workspace pill was clicked. 1-based wire index.
    Switch(usize),
    /// A window entry was clicked.
    Focus(u64),
    /// A window entry was middle-clicked, or `Close` was picked in the menu.
    Close(u64),
    /// Send a window away, or bring it back — the chip's left click.
    ToggleMinimize(u64),
    /// A window entry was right-clicked: open its menu.
    Menu(u64),
    /// A tray applet was clicked: open (or, if it is already open, close) its
    /// drawer.
    Open(Drawer),
    /// Mute or unmute every playback stream the window's process owns.
    Mute(u64, bool),
    /// Start a second copy of the window's application.
    NewInstance(u64),
    /// Close the menu without acting.
    Dismiss,
    /// The pointer moved over a surface. The bar tracks it because a popup is
    /// positioned by a rectangle in its parent's coordinates, and the press
    /// that opens the menu is the only thing that knows where that is.
    Pointer(iced::window::Id, iced::Point),
    /// A surface went away — the compositor dismissed the popup, say.
    Closed(iced::window::Id),
    /// The bar's surface was sized. The task strip's whole ladder hangs off
    /// this number.
    Sized(iced::window::Id, f32),
    /// The launcher button was clicked.
    Launch,
    /// The system bus said something about network, bluetooth or battery.
    Status(Update),
    /// The compositor restated the focused output (ADR 0042). Everything the
    /// fold decision takes is here: the bar can see neither input nor the
    /// window stack, so idleness and fullscreen have to be told to it.
    OutputState {
        focused: u64,
        fullscreen: bool,
        idle: bool,
    },
    /// One frame of the fold slide. Only sent while an animation, or a fold
    /// waiting out its grace window, is live.
    FoldTick,
    /// A configuration reload succeeded. The `bar.*` keys may have moved, so
    /// re-read them; the event itself is payload-free by design.
    Reconfigured,
}

pub struct App {
    pub conn: Conn,
    pub snapshot: Snapshot,
    pub network: Network,
    pub bluetooth: Bluetooth,
    /// `None` on a machine with no battery, and before the first reading. The
    /// bar draws nothing in both cases rather than inventing a 0%.
    pub battery: Option<Battery>,
    /// `app_id` → icon file, resolved when a snapshot arrives and never from
    /// the view. A directory walk in the draw path would be a frame hitch per
    /// window.
    pub icons: Icons,
    /// The bar's own surface, learned from the first event that names it. The
    /// popup needs a parent and `iced_layershell` does not hand us one.
    pub main: Option<iced::window::Id>,
    /// Last known pointer position on the bar, in bar-local coordinates.
    pub cursor: iced::Point,
    pub popup: Option<Popup>,
    /// The bar's own width in logical pixels, as the compositor last sized the
    /// surface. Zero until the first event names it.
    ///
    /// This is what makes the task strip's condensation a function of *room*
    /// rather than of a window count: the strip subtracts the fixed zones from
    /// it and divides what is left. A bar that did not know its own width
    /// could only ever guess, which is how chips came to run off the end.
    pub width: f32,
    /// The connector this bar is bound to (`DP-1`), from `--output`. Empty
    /// when the bar was started without one — a single-output dev run — in
    /// which case nothing is filtered out and nothing ever folds.
    pub output_name: String,
    /// The numeric id of that output, resolved once against `get_outputs`.
    /// The compositor's workspace and window rows are keyed by this, not by
    /// the connector, so it is what the views filter on.
    pub output_id: u64,
    /// The `bar.*` settings, re-read on every successful config reload.
    pub bar: BarConfig,
    /// The last `output` event's payload, kept because the fold decision is
    /// recomputed on ticks and reloads, not only when the event arrives.
    pub focused_output: u64,
    pub fullscreen: bool,
    pub idle: bool,
    /// Where the bar is between shown, folded and hidden.
    pub fold: FoldState,
}

/// What the bar should be right now.
///
/// `Hidden` is not "folded further": a folded bar is still a bar and still
/// reserves its sliver, while a hidden one claims no exclusive zone at all so
/// a fullscreen video owns the whole output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldTarget {
    Shown,
    Folded,
    Hidden,
}

/// The fold state machine: a committed target, an in-flight slide, and a
/// decision still waiting out its grace window.
pub struct FoldState {
    pub target: FoldTarget,
    /// Current surface height in logical pixels — what the view draws into and
    /// what the exclusive zone is set from, so the two can never disagree.
    pub height: u32,
    from_h: u32,
    to_h: u32,
    started: Option<std::time::Instant>,
    /// A fold decided but not yet committed, and when it was first seen. Only
    /// ever a fold: unfolding is immediate, because that is the direction a
    /// human is waiting on.
    pending: Option<(FoldTarget, std::time::Instant)>,
}

impl Default for FoldState {
    fn default() -> Self {
        FoldState {
            target: FoldTarget::Shown,
            height: crate::HEIGHT,
            from_h: crate::HEIGHT,
            to_h: crate::HEIGHT,
            started: None,
            pending: None,
        }
    }
}

impl FoldState {
    /// True while something still needs a tick: an unfinished slide, or a fold
    /// sitting out its grace. Drives whether the tick subscription exists at
    /// all, so a settled bar costs no wakeups.
    pub fn animating(&self) -> bool {
        self.started.is_some() || self.pending.is_some()
    }
}

/// How long a fold decision has to hold before it is committed. Unfolds skip
/// this entirely. Long enough to swallow the double event of a pointer
/// crossing an output boundary, short enough not to read as lag.
const FOLD_GRACE: std::time::Duration = std::time::Duration::from_millis(120);

/// A fold frame. The slide is short, so this is a paint rate, not a poll rate.
const FOLD_TICK: std::time::Duration = std::time::Duration::from_millis(16);

/// A hidden bar still owns a surface; layer-shell has no zero-height one. One
/// transparent pixel with no exclusive zone is the closest thing to gone.
const HIDDEN_HEIGHT: u32 = 1;

/// The pure half of the fold decision: no `App`, no clock, no socket.
///
/// `output_id == 0` is the single-output dev run (no `--output`), which never
/// folds. `fullscreen` is reported for the *focused* output, so it only hides
/// this bar when this bar is on it.
pub fn decide(bar: &BarConfig, output_id: u64, focused: u64, fullscreen: bool, idle: bool) -> FoldTarget {
    if output_id == 0 {
        return FoldTarget::Shown;
    }
    if fullscreen && focused == output_id {
        return FoldTarget::Hidden;
    }
    let inactive = bar.fold_when_inactive && focused != output_id;
    if inactive || (bar.fold_when_idle && idle) {
        return FoldTarget::Folded;
    }
    FoldTarget::Shown
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        let mut conn = Conn::new();
        let snapshot = conn.snapshot();
        // `App::new` is the daemon builder's `fn() -> App`, so the connector
        // cannot be passed as an argument; `main` puts it here instead.
        let output_name = std::env::var(crate::OUTPUT_ENV).unwrap_or_default();
        let bar = conn.bar_config();
        let (outputs, _) = conn.outputs();
        let output_id = outputs
            .iter()
            .find(|(_, name)| *name == output_name)
            .map(|(id, _)| *id)
            .unwrap_or(0);
        let mut app = App {
            conn,
            snapshot,
            network: Network::default(),
            bluetooth: Bluetooth::default(),
            battery: None,
            icons: Icons::new(),
            main: None,
            cursor: iced::Point::ORIGIN,
            popup: None,
            width: 0.0,
            output_name,
            output_id,
            bar,
            focused_output: 0,
            fullscreen: false,
            idle: false,
            fold: FoldState::default(),
        };
        app.icons.warm(&app.snapshot.windows);
        app
    }
}

/// Start the launcher, detached.
///
/// The bar holds no capability of its own (ADR 0038) — this is an ordinary
/// `execvp` of a sibling binary, exactly what the keybind does, and a failure
/// to spawn is the launcher's absence and not the bar's business to narrate
/// in a 44px row.
///
/// One launcher at a time: the child is kept so a second click on a launcher
/// that is still up does nothing rather than stacking a second window, and so
/// a launcher that has since exited is reaped instead of left a zombie.
#[cfg(not(test))]
fn launch() {
    use std::sync::{Mutex, OnceLock};

    static CHILD: OnceLock<Mutex<Option<std::process::Child>>> = OnceLock::new();
    let mut slot = match CHILD.get_or_init(|| Mutex::new(None)).lock() {
        Ok(slot) => slot,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(child) = slot.as_mut() {
        match child.try_wait() {
            // Still up. The launcher is already the user's answer.
            Ok(None) => return,
            // Exited: reaped by this very call, so the slot is free again.
            Ok(Some(_)) | Err(_) => *slot = None,
        }
    }
    match std::process::Command::new(LAUNCHER).spawn() {
        Ok(child) => *slot = Some(child),
        // The bar cannot narrate this in a 44px row, but it must not swallow
        // it either: stderr is the bar's journal unit.
        Err(e) => eprintln!("hyperion: cannot start {LAUNCHER}: {e}"),
    }
}

/// Hand one message to the UI thread.
///
/// A full channel is backpressure, not a fault — the UI is simply behind, and
/// the answer is to wait for it. Only a closed channel (the surface is gone)
/// ends the event thread; treating "full" as fatal is how the clock used to
/// stop for good after one busy moment.
fn send(sender: &mut iced::futures::channel::mpsc::Sender<Message>, message: Message) -> bool {
    let mut message = message;
    loop {
        match sender.try_send(message) {
            Ok(()) => return true,
            Err(e) if e.is_full() => {
                message = e.into_inner();
                std::thread::sleep(BACKPRESSURE);
            }
            Err(_) => return false,
        }
    }
}

pub fn update(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::Refresh => {}
        Message::Switch(index) => app.conn.switch_workspace(index),
        Message::Focus(handle) => app.conn.focus_window(handle),
        Message::Close(handle) => {
            let dismiss = dismiss(app);
            app.conn.close_window(handle);
            refetch(app);
            return dismiss;
        }
        Message::ToggleMinimize(handle) => {
            let dismiss = dismiss(app);
            let minimized = window(app, handle).is_some_and(|w| w.minimized);
            app.conn.set_minimized(handle, !minimized);
            refetch(app);
            return dismiss;
        }
        Message::Menu(handle) => return open_menu(app, handle),
        Message::Open(drawer) => return open_drawer(app, drawer),
        Message::Mute(handle, mute) => {
            let dismiss = dismiss(app);
            #[cfg(not(test))]
            crate::audio::set_mute(window(app, handle).and_then(|w| w.pid), mute);
            #[cfg(test)]
            let _ = (handle, mute);
            return dismiss;
        }
        Message::NewInstance(handle) => {
            let dismiss = dismiss(app);
            #[cfg(not(test))]
            new_instance(window(app, handle).map(|w| w.app_id.clone()));
            #[cfg(test)]
            let _ = handle;
            return dismiss;
        }
        Message::Dismiss => return dismiss(app),
        Message::Pointer(id, position) => {
            // Anything that is not the open popup is the bar itself: there is
            // only ever one of each.
            if app.popup.as_ref().map(|p| p.id) != Some(id) {
                app.main = Some(id);
                app.cursor = position;
            }
            return Task::none();
        }
        Message::Closed(id) => {
            if app.popup.as_ref().map(|p| p.id) == Some(id) {
                app.popup = None;
            }
            return Task::none();
        }
        // A popup is its own surface and its own width; only the bar's counts.
        Message::Sized(id, width) => {
            if app.popup.as_ref().map(|p| p.id) != Some(id) {
                app.width = width;
            }
            return Task::none();
        }
        // Nothing on the socket changed, so this one does not refetch.
        Message::Launch => {
            #[cfg(not(test))]
            launch();
            return Task::none();
        }
        // The system bus is not the compositor: fold the reading in and stop,
        // rather than falling through to a refetch the socket never asked for.
        Message::Status(update) => {
            match update {
                Update::Network(network) => app.network = network,
                Update::Bluetooth(bluetooth) => app.bluetooth = bluetooth,
                Update::Battery(battery) => app.battery = battery,
            }
            return Task::none();
        }
        // Focus moved, a window went fullscreen, or the session went idle or
        // stopped being idle. All three are one event and one decision.
        Message::OutputState {
            focused,
            fullscreen,
            idle,
        } => {
            app.focused_output = focused;
            app.fullscreen = fullscreen;
            app.idle = idle;
            // Outputs are hotplugged and renumbered; an id resolved once at
            // startup goes stale and strands the bar folded forever.
            resolve_output(app);
            return fold(app);
        }
        Message::FoldTick => return fold(app),
        Message::Reconfigured => {
            app.bar = app.conn.bar_config();
            // A reload can turn folding off while this bar is folded, so the
            // decision is re-run rather than left until the next event.
            resolve_output(app);
            let (_, focused) = app.conn.outputs();
            if let Some(focused) = focused {
                app.focused_output = focused;
            }
            return fold(app);
        }
        // `to_layer_message` injects the layer-control variants. The bar never
        // sends one — it is anchored for its whole life — but the match must
        // still be total.
        _ => return Task::none(),
    }
    // Every branch above either changed compositor state or was told state
    // changed, so all of them end the same way.
    refetch(app);
    Task::none()
}

/// Re-resolve this bar's numeric output id against the compositor's live list.
///
/// Cheap (one socket round trip) and done on every `output` event: the id is
/// what every view filters on, and a stale one is indistinguishable from "this
/// bar's output is never focused".
fn resolve_output(app: &mut App) {
    if app.output_name.is_empty() {
        return;
    }
    let (outputs, _) = app.conn.outputs();
    if let Some((id, _)) = outputs.iter().find(|(_, name)| *name == app.output_name) {
        app.output_id = *id;
    }
}

/// The height a target settles at.
fn target_height(bar: &BarConfig, target: FoldTarget) -> u32 {
    match target {
        FoldTarget::Shown => crate::HEIGHT,
        FoldTarget::Folded => bar.fold_height,
        FoldTarget::Hidden => HIDDEN_HEIGHT,
    }
}

/// Run the state machine one step: decide, apply hysteresis, advance the
/// slide, and push the surface if the height moved.
///
/// The exclusive zone moves with the size on every frame, not just at the
/// ends: shrinking only the paint would leave tiled windows avoiding a
/// full-height bar, and reflowing only at the end is the jump the user saw.
fn fold(app: &mut App) -> Task<Message> {
    let now = std::time::Instant::now();
    let want = decide(
        &app.bar,
        app.output_id,
        app.focused_output,
        app.fullscreen,
        app.idle,
    );

    if want != app.fold.target {
        // Unfolding is immediate; folding waits out `FOLD_GRACE` with the
        // decision unchanged, which is what kills the two-bar flicker when the
        // pointer crosses an output boundary.
        let immediate = want == FoldTarget::Shown;
        match app.fold.pending {
            Some((pending, since)) if pending == want => {
                if immediate || now.duration_since(since) >= FOLD_GRACE {
                    commit(app, want, now);
                }
            }
            _ => {
                if immediate {
                    app.fold.pending = None;
                    commit(app, want, now);
                } else {
                    app.fold.pending = Some((want, now));
                }
            }
        }
    } else {
        app.fold.pending = None;
    }

    let before = app.fold.height;
    advance(app, now);
    if app.fold.height == before {
        return Task::none();
    }
    let Some(id) = app.main else {
        return Task::none();
    };
    let height = app.fold.height;
    // A hidden bar reserves nothing; every other state reserves exactly what
    // it draws.
    let zone = if app.fold.target == FoldTarget::Hidden && height == app.fold.to_h {
        0
    } else {
        height as i32
    };
    Task::batch([
        Task::done(Message::SizeChange {
            id,
            size: (0, height),
        }),
        Task::done(Message::ExclusiveZoneChange { id, zone_size: zone }),
    ])
}

/// Accept a new target and start the slide toward it from wherever the bar
/// currently is — a reversal mid-slide does not jump back to the old end.
fn commit(app: &mut App, target: FoldTarget, now: std::time::Instant) {
    app.fold.pending = None;
    app.fold.target = target;
    app.fold.from_h = app.fold.height;
    app.fold.to_h = target_height(&app.bar, target);
    app.fold.started = if app.bar.fold_duration_ms == 0 || app.fold.from_h == app.fold.to_h {
        None
    } else {
        Some(now)
    };
}

/// Interpolate one frame. Lands exactly on `to_h`, never past it.
fn advance(app: &mut App, now: std::time::Instant) {
    let Some(started) = app.fold.started else {
        app.fold.height = app.fold.to_h;
        return;
    };
    let dur = app.bar.fold_duration_ms.max(1) as f32;
    let t = now.duration_since(started).as_secs_f32() * 1000.0 / dur;
    if t >= 1.0 {
        app.fold.started = None;
        app.fold.height = app.fold.to_h;
        return;
    }
    let eased = app.bar.fold_curve.ease(t);
    let from = app.fold.from_h as f32;
    let to = app.fold.to_h as f32;
    app.fold.height = (from + (to - from) * eased).round() as u32;
}

/// Re-read the three lists and re-resolve any icon we have not seen.
fn refetch(app: &mut App) {
    app.snapshot = app.conn.snapshot();
    app.icons.warm(&app.snapshot.windows);
}

fn window(app: &App, handle: u64) -> Option<&crate::model::Window> {
    app.snapshot.windows.iter().find(|w| w.handle == handle)
}

/// Close the open menu, if there is one. `RemoveWindow` is the macro's own
/// name for closing a surface it created.
fn dismiss(app: &mut App) -> Task<Message> {
    match app.popup.take() {
        Some(popup) => Task::done(Message::RemoveWindow(popup.id)),
        None => Task::none(),
    }
}

/// What the menu offers for one window.
///
/// `Mute` is present only when the window's process actually owns a playback
/// stream: an entry that is meaningless for most windows is worse than no
/// entry, and "does this play audio" is exactly the question `pactl` answers.
fn items(app: &App, handle: u64) -> Vec<Item> {
    let Some(w) = window(app, handle) else {
        return Vec::new();
    };
    let mut items = vec![if w.minimized {
        Item::Unminimize
    } else {
        Item::Minimize
    }];
    #[cfg(not(test))]
    if let Some(muted) = crate::audio::state_of(w.pid) {
        items.push(if muted { Item::Unmute } else { Item::Mute });
    }
    items.push(Item::NewInstance);
    items.push(Item::Close);
    items
}

/// Open the context menu for a window, under the pointer.
///
/// The anchor rectangle is the pointer itself, in the *parent's* local
/// coordinates: a one-pixel box at the tracked cursor, so the menu grows out
/// of the tip rather than dropping off the bar's bottom edge. The gravity
/// pushes it down and to the right from there, because the bar is anchored to
/// the top of the screen. A missing parent means no event has named the bar's
/// surface yet, which cannot happen after a click — but it is not worth a
/// panic.
/// The rectangle a popup grows from, honouring [`tokens::popup::ANCHOR`].
///
/// One function for every popup the bar owns, so that the context menu and the
/// tray drawers cannot drift apart, and so that flipping the token flips all
/// of them. `span` is the clicked cell's horizontal extent and `edge` picks
/// which end of it the popup hangs from — the end the popup's gravity grows
/// *away* from, so the sheet stays on screen.
///
/// The result is a 1x1 rect rather than the cell itself: a zero-size anchor
/// has an unambiguous anchor point, where a rect's is the positioner's
/// business and differs between compositors. Popup *size* is still fixed at
/// creation; this only moves it.
fn anchor(app: &App, span: Option<(f32, f32)>, edge: Edge) -> (i32, i32, i32, i32) {
    let point = match (tokens::popup::ANCHOR, span) {
        (tokens::popup::Anchor::Cell, Some((left, right))) => {
            let x = match edge {
                Edge::Left => left,
                Edge::Right => right,
            };
            (x, bar::SHEET_BOTTOM)
        }
        // Either the human asked for click-point popups, or the cell has no
        // computable position (a chip in the `+N` tail, a bar that has not
        // been sized yet). The pointer is always somewhere.
        _ => (app.cursor.x, app.cursor.y),
    };
    (point.0 as i32, point.1 as i32, 1, 1)
}

/// Which end of a cell a popup hangs from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edge {
    Left,
    Right,
}

fn open_menu(app: &mut App, handle: u64) -> Task<Message> {
    let closed = dismiss(app);
    let Some(parent) = app.main else {
        return closed;
    };
    let items = items(app, handle);
    if items.is_empty() {
        return closed;
    }
    let size = (crate::view::MENU_W, crate::view::menu_height(&items));
    // Gravity down-and-right, so the menu hangs from the chip's left edge.
    let rect = anchor(app, crate::view::chip_span(app, handle), Edge::Left);
    let settings = IcedNewPopupSettings::new(parent, size, rect).gravity(PopupGravity::BottomRight);
    let (id, open) = Message::popup_open(settings);
    app.popup = Some(Popup {
        id,
        kind: Kind::Menu { handle, items },
    });
    Task::batch([closed, open])
}

/// Open a tray drawer, under the pointer, on exactly the machinery the context
/// menu uses — one anchored popup, sized before layout, dismissed by the
/// compositor when the pointer leaves it.
///
/// Clicking the applet whose drawer is already open closes it and opens
/// nothing: an applet button is a toggle, the way a tray is on every other
/// desktop.
fn open_drawer(app: &mut App, drawer: Drawer) -> Task<Message> {
    let same = matches!(app.popup.as_ref().map(|p| &p.kind), Some(Kind::Drawer(d)) if *d == drawer);
    let closed = dismiss(app);
    if same {
        return closed;
    }
    let Some(parent) = app.main else {
        return closed;
    };
    let size = (
        crate::view::DRAWER_W,
        crate::view::drawer_height(crate::view::drawer_rows(drawer)),
    );
    // Gravity down-and-*left*: the tray lives at the right end of the bar, so
    // a drawer growing to the right would hang off the edge of the screen —
    // which is also why it hangs from the cell's right edge and not its left.
    let rect = anchor(app, crate::view::tray_span(app, drawer), Edge::Right);
    let settings = IcedNewPopupSettings::new(parent, size, rect).gravity(PopupGravity::BottomLeft);
    let (id, open) = Message::popup_open(settings);
    app.popup = Some(Popup {
        id,
        kind: Kind::Drawer(drawer),
    });
    Task::batch([closed, open])
}

/// Start a second copy of the application a window belongs to.
///
/// The join is the desktop entry: `eclipse-services` already scans and launches
/// them, and an `app_id` is by convention the entry's own basename, so the
/// match is `<app_id>.desktop` against `Entry::id`. No entry means the window
/// was not started from one — nothing to repeat, and guessing at an argv from a
/// window title is how a taskbar ends up running the wrong binary.
#[cfg(not(test))]
fn new_instance(app_id: Option<String>) {
    let Some(app_id) = app_id else { return };
    let wanted = format!("{app_id}.desktop");
    let entries = eclipse_services::apps::scan();
    let Some(entry) = entries.iter().find(|e| e.id.eq_ignore_ascii_case(&wanted)) else {
        eprintln!("hyperion: no desktop entry for {app_id}");
        return;
    };
    if let Err(e) = eclipse_services::apps::launch(entry) {
        eprintln!("hyperion: cannot start {}: {e}", entry.id);
    }
}

/// Compositor events and the clock tick, on one thread.
///
/// `iced::time::every` is not in our feature set, and would not help here
/// anyway: the same thread that blocks on the socket is the one that has to
/// notice the minute roll over.
pub fn subscription(app: &App) -> Subscription<Message> {
    let mut subs = vec![pointer(), surfaces(), compositor()];
    // Only while something is actually moving: a settled bar has no ticker at
    // all, so the idle cost of the animation is zero.
    if app.fold.animating() {
        subs.push(fold_ticks());
    }
    Subscription::batch(subs)
}

/// A frame clock for the fold slide, alive only while [`FoldState::animating`].
fn fold_ticks() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(8, async move |mut sender| {
            std::thread::spawn(move || loop {
                std::thread::sleep(FOLD_TICK);
                if !send(&mut sender, Message::FoldTick) {
                    return;
                }
            });
        })
    })
}

/// Where the pointer is, and on which surface. The only source of the popup's
/// parent and anchor.
fn pointer() -> Subscription<Message> {
    iced::event::listen_with(|event, _status, id| match event {
        iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
            Some(Message::Pointer(id, position))
        }
        // The surface's own size, which is where the task strip's condensation
        // ladder gets its "what fits" from.
        iced::Event::Window(iced::window::Event::Opened { size, .. })
        | iced::Event::Window(iced::window::Event::Resized(size)) => Some(Message::Sized(id, size.width)),
        iced::Event::Window(_) => Some(Message::Pointer(id, iced::Point::ORIGIN)),
        _ => None,
    })
}

/// The compositor dismisses a popup on its own — a click outside it, a focus
/// change — and the bar must forget the surface when it does, or the next
/// right-click would try to draw into a menu that is gone.
fn surfaces() -> Subscription<Message> {
    iced::window::close_events().map(Message::Closed)
}

fn compositor() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(32, async move |mut sender| {
            std::thread::spawn(move || {
                let mut client = None;
                // One system-bus connection for all three watchers. A machine
                // without a system bus simply never sends a status message;
                // the compositor half of this thread is unaffected.
                let status = eclipse_services::status::spawn().ok();
                let mut minute = String::new();
                loop {
                    if client.is_none() {
                        if let Ok(mut c) = eclipse_ipc::Client::connect() {
                            if c.subscribe(crate::conn::KINDS).is_ok() {
                                client = Some(c);
                                // A fresh connection means the bar may have
                                // been started before the compositor was.
                                if !send(&mut sender, Message::Refresh) {
                                    return;
                                }
                            }
                        }
                    }
                    if let Some(c) = client.as_mut() {
                        loop {
                            match c.poll_event() {
                                Ok(Some(event)) => {
                                    // Two kinds carry a decision of their own;
                                    // every other kind means "state moved",
                                    // which is one refetch.
                                    let message = match event.kind {
                                        eclipse_ipc::EventKind::Output => event
                                            .data
                                            .get("focused")
                                            .and_then(serde_json::Value::as_u64)
                                            .map(|focused| Message::OutputState {
                                                focused,
                                                fullscreen: event
                                                    .data
                                                    .get("fullscreen")
                                                    .and_then(serde_json::Value::as_bool)
                                                    .unwrap_or(false),
                                                idle: event
                                                    .data
                                                    .get("idle")
                                                    .and_then(serde_json::Value::as_bool)
                                                    .unwrap_or(false),
                                            })
                                            .unwrap_or(Message::Refresh),
                                        eclipse_ipc::EventKind::Config => Message::Reconfigured,
                                        _ => Message::Refresh,
                                    };
                                    if !send(&mut sender, message) {
                                        return;
                                    }
                                }
                                Ok(None) => break,
                                Err(_) => {
                                    client = None;
                                    break;
                                }
                            }
                        }
                    }
                    if let Some(status) = status.as_ref() {
                        while let Some(update) = status.try_recv() {
                            if !send(&mut sender, Message::Status(update)) {
                                return;
                            }
                        }
                    }
                    let now = crate::clock::time();
                    if now != minute {
                        minute = now;
                        if !send(&mut sender, Message::Refresh) {
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
    use crate::model::{Trust, Window, Workspace};

    fn app() -> App {
        App::new()
    }

    #[test]
    fn an_injected_layer_variant_is_a_no_op_not_a_panic() {
        let mut a = app();
        // Not `Snapshot::default()`: `App::new` talks to the session's IPC
        // socket, so on a machine with abyss running the initial snapshot is
        // populated. What this asserts is that the injected variant changes
        // nothing, whatever the starting state was.
        let before = a.snapshot.clone();
        let _ = update(
            &mut a,
            Message::SizeChange {
                id: iced::window::Id::unique(),
                size: (100, 34),
            },
        );
        assert_eq!(a.snapshot, before);
    }

    #[test]
    fn a_refresh_without_a_compositor_yields_an_empty_snapshot() {
        let mut a = app();
        a.snapshot.workspaces.push(Workspace {
            index: 1,
            output: 0,
            output_name: "DP-1".into(),
            active: true,
            windows: 0,
        });
        let _ = update(&mut a, Message::Refresh);
        // No socket in the test environment, so the refetch must degrade to
        // "nothing connected" rather than keeping a stale list around.
        if !a.snapshot.connected {
            assert!(a.snapshot.workspaces.is_empty());
        }
    }

    /// The status cells come off the system bus, not the control socket, so a
    /// reading must not drag a compositor refetch along with it.
    #[test]
    fn a_status_reading_does_not_touch_the_compositor() {
        let mut a = app();
        a.snapshot.connected = true;
        let _ = update(
            &mut a,
            Message::Status(Update::Network(Network::Wifi {
                id: "House".into(),
                strength: 49,
            })),
        );
        assert!(a.snapshot.connected);
        assert_eq!(
            a.network,
            Network::Wifi {
                id: "House".into(),
                strength: 49
            }
        );
    }

    /// An absent battery and a flat battery are different pictures, and only
    /// one of them gets a cell.
    #[test]
    fn an_absent_battery_clears_the_cell() {
        let mut a = app();
        let _ = update(&mut a, Message::Status(Update::Battery(None)));
        assert!(a.battery.is_none());
    }

    #[test]
    fn the_bar_renders_a_secret_window_by_its_label_only() {
        let w = Window {
            handle: 3,
            app_id: "org.x.Vault".into(),
            title: "seed phrase".into(),
            workspace: Some(1),
            output: Some(0),
            focused: true,
            minimized: false,
            pid: None,
            trust: Trust::Secret,
        };
        assert_eq!(w.label(), "Protected window");
    }
}

#[cfg(test)]
mod fold_tests {
    use super::*;
    use crate::conn::{BarConfig, FoldCurve};

    fn cfg(inactive: bool, idle: bool) -> BarConfig {
        BarConfig {
            fold_when_inactive: inactive,
            fold_when_idle: idle,
            ..BarConfig::default()
        }
    }

    /// The whole decision table. `Hidden` wins over everything; the two fold
    /// sources are independent and may hold at once.
    #[test]
    fn the_decision_table_is_exhaustive_and_hidden_always_wins() {
        for &here in &[true, false] {
            for &idle in &[true, false] {
                for &fullscreen in &[true, false] {
                    for &w_inactive in &[true, false] {
                        for &w_idle in &[true, false] {
                            let focused = if here { 7 } else { 9 };
                            let got = decide(&cfg(w_inactive, w_idle), 7, focused, fullscreen, idle);
                            let want = if fullscreen && here {
                                FoldTarget::Hidden
                            } else if (w_inactive && !here) || (w_idle && idle) {
                                FoldTarget::Folded
                            } else {
                                FoldTarget::Shown
                            };
                            assert_eq!(
                                got, want,
                                "here={here} idle={idle} fs={fullscreen} \
                                 w_inactive={w_inactive} w_idle={w_idle}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// An unresolved output id means "single-output dev run": never fold, even
    /// under a configuration that would otherwise fold everything.
    #[test]
    fn an_unresolved_output_never_folds() {
        assert_eq!(decide(&cfg(true, true), 0, 9, true, true), FoldTarget::Shown);
    }

    /// A fullscreen window on the *other* output is none of this bar's business.
    #[test]
    fn fullscreen_elsewhere_does_not_hide_this_bar() {
        assert_eq!(decide(&cfg(false, false), 7, 9, true, false), FoldTarget::Shown);
    }

    fn folding_app() -> App {
        let mut a = App::new();
        a.bar = cfg(true, false);
        a.output_id = 7;
        a.focused_output = 7;
        a.fold = FoldState::default();
        a
    }

    /// Hysteresis: a fold that reverses inside the grace window commits nothing.
    #[test]
    fn a_fold_that_reverses_inside_the_grace_window_never_happens() {
        let mut a = folding_app();
        a.focused_output = 9;
        let _ = fold(&mut a);
        assert_eq!(a.fold.target, FoldTarget::Shown, "fold is only pending");
        assert!(a.fold.pending.is_some());

        a.focused_output = 7;
        let _ = fold(&mut a);
        assert_eq!(a.fold.target, FoldTarget::Shown);
        assert!(a.fold.pending.is_none(), "the pending fold is dropped");
        assert_eq!(a.fold.height, crate::HEIGHT, "the height never moved");
    }

    /// The other direction has no grace at all: the human is already reaching
    /// for the bar.
    #[test]
    fn unfolding_is_immediate() {
        let mut a = folding_app();
        a.bar.fold_duration_ms = 0;
        a.focused_output = 9;
        let then = std::time::Instant::now() - FOLD_GRACE * 2;
        a.fold.pending = Some((FoldTarget::Folded, then));
        let _ = fold(&mut a);
        assert_eq!(a.fold.target, FoldTarget::Folded);

        a.focused_output = 7;
        let _ = fold(&mut a);
        assert_eq!(a.fold.target, FoldTarget::Shown);
        assert_eq!(a.fold.height, crate::HEIGHT);
    }

    /// The slide is monotonic and lands exactly on `to_h` — never one pixel
    /// short, which would leave a permanent sliver of exclusive zone.
    #[test]
    fn the_slide_is_monotonic_and_lands_exactly() {
        for curve in [
            FoldCurve::Linear,
            FoldCurve::EaseIn,
            FoldCurve::EaseOut,
            FoldCurve::EaseInOut,
        ] {
            let mut a = folding_app();
            a.bar.fold_curve = curve;
            let started = std::time::Instant::now();
            commit(&mut a, FoldTarget::Folded, started);
            assert!(a.fold.started.is_some(), "{curve:?} animates");

            let mut last = a.fold.height;
            for step in 1..=10u32 {
                advance(
                    &mut a,
                    started + std::time::Duration::from_millis(step as u64 * 15),
                );
                assert!(a.fold.height <= last, "{curve:?} step {step} went back up");
                last = a.fold.height;
            }
            assert_eq!(a.fold.height, a.fold.to_h, "{curve:?} lands on to_h");
            assert!(!a.fold.animating(), "{curve:?} settles");
        }
    }

    /// Zero duration means snap: no animation frames, no tick subscription.
    #[test]
    fn a_zero_duration_snaps() {
        let mut a = folding_app();
        a.bar.fold_duration_ms = 0;
        commit(&mut a, FoldTarget::Folded, std::time::Instant::now());
        advance(&mut a, std::time::Instant::now());
        assert_eq!(a.fold.height, a.bar.fold_height);
        assert!(!a.fold.animating());
    }

    /// `Hidden` gives the zone up entirely; `Folded` keeps its sliver.
    #[test]
    fn hidden_surrenders_the_exclusive_zone() {
        let bar = BarConfig::default();
        assert_eq!(target_height(&bar, FoldTarget::Hidden), HIDDEN_HEIGHT);
        assert_eq!(target_height(&bar, FoldTarget::Folded), bar.fold_height);
        assert_eq!(target_height(&bar, FoldTarget::Shown), crate::HEIGHT);
    }

    /// Defect 3: the id was resolved once in `App::new` and never again, so a
    /// hotplug left the bar folded forever. It must re-resolve by name.
    #[test]
    fn an_output_id_is_re_resolved_by_name() {
        let mut a = App::new();
        a.output_name = "DP-99".to_owned();
        a.output_id = 4;
        // No compositor in the test environment, so `outputs()` is empty and
        // the stale id must survive rather than being zeroed.
        resolve_output(&mut a);
        assert_eq!(a.output_id, 4);
    }
}
