// SPDX-License-Identifier: AGPL-3.0-only
//! The radio and tray models the drawers draw, and the actions they send.
//!
//! Both halves are `eclipse_services` (ADR 0053): `status::Actions` for wifi
//! and bluetooth, `tray` for StatusNotifierItems. They are started once per
//! bar process, on first use, and report back on their own channels, which the
//! compositor thread drains and converts into [`Feed`]s — so the icon lookups
//! a tray item needs happen there, never in `update` or the view. Under test
//! neither is started: the bar's tests never touch a bus.
//!
//! This module is not BlueZ's pairing agent. [`crate::pairing`] is (one agent
//! per session, not one per monitor), and it starts the secret prompt itself.
//!
//! Nothing in this module ever holds a secret. A network that needs one is
//! handed to `eclipse-secret-prompt`, a separate process whose whole surface
//! is `secret`, and the passphrase goes from there to the service without
//! passing through the bar.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, PoisonError};

use eclipse_services::status::{Actions, ConnectError, Event, Events};
use eclipse_services::tray::{MenuKind, Pixmap, Tray, TrayUpdate, TrayUpdates};
use iced::widget::image;

/// The service's own shapes, which already carry exactly what the drawers
/// draw: no strength, band or rate (those live in Settings → Network).
pub use eclipse_services::status::{BtDevice, BtKind, WifiNetwork};

/// One StatusNotifierItem. The icon is resolved when the item arrives, never
/// from the view — a theme walk in the draw path is a frame hitch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayItem {
    /// The app's own name, which `bar.tray.*` lists. Not unique.
    pub id: String,
    /// Unique while the item lives: what every action is sent to.
    pub address: String,
    pub title: String,
    pub icon: TrayIcon,
}

/// A tray item's art, resolved once in [`from_tray`]. The image handles are
/// built there too, never in the view: a handle made per frame is a fresh
/// id per frame, and the renderer re-uploads the texture every time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayIcon {
    /// A themed SVG: recoloured to the tray's ink by `parts::mark`.
    Svg(PathBuf),
    /// Pixels to draw as they are, dimmed to the ink's alpha: the item's own
    /// pixmap already silhouetted (see [`silhouette`]), or a themed PNG,
    /// which this process cannot recolour without decoding it.
    Image(image::Handle),
    /// Nothing found: the token placeholder, never a hole.
    None,
}

impl TrayIcon {
    /// A themed name wins over pixels, as the StatusNotifierItem spec asks;
    /// the pixmap is the fallback for apps that ship no theme icon.
    fn resolve(name: &str, theme_path: &str, pixmap: Option<&Pixmap>) -> Self {
        let themed = crate::icons::tray(name, theme_path);
        match (themed, pixmap) {
            (Some(path), _) if !is_raster(&path) => TrayIcon::Svg(path),
            (_, Some(p)) if p.width > 0 && p.height > 0 => TrayIcon::Image(silhouette(p)),
            (Some(path), _) => TrayIcon::Image(image::Handle::from_path(path)),
            (None, _) => TrayIcon::None,
        }
    }
}

fn is_raster(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("png") || e.eq_ignore_ascii_case("xpm"))
}

/// The pixmap as a white shape with its own alpha — what `parts::glyph` does
/// to an SVG, done to pixels. A vendor-coloured logo in the tray is the one
/// place the bar would lose its palette to a guest; this keeps it the bar's
/// ink, and the view's opacity supplies the ink's level.
fn silhouette(p: &Pixmap) -> image::Handle {
    let rgba: Vec<u8> = p
        .rgba
        .chunks_exact(4)
        .flat_map(|px| [u8::MAX, u8::MAX, u8::MAX, px[3]])
        .collect();
    image::Handle::from_rgba(p.width, p.height, rgba)
}

/// One line of an item's own menu (`com.canonical.dbusmenu`, flattened).
/// `enabled` is already false for anything but an [`MenuKind::Item`], so a
/// header or a rule can never be pressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuEntry {
    pub id: i32,
    pub label: String,
    pub enabled: bool,
    pub kind: MenuKind,
    /// `Some` for a checkbox or radio entry, with whether it is on.
    pub checked: Option<bool>,
}

/// Everything the radio drawers and the tray read that the one-line status
/// feed does not carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Radios {
    pub wifi_enabled: bool,
    pub networks: Vec<WifiNetwork>,
    pub devices: Vec<BtDevice>,
    /// Discovery is running, so unpaired devices in range are listed.
    pub scanning: bool,
    pub tray: Vec<TrayItem>,
}

