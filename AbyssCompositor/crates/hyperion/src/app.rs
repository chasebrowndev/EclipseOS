// SPDX-License-Identifier: AGPL-3.0-only
//! Bar state and the update loop.
//!
//! The model is a single [`Snapshot`]. Every event the compositor pushes
//! triggers one refetch of the three lists rather than a delta merge: the
//! events carry enough to reconstruct the state, but a refetch is one round
//! trip on a socket we already hold and keeps the model single-sourced.
//!
//! One process draws a bar on every output. [`App`] holds what every bar
//! shares — the snapshot, the services' readings, the configuration — and
//! [`App::bars`] one [`Bar`] per output: its surface, fold, layout and
//! motion. The set follows the compositor's output list ([`reconcile`]).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use iced::window::Id;
use iced::{Subscription, Task};
use iced_layershell::actions::IcedNewPopupSettings;
use iced_layershell::reexport::PopupGravity;
use iced_layershell::to_layer_message;

use eclipse_services::status::{Battery, Bluetooth, Network, Update};
use eclipse_ui::tokens::{self, bar};

use crate::conn::{BarConfig, BarPosition, Conn};
use crate::icons::Icons;
use crate::layout::{self, Pin};
use crate::model::Snapshot;
use crate::motion::{settle, Drag};
use crate::widgets::{self, GripEv};

/// The launcher binary the launcher button starts. The same name the
/// compositor's default keybind spawns (`abyss` config `Action::Spawn`), so
/// the button and the chord are one path and cannot drift.
pub const LAUNCHER: &str = "eclipse-launcher";

/// The longest the event thread waits between passes when the compositor is
/// quiet. Short enough that the clock's minute boundary is never more than
/// this late, long enough that an idle bar costs nothing. A compositor event
/// ends the wait at once.
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
/// and asking the audio service again from the draw path would be a frame hitch.
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
/// general tray drawer — the home for every tray entry that is neither pinned
/// to the bar nor hidden — and `Network` and `Bluetooth` are the two radios'
/// own. Adding a tenant is a variant plus a body builder, never a new popup
/// path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drawer {
    /// The wifi applet, opened from the signal cell.
    Network,
    /// The bluetooth applet.
    Bluetooth,
    /// The tray overflow, opened from the disclosure arrow.
    Overflow,
}

/// What an open popup is.
#[derive(Debug)]
pub enum Kind {
    /// A window's right-click menu.
    Menu { handle: u64, items: Vec<Item> },
    /// A tray drawer, and the height its surface was created at. The view
    /// draws from live data; when that data would need a different height the
    /// drawer is reopened at the new one rather than clipped (see [`reflow`]).
    Drawer { which: Drawer, height: u32 },
    /// A StatusNotifierItem's own menu, fetched once when it opened.
    TrayMenu {
        id: String,
        entries: Vec<crate::radio::MenuEntry>,
    },
}

/// The one open popup. One at a time, as on any desktop: opening a second
/// closes the first, whichever kind each of them is and whichever bar it
/// hangs from.
#[derive(Debug)]
pub struct Popup {
    /// The popup surface. Also the key `view` branches on.
    pub id: Id,
    /// The bar it hangs from: its parent surface, and the bar a message from
    /// inside it acts for.
    pub owner: Id,
    pub kind: Kind,
}

#[to_layer_message(multi)]
#[derive(Debug, Clone)]
pub enum Message {
    /// Something changed; re-read the compositor.
    Refresh,
    /// A workspace pill was clicked. 1-based wire index.
    Switch(usize),
    /// A window chip was clicked while its window is up but not focused:
    /// bring it forward rather than send it away.
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
    /// Close the menu without acting: Escape, or a press on the bar that
    /// no cell took ([`Message::BarPress`]).
    Dismiss,
    /// A press on a surface that no widget took, and where it landed when it
    /// was a finger. On the bar it closes the open popup: abyss counts the
    /// parent bar as inside the popup's grab (`shell::popup_grab_button_press`),
    /// so the compositor will not dismiss it for us.
    BarPress(iced::window::Id, Option<iced::Point>),
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
    /// Oracle-Eyes' beacon moved (ADR 0055). Only sent while `bar.eye` is on.
    Eye(crate::eye::Eye),
    /// One frame of the eclipse mark's pupil, or the end of a hold between
    /// darts. Only sent while the pupil is moving or waiting to.
    EyeTick,
    /// A configuration reload succeeded. The `bar.*` keys may have moved, so
    /// re-read them; the event itself is payload-free by design.
    Reconfigured,
    /// The wi-fi drawer's switch.
    WifiEnable(bool),
    /// A network row was clicked. A secured network with no saved profile
    /// hands off to `eclipse-secret-prompt` instead of joining.
    WifiConnect(String),
    /// The current network's Disconnect.
    WifiDisconnect,
    /// The bluetooth drawer's switch.
    BtPower(bool),
    /// Start or stop discovery.
    BtScan,
    /// A device row was clicked: connect a paired device, pair a new one.
    BtConnect(String),
    BtDisconnect(String),
    /// A drawer's way out: start Settings on the named pane.
    OpenSettings(&'static str),
    /// A tray item was left-clicked.
    TrayActivate(String),
    /// A tray item was right-clicked: open its own menu.
    TrayMenu(String),
    /// A line of a tray item's menu was picked.
    TrayMenuClick(String, i32),
    /// The wifi, bluetooth or tray service said something.
    Radio(crate::radio::Feed),
    /// A widget's service reported, or the human used a widget (ADR 0065).
    Widget(widgets::Feed),
    /// The playback streams, which the per-window mute reads.
    Streams(Vec<eclipse_services::audio::Stream>),
    /// A widget's grip, by widget key.
    Grip(String, GripEv),
    /// One frame of the bar's motion. Only sent while something moves.
    Frame,
    /// Debug previews: the next step of `HYPERION_PREVIEW_SCRIPT`.
    Script(u32),
    /// A message a surface's view produced, and that surface. The view is
    /// one function for every bar; this is how a click knows which bar it
    /// was on. See [`owner`].
    On(Id, Box<Message>),
    /// Look at the output list again: an output seen for the first time has
    /// waited out [`SETTLE`].
    Screens,
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
    /// One bar per output, keyed by its layer surface.
    pub bars: HashMap<Id, Bar>,
    /// `--output NAME`: one bar on that connector and no reconciling. A
    /// debug path; the unit runs without it.
    pub pin: Option<String>,
    /// Outputs listed but not yet given a bar, and when each was first
    /// seen. See [`reconcile`].
    pub seen: HashMap<String, Instant>,
    /// A [`Message::Screens`] is already on its way.
    pub checking: bool,
    /// The one open popup, on whichever bar opened it.
    pub popup: Option<Popup>,
    /// The `bar.*` settings, re-read on every successful config reload.
    pub bar: BarConfig,
    /// The edge the surface is anchored to, fixed at startup like the anchor
    /// itself (`bar.position` is `reload: restart`). The float gap is a
    /// layer-shell margin on this edge, so it must not follow a reload that
    /// the anchor did not.
    pub edge: BarPosition,
    /// The last `output` event's payload, kept because the fold decision is
    /// recomputed on ticks and reloads, not only when the event arrives.
    pub focused_output: u64,
    pub fullscreen: bool,
    pub idle: bool,
    /// What the beacon last said, and where that has the pupil right now;
    /// [`Eye::Off`](crate::eye::Eye::Off) while `bar.eye` is off or nothing is
    /// listening.
    pub iris: crate::eye::Iris,
    /// Radio lists and tray items — what the drawers draw beyond the one-line
    /// status feed. See [`crate::radio`].
    pub radios: crate::radio::Radios,
    /// The tray item whose menu a right-click asked for, and the bar it was
    /// asked on. The entries arrive later; they open a sheet only if this
    /// still names the item, so a menu that answers after the user moved on
    /// never pops up.
    pub pending_menu: Option<(Id, String)>,
    /// `bar.tray.*`: which tray entries are pinned, in what order, and which
    /// are hidden.
    pub tray: crate::conn::TrayConfig,
    /// Debug builds only: the popup `HYPERION_PREVIEW` asked for, opened on
    /// the first event that names the bar's surface. Always `None` in release.
    pub preview: Option<Preview>,
    /// The bar sheet's own glass radius, live-synced to `bar.rounding`
    /// (BLUR-06) — distinct from [`menu_radius`] because the bar's corner is
    /// its own setting, not `decoration.rounding`.
    ///
    /// [`menu_radius`]: App::menu_radius
    pub bar_radius: f32,
    /// Every popup's glass radius (a context menu, the tray menu, a drawer),
    /// live-synced to `decoration.rounding` (BLUR-06) — the same value the
    /// rest of the desktop's glass uses, kept apart from [`bar_radius`].
    ///
    /// [`bar_radius`]: App::bar_radius
    pub menu_radius: f32,
    /// `bar.widgets.*`, `bar.motion.*` and the `widget` blocks, re-read on
    /// every successful config reload.
    pub widget_cfg: widgets::Config,
    /// What the widgets' services last said. Shared: every bar draws the
    /// same readings, and a click on any of them acts once.
    pub widgets: widgets::State,
    /// The audio service's playback streams, for the per-window mute.
    pub streams: Vec<eclipse_services::audio::Stream>,
    /// Debug previews only: when a widget fixture started. A fixture bar
    /// neither refetches nor starts services — the fixture stands in for
    /// both.
    pub fixture: Option<Instant>,
}

/// One output's bar: everything that depends on *where* it is drawn.
pub struct Bar {
    /// The bar's layer surface, chosen when it was asked for. Also a popup's
    /// parent.
    pub id: Id,
    /// The connector this bar is bound to (`DP-1`).
    pub output_name: String,
    /// The numeric id of that output, re-resolved by name against
    /// `get_outputs`. The compositor's workspace and window rows are keyed
    /// by this, not by the connector, so it is what the views filter on. `0`
    /// while unresolved: nothing is filtered and nothing folds.
    pub output_id: u64,
    /// Last known pointer position on the bar, in bar-local coordinates.
    pub cursor: iced::Point,
    /// The bar's own width in logical pixels, as the compositor last sized the
    /// surface. Zero until the first event names it.
    ///
    /// This is what makes the task strip's condensation a function of *room*
    /// rather than of a window count: the strip subtracts the fixed zones from
    /// it and divides what is left. A bar that did not know its own width
    /// could only ever guess, which is how chips came to run off the end.
    pub width: f32,
    /// The surface has been configured. A fold is pushed to it only after.
    pub mapped: bool,
    /// Where the bar is between shown, folded and hidden.
    pub fold: FoldState,
    /// The live eye's own layer surface, while there is one. See
    /// [`sync_eye`].
    pub eye_surface: Option<Id>,
    /// The solver's widget inputs as of the last [`relayout`], one per
    /// `widget_cfg.order` entry: what a cell's shell is drawn against.
    pub widget_inputs: Vec<layout::WidgetIn>,
    /// Where everything is going.
    pub layout: layout::Layout,
    /// How far everything has got there.
    pub motion: crate::motion::Bar,
}

impl Bar {
    pub fn new(id: Id, output_name: String, output_id: u64, motion: eclipse_ui::motion::Motion) -> Self {
        let mut m = crate::motion::Bar::default();
        m.set_motion(motion);
        Bar {
            id,
            output_name,
            output_id,
            cursor: iced::Point::ORIGIN,
            width: 0.0,
            mapped: false,
            fold: FoldState::default(),
            eye_surface: None,
            widget_inputs: Vec::new(),
            layout: layout::Layout::default(),
            motion: m,
        }
    }
}

/// A popup a debug preview opens by itself, since nothing can click.
#[derive(Debug, Clone)]
pub enum Preview {
    Drawer(Drawer),
    /// A tray item's menu: the item's address and the fixture entries.
    TrayMenu(String, Vec<crate::radio::MenuEntry>),
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
    to_h: u32,
    /// The slide itself, in fractional pixels so a reversal starts from where
    /// the bar really is rather than from the rounded `height`.
    slide: Slide,
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
            to_h: crate::HEIGHT,
            slide: Slide::Rest,
            pending: None,
        }
    }
}

