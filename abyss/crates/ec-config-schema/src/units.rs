// SPDX-License-Identifier: Apache-2.0
//! Textual forms of `Ty::Duration` and `Ty::Bytes`.

/// `"150ms"`, `"2s"`, `"1.5s"` to whole milliseconds. A bare number, a
/// negative, a non-finite or an unknown unit is `None`; sub-millisecond
/// fractions round to nearest.
pub fn parse_duration(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, mult) = if let Some(n) = s.strip_suffix("ms") {
        (n, 1.0)
    } else {
        (s.strip_suffix('s')?, 1000.0)
    };
    let num = num.trim();
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return None;
    }
    let v: f64 = num.parse().ok()?;
    let ms = (v * mult).round();
    (0.0..1.8e19).contains(&ms).then_some(ms as u64)
}

/// The shortest exact form: whole seconds as `"2s"`, otherwise `"150ms"`.
pub fn format_duration(ms: u64) -> String {
    if ms != 0 && ms % 1000 == 0 {
        format!("{}s", ms / 1000)
    } else {
        format!("{ms}ms")
    }
}

/// `"50MiB"`, `"512KiB"`, `"1GiB"`, `"4096"` (plain bytes; also `"4096B"`) to
/// bytes. Binary units only; `None` on overflow or anything else.
pub fn parse_bytes(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, shift) = if let Some(n) = s.strip_suffix("GiB") {
        (n, 30)
    } else if let Some(n) = s.strip_suffix("MiB") {
        (n, 20)
    } else if let Some(n) = s.strip_suffix("KiB") {
        (n, 10)
    } else if let Some(n) = s.strip_suffix('B') {
        (n, 0)
    } else {
        (s, 0)
    };
    let num = num.trim();
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    num.parse::<u64>().ok()?.checked_mul(1u64 << shift)
}

/// The largest binary unit that divides `n` exactly, else plain bytes.
pub fn format_bytes(n: u64) -> String {
    for (shift, unit) in [(30, "GiB"), (20, "MiB"), (10, "KiB")] {
        if n != 0 && n % (1u64 << shift) == 0 {
            return format!("{}{unit}", n >> shift);
        }
    }
    n.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("150ms"), Some(150));
        assert_eq!(parse_duration("2s"), Some(2000));
        assert_eq!(parse_duration("1.5s"), Some(1500));
        assert_eq!(parse_duration("0ms"), Some(0));
        for bad in ["", "150", "ms", "-1s", "1m", "1..5s", "abcs", "1e3s", "inf s"] {
            assert_eq!(parse_duration(bad), None, "{bad}");
        }
    }

    #[test]
    fn duration_round_trip() {
        for ms in [0, 1, 150, 999, 1000, 1500, 2000, 86_400_000] {
            assert_eq!(parse_duration(&format_duration(ms)), Some(ms));
        }
        assert_eq!(format_duration(2000), "2s");
        assert_eq!(format_duration(1500), "1500ms");
    }

    #[test]
    fn bytes() {
        assert_eq!(parse_bytes("50MiB"), Some(50 << 20));
        assert_eq!(parse_bytes("512KiB"), Some(512 << 10));
        assert_eq!(parse_bytes("1GiB"), Some(1 << 30));
        assert_eq!(parse_bytes("4096"), Some(4096));
        assert_eq!(parse_bytes("4096B"), Some(4096));
        for bad in [
            "",
            "MiB",
            "-1",
            "1.5MiB",
            "1TiB",
            "1mib",
            "18446744073709551615GiB",
        ] {
            assert_eq!(parse_bytes(bad), None, "{bad}");
        }
    }

    #[test]
    fn bytes_round_trip() {
        for n in [0, 1, 4096, 1 << 10, 50 << 20, 1 << 30, (1 << 30) + 1, u64::MAX] {
            assert_eq!(parse_bytes(&format_bytes(n)), Some(n));
        }
        assert_eq!(format_bytes(50 << 20), "50MiB");
    }
}
