// SPDX-License-Identifier: AGPL-3.0-only
//! The system tray: a StatusNotifierItem watcher and host (ADR 0038's
//! `system-tray`), on plain zbus.
//!
//! Apps announce tray items to `org.kde.StatusNotifierWatcher` on the session
//! bus. [`spawn`] serves that name if nobody else does, and queues for it if
//! someone does, so that if another watcher exits we take over and the apps
//! re-register with us, which Qt, GTK's libappindicator and Electron all do
//! when the watcher's owner changes. Either way the host reads the list from
//! whoever owns the name. That way there is one code path whether or not the
//! watcher is ours.
//!
//! [`observe`] is the read-only half, for a second view of the same tray (the
//! Settings pane): it never serves or asks for the watcher name, only hosts
//! against whichever watcher owns it, and follows the name when it changes
//! hands. It gets no [`Tray`], so it cannot click anything.
//!
//! Dropping [`TrayUpdates`] ends the tray: its connection closes at once, so
//! the host thread and its signal pumps return promptly rather than at the
//! next change, and any [`Tray`] clone left over is then a click that does
//! nothing. For [`spawn`] that also stops serving the watcher.
//!
//! The shape is the same as the status watchers. One thread owns the reading.
//! It re-reads when the watcher or any item signals a change, debounced, and
//! at least every 30 s, and reports [`TrayUpdate::Items`] over a channel.
//! Clicks ([`Tray::activate`], [`Tray::menu`], [`Tray::menu_click`]) each run
//! on their own short thread, because the app on the other end can be slow or
//! wedged, and a menu comes back as [`TrayUpdate::Menu`] rather than as a
//! return value, so no view ever waits on an app.
//!
//! Items and menus are app-supplied and untrusted. Counts, sizes and label
//! lengths are bounded (see `item.rs`, `menu.rs`), and none of it is logged.

mod item;
mod menu;
mod watcher;

use std::collections::HashMap;
use std::fmt;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use zbus::blocking::{connection, Connection, MessageIterator};
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::message::Type;
use zbus::{MatchRule, Message};

use crate::status::uncached;
use watcher::{Watcher, WATCHER_INTERFACE, WATCHER_NAME, WATCHER_PATH};

/// One app's tray icon.
#[derive(Clone, PartialEq, Eq)]
pub struct TrayItem {
    /// The app's own name for the item (`nm-applet`, `discord`), which is
    /// what `bar.tray.pinned` and `bar.tray.hidden` list. Stable across
    /// restarts, but not unique: two instances of one app share it. Falls
    /// back to `address` when the app gives none.
    pub id: String,
    /// `bus/path`: unique while the item lives, gone when it does. Every
    /// [`Tray`] method accepts this or `id`.
    pub address: String,
    /// What to show on hover or in the overflow list. Never empty: falls back
    /// to the tooltip title, then `id`.
    pub title: String,
    pub status: ItemStatus,
    /// A freedesktop icon name, looked up in `icon_theme_path` first, then
    /// the theme. Some apps (Electron) put an absolute file path here. Empty
    /// if the app only sends pixels.
    pub icon_name: String,
    pub icon_theme_path: String,
    /// The app's own pixels, for when the name does not resolve.
    pub icon_pixmap: Option<Pixmap>,
    /// The app wants a click to open the menu, not `Activate`.
    pub item_is_menu: bool,
    pub has_menu: bool,
}

impl fmt::Debug for TrayItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TrayItem")
            .field("id", &self.id)
            .field("address", &self.address)
            .field("status", &self.status)
            .field("icon_name", &self.icon_name)
            .field("icon_pixmap", &self.icon_pixmap)
            .field("item_is_menu", &self.item_is_menu)
            .field("has_menu", &self.has_menu)
            .finish_non_exhaustive()
    }
}

/// SNI's `Status`. A passive item has nothing to say right now. The spec lets
/// a host tuck it away, and the overflow drawer is where it goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemStatus {
    Passive,
    Active,
    NeedsAttention,
}

/// Straight RGBA, row-major, ready for an image handle.
#[derive(Clone, PartialEq, Eq)]
pub struct Pixmap {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl fmt::Debug for Pixmap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Pixmap({}x{})", self.width, self.height)
    }
}

