// SPDX-License-Identifier: AGPL-3.0-only
//! The clock face: the time and date the greeter shows, in the operator's
//! preset (`ec_greeter::Clock`). The strings are `ec_glance::clock`'s, the
//! same ones the lock screen draws; the zone rules come from the C library's
//! `localtime_r`.

pub use ec_glance::clock::{Face, Format};
use ec_greeter::Clock;

/// What a preset asks for. A missing preset is the shipped default: 12-hour
/// time with the date.
pub fn format_of(clock: Option<Clock>) -> Format {
    ec_greeter::clock_format(clock)
}

/// The face for the Unix time `secs` in the local zone. Falls back to an
/// empty face if the C library refuses the time: the tile shows nothing rather
/// than a wrong time.
pub fn face(format: Format, secs: i64) -> Face {
    match local(secs) {
        Some(tm) => render(&tm, format),
        None => Face::default(),
    }
}

fn local(secs: i64) -> Option<libc::tm> {
    let t = secs as libc::time_t;
    // SAFETY: `tm` is plain old data, so the all-zero value is a valid one, and
    // `localtime_r` writes into it only on success, which the null check tests.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            None
        } else {
            Some(tm)
        }
    }
}

/// The face for a broken-down local time. Pure, so the tests need no clock.
pub fn render(tm: &libc::tm, format: Format) -> Face {
    ec_glance::clock::render(tm.tm_hour, tm.tm_min, tm.tm_wday, tm.tm_mon, tm.tm_mday, format)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tm(hour: i32, min: i32) -> libc::tm {
        // SAFETY: plain old data; the test writes the fields it reads.
        let mut t: libc::tm = unsafe { std::mem::zeroed() };
        t.tm_hour = hour;
        t.tm_min = min;
        t.tm_mday = 10;
        t.tm_mon = 10;
        t.tm_wday = 2;
        t
    }

    #[test]
    fn twelve_hour_time_matches_the_lock_screen() {
        let f = Format {
            twelve: true,
            date: false,
        };
        assert_eq!(render(&tm(23, 15), f).time, "11:15 PM");
        assert_eq!(render(&tm(0, 5), f).time, "12:05 AM");
        assert_eq!(render(&tm(12, 0), f).time, "12:00 PM");
    }

    #[test]
    fn twenty_four_hour_time_is_padded() {
        let f = Format {
            twelve: false,
            date: false,
        };
        assert_eq!(render(&tm(23, 15), f).time, "23:15");
        assert_eq!(render(&tm(7, 3), f).time, "07:03");
    }

    #[test]
    fn the_date_line_shows_only_for_date_presets() {
        assert_eq!(
            render(
                &tm(9, 0),
                Format {
                    twelve: true,
                    date: true
                }
            )
            .date,
            "Tuesday, November 10"
        );
        assert_eq!(
            render(
                &tm(9, 0),
                Format {
                    twelve: true,
                    date: false
                }
            )
            .date,
            ""
        );
    }

    #[test]
    fn a_missing_preset_is_the_shipped_default() {
        assert_eq!(
            format_of(None),
            Format {
                twelve: true,
                date: true
            }
        );
    }

    #[test]
    fn out_of_range_fields_do_not_panic() {
        let mut t = tm(0, 0);
        t.tm_wday = 99;
        t.tm_mon = -4;
        t.tm_hour = 500;
        let f = render(
            &t,
            Format {
                twelve: true,
                date: true,
            },
        );
        assert_eq!(f.date, "Saturday, January 10");
    }
}
