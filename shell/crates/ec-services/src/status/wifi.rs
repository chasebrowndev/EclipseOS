// SPDX-License-Identifier: AGPL-3.0-only
//! NetworkManager's wifi, both directions: the picker's list and the actions
//! behind it. Plain D-Bus, no `nmcli` — a subprocess is a parser we do not
//! control and an argv a secret would show up in.
//!
//! Every function here blocks on the bus. They are only ever called from an
//! action thread (see `actions.rs`), never from a view.

use std::collections::{HashMap, HashSet};
use std::net::Ipv6Addr;
use std::time::{Duration, Instant};

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

use super::network::{proxy, NM, NM_PATH};
use super::{ConnectError, Secret};

const SETTINGS_PATH: &str = "/org/freedesktop/NetworkManager/Settings";
const SETTINGS: &str = "org.freedesktop.NetworkManager.Settings";
const SETTINGS_CONNECTION: &str = "org.freedesktop.NetworkManager.Settings.Connection";
const DEVICE: &str = "org.freedesktop.NetworkManager.Device";
const WIRELESS: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const ACCESS_POINT: &str = "org.freedesktop.NetworkManager.AccessPoint";
const ACTIVE: &str = "org.freedesktop.NetworkManager.Connection.Active";
const WIFI_TYPE: &str = "802-11-wireless";
const SECURITY: &str = "802-11-wireless-security";

/// `NM_DEVICE_TYPE_WIFI`.
const DEVICE_TYPE_WIFI: u32 = 2;
/// `NM_ACTIVE_CONNECTION_STATE_*`.
const ACTIVATED: u32 = 2;
const DEACTIVATING: u32 = 3;
const DEACTIVATED: u32 = 4;
/// `NM_DEVICE_STATE_REASON_*` that mean "the secret was missing or wrong":
/// no secrets, and the supplicant giving up during the handshake.
const SECRET_REASONS: [u32; 4] = [7, 8, 10, 11];

/// `NM_802_11_AP_FLAGS_PRIVACY`, and the key-management bits of
/// `NM_802_11_AP_SEC_*` (the same in `WpaFlags` and `RsnFlags`).
const AP_PRIVACY: u32 = 0x1;
const SEC_PSK: u32 = 0x100;
const SEC_8021X: u32 = 0x200;
const SEC_SAE: u32 = 0x400;
const SEC_OWE: u32 = 0x800;

/// How long a join may take before we call it failed. Long enough for a slow
/// DHCP server; a join that is still going after this is not going to finish.
const JOIN_TIMEOUT: Duration = Duration::from_secs(45);
const JOIN_POLL: Duration = Duration::from_millis(250);

/// One network in the picker. Strength decides the order and is then dropped:
/// the drawer does not draw bars, and a field it would ignore is a field that
/// changes on every read and defeats the equality check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiNetwork {
    pub ssid: String,
    /// Joining needs a secret we do not have (or cannot use — enterprise).
    pub secured: bool,
    /// A saved profile exists, so joining needs no prompt.
    pub known: bool,
    pub active: bool,
}

/// What an access point asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    Open,
    /// OWE: encrypted, but no secret to type.
    EnhancedOpen,
    Wep,
    /// WPA/WPA2 personal, including WPA2/WPA3 transition networks.
    WpaPersonal,
    /// SAE only.
    Wpa3Personal,
    /// 802.1X. Listed, never joined from the picker.
    Enterprise,
}

impl Security {
    fn of(flags: u32, wpa: u32, rsn: u32) -> Self {
        let keys = wpa | rsn;
        if keys & SEC_8021X != 0 {
            Self::Enterprise
        } else if keys & SEC_PSK != 0 {
            Self::WpaPersonal
        } else if keys & SEC_SAE != 0 {
            Self::Wpa3Personal
        } else if keys & SEC_OWE != 0 {
            Self::EnhancedOpen
        } else if flags & AP_PRIVACY != 0 {
            Self::Wep
        } else {
            Self::Open
        }
    }

    fn needs_secret(self) -> bool {
        !matches!(self, Self::Open | Self::EnhancedOpen)
    }
}