/// One row of an item's menu, flattened (see `menu.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuEntry {
    /// What [`Tray::menu_click`] takes.
    pub id: i32,
    pub label: String,
    pub enabled: bool,
    pub kind: MenuKind,
    /// `Some` for a checkbox or radio entry, with whether it is on.
    pub checked: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuKind {
    Item,
    Separator,
    /// A submenu's title, shown above its entries. Not clickable.
    Header,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayUpdate {
    /// The whole tray, in registration order. Sent only when it changed.
    Items(Vec<TrayItem>),
    /// The menu asked for with [`Tray::menu`], or the one opened because the
    /// item had no `Activate`. `item` is exactly what the caller passed.
    /// Empty if the item has no menu or did not answer.
    Menu { item: String, entries: Vec<MenuEntry> },
}

/// The GUI's end of the tray. Dropping it shuts the tray down.
pub struct TrayUpdates {
    updates: Receiver<TrayUpdate>,
    /// Closed on drop. The socket reader then fails every pending call and
    /// ends every signal stream, which is what wakes the host thread.
    connection: Connection,
}

impl Drop for TrayUpdates {
    fn drop(&mut self) {
        let _ = self.connection.clone().close();
    }
}

impl TrayUpdates {
    /// The next change, if one is waiting. Never blocks.
    pub fn try_recv(&self) -> Option<TrayUpdate> {
        self.updates.try_recv().ok()
    }

    /// Wait up to `timeout` for the next change.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<TrayUpdate> {
        self.updates.recv_timeout(timeout).ok()
    }
}

/// Where to send a click: the item's bus, path, and which of the two
/// interface spellings it answered to, and its menu.
#[derive(Debug, Clone)]
struct Target {
    bus: String,
    path: String,
    interface: &'static str,
    menu: Option<String>,
}

/// The tray's click side. Cheap to clone. Every method returns at once and
/// does its bus work on a thread of its own.
#[derive(Clone)]
pub struct Tray {
    connection: Connection,
    /// The last read's targets, by address and by id. Written only by the
    /// host thread and read here, so a click resolves the item it was drawn
    /// from without a bus call.
    targets: Arc<Mutex<HashMap<String, Target>>>,
    updates: Sender<TrayUpdate>,
}

impl Tray {
    /// A primary click at screen position (`x`, `y`). If the app has no
    /// `Activate` (menu-only items such as `nm-applet --indicator`), its menu
    /// is fetched and arrives as [`TrayUpdate::Menu`] instead.
    pub fn activate(&self, item: &str, x: i32, y: i32) {
        self.click(item, "Activate", x, y, true);
    }

    /// A middle click.
    pub fn secondary_activate(&self, item: &str, x: i32, y: i32) {
        self.click(item, "SecondaryActivate", x, y, false);
    }

    /// Fetch the item's menu. It arrives as [`TrayUpdate::Menu`].
    pub fn menu(&self, item: &str) {
        let Some(target) = self.target(item) else {
            return;
        };
        let (connection, updates, item) = (self.connection.clone(), self.updates.clone(), item.to_owned());
        run("eclipse-tray-menu", move || {
            let entries = menu::read(&connection, &target);
            let _ = updates.send(TrayUpdate::Menu { item, entries });
        });
    }

    /// The human picked `entry` from the item's menu.
    pub fn menu_click(&self, item: &str, entry: i32) {
        let Some(target) = self.target(item) else {
            return;
        };
        let connection = self.connection.clone();
        run("eclipse-tray-click", move || {
            let _ = menu::click(&connection, &target, entry);
        });
    }

    fn click(&self, item: &str, method: &'static str, x: i32, y: i32, menu_fallback: bool) {
        let Some(target) = self.target(item) else {
            return;
        };
        let (connection, updates, item) = (self.connection.clone(), self.updates.clone(), item.to_owned());
        run("eclipse-tray-click", move || {
            let Some(proxy) = uncached(&connection, &target.bus, &target.path, target.interface) else {
                return;
            };
            let unanswered = proxy.call_method(method, &(x, y)).is_err();
            if unanswered && menu_fallback && target.menu.is_some() {
                let entries = menu::read(&connection, &target);
                let _ = updates.send(TrayUpdate::Menu { item, entries });
            }
        });
    }