impl Default for Radios {
    fn default() -> Self {
        Self {
            // A radio we cannot ask about is drawn on, not off: "off" would
            // be a claim, and the status feed's own `Network` already says
            // whether anything is connected.
            wifi_enabled: true,
            networks: Vec::new(),
            devices: Vec::new(),
            scanning: false,
            tray: Vec::new(),
        }
    }
}

impl Radios {
    pub fn active_network(&self) -> Option<&WifiNetwork> {
        self.networks.iter().find(|n| n.active)
    }

    pub fn network(&self, ssid: &str) -> Option<&WifiNetwork> {
        self.networks.iter().find(|n| n.ssid == ssid)
    }

    /// The fixture `HYPERION_PREVIEW` draws: a plausible room, so a drawer can
    /// be screenshotted on a machine with no service behind it.
    #[cfg(debug_assertions)]
    pub fn preview() -> Self {
        let net = |ssid: &str, secured, known, active| WifiNetwork {
            ssid: ssid.to_owned(),
            secured,
            known,
            active,
        };
        let dev = |addr: &str, name: &str, paired, connected, kind| BtDevice {
            addr: addr.to_owned(),
            name: name.to_owned(),
            paired,
            connected,
            kind,
        };
        Self {
            wifi_enabled: true,
            networks: vec![
                net("abyss-5g", true, true, true),
                net("Hollow Point", true, false, false),
                net("guest", false, false, false),
                net("NETGEAR-2F", true, false, false),
                net("xfinitywifi", false, false, false),
            ],
            devices: vec![
                dev("AA:00", "WH-1000XM5", true, true, BtKind::Audio),
                dev("AA:01", "MX Master 3S", true, false, BtKind::Input),
                dev("AA:02", "Pixel 9", true, false, BtKind::Phone),
                dev("AA:03", "JBL Flip 6", false, false, BtKind::Audio),
            ],
            scanning: true,
            tray: vec![
                TrayItem {
                    id: "org.syncthing".to_owned(),
                    address: "org.syncthing".to_owned(),
                    title: "Syncthing".to_owned(),
                    icon: crate::icons::symbolic("emblem-synchronizing")
                        .map_or(TrayIcon::None, TrayIcon::Svg),
                },
                TrayItem {
                    id: "org.keepassxc".to_owned(),
                    address: "org.keepassxc".to_owned(),
                    title: "KeePassXC".to_owned(),
                    icon: crate::icons::symbolic("dialog-password").map_or(TrayIcon::None, TrayIcon::Svg),
                },
                // No theme icon, only pixels: the path most vendor tray apps
                // take. A ring with a notch, in a colour the silhouette drops.
                TrayItem {
                    id: "com.vendor.app".to_owned(),
                    address: "com.vendor.app".to_owned(),
                    title: "Vendor App".to_owned(),
                    icon: TrayIcon::resolve("", "", Some(&preview_pixmap())),
                },
            ],
        }
    }
}

/// A tray menu with every kind of line: a header, ticked and unticked
/// checkboxes, a disabled entry and rules, as Syncthing-like apps send it.
#[cfg(debug_assertions)]
pub fn preview_menu() -> Vec<MenuEntry> {
    let line = |id, label: &str, kind, enabled, checked| MenuEntry {
        id,
        label: label.to_owned(),
        enabled: enabled && kind == MenuKind::Item,
        kind,
        checked,
    };
    vec![
        line(1, "Syncthing 1.29", MenuKind::Header, false, None),
        line(2, "Open web UI", MenuKind::Item, true, None),
        line(3, "Rescan all", MenuKind::Item, false, None),
        line(0, "", MenuKind::Separator, false, None),
        line(4, "Pause syncing", MenuKind::Item, true, Some(false)),
        line(5, "Start at login", MenuKind::Item, true, Some(true)),
        line(0, "", MenuKind::Separator, false, None),
        line(6, "Quit", MenuKind::Item, true, None),
    ]
}

/// A 22×22 ring with a notch cut at the top: art only a pixmap could carry.
#[cfg(debug_assertions)]
fn preview_pixmap() -> Pixmap {
    const SIDE: u32 = 22;
    let c = (SIDE as f32 - 1.0) / 2.0;
    let mut rgba = Vec::with_capacity((SIDE * SIDE * 4) as usize);
    for y in 0..SIDE {
        for x in 0..SIDE {
            let (dx, dy) = (x as f32 - c, y as f32 - c);
            let r = (dx * dx + dy * dy).sqrt();
            let notch = dx.abs() < 2.5 && dy < 0.0;
            let on = ((6.5..=10.0).contains(&r) && !notch) || r <= 3.0;
            rgba.extend_from_slice(&[0x33, 0x99, 0xff, if on { u8::MAX } else { 0 }]);
        }
    }
    Pixmap {
        width: SIDE,
        height: SIDE,
        rgba,
    }
}

