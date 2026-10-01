// SPDX-License-Identifier: AGPL-3.0-only
//! BlueZ: the bar cell's reading, and the picker's device list and actions.
//!
//! The action functions block on the bus and are only called from an action
//! thread (see `actions.rs`), never from a view.

use std::collections::HashMap;
use std::sync::mpsc::Sender;

use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use super::{signal_rule, uncached, watch_system, Update};

pub(super) const BLUEZ: &str = "org.bluez";
const ADAPTER: &str = "org.bluez.Adapter1";
const DEVICE: &str = "org.bluez.Device1";

/// Enough for one bar cell: whether the radio is on, and how many things are
/// actually talking to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Bluetooth {
    pub powered: bool,
    pub connected: usize,
}

/// One device in the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtDevice {
    /// `AA:BB:CC:DD:EE:FF` — what every action takes.
    pub addr: String,
    pub name: String,
    pub paired: bool,
    pub connected: bool,
    pub kind: BtKind,
}

/// Which glyph the picker draws, from BlueZ's `Icon` hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BtKind {
    Audio,
    Input,
    Phone,
    Other,
}

impl BtKind {
    fn from_icon(icon: &str) -> Self {
        if icon.starts_with("audio") {
            Self::Audio
        } else if icon.starts_with("input") {
            Self::Input
        } else if icon == "phone" {
            Self::Phone
        } else {
            Self::Other
        }
    }
}

pub(super) fn watch(updates: Sender<Update>) {
    watch_system(
        "eclipse-bt",
        signal_rule(BLUEZ, None, None),
        read,
        Update::Bluetooth,
        updates,
    );
}

/// A machine with no adapter, or no BlueZ at all, reads as an unpowered
/// radio — which is what the human sees anyway.
fn read(connection: &Connection) -> Bluetooth {
    let Some(objects) = managed_objects(connection) else {
        return Bluetooth::default();
    };

    let mut status = Bluetooth::default();
    for interfaces in objects.values() {
        if let Some(adapter) = interfaces.get(ADAPTER) {
            status.powered |= flag(adapter, "Powered");
        }
        if let Some(device) = interfaces.get(DEVICE) {
            if flag(device, "Connected") {
                status.connected += 1;
            }
        }
    }
    status
}

/// Every device BlueZ knows: paired ones, and whatever discovery has found.
/// Connected first, then paired, then by name. Unnamed devices found by a
/// scan (beacons, a neighbour's fridge) are left out — nothing a human can
/// pick by an address alone.
pub(super) fn devices(connection: &Connection) -> Vec<BtDevice> {
    let Some(objects) = managed_objects(connection) else {
        return Vec::new();
    };
    let mut devices = objects
        .values()
        .filter_map(|interfaces| {
            let device = interfaces.get(DEVICE)?;
            let addr = string(device, "Address")?;
            let paired = flag(device, "Paired");
            let name =
                string(device, "Name").or_else(|| (paired).then(|| string(device, "Alias")).flatten())?;
            Some(BtDevice {
                kind: BtKind::from_icon(&string(device, "Icon").unwrap_or_default()),
                connected: flag(device, "Connected"),
                paired,
                name,
                addr,
            })
        })
        .collect::<Vec<_>>();
    devices.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then(b.paired.cmp(&a.paired))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    devices
}

pub(super) fn set_powered(connection: &Connection, on: bool) -> Result<(), String> {
    let adapter = adapter(connection)?;
    let proxy = uncached(connection, BLUEZ, adapter.as_str(), ADAPTER).ok_or_else(bluez_down)?;
    proxy
        .set_property("Powered", on)
        .map_err(|error| error.to_string())
}

pub(super) fn set_discovering(connection: &Connection, on: bool) -> Result<(), String> {
    let adapter = adapter(connection)?;
    let proxy = uncached(connection, BLUEZ, adapter.as_str(), ADAPTER).ok_or_else(bluez_down)?;
    let method = if on { "StartDiscovery" } else { "StopDiscovery" };
    match proxy.call_method(method, &()) {
        Ok(_) => Ok(()),
        // Already in the state asked for: the human's intent holds.
        Err(zbus::Error::MethodError(name, _, _))
            if matches!(
                name.as_str(),
                "org.bluez.Error.InProgress" | "org.bluez.Error.NotReady"
            ) && on =>
        {
            Ok(())
        }
        Err(error) => Err(error.to_string()),
    }
}