    fn target(&self, item: &str) -> Option<Target> {
        self.targets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(item)
            .cloned()
    }
}

/// Start the watcher and host. Fails only if the session bus is unreachable.
/// Another watcher already running is not a failure: we host against it and
/// queue for the name.
pub fn spawn() -> zbus::Result<(Tray, TrayUpdates)> {
    start(true)
}

/// Host against whichever watcher owns the name, without ever serving or
/// requesting it: a read-only view of the tray someone else runs. Nothing
/// shows until a watcher appears, and a new owner is picked up when the name
/// changes hands. Fails only if the session bus is unreachable.
pub fn observe() -> zbus::Result<TrayUpdates> {
    start(false).map(|(_, updates)| updates)
}

fn start(serve: bool) -> zbus::Result<(Tray, TrayUpdates)> {
    // An app that stops answering costs one read two seconds, not the tray.
    // Nothing here waits on a human, so nothing needs longer.
    let builder = connection::Builder::session()?.method_timeout(Duration::from_secs(2));
    let connection = if serve {
        builder.serve_at(WATCHER_PATH, Watcher::default())?
    } else {
        builder
    }
    .build()?;
    if serve {
        // Queue, and never replace: if a watcher is already running we use
        // it, and the bus hands us the name the moment it exits. Not
        // `Default::default()`, which is `ReplaceExisting | DoNotQueue` and
        // would take the name from a running watcher and leave it unowned
        // once we exit. Allowing replacement means a watcher that insists
        // on the name gets it, and we queue behind it again.
        let _: RequestNameReply =
            connection.request_name_with_flags(WATCHER_NAME, RequestNameFlags::AllowReplacement.into())?;
    }

    let host = format!("org.kde.StatusNotifierHost-{}", std::process::id());
    let _ = connection.request_name_with_flags(host.as_str(), RequestNameFlags::DoNotQueue.into());

    let (tx, updates) = mpsc::channel();
    let targets = Arc::new(Mutex::new(HashMap::new()));
    let tray = Tray {
        connection: connection.clone(),
        targets: Arc::clone(&targets),
        updates: tx.clone(),
    };
    let updates = TrayUpdates {
        updates,
        connection: connection.clone(),
    };

    let signals = listen(&connection)?;
    thread::Builder::new()
        .name("eclipse-tray".to_owned())
        .spawn(move || host_loop(&connection, &host, &signals, &targets, &tx))
        .map_err(|error| zbus::Error::Failure(error.to_string()))?;
    Ok((tray, updates))
}

/// Re-read at least this often, whatever the bus says.
const FLOOR: Duration = Duration::from_secs(30);
/// Swallow the rest of a burst (an app registering sends several signals, and
/// an animated icon sends many) before reading.
const DEBOUNCE: Duration = Duration::from_millis(100);
/// More than any bar can draw. Past this an app is flooding the registry.
const MAX_ITEMS: usize = 64;