/// Settings → Network's view of the current wifi link. Everything the picker
/// leaves out. `None` fields are ones NetworkManager did not report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiDetail {
    pub ssid: String,
    pub interface: String,
    /// Percent.
    pub signal: u8,
    pub frequency_mhz: u32,
    pub security: Security,
    /// `address/prefix`.
    pub ipv4: Vec<String>,
    pub ipv6: Vec<String>,
    pub gateway: Option<String>,
    pub dns: Vec<String>,
    pub mac: String,
    pub link_speed_mbps: Option<u32>,
    /// Bytes per second, sampled over one second when the detail is read.
    pub rx_rate: u64,
    pub tx_rate: u64,
}

impl WifiDetail {
    /// "2.4 GHz", "5 GHz", "6 GHz" — derived, so there is no second field to
    /// disagree with the frequency.
    pub fn band(&self) -> &'static str {
        match self.frequency_mhz {
            2400..=2500 => "2.4 GHz",
            5925..=7125 => "6 GHz",
            4900..=5924 => "5 GHz",
            _ => "",
        }
    }
}

// --- reads -----------------------------------------------------------------

pub(super) fn enabled(connection: &Connection) -> Option<bool> {
    proxy(connection, NM_PATH, NM)?
        .get_property("WirelessEnabled")
        .ok()
}

pub(super) fn set_enabled(connection: &Connection, on: bool) -> Result<(), String> {
    let manager = proxy(connection, NM_PATH, NM).ok_or("NetworkManager is not running")?;
    manager
        .set_property("WirelessEnabled", on)
        .map_err(|error| error.to_string())
}

/// Every network in range, one row per SSID, active first then strongest
/// first. Hidden networks (empty SSID) are not listed.
pub(super) fn networks(connection: &Connection) -> Vec<WifiNetwork> {
    let known = saved(connection)
        .into_iter()
        .map(|profile| profile.ssid)
        .collect::<HashSet<_>>();
    let mut rows = access_points(connection)
        .into_iter()
        .map(|ap| {
            let row = WifiNetwork {
                known: known.contains(&ap.ssid),
                secured: ap.security.needs_secret(),
                active: ap.active,
                ssid: ap.ssid,
            };
            (row, ap.strength)
        })
        .collect::<Vec<_>>();
    rows.sort_by(|(a, sa), (b, sb)| b.active.cmp(&a.active).then(sb.cmp(sa)));
    let mut seen = HashSet::new();
    rows.into_iter()
        .filter(|(row, _)| seen.insert(row.ssid.clone()))
        .map(|(row, _)| row)
        .collect()
}

/// The SSIDs of every saved wifi profile, for Settings' forget list.
pub(super) fn saved_ssids(connection: &Connection) -> Vec<String> {
    let mut ssids = saved(connection)
        .into_iter()
        .map(|profile| profile.ssid)
        .collect::<Vec<_>>();
    ssids.sort();
    ssids.dedup();
    ssids
}

