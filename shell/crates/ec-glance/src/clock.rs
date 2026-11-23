// SPDX-License-Identifier: AGPL-3.0-only
//! What the clock tile says: the time and the date line, for a preset's two
//! facts (12-hour or not, with a date line or not). Pure: the caller reads the
//! local time (`localtime_r`) and passes its fields in. The greeter and the
//! lock screen both build their strings here, so the two cannot drift; how the
//! strings are sized is `tile::clock`'s.

const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// What a preset asks for: the hour format, and whether a date line shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    pub twelve: bool,
    pub date: bool,
}

impl Default for Format {
    /// The shipped look: 12-hour time with the date.
    fn default() -> Self {
        Format {
            twelve: true,
            date: true,
        }
    }
}

/// The text on the clock tile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Face {
    pub time: String,
    pub date: String,
}

/// The face for broken-down local time fields (`tm_hour`, `tm_min`, `tm_wday`,
/// `tm_mon`, `tm_mday`). Out-of-range fields are clamped, never a panic.
pub fn render(hour: i32, min: i32, wday: i32, mon: i32, mday: i32, format: Format) -> Face {
    let hour = hour.clamp(0, 23) as u32;
    let minute = min.clamp(0, 59) as u32;
    let time = if format.twelve {
        let h12 = match hour % 12 {
            0 => 12,
            h => h,
        };
        let suffix = if hour < 12 { "AM" } else { "PM" };
        format!("{h12}:{minute:02} {suffix}")
    } else {
        format!("{hour:02}:{minute:02}")
    };
    let date = if format.date {
        let weekday = WEEKDAYS.get(wday.clamp(0, 6) as usize).copied().unwrap_or("");
        let month = MONTHS.get(mon.clamp(0, 11) as usize).copied().unwrap_or("");
        format!("{weekday}, {month} {}", mday.clamp(1, 31))
    } else {
        String::new()
    };
    Face { time, date }
}

#[cfg(test)]
mod tests {
    use super::*;

    const F: [Format; 4] = [
        Format {
            twelve: false,
            date: false,
        },
        Format {
            twelve: true,
            date: false,
        },
        Format {
            twelve: false,
            date: true,
        },
        Format {
            twelve: true,
            date: true,
        },
    ];

    #[test]
    fn each_preset_makes_its_strings() {
        let f: Vec<Face> = F.iter().map(|&f| render(23, 15, 2, 10, 10, f)).collect();
        assert_eq!((f[0].time.as_str(), f[0].date.as_str()), ("23:15", ""));
        assert_eq!((f[1].time.as_str(), f[1].date.as_str()), ("11:15 PM", ""));
        assert_eq!(f[2].date, "Tuesday, November 10");
        assert_eq!(f[2].time, "23:15");
        assert_eq!(f[3].time, "11:15 PM");
        assert_eq!(f[3].date, "Tuesday, November 10");
    }

    #[test]
    fn the_default_is_twelve_hour_with_the_date() {
        assert_eq!(Format::default(), F[3]);
    }

    #[test]
    fn edges_and_garbage_do_not_panic() {
        assert_eq!(render(0, 5, 0, 0, 1, F[1]).time, "12:05 AM");
        assert_eq!(render(12, 0, 0, 0, 1, F[1]).time, "12:00 PM");
        let g = render(500, -3, 99, -4, 0, F[3]);
        assert_eq!(g.date, "Saturday, January 1");
    }
}