/// What the services said, already in the bar's terms.
#[derive(Debug, Clone)]
pub enum Feed {
    WifiEnabled(bool),
    Networks(Vec<WifiNetwork>),
    Devices(Vec<BtDevice>),
    Scanning(bool),
    /// A join needs a passphrase the service does not have, or the saved one
    /// was refused: the prompt's job.
    NeedsSecret(String),
    Tray(Vec<TrayItem>),
    /// The menu a right-click (or a menu-only item's left click) asked for.
    Menu {
        item: String,
        entries: Vec<MenuEntry>,
    },
}

struct Services {
    actions: Option<Actions>,
    tray: Option<Tray>,
    /// The receiving ends, until the compositor thread takes them.
    feeds: Mutex<Option<Feeds>>,
}

/// The two service channels, owned by whichever thread drains them.
pub struct Feeds {
    events: Option<Events>,
    tray: Option<TrayUpdates>,
}

fn services() -> &'static Services {
    static SERVICES: OnceLock<Services> = OnceLock::new();
    SERVICES.get_or_init(start)
}

#[cfg(not(test))]
fn start() -> Services {
    // Each half is optional: no system bus means no radios, no session bus
    // means no tray, and neither takes the other down.
    let (actions, events) =
        match eclipse_services::status::actions(eclipse_services::status::PairingAgent::None) {
            Ok((a, e)) => (Some(a), Some(e)),
            Err(e) => {
                eprintln!("hyperion: radio actions unavailable: {e}");
                (None, None)
            }
        };
    let (tray, updates) = match eclipse_services::tray::spawn() {
        Ok((t, u)) => (Some(t), Some(u)),
        Err(e) => {
            eprintln!("hyperion: tray unavailable: {e}");
            (None, None)
        }
    };
    if let Some(actions) = actions.as_ref() {
        // The first reading; after this every list arrives because something
        // changed it.
        actions.wifi_networks();
        actions.bt_devices();
    }
    Services {
        actions,
        tray,
        feeds: Mutex::new(Some(Feeds {
            events,
            tray: updates,
        })),
    }
}

#[cfg(test)]
fn start() -> Services {
    Services {
        actions: None,
        tray: None,
        feeds: Mutex::new(None),
    }
}

/// Start the services if they are not yet, and hand over their channels.
/// `Some` exactly once per process.
pub fn take_feeds() -> Option<Feeds> {
    services()
        .feeds
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
}

impl Feeds {
    /// Everything waiting, converted. Never blocks.
    pub fn drain(&self) -> Vec<Feed> {
        let mut out = Vec::new();
        if let Some(events) = self.events.as_ref() {
            while let Some(event) = events.try_recv() {
                out.extend(from_event(event));
            }
        }
        if let Some(tray) = self.tray.as_ref() {
            while let Some(update) = tray.try_recv() {
                out.push(from_tray(update));
            }
        }
        out
    }
}

fn from_event(event: Event) -> Option<Feed> {
    match event {
        Event::WifiEnabled(on) => Some(Feed::WifiEnabled(on)),
        Event::WifiNetworks(networks) => Some(Feed::Networks(networks)),
        Event::BtDevices(devices) => Some(Feed::Devices(devices)),
        Event::BtDiscovering(on) => Some(Feed::Scanning(on)),
        Event::WifiConnect { ssid, result } => match result {
            Ok(()) => None,
            Err(ConnectError::NeedsSecret | ConnectError::WrongSecret) => Some(Feed::NeedsSecret(ssid)),
            // The SSID is not named: stderr is a journal, and which networks
            // the user tried is theirs.
            Err(ConnectError::Failed(reason)) => {
                eprintln!("hyperion: wifi join failed: {reason}");
                None
            }
        },
        Event::BtPair {
            result: Err(reason), ..
        } => {
            eprintln!("hyperion: bluetooth pairing failed: {reason}");
            None
        }
        Event::Failed { action, reason } => {
            eprintln!("hyperion: {action} failed: {reason}");
            None
        }
        // Settings' business, or the pairing agent's (`crate::pairing`).
        _ => None,
    }
}

