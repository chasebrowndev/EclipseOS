// SPDX-License-Identifier: AGPL-3.0-only
//! Wall-clock text for the lock screen, from `localtime_r` (the zone rules
//! live in the C library, as in the taskbar's `clock.rs`).

use std::time::{SystemTime, UNIX_EPOCH};

/// The time and date as the view draws them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reading {
    /// `11:15 PM` or `23:15`.
    pub time: String,
    /// `Tuesday, November 10`.
    pub date: String,
}

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

/// Now, local time. `hour_12` picks the clock face.
pub fn now(hour_12: bool) -> Reading {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as libc::time_t);
    // SAFETY: `tm` is plain C data (integers and, on glibc, a `tm_zone`
    // pointer), for which all-zero is a valid value.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `secs` is a valid time_t and `tm` a live, owned struct;
    // `localtime_r` is the reentrant form and writes only through the pointer.
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        return Reading {
            time: "--:--".to_owned(),
            date: String::new(),
        };
    }
    format(&tm, hour_12)
}

fn format(tm: &libc::tm, hour_12: bool) -> Reading {
    let time = if hour_12 {
        let (h, m) = match tm.tm_hour {
            0 => (12, "AM"),
            h @ 1..=11 => (h, "AM"),
            12 => (12, "PM"),
            h => (h - 12, "PM"),
        };
        format!("{h}:{:02} {m}", tm.tm_min)
    } else {
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    };
    let day = WEEKDAYS.get(tm.tm_wday as usize).copied().unwrap_or("");
    let month = MONTHS.get(tm.tm_mon as usize).copied().unwrap_or("");
    Reading {
        time,
        date: format!("{day}, {month} {}", tm.tm_mday),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tm(hour: i32, min: i32, wday: i32, mon: i32, mday: i32) -> libc::tm {
        // SAFETY: as in `now`: all-zero is a valid `tm`.
        let mut t: libc::tm = unsafe { std::mem::zeroed() };
        t.tm_hour = hour;
        t.tm_min = min;
        t.tm_wday = wday;
        t.tm_mon = mon;
        t.tm_mday = mday;
        t
    }

    #[test]
    fn twelve_and_twenty_four_hour() {
        assert_eq!(format(&tm(0, 5, 2, 10, 10), true).time, "12:05 AM");
        assert_eq!(format(&tm(12, 0, 2, 10, 10), true).time, "12:00 PM");
        assert_eq!(format(&tm(23, 15, 2, 10, 10), true).time, "11:15 PM");
        assert_eq!(format(&tm(23, 15, 2, 10, 10), false).time, "23:15");
        assert_eq!(format(&tm(7, 3, 2, 10, 10), false).time, "07:03");
    }

    #[test]
    fn spelled_out_date() {
        assert_eq!(format(&tm(9, 0, 2, 10, 10), true).date, "Tuesday, November 10");
    }
}
