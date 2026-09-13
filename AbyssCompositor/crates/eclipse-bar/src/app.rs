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

use crate::conn::Conn;
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
        Err(e) => eprintln!("eclipse-bar: cannot start {LAUNCHER}: {e}"),
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
        eprintln!("eclipse-bar: no desktop entry for {app_id}");
        return;
    };
    if let Err(e) = eclipse_services::apps::launch(entry) {
        eprintln!("eclipse-bar: cannot start {}: {e}", entry.id);
    }
}

/// Compositor events and the clock tick, on one thread.
///
/// `iced::time::every` is not in our feature set, and would not help here
/// anyway: the same thread that blocks on the socket is the one that has to
/// notice the minute roll over.
pub fn subscription(_app: &App) -> Subscription<Message> {
    Subscription::batch([pointer(), surfaces(), compositor()])
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
                                Ok(Some(_)) => {
                                    if !send(&mut sender, Message::Refresh) {
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
            focused: true,
            minimized: false,
            pid: None,
            trust: Trust::Secret,
        };
        assert_eq!(w.label(), "Protected window");
    }
}
