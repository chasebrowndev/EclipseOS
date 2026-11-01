// SPDX-License-Identifier: AGPL-3.0-only
//! The radio and tray models the drawers draw, and the actions they send.
//!
//! Both halves are `ec_services` (ADR 0053): `status::Actions` for wifi
//! and bluetooth, `tray` for StatusNotifierItems. They are started once per
//! bar process, on first use, and report back on their own channels, which the
//! compositor thread drains and converts into [`Feed`]s — so the icon lookups
//! a tray item needs happen there, never in `update` or the view. Under test
//! neither is started: the bar's tests never touch a bus.
//!
//! This module is not BlueZ's pairing agent. `ec-pairing` is (one agent
//! per session, its own process), and it starts the secret prompt itself.
//!
//! Nothing in this module ever holds a secret. A network that needs one is
//! handed to `ec-secret-prompt`, a separate process whose whole surface
//! is `secret`, and the passphrase goes from there to the service without
//! passing through the bar.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, PoisonError};

use ec_services::status::{Actions, ConnectError, Event, Events};
use ec_services::tray::{MenuKind, Pixmap, Tray, TrayUpdate, TrayUpdates};
use iced::widget::image;

/// The service's own shapes, which already carry exactly what the drawers
/// draw: no strength, band or rate (those live in Settings → Network).
pub use ec_services::status::{BtDevice, BtKind, WifiNetwork};

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
    /// Bluetooth discovery is running, so unpaired devices in range are listed.
    pub scanning: bool,
    /// A wifi scan is in flight: the cached `networks` stay clickable, and
    /// fresh results stream in as they land. Set the moment the scan is
    /// asked for; cleared when the scan's last list arrives.
    pub wifi_scanning: bool,
    /// Lists still to come before the scan in flight is over (see
    /// [`Radios::start_wifi_scan`]).
    scan_lists: u8,
    /// The SSID being joined, from the click until the feed confirms it
    /// (`active` in the list) or fails. The row draws "connecting".
    pub connecting: Option<String>,
    /// The service said the join went through; the next list settles it.
    join_done: bool,
    /// The last join that failed, with the service's reason verbatim, for
    /// that SSID's row. Cleared by the next attempt at any network.
    pub join_error: Option<(String, String)>,
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
            wifi_scanning: false,
            scan_lists: 0,
            connecting: None,
            join_done: false,
            join_error: None,
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

    /// Whether `ssid` is the join in flight.
    pub fn is_joining(&self, ssid: &str) -> bool {
        self.connecting.as_deref() == Some(ssid)
    }

    /// Why the last join of `ssid` failed, if that is the one that did.
    pub fn failure(&self, ssid: &str) -> Option<&str> {
        self.join_error
            .as_ref()
            .filter(|(s, _)| s == ssid)
            .map(|(_, reason)| reason.as_str())
    }

    /// The wifi switch, flipped at once. The radio's own report corrects it
    /// if it disagrees. Turning wifi off ends whatever was in flight.
    pub fn set_wifi_enabled(&mut self, on: bool) {
        self.wifi_enabled = on;
        if !on {
            self.connecting = None;
            self.wifi_scanning = false;
            self.scan_lists = 0;
        }
    }

    /// A join was asked for: `ssid` reads as connecting until the feed says
    /// otherwise, and any earlier failure is stale.
    pub fn begin_join(&mut self, ssid: &str) {
        self.connecting = Some(ssid.to_owned());
        self.join_done = false;
        self.join_error = None;
    }

    /// A scan was asked for. The service answers with two lists — the one it
    /// has, then the one the scan found — and the scan is over after the
    /// second.
    pub fn start_wifi_scan(&mut self) {
        self.wifi_scanning = true;
        self.scan_lists = 2;
    }

    /// Fold one wifi report into the optimistic state. The lists and flags the
    /// feed itself carries are applied by `app::radio`.
    pub fn note(&mut self, feed: &Feed) {
        match feed {
            Feed::WifiEnabled(on) => {
                self.wifi_enabled = *on;
                if !on {
                    self.connecting = None;
                    self.wifi_scanning = false;
                    self.scan_lists = 0;
                }
            }
            Feed::Networks(networks) => {
                if self.scan_lists > 0 {
                    self.scan_lists -= 1;
                    self.wifi_scanning = self.scan_lists > 0;
                }
                // The join landed: the list says it is the active one, or the
                // service said it was done and this is the list after it.
                if self
                    .connecting
                    .as_deref()
                    .is_some_and(|j| self.join_done || networks.iter().any(|n| n.active && n.ssid == j))
                {
                    self.connecting = None;
                    self.join_done = false;
                }
            }
            // The list that follows settles it; until then the row keeps its
            // spinner.
            Feed::Joined(ssid) => self.join_done = self.is_joining(ssid),
            Feed::JoinFailed { ssid, reason } => {
                if self.is_joining(ssid) {
                    self.connecting = None;
                }
                self.join_error = Some((ssid.clone(), reason.clone()));
            }
            // The prompt takes over from here.
            Feed::NeedsSecret(ssid) if self.is_joining(ssid) => self.connecting = None,
            _ => {}
        }
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
            strength: 80u8.saturating_sub(ssid.len() as u8 * 3),
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
            wifi_scanning: true,
            scan_lists: 0,
            connecting: Some("guest".to_owned()),
            join_done: false,
            join_error: Some((
                "NETGEAR-2F".to_owned(),
                "no response from the access point".to_owned(),
            )),
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
    /// A join went through; the list that follows confirms it.
    Joined(String),
    /// A join failed for a reason other than a missing secret. The reason is
    /// the service's, verbatim, for that SSID's row.
    JoinFailed {
        ssid: String,
        reason: String,
    },
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
    let (actions, events) = match ec_services::status::actions(ec_services::status::PairingAgent::None) {
        Ok((a, e)) => (Some(a), Some(e)),
        Err(e) => {
            eprintln!("ec-hyperion-bar: radio actions unavailable: {e}");
            (None, None)
        }
    };
    let (tray, updates) = match ec_services::tray::spawn() {
        Ok((t, u)) => (Some(t), Some(u)),
        Err(e) => {
            eprintln!("ec-hyperion-bar: tray unavailable: {e}");
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
    /// Hand every report to `emit` the moment it arrives: one thread per
    /// service channel, each blocked on its receiver, so a result reaches the
    /// bar within a frame rather than at the next poll. `emit` returns false
    /// when nobody is listening any more, which ends the thread.
    pub fn pump(self, emit: impl Fn(Feed) -> bool + Clone + Send + 'static) {
        const SLICE: std::time::Duration = std::time::Duration::from_secs(1);
        if let Some(events) = self.events {
            let emit = emit.clone();
            std::thread::spawn(move || loop {
                let began = std::time::Instant::now();
                match events.recv_timeout(SLICE) {
                    Some(event) => {
                        if let Some(feed) = from_event(event) {
                            if !emit(feed) {
                                return;
                            }
                        }
                    }
                    // A timeout takes the whole slice; an instant `None` is a
                    // channel with no sender left.
                    None if began.elapsed() < SLICE / 2 => return,
                    None => {}
                }
            });
        }
        if let Some(tray) = self.tray {
            std::thread::spawn(move || loop {
                let began = std::time::Instant::now();
                match tray.recv_timeout(SLICE) {
                    Some(update) => {
                        if !emit(from_tray(update)) {
                            return;
                        }
                    }
                    None if began.elapsed() < SLICE / 2 => return,
                    None => {}
                }
            });
        }
    }
}

fn from_event(event: Event) -> Option<Feed> {
    match event {
        Event::WifiEnabled(on) => Some(Feed::WifiEnabled(on)),
        Event::WifiNetworks(networks) => Some(Feed::Networks(networks)),
        Event::BtDevices(devices) => Some(Feed::Devices(devices)),
        Event::BtDiscovering(on) => Some(Feed::Scanning(on)),
        Event::WifiConnect { ssid, result } => match result {
            Ok(()) => Some(Feed::Joined(ssid)),
            Err(ConnectError::NeedsSecret | ConnectError::WrongSecret) => Some(Feed::NeedsSecret(ssid)),
            // The SSID is not named in the journal: stderr is a journal, and
            // which networks the user tried is theirs. The row gets it.
            Err(ConnectError::Failed(reason)) => {
                eprintln!("ec-hyperion-bar: wifi join failed: {reason}");
                Some(Feed::JoinFailed { ssid, reason })
            }
        },
        Event::BtPair {
            result: Err(reason), ..
        } => {
            eprintln!("ec-hyperion-bar: bluetooth pairing failed: {reason}");
            None
        }
        Event::Failed { action, reason } => {
            eprintln!("ec-hyperion-bar: {action} failed: {reason}");
            None
        }
        // Settings' business, or the pairing agent's (`ec-pairing`).
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
fn menu_entries(entries: Vec<ec_services::tray::MenuEntry>) -> Vec<MenuEntry> {
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

    /// Whether a radio service is behind the actions. Without one a call does
    /// nothing, so the optimistic states (scanning, connecting) must not be
    /// entered: nothing would ever clear them.
    pub fn available() -> bool {
        services().actions.is_some()
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
    /// (`ec-pairing`), which starts the prompt; the bar never sees it.
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

    fn raw(id: i32, kind: MenuKind) -> ec_services::tray::MenuEntry {
        ec_services::tray::MenuEntry {
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

    fn net(ssid: &str, active: bool) -> WifiNetwork {
        WifiNetwork {
            ssid: ssid.to_owned(),
            secured: false,
            known: true,
            active,
            strength: 50,
        }
    }

    /// The switch flips at once, and off ends a join and a scan in flight.
    #[test]
    fn the_wifi_switch_is_optimistic_and_off_ends_what_was_pending() {
        let mut r = Radios::default();
        r.begin_join("home");
        r.start_wifi_scan();
        r.set_wifi_enabled(false);
        assert!(!r.wifi_enabled);
        assert!(r.connecting.is_none());
        assert!(!r.wifi_scanning);
    }

    /// A join reads as connecting until the list says it is active.
    #[test]
    fn a_join_is_connecting_until_the_list_confirms_it() {
        let mut r = Radios::default();
        r.begin_join("home");
        assert!(r.is_joining("home"));
        r.note(&Feed::Networks(vec![net("home", false)]));
        assert!(r.is_joining("home"), "not active yet");
        r.note(&Feed::Networks(vec![net("home", true)]));
        assert!(r.connecting.is_none());
    }

    /// The service's word that the join is done settles it on the next list.
    #[test]
    fn a_finished_join_settles_on_the_next_list() {
        let mut r = Radios::default();
        r.begin_join("home");
        r.note(&Feed::Joined("home".to_owned()));
        assert!(r.is_joining("home"));
        r.note(&Feed::Networks(vec![net("home", false)]));
        assert!(r.connecting.is_none());
    }

    /// A failure keeps its reason for that row, and the next attempt clears it.
    #[test]
    fn a_failed_join_keeps_its_reason_for_the_row() {
        let mut r = Radios::default();
        r.begin_join("home");
        r.note(&Feed::JoinFailed {
            ssid: "home".to_owned(),
            reason: "timed out".to_owned(),
        });
        assert!(r.connecting.is_none());
        assert_eq!(r.failure("home"), Some("timed out"));
        assert_eq!(r.failure("other"), None);
        r.begin_join("other");
        assert_eq!(r.failure("home"), None);
    }

    /// A scan is over after the service's second list, not the first.
    #[test]
    fn a_scan_lasts_until_its_second_list() {
        let mut r = Radios::default();
        r.start_wifi_scan();
        assert!(r.wifi_scanning);
        r.note(&Feed::Networks(Vec::new()));
        assert!(r.wifi_scanning, "that was the cached list");
        r.note(&Feed::Networks(Vec::new()));
        assert!(!r.wifi_scanning);
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
