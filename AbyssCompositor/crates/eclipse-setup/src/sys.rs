// SPDX-License-Identifier: AGPL-3.0-only
//! Read-only questions the wizard asks the machine.

use crate::data;
use crate::model::ZoneClock;
use std::process::{Command, Stdio};

/// What `date` says at `zone`: `HH:MM` and the numeric offset. Fixed argv, the
/// zone only ever reaches it as the `TZ` variable, and only after it has been
/// checked against the same pattern the helper enforces. `None` when `date`
/// cannot answer; the step then simply shows no clock.
pub fn zone_clock(zone: &str) -> Option<ZoneClock> {
    if !data::valid_zone(zone) {
        return None;
    }
    let out = Command::new("date")
        .env("TZ", zone)
        .arg("+%H:%M %z")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_clock(&String::from_utf8_lossy(&out.stdout))
}

/// `14:32 +0200`.
pub fn parse_clock(line: &str) -> Option<ZoneClock> {
    let (time, offset) = line.trim().split_once(' ')?;
    let ok_time = time.len() == 5
        && time.as_bytes()[2] == b':'
        && time.bytes().filter(|b| *b != b':').all(|b| b.is_ascii_digit());
    let ok_off = offset.len() == 5
        && matches!(offset.as_bytes()[0], b'+' | b'-')
        && offset.bytes().skip(1).all(|b| b.is_ascii_digit());
    (ok_time && ok_off).then(|| ZoneClock {
        time: time.to_owned(),
        offset: offset.to_owned(),
    })
}

/// A numeric `+0530` offset as hours east of UTC: `5.5`.
pub fn offset_hours(offset: &str) -> Option<f32> {
    let sign = match offset.as_bytes().first()? {
        b'+' => 1.0,
        b'-' => -1.0,
        _ => return None,
    };
    let digits = offset.get(1..)?;
    if digits.len() != 4 {
        return None;
    }
    let h: f32 = digits[..2].parse().ok()?;
    let m: f32 = digits[2..].parse().ok()?;
    Some(sign * (h + m / 60.0))
}

/// `+0530` as `UTC+05:30`.
pub fn offset_label(offset: &str) -> String {
    match (offset.get(..1), offset.get(1..3), offset.get(3..5)) {
        (Some(sign), Some(h), Some(m)) if offset.len() == 5 => format!("UTC{sign}{h}:{m}"),
        _ => "UTC".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_date_line_is_parsed_strictly() {
        let c = parse_clock("14:32 +0200\n").unwrap();
        assert_eq!((c.time.as_str(), c.offset.as_str()), ("14:32", "+0200"));
        for bad in [
            "",
            "14:32",
            "1432 +0200",
            "14:32 0200",
            "aa:bb +0200",
            "14:32 +02:00",
        ] {
            assert!(parse_clock(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn offsets_become_hours_and_labels() {
        assert_eq!(offset_hours("+0530"), Some(5.5));
        assert_eq!(offset_hours("-0800"), Some(-8.0));
        assert_eq!(offset_hours("0530"), None);
        assert_eq!(offset_label("+0530"), "UTC+05:30");
        assert_eq!(offset_label("-0800"), "UTC-08:00");
        assert_eq!(offset_label("x"), "UTC");
    }

    #[test]
    fn a_bad_zone_never_reaches_date() {
        assert!(zone_clock("../../etc/passwd").is_none());
        assert!(zone_clock("").is_none());
    }
}