/// What is moving the fold height.
///
/// A leg that starts from rest follows the configured `bar.fold-curve`, so the
/// key keeps its meaning. A leg that interrupts a slide still in flight — the
/// pointer came back before the fold landed — is handed to an
/// [`Animated`](eclipse_ui::motion::Animated) spring seeded with the speed the
/// bar had, so the reversal has neither a position jump nor a velocity kink.
#[derive(Debug, Clone, Copy)]
enum Slide {
    Rest,
    Curve { from: f32, started: std::time::Instant },
    Spring(eclipse_ui::motion::Animated),
}

/// What the bar's layer surface is asked for: its height, its exclusive zone
/// and its layer-shell margin `(top, right, bottom, left)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub height: u32,
    pub zone: i32,
    pub margin: (i32, i32, i32, i32),
}

impl FoldState {
    /// True when the full pill is drawn: shown, and the slide has landed.
    /// Anything else is the folded strip, whatever its height.
    pub fn pill(&self) -> bool {
        self.target == FoldTarget::Shown && self.height == crate::HEIGHT
    }

    /// The surface this state needs, anchored to `edge`.
    ///
    /// The drawn sheet fills its surface exactly: the compositor blurs the
    /// whole surface at `bar.rounding`, and any air left inside it would show
    /// as a blurred rim around the pill. So the float gap is layer-shell
    /// margin — `MARGIN_X` on both sides always, and `MARGIN_Y` on the
    /// anchored edge only while the pill is up (the folded strip sits flush
    /// against the edge, as it always has).
    ///
    /// wlr-layer-shell adds the anchored edge's margin to the exclusive zone
    /// (abyss: `shell::accumulate_non_exclusive_zone`), so the zone asked for
    /// is the reserved strip less that margin, and tiled windows keep exactly
    /// the gap they had. A hidden bar reserves nothing.
    pub fn geometry(&self, edge: BarPosition) -> Geometry {
        let side = bar::MARGIN_X as i32;
        let (height, air) = if self.pill() {
            (bar::PILL_H as u32, bar::MARGIN_Y as i32)
        } else {
            (self.height, 0)
        };
        let zone = if self.target == FoldTarget::Hidden && self.height == self.to_h {
            0
        } else {
            height as i32 + air
        };
        let margin = match edge {
            BarPosition::Top => (air, side, 0, side),
            BarPosition::Bottom => (0, side, air, side),
        };
        Geometry { height, zone, margin }
    }

