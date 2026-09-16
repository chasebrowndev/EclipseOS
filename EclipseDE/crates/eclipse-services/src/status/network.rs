// SPDX-License-Identifier: AGPL-3.0-only
//! NetworkManager, read-only.

use std::sync::mpsc::Sender;

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::OwnedObjectPath;

use super::{watch_service, Update};

const NM: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";

/// What the bar draws. There is no "connecting" state on purpose: a bar that
/// flickers through three shapes on every roam is worse than one that shows
/// where you actually are.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Network {
    #[default]
    Offline,
    Wired {
        id: String,
    },
    Wifi {
        id: String,
        /// Percent, as NetworkManager reports it.
        strength: u8,
    },
    /// A VPN, a modem, a bridge — named but not iconified.
    Other {
        id: String,
    },
}

pub(super) fn watch(connection: &Connection, updates: Sender<Update>) {
    watch_service("eclipse-net", connection, NM, read, Update::Network, updates);
}

fn read(connection: &Connection) -> Network {
    read_inner(connection).unwrap_or_default()
}

/// Any failure along the way reads as offline. A bar that cannot ask
/// NetworkManager has no business claiming the machine is connected.
fn read_inner(connection: &Connection) -> Option<Network> {
    let manager = proxy(connection, NM_PATH, NM)?;
    let active: OwnedObjectPath = manager.get_property("PrimaryConnection").ok()?;
    if active.as_str() == "/" {
        return Some(Network::Offline);
    }

    let connection_proxy = proxy(
        connection,
        active.as_str(),
        "org.freedesktop.NetworkManager.Connection.Active",
    )?;
    let id: String = connection_proxy.get_property("Id").ok()?;
    let kind: String = connection_proxy.get_property("Type").ok()?;

    Some(match kind.as_str() {
        "802-3-ethernet" => Network::Wired { id },
        "802-11-wireless" => Network::Wifi {
            strength: wifi_strength(connection, &connection_proxy).unwrap_or(0),
            id,
        },
        _ => Network::Other { id },
    })
}

/// The strength lives two hops away: the active connection names its devices,
/// a wireless device names the access point it is on, and the access point
/// carries the number.
fn wifi_strength(connection: &Connection, active: &Proxy<'_>) -> Option<u8> {
    let devices: Vec<OwnedObjectPath> = active.get_property("Devices").ok()?;
    for device in devices {
        let Some(wireless) = proxy(
            connection,
            device.as_str(),
            "org.freedesktop.NetworkManager.Device.Wireless",
        ) else {
            continue;
        };
        let Ok(ap) = wireless.get_property::<OwnedObjectPath>("ActiveAccessPoint") else {
            continue;
        };
        if ap.as_str() == "/" {
            continue;
        }
        let strength = proxy(
            connection,
            ap.as_str(),
            "org.freedesktop.NetworkManager.AccessPoint",
        )
        .and_then(|ap| ap.get_property::<u8>("Strength").ok());
        if let Some(strength) = strength {
            return Some(strength.min(100));
        }
    }
    None
}

fn proxy<'a>(connection: &Connection, path: &'a str, interface: &'a str) -> Option<Proxy<'a>> {
    Proxy::new(connection, NM, path, interface).ok()
}