fn from_tray(update: TrayUpdate) -> Feed {
    match update {
        TrayUpdate::Items(items) => Feed::Tray(
            items
                .into_iter()
                .map(|item| TrayItem {
                    icon: TrayIcon::resolve(
                        &item.icon_name,
                        &item.icon_theme_path,
                        item.icon_pixmap.as_ref(),
                    ),
                    id: item.id,
                    address: item.address,
                    title: item.title,
                })
                .collect(),
        ),
        TrayUpdate::Menu { item, entries } => Feed::Menu {
            item,
            entries: menu_entries(entries),
        },
    }
}

/// The service's menu in the sheet's terms. A rule at either end, or next to
/// another rule, divides nothing and is dropped — apps hide the entries
/// between two rules and leave both behind.
fn menu_entries(entries: Vec<eclipse_services::tray::MenuEntry>) -> Vec<MenuEntry> {
    let mut out: Vec<MenuEntry> = Vec::with_capacity(entries.len());
    for e in entries {
        let rule = e.kind == MenuKind::Separator;
        if rule && out.last().is_none_or(|last| last.kind == MenuKind::Separator) {
            continue;
        }
        out.push(MenuEntry {
            id: e.id,
            enabled: e.enabled && e.kind == MenuKind::Item,
            kind: e.kind,
            checked: e.checked,
            label: e.label,
        });
    }
    if out.last().is_some_and(|last| last.kind == MenuKind::Separator) {
        out.pop();
    }
    out
}

/// What the drawers and the tray send. Every call returns at once; the answer,
/// if any, comes back as a [`Feed`]. With no service behind it, a call does
/// nothing.
pub mod actions {
    use super::services;

    fn radio(f: impl FnOnce(&super::Actions)) {
        if let Some(a) = services().actions.as_ref() {
            f(a);
        }
    }

    fn tray(f: impl FnOnce(&super::Tray)) {
        if let Some(t) = services().tray.as_ref() {
            f(t);
        }
    }

    pub fn set_wifi_enabled(on: bool) {
        radio(|a| a.set_wifi_enabled(on));
    }
    pub fn connect(ssid: &str) {
        radio(|a| a.wifi_connect(ssid.to_owned()));
    }
    pub fn disconnect() {
        radio(|a| a.wifi_disconnect());
    }
    /// Look again: the wifi drawer opening is the moment a fresh list matters.
    pub fn scan() {
        radio(|a| a.wifi_scan());
    }
    pub fn set_bt_powered(on: bool) {
        radio(|a| a.bt_set_powered(on));
    }
    pub fn discover(on: bool) {
        radio(|a| a.bt_discover(on));
    }
    pub fn bt_connect(addr: &str) {
        radio(|a| a.bt_connect(addr.to_owned()));
    }
    pub fn bt_disconnect(addr: &str) {
        radio(|a| a.bt_disconnect(addr.to_owned()));
    }
    /// A PIN or passkey the device asks for goes to the pairing agent
    /// ([`crate::pairing`]), which starts the prompt; the bar never sees it.
    pub fn pair(addr: &str) {
        radio(|a| a.bt_pair(addr.to_owned()));
    }
    pub fn tray_activate(item: &str, x: i32, y: i32) {
        tray(|t| t.activate(item, x, y));
    }
    /// The entries arrive later, as [`super::Feed::Menu`].
    pub fn tray_menu(item: &str) {
        tray(|t| t.menu(item));
    }
    pub fn tray_menu_click(item: &str, entry: i32) {
        tray(|t| t.menu_click(item, entry));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(id: i32, kind: MenuKind) -> eclipse_services::tray::MenuEntry {
        eclipse_services::tray::MenuEntry {
            id,
            label: String::new(),
            enabled: true,
            kind,
            checked: None,
        }
    }

    /// A rule that divides nothing is dropped; a header is never pressable.
    #[test]
    fn stray_rules_are_dropped_and_headers_stay_inert() {
        use MenuKind::{Header, Item, Separator};
        let got = menu_entries(vec![
            raw(0, Separator),
            raw(1, Header),
            raw(2, Item),
            raw(0, Separator),
            raw(0, Separator),
            raw(3, Item),
            raw(0, Separator),
        ]);
        let kinds: Vec<_> = got.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, [Header, Item, Separator, Item]);
        assert!(!got[0].enabled);
        assert!(got[1].enabled);
    }

    /// The silhouette keeps the art's alpha and drops its colour.
    #[test]
    fn a_pixmap_becomes_the_ink_shape() {
        let icon = TrayIcon::resolve(
            "",
            "",
            Some(&Pixmap {
                width: 1,
                height: 1,
                rgba: vec![0x33, 0x99, 0xff, 0x80],
            }),
        );
        assert!(matches!(icon, TrayIcon::Image(_)));
        assert_eq!(TrayIcon::resolve("", "", None), TrayIcon::None);
    }
}
