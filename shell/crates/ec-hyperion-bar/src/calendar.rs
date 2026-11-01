// SPDX-License-Identifier: AGPL-3.0-only
//! The calendar drawer's model: month arithmetic and the day/event state
//! (ADR 0079).
//!
//! Dates are plain `(year, month, day)` triples run through proleptic
//! Gregorian arithmetic; only "what is local midnight" and "what is today"
//! ask libc (`mktime`, `localtime_r`), the same way `clock.rs` does, so the
//! zone rules stay in the C library and no date crate is needed.
//!
//! Event data is `private` (ADR 0079): nothing here logs a title, a location
//! or how many events a day holds.

use std::time::{SystemTime, UNIX_EPOCH};

use ec_services::calendar::Event;

/// `bar.clock.week-start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeekStart {
    /// The locale's own first weekday (glibc `_NL_TIME_FIRST_WEEKDAY`).
    Auto,
    Monday,
    Sunday,
}

impl WeekStart {
    /// The first column's weekday, `0` = Sunday.
    pub fn first(self) -> u32 {
        match self {
            WeekStart::Sunday => 0,
            WeekStart::Monday => 1,
            WeekStart::Auto => locale_first_weekday(),
        }
    }
}

// glibc's <langinfo.h> item, which the libc crate does not bind. The value is
// its enum position (`_NL_ITEM(LC_TIME, 104)`), fixed by the glibc ABI.
const NL_TIME_FIRST_WEEKDAY: libc::nl_item = 0x20000 + 104;

/// The locale's first weekday, `0` = Sunday. glibc's item is the weekday of
/// the reference week's first day counted from Sunday = 1, so `1` is Sunday
/// and `2` Monday. Monday when the locale says nothing sensible.
fn locale_first_weekday() -> u32 {
    use std::sync::OnceLock;
    static FIRST: OnceLock<u32> = OnceLock::new();
    *FIRST.get_or_init(|| {
        // SAFETY: setlocale/nl_langinfo are called once, here, with a valid
        // empty C string; LC_TIME only affects time formatting, which the bar
        // never uses. The returned pointer is read at once.
        let value = unsafe {
            libc::setlocale(libc::LC_TIME, c"".as_ptr());
            let p = libc::nl_langinfo(NL_TIME_FIRST_WEEKDAY);
            if p.is_null() {
                0
            } else {
                *p as u8
            }
        };
        match value {
            1..=7 => u32::from(value - 1),
            _ => 1,
        }
    })
}

/// One cell of the month grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Day {
    pub y: i32,
    pub m: u32,
    pub d: u32,
    /// Belongs to the month the grid shows, not its leading or trailing days.
    pub in_month: bool,
    pub today: bool,
}

pub fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

pub fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap(y) => 29,
        _ => 28,
    }
}

/// Days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = i64::from(y) - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The inverse of [`days_from_civil`].
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = (yoe + era * 400) as i32 + i32::from(m <= 2);
    (y, m, d)
}

/// `0` = Sunday … `6` = Saturday.
pub fn weekday_of(y: i32, m: u32, d: u32) -> u32 {
    // 1970-01-01 was a Thursday.
    (days_from_civil(y, m, d) + 4).rem_euclid(7) as u32
}

/// `1..=366`.
pub fn day_of_year(y: i32, m: u32, d: u32) -> u32 {
    (days_from_civil(y, m, d) - days_from_civil(y, 1, 1)) as u32 + 1
}

/// The ISO 8601 week number, `1..=53`: weeks start Monday and week 1 holds
/// the year's first Thursday.
pub fn iso_week(y: i32, m: u32, d: u32) -> u32 {
    let days = days_from_civil(y, m, d);
    // The Thursday of this date's ISO week decides which year it belongs to.
    let monday_based = (weekday_of(y, m, d) + 6) % 7;
    let thursday = days - i64::from(monday_based) + 3;
    let (ty, _, _) = civil_from_days(thursday);
    ((thursday - days_from_civil(ty, 1, 1)) / 7) as u32 + 1
}

