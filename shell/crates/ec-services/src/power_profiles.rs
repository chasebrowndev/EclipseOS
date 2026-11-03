// SPDX-License-Identifier: AGPL-3.0-only
//! power-profiles-daemon (`net.hadess.PowerProfiles`), read and set.
//!
//! The same shape as `status/`: one watcher thread on the system bus reads the
//! daemon and re-reads on its `PropertiesChanged`, reporting over a channel
//! (iced runs no async runtime). Setting a profile goes through
//! [`PowerActions`], which runs on its own short-lived thread and reports back
//! on the same [`PowerEvents`] receiver.
//!
//! This channel is separate from `status::Update` on purpose: that enum is
//! matched exhaustively by the bar, and a power reading should not force a
//! consumer that does not draw it to change.
//!
//! A machine without the daemon reports [`PowerState::Unavailable`] and the
//! watcher keeps its normal cadence (the floor re-reads, so a daemon started
//! later is picked up); nothing error-loops.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};

use zbus::blocking::Connection;
use zbus::zvariant::OwnedValue;

use crate::status::{signal_rule, uncached, watch_service, CADENCE};

const SERVICE: &str = "net.hadess.PowerProfiles";
const PATH: &str = "/net/hadess/PowerProfiles";
const INTERFACE: &str = "net.hadess.PowerProfiles";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// A profile the daemon knows by name. Names the daemon offers beyond these
/// three are ignored: the bar has no icon or label for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    PowerSaver,
    Balanced,
    Performance,
}

impl Profile {
    /// Every profile this module can represent, in the daemon's own order.
    pub const ALL: [Profile; 3] = [Profile::PowerSaver, Profile::Balanced, Profile::Performance];

    /// The name the daemon uses on the bus.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PowerSaver => "power-saver",
            Self::Balanced => "balanced",
            Self::Performance => "performance",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|profile| profile.as_str() == name)
    }
}

/// Why the daemon says performance is limited right now. Informational: the
/// daemon still applies the profile the human picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Degraded {
    LapDetected,
    HighOperatingTemperature,
    /// A reason this module does not name. Shown as the daemon's own text.
    Other(String),
}

