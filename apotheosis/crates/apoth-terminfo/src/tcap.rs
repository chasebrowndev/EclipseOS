// SPDX-License-Identifier: Apache-2.0
//! The XTGETTCAP responder (T-19 §4, VTCORE-25, VTCORE-53): `DCS + q Pt ST`.
//!
//! Pure: bytes in, reply byte strings out. The caller queues each reply on the pane's
//! reply queue (T-00 §4.2). Hex encoding of name and value is the reply-injection
//! defence (T-18 §2). Request content is never logged.

use crate::table::INDEXED;
use crate::{Cap, Value};

const ST: &[u8] = b"\x1b\\";

/// What a name resolves to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TcapAnswer {
    /// Boolean capability: answered without `=`.
    Bool,
    /// String or numeric capability (numbers already as decimal text).
    Text(String),
}

fn unhex(b: &[u8]) -> Option<Vec<u8>> {
    if !b.len().is_multiple_of(2) {
        return None;
    }
    let nib = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    b.chunks(2)
        .map(|p| Some(nib(p[0])? << 4 | nib(p[1])?))
        .collect()
}

fn hex_upper(b: &[u8], out: &mut Vec<u8>) {
    const D: &[u8; 16] = b"0123456789ABCDEF";
    for &c in b {
        out.push(D[usize::from(c >> 4)]);
        out.push(D[usize::from(c & 15)]);
    }
}

/// Resolve a decoded capability name. `term_name` is the `TERM` exported to this
/// pane's child (`TN`); `Co`, `RGB` and `Tc` describe the emulator whatever the
/// installed database says; everything else comes from the `xterm-apotheosis` table.
pub fn lookup(name: &[u8], term_name: &str) -> Option<TcapAnswer> {
    match name {
        b"TN" => return Some(TcapAnswer::Text(term_name.to_owned())),
        b"Co" => return Some(TcapAnswer::Text("256".to_owned())),
        b"RGB" => return Some(TcapAnswer::Text("8/8/8".to_owned())),
        _ => {}
    }
    let name = std::str::from_utf8(name).ok()?;
    let i = INDEXED
        .binary_search_by(|c: &Cap| c.name.as_bytes().cmp(name.as_bytes()))
        .ok()?;
    Some(match INDEXED[i].value {
        Value::Bool => TcapAnswer::Bool,
        Value::Num(n) => TcapAnswer::Text(n.to_string()),
        Value::Str(s) => TcapAnswer::Text(s.to_owned()),
    })
}

/// Answer a `DCS + q Pt ST` payload `Pt` (the bytes between `q` and ST): one complete
/// reply (`ESC P ... ESC \`) per processed name. Processing stops after the first
/// unknown or non-hex name, which is itself answered with `DCS 0 + r`.
pub fn xtgettcap(payload: &[u8], term_name: &str) -> Vec<Vec<u8>> {
    let mut replies = Vec::new();
    for item in payload.split(|&c| c == b';') {
        let mut r = Vec::new();
        let Some(name) = unhex(item) else {
            r.extend_from_slice(b"\x1bP0+r");
            r.extend_from_slice(ST);
            replies.push(r);
            break;
        };
        match lookup(&name, term_name) {
            None => {
                r.extend_from_slice(b"\x1bP0+r");
                hex_upper(&name, &mut r);
                r.extend_from_slice(ST);
                replies.push(r);
                break;
            }
            Some(ans) => {
                r.extend_from_slice(b"\x1bP1+r");
                hex_upper(&name, &mut r);
                if let TcapAnswer::Text(v) = ans {
                    r.push(b'=');
                    hex_upper(v.as_bytes(), &mut r);
                }
                r.extend_from_slice(ST);
                replies.push(r);
            }
        }
    }
    replies
}