pub(super) fn connect(connection: &Connection, addr: &str) -> Result<(), String> {
    device_call(connection, addr, "Connect")
}

pub(super) fn disconnect(connection: &Connection, addr: &str) -> Result<(), String> {
    device_call(connection, addr, "Disconnect")
}

/// Pair, trust, connect — what a human means by clicking an unpaired device.
/// Trust is what lets it reconnect later without asking again. Pair blocks
/// until the agent has had its answer, which may be a human typing a PIN.
pub(super) fn pair(connection: &Connection, addr: &str) -> Result<(), String> {
    let path = device_path(connection, addr)?;
    let device = uncached(connection, BLUEZ, path.as_str(), DEVICE).ok_or_else(bluez_down)?;
    if !device.get_property::<bool>("Paired").unwrap_or(false) {
        device
            .call_method("Pair", &())
            .map_err(|error| error.to_string())?;
    }
    let _ = device.set_property("Trusted", true);
    device
        .call_method("Connect", &())
        .map(drop)
        .map_err(|error| error.to_string())
}

/// Remove the device and its bond.
pub(super) fn forget(connection: &Connection, addr: &str) -> Result<(), String> {
    let path = device_path(connection, addr)?;
    let adapter = adapter(connection)?;
    let proxy = uncached(connection, BLUEZ, adapter.as_str(), ADAPTER).ok_or_else(bluez_down)?;
    proxy
        .call_method("RemoveDevice", &(path,))
        .map(drop)
        .map_err(|error| error.to_string())
}

type Objects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

/// One call for the whole tree: adapters and devices arrive together, so the
/// count and the radio state are always from the same instant.
fn managed_objects(connection: &Connection) -> Option<Objects> {
    uncached(connection, BLUEZ, "/", "org.freedesktop.DBus.ObjectManager")?
        .call("GetManagedObjects", &())
        .ok()
}

/// The first adapter. Machines with two are rare enough that picking one
/// quietly beats asking.
fn adapter(connection: &Connection) -> Result<OwnedObjectPath, String> {
    let objects = managed_objects(connection).ok_or_else(bluez_down)?;
    let mut adapters = objects
        .into_iter()
        .filter(|(_, interfaces)| interfaces.contains_key(ADAPTER))
        .map(|(path, _)| path)
        .collect::<Vec<_>>();
    adapters.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    adapters
        .into_iter()
        .next()
        .ok_or_else(|| "no bluetooth adapter".to_owned())
}

fn device_path(connection: &Connection, addr: &str) -> Result<OwnedObjectPath, String> {
    let objects = managed_objects(connection).ok_or_else(bluez_down)?;
    objects
        .into_iter()
        .find(|(_, interfaces)| {
            interfaces
                .get(DEVICE)
                .and_then(|device| string(device, "Address"))
                .is_some_and(|a| a.eq_ignore_ascii_case(addr))
        })
        .map(|(path, _)| path)
        .ok_or_else(|| "no such device".to_owned())
}

fn device_call(connection: &Connection, addr: &str, method: &str) -> Result<(), String> {
    let path = device_path(connection, addr)?;
    let proxy = uncached(connection, BLUEZ, path.as_str(), DEVICE).ok_or_else(bluez_down)?;
    proxy
        .call_method(method, &())
        .map(drop)
        .map_err(|error| error.to_string())
}

fn bluez_down() -> String {
    "bluetooth service is not running".to_owned()
}

fn flag(properties: &HashMap<String, OwnedValue>, name: &str) -> bool {
    properties
        .get(name)
        .and_then(|value| value.downcast_ref::<bool>().ok())
        .unwrap_or(false)
}

fn string(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<String> {
    properties
        .get(name)
        .and_then(|value| value.downcast_ref::<&str>().ok())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_hints_pick_a_glyph() {
        assert_eq!(BtKind::from_icon("audio-headset"), BtKind::Audio);
        assert_eq!(BtKind::from_icon("audio-card"), BtKind::Audio);
        assert_eq!(BtKind::from_icon("input-mouse"), BtKind::Input);
        assert_eq!(BtKind::from_icon("phone"), BtKind::Phone);
        assert_eq!(BtKind::from_icon(""), BtKind::Other);
    }
}
