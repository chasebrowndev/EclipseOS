// SPDX-License-Identifier: AGPL-3.0-only
//! Turning a command's stdout into [`Output`]s: the 4 KiB line cap, control
//! character stripping and the plain/JSON split (ADR 0065).
//!
//! Everything here handles untrusted bytes. Nothing is logged, and nothing is
//! interpreted as markup; the GUI renders the result as plain text.

use std::io::{self, Read};

use super::Output;

/// The longest line kept, in bytes. Anything past it, up to the next
/// newline, is dropped.
pub(super) const LINE_CAP: usize = 4096;

/// Reads `r` to EOF and calls `line` once per newline-terminated line (and
/// once for a trailing unterminated one). Each line is at most [`LINE_CAP`]
/// bytes, cut on a UTF-8 boundary; the buffer never grows past the cap
/// however long the command's line is.
pub(super) fn read_lines(mut r: impl Read, mut line: impl FnMut(&[u8])) -> io::Result<()> {
    let mut chunk = [0u8; 4096];
    let mut cur: Vec<u8> = Vec::with_capacity(256);
    let mut over = false;
    loop {
        let n = match r.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        let mut rest = &chunk[..n];
        while let Some(nl) = rest.iter().position(|&b| b == b'\n') {
            push_capped(&mut cur, &mut over, &rest[..nl]);
            emit(&mut cur, &mut over, &mut line);
            rest = &rest[nl + 1..];
        }
        push_capped(&mut cur, &mut over, rest);
    }
    if !cur.is_empty() || over {
        emit(&mut cur, &mut over, &mut line);
    }
    Ok(())
}

fn push_capped(cur: &mut Vec<u8>, over: &mut bool, bytes: &[u8]) {
    let room = LINE_CAP - cur.len();
    if bytes.len() > room {
        *over = true;
    }
    cur.extend_from_slice(&bytes[..bytes.len().min(room)]);
}

fn emit(cur: &mut Vec<u8>, over: &mut bool, line: &mut impl FnMut(&[u8])) {
    if *over {
        let keep = utf8_floor(cur);
        cur.truncate(keep);
    }
    line(cur);
    cur.clear();
    *over = false;
}

/// The length of `b` without a trailing, incomplete UTF-8 sequence, so a
/// line cut at the cap does not end in a replacement character.
fn utf8_floor(b: &[u8]) -> usize {
    // A sequence is at most four bytes: look back over at most three
    // continuation bytes for its lead byte.
    for back in 1..=4.min(b.len()) {
        let i = b.len() - back;
        let lead = b[i];
        if lead & 0xC0 == 0x80 {
            continue; // continuation byte
        }
        let want = match lead {
            0x00..=0x7F => 1,
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => return b.len(), // invalid lead: lossy decoding handles it
        };
        return if back < want { i } else { b.len() };
    }
    b.len()
}

/// Truncates `s` to at most [`LINE_CAP`] bytes on a char boundary. Lossy
/// decoding can lengthen a capped line (each bad byte becomes three).
fn cap_str(s: &mut String) {
    if s.len() <= LINE_CAP {
        return;
    }
    let mut end = LINE_CAP;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
}

/// Drops control characters (C0, DEL, C1) and the bidi embedding, override
/// and isolate controls, which could reorder what the bar shows. A tab
/// becomes a space.
pub(super) fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\t' => out.push(' '),
            c if c.is_control() => {}
            '\u{200E}' | '\u{200F}' | '\u{061C}' => {}
            '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' => {}
            c => out.push(c),
        }
    }
    cap_str(&mut out);
    out
}

/// Whether a raw line has anything but whitespace in it.
pub(super) fn is_blank(raw: &[u8]) -> bool {
    raw.iter().all(u8::is_ascii_whitespace)
}

/// One capped raw line to an [`Output`]. A line starting with `{` must be a
/// JSON object `{text, detail, tooltip, state}`; anything else is plain text.
/// `Err` carries our reason, never the line.
pub(super) fn parse_line(raw: &[u8]) -> Result<Output, &'static str> {
    let mut decoded = String::from_utf8_lossy(raw).into_owned();
    cap_str(&mut decoded);
    let line = decoded.trim_end_matches('\r');
    if line.trim_start().starts_with('{') {
        return parse_json(line);
    }
    Ok(Output {
        text: sanitize(line),
        detail: Vec::new(),
        tooltip: None,
        state: None,
    })
}