/// Ask every wifi device to scan, then wait (bounded) for the scan to land.
/// NetworkManager refuses a scan it thinks is too soon after the last; that
/// is not an error for us, the list we then read is simply the recent one.
pub(super) fn scan(connection: &Connection) {
    let devices = wifi_devices(connection);
    let before = devices
        .iter()
        .map(|device| last_scan(connection, device))
        .collect::<Vec<_>>();
    for device in &devices {
        if let Some(wireless) = proxy(connection, device.as_str(), WIRELESS) {
            let options: HashMap<&str, Value<'_>> = HashMap::new();
            let _ = wireless.call_method("RequestScan", &(options,));
        }
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let moved = devices
            .iter()
            .zip(&before)
            .any(|(device, was)| last_scan(connection, device) != *was);
        if moved {
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

pub(super) fn detail(connection: &Connection) -> Option<WifiDetail> {
    let device = wifi_devices(connection).into_iter().find(|device| {
        proxy(connection, device.as_str(), WIRELESS)
            .and_then(|w| w.get_property::<OwnedObjectPath>("ActiveAccessPoint").ok())
            .is_some_and(|ap| ap.as_str() != "/")
    })?;
    let dev = proxy(connection, device.as_str(), DEVICE)?;
    let wireless = proxy(connection, device.as_str(), WIRELESS)?;
    let ap_path: OwnedObjectPath = wireless.get_property("ActiveAccessPoint").ok()?;
    let ap = read_ap(connection, ap_path.as_str(), false)?;
    let interface: String = dev.get_property("Interface").ok()?;

    let (ipv4, gateway4, dns4) = ip_config(connection, &dev, "Ip4Config", false);
    let (ipv6, gateway6, dns6) = ip_config(connection, &dev, "Ip6Config", true);
    let (rx_rate, tx_rate) = rates(&interface);

    Some(WifiDetail {
        ssid: ap.ssid,
        signal: ap.strength,
        frequency_mhz: ap.frequency,
        security: ap.security,
        ipv4,
        ipv6,
        gateway: gateway4.or(gateway6),
        dns: dns4.into_iter().chain(dns6).collect(),
        mac: dev.get_property("HwAddress").unwrap_or_default(),
        link_speed_mbps: wireless
            .get_property::<u32>("Bitrate")
            .ok()
            .filter(|&kbit| kbit > 0)
            .map(|kbit| kbit / 1000),
        rx_rate,
        tx_rate,
        interface,
    })
}

// --- actions ---------------------------------------------------------------

/// Join `ssid`. A saved profile is activated as-is; an open network gets a
/// fresh profile; a secured network we have never joined is `NeedsSecret`,
/// and the caller hands it to the secret prompt.
pub(super) fn connect(connection: &Connection, ssid: &str) -> Result<(), ConnectError> {
    let device = first_wifi_device(connection)?;
    let ap = best_ap(connection, ssid);
    if let Some(profile) = saved(connection).into_iter().find(|p| p.ssid == ssid) {
        let active = activate(connection, &profile.path, &device, ap.as_ref())?;
        return wait(connection, &active, &device, false);
    }
    let ap = ap.ok_or_else(|| ConnectError::Failed("network is out of range".to_owned()))?;
    if ap.security.needs_secret() {
        return Err(ConnectError::NeedsSecret);
    }
    let (profile, active) = add_and_activate(connection, ssid, ap.security, None, &device, &ap)?;
    let joined = wait(connection, &active, &device, false);
    if joined.is_err() {
        delete(connection, &profile);
    }
    joined
}

/// Join `ssid` with a passphrase the human just typed. A saved profile has its
/// secret replaced; otherwise a new profile is made, and deleted again if the
/// secret turns out wrong so a typo does not leave a broken network behind.
pub(super) fn connect_with_secret(
    connection: &Connection,
    ssid: &str,
    secret: &Secret,
) -> Result<(), ConnectError> {
    let device = first_wifi_device(connection)?;
    let ap = best_ap(connection, ssid);
    if let Some(profile) = saved(connection).into_iter().find(|p| p.ssid == ssid) {
        let security = ap.as_ref().map_or(Security::WpaPersonal, |ap| ap.security);
        update_secret(connection, &profile.path, security, secret)?;
        let active = activate(connection, &profile.path, &device, ap.as_ref())?;
        return wait(connection, &active, &device, true);
    }
    let ap = ap.ok_or_else(|| ConnectError::Failed("network is out of range".to_owned()))?;
    let (profile, active) = add_and_activate(connection, ssid, ap.security, Some(secret), &device, &ap)?;
    let joined = wait(connection, &active, &device, true);
    if matches!(joined, Err(ConnectError::WrongSecret)) {
        delete(connection, &profile);
    }
    joined
}

/// Drop the wifi link. NetworkManager will not auto-reconnect until asked,
/// which is what a human who clicked "Disconnect" means.
pub(super) fn disconnect(connection: &Connection) -> Result<(), String> {
    let mut any = false;
    for device in wifi_devices(connection) {
        let Some(dev) = proxy(connection, device.as_str(), DEVICE) else {
            continue;
        };
        let active: Option<OwnedObjectPath> = dev.get_property("ActiveConnection").ok();
        if active.is_some_and(|a| a.as_str() != "/") {
            dev.call_method("Disconnect", &())
                .map_err(|error| error.to_string())?;
            any = true;
        }
    }
    if any {
        Ok(())
    } else {
        Err("not connected".to_owned())
    }
}

/// Delete every saved profile for `ssid`.
pub(super) fn forget(connection: &Connection, ssid: &str) -> Result<(), String> {
    let profiles = saved(connection)
        .into_iter()
        .filter(|p| p.ssid == ssid)
        .collect::<Vec<_>>();
    if profiles.is_empty() {
        return Err("no saved network by that name".to_owned());
    }
    for profile in profiles {
        let settings = proxy(connection, profile.path.as_str(), SETTINGS_CONNECTION)
            .ok_or("NetworkManager is not running")?;
        settings
            .call_method("Delete", &())
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

// --- plumbing --------------------------------------------------------------

struct AccessPoint {
    path: OwnedObjectPath,
    ssid: String,
    strength: u8,
    frequency: u32,
    security: Security,
    active: bool,
}

struct Profile {
    path: OwnedObjectPath,
    ssid: String,
}

type Settings = HashMap<String, HashMap<String, OwnedValue>>;

fn wifi_devices(connection: &Connection) -> Vec<OwnedObjectPath> {
    let Some(manager) = proxy(connection, NM_PATH, NM) else {
        return Vec::new();
    };
    let devices: Vec<OwnedObjectPath> = manager.call("GetDevices", &()).unwrap_or_default();
    devices
        .into_iter()
        .filter(|device| {
            proxy(connection, device.as_str(), DEVICE).and_then(|d| d.get_property::<u32>("DeviceType").ok())
                == Some(DEVICE_TYPE_WIFI)
        })
        .collect()
}

fn first_wifi_device(connection: &Connection) -> Result<OwnedObjectPath, ConnectError> {
    wifi_devices(connection)
        .into_iter()
        .next()
        .ok_or_else(|| ConnectError::Failed("no wifi device".to_owned()))
}

fn last_scan(connection: &Connection, device: &OwnedObjectPath) -> Option<i64> {
    proxy(connection, device.as_str(), WIRELESS)?
        .get_property("LastScan")
        .ok()
}

fn access_points(connection: &Connection) -> Vec<AccessPoint> {
    let mut out = Vec::new();
    for device in wifi_devices(connection) {
        let Some(wireless) = proxy(connection, device.as_str(), WIRELESS) else {
            continue;
        };
        let active: Option<OwnedObjectPath> = wireless.get_property("ActiveAccessPoint").ok();
        let aps: Vec<OwnedObjectPath> = wireless.call("GetAllAccessPoints", &()).unwrap_or_default();
        for ap in aps {
            let is_active = active.as_ref() == Some(&ap);
            if let Some(ap) = read_ap(connection, ap.as_str(), is_active) {
                if !ap.ssid.is_empty() {
                    out.push(ap);
                }
            }
        }
    }
    out
}

/// One `GetAll` per access point rather than a round trip per property: a
/// busy city block is forty of them.
fn read_ap(connection: &Connection, path: &str, active: bool) -> Option<AccessPoint> {
    let all = get_all(connection, path, ACCESS_POINT)?;
    let u32_of = |key: &str| all.get(key).and_then(|v| v.downcast_ref::<u32>().ok());
    let ssid = all
        .get("Ssid")
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| Vec::<u8>::try_from(v).ok())?;
    Some(AccessPoint {
        path: OwnedObjectPath::try_from(path).ok()?,
        ssid: String::from_utf8_lossy(&ssid).into_owned(),
        strength: all
            .get("Strength")
            .and_then(|v| v.downcast_ref::<u8>().ok())
            .unwrap_or(0)
            .min(100),
        frequency: u32_of("Frequency").unwrap_or(0),
        security: Security::of(
            u32_of("Flags").unwrap_or(0),
            u32_of("WpaFlags").unwrap_or(0),
            u32_of("RsnFlags").unwrap_or(0),
        ),
        active,
    })
}

fn best_ap(connection: &Connection, ssid: &str) -> Option<AccessPoint> {
    access_points(connection)
        .into_iter()
        .filter(|ap| ap.ssid == ssid)
        .max_by_key(|ap| ap.strength)
}

fn get_all(connection: &Connection, path: &str, interface: &str) -> Option<HashMap<String, OwnedValue>> {
    proxy(connection, path, "org.freedesktop.DBus.Properties")?
        .call("GetAll", &(interface,))
        .ok()
}

/// Saved wifi profiles. `GetSettings` never returns secrets, so nothing read
/// here is sensitive.
fn saved(connection: &Connection) -> Vec<Profile> {
    let Some(settings) = proxy(connection, SETTINGS_PATH, SETTINGS) else {
        return Vec::new();
    };
    let paths: Vec<OwnedObjectPath> = settings.call("ListConnections", &()).unwrap_or_default();
    paths
        .into_iter()
        .filter_map(|path| {
            let profile: Settings = proxy(connection, path.as_str(), SETTINGS_CONNECTION)?
                .call("GetSettings", &())
                .ok()?;
            let kind = profile
                .get("connection")?
                .get("type")?
                .downcast_ref::<&str>()
                .ok()?;
            if kind != WIFI_TYPE {
                return None;
            }
            let ssid = profile
                .get(WIFI_TYPE)?
                .get("ssid")?
                .try_clone()
                .ok()
                .and_then(|v| Vec::<u8>::try_from(v).ok())?;
            Some(Profile {
                path,
                ssid: String::from_utf8_lossy(&ssid).into_owned(),
            })
        })
        .collect()
}

fn activate(
    connection: &Connection,
    profile: &OwnedObjectPath,
    device: &OwnedObjectPath,
    ap: Option<&AccessPoint>,
) -> Result<OwnedObjectPath, ConnectError> {
    let manager = proxy(connection, NM_PATH, NM).ok_or_else(nm_down)?;
    let specific = ap.map_or_else(root, |ap| ap.path.clone());
    manager
        .call("ActivateConnection", &(profile, device, specific))
        .map_err(failed)
}

/// `key-mgmt` for a network we are about to make a profile for. Enterprise
/// and WEP need more than one passphrase field and are refused here rather
/// than half-configured.
fn key_mgmt(security: Security) -> Result<Option<&'static str>, ConnectError> {
    match security {
        Security::Open => Ok(None),
        Security::EnhancedOpen => Ok(Some("owe")),
        Security::WpaPersonal => Ok(Some("wpa-psk")),
        Security::Wpa3Personal => Ok(Some("sae")),
        Security::Wep | Security::Enterprise => Err(ConnectError::Failed(
            "this network's security is not supported here".to_owned(),
        )),
    }
}

fn add_and_activate(
    connection: &Connection,
    ssid: &str,
    security: Security,
    secret: Option<&Secret>,
    device: &OwnedObjectPath,
    ap: &AccessPoint,
) -> Result<(OwnedObjectPath, OwnedObjectPath), ConnectError> {
    let manager = proxy(connection, NM_PATH, NM).ok_or_else(nm_down)?;
    let mut settings: HashMap<&str, HashMap<&str, Value<'_>>> = HashMap::new();
    settings.insert(
        "connection",
        HashMap::from([("type", Value::from(WIFI_TYPE)), ("id", Value::from(ssid))]),
    );
    settings.insert(
        WIFI_TYPE,
        HashMap::from([
            ("ssid", Value::from(ssid.as_bytes().to_vec())),
            ("mode", Value::from("infrastructure")),
        ]),
    );
    if let Some(mgmt) = key_mgmt(security)? {
        let mut sec = HashMap::from([("key-mgmt", Value::from(mgmt))]);
        if let Some(secret) = secret {
            sec.insert("psk", Value::from(secret.expose()));
            // 0: NetworkManager stores it, so the next join needs no prompt.
            sec.insert("psk-flags", Value::from(0u32));
        }
        settings.insert(SECURITY, sec);
    }
    manager
        .call("AddAndActivateConnection", &(settings, device, &ap.path))
        .map_err(failed)
}

/// Put a new passphrase into a saved profile. `GetSettings` returns the
/// profile without secrets and `Update` replaces the whole thing, so the psk
/// is spliced into what came back.
fn update_secret(
    connection: &Connection,
    profile: &OwnedObjectPath,
    security: Security,
    secret: &Secret,
) -> Result<(), ConnectError> {
    let settings_proxy = proxy(connection, profile.as_str(), SETTINGS_CONNECTION).ok_or_else(nm_down)?;
    let current: Settings = settings_proxy.call("GetSettings", &()).map_err(failed)?;
    let mut settings: HashMap<String, HashMap<String, Value<'_>>> = current
        .into_iter()
        .map(|(group, values)| {
            let values = values
                .into_iter()
                .map(|(key, value)| (key, Value::from(value)))
                .collect();
            (group, values)
        })
        .collect();
    // The deprecated address arrays ride along in `GetSettings`; sending
    // them back next to the `-data` forms is ambiguous, so drop them.
    for group in ["ipv4", "ipv6"] {
        if let Some(values) = settings.get_mut(group) {
            values.remove("addresses");
            values.remove("routes");
        }
    }
    let sec = settings.entry(SECURITY.to_owned()).or_default();
    if !sec.contains_key("key-mgmt") {
        let mgmt = key_mgmt(security)?.unwrap_or("wpa-psk");
        sec.insert("key-mgmt".to_owned(), Value::from(mgmt));
    }
    sec.insert("psk".to_owned(), Value::from(secret.expose()));
    sec.insert("psk-flags".to_owned(), Value::from(0u32));
    settings_proxy
        .call_method("Update", &(settings,))
        .map(drop)
        .map_err(failed)
}

/// Poll the activation until it lands or dies. Polling rather than a signal
/// subscription: a subscription is a thread and a match rule that outlive
/// the join unless torn down carefully, and a quarter-second is well inside
/// what a human notices on a join that takes seconds anyway.
fn wait(
    connection: &Connection,
    active: &OwnedObjectPath,
    device: &OwnedObjectPath,
    had_secret: bool,
) -> Result<(), ConnectError> {
    let deadline = Instant::now() + JOIN_TIMEOUT;
    loop {
        let state =
            proxy(connection, active.as_str(), ACTIVE).and_then(|a| a.get_property::<u32>("State").ok());
        match state {
            Some(ACTIVATED) => return Ok(()),
            // The object vanishes once NetworkManager gives up on it.
            None | Some(DEACTIVATING) | Some(DEACTIVATED) => {
                return Err(why_failed(connection, device, had_secret))
            }
            Some(_) => {}
        }
        if Instant::now() >= deadline {
            return Err(ConnectError::Failed("timed out".to_owned()));
        }
        std::thread::sleep(JOIN_POLL);
    }
}

fn why_failed(connection: &Connection, device: &OwnedObjectPath, had_secret: bool) -> ConnectError {
    let reason = proxy(connection, device.as_str(), DEVICE)
        .and_then(|d| d.get_property::<(u32, u32)>("StateReason").ok())
        .map(|(_, reason)| reason);
    match reason {
        Some(reason) if SECRET_REASONS.contains(&reason) => {
            if had_secret {
                ConnectError::WrongSecret
            } else {
                ConnectError::NeedsSecret
            }
        }
        Some(reason) => ConnectError::Failed(format!("NetworkManager reason {reason}")),
        None => ConnectError::Failed("activation failed".to_owned()),
    }
}

fn delete(connection: &Connection, profile: &OwnedObjectPath) {
    if let Some(settings) = proxy(connection, profile.as_str(), SETTINGS_CONNECTION) {
        let _ = settings.call_method("Delete", &());
    }
}

/// Addresses, gateway and DNS from the device's `Ip4Config`/`Ip6Config`.
fn ip_config(
    connection: &Connection,
    device: &Proxy<'_>,
    property: &str,
    v6: bool,
) -> (Vec<String>, Option<String>, Vec<String>) {
    let empty = (Vec::new(), None, Vec::new());
    let Ok(path) = device.get_property::<OwnedObjectPath>(property) else {
        return empty;
    };
    if path.as_str() == "/" {
        return empty;
    }
    let interface = if v6 {
        "org.freedesktop.NetworkManager.IP6Config"
    } else {
        "org.freedesktop.NetworkManager.IP4Config"
    };
    let Some(config) = get_all(connection, path.as_str(), interface) else {
        return empty;
    };
    let addresses = dicts(config.get("AddressData"))
        .iter()
        .filter_map(|entry| {
            let address = entry.get("address")?.downcast_ref::<&str>().ok()?;
            let prefix = entry.get("prefix")?.downcast_ref::<u32>().ok()?;
            Some(format!("{address}/{prefix}"))
        })
        .collect();
    let gateway = config
        .get("Gateway")
        .and_then(|v| v.downcast_ref::<&str>().ok())
        .filter(|g| !g.is_empty())
        .map(str::to_owned);
    let dns = if v6 {
        config
            .get("Nameservers")
            .and_then(|v| v.try_clone().ok())
            .and_then(|v| Vec::<Vec<u8>>::try_from(v).ok())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|bytes| <[u8; 16]>::try_from(bytes).ok())
            .map(|octets| Ipv6Addr::from(octets).to_string())
            .collect()
    } else {
        dicts(config.get("NameserverData"))
            .iter()
            .filter_map(|entry| Some(entry.get("address")?.downcast_ref::<&str>().ok()?.to_owned()))
            .collect()
    };
    (addresses, gateway, dns)
}

fn dicts(value: Option<&OwnedValue>) -> Vec<HashMap<String, OwnedValue>> {
    value
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| Vec::<HashMap<String, OwnedValue>>::try_from(v).ok())
        .unwrap_or_default()
}

/// Bytes per second over one second, from the kernel's own counters.
fn rates(interface: &str) -> (u64, u64) {
    let read = |which: &str| -> Option<u64> {
        // The name came from NetworkManager, but it is still a path segment.
        if interface.contains('/') || interface.starts_with('.') {
            return None;
        }
        std::fs::read_to_string(format!("/sys/class/net/{interface}/statistics/{which}"))
            .ok()?
            .trim()
            .parse()
            .ok()
    };
    let (rx0, tx0) = (read("rx_bytes"), read("tx_bytes"));
    std::thread::sleep(Duration::from_secs(1));
    let (rx1, tx1) = (read("rx_bytes"), read("tx_bytes"));
    let rate = |a: Option<u64>, b: Option<u64>| match (a, b) {
        (Some(a), Some(b)) => b.saturating_sub(a),
        _ => 0,
    };
    (rate(rx0, rx1), rate(tx0, tx1))
}

fn root() -> OwnedObjectPath {
    OwnedObjectPath::from(ObjectPath::from_static_str_unchecked("/"))
}

fn nm_down() -> ConnectError {
    ConnectError::Failed("NetworkManager is not running".to_owned())
}

fn failed(error: zbus::Error) -> ConnectError {
    ConnectError::Failed(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_point_flags_map_to_what_joining_needs() {
        assert_eq!(Security::of(0, 0, 0), Security::Open);
        assert_eq!(Security::of(AP_PRIVACY, 0, 0), Security::Wep);
        assert_eq!(Security::of(AP_PRIVACY, 0, SEC_PSK), Security::WpaPersonal);
        // Transition networks join with a psk.
        assert_eq!(
            Security::of(AP_PRIVACY, 0, SEC_PSK | SEC_SAE),
            Security::WpaPersonal
        );
        assert_eq!(Security::of(AP_PRIVACY, 0, SEC_SAE), Security::Wpa3Personal);
        assert_eq!(Security::of(AP_PRIVACY, 0, SEC_OWE), Security::EnhancedOpen);
        assert_eq!(
            Security::of(AP_PRIVACY, SEC_8021X, SEC_8021X),
            Security::Enterprise
        );
        assert!(!Security::EnhancedOpen.needs_secret());
        assert!(Security::Wep.needs_secret());
    }

    #[test]
    fn frequencies_name_their_band() {
        let mut detail = WifiDetail {
            ssid: String::new(),
            interface: String::new(),
            signal: 0,
            frequency_mhz: 2437,
            security: Security::Open,
            ipv4: Vec::new(),
            ipv6: Vec::new(),
            gateway: None,
            dns: Vec::new(),
            mac: String::new(),
            link_speed_mbps: None,
            rx_rate: 0,
            tx_rate: 0,
        };
        assert_eq!(detail.band(), "2.4 GHz");
        detail.frequency_mhz = 5180;
        assert_eq!(detail.band(), "5 GHz");
        detail.frequency_mhz = 5955;
        assert_eq!(detail.band(), "6 GHz");
    }
}
