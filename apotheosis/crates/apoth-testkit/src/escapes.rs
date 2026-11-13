// SPDX-License-Identifier: AGPL-3.0-only
//! The byte-escape layer used inside fixture `feed` / `replies` strings.
//!
//! Applied after KDL string decoding, normally to raw strings (`#"..."#`) so that `\e`
//! survives KDL. Escapes: `\e \a \b \f \n \r \t \v \0 \\ \" \xNN` (one raw byte, so C1
//! bytes and malformed UTF-8 are expressible) and `\u{H..}` (a scalar, UTF-8 encoded).
//! Everything else is literal UTF-8.

/// Decode an escaped string to bytes.
///
/// # Errors
/// An unknown or malformed escape.
pub fn unescape(s: &str) -> Result<Vec<u8>, String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' {
            out.push(b[i]);
            i += 1;
            continue;
        }
        i += 1;
        let Some(&e) = b.get(i) else {
            return Err("dangling backslash".into());
        };
        i += 1;
        match e {
            b'e' => out.push(0x1b),
            b'a' => out.push(0x07),
            b'b' => out.push(0x08),
            b'f' => out.push(0x0c),
            b'n' => out.push(0x0a),
            b'r' => out.push(0x0d),
            b't' => out.push(0x09),
            b'v' => out.push(0x0b),
            b'0' => out.push(0x00),
            b'\\' => out.push(b'\\'),
            b'"' => out.push(b'"'),
            b'x' => {
                let h = s.get(i..i + 2).ok_or("short \\x escape")?;
                out.push(u8::from_str_radix(h, 16).map_err(|_| format!("bad \\x{h}"))?);
                i += 2;
            }
            b'u' => {
                if b.get(i) != Some(&b'{') {
                    return Err("expected `{` after \\u".into());
                }
                let end = s[i..].find('}').ok_or("unterminated \\u{")? + i;
                let h = &s[i + 1..end];
                let ch = u32::from_str_radix(h, 16)
                    .ok()
                    .and_then(char::from_u32)
                    .ok_or_else(|| format!("bad \\u{{{h}}}"))?;
                let mut buf = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                i = end + 1;
            }
            other => return Err(format!("unknown escape `\\{}`", other as char)),
        }
    }
    Ok(out)
}

/// Encode bytes as an escaped string that [`unescape`] reads back exactly.
#[must_use]
pub fn escape(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        // Pass valid multi-byte UTF-8 through unchanged; C1 scalars are escaped.
        let rest = &bytes[i..];
        let take = utf8_prefix(rest);
        if take > 1 {
            let s = std::str::from_utf8(&rest[..take]).unwrap_or("");
            for c in s.chars() {
                if (0x80..0xa0).contains(&(c as u32)) {
                    out.push_str(&format!("\\u{{{:x}}}", c as u32));
                } else {
                    out.push(c);
                }
            }
            i += take;
            continue;
        }
        let b = bytes[i];
        i += 1;
        match b {
            0x1b => out.push_str("\\e"),
            0x07 => out.push_str("\\a"),
            0x08 => out.push_str("\\b"),
            0x0c => out.push_str("\\f"),
            0x0a => out.push_str("\\n"),
            0x0d => out.push_str("\\r"),
            0x09 => out.push_str("\\t"),
            0x0b => out.push_str("\\v"),
            0x00 => out.push_str("\\0"),
            b'\\' => out.push_str("\\\\"),
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out
}

/// Length of the valid multi-byte UTF-8 scalar at the start of `b` (1 if ASCII or invalid).
fn utf8_prefix(b: &[u8]) -> usize {
    let n = match b.first() {
        Some(0xc2..=0xdf) => 2,
        Some(0xe0..=0xef) => 3,
        Some(0xf0..=0xf4) => 4,
        _ => return 1,
    };
    match b.get(..n).map(std::str::from_utf8) {
        Some(Ok(_)) => n,
        _ => 1,
    }
}

/// Human-readable form for diffs (like [`escape`], but `\"` is not needed).
#[must_use]
pub fn visible(bytes: &[u8]) -> String {
    escape(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_each_escape() {
        assert_eq!(
            unescape("\\e[1m\\r\\n\\t\\a\\b\\f\\v\\0\\\\").unwrap(),
            b"\x1b[1m\r\n\t\x07\x08\x0c\x0b\x00\\"
        );
        assert_eq!(unescape("\\x9b\\xff").unwrap(), vec![0x9b, 0xff]);
        assert_eq!(unescape("\\u{e9}é").unwrap(), "éé".as_bytes());
        assert!(unescape("\\q").is_err());
        assert!(unescape("\\x1").is_err());
        assert!(unescape("\\").is_err());
        assert!(unescape("\\u{110000}").is_err());
    }

    #[test]
    fn round_trips_arbitrary_bytes() {
        let mut all: Vec<u8> = (0..=255).collect();
        all.extend_from_slice("日本 é 😀".as_bytes());
        all.extend_from_slice(&[0xc2, 0x9b, 0xe2, 0x82]); // C1 scalar, then truncated scalar
        assert_eq!(unescape(&escape(&all)).unwrap(), all);
    }
}