fn host_loop(
    connection: &Connection,
    host: &str,
    signals: &Receiver<Message>,
    targets: &Mutex<HashMap<String, Target>>,
    updates: &Sender<TrayUpdate>,
) {
    register_host(connection, host);
    let mut last = None;
    loop {
        let (items, read_targets) = read_items(connection);
        *targets.lock().unwrap_or_else(PoisonError::into_inner) = read_targets;
        if last.as_ref() != Some(&items) {
            if updates.send(TrayUpdate::Items(items.clone())).is_err() {
                return; // The GUI is gone.
            }
            last = Some(items);
        }

        match signals.recv_timeout(FLOOR) {
            Ok(message) => {
                handle(connection, host, &message);
                thread::sleep(DEBOUNCE);
                while let Ok(message) = signals.try_recv() {
                    handle(connection, host, &message);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            // Every pump ended: the connection closed, because the GUI
            // dropped its end or the session bus hung up.
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Everything in the tray right now, and where each item's clicks go.
fn read_items(connection: &Connection) -> (Vec<TrayItem>, HashMap<String, Target>) {
    let entries = uncached(connection, WATCHER_NAME, WATCHER_PATH, WATCHER_INTERFACE)
        .and_then(|watcher| {
            watcher
                .get_property::<Vec<String>>("RegisteredStatusNotifierItems")
                .ok()
        })
        .unwrap_or_default();
    let mut items = Vec::new();
    let mut targets = HashMap::new();
    for entry in entries.iter().take(MAX_ITEMS) {
        let Some((item, target)) = item::read(connection, entry) else {
            continue;
        };
        // By id, the first instance wins, and by address every one is reachable.
        targets.entry(item.id.clone()).or_insert_with(|| target.clone());
        targets.insert(item.address.clone(), target);
        items.push(item);
    }
    (items, targets)
}

/// Tell whoever owns the watcher name that we are listening. Apps check
/// `IsStatusNotifierHostRegistered` before they show an item at all.
fn register_host(connection: &Connection, host: &str) {
    if let Some(watcher) = uncached(connection, WATCHER_NAME, WATCHER_PATH, WATCHER_INTERFACE) {
        let _ = watcher.call_method("RegisterStatusNotifierHost", &(host,));
    }
}

/// Bookkeeping a signal asks for before the re-read: clearing out items whose
/// app left the bus, and re-registering as a host with a new watcher.
fn handle(connection: &Connection, host: &str, message: &Message) {
    let header = message.header();
    if header.member().map(|member| member.as_str()) != Some("NameOwnerChanged") {
        return;
    }
    let Ok((name, _old, new)) = message.body().deserialize::<(String, String, String)>() else {
        return;
    };
    if name == WATCHER_NAME {
        if !new.is_empty() {
            register_host(connection, host);
        }
        return;
    }
    if new.is_empty() {
        forget(connection, &name);
    }
}

/// `name` left the bus: drop what it registered with our watcher, if ours is
/// the one serving, and say so as the spec asks.
fn forget(connection: &Connection, name: &str) {
    let Ok(watcher) = connection.object_server().interface::<_, Watcher>(WATCHER_PATH) else {
        return;
    };
    let forgotten = watcher.get_mut().forget(name);
    let emitter = watcher.signal_emitter();
    for entry in &forgotten.items {
        let _ = zbus::block_on(Watcher::status_notifier_item_unregistered(emitter, entry));
    }
    if !forgotten.items.is_empty() {
        let _ = zbus::block_on(watcher.get().registered_status_notifier_items_changed(emitter));
    }
    if forgotten.last_host {
        let _ = zbus::block_on(Watcher::status_notifier_host_unregistered(emitter));
        let _ = zbus::block_on(watcher.get().is_status_notifier_host_registered_changed(emitter));
    }
}

/// Subscribe to everything that can change the tray, drained by pumps that do
/// nothing else, for the same reason as the status watchers: a full match
/// queue parks the socket reader, and with it every reply the host is
/// waiting on.
fn listen(connection: &Connection) -> zbus::Result<Receiver<Message>> {
    let signal = || MatchRule::builder().msg_type(Type::Signal);
    let rules = [
        // Items coming and going, from whoever owns the watcher name.
        signal()
            .sender(WATCHER_NAME)?
            .interface(WATCHER_INTERFACE)?
            .build(),
        // Any item's icon, title, status or menu changing.
        signal().interface("org.kde.StatusNotifierItem")?.build(),
        // Apps leaving the bus, and the watcher changing hands.
        signal()
            .sender("org.freedesktop.DBus")?
            .interface("org.freedesktop.DBus")?
            .member("NameOwnerChanged")?
            .build(),
    ];
    let (tx, rx) = mpsc::channel();
    for rule in rules {
        let messages = MessageIterator::for_match_rule(rule, connection, Some(64))?;
        let tx = tx.clone();
        thread::Builder::new()
            .name("eclipse-tray-signals".to_owned())
            .spawn(move || {
                for message in messages {
                    let Ok(message) = message else { return };
                    if tx.send(message).is_err() {
                        return;
                    }
                }
            })
            .map_err(|error| zbus::Error::Failure(error.to_string()))?;
    }
    Ok(rx)
}

fn run(name: &str, work: impl FnOnce() + Send + 'static) {
    // A click we cannot start a thread for is a click that does nothing, the
    // same as an app that did not answer.
    drop(thread::Builder::new().name(name.to_owned()).spawn(work));
}
