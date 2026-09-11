// SPDX-License-Identifier: AGPL-3.0-only
//! BlueZ, read-only.

use std::collections::HashMap;
use std::sync::mpsc::Sender;

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use super::{watch_service, Update};

const BLUEZ: &str = "org.bluez";

/// Enough for one bar cell: whether the radio is on, and how many things are
/// actually talking to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Bluetooth {
    pub powered: bool,
    pub connected: usize,
}

pub(super) fn watch(connection: &Connection, updates: Sender<Update>) {
    watch_service("eclipse-bt", connection, BLUEZ, read, Update::Bluetooth, updates);
}

/// A machine with no adapter, or no BlueZ at all, reads as an unpowered
/// radio — which is what the human sees anyway.
fn read(connection: &Connection) -> Bluetooth {
    let Some(objects) = managed_objects(connection) else {
        return Bluetooth::default();
    };

    let mut status = Bluetooth::default();
    for interfaces in objects.values() {
        if let Some(adapter) = interfaces.get("org.bluez.Adapter1") {
            status.powered |= flag(adapter, "Powered");
        }
        if let Some(device) = interfaces.get("org.bluez.Device1") {
            if flag(device, "Connected") {
                status.connected += 1;
            }
        }
    }
    status
}

type Objects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

/// One call for the whole tree: adapters and devices arrive together, so the
/// count and the radio state are always from the same instant.
fn managed_objects(connection: &Connection) -> Option<Objects> {
    let proxy = Proxy::new(connection, BLUEZ, "/", "org.freedesktop.DBus.ObjectManager").ok()?;
    proxy.call("GetManagedObjects", &()).ok()
}

fn flag(properties: &HashMap<String, OwnedValue>, name: &str) -> bool {
    properties
        .get(name)
        .and_then(|value| value.downcast_ref::<bool>().ok())
        .unwrap_or(false)
}