/// The date `n` days after (or before) `(y, m, d)`.
pub fn add_days(y: i32, m: u32, d: u32, n: i64) -> (i32, u32, u32) {
    civil_from_days(days_from_civil(y, m, d) + n)
}

/// `(y, m)` moved by `n` months.
pub fn add_months(y: i32, m: u32, n: i32) -> (i32, u32) {
    let idx = y * 12 + (m as i32 - 1) + n;
    (idx.div_euclid(12), idx.rem_euclid(12) as u32 + 1)
}

/// Today in local time.
pub fn today() -> (i32, u32, u32) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    local_date(secs)
}

fn local_date(unix: i64) -> (i32, u32, u32) {
    let t: libc::time_t = unix;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `t` is a valid time_t and `tm` a live, owned, zeroed struct;
    // `localtime_r` is the reentrant form and writes only through the pointer.
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return (1970, 1, 1);
    }
    (tm.tm_year + 1900, (tm.tm_mon + 1) as u32, tm.tm_mday as u32)
}

/// Unix seconds of local 00:00 on the date. `d` or `m` out of range roll over
/// (`mktime` normalises), so `local_midnight(y, m, d + 1)` is the day's end.
pub fn local_midnight(y: i32, m: u32, d: u32) -> i64 {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = y - 1900;
    tm.tm_mon = m as i32 - 1;
    tm.tm_mday = d as i32;
    tm.tm_isdst = -1;
    // SAFETY: `tm` is a live, owned struct; `mktime` reads and normalises it.
    unsafe { libc::mktime(&mut tm) }
}

/// The six weeks that show month `(y, m)`, starting on `week_start`.
pub fn grid(y: i32, m: u32, week_start: WeekStart) -> [[Day; 7]; 6] {
    let lead = (weekday_of(y, m, 1) + 7 - week_start.first()) % 7;
    let first = days_from_civil(y, m, 1) - i64::from(lead);
    let now = today();
    std::array::from_fn(|w| {
        std::array::from_fn(|i| {
            let (dy, dm, dd) = civil_from_days(first + (w * 7 + i) as i64);
            Day {
                y: dy,
                m: dm,
                d: dd,
                in_month: (dy, dm) == (y, m),
                today: (dy, dm, dd) == now,
            }
        })
    })
}

/// The bar's calendar state.
#[derive(Debug, Clone)]
pub struct Calendar {
    /// The month the grid shows.
    pub view_y: i32,
    pub view_m: u32,
    /// The day whose detail is open.
    pub selected: (i32, u32, u32),
    /// A provider owns `org.eclipse.Calendar1`.
    pub provider: bool,
    pub events: Vec<Event>,
    /// The wheel over the grid, so one click is one month.
    pub wheel: crate::widgets::Notches,
}

impl Default for Calendar {
    fn default() -> Self {
        let (y, m, d) = today();
        Calendar {
            view_y: y,
            view_m: m,
            selected: (y, m, d),
            provider: false,
            events: Vec::new(),
            wheel: Default::default(),
        }
    }
}

impl Calendar {
    /// Back to today's month with today selected.
    pub fn reset(&mut self) {
        let (y, m, d) = today();
        self.view_y = y;
        self.view_m = m;
        self.selected = (y, m, d);
    }

    /// Select a day; the view follows it into its month.
    pub fn select(&mut self, y: i32, m: u32, d: u32) {
        self.selected = (y, m, d);
        self.view_y = y;
        self.view_m = m;
    }

    /// Move the selection by `n` days.
    pub fn step_days(&mut self, n: i64) {
        let (y, m, d) = self.selected;
        let (y, m, d) = add_days(y, m, d, n);
        self.select(y, m, d);
    }