impl Degraded {
    /// `""` is the daemon's "not degraded", and maps to `None`.
    fn parse(reason: &str) -> Option<Self> {
        match reason {
            "" => None,
            "lap-detected" => Some(Self::LapDetected),
            "high-operating-temperature" => Some(Self::HighOperatingTemperature),
            other => Some(Self::Other(other.to_owned())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub active: Profile,
    /// The profiles this machine offers, in [`Profile::ALL`] order. Never
    /// empty, and always contains `active`.
    pub profiles: Vec<Profile>,
    pub degraded: Option<Degraded>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PowerState {
    /// No daemon, a daemon that does not answer, or an answer this module
    /// cannot represent. The control draws nothing rather than a guess.
    Unavailable,
    Ready(Snapshot),
}

/// What the power watcher and the setter report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The reading changed (or was re-sent after a set).
    State(PowerState),
    /// A set did not happen. The daemon's own reason, verbatim.
    Failed { action: &'static str, reason: String },
}

/// The receiving end of the watcher and the setter.
pub struct PowerEvents {
    events: Receiver<Event>,
}

impl PowerEvents {
    /// The next event, if one is waiting. Never blocks.
    pub fn try_recv(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }
}

/// The action handle. Clone freely; every clone drives the same connection and
/// reports on the same [`PowerEvents`].
#[derive(Clone)]
pub struct PowerActions {
    connection: Connection,
    events: Sender<Event>,
}

impl PowerActions {
    /// Set the active profile. Returns at once. On success or failure a fresh
    /// [`Event::State`] follows, so the view is never left showing a guess; a
    /// failure also sends [`Event::Failed`].
    pub fn set_profile(&self, profile: Profile) {
        let connection = self.connection.clone();
        let events = self.events.clone();
        let spawned = std::thread::Builder::new()
            .name("eclipse-power-set".to_owned())
            .spawn(move || {
                if let Err(reason) = set_active(&connection, profile) {
                    let _ = events.send(Event::Failed {
                        action: "set_profile",
                        reason,
                    });
                }
                let _ = events.send(Event::State(read(&connection)));
            });
        if let Err(error) = spawned {
            let _ = self.events.send(Event::Failed {
                action: "set_profile",
                reason: error.to_string(),
            });
        }
    }
}

/// Start watching the daemon and connect the action path. Fails only if the
/// system bus itself is unreachable; a missing daemon is a reading, not an
/// error.
pub fn spawn() -> zbus::Result<(PowerActions, PowerEvents)> {
    // Fail early and honestly if there is no system bus at all, as status does.
    let connection = Connection::system()?;
    let (tx, events) = mpsc::channel();

    // A rule we cannot build is a constant error; the floor still keeps the
    // reading fresh, so degrade rather than panic (as `watch_system` does).
    let rule = signal_rule(SERVICE, Some(PATH), Some(PROPERTIES)).ok();
    watch_service(
        "eclipse-power-profiles",
        Connection::system,
        rule,
        CADENCE,
        read,
        Event::State,
        tx.clone(),
    );

    Ok((
        PowerActions {
            connection,
            events: tx,
        },
        PowerEvents { events },
    ))
}

fn read(connection: &Connection) -> PowerState {
    let Some(proxy) = uncached(connection, SERVICE, PATH, INTERFACE) else {
        return PowerState::Unavailable;
    };
    let Ok(active) = proxy.get_property::<String>("ActiveProfile") else {
        return PowerState::Unavailable;
    };
    let Ok(entries) = proxy.get_property::<Vec<HashMap<String, OwnedValue>>>("Profiles") else {
        return PowerState::Unavailable;
    };
    // Optional on older daemons: absent means "not degraded".
    let degraded = proxy
        .get_property::<String>("PerformanceDegraded")
        .unwrap_or_default();

    let offered: Vec<String> = entries.iter().filter_map(profile_name).collect();
    snapshot(&active, &offered, &degraded)
}

fn set_active(connection: &Connection, profile: Profile) -> Result<(), String> {
    let proxy = uncached(connection, SERVICE, PATH, INTERFACE)
        .ok_or_else(|| "power profiles service is not running".to_owned())?;
    proxy
        .set_property("ActiveProfile", profile.as_str())
        .map_err(|error| match error {
            zbus::fdo::Error::Failed(detail) => detail,
            other => other.to_string(),
        })
}

/// The `Profile` key of one entry of the daemon's `Profiles` array.
fn profile_name(entry: &HashMap<String, OwnedValue>) -> Option<String> {
    entry
        .get("Profile")
        .and_then(|value| value.downcast_ref::<&str>().ok())
        .map(str::to_owned)
}

/// The pure part of a reading: the daemon's strings in, a [`PowerState`] out.
/// Fail-closed: an active profile this module cannot name, or one the daemon
/// does not list as offered, is `Unavailable`.
fn snapshot(active: &str, offered: &[String], degraded: &str) -> PowerState {
    let Some(active) = Profile::parse(active) else {
        return PowerState::Unavailable;
    };
    let profiles: Vec<Profile> = Profile::ALL
        .into_iter()
        .filter(|profile| offered.iter().any(|name| name == profile.as_str()))
        .collect();
    if profiles.is_empty() || !profiles.contains(&active) {
        return PowerState::Unavailable;
    }
    PowerState::Ready(Snapshot {
        active,
        profiles,
        degraded: Degraded::parse(degraded),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    fn entry(name: &str) -> HashMap<String, OwnedValue> {
        let mut map = HashMap::new();
        map.insert(
            "Profile".to_owned(),
            OwnedValue::try_from(zbus::zvariant::Value::from(name)).unwrap(),
        );
        map.insert(
            "Driver".to_owned(),
            OwnedValue::try_from(zbus::zvariant::Value::from("placeholder")).unwrap(),
        );
        map
    }

    #[test]
    fn profile_names_round_trip_and_unknown_names_are_refused() {
        for profile in Profile::ALL {
            assert_eq!(Profile::parse(profile.as_str()), Some(profile));
        }
        for unknown in ["", "Balanced", "quiet", "power_saver", "performance "] {
            assert_eq!(Profile::parse(unknown), None, "{unknown:?}");
        }
    }

    #[test]
    fn the_profile_name_comes_from_the_profile_key_only() {
        assert_eq!(profile_name(&entry("balanced")), Some("balanced".to_owned()));
        let mut no_key = entry("balanced");
        no_key.remove("Profile");
        assert_eq!(profile_name(&no_key), None);
        let mut not_a_string = HashMap::new();
        not_a_string.insert(
            "Profile".to_owned(),
            OwnedValue::try_from(zbus::zvariant::Value::from(7u32)).unwrap(),
        );
        assert_eq!(profile_name(&not_a_string), None);
    }

    #[test]
    fn offered_profiles_come_back_in_canonical_order_and_unknown_ones_are_dropped() {
        let state = snapshot(
            "balanced",
            &names(&["performance", "quiet", "balanced", "power-saver"]),
            "",
        );
        assert_eq!(
            state,
            PowerState::Ready(Snapshot {
                active: Profile::Balanced,
                profiles: vec![Profile::PowerSaver, Profile::Balanced, Profile::Performance],
                degraded: None,
            })
        );
    }

    #[test]
    fn a_machine_without_performance_offers_two() {
        let state = snapshot("power-saver", &names(&["power-saver", "balanced"]), "");
        let PowerState::Ready(snap) = state else {
            panic!("expected a reading");
        };
        assert_eq!(snap.profiles, vec![Profile::PowerSaver, Profile::Balanced]);
    }

    #[test]
    fn an_active_profile_the_daemon_does_not_offer_is_unavailable() {
        assert_eq!(
            snapshot("performance", &names(&["power-saver", "balanced"]), ""),
            PowerState::Unavailable
        );
    }

    #[test]
    fn an_unknown_active_profile_or_an_empty_list_is_unavailable() {
        assert_eq!(
            snapshot("quiet", &names(&["quiet", "balanced"]), ""),
            PowerState::Unavailable
        );
        assert_eq!(snapshot("balanced", &[], ""), PowerState::Unavailable);
        assert_eq!(
            snapshot("balanced", &names(&["quiet"]), ""),
            PowerState::Unavailable
        );
    }

    #[test]
    fn degraded_reasons_map_to_what_the_view_can_say() {
        assert_eq!(Degraded::parse(""), None);
        assert_eq!(Degraded::parse("lap-detected"), Some(Degraded::LapDetected));
        assert_eq!(
            Degraded::parse("high-operating-temperature"),
            Some(Degraded::HighOperatingTemperature)
        );
        assert_eq!(
            Degraded::parse("something-new"),
            Some(Degraded::Other("something-new".to_owned()))
        );
    }

    #[test]
    fn a_degraded_reason_rides_along_with_the_reading() {
        let state = snapshot(
            "performance",
            &names(&["balanced", "performance"]),
            "lap-detected",
        );
        let PowerState::Ready(snap) = state else {
            panic!("expected a reading");
        };
        assert_eq!(snap.degraded, Some(Degraded::LapDetected));
        assert_eq!(snap.active, Profile::Performance);
    }
}
