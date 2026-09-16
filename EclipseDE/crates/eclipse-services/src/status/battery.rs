// SPDX-License-Identifier: AGPL-3.0-only
//! UPower, read-only.

use std::sync::mpsc::Sender;
use std::time::Duration;

use zbus::blocking::{Connection, Proxy};

use super::{watch_service, Update};

const UPOWER: &str = "org.freedesktop.UPower";
/// UPower's own composite of every battery in the machine. Reading this rather
/// than enumerating devices means a laptop with two packs shows one number,
/// which is the number the human cares about.
const DISPLAY_DEVICE: &str = "/org/freedesktop/UPower/devices/DisplayDevice";
const DEVICE: &str = "org.freedesktop.UPower.Device";

/// `Type` 2 is a battery. A desktop's display device exists but reports
/// `Unknown`, which is how we tell there is nothing to draw.
const TYPE_BATTERY: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charge {
    Charging,
    Discharging,
    Full,
    Unknown,
}

impl Charge {
    fn from_state(state: u32) -> Self {
        match state {
            1 => Self::Charging,
            2 => Self::Discharging,
            4 => Self::Full,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Battery {
    pub percent: u8,
    pub state: Charge,
    /// Time to empty while discharging, to full while charging. `None` when
    /// UPower has not worked it out yet — which it often has not for the first
    /// minute after a plug change, and a bar that shows "0:00" then is lying.
    pub remaining: Option<Duration>,
}

pub(super) fn watch(connection: &Connection, updates: Sender<Update>) {
    watch_service(
        "eclipse-battery",
        connection,
        UPOWER,
        read,
        Update::Battery,
        updates,
    );
}

fn read(connection: &Connection) -> Option<Battery> {
    let device = Proxy::new(connection, UPOWER, DISPLAY_DEVICE, DEVICE).ok()?;
    if device.get_property::<u32>("Type").ok()? != TYPE_BATTERY {
        return None;
    }
    if !device.get_property::<bool>("IsPresent").ok()? {
        return None;
    }

    let state = Charge::from_state(device.get_property::<u32>("State").ok()?);
    let seconds = match state {
        Charge::Charging => device.get_property::<i64>("TimeToFull").ok()?,
        _ => device.get_property::<i64>("TimeToEmpty").ok()?,
    };

    Some(Battery {
        percent: device
            .get_property::<f64>("Percentage")
            .ok()?
            .clamp(0.0, 100.0)
            .round() as u8,
        state,
        remaining: (seconds > 0).then(|| Duration::from_secs(seconds as u64)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UPower's numbers, not ours. Anything we do not have an icon for is
    /// `Unknown` rather than a guess — "pending charge" drawn as charging is a
    /// lie the human acts on.
    #[test]
    fn upower_states_map_to_what_the_bar_can_draw() {
        assert_eq!(Charge::from_state(1), Charge::Charging);
        assert_eq!(Charge::from_state(2), Charge::Discharging);
        assert_eq!(Charge::from_state(4), Charge::Full);
        for unknown in [0, 3, 5, 6, 99] {
            assert_eq!(Charge::from_state(unknown), Charge::Unknown);
        }
    }
}