    /// Move the view (and the selection with it) by `n` months; the day
    /// clamps to the new month's length.
    pub fn step_months(&mut self, n: i32) {
        let (y, m) = add_months(self.view_y, self.view_m, n);
        let d = self.selected.2.min(days_in_month(y, m));
        self.select(y, m, d);
    }

    /// The range the service should hold: the visible month, one either side.
    pub fn range(&self) -> (i64, i64) {
        let (py, pm) = add_months(self.view_y, self.view_m, -1);
        let (ny, nm) = add_months(self.view_y, self.view_m, 2);
        (local_midnight(py, pm, 1), local_midnight(ny, nm, 1))
    }

    /// Events touching the local day `(y, m, d)`: all-day and multi-day ones
    /// on every day they span (an all-day `end` is exclusive, like any other).
    pub fn day_events(&self, y: i32, m: u32, d: u32) -> impl Iterator<Item = &Event> {
        let start = local_midnight(y, m, d);
        let end = local_midnight(y, m, d + 1);
        self.events
            .iter()
            .filter(move |e| e.start < end && (e.end > start || (e.end <= e.start && e.start >= start)))
    }
}

/// What the service said, as a bar message payload.
#[derive(Debug, Clone)]
pub enum Feed {
    Provider(bool),
    Events {
        start: i64,
        end: i64,
        events: Vec<Event>,
    },
}

impl Calendar {
    pub fn apply(&mut self, feed: Feed) {
        match feed {
            Feed::Provider(on) => {
                self.provider = on;
                if !on {
                    self.events.clear();
                }
            }
            Feed::Events { events, .. } => self.events = events,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(start: i64, end: i64, all_day: bool) -> Event {
        Event {
            id: "e".into(),
            title: "t".into(),
            start,
            end,
            all_day,
            calendar: "c".into(),
            colour: None,
            location: None,
        }
    }

    #[test]
    fn leap_years() {
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2100, 2), 28);
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(day_of_year(2024, 12, 31), 366);
        assert_eq!(day_of_year(2026, 12, 31), 365);
        assert_eq!(add_days(2024, 2, 28, 1), (2024, 2, 29));
        assert_eq!(add_days(2100, 2, 28, 1), (2100, 3, 1));
    }

    #[test]
    fn weekdays() {
        assert_eq!(weekday_of(1970, 1, 1), 4);
        assert_eq!(weekday_of(2026, 10, 22), 4); // Thursday
        assert_eq!(weekday_of(2024, 2, 29), 4);
        assert_eq!(weekday_of(2000, 1, 1), 6);
    }

    #[test]
    fn iso_weeks_across_year_boundaries() {
        // 2026 starts on a Thursday, so it has 53 weeks.
        assert_eq!(iso_week(2026, 12, 31), 53);
        assert_eq!(iso_week(2027, 1, 1), 53);
        assert_eq!(iso_week(2027, 1, 4), 1);
        assert_eq!(iso_week(2026, 1, 1), 1);
        assert_eq!(iso_week(2024, 12, 30), 1); // Monday of 2025-W01
        assert_eq!(iso_week(2021, 1, 3), 53);
        assert_eq!(iso_week(2026, 10, 22), 43);
    }

    #[test]
    fn month_arithmetic_rolls_over() {
        assert_eq!(add_months(2026, 12, 1), (2027, 1));
        assert_eq!(add_months(2026, 1, -1), (2025, 12));
        assert_eq!(add_months(2026, 3, -15), (2024, 12));
    }

