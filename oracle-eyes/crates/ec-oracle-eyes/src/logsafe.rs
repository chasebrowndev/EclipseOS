// SPDX-License-Identifier: AGPL-3.0-only

//! The one door screen-derived text goes through on its way into a log.
//!
//! Oracle-Eyes' debug log (see `logging.rs`) is a trace of what the daemon
//! read and decided, and what it read is whatever was on screen. Two rules
//! follow, and this module is where both are enforced:
//!
//! * Password-role values and keystroke/input content are never logged. The
//!   daemon has no password concept, only [`crate::redact`]'s regex markers,
//!   so a line that mentions a secret, bearer token or private key is blanked
//!   whole rather than trusted to have been scrubbed well.
//! * Everything logged that came off the screen goes through [`log_safe`].
//!   There is no second path. Counts, boxes and timings are not screen text
//!   and may be logged directly.
//!
//! Like redaction itself this is best-effort: a secret that does not look
//! like one is not caught.

use std::collections::BTreeMap;

use crate::redact;

/// Marker classes that mean the surrounding line was about a credential.
const BLANKING: [&str; 3] = [
    "[REDACTED:secret]",
    "[REDACTED:bearer]",
    "[REDACTED:private_key]",
];

/// What stands in for a blanked line.
const BLANK: &str = "[line withheld]";

/// `text`, redacted, with every line that still carries a secret, bearer or
/// private-key marker replaced wholesale. Safe to hand to `tracing`.
pub fn log_safe(text: &str) -> String {
    redact::redact(text)
        .lines()
        .map(|l| {
            if BLANKING.iter().any(|m| l.contains(m)) {
                BLANK
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// How many markers of each class `redacted` contains, as `class=n` pairs
/// ("secret=1 jwt=2"), or `none`. Takes text that has already been through
/// [`redact::redact`]; reports classes and counts, never what was masked.
pub fn marker_counts(redacted: &str) -> String {
    const OPEN: &str = "[REDACTED:";
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut rest = redacted;
    while let Some(i) = rest.find(OPEN) {
        rest = &rest[i + OPEN.len()..];
        if let Some(j) = rest.find(']') {
            *counts.entry(&rest[..j]).or_default() += 1;
            rest = &rest[j..];
        }
    }
    if counts.is_empty() {
        return "none".to_string();
    }
    counts
        .iter()
        .map(|(k, n)| format!("{k}={n}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_passes_through() {
        assert_eq!(
            log_safe("Hello world\nsecond line"),
            "Hello world\nsecond line"
        );
    }

    #[test]
    fn a_labelled_password_line_is_blanked_whole() {
        let out = log_safe("Sign in\npassword: hunter2\nForgot it?");
        assert_eq!(out, format!("Sign in\n{BLANK}\nForgot it?"));
        assert!(!out.contains("hunter2"));
        assert!(!out.contains("password"));
    }

    #[test]
    fn a_bearer_header_is_blanked() {
        let out = log_safe("Authorization: Bearer abc.def.ghi");
        assert_eq!(out, BLANK);
    }

    #[test]
    fn a_private_key_armour_line_is_blanked() {
        let out = log_safe("x\n-----BEGIN RSA PRIVATE KEY-----\ny");
        assert_eq!(out, format!("x\n{BLANK}\ny"));
    }

    #[test]
    fn other_markers_keep_their_line() {
        let out = log_safe("commit 0123456789abcdef0123456789abcdef01234567 fixed");
        assert_eq!(out, "commit [REDACTED:hex] fixed");
    }

    #[test]
    fn nothing_raw_survives_a_mixed_screen() {
        let out = log_safe("token = s3cr3tvalue\napi sk-abcdefghijklmnopqrstuv end");
        assert!(!out.contains("s3cr3tvalue"));
        assert!(!out.contains("sk-abc"));
    }

    #[test]
    fn counts_name_classes_not_contents() {
        let r =
            redact::redact("a password: x\nb password: y\nsha 0123456789abcdef0123456789abcdef");
        assert_eq!(marker_counts(&r), "hex=1 secret=2");
        assert_eq!(marker_counts("clean"), "none");
    }
}
