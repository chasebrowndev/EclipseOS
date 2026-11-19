// SPDX-License-Identifier: AGPL-3.0-only
//! The Glances' feed: batteries and link quality, polled off the UI thread.
//!
//! The readers in `ec_glance` touch sysfs and procfs and may stall, so each
//! read runs under `ec_glance::bounded`, and the polling loop lives on its own
//! thread. The UI only ever receives a finished [`Snapshot`] through a
//! non-blocking send, so a stalled sysfs read leaves the last good numbers on
//! screen rather than freezing the greeter.

use std::path::Path;
use std::time::Duration;

use ec_glance::{batteries, bounded, wifi, Battery, Wifi};
use iced::futures::channel::mpsc::Sender;
use iced::futures::Stream;

pub const POWER_SUPPLY: &str = "/sys/class/power_supply";
pub const NET: &str = "/sys/class/net";
pub const WIRELESS: &str = "/proc/net/wireless";

/// How long one read may take before the poll gives up on it.
const LIMIT: Duration = Duration::from_millis(900);
/// Time between polls. Batteries and links change slowly.
const EVERY: Duration = Duration::from_secs(5);

/// One poll's answer. Batteries are sorted internal first.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub batteries: Vec<Battery>,
    pub wifi: Option<Wifi>,
}

/// Read both sources once. Blocking: call it under `bounded`.
pub fn read() -> Snapshot {
    let mut batteries = batteries(Path::new(POWER_SUPPLY));
    batteries.sort_by_key(|b| !b.internal);
    Snapshot {
        batteries,
        wifi: wifi(Path::new(NET), WIRELESS),
    }
}

/// The subscription's stream: a snapshot now, then one every [`EVERY`]. `None`
/// means the read timed out, and the UI keeps what it had. Ends when the
/// subscription is dropped.
pub fn poll() -> impl Stream<Item = Option<Snapshot>> {
    iced::stream::channel(1, |mut out: Sender<Option<Snapshot>>| async move {
        std::thread::spawn(move || loop {
            let snap = bounded(LIMIT, read);
            if let Err(e) = out.try_send(snap) {
                if e.is_disconnected() {
                    break;
                }
            }
            std::thread::sleep(EVERY);
        });
        std::future::pending::<()>().await
    })
}
