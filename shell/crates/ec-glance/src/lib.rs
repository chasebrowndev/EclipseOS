// SPDX-License-Identifier: AGPL-3.0-only
//! Data for Glances that need no session (ADR 0126): batteries, wifi. sysfs and
//! `/proc` only, so the greeter can link it without a bus or a new privilege.
//! Anything a device reports is untrusted text: [`clean`] caps it and drops
//! control characters before it can reach a screen.
//!
//! Readers take their roots as arguments so tests run on a fake tree.
//! [`bounded`] runs one on a thread and gives up after a deadline, so a wedged
//! device can never hold the UI (or the password field) up.

pub mod clock;
pub mod tile;

use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

const NAME_MAX: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charge {
    Charging,
    Discharging,
    Full,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Battery {
    /// The machine's own battery, as opposed to a connected device's.
    pub internal: bool,
    pub name: String,
    pub percent: u8,
    pub charge: Charge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wifi {
    /// Link quality, 0-100.
    pub percent: u8,
}

/// Printable, one line, at most [`NAME_MAX`] characters.
pub fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(NAME_MAX)
        .collect::<String>()
        .trim()
        .to_string()
}

fn read(dir: &Path, file: &str) -> Option<String> {
    let mut s = fs::read_to_string(dir.join(file)).ok()?;
    s.truncate(s.trim_end().len());
    Some(s)
}

/// Every battery under `power_supply`: the machine's first, then connected
/// devices (scope `Device`), each by name. Entries it cannot read are skipped.
pub fn batteries(power_supply: &Path) -> Vec<Battery> {
    let Ok(entries) = fs::read_dir(power_supply) else {
        return Vec::new();
    };
    let mut out: Vec<Battery> = entries
        .flatten()
        .filter_map(|e| {
            let d = e.path();
            if read(&d, "type")? != "Battery" {
                return None;
            }
            let percent = read(&d, "capacity")?.parse::<u32>().ok()?.min(100) as u8;
            let charge = match read(&d, "status").as_deref() {
                Some("Charging") => Charge::Charging,
                Some("Discharging") => Charge::Discharging,
                Some("Full") => Charge::Full,
                _ => Charge::Unknown,
            };
            let internal = read(&d, "scope").as_deref() != Some("Device");
            let name = read(&d, "model_name")
                .map(|n| clean(&n))
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| clean(&e.file_name().to_string_lossy()));
            Some(Battery {
                internal,
                name,
                percent,
                charge,
            })
        })
        .collect();
    out.sort_by(|a, b| b.internal.cmp(&a.internal).then_with(|| a.name.cmp(&b.name)));
    out
}

/// The first wireless interface that is up, with its link quality from
/// `/proc/net/wireless` text (`proc`). `None` when there is none.
pub fn wifi(net: &Path, proc: &str) -> Option<Wifi> {
    let mut ifaces: Vec<_> = fs::read_dir(net).ok()?.flatten().collect();
    ifaces.sort_by_key(|e| e.file_name());
    let up = ifaces.into_iter().find(|e| {
        e.path().join("wireless").is_dir() && read(&e.path(), "operstate").as_deref() == Some("up")
    })?;
    let name = up.file_name().to_string_lossy().into_owned();
    let link = proc.lines().find_map(|l| {
        let (iface, rest) = l.split_once(':')?;
        if iface.trim() != name {
            return None;
        }
        rest.split_whitespace()
            .nth(1)?
            .trim_end_matches('.')
            .parse::<f32>()
            .ok()
    })?;
    // The driver's link quality is out of 70 on nearly everything.
    Some(Wifi {
        percent: (link / 70.0 * 100.0).clamp(0.0, 100.0) as u8,
    })
}

/// Run `f` on its own thread; `None` if it has not answered in `limit`.
/// The thread is left to finish on its own.
pub fn bounded<T: Send + 'static>(limit: Duration, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(limit).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tree(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ec-glance-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn supply(root: &Path, name: &str, files: &[(&str, &str)]) {
        let d = root.join(name);
        fs::create_dir_all(&d).unwrap();
        for (f, v) in files {
            fs::write(d.join(f), format!("{v}\n")).unwrap();
        }
    }

    #[test]
    fn the_machine_comes_first_then_devices_and_mains_is_skipped() {
        let r = tree("bat");
        supply(&r, "ACAD", &[("type", "Mains")]);
        supply(
            &r,
            "hid-1",
            &[
                ("type", "Battery"),
                ("scope", "Device"),
                ("capacity", "40"),
                ("model_name", "MX Master"),
            ],
        );
        supply(
            &r,
            "BAT1",
            &[("type", "Battery"), ("capacity", "72"), ("status", "Discharging")],
        );
        let b = batteries(&r);
        assert_eq!(b.len(), 2);
        assert!(b[0].internal && b[0].percent == 72 && b[0].charge == Charge::Discharging);
        assert_eq!(
            (b[1].name.as_str(), b[1].internal, b[1].percent),
            ("MX Master", false, 40)
        );
    }

    #[test]
    fn junk_from_a_device_is_cleaned_or_skipped() {
        let r = tree("junk");
        supply(
            &r,
            "a",
            &[
                ("type", "Battery"),
                ("capacity", "900"),
                ("model_name", "x\u{1b}[31my"),
            ],
        );
        supply(&r, "b", &[("type", "Battery"), ("capacity", "lots")]);
        let b = batteries(&r);
        assert_eq!(b.len(), 1);
        assert_eq!((b[0].percent, b[0].name.as_str()), (100, "x[31my"));
        assert_eq!(clean(&"é".repeat(99)).chars().count(), NAME_MAX);
        assert!(batteries(&r.join("missing")).is_empty());
    }

    #[test]
    fn wifi_reads_the_first_wireless_interface_that_is_up() {
        let r = tree("net");
        fs::create_dir_all(r.join("lo")).unwrap();
        fs::create_dir_all(r.join("wlan0/wireless")).unwrap();
        fs::write(r.join("wlan0/operstate"), "up\n").unwrap();
        let proc = "Inter-| sta-|   Quality\n face | tus | link level noise\n wlan0: 0000   35.  -60.  -256        0\n";
        assert_eq!(wifi(&r, proc), Some(Wifi { percent: 50 }));
        fs::write(r.join("wlan0/operstate"), "down\n").unwrap();
        assert_eq!(wifi(&r, proc), None);
    }

    #[test]
    fn a_stuck_read_is_given_up_on() {
        let slow = bounded(Duration::from_millis(20), || {
            std::thread::sleep(Duration::from_millis(500));
            1
        });
        assert_eq!(slow, None);
        assert_eq!(bounded(Duration::from_secs(1), || 7), Some(7));
    }
}