    /// True while something still needs a tick: an unfinished slide, or a fold
    /// sitting out its grace. Drives whether the tick subscription exists at
    /// all, so a settled bar costs no wakeups.
    pub fn animating(&self) -> bool {
        !matches!(self.slide, Slide::Rest) || self.pending.is_some()
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
        let bar = conn.bar_config();
        let tray = conn.tray_config();
        let bar_radius = conn
            .glass_radius("bar.rounding")
            .unwrap_or(eclipse_ui::tokens::bar::RADIUS_SHEET);
        let menu_radius = conn
            .glass_radius("decoration.rounding")
            .unwrap_or(eclipse_ui::tokens::radius::CARD);
        // The focused output now, so a bar opened on any other one folds
        // from its first frame rather than on the next output event.
        let (_, focused) = conn.outputs();
        let widget_cfg = conn.widgets_config();
        let mut app = App {
            conn,
            snapshot,
            network: Network::default(),
            bluetooth: Bluetooth::default(),
            battery: None,
            icons: Icons::new(),
            bars: HashMap::new(),
            pin: None,
            seen: HashMap::new(),
            checking: false,
            popup: None,
            edge: bar.position,
            bar,
            focused_output: focused.unwrap_or(0),
            fullscreen: false,
            idle: false,
            iris: crate::eye::Iris::default(),
            radios: crate::radio::Radios::default(),
            pending_menu: None,
            tray,
            preview: None,
            bar_radius,
            menu_radius,
            widget_cfg,
            widgets: widgets::State::default(),
            streams: Vec::new(),
            fixture: None,
        };
        app.icons.warm(&app.snapshot.windows);
        #[cfg(debug_assertions)]
        preview(&mut app);
        if app.fixture.is_none() {
            crate::services::configure(&app.widget_cfg);
        }
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
///
/// The same holds for every sibling the bar starts — Settings from a drawer's
/// link, the secret prompt from a secured network — so the slot is keyed by
/// binary: one of each, never two password prompts stacked.
#[cfg(not(test))]
fn spawn_once(bin: &'static str, args: &[&str]) {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    static CHILDREN: OnceLock<Mutex<HashMap<&'static str, std::process::Child>>> = OnceLock::new();
    let mut slots = match CHILDREN.get_or_init(|| Mutex::new(HashMap::new())).lock() {
        Ok(slots) => slots,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(child) = slots.get_mut(bin) {
        match child.try_wait() {
            // Still up. That window is already the user's answer.
            Ok(None) => return,
            // Exited: reaped by this very call, so the slot is free again.
            Ok(Some(_)) | Err(_) => {
                slots.remove(bin);
            }
        }
    }
    match std::process::Command::new(bin).args(args).spawn() {
        Ok(child) => {
            slots.insert(bin, child);
        }
        // The bar cannot narrate this in a 44px row, but it must not swallow
        // it either: stderr is the bar's journal unit. Only the binary is
        // named — the arguments may carry an SSID.
        Err(e) => eprintln!("hyperion: cannot start {bin}: {e}"),
    }
}

#[cfg(test)]
fn spawn_once(_bin: &'static str, _args: &[&str]) {}

/// The Settings binary a drawer's link starts, with the pane as its argument.
pub const SETTINGS: &str = "eclipse-settings";

/// The password prompt: a separate process whose whole surface is `secret`
/// (ADR 0053), so the passphrase never passes through the bar.
pub const SECRET_PROMPT: &str = "eclipse-secret-prompt";

/// `HYPERION_PREVIEW=network|bluetooth|overflow|traymenu|bar` fills the radios and the
/// tray with a fixture and, for a drawer, opens it as soon as the bar has a
/// surface — so a drawer can be screenshotted on a machine with no service
/// behind it and no way to click. Debug builds only.
#[cfg(debug_assertions)]
fn preview(app: &mut App) {
    let Ok(which) = std::env::var("HYPERION_PREVIEW") else {
        return;
    };
    app.radios = crate::radio::Radios::preview();
    app.network = Network::Wifi {
        id: "abyss-5g".to_owned(),
        strength: 82,
    };
    app.bluetooth = Bluetooth {
        powered: true,
        connected: 1,
    };
    app.preview = match which.as_str() {
        "network" => Some(Preview::Drawer(Drawer::Network)),
        "bluetooth" => Some(Preview::Drawer(Drawer::Bluetooth)),
        "overflow" => Some(Preview::Drawer(Drawer::Overflow)),
        "traymenu" => Some(Preview::TrayMenu(
            "org.syncthing".to_owned(),
            crate::radio::preview_menu(),
        )),
        "widgets" | "widgets-idle" => {
            let output = app.conn.outputs().0.first().map_or(0, |o| o.0);
            crate::preview::widgets(app, which == "widgets-idle", output);
            None
        }
        _ => None,
    };
    if which == "bar" {
        app.tray.pinned = Some(
            [
                "org.syncthing",
                "com.vendor.app",
                "bluetooth",
                "network",
                "battery",
            ]
            .map(str::to_owned)
            .to_vec(),
        );
    }
}

/// Hand one message to the UI thread.
///
/// A full channel is backpressure, not a fault — the UI is simply behind, and
/// the answer is to wait for it. Only a closed channel (the surface is gone)
/// ends the event thread; treating "full" as fatal is how the clock used to
/// stop for good after one busy moment.
pub(crate) fn send(sender: &mut iced::futures::channel::mpsc::Sender<Message>, message: Message) -> bool {
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

/// Sleep until the compositor has something to say, or `timeout` passes.
/// Without a connection there is nothing to wake on, so it is a plain sleep.
fn wait(client: Option<&eclipse_ipc::Client>, timeout: std::time::Duration) {
    let Some(c) = client else {
        std::thread::sleep(timeout);
        return;
    };
    let mut fd = libc::pollfd {
        fd: c.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd on the stack, count 1; the fd outlives the call.
    let n = unsafe { libc::poll(&mut fd, 1, timeout.as_millis() as libc::c_int) };
    if n < 0 {
        // EINTR or worse: do not spin.
        std::thread::sleep(BACKPRESSURE);
    }
}

/// How long an output seen for the first time waits before it gets a bar.
///
/// A bar asks for its surface by connector name, which the toolkit resolves
/// against the `xdg_output` names it has been told. A hotplugged output's
/// name arrives a round trip after the output itself; a surface asked for
/// before then lands on whichever output is focused — a second bar there and
/// none on the new one. At boot the names are already in, so nothing waits.
pub const SETTLE: Duration = Duration::from_millis(250);

/// What to do about the output list. See [`reconcile`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Connectors to open a bar on.
    pub open: Vec<String>,
    /// Bars whose output is gone.
    pub close: Vec<Id>,
    /// An output is still settling: look again after [`SETTLE`].
    pub wait: bool,
}

/// The pure half of following the outputs: no `App`, no clock, no socket.
///
/// `open` is the bars there are, by connector; `live` is the compositor's
/// output list; `seen` carries each unbarred output's first sighting from
/// one call to the next, and an output gets its bar once it has been listed
/// for `settle`.
///
/// An empty `live` is no answer — the socket is not up yet, or has just
/// dropped — and never reads as "every monitor went away". A renamed output
/// is its old name's removal and its new name's addition.
pub fn reconcile(
    open: &[(Id, &str)],
    live: &[(u64, String)],
    seen: &mut HashMap<String, Instant>,
    now: Instant,
    settle: Duration,
) -> Plan {
    let mut plan = Plan::default();
    if live.is_empty() {
        return plan;
    }
    let listed = |name: &str| live.iter().any(|(_, n)| n == name);
    for (id, name) in open {
        if !listed(name) {
            plan.close.push(*id);
        }
    }
    // An output that came and went while settling starts over next time.
    seen.retain(|name, _| listed(name));
    for (_, name) in live {
        if open.iter().any(|(_, n)| n == name) || plan.open.contains(name) {
            seen.remove(name);
            continue;
        }
        let first = *seen.entry(name.clone()).or_insert(now);
        if now.saturating_duration_since(first) >= settle {
            seen.remove(name);
            plan.open.push(name.clone());
        } else {
            plan.wait = true;
        }
    }
    plan
}

/// Point every bar at its output's current numeric id. Outputs are
/// hotplugged and renumbered; an id resolved once goes stale and strands the
/// bar folded forever. An empty list is no answer and changes nothing.
fn resolve(bars: &mut HashMap<Id, Bar>, live: &[(u64, String)]) {
    for bar in bars.values_mut() {
        if let Some((id, _)) = live.iter().find(|(_, name)| *name == bar.output_name) {
            bar.output_id = *id;
        }
    }
}

/// The daemon's boot: the shared state, and a bar on every output there is —
/// or, with `--output`, on that one alone.
pub fn boot(pin: Option<String>) -> (App, Task<Message>) {
    let mut app = App::new();
    let task = match pin.clone() {
        Some(name) => {
            let (live, _) = app.conn.outputs();
            let output_id = live.iter().find(|(_, n)| *n == name).map_or(0, |(id, _)| *id);
            open_bar(&mut app, name, output_id)
        }
        None => screens(&mut app, Duration::ZERO),
    };
    app.pin = pin;
    (app, task)
}

/// Bring the bars in line with the compositor's outputs: a bar opens on an
/// output that has settled, and closes on one that has gone, and no other
/// bar is touched.
fn screens(app: &mut App, settle: Duration) -> Task<Message> {
    let (live, _) = app.conn.outputs();
    resolve(&mut app.bars, &live);
    if app.pin.is_some() {
        return Task::none();
    }
    let names: Vec<(Id, String)> = app.bars.values().map(|b| (b.id, b.output_name.clone())).collect();
    let open: Vec<(Id, &str)> = names.iter().map(|(id, n)| (*id, n.as_str())).collect();
    let plan = reconcile(&open, &live, &mut app.seen, Instant::now(), settle);
    let mut tasks = Vec::new();
    for id in plan.close {
        tasks.push(close_bar(app, id));
    }
    for name in plan.open {
        let output_id = live.iter().find(|(_, n)| *n == name).map_or(0, |(id, _)| *id);
        tasks.push(open_bar(app, name, output_id));
    }
    if plan.wait && !app.checking {
        app.checking = true;
        tasks.push(recheck());
    }
    Task::batch(tasks)
}

/// A [`Message::Screens`] after [`SETTLE`]. A thread and a oneshot: the
/// runtime has no timer of its own in our feature set.
fn recheck() -> Task<Message> {
    let (tx, rx) = iced::futures::channel::oneshot::channel::<()>();
    std::thread::spawn(move || {
        std::thread::sleep(SETTLE);
        let _ = tx.send(());
    });
    Task::future(async move {
        let _ = rx.await;
        Message::Screens
    })
}

/// Ask for a bar on `name` and start its state.
///
/// The first frame's geometry is the one every fold step asks for, so the
/// first frame and a settled unfold cannot disagree. The anchor is the edge
/// read at startup (`bar.position` is `reload: restart`), plus both sides.
pub fn open_bar(app: &mut App, name: String, output_id: u64) -> Task<Message> {
    use iced_layershell::reexport::{
        Anchor, KeyboardInteractivity, Layer, NewLayerShellSettings, OutputOption,
    };
    let id = Id::unique();
    let bar = Bar::new(id, name.clone(), output_id, app.widget_cfg.motion);
    let geometry = bar.fold.geometry(app.edge);
    app.bars.insert(id, bar);
    let edge = match app.edge {
        BarPosition::Top => Anchor::Top,
        BarPosition::Bottom => Anchor::Bottom,
    };
    Task::done(Message::NewLayerShell {
        settings: NewLayerShellSettings {
            // Width 0 means "as wide as the output, less the side margins".
            // The pill fills the surface and the float gap is margin (see
            // `FoldState::geometry`); the zone plus the edge margin is the
            // full strip, so a window opened afterwards starts below the bar
            // rather than under it.
            size: Some((0, geometry.height)),
            layer: Layer::Top,
            anchor: edge | Anchor::Left | Anchor::Right,
            exclusive_zone: Some(geometry.zone),
            margin: Some(geometry.margin),
            // The bar is pointer-driven. It must never take the keyboard
            // away from the window the human is typing into.
            keyboard_interactivity: KeyboardInteractivity::None,
            // By name: a null output would be resolved to whichever monitor
            // happens to be focused.
            output_option: OutputOption::OutputName(name),
            events_transparent: false,
            namespace: None,
        },
        id,
    })
}

/// Forget a bar and close its surface, and with it its popup and its eye.
fn close_bar(app: &mut App, id: Id) -> Task<Message> {
    let Some(bar) = app.bars.remove(&id) else {
        return Task::none();
    };
    let mut tasks = vec![Task::done(Message::RemoveWindow(id))];
    tasks.extend(bar.eye_surface.map(|eye| Task::done(Message::RemoveWindow(eye))));
    tasks.push(orphan(app, id));
    Task::batch(tasks)
}

/// Drop what hung from a bar that is gone: its popup and a menu it asked for.
fn orphan(app: &mut App, id: Id) -> Task<Message> {
    if app.pending_menu.as_ref().is_some_and(|(owner, _)| *owner == id) {
        app.pending_menu = None;
    }
    match app.popup.take() {
        Some(p) if p.owner == id => Task::done(Message::RemoveWindow(p.id)),
        other => {
            app.popup = other;
            Task::none()
        }
    }
}

/// The bar a surface belongs to: the bar itself, or the bar its popup or eye
/// hangs from.
pub fn owner(app: &App, id: Id) -> Option<Id> {
    if app.bars.contains_key(&id) {
        return Some(id);
    }
    if let Some(p) = app.popup.as_ref().filter(|p| p.id == id) {
        return Some(p.owner);
    }
    app.bars
        .values()
        .find(|b| b.eye_surface == Some(id))
        .map(|b| b.id)
}

/// Run `f` on every bar. The bars are lifted out of the map for the
/// duration, so `f` may read the rest of the app freely.
fn each_bar(app: &mut App, mut f: impl FnMut(&App, &mut Bar) -> Task<Message>) -> Task<Message> {
    let mut bars = std::mem::take(&mut app.bars);
    let tasks: Vec<Task<Message>> = bars.values_mut().map(|bar| f(app, bar)).collect();
    let added = std::mem::replace(&mut app.bars, bars);
    app.bars.extend(added);
    Task::batch(tasks)
}

/// Run `f` on one bar, lifted out of the map as in [`each_bar`].
fn with_bar<R>(app: &mut App, id: Id, f: impl FnOnce(&App, &mut Bar) -> R) -> Option<R> {
    let mut bar = app.bars.remove(&id)?;
    let r = f(app, &mut bar);
    app.bars.insert(id, bar);
    Some(r)
}

/// Handle one message, then re-solve every bar against whatever it changed.
///
/// Every message ends in [`relayout`]: the solver is pure and cheap, and
/// retargeting an animation at the target it already has is a no-op, so one
/// unconditional pass is simpler than knowing which messages move a cell.
pub fn update(app: &mut App, message: Message) -> Task<Message> {
    let task = step(app, message, None);
    relayout_all(app, Instant::now());
    task
}

/// One message. `at` is the bar it came from, when a view produced it.
fn step(app: &mut App, message: Message, at: Option<Id>) -> Task<Message> {
    match message {
        Message::On(id, inner) => {
            let at = owner(app, id);
            return step(app, *inner, at);
        }
        Message::Refresh => {
            refetch(app);
            return screens(app, SETTLE);
        }
        Message::Screens => {
            app.checking = false;
            return screens(app, SETTLE);
        }
        Message::Switch(index) => app.conn.switch_workspace(index),
        Message::Focus(handle) => {
            let dismiss = dismiss(app);
            app.conn.focus_window(handle);
            refetch(app);
            return dismiss;
        }
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
        Message::Menu(handle) => {
            return match at {
                Some(at) => open_menu(app, at, handle),
                None => Task::none(),
            }
        }
        Message::Open(drawer) => {
            return match at {
                Some(at) => open_drawer(app, at, drawer, true),
                None => Task::none(),
            }
        }
        Message::Mute(handle, mute) => {
            let dismiss = dismiss(app);
            // A browser plays from a child process, so every stream the
            // window's process tree owns is muted, not only its own pid's.
            if let Some(pid) = window(app, handle).and_then(|w| w.pid) {
                for stream in crate::audio::streams_for(&app.streams, pid) {
                    crate::services::set_app_muted(stream.pid, mute);
                }
            }
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
        Message::BarPress(id, point) => {
            if let Some(point) = point {
                let _ = step(app, Message::Pointer(id, point), at);
            }
            // A press inside the popup (its padding, a separator) is not a
            // press outside it; neither is one on an eye. A press on any bar
            // is: there is one popup, whichever bar it hangs from.
            let on_popup = app.popup.as_ref().is_some_and(|p| p.id == id);
            let on_eye = app.bars.values().any(|b| b.eye_surface == Some(id));
            if on_popup || on_eye {
                return Task::none();
            }
            return dismiss(app);
        }
        Message::Pointer(id, position) => {
            // Only a bar's own surface: a popup and an eye have their own
            // coordinates, and a popup is anchored in its bar's.
            if let Some(bar) = app.bars.get_mut(&id) {
                bar.cursor = position;
            }
            return Task::none();
        }
        Message::Closed(id) => {
            if app.bars.contains_key(&id) {
                // The compositor took the bar away — its output went, most
                // likely. Its popup and eye go with it; if the output is in
                // fact still listed, it gets a fresh bar once it settles.
                let closed = close_bar(app, id);
                return Task::batch([closed, screens(app, SETTLE)]);
            }
            if app.popup.as_ref().map(|p| p.id) == Some(id) {
                app.popup = None;
            }
            for bar in app.bars.values_mut() {
                if bar.eye_surface == Some(id) {
                    bar.eye_surface = None;
                }
            }
            return Task::none();
        }
        // A popup is its own surface and its own width, and so is an eye;
        // only a bar's counts.
        Message::Sized(id, width) => {
            let Some(first) = with_bar(app, id, |_, bar| {
                bar.width = width;
                !std::mem::replace(&mut bar.mapped, true)
            }) else {
                return Task::none();
            };
            let mut tasks = Vec::new();
            // A fold committed before the surface was configured has not
            // reached it yet; the surface is still the one it was asked as.
            let asked = FoldState::default().geometry(app.edge);
            if let Some(bar) = app
                .bars
                .get(&id)
                .filter(|b| first && b.fold.geometry(app.edge) != asked)
            {
                tasks.push(push_geometry(app, bar));
            }
            if let Some(which) = app.preview.take() {
                tasks.push(match which {
                    Preview::Drawer(drawer) => open_drawer(app, id, drawer, false),
                    Preview::TrayMenu(item, entries) => show_tray_menu(app, id, item, entries),
                });
            }
            tasks.extend(with_bar(app, id, fold_and_eye));
            return Task::batch(tasks);
        }
        // Nothing on the socket changed, so this one does not refetch.
        Message::Launch => {
            spawn_once(LAUNCHER, &[]);
            return Task::none();
        }
        // The system bus is not the compositor: fold the reading in and stop,
        // rather than falling through to a refetch the socket never asked for.
        // A fixture's readings are the thing being screenshotted.
        Message::Status(_) if app.fixture.is_some() => return Task::none(),
        Message::Status(update) => {
            match update {
                Update::Network(network) => app.network = network,
                Update::Bluetooth(bluetooth) => app.bluetooth = bluetooth,
                Update::Battery(battery) => app.battery = battery,
            }
            return reflow(app);
        }
        // Focus moved, a window went fullscreen, or the session went idle or
        // stopped being idle. All three are one event and one decision, taken
        // by every bar for itself.
        Message::OutputState {
            focused,
            fullscreen,
            idle,
        } => {
            app.focused_output = focused;
            app.fullscreen = fullscreen;
            app.idle = idle;
            // The output event is also the one that says monitors came or went.
            let screens = screens(app, SETTLE);
            return Task::batch([screens, each_bar(app, fold_and_eye)]);
        }
        Message::FoldTick => return each_bar(app, fold_and_eye),
        // The beacon and the eye's frames are not the compositor: nothing to
        // refetch, only the eyes' surfaces to raise or drop.
        Message::Eye(eye) => {
            app.iris.set(eye, std::time::Instant::now());
            return each_bar(app, sync_eye);
        }
        Message::EyeTick => {
            app.iris.tick(std::time::Instant::now());
            return each_bar(app, sync_eye);
        }
        Message::Reconfigured => {
            app.bar = app.conn.bar_config();
            if !app.bar.eye {
                app.iris.set(crate::eye::Eye::Off, std::time::Instant::now());
            }
            app.tray = app.conn.tray_config();
            if let Some(radius) = app.conn.glass_radius("bar.rounding") {
                app.bar_radius = radius;
            }
            if let Some(radius) = app.conn.glass_radius("decoration.rounding") {
                app.menu_radius = radius;
            }
            if app.fixture.is_none() {
                app.widget_cfg = app.conn.widgets_config();
                let motion = app.widget_cfg.motion;
                for bar in app.bars.values_mut() {
                    bar.motion.set_motion(motion);
                }
                crate::services::configure(&app.widget_cfg);
            }
            // A reload can turn folding off while a bar is folded, so the
            // decision is re-run rather than left until the next event.
            let (_, focused) = app.conn.outputs();
            if let Some(focused) = focused {
                app.focused_output = focused;
            }
            let screens = screens(app, SETTLE);
            return Task::batch([screens, each_bar(app, fold_and_eye)]);
        }
        // The radio verbs. Each is an action on the service and nothing else:
        // the drawer redraws from the `Update` the service answers with, not
        // from a guess. The two switches are the exception — a toggle that
        // springs back until the bus answers reads as broken — and are set
        // here and corrected by the next reading if the radio disagrees.
        Message::WifiEnable(on) => {
            app.radios.wifi_enabled = on;
            crate::radio::actions::set_wifi_enabled(on);
            return reflow(app);
        }
        Message::WifiConnect(ssid) => {
            let needs_secret = app.radios.network(&ssid).is_some_and(|n| n.secured && !n.known);
            if needs_secret {
                // The bar never sees the passphrase: the prompt asks for it
                // and hands it straight to the service.
                let dismiss = dismiss(app);
                spawn_once(SECRET_PROMPT, &["wifi", &ssid]);
                return dismiss;
            }
            crate::radio::actions::connect(&ssid);
            return Task::none();
        }
        Message::WifiDisconnect => {
            crate::radio::actions::disconnect();
            return Task::none();
        }
        Message::BtPower(on) => {
            app.bluetooth.powered = on;
            if !on {
                app.radios.scanning = false;
            }
            crate::radio::actions::set_bt_powered(on);
            return reflow(app);
        }
        Message::BtScan => {
            app.radios.scanning = !app.radios.scanning;
            crate::radio::actions::discover(app.radios.scanning);
            return reflow(app);
        }
        Message::BtConnect(addr) => {
            let paired = app.radios.devices.iter().any(|d| d.addr == addr && d.paired);
            if paired {
                crate::radio::actions::bt_connect(&addr);
            } else {
                crate::radio::actions::pair(&addr);
            }
            return Task::none();
        }
        Message::BtDisconnect(addr) => {
            crate::radio::actions::bt_disconnect(&addr);
            return Task::none();
        }
        Message::OpenSettings(pane) => {
            let dismiss = dismiss(app);
            spawn_once(SETTINGS, &[pane]);
            return dismiss;
        }
        Message::TrayActivate(id) => {
            let dismiss = dismiss(app);
            let cursor = at
                .and_then(|at| app.bars.get(&at))
                .map_or(iced::Point::ORIGIN, |b| b.cursor);
            crate::radio::actions::tray_activate(&id, cursor.x as i32, cursor.y as i32);
            return dismiss;
        }
        Message::TrayMenu(id) => {
            return match at {
                Some(at) => open_tray_menu(app, at, id),
                None => Task::none(),
            }
        }
        Message::TrayMenuClick(id, entry) => {
            let dismiss = dismiss(app);
            crate::radio::actions::tray_menu_click(&id, entry);
            return dismiss;
        }
        Message::Radio(feed) => return radio(app, feed),
        // Shared state, so one action however many bars draw the widget.
        Message::Widget(feed) => {
            if let Some(action) = widgets::update(&mut app.widgets, feed) {
                if app.fixture.is_none() {
                    crate::services::act(action);
                }
            }
            return Task::none();
        }
        Message::Streams(streams) => {
            app.streams = streams;
            return Task::none();
        }
        // A grip is a gesture on one bar: the pins and the drag are that
        // bar's own.
        Message::Grip(key, ev) => {
            if let Some(at) = at {
                with_bar(app, at, |app, bar| grip(app, bar, key, ev, Instant::now()));
            }
            return Task::none();
        }
        Message::Frame => {
            let now = Instant::now();
            for bar in app.bars.values_mut() {
                bar.motion.tick(now);
            }
            #[cfg(debug_assertions)]
            crate::preview::frame(app, now);
            return Task::none();
        }
        Message::Script(n) => {
            #[cfg(debug_assertions)]
            crate::preview::step(app, n);
            #[cfg(not(debug_assertions))]
            let _ = n;
            return Task::none();
        }
        // `to_layer_message` injects the layer-control variants. The bar only
        // ever sends them; the match must still be total.
        _ => return Task::none(),
    }
    // Every branch above either changed compositor state or was told state
    // changed, so all of them end the same way.
    refetch(app);
    Task::none()
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
fn fold(app: &App, bar: &mut Bar) -> Task<Message> {
    let now = std::time::Instant::now();
    let want = decide(
        &app.bar,
        bar.output_id,
        app.focused_output,
        app.fullscreen,
        app.idle,
    );
    let fold = &mut bar.fold;

    if want != fold.target {
        // Unfolding is immediate; folding waits out `FOLD_GRACE` with the
        // decision unchanged, which is what kills the two-bar flicker when the
        // pointer crosses an output boundary.
        let immediate = want == FoldTarget::Shown;
        match fold.pending {
            Some((pending, since)) if pending == want => {
                if immediate || now.duration_since(since) >= FOLD_GRACE {
                    commit(&app.bar, fold, want, now);
                }
            }
            _ => {
                if immediate {
                    fold.pending = None;
                    commit(&app.bar, fold, want, now);
                } else {
                    fold.pending = Some((want, now));
                }
            }
        }
    } else {
        fold.pending = None;
    }

    // Compared as a whole geometry, not a height: committing a fold swaps
    // the pill for the strip (dropping the edge margin) before the height
    // has moved at all.
    let before = fold.geometry(app.edge);
    advance(&app.bar, fold, now);
    if fold.geometry(app.edge) == before {
        return Task::none();
    }
    push_geometry(app, bar)
}

/// Ask the compositor for the surface the fold state needs. Nothing before
/// the surface is configured: [`Message::Sized`] pushes the state it reached
/// by then.
fn push_geometry(app: &App, bar: &Bar) -> Task<Message> {
    let g = bar.fold.geometry(app.edge);
    if !bar.mapped {
        return Task::none();
    }
    let id = bar.id;
    Task::batch([
        Task::done(Message::SizeChange {
            id,
            size: (0, g.height),
        }),
        Task::done(Message::MarginChange { id, margin: g.margin }),
        Task::done(Message::ExclusiveZoneChange {
            id,
            zone_size: g.zone,
        }),
    ])
}

/// A fold step, then the eye brought in line with it: the eye rides only on
/// a full pill, so a fold drops it and an unfold that lands raises it again.
fn fold_and_eye(app: &App, bar: &mut Bar) -> Task<Message> {
    let fold = fold(app, bar);
    Task::batch([fold, sync_eye(app, bar)])
}

/// Where the eye's surface sits: the launcher button's own box, anchored to
/// the bar's edge and the output's left, so the disc drawn centred in it lands
/// pixel for pixel on the ring the bar draws centred in the button.
///
/// The offsets are the pill's layer-shell margin plus the row's inset to the
/// button: `MARGIN_X + EDGE` across, and down (or up) `MARGIN_Y` plus half
/// the room the `PILL_H` row leaves around the `TASK_H` button.
fn eye_placement(
    edge: BarPosition,
) -> (
    iced_layershell::reexport::Anchor,
    (i32, i32, i32, i32),
    (u32, u32),
) {
    use iced_layershell::reexport::Anchor;
    let left = (bar::MARGIN_X + bar::EDGE) as i32;
    let off = (bar::MARGIN_Y + (bar::PILL_H - bar::TASK_H) / 2.0) as i32;
    let size = (bar::TASK_MIN as u32, bar::TASK_H as u32);
    match edge {
        BarPosition::Top => (Anchor::Top | Anchor::Left, (off, 0, 0, left), size),
        BarPosition::Bottom => (Anchor::Bottom | Anchor::Left, (0, 0, off, left), size),
    }
}

/// Raise or drop a bar's eye surface to match the iris (ADR 0056).
///
/// The bar only ever draws the plain ring. The live eye is a second layer
/// surface, namespace [`crate::eye::NAMESPACE`], laid over the mark, which
/// abyss leaves out of screenshots and recordings (`capture { hide-layer
/// "hyperion:eclipse-eye" }`) — so a capture shows the still ring beneath and
/// the person at the screen sees the eye. It exists only while there is an
/// eye to show on a full pill: a settled Off, a folded or hidden bar, or
/// `bar.eye = false` costs no surface at all. Each bar has its own, on its
/// own output.
///
/// Exclusive zone `-1`, not unset: an unset zone is `0`, which wlr-layer-shell
/// places clear of the bar's own reserved strip, i.e. under the bar rather
/// than on it. An empty input region, so a click falls through to the
/// launcher button beneath.
fn sync_eye(app: &App, bar: &mut Bar) -> Task<Message> {
    use iced_layershell::reexport::{KeyboardInteractivity, Layer, NewLayerShellSettings, OutputOption};
    let want = app.bar.eye && !app.iris.is_plain() && bar.fold.pill();
    match (want, bar.eye_surface) {
        (true, None) => {
            let (anchor, margin, size) = eye_placement(app.edge);
            let id = Id::unique();
            bar.eye_surface = Some(id);
            Task::done(Message::NewLayerShell {
                settings: NewLayerShellSettings {
                    size: Some(size),
                    layer: Layer::Top,
                    anchor,
                    exclusive_zone: Some(-1),
                    margin: Some(margin),
                    keyboard_interactivity: KeyboardInteractivity::None,
                    output_option: OutputOption::OutputName(bar.output_name.clone()),
                    events_transparent: true,
                    namespace: Some(crate::eye::NAMESPACE.to_owned()),
                },
                id,
            })
        }
        (false, Some(id)) => {
            bar.eye_surface = None;
            Task::done(Message::RemoveWindow(id))
        }
        _ => Task::none(),
    }
}

/// Accept a new target and start the slide toward it from wherever the bar
/// currently is. From rest the leg follows `bar.fold-curve`; a reversal
/// mid-slide becomes a spring that keeps the bar's current speed, so it
/// neither jumps back to the old end nor stops dead before turning.
fn commit(cfg: &BarConfig, fold: &mut FoldState, target: FoldTarget, now: std::time::Instant) {
    use eclipse_ui::motion::{Animated, Curve, Motion};
    fold.pending = None;
    fold.target = target;
    let (at, speed) = slide_at(cfg, fold, now);
    let to_h = target_height(cfg, target);
    fold.to_h = to_h;
    let to = to_h as f32;
    fold.slide = if cfg.fold_duration_ms == 0 || at == to {
        fold.height = to_h;
        Slide::Rest
    } else if matches!(fold.slide, Slide::Rest) {
        Slide::Curve {
            from: at,
            started: now,
        }
    } else {
        let motion = Motion {
            enabled: true,
            curve: Curve::Spring,
            duration: std::time::Duration::from_millis(cfg.fold_duration_ms as u64),
        };
        let mut spring = Animated::new(at, motion);
        spring.fling(to, speed, now);
        Slide::Spring(spring)
    };
}

/// Where the slide is at `now`, in fractional pixels, and how fast it is
/// moving there (pixels per second).
fn slide_at(cfg: &BarConfig, fold: &FoldState, now: std::time::Instant) -> (f32, f32) {
    match fold.slide {
        Slide::Rest => (fold.height as f32, 0.0),
        Slide::Curve { from, started } => {
            let dur = cfg.fold_duration_ms.max(1) as f32 / 1000.0;
            let t = now.saturating_duration_since(started).as_secs_f32() / dur;
            if t >= 1.0 {
                return (fold.to_h as f32, 0.0);
            }
            let curve = cfg.fold_curve;
            let span = fold.to_h as f32 - from;
            // The curves are quadratics: a central difference is exact.
            let (lo, hi) = ((t - 1e-3).max(0.0), t + 1e-3);
            let slope = (curve.ease(hi) - curve.ease(lo)) / (hi - lo);
            (from + span * curve.ease(t), span * slope / dur)
        }
        Slide::Spring(mut s) => {
            s.tick(now);
            (s.value(), s.velocity())
        }
    }
}

/// Advance one frame. Lands exactly on `to_h`, never past it.
fn advance(cfg: &BarConfig, fold: &mut FoldState, now: std::time::Instant) {
    let (at, _) = slide_at(cfg, fold, now);
    let landed = match &mut fold.slide {
        Slide::Rest => true,
        Slide::Curve { started, .. } => {
            now.saturating_duration_since(*started).as_millis() >= cfg.fold_duration_ms as u128
        }
        Slide::Spring(s) => {
            s.tick(now);
            !s.animating()
        }
    };
    if landed {
        fold.slide = Slide::Rest;
        fold.height = fold.to_h;
    } else {
        fold.height = at.round().max(0.0) as u32;
    }
}

/// Re-read the three lists and re-resolve any icon we have not seen.
fn refetch(app: &mut App) {
    if app.fixture.is_some() {
        return;
    }
    app.snapshot = app.conn.snapshot();
    app.icons.warm(&app.snapshot.windows);
}

/// Re-solve every bar, then open or close the monitor tap for all of them.
pub fn relayout_all(app: &mut App, now: Instant) {
    let _ = each_bar(app, |app, bar| {
        relayout(app, bar, now);
        Task::none()
    });
    if app.fixture.is_none() {
        crate::services::set_tap(tap_wanted(app));
    }
}

/// Re-solve one bar and point its animations at the answer.
///
/// The widgets' inputs are rebuilt from their state, a pin whose widget has
/// changed category is dropped (ADR 0065), and a grip under the finger is
/// taken at exactly the width the finger asks for. Nothing is placed until
/// the surface has a width: the first real layout must land, not grow in
/// from a zero-width bar.
pub fn relayout(app: &App, bar: &mut Bar, now: Instant) {
    let order = &app.widget_cfg.order;
    let mut inputs = Vec::with_capacity(order.len());
    for id in order {
        let key = id.key();
        let category = widgets::category(app, id);
        if bar.motion.pins.get(&key).is_some_and(|(_, c)| *c != category) {
            bar.motion.pins.remove(&key);
        }
        let spans = widgets::spans(app, id);
        inputs.push(layout::WidgetIn {
            core: spans.core,
            revealed: spans.revealed,
            important: app.widget_cfg.important.contains(id),
            present: spans.present,
            pin: bar.motion.pins.get(&key).map(|(pin, _)| *pin),
            live: bar.motion.drag.as_ref().filter(|d| d.key == key).map(Drag::live),
        });
    }
    bar.widget_inputs = inputs;
    if bar.width <= 0.0 {
        return;
    }

    let windows: Vec<&crate::model::Window> = crate::view::strip_windows(app, bar);
    let chips: Vec<layout::ChipIn> = windows
        .iter()
        .map(|w| layout::ChipIn {
            whole: layout::whole_width(w.label()),
            name: layout::whole_width(w.name()),
            minimized: w.minimized,
        })
        .collect();
    bar.layout = layout::solve(layout::Input {
        width: bar.width,
        lead: crate::view::strip_left(app, bar),
        chips: &chips,
        widgets: &bar.widget_inputs,
    });

    // Before the chips: their retarget marks the bar laid out, and a cover
    // already on hand at the first layout must land, not fade.
    let cover = app.widgets.now_playing.art_key();
    bar.motion.retarget_art(cover, now);

    let workspace = crate::view::strip_workspace(app, bar);
    let snap = bar.motion.snaps(workspace);
    bar.motion
        .retarget_chips(&windows, &bar.layout.chips, workspace, now);
    let targets: Vec<(String, bool, layout::WidgetOut)> = order
        .iter()
        .zip(&bar.widget_inputs)
        .zip(&bar.layout.widgets)
        .map(|((id, input), out)| (id.key(), input.present, *out))
        .collect();
    bar.motion.retarget_widgets(&targets, snap, now);
}

/// The monitor tap costs a capture stream, so it runs only while a bar can
/// show it: playing, visualizer on, and on at least one bar that has not
/// compressed it away and is itself up. A folded strip draws no widgets and
/// a hidden bar (fullscreen) draws nothing, so neither holds the stream
/// open; a fold still sitting out its grace window has not happened yet and
/// keeps it.
fn tap_wanted(app: &App) -> bool {
    let np = widgets::WidgetId::NowPlaying;
    app.widget_cfg.now_playing.visualizer
        && app.widget_cfg.order.contains(&np)
        && app.widgets.now_playing.playing()
        && app.bars.values().any(|bar| {
            bar.fold.target == FoldTarget::Shown
                && bar
                    .motion
                    .widgets
                    .get(&np.key())
                    .is_some_and(|w| w.extent.target() > 0.0)
        })
}

/// A grip gesture.
///
/// Press starts a drag from wherever the body is; drag follows the finger
/// 1:1 (the solver takes the live width exactly, so the chips re-ladder
/// under it); release settles on the rest nearest to where the finger's
/// velocity was carrying it, and pins it there. A press that barely moved is
/// a tap and toggles instead.
pub(crate) fn grip(app: &App, bar: &mut Bar, key: String, ev: GripEv, now: Instant) {
    match ev {
        GripEv::Press => {
            let start = bar.motion.widgets.get(&key).map_or(0.0, |w| w.extent.value());
            bar.motion.drag = Some(Drag::new(key, start, now));
        }
        GripEv::Drag(dx) => {
            if let Some(d) = bar.motion.drag.as_mut().filter(|d| d.key == key) {
                d.follow(dx, now);
            }
        }
        GripEv::Release => {
            let Some(drag) = bar.motion.drag.take().filter(|d| d.key == key) else {
                return;
            };
            let Some(index) = app.widget_cfg.order.iter().position(|id| id.key() == key) else {
                return;
            };
            let Some(input) = bar.widget_inputs.get(index).copied() else {
                return;
            };
            let pin = if drag.is_tap() {
                tap_pin(&input, drag.start)
            } else {
                let mut rests = vec![
                    (Pin::Collapsed, input.min_extent()),
                    (Pin::Open, input.core_run()),
                ];
                if input.revealed > 0.0 {
                    rests.push((Pin::Revealed, input.max_extent()));
                }
                settle(drag.live(), drag.velocity, &rests).unwrap_or(Pin::Open)
            };
            let category = widgets::category(app, &app.widget_cfg.order[index]);
            bar.motion.pins.insert(key.clone(), (pin, category));
            relayout(app, bar, now);
            if let Some(out) = bar.layout.widgets.get(index) {
                bar.motion.fling(&key, out.extent, drag.velocity, now);
            }
        }
    }
}

/// What a tap on a grip asks for: a compressed widget opens, an open one
/// compresses, and an important one — which never compresses — toggles its
/// revealed section instead.
fn tap_pin(input: &layout::WidgetIn, extent: f32) -> Pin {
    let open = extent >= input.core_run() - 0.5;
    if input.important {
        if extent >= input.max_extent() - 0.5 {
            Pin::Open
        } else {
            Pin::Revealed
        }
    } else if open {
        Pin::Collapsed
    } else {
        Pin::Open
    }
}

fn window(app: &App, handle: u64) -> Option<&crate::model::Window> {
    app.snapshot.windows.iter().find(|w| w.handle == handle)
}

/// Close the open popup, whichever bar it hangs from, and forget a menu
/// still on its way. `RemoveWindow` is the macro's own name for closing a
/// surface it created.
fn dismiss(app: &mut App) -> Task<Message> {
    app.pending_menu = None;
    match app.popup.take() {
        Some(popup) => Task::done(Message::RemoveWindow(popup.id)),
        None => Task::none(),
    }
}

/// What the menu offers for one window.
///
/// `Mute` is present only when the window's process actually owns a playback
/// stream: an entry that is meaningless for most windows is worse than no
/// entry, and "does this play audio" is exactly what the audio streams answer.
fn items(app: &App, handle: u64) -> Vec<Item> {
    let Some(w) = window(app, handle) else {
        return Vec::new();
    };
    let mut items = vec![if w.minimized {
        Item::Unminimize
    } else {
        Item::Minimize
    }];
    if let Some(muted) = crate::audio::state_of(&app.streams, w.pid) {
        items.push(if muted { Item::Unmute } else { Item::Mute });
    }
    items.push(Item::NewInstance);
    items.push(Item::Close);
    items
}

/// The rectangle a popup grows from, honouring `bar.popup-anchor`
/// ([`BarConfig::popup_anchor`], defaulting to [`tokens::popup::ANCHOR`]).
///
/// One function for every popup the bar owns, so that the context menu and the
/// tray drawers cannot drift apart, and so that flipping the setting flips all
/// of them. `span` is the clicked cell's horizontal extent and `edge` picks
/// which end of it the popup hangs from — the end the popup's gravity grows
/// *away* from, so the sheet stays on screen.
///
/// The result is a 1x1 rect rather than the cell itself: a zero-size anchor
/// has an unambiguous anchor point, where a rect's is the positioner's
/// business and differs between compositors. Popup *size* is still fixed at
/// creation; this only moves it.
fn anchor(app: &App, bar: &Bar, span: Option<(f32, f32)>, edge: Edge) -> (i32, i32, i32, i32) {
    let point = match (app.bar.popup_anchor, span) {
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
        _ => (bar.cursor.x, bar.cursor.y),
    };
    (point.0 as i32, point.1 as i32, 1, 1)
}

/// Which end of a cell a popup hangs from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edge {
    Left,
    Right,
}

fn open_menu(app: &mut App, at: Id, handle: u64) -> Task<Message> {
    let closed = dismiss(app);
    let Some(bar) = app.bars.get(&at) else {
        return closed;
    };
    let items = items(app, handle);
    if items.is_empty() {
        return closed;
    }
    let size = (crate::view::MENU_W, crate::view::menu_height(&items));
    // Gravity down-and-right, so the menu hangs from the chip's left edge.
    let rect = anchor(app, bar, crate::view::chip_span(app, bar, handle), Edge::Left);
    let settings = IcedNewPopupSettings::new(at, size, rect).gravity(PopupGravity::BottomRight);
    let (id, open) = Message::popup_open(settings);
    app.popup = Some(Popup {
        id,
        owner: at,
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
/// desktop. The same applet on another bar is another button: its click
/// moves the drawer there.
///
/// `toggle` is false when the drawer is being *re*opened at a new height by
/// [`reflow`], which must never read as a second click.
fn open_drawer(app: &mut App, at: Id, drawer: Drawer, toggle: bool) -> Task<Message> {
    let same = matches!(
        app.popup.as_ref(),
        Some(Popup { owner, kind: Kind::Drawer { which, .. }, .. }) if *which == drawer && *owner == at
    );
    let closed = dismiss(app);
    if same && toggle {
        return closed;
    }
    let Some(bar) = app.bars.get(&at) else {
        return closed;
    };
    if drawer == Drawer::Network && toggle {
        crate::radio::actions::scan();
    }
    let height = crate::view::drawer_height(app, drawer);
    let size = (crate::view::DRAWER_W, height);
    // Gravity down-and-*left*: the tray lives at the right end of the bar, so
    // a drawer growing to the right would hang off the edge of the screen —
    // which is also why it hangs from the cell's right edge and not its left.
    let rect = anchor(app, bar, crate::view::drawer_span(app, bar, drawer), Edge::Right);
    let settings = IcedNewPopupSettings::new(at, size, rect).gravity(PopupGravity::BottomLeft);
    let (id, open) = Message::popup_open(settings);
    app.popup = Some(Popup {
        id,
        owner: at,
        kind: Kind::Drawer {
            which: drawer,
            height,
        },
    });
    Task::batch([closed, open])
}

/// Keep an open drawer's surface the height its content now needs.
///
/// A popup's size is fixed when it is created, so a scan that finds three more
/// networks cannot grow the sheet in place. The drawer is closed and reopened
/// at the new height instead, on the same bar — one frame of churn, against a
/// sheet that would otherwise clip its own link row or hang empty glass
/// under it.
fn reflow(app: &mut App) -> Task<Message> {
    let Some(Popup {
        owner,
        kind: Kind::Drawer { which, height },
        ..
    }) = app.popup.as_ref()
    else {
        return Task::none();
    };
    let (owner, which) = (*owner, *which);
    if crate::view::drawer_height(app, which) == *height {
        return Task::none();
    }
    open_drawer(app, owner, which, false)
}

/// Ask for a tray item's own menu. It opens in [`radio`], when the entries
/// arrive, on the bar that asked.
fn open_tray_menu(app: &mut App, at: Id, id: String) -> Task<Message> {
    let closed = dismiss(app);
    crate::radio::actions::tray_menu(&id);
    app.pending_menu = Some((at, id));
    closed
}

/// Open a tray item's menu under the mark or drawer it was asked from.
///
/// An item with no menu opens nothing: an empty sheet is a worse answer to a
/// right-click than no sheet.
fn show_tray_menu(app: &mut App, at: Id, id: String, entries: Vec<crate::radio::MenuEntry>) -> Task<Message> {
    let Some(bar) = app.bars.get(&at) else {
        return Task::none();
    };
    if entries.is_empty() {
        return Task::none();
    }
    let size = (crate::view::MENU_W, crate::view::tray_menu_height(&entries));
    // Down-and-left from the right end, like the drawer it may have come
    // from: the tray sits at the bar's right end.
    let rect = anchor(app, bar, crate::view::tray_item_span(app, bar, &id), Edge::Right);
    let settings = IcedNewPopupSettings::new(at, size, rect).gravity(PopupGravity::BottomLeft);
    let (id_, open) = Message::popup_open(settings);
    app.popup = Some(Popup {
        id: id_,
        owner: at,
        kind: Kind::TrayMenu { id, entries },
    });
    open
}

/// Fold one service report into the model.
fn radio(app: &mut App, feed: crate::radio::Feed) -> Task<Message> {
    use crate::radio::Feed;
    match feed {
        Feed::WifiEnabled(on) => app.radios.wifi_enabled = on,
        Feed::Networks(networks) => app.radios.networks = networks,
        Feed::Devices(devices) => app.radios.devices = devices,
        Feed::Scanning(on) => app.radios.scanning = on,
        // A preview's fixture tray is the thing being screenshotted: the
        // session's live items would replace it on their first update.
        Feed::Tray(_) if cfg!(debug_assertions) && std::env::var_os("HYPERION_PREVIEW").is_some() => {}
        Feed::Tray(items) => app.radios.tray = items,
        Feed::NeedsSecret(ssid) => {
            let dismiss = dismiss(app);
            spawn_once(SECRET_PROMPT, &["wifi", &ssid]);
            return dismiss;
        }
        Feed::Menu { item, entries } => {
            let Some((at, _)) = app.pending_menu.as_ref().filter(|(_, pending)| *pending == item) else {
                return Task::none();
            };
            let at = *at;
            let closed = dismiss(app);
            return Task::batch([closed, show_tray_menu(app, at, item, entries)]);
        }
    }
    reflow(app)
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
    // No terminal command sourced here: a second instance is started without
    // asking the compositor anything, same as before `misc.terminal-command`
    // existed, so a `Terminal=true` entry still does not relaunch from the
    // taskbar (TERM-01 only changes what `eclipse-launcher` offers to run).
    let entries = eclipse_services::apps::scan(None);
    let Some(entry) = entries.iter().find(|e| e.id.eq_ignore_ascii_case(&wanted)) else {
        eprintln!("hyperion: no desktop entry for {app_id}");
        return;
    };
    if let Err(e) = eclipse_services::apps::launch(entry, None) {
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
    // One clock for every bar, alive while any of them is moving.
    if app.bars.values().any(|b| b.fold.animating()) {
        subs.push(fold_ticks());
    }
    // The bars' own motion clock, on the same rule: frames only while a
    // chip or a widget is moving or a grip is held, on any bar.
    if app.bars.values().any(|b| b.motion.animating()) || fixture_moves(app) {
        subs.push(frames());
    }
    #[cfg(debug_assertions)]
    if app.fixture.is_some() {
        subs.push(crate::preview::script());
    }
    if app.bar.eye {
        subs.push(crate::eye::watch());
    }
    // Same rule for the eye: frames only mid-dart or mid-resize, one deadline
    // while holding, nothing at all once it has settled back to the ring.
    if app.iris.animating() {
        subs.push(crate::eye::frames());
    } else if let Some(at) = app.iris.dart_at() {
        subs.push(crate::eye::after(at));
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

/// A preview fixture's visualizer is its own clock: it moves while its
/// player plays, with no service behind it.
fn fixture_moves(app: &App) -> bool {
    app.fixture.is_some() && app.widgets.now_playing.playing()
}

/// The bar's motion clock, alive only while something animates.
fn frames() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(8, async move |mut sender| {
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_millis(tokens::motion::FRAME_MS));
                if !send(&mut sender, Message::Frame) {
                    return;
                }
            });
        })
    })
}

/// Where the pointer (or a finger) is, and on which surface. The only source
/// of a popup's anchor.
fn pointer() -> Subscription<Message> {
    use iced::event::Status;
    iced::event::listen_with(|event, status, id| match event {
        // A press no cell took is a press on bare bar: it closes the popup.
        // Only uncaptured ones — a press a cell took has already acted, and
        // may have just opened the popup this would close.
        iced::Event::Mouse(iced::mouse::Event::ButtonPressed(_)) if status == Status::Ignored => {
            Some(Message::BarPress(id, None))
        }
        iced::Event::Touch(iced::touch::Event::FingerPressed { position, .. })
            if status == Status::Ignored =>
        {
            Some(Message::BarPress(id, Some(position)))
        }
        // A finger is the pointer too: a tap must anchor the popup it opens.
        iced::Event::Mouse(iced::mouse::Event::CursorMoved { position })
        | iced::Event::Touch(
            iced::touch::Event::FingerPressed { position, .. }
            | iced::touch::Event::FingerMoved { position, .. },
        ) => Some(Message::Pointer(id, position)),
        // The popup's grab holds the keyboard, so Escape arrives on the
        // popup's own surface.
        iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
            ..
        }) => Some(Message::Dismiss),
        // The surface's own size, which is where the task strip's condensation
        // ladder gets its "what fits" from.
        iced::Event::Window(iced::window::Event::Opened { size, .. })
        | iced::Event::Window(iced::window::Event::Resized(size)) => Some(Message::Sized(id, size.width)),
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
                // Wifi, bluetooth and the tray: the services behind the
                // drawers' actions, whose answers come back here.
                let feeds = crate::radio::take_feeds();
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
                        // A workspace switch or a new window arrives as a burst
                        // of events; the whole burst is one refetch, not one each.
                        let mut dirty = false;
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
                                    if matches!(message, Message::Refresh) {
                                        dirty = true;
                                    } else if !send(&mut sender, message) {
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
                        if dirty && !send(&mut sender, Message::Refresh) {
                            return;
                        }
                    }
                    if let Some(status) = status.as_ref() {
                        while let Some(update) = status.try_recv() {
                            if !send(&mut sender, Message::Status(update)) {
                                return;
                            }
                        }
                    }
                    if let Some(feeds) = feeds.as_ref() {
                        for feed in feeds.drain() {
                            if !send(&mut sender, Message::Radio(feed)) {
                                return;
                            }
                        }
                    }
                    // The widgets' services: media, audio, usage, custom.
                    for message in crate::services::drain() {
                        if !send(&mut sender, message) {
                            return;
                        }
                    }
                    // Only the minute rollover matters here, so the format
                    // is whichever; the view formats per `bar.clock.*`.
                    let now = crate::clock::time(false);
                    if now != minute {
                        minute = now;
                        if !send(&mut sender, Message::Refresh) {
                            return;
                        }
                    }
                    // The visualizer's levels arrive a frame apart, so while
                    // the tap is open the wait is a frame, not a poll.
                    let wait_for = if crate::services::tapping() {
                        std::time::Duration::from_millis(tokens::motion::FRAME_MS)
                    } else {
                        POLL
                    };
                    wait(client.as_ref(), wait_for);
                }
            });
        })
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::{Trust, Window, Workspace};

    /// An app whose bars are the test's own. `App::new` talks to the
    /// session's control socket, which on a machine running abyss lists real
    /// outputs; pinned, the app never reconciles its bars against them.
    fn app() -> App {
        let mut a = App::new();
        a.pin = Some("test".to_owned());
        a
    }

    /// A bar on `name` as [`open_bar`] makes it, already configured at a
    /// laptop's width.
    pub(crate) fn bar_on(a: &mut App, name: &str, output_id: u64) -> Id {
        let _ = open_bar(a, name.to_owned(), output_id);
        let bar = a
            .bars
            .values_mut()
            .find(|b| b.output_name == name)
            .expect("open_bar keeps the bar");
        bar.width = 1440.0;
        bar.mapped = true;
        bar.id
    }

    fn now_playing(a: &mut App) {
        use crate::widgets::{now_playing, Feed};
        use eclipse_services::media::{NowPlaying, Playback};
        widgets::update(
            &mut a.widgets,
            Feed::NowPlaying(now_playing::Feed::Player(Some(NowPlaying {
                player: "player".into(),
                title: "Track".into(),
                artist: None,
                album: None,
                art: None,
                status: Playback::Playing,
                can_prev: true,
                can_next: true,
                can_pause: true,
            }))),
        );
    }

    #[test]
    fn the_eye_surface_exists_only_while_there_is_an_eye_on_a_full_pill() {
        let mut a = app();
        let id = bar_on(&mut a, "DP-1", 0);
        let eye = |a: &mut App| {
            let _ = with_bar(a, id, sync_eye);
            a.bars[&id].eye_surface
        };
        a.bar.eye = true;
        assert!(a.iris.is_plain());
        assert_eq!(eye(&mut a), None, "a settled Off costs no surface");
        a.iris.set(crate::eye::Eye::Watch, std::time::Instant::now());
        let raised = eye(&mut a).expect("a watching eye raises its surface");
        assert_eq!(eye(&mut a), Some(raised), "raised once, not once per frame");
        a.bars.get_mut(&id).unwrap().fold.target = FoldTarget::Folded;
        assert_eq!(eye(&mut a), None, "a folded bar has no mark to cover");
        a.bars.get_mut(&id).unwrap().fold.target = FoldTarget::Shown;
        a.bar.eye = false;
        assert_eq!(eye(&mut a), None);
    }

    #[test]
    fn the_eye_surface_is_small_enough_to_leave_out_of_captures() {
        // abyss omits a `hide-layer` surface only up to 64x64 logical px
        // (render/capture.rs `HIDE_LAYER_MAX`); past that it is captured.
        for edge in [BarPosition::Top, BarPosition::Bottom] {
            let (_, _, (w, h)) = eye_placement(edge);
            assert!(w <= 64 && h <= 64);
        }
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
                id: Id::unique(),
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

    /// The grip's three gestures: a tap on an open widget compresses it, a
    /// leftward drag opens it at least to its core, and a rightward drag
    /// compresses it again — each pinned, and each the solver's answer.
    #[test]
    fn a_grip_taps_shut_and_drags_open_and_shut() {
        use crate::widgets::{volume, Feed, WidgetId};
        let mut a = app();
        let id = bar_on(&mut a, "DP-1", 0);
        widgets::update(
            &mut a.widgets,
            Feed::Volume(volume::Feed::Sink(Some(eclipse_services::audio::Sink {
                volume: 0.5,
                muted: false,
                description: "Speakers".into(),
            }))),
        );
        let t0 = Instant::now();
        relayout_all(&mut a, t0);
        let mut b = a.bars.remove(&id).unwrap();
        let i = a
            .widget_cfg
            .order
            .iter()
            .position(|id| *id == WidgetId::Volume)
            .expect("volume is in the default order");
        let key = WidgetId::Volume.key();
        let input = b.widget_inputs[i];
        assert!(
            b.layout.widgets[i].extent >= input.core_run(),
            "room to spare: open"
        );

        let later = |n: u64| t0 + std::time::Duration::from_secs(n);
        grip(&a, &mut b, key.clone(), GripEv::Press, later(1));
        grip(&a, &mut b, key.clone(), GripEv::Release, later(1));
        assert_eq!(b.motion.pins.get(&key).map(|p| p.0), Some(Pin::Collapsed));
        assert_eq!(b.layout.widgets[i].extent, 0.0);
        b.motion.tick(later(3));

        grip(&a, &mut b, key.clone(), GripEv::Press, later(4));
        grip(
            &a,
            &mut b,
            key.clone(),
            GripEv::Drag(-input.max_extent()),
            later(4),
        );
        relayout(&a, &mut b, later(4));
        assert_eq!(
            b.layout.widgets[i].extent,
            input.max_extent(),
            "the width follows the finger"
        );
        grip(&a, &mut b, key.clone(), GripEv::Release, later(4));
        assert_ne!(b.motion.pins.get(&key).map(|p| p.0), Some(Pin::Collapsed));
        assert!(b.layout.widgets[i].extent >= input.core_run());
        b.motion.tick(later(6));

        grip(&a, &mut b, key.clone(), GripEv::Press, later(7));
        grip(
            &a,
            &mut b,
            key.clone(),
            GripEv::Drag(input.max_extent()),
            later(7),
        );
        grip(&a, &mut b, key.clone(), GripEv::Release, later(7));
        assert_eq!(b.motion.pins.get(&key).map(|p| p.0), Some(Pin::Collapsed));
    }

    /// A grip is one bar's gesture: its pin lands on the bar it was on and
    /// no other.
    #[test]
    fn a_grip_pins_only_the_bar_it_was_on() {
        use crate::widgets::WidgetId;
        let mut a = app();
        let (one, two) = (bar_on(&mut a, "DP-1", 1), bar_on(&mut a, "DP-2", 2));
        relayout_all(&mut a, Instant::now());
        let key = WidgetId::Clock.key();
        for ev in [GripEv::Press, GripEv::Release] {
            let _ = update(&mut a, Message::On(one, Box::new(Message::Grip(key.clone(), ev))));
        }
        assert!(a.bars[&one].motion.pins.contains_key(&key));
        assert!(!a.bars[&two].motion.pins.contains_key(&key));
    }

    /// The monitor tap follows the bars off screen: a folded or hidden bar
    /// draws no visualizer, so it holds no capture stream — unless another
    /// bar still shows it.
    #[test]
    fn the_tap_is_open_while_any_bar_shows_now_playing() {
        let mut a = app();
        let one = bar_on(&mut a, "DP-1", 1);
        let two = bar_on(&mut a, "DP-2", 2);
        now_playing(&mut a);
        relayout_all(&mut a, Instant::now());
        let set = |a: &mut App, id: Id, t: FoldTarget| a.bars.get_mut(&id).unwrap().fold.target = t;
        assert!(tap_wanted(&a), "playing, on two shown bars");
        set(&mut a, one, FoldTarget::Folded);
        assert!(tap_wanted(&a), "one bar folded, the other still shows it");
        set(&mut a, two, FoldTarget::Hidden);
        assert!(
            !tap_wanted(&a),
            "folded on one, hidden under fullscreen on the other"
        );
        set(&mut a, two, FoldTarget::Shown);
        assert!(tap_wanted(&a));
        a.widget_cfg.now_playing.visualizer = false;
        assert!(!tap_wanted(&a));
        a.widget_cfg.now_playing.visualizer = true;
        a.bars.clear();
        assert!(!tap_wanted(&a), "no bar, no one to show it to");
    }

    /// Services are the process's, not a bar's: however many bars there
    /// are, a reload configures them once and a click on a custom widget
    /// runs its command once.
    #[test]
    fn a_service_starts_and_acts_once_whatever_the_bar_count() {
        use crate::widgets::{custom, Feed};
        let before = crate::services::calls();
        let mut a = app();
        let bars: Vec<Id> = ["DP-1", "DP-2", "HDMI-A-1"]
            .iter()
            .zip(1..)
            .map(|(n, i)| bar_on(&mut a, n, i))
            .collect();
        let _ = update(&mut a, Message::Reconfigured);
        let click = Message::Widget(Feed::Custom(
            "weather".into(),
            custom::Feed::Run(vec!["true".into()]),
        ));
        let _ = update(&mut a, Message::On(bars[1], Box::new(click)));
        let after = crate::services::calls();
        assert_eq!(
            after.0 - before.0,
            2,
            "configured at start and on reload, not per bar"
        );
        assert_eq!(after.1 - before.1, 1, "one click, one run");
    }

    /// A press on bare bar closes the menu — on any bar, since there is one
    /// popup; one inside the menu does not.
    #[test]
    fn a_press_on_bare_bar_closes_the_menu() {
        let mut a = app();
        let (one, two) = (bar_on(&mut a, "DP-1", 1), bar_on(&mut a, "DP-2", 2));
        let menu_id = Id::unique();
        let menu = |id| Popup {
            id,
            owner: one,
            kind: Kind::Menu {
                handle: 1,
                items: vec![Item::Close],
            },
        };
        a.popup = Some(menu(menu_id));
        let _ = step(&mut a, Message::BarPress(menu_id, None), None);
        assert!(a.popup.is_some(), "a press inside the menu keeps it");
        let _ = step(&mut a, Message::BarPress(two, None), None);
        assert!(a.popup.is_none(), "a press on another bar closes it");
        a.popup = Some(menu(menu_id));
        let _ = step(&mut a, Message::BarPress(one, None), None);
        assert!(a.popup.is_none(), "a press on its own bar closes it");
        a.popup = Some(menu(menu_id));
        let _ = step(&mut a, Message::Dismiss, None);
        assert!(a.popup.is_none(), "Escape's message closes it");
    }

    /// A message from inside a popup acts for the bar the popup hangs from,
    /// and a bar that goes takes its popup with it and leaves the rest.
    #[test]
    fn a_popup_belongs_to_the_bar_it_opened_from() {
        let mut a = app();
        let (one, two) = (bar_on(&mut a, "DP-1", 1), bar_on(&mut a, "DP-2", 2));
        let menu_id = Id::unique();
        a.popup = Some(Popup {
            id: menu_id,
            owner: two,
            kind: Kind::Menu {
                handle: 1,
                items: vec![Item::Close],
            },
        });
        assert_eq!(owner(&a, menu_id), Some(two));
        assert_eq!(owner(&a, one), Some(one));
        assert_eq!(owner(&a, Id::unique()), None);
        let _ = close_bar(&mut a, one);
        assert!(a.popup.is_some(), "another bar going leaves it");
        let _ = close_bar(&mut a, two);
        assert!(a.popup.is_none(), "its own bar going takes it");
    }

    /// A drawer's toggle is per bar: the same applet on the other bar moves
    /// the drawer there rather than just closing it.
    #[test]
    fn a_drawer_toggles_on_its_own_bar_and_moves_from_another() {
        let mut a = app();
        let (one, two) = (bar_on(&mut a, "DP-1", 1), bar_on(&mut a, "DP-2", 2));
        let _ = open_drawer(&mut a, one, Drawer::Overflow, true);
        assert_eq!(a.popup.as_ref().map(|p| p.owner), Some(one));
        let _ = open_drawer(&mut a, two, Drawer::Overflow, true);
        assert_eq!(a.popup.as_ref().map(|p| p.owner), Some(two), "moved");
        let _ = open_drawer(&mut a, two, Drawer::Overflow, true);
        assert!(a.popup.is_none(), "toggled shut");
    }

    fn live(names: &[&str]) -> Vec<(u64, String)> {
        names.iter().zip(1..).map(|(n, i)| (i, (*n).to_owned())).collect()
    }

    /// A new output gets a bar once it has settled; a gone one loses its
    /// bar; the others are left alone.
    #[test]
    fn outputs_are_added_and_removed_without_touching_the_rest() {
        let (a, b) = (Id::unique(), Id::unique());
        let mut seen = HashMap::new();
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);

        let boot = reconcile(&[], &live(&["DP-1", "DP-2"]), &mut seen, t0, Duration::ZERO);
        assert_eq!(boot.open, ["DP-1", "DP-2"], "at boot nothing waits");
        assert!(boot.close.is_empty() && !boot.wait);

        let open = [(a, "DP-1"), (b, "DP-2")];
        let plugged = live(&["DP-1", "DP-2", "HDMI-A-1"]);
        let first = reconcile(&open, &plugged, &mut seen, ms(0), SETTLE);
        assert!(first.open.is_empty() && first.close.is_empty());
        assert!(first.wait, "a new output waits out the settle");
        let early = reconcile(&open, &plugged, &mut seen, ms(100), SETTLE);
        assert!(early.open.is_empty() && early.wait);
        let settled = reconcile(&open, &plugged, &mut seen, ms(300), SETTLE);
        assert_eq!(settled.open, ["HDMI-A-1"]);
        assert!(settled.close.is_empty() && !settled.wait);
        assert!(seen.is_empty());

        let unplugged = reconcile(&open, &live(&["DP-2"]), &mut seen, ms(400), SETTLE);
        assert_eq!(unplugged.close, [a], "only the gone output's bar closes");
        assert!(unplugged.open.is_empty());
    }

    /// An empty list is the socket not being up, not every monitor gone.
    #[test]
    fn an_empty_output_list_closes_nothing() {
        let (a, b) = (Id::unique(), Id::unique());
        let mut seen = HashMap::new();
        let plan = reconcile(
            &[(a, "DP-1"), (b, "DP-2")],
            &[],
            &mut seen,
            Instant::now(),
            SETTLE,
        );
        assert_eq!(plan, Plan::default());
    }

    /// A renamed output is its old bar closed and a new one opened, the new
    /// one after the settle like any other arrival.
    #[test]
    fn a_renamed_output_is_a_removal_and_an_addition() {
        let a = Id::unique();
        let mut seen = HashMap::new();
        let t0 = Instant::now();
        let renamed = live(&["DP-3"]);
        let plan = reconcile(&[(a, "DP-1")], &renamed, &mut seen, t0, SETTLE);
        assert_eq!(plan.close, [a]);
        assert!(plan.open.is_empty() && plan.wait);
        let plan = reconcile(&[], &renamed, &mut seen, t0 + SETTLE, SETTLE);
        assert_eq!(plan.open, ["DP-3"]);
    }

    /// An output that flickers out while settling starts its settle over.
    #[test]
    fn an_output_that_leaves_while_settling_starts_over() {
        let mut seen = HashMap::new();
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let _ = reconcile(&[], &live(&["DP-1", "DP-2"]), &mut seen, ms(0), SETTLE);
        let _ = reconcile(&[], &live(&["DP-1"]), &mut seen, ms(100), SETTLE);
        let plan = reconcile(&[], &live(&["DP-1", "DP-2"]), &mut seen, ms(300), SETTLE);
        assert_eq!(plan.open, ["DP-1"], "DP-2 came back at 300 ms and waits again");
        assert!(plan.wait);
    }

    /// Output ids are re-resolved by name on every look; an empty list
    /// leaves a stale id alone rather than zeroing it.
    #[test]
    fn an_output_id_is_re_resolved_by_name() {
        let mut a = app();
        let id = bar_on(&mut a, "DP-99", 4);
        resolve(&mut a.bars, &[]);
        assert_eq!(a.bars[&id].output_id, 4);
        resolve(&mut a.bars, &[(11, "DP-99".to_owned())]);
        assert_eq!(a.bars[&id].output_id, 11);
    }

    /// A bar the compositor closed is forgotten; the others stay.
    #[test]
    fn a_closed_bar_is_forgotten() {
        let mut a = app();
        let (one, two) = (bar_on(&mut a, "DP-1", 1), bar_on(&mut a, "DP-2", 2));
        let _ = step(&mut a, Message::Closed(one), None);
        assert!(!a.bars.contains_key(&one));
        assert!(a.bars.contains_key(&two));
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

    /// An unresolved output id never folds, even under a configuration that
    /// would otherwise fold everything.
    #[test]
    fn an_unresolved_output_never_folds() {
        assert_eq!(decide(&cfg(true, true), 0, 9, true, true), FoldTarget::Shown);
    }

    /// A fullscreen window on the *other* output is none of this bar's business.
    #[test]
    fn fullscreen_elsewhere_does_not_hide_this_bar() {
        assert_eq!(decide(&cfg(false, false), 7, 9, true, false), FoldTarget::Shown);
    }

    fn folding_app() -> (App, Bar) {
        let mut a = App::new();
        a.bar = cfg(true, false);
        a.focused_output = 7;
        let b = Bar::new(
            Id::unique(),
            "DP-1".into(),
            7,
            eclipse_ui::motion::Motion::DEFAULT,
        );
        (a, b)
    }

    /// Hysteresis: a fold that reverses inside the grace window commits nothing.
    #[test]
    fn a_fold_that_reverses_inside_the_grace_window_never_happens() {
        let (mut a, mut b) = folding_app();
        a.focused_output = 9;
        let _ = fold(&a, &mut b);
        assert_eq!(b.fold.target, FoldTarget::Shown, "fold is only pending");
        assert!(b.fold.pending.is_some());

        a.focused_output = 7;
        let _ = fold(&a, &mut b);
        assert_eq!(b.fold.target, FoldTarget::Shown);
        assert!(b.fold.pending.is_none(), "the pending fold is dropped");
        assert_eq!(b.fold.height, crate::HEIGHT, "the height never moved");
    }

    /// The other direction has no grace at all: the human is already reaching
    /// for the bar.
    #[test]
    fn unfolding_is_immediate() {
        let (mut a, mut b) = folding_app();
        a.bar.fold_duration_ms = 0;
        a.focused_output = 9;
        let then = std::time::Instant::now() - FOLD_GRACE * 2;
        b.fold.pending = Some((FoldTarget::Folded, then));
        let _ = fold(&a, &mut b);
        assert_eq!(b.fold.target, FoldTarget::Folded);

        a.focused_output = 7;
        let _ = fold(&a, &mut b);
        assert_eq!(b.fold.target, FoldTarget::Shown);
        assert_eq!(b.fold.height, crate::HEIGHT);
    }

    /// Each bar folds for its own output: the focused one stays, the other
    /// folds, from one shared output event.
    #[test]
    fn each_bar_folds_for_its_own_output() {
        let (mut a, _) = folding_app();
        a.bar.fold_duration_ms = 0;
        let here = crate::app::tests::bar_on(&mut a, "DP-1", 7);
        let there = crate::app::tests::bar_on(&mut a, "DP-2", 9);
        let then = std::time::Instant::now() - FOLD_GRACE * 2;
        a.bars.get_mut(&there).unwrap().fold.pending = Some((FoldTarget::Folded, then));
        let _ = each_bar(&mut a, fold);
        assert_eq!(a.bars[&here].fold.target, FoldTarget::Shown);
        assert_eq!(a.bars[&there].fold.target, FoldTarget::Folded);
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
            let (mut a, mut b) = folding_app();
            a.bar.fold_curve = curve;
            let started = std::time::Instant::now();
            commit(&a.bar, &mut b.fold, FoldTarget::Folded, started);
            assert!(b.fold.animating(), "{curve:?} animates");

            let mut last = b.fold.height;
            for step in 1..=10u32 {
                advance(
                    &a.bar,
                    &mut b.fold,
                    started + std::time::Duration::from_millis(step as u64 * 15),
                );
                assert!(b.fold.height <= last, "{curve:?} step {step} went back up");
                last = b.fold.height;
            }
            assert_eq!(b.fold.height, b.fold.to_h, "{curve:?} lands on to_h");
            assert!(!b.fold.animating(), "{curve:?} settles");
        }
    }

    /// A reversal mid-slide is continuous: no height jump at the turn, no
    /// velocity kink, no frame that leaps, and it lands exactly on the shown
    /// height with the clock stopped.
    #[test]
    fn a_reversed_fold_turns_without_a_jump() {
        let (mut a, mut b) = folding_app();
        a.bar.fold_duration_ms = 200;
        let t0 = std::time::Instant::now();
        let ms = |n: u64| t0 + std::time::Duration::from_millis(n);
        commit(&a.bar, &mut b.fold, FoldTarget::Folded, t0);
        for n in (8..=80).step_by(8) {
            advance(&a.bar, &mut b.fold, ms(n));
        }
        let mid = b.fold.height;
        assert!(
            mid < crate::HEIGHT && mid > a.bar.fold_height,
            "mid-slide at {mid}"
        );
        let (at, speed) = slide_at(&a.bar, &b.fold, ms(80));
        assert!(speed < 0.0, "folding moves down");

        commit(&a.bar, &mut b.fold, FoldTarget::Shown, ms(80));
        let (at2, speed2) = slide_at(&a.bar, &b.fold, ms(80));
        assert!((at2 - at).abs() < 0.01, "no jump at the turn: {at} -> {at2}");
        assert!(
            (speed2 - speed).abs() < 1.0,
            "no velocity kink: {speed} -> {speed2}"
        );

        let mut prev = b.fold.height;
        let mut steps = Vec::new();
        for n in (88..=1000).step_by(8) {
            advance(&a.bar, &mut b.fold, ms(n));
            steps.push(b.fold.height as i32 - prev as i32);
            prev = b.fold.height;
        }
        let max_step = steps.iter().map(|d| d.abs()).max().unwrap_or(0);
        assert!(max_step <= 4, "no frame leaps: {steps:?}");
        assert_eq!(b.fold.height, crate::HEIGHT, "lands exactly");
        assert!(!b.fold.animating(), "and the clock stops");
    }

    /// Zero duration means snap: no animation frames, no tick subscription.
    #[test]
    fn a_zero_duration_snaps() {
        let (mut a, mut b) = folding_app();
        a.bar.fold_duration_ms = 0;
        commit(&a.bar, &mut b.fold, FoldTarget::Folded, std::time::Instant::now());
        advance(&a.bar, &mut b.fold, std::time::Instant::now());
        assert_eq!(b.fold.height, a.bar.fold_height);
        assert!(!b.fold.animating());
    }

    /// The pill fills its surface and the float gap is margin, yet the strip
    /// a tiled window avoids is still the full `HEIGHT`: the compositor adds
    /// the anchored edge's margin to the zone. A folded strip sits flush.
    #[test]
    fn the_gap_is_margin_and_the_reserved_strip_is_unchanged() {
        let shown = FoldState::default();
        let g = shown.geometry(BarPosition::Top);
        assert_eq!(g.height, bar::PILL_H as u32);
        assert_eq!(
            g.margin,
            (
                bar::MARGIN_Y as i32,
                bar::MARGIN_X as i32,
                0,
                bar::MARGIN_X as i32
            )
        );
        assert_eq!(g.zone + g.margin.0, crate::HEIGHT as i32);
        let g = shown.geometry(BarPosition::Bottom);
        assert_eq!(g.margin.0, 0);
        assert_eq!(g.zone + g.margin.2, crate::HEIGHT as i32);

        let (mut a, mut b) = folding_app();
        a.bar.fold_duration_ms = 0;
        commit(&a.bar, &mut b.fold, FoldTarget::Folded, std::time::Instant::now());
        advance(&a.bar, &mut b.fold, std::time::Instant::now());
        let g = b.fold.geometry(BarPosition::Top);
        assert_eq!(g.height, a.bar.fold_height);
        assert_eq!(g.zone, a.bar.fold_height as i32);
        assert_eq!((g.margin.0, g.margin.2), (0, 0));
    }

    /// `Hidden` gives the zone up entirely; `Folded` keeps its sliver.
    #[test]
    fn hidden_surrenders_the_exclusive_zone() {
        let bar = BarConfig::default();
        assert_eq!(target_height(&bar, FoldTarget::Hidden), HIDDEN_HEIGHT);
        assert_eq!(target_height(&bar, FoldTarget::Folded), bar.fold_height);
        assert_eq!(target_height(&bar, FoldTarget::Shown), crate::HEIGHT);
    }
}