/// [`xtgettcap`] with the replies concatenated.
pub fn xtgettcap_flat(payload: &[u8], term_name: &str) -> Vec<u8> {
    xtgettcap(payload, term_name).concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(s: &str) -> String {
        let mut v = Vec::new();
        hex_upper(s.as_bytes(), &mut v);
        String::from_utf8(v).unwrap()
    }
    fn one(payload: &str, tn: &str) -> Vec<String> {
        xtgettcap(payload.as_bytes(), tn)
            .into_iter()
            .map(|r| String::from_utf8(r).unwrap().replace('\x1b', "ESC"))
            .collect()
    }

    #[test]
    fn string_numeric_boolean() {
        assert_eq!(
            one(&h("Ms"), "xterm-apotheosis"),
            [format!(
                "ESCP1+r{}={}ESC\\",
                h("Ms"),
                h("\x1b]52;%p1%s;%p2%s\x07")
            )]
        );
        assert_eq!(
            one(&h("colors"), "x"),
            [format!("ESCP1+r{}={}ESC\\", h("colors"), h("256"))]
        );
        assert_eq!(one(&h("Tc"), "x"), [format!("ESCP1+r{}ESC\\", h("Tc"))]);
    }

    #[test]
    fn dynamic_tn_co_rgb() {
        assert_eq!(
            one(
                &format!("{};{};{}", h("TN"), h("Co"), h("RGB")),
                "xterm-256color"
            ),
            [
                format!("ESCP1+r{}={}ESC\\", h("TN"), h("xterm-256color")),
                format!("ESCP1+r{}={}ESC\\", h("Co"), h("256")),
                format!("ESCP1+r{}={}ESC\\", h("RGB"), h("8/8/8")),
            ]
        );
    }

    #[test]
    fn lowercase_hex_in_uppercase_out() {
        let lower = h("Ss").to_lowercase();
        assert_eq!(
            xtgettcap(lower.as_bytes(), "x"),
            xtgettcap(h("Ss").as_bytes(), "x")
        );
        assert!(one(&lower, "x")[0].contains("5373"));
    }

    #[test]
    fn unknown_name_stops_processing() {
        assert_eq!(
            one(&h("nosuchcap"), "x"),
            [format!("ESCP0+r{}ESC\\", h("nosuchcap"))]
        );
        let p = format!("{};{};{}", h("Tc"), h("zzzz"), h("Ss"));
        let r = one(&p, "x");
        assert_eq!(r.len(), 2);
        assert_eq!(r[1], format!("ESCP0+r{}ESC\\", h("zzzz")));
    }

    #[test]
    fn invalid_hex_echoes_nothing_and_stops() {
        for bad in ["4G", "123", "zz"] {
            let p = format!("{bad};{}", h("Tc"));
            assert_eq!(one(&p, "x"), ["ESCP0+rESC\\".to_owned()]);
        }
    }

    #[test]
    fn empty_string_cap_is_valid() {
        // An empty value answers `DCS 1 + r NAME = ST`: present, empty.
        let r = xtgettcap_flat(h("TN").as_bytes(), "");
        assert_eq!(r, format!("\x1bP1+r{}=\x1b\\", h("TN")).into_bytes());
    }

    #[test]
    fn replies_contain_only_hex_between_framing() {
        // A hostile TN cannot inject framing: the value is hex-encoded.
        let r = xtgettcap_flat(h("TN").as_bytes(), "a\x1b\\b");
        assert_eq!(r.iter().filter(|&&c| c == 0x1b).count(), 2);
    }

    #[test]
    fn answers_agree_with_table() {
        for c in INDEXED {
            let r = xtgettcap_flat(h(c.name).as_bytes(), "x");
            let want = match c.value {
                Value::Bool => format!("\x1bP1+r{}\x1b\\", h(c.name)),
                Value::Num(n) => format!("\x1bP1+r{}={}\x1b\\", h(c.name), h(&n.to_string())),
                Value::Str(s) => format!("\x1bP1+r{}={}\x1b\\", h(c.name), h(s)),
            };
            assert_eq!(String::from_utf8(r).unwrap(), want, "{}", c.name);
        }
    }
}
