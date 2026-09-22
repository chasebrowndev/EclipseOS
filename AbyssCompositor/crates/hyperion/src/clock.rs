// SPDX-License-Identifier: AGPL-3.0-only
//! Local wall-clock text for the bar's last cell.
//!
//! `localtime_r` rather than a date crate: the zone rules live in the C
//! library and the bar needs exactly one string per second.

use std::time::{SystemTime, UNIX_EPOCH};

use eclipse_ui::tokens::clock;

/// `11:15 PM` by default, `23:15` when the 12-hour default is turned off.
///
/// Which of the two is a matter of taste, so it is not decided here: the
/// switch is `eclipse_ui::tokens::clock`, alongside the colours and the
/// radii, and `docs/PROPOSEDFEATURES.md` has the whole token set coming from
/// config with the compiled values as defaults. The bar reads the token; it
/// never hardcodes a preference.
pub fn time() -> String {
    let Some(tm) = local() else {
        return "--:--".to_owned();
    };
    if !clock::HOUR_12 {
        return format!("{:02}:{:02}", tm.tm_hour, tm.tm_min);
    }
    let (hour, meridiem) = twelve(tm.tm_hour);
    format!("{hour}:{:02} {meridiem}", tm.tm_min)
}

/// `9/12/26` by default — the date as a reading, not as a sentence. A bar
/// cell is 74px wide and "Friday, September 12" is not a thing that fits in
/// it; the spelled-out form is what a calendar drawer is for.
pub fn date() -> String {
    let Some(tm) = local() else {
        return String::new();
    };
    if !clock::DATE_MDY {
        let day = DAYS.get(tm.tm_wday.clamp(0, 6) as usize).unwrap_or(&"");
        let month = MONTHS.get(tm.tm_mon.clamp(0, 11) as usize).unwrap_or(&"");
        return format!("{day} {:02} {month}", tm.tm_mday);
    }
    // `tm_year` is years since 1900; the two-digit form is the last two of
    // the calendar year, and it stays two digits past 2100.
    let year = (tm.tm_year + 1900).rem_euclid(100);
    format!("{}/{}/{:02}", tm.tm_mon + 1, tm.tm_mday, year)
}

/// Clock hour and meridiem from a 24-hour hour. Midnight and noon are the two
/// cases a naive `% 12` gets wrong, and they are both `12`.
fn twelve(hour24: i32) -> (i32, &'static str) {
    let meridiem = if hour24 < 12 { "AM" } else { "PM" };
    let hour = match hour24.rem_euclid(12) {
        0 => 12,
        h => h,
    };
    (hour, meridiem)
}

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

// POSIX, but not bound by the `libc` crate.
unsafe extern "C" {
    fn tzset();
}

fn local() -> Option<libc::tm> {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let t = libc::time_t::try_from(secs).ok()?;
    // `localtime_r` does not re-read the zone; glibc caches it from the first
    // call. `tzset` re-stats /etc/localtime (TZ unset), so a zone change made
    // while the bar runs shows on the next tick instead of after a restart.
    // SAFETY: no arguments; it only refreshes libc's own zone state.
    unsafe { tzset() };
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
    fn midnight_and_noon_are_twelve_and_not_zero() {
        assert_eq!(twelve(0), (12, "AM"));
        assert_eq!(twelve(12), (12, "PM"));
        assert_eq!(twelve(11), (11, "AM"));
        assert_eq!(twelve(23), (11, "PM"));
        assert_eq!(twelve(13), (1, "PM"));
    }

    #[test]
    fn the_time_reads_as_a_clock() {
        let t = time();
        let (hhmm, suffix) = match t.split_once(' ') {
            Some((hhmm, m)) => (hhmm, Some(m)),
            None => (t.as_str(), None),
        };
        let (h, m) = hhmm.split_once(':').expect("{t}");
        let h: i32 = h.parse().expect("{t}");
        let m: i32 = m.parse().expect("{t}");
        assert!((0..60).contains(&m), "{t}");
        if clock::HOUR_12 {
            assert!((1..=12).contains(&h), "{t}");
            assert!(matches!(suffix, Some("AM" | "PM")), "{t}");
        } else {
            assert!((0..24).contains(&h), "{t}");
        }
    }

    /// `9/12/26`, not `Friday, September 12` — three numbers and two slashes.
    #[test]
    fn the_date_is_three_numbers() {
        let d = date();
        if !clock::DATE_MDY {
            return;
        }
        let parts: Vec<&str> = d.split('/').collect();
        assert_eq!(parts.len(), 3, "{d}");
        let n: Vec<i32> = parts.iter().map(|p| p.parse().expect("{d}")).collect();
        assert!((1..=12).contains(&n[0]), "{d}");
        assert!((1..=31).contains(&n[1]), "{d}");
        assert!((0..100).contains(&n[2]), "{d}");
    }
}