fn parse_json(line: &str) -> Result<Output, &'static str> {
    let value: serde_json::Value = serde_json::from_str(line).map_err(|_| "output line is not valid JSON")?;
    let obj = value.as_object().ok_or("output line is not a JSON object")?;
    let string = |key: &str| -> Result<Option<String>, &'static str> {
        match obj.get(key) {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::String(s)) => Ok(Some(sanitize(s))),
            Some(_) => Err("a JSON output field has the wrong type"),
        }
    };
    let detail = match obj.get("detail") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .map(|v| v.as_str().map(sanitize))
            .collect::<Option<Vec<_>>>()
            .ok_or("JSON output `detail` must be an array of strings")?,
        Some(_) => return Err("JSON output `detail` must be an array of strings"),
    };
    Ok(Output {
        text: string("text")?.unwrap_or_default(),
        detail,
        tooltip: string("tooltip")?,
        state: string("state")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(input: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        read_lines(input, |l| out.push(l.to_vec())).unwrap();
        out
    }

    #[test]
    fn plain_line() {
        let out = parse_line(b"  22\xC2\xB0C sunny\r").unwrap();
        assert_eq!(out.text, "  22°C sunny");
        assert!(out.detail.is_empty());
        assert_eq!(out.tooltip, None);
        assert_eq!(out.state, None);
    }

    #[test]
    fn json_line() {
        let out =
            parse_line(br#"{"text":"42%","detail":["a","b\u0007c"],"tooltip":"tip","state":"warning"}"#)
                .unwrap();
        assert_eq!(out.text, "42%");
        assert_eq!(out.detail, vec!["a".to_string(), "bc".to_string()]);
        assert_eq!(out.tooltip.as_deref(), Some("tip"));
        assert_eq!(out.state.as_deref(), Some("warning"));
    }

    #[test]
    fn json_partial_and_bad() {
        let out = parse_line(br#"{"text":"x"}"#).unwrap();
        assert_eq!(out.text, "x");
        assert!(out.detail.is_empty());
        assert!(parse_line(b"{not json").is_err());
        assert!(parse_line(br#"{"text":1}"#).is_err());
        assert!(parse_line(br#"{"detail":[1]}"#).is_err());
        // Not starting with `{`: plain, even if it looks like JSON later.
        assert_eq!(parse_line(br#"[{"text":1}]"#).unwrap().text, r#"[{"text":1}]"#);
    }

    #[test]
    fn control_and_bidi_stripped() {
        let out = parse_line(b"a\x1b[31mred\x07\tb\xC2\x85c\xE2\x80\xAEd\x7f").unwrap();
        assert_eq!(out.text, "a[31mred bcd");
    }

    #[test]
    fn splits_lines() {
        assert_eq!(
            lines(b"one\ntwo\n\nthree"),
            vec![b"one".to_vec(), b"two".to_vec(), b"".to_vec(), b"three".to_vec()]
        );
        assert!(lines(b"").is_empty());
    }

    #[test]
    fn caps_long_lines_on_char_boundary() {
        // 4095 ASCII bytes then a two-byte char straddling the cap.
        let mut input = vec![b'a'; LINE_CAP - 1];
        input.extend_from_slice("é".as_bytes());
        input.extend_from_slice(&[b'z'; 10_000]);
        input.extend_from_slice(b"\nnext\n");
        let got = lines(&input);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].len(), LINE_CAP - 1);
        assert!(got[0].iter().all(|&b| b == b'a'));
        assert_eq!(got[1], b"next");

        let long = vec![b'x'; 3 * LINE_CAP];
        let got = lines(&long);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].len(), LINE_CAP);
    }

    #[test]
    fn lossy_growth_is_capped() {
        let raw = vec![0xFFu8; LINE_CAP];
        let out = parse_line(&raw).unwrap();
        assert!(out.text.len() <= LINE_CAP);
        assert!(out.text.chars().all(|c| c == '\u{FFFD}'));
    }
}
