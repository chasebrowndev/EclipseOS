// SPDX-License-Identifier: AGPL-3.0-only
//! Wall-clock text for the lock screen, from `localtime_r` (the zone rules
//! live in the C library, as in the taskbar's `clock.rs`).

use std::time::{SystemTime, UNIX_EPOCH};

use ec_glance::clock::Format;

/// The time and date as the view draws them, and the preset they came from
/// (the view sizes the tile against it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reading {
    /// `11:15 PM` or `23:15`.
    pub time: String,
    /// `Tuesday, November 10`; empty when the preset has no date line.
    pub date: String,
    pub format: Format,
}

/// Now, local time, in the preset `format`.
pub fn now(format: Format) -> Reading {
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
            format,
        };
    }
    reading(&tm, format)
}

fn reading(tm: &libc::tm, format: Format) -> Reading {
    let f = ec_glance::clock::render(tm.tm_hour, tm.tm_min, tm.tm_wday, tm.tm_mon, tm.tm_mday, format);
    Reading {
        time: f.time,
        date: f.date,
        format,
    }
}

/// The preset the login screen is set to, which the lock follows so the two
/// agree: `clock` in the greeter's settings file (world-readable, root-written,
/// never the user's to change). Anything unreadable, invalid or absent is the
/// shipped default, 12-hour with the date. Never errors.
pub fn load_format() -> Format {
    let path = std::path::Path::new(ec_greeter::STATE_DIR).join(ec_greeter::SETTINGS_FILE);
    format_from_text(crate::face::read_capped(&path, ec_greeter::MAX_SETTINGS).as_deref())
}

/// The preset in settings-file text, or the default.
pub fn format_from_text(text: Option<&str>) -> Format {
    let clock = text
        .and_then(|t| ec_greeter::Settings::parse(t).ok())
        .and_then(|s| s.clock);
    ec_greeter::clock_format(clock)
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

    const F12: Format = Format {
        twelve: true,
        date: true,
    };
    const F24: Format = Format {
        twelve: false,
        date: false,
    };

    #[test]
    fn twelve_and_twenty_four_hour() {
        assert_eq!(reading(&tm(0, 5, 2, 10, 10), F12).time, "12:05 AM");
        assert_eq!(reading(&tm(12, 0, 2, 10, 10), F12).time, "12:00 PM");
        assert_eq!(reading(&tm(23, 15, 2, 10, 10), F12).time, "11:15 PM");
        assert_eq!(reading(&tm(23, 15, 2, 10, 10), F24).time, "23:15");
        assert_eq!(reading(&tm(7, 3, 2, 10, 10), F24).time, "07:03");
    }

    #[test]
    fn the_date_follows_the_preset() {
        assert_eq!(reading(&tm(9, 0, 2, 10, 10), F12).date, "Tuesday, November 10");
        assert_eq!(reading(&tm(9, 0, 2, 10, 10), F24).date, "");
    }

    #[test]
    fn the_preset_comes_from_the_settings_text() {
        let f = |t: Option<&str>| format_from_text(t);
        let tw = |twelve, date| Format { twelve, date };
        assert_eq!(f(Some("clock = \"24h\"\n")), tw(false, false));
        assert_eq!(f(Some("clock = \"12h\"\n")), tw(true, false));
        assert_eq!(f(Some("clock = \"24h-date\"\n")), tw(false, true));
        assert_eq!(f(Some("clock = \"12h-date\"\n")), tw(true, true));
        // Absent key, absent file, garbage: the shipped default.
        assert_eq!(f(Some("wallpaper = false\n")), tw(true, true));
        assert_eq!(f(None), tw(true, true));
        assert_eq!(f(Some("clock = \"%H:%M\"\n")), tw(true, true));
        assert_eq!(f(Some("\u{0}\u{1} nonsense")), tw(true, true));
    }

    #[test]
    fn every_preset_fits_its_tile() {
        use ec_glance::tile::{self, Inner};
        for (w, h) in [(120.0, 80.0), (300.0, 160.0), (560.0, 300.0), (900.0, 600.0)] {
            for (twelve, date) in [(false, false), (true, false), (false, true), (true, true)] {
                let p = tile::clock(Inner { w, h }, twelve, date);
                assert!(p.used_h() <= h, "{w}x{h} {twelve} {date}");
                assert!(p.numeral > 0.0);
                assert!(p.date.is_none() || date);
            }
        }
    }

    #[test]
    fn a_missing_settings_file_is_not_an_error() {
        let _ = load_format();
        let _ = now(F24);
    }
}
