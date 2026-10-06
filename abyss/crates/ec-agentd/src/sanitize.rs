// SPDX-License-Identifier: AGPL-3.0-only
//! Message text sanitising (A-08 §6.1, as COMP-10 §3.2): C0 and C1 controls
//! other than newline and tab are dropped, and so are the bidirectional
//! overrides and isolates that let text lie about its own order.

/// The largest conversation message, in bytes (A-08 §6.1).
pub const MAX_TEXT: usize = 16 * 1024;

fn bad(c: char) -> bool {
    match c {
        '\n' | '\t' => false,
        c if c.is_control() => true, // C0, DEL and C1
        '\u{061C}' | '\u{200E}' | '\u{200F}' => true,
        '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' => true,
        _ => false,
    }
}

pub fn clean(s: &str) -> String {
    s.chars().filter(|c| !bad(*c)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_controls_and_bidi_keeps_whitespace() {
        assert_eq!(clean("a\u{1b}[31mb\tc\nd\u{7}"), "a[31mb\tc\nd");
        assert_eq!(clean("x\u{202E}evil\u{2066}y\u{85}z\r"), "xevilyz");
        assert_eq!(clean("héllo ✓"), "héllo ✓");
    }
}
