// SPDX-License-Identifier: AGPL-3.0-only
//! The sample data the builder's preview draws its Glances and clock with.
//! Pure. Fixed numbers (the real screens read sysfs and the clock), but of the
//! same *shapes* the real ones take, so the owner can see how a tile reads and
//! resizes when the data changes: no battery, one, several; no link, strong,
//! weak; each clock preset. Sizing is never decided here: that is
//! `ec_glance::tile`, the module the greeter and the lock ask too.

use ec_glance::{Battery, Charge};
use ec_greeter::Clock;

/// How many batteries the preview pretends the machine has. Several is the
/// default: the content size the layout editor measures a Battery tile for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Batteries {
    None,
    One,
    #[default]
    Several,
}

impl Batteries {
    pub const ALL: [Batteries; 3] = [Batteries::None, Batteries::One, Batteries::Several];

    pub fn label(self) -> &'static str {
        match self {
            Batteries::None => "No battery",
            Batteries::One => "One battery",
            Batteries::Several => "Several",
        }
    }

    /// Internal first, as `ec_glance::batteries` returns them.
    pub fn data(self) -> Vec<Battery> {
        let device = |name: &str, percent, charge| Battery {
            internal: false,
            name: name.to_owned(),
            percent,
            charge,
        };
        let internal = Battery {
            internal: true,
            name: "BAT0".to_owned(),
            percent: 72,
            charge: Charge::Discharging,
        };
        match self {
            Batteries::None => Vec::new(),
            Batteries::One => vec![internal],
            Batteries::Several => vec![
                internal,
                device("Logitech MX Master 3S", 40, Charge::Discharging),
                device("Keychron K3", 100, Charge::Charging),
            ],
        }
    }
}

/// How good the preview's wireless link is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Link {
    None,
    #[default]
    Strong,
    Weak,
}

impl Link {
    pub const ALL: [Link; 3] = [Link::None, Link::Strong, Link::Weak];

    pub fn label(self) -> &'static str {
        match self {
            Link::None => "No link",
            Link::Strong => "Strong",
            Link::Weak => "Weak",
        }
    }

    /// Link quality, percent; `None` is no link.
    pub fn quality(self) -> Option<u8> {
        match self {
            Link::None => None,
            Link::Strong => Some(92),
            Link::Weak => Some(24),
        }
    }
}

/// What the clock tile shows for a preset: the strings, and the two facts the
/// shared sizing rule needs (12-hour, a date line). A missing preset is the
/// shipped default, 12-hour with the date, as the greeter's `Format::of`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockSample {
    pub time: &'static str,
    pub date: &'static str,
    pub twelve: bool,
    pub has_date: bool,
}

const DATE: &str = "Tuesday, November 10";

pub fn clock(c: Option<Clock>) -> ClockSample {
    let f = ec_greeter::clock_format(c);
    let (twelve, has_date) = (f.twelve, f.date);
    ClockSample {
        time: if twelve { "9:41 AM" } else { "09:41" },
        date: if has_date { DATE } else { "" },
        twelve,
        has_date,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shapes_are_what_they_say() {
        assert!(Batteries::None.data().is_empty());
        assert_eq!(Batteries::One.data().len(), 1);
        assert!(Batteries::Several.data().len() > 1);
        assert!(Batteries::Several.data()[0].internal);
        assert_eq!(Link::None.quality(), None);
        assert!(Link::Strong.quality() > Link::Weak.quality());
    }

    #[test]
    fn the_clock_samples_follow_the_greeters_presets() {
        let shipped = clock(None);
        assert!(shipped.twelve && shipped.has_date);
        assert_eq!(clock(Some(Clock::Time24)).time, "09:41");
        assert_eq!(clock(Some(Clock::Time12)).time, "9:41 AM");
        assert_eq!(clock(Some(Clock::Time12)).date, "");
        assert_eq!(clock(Some(Clock::Date24)).date, DATE);
    }
}
