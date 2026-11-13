// SPDX-License-Identifier: Apache-2.0
//! The `.ti` text emitter. Output is `tic -x`-compatible and laid out exactly as
//! `infocmp -x -1` prints a compiled entry (minus its comment line): predefined
//! booleans, extended booleans, predefined numbers, extended numbers, predefined
//! strings, extended strings; each group sorted by name in byte order.

use crate::std_names::{BOOL_NAMES, NUM_NAMES, STR_NAMES};
use crate::{Cap, Entry, Value};

fn is_std(cap: &Cap) -> bool {
    match cap.value {
        Value::Bool => BOOL_NAMES.contains(&cap.name),
        Value::Num(_) => NUM_NAMES.contains(&cap.name),
        Value::Str(_) => STR_NAMES.contains(&cap.name),
    }
}

fn kind(cap: &Cap) -> u8 {
    match cap.value {
        Value::Bool => 0,
        Value::Num(_) => 1,
        Value::Str(_) => 2,
    }
}

/// Escape a string value the way `infocmp` prints it.
fn escape(s: &str, out: &mut String) {
    let b = s.as_bytes();
    for (i, &c) in b.iter().enumerate() {
        match c {
            0x1b => out.push_str("\\E"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            0x07 if i > 0 => out.push_str("\\007"),
            0x7f => out.push_str("^?"),
            0..=0x1f => {
                out.push('^');
                out.push(char::from(c + 64));
            }
            b'\\' | b',' | b'^' => {
                out.push('\\');
                out.push(char::from(c));
            }
            b' ' if i == 0 || i + 1 == b.len() => out.push_str("\\s"),
            0x80..=0xff => out.push_str(&format!("\\{c:03o}")),
            _ => out.push(char::from(c)),
        }
    }
}

/// Render the entry as terminfo source text.
pub fn source(entry: &Entry) -> String {
    let mut out = String::new();
    out.push_str(&entry.names);
    out.push_str(",\n");
    let mut caps: Vec<&Cap> = entry.caps.iter().collect();
    caps.sort_by(|a, b| {
        (kind(a), !is_std(a), a.name.as_bytes()).cmp(&(kind(b), !is_std(b), b.name.as_bytes()))
    });
    for c in caps {
        out.push('\t');
        out.push_str(c.name);
        match c.value {
            Value::Bool => {}
            Value::Num(n) => {
                if n >= 256 && n.is_power_of_two() {
                    out.push_str(&format!("#0x{n:x}"));
                } else {
                    out.push_str(&format!("#{n}"));
                }
            }
            Value::Str(s) => {
                out.push('=');
                escape(s, &mut out);
            }
        }
        out.push_str(",\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Variant, entry};

    #[test]
    fn escapes_match_infocmp_conventions() {
        let mut s = String::new();
        escape("\x1b[%i%p1%d;%p2%dH", &mut s);
        assert_eq!(s, "\\E[%i%p1%d;%p2%dH");
        s.clear();
        escape("\x07", &mut s);
        assert_eq!(s, "^G");
        s.clear();
        escape("\x1b]112\x07", &mut s);
        assert_eq!(s, "\\E]112\\007");
        s.clear();
        escape("\x1b\\", &mut s);
        assert_eq!(s, "\\E\\\\");
        s.clear();
        escape("\x1b[2 q", &mut s);
        assert_eq!(s, "\\E[2 q");
        s.clear();
        escape("a,b^c", &mut s);
        assert_eq!(s, "a\\,b\\^c");
    }

    #[test]
    fn header_and_shape() {
        let t = source(&entry(Variant::Indexed));
        assert!(t.starts_with("xterm-apotheosis|Apotheosis terminal with 256 colors,\n"));
        assert!(t.contains("\tcolors#0x100,\n"));
        assert!(t.contains("\tSmulx=\\E[4:%p1%dm,\n"));
        assert!(t.contains("\tTc,\n"));
    }
}