    #[test]
    fn the_grid_is_six_whole_weeks_around_the_month() {
        for ws in [WeekStart::Monday, WeekStart::Sunday] {
            let g = grid(2026, 2, ws);
            let first = ws.first();
            assert_eq!(weekday_of(g[0][0].y, g[0][0].m, g[0][0].d), first);
            let inside: Vec<_> = g.iter().flatten().filter(|d| d.in_month).collect();
            assert_eq!(inside.len(), 28);
            assert_eq!(inside[0].d, 1);
            // Consecutive days throughout.
            let flat: Vec<_> = g.iter().flatten().collect();
            for pair in flat.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                assert_eq!(add_days(a.y, a.m, a.d, 1), (b.y, b.m, b.d));
            }
        }
        // October 2026 starts on a Thursday: Monday-first leads with 3 days.
        let g = grid(2026, 10, WeekStart::Monday);
        assert_eq!((g[0][0].m, g[0][0].d), (9, 28));
        assert!(g[0][3].in_month && g[0][3].d == 1);
        // Sunday-first leads with 4.
        let g = grid(2026, 10, WeekStart::Sunday);
        assert_eq!((g[0][0].m, g[0][0].d), (9, 27));
    }

    #[test]
    fn today_is_marked_once_in_its_own_month() {
        let (y, m, _) = today();
        let g = grid(y, m, WeekStart::Monday);
        assert_eq!(g.iter().flatten().filter(|d| d.today).count(), 1);
        let (ny, nm) = add_months(y, m, 12);
        assert!(grid(ny, nm, WeekStart::Monday).iter().flatten().all(|d| !d.today));
    }

    #[test]
    fn auto_resolves_to_a_real_weekday() {
        assert!(WeekStart::Auto.first() < 7);
    }

    /// One test owns the zone: the process-wide TZ is shared, so every
    /// assertion that depends on it lives here.
    #[test]
    fn local_midnights_follow_dst() {
        unsafe extern "C" {
            fn tzset();
        }
        // SAFETY: tests that read the zone only assert zone-independent facts.
        unsafe {
            std::env::set_var("TZ", "America/New_York");
            tzset();
        }
        // US spring-forward: 2026-03-08 has 23 hours, fall-back 2026-11-01 has 25.
        let day = |y, m, d| local_midnight(y, m, d + 1) - local_midnight(y, m, d);
        assert_eq!(day(2026, 3, 8), 23 * 3600);
        assert_eq!(day(2026, 11, 1), 25 * 3600);
        assert_eq!(day(2026, 10, 22), 24 * 3600);
        // Rolling over a month end normalises.
        assert_eq!(local_midnight(2026, 1, 32), local_midnight(2026, 2, 1));
        assert_eq!(local_date(local_midnight(2026, 3, 8)), (2026, 3, 8));

        // Events: an all-day event ends exclusive, a multi-day one spans.
        let mut cal = Calendar::default();
        let d = |y, m, d| local_midnight(y, m, d);
        cal.events = vec![
            event(d(2026, 3, 8), d(2026, 3, 9), true),
            event(d(2026, 3, 7), d(2026, 3, 10), true),
            event(d(2026, 3, 8) + 3600, d(2026, 3, 8) + 7200, false),
        ];
        let on = |cal: &Calendar, day| cal.day_events(2026, 3, day).count();
        assert_eq!(on(&cal, 6), 0);
        assert_eq!(on(&cal, 7), 1);
        assert_eq!(on(&cal, 8), 3);
        assert_eq!(on(&cal, 9), 1);
        assert_eq!(on(&cal, 10), 0, "an all-day end is exclusive");
    }

    #[test]
    fn stepping_moves_the_view_with_the_selection() {
        let mut cal = Calendar::default();
        cal.select(2026, 1, 31);
        cal.step_days(1);
        assert_eq!((cal.selected, cal.view_y, cal.view_m), ((2026, 2, 1), 2026, 2));
        cal.step_days(-1);
        assert_eq!((cal.view_y, cal.view_m), (2026, 1));
        cal.select(2026, 1, 31);
        cal.step_months(1);
        assert_eq!(cal.selected, (2026, 2, 28), "the day clamps");
        cal.step_months(-2);
        assert_eq!(cal.selected, (2025, 12, 28));
    }
}
