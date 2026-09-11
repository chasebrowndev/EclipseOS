// SPDX-License-Identifier: AGPL-3.0-only
//! Local wall-clock text for the bar's last cell.
//!
//! `localtime_r` rather than a date crate: the zone rules live in the C
//! library and the bar needs exactly one string per second.

use std::time::{SystemTime, UNIX_EPOCH};

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `HH:MM` in the session's own zone.
pub fn time() -> String {
    match local() {
        Some(tm) => format!("{:02}:{:02}", tm.tm_hour, tm.tm_min),
        None => "--:--".to_owned(),
    }
}

/// `Thu 11 Sep`.
pub fn date() -> String {
    let Some(tm) = local() else {
        return String::new();
    };
    let day = DAYS.get(tm.tm_wday.clamp(0, 6) as usize).unwrap_or(&"");
    let month = MONTHS.get(tm.tm_mon.clamp(0, 11) as usize).unwrap_or(&"");
    format!("{day} {:02} {month}", tm.tm_mday)
}

fn local() -> Option<libc::tm> {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let t = libc::time_t::try_from(secs).ok()?;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `t` is a valid time_t and `tm` is a live, owned, zeroed struct;
    // `localtime_r` is the reentrant form and writes only through the pointer.
    let out = unsafe { libc::localtime_r(&t, &mut tm) };
    (!out.is_null()).then_some(tm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_time_is_always_five_characters() {
        let t = time();
        assert_eq!(t.len(), 5, "{t}");
        assert_eq!(&t[2..3], ":");
    }

    #[test]
    fn the_date_names_a_real_day_and_month() {
        let d = date();
        let parts: Vec<&str> = d.split(' ').collect();
        assert_eq!(parts.len(), 3, "{d}");
        assert!(DAYS.contains(&parts[0]));
        assert!(MONTHS.contains(&parts[2]));
    }
}
