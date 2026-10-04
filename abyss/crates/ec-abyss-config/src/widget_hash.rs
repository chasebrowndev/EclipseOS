// SPDX-License-Identifier: AGPL-3.0-only
//! The canonical hash of what a command widget runs (ADR 0067 "Hash";
//! COMP-10 §3.11).
//!
//! blake3 in derive-key mode, keyed by [`CONTEXT`], over this encoding, where
//! `u8`/`u64` are little-endian and `str` is `u64 byte length ‖ UTF-8 bytes`
//! and `list` is `u64 count ‖ str…`:
//!
//! ```text
//! str   kind            "exec" | "stream"
//! list  argv
//! u8    interval tag    1 for exec, then u64 interval in ms; 0 for stream
//! u8    on-click tag    0 absent | 1 present, then list
//! u8    on-scroll-up    same
//! u8    on-scroll-down  same
//! ```
//!
//! Not the KDL text: whitespace, comments, property order and `"5s"` vs `5000`
//! all hash the same, because the parser has already normalised them. The
//! name, the icon and anything else that does not run are not covered.
//! Declarative `source` widgets run nothing and have no hash.

use super::{CustomWidget, CustomWidgetKind};

/// Domain separation. A change to the encoding above is a new context string,
/// never a silent reinterpretation of old approvals.
pub const CONTEXT: &str = "EclipseOS 2026-09-26 command widget approval v1";

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct WidgetHash(pub [u8; 32]);

impl WidgetHash {
    pub fn to_hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for b in self.0 {
            s.push(char::from_digit(u32::from(b >> 4), 16).unwrap_or('0'));
            s.push(char::from_digit(u32::from(b & 0xf), 16).unwrap_or('0'));
        }
        s
    }

    /// Exactly 64 lowercase hex digits, or `None`.
    pub fn from_hex(s: &str) -> Option<WidgetHash> {
        let b = s.as_bytes();
        if b.len() != 64 {
            return None;
        }
        let digit = |c: u8| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        };
        let mut out = [0u8; 32];
        for (i, o) in out.iter_mut().enumerate() {
            *o = digit(b[2 * i])? << 4 | digit(b[2 * i + 1])?;
        }
        Some(WidgetHash(out))
    }
}

impl std::fmt::Debug for WidgetHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "WidgetHash({})", self.to_hex())
    }
}

impl std::fmt::Display for WidgetHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// The hash of a command widget; `None` for a declarative `source` widget.
pub fn hash(w: &CustomWidget) -> Option<WidgetHash> {
    let (kind, argv, interval) = match &w.kind {
        CustomWidgetKind::Exec { argv, interval_ms } => ("exec", argv, Some(*interval_ms)),
        CustomWidgetKind::Stream { argv } => ("stream", argv, None),
        CustomWidgetKind::Source { .. } => return None,
    };
    let mut h = blake3::Hasher::new_derive_key(CONTEXT);
    put_str(&mut h, kind);
    put_list(&mut h, argv);
    match interval {
        Some(ms) => {
            h.update(&[1]);
            h.update(&u64::from(ms).to_le_bytes());
        }
        None => {
            h.update(&[0]);
        }
    }
    for action in [&w.on_click, &w.on_scroll_up, &w.on_scroll_down] {
        match action {
            None => {
                h.update(&[0]);
            }
            Some(argv) => {
                h.update(&[1]);
                put_list(&mut h, argv);
            }
        }
    }
    Some(WidgetHash(*h.finalize().as_bytes()))
}

/// Domain separation for [`model_command_hash`]: never equal to a widget
/// hash of the same bytes.
pub const MODEL_COMMAND_CONTEXT: &str = "EclipseOS 2026-10-02 oracle-eyes model command approval v1";

/// The hash of the Oracle Eyes model command (owner decision 3): blake3
/// derive-key under [`MODEL_COMMAND_CONTEXT`] over `list argv`, encoded as
/// above. The flags the daemon appends itself are not part of it.
pub fn model_command_hash(argv: &[String]) -> WidgetHash {
    let mut h = blake3::Hasher::new_derive_key(MODEL_COMMAND_CONTEXT);
    put_list(&mut h, argv);
    WidgetHash(*h.finalize().as_bytes())
}

fn put_str(h: &mut blake3::Hasher, s: &str) {
    h.update(&(s.len() as u64).to_le_bytes());
    h.update(s.as_bytes());
}

fn put_list(h: &mut blake3::Hasher, v: &[String]) {
    h.update(&(v.len() as u64).to_le_bytes());
    for s in v {
        put_str(h, s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(text: &str) -> CustomWidget {
        let cfg = crate::parse_single_for_tests(text);
        assert!(cfg.errors.is_empty(), "{text}: {:?}", cfg.errors);
        let [w] = &cfg.bar.custom_widgets[..] else {
            panic!("{text}: expected one widget")
        };
        w.clone()
    }

    fn h(text: &str) -> WidgetHash {
        hash(&one(text)).expect("a command widget")
    }

    const LOAD: &str = "bar {\n    widget \"load\" {\n        exec \"cut\" \"--delimiter= \" \"--fields=1-3\" \"/proc/loadavg\"\n        \
                        interval-ms \"5s\"\n        on-click \"foot\" \"top\"\n    }\n}\n";

    #[test]
    fn formatting_whitespace_and_order_do_not_change_it() {
        let base = h(LOAD);
        for same in [
            // One line, semicolons, other node order, ms instead of "5s".
            "bar { widget \"load\" { on-click \"foot\" \"top\"; interval-ms 5000; exec \"cut\" \"--delimiter= \" \"--fields=1-3\" \"/proc/loadavg\"; } }",
            // Comments, an icon, and a different name do not run.
            "// hi\nbar {\n  /* x */ widget \"other\" {\n    icon \"x\"\n    exec \"cut\"   \"--delimiter= \" \"--fields=1-3\" \"/proc/loadavg\" // y\n    interval-ms \"5000ms\"\n    on-click \"foot\" \"top\"\n  }\n}\n",
        ] {
            assert_eq!(h(same), base, "{same}");
        }
    }

    #[test]
    fn anything_that_runs_changes_it() {
        let base = h(LOAD);
        for other in [
            // One argv byte.
            "bar { widget \"load\" { exec \"cut\" \"--delimiter= \" \"--fields=1-4\" \"/proc/loadavg\"; interval-ms 5000; on-click \"foot\" \"top\"; } }",
            // Argument boundaries (length prefixes): "a" "bc" vs "ab" "c".
            "bar { widget \"load\" { exec \"cut\" \"--delimiter=\" \" --fields=1-3\" \"/proc/loadavg\"; interval-ms 5000; on-click \"foot\" \"top\"; } }",
            // Interval.
            "bar { widget \"load\" { exec \"cut\" \"--delimiter= \" \"--fields=1-3\" \"/proc/loadavg\"; interval-ms 6000; on-click \"foot\" \"top\"; } }",
            // Stream instead of exec.
            "bar { widget \"load\" { exec \"cut\" \"--delimiter= \" \"--fields=1-3\" \"/proc/loadavg\"; stream #true; on-click \"foot\" \"top\"; } }",
            // Action absent.
            "bar { widget \"load\" { exec \"cut\" \"--delimiter= \" \"--fields=1-3\" \"/proc/loadavg\"; interval-ms 5000; } }",
            // Action moved to another slot.
            "bar { widget \"load\" { exec \"cut\" \"--delimiter= \" \"--fields=1-3\" \"/proc/loadavg\"; interval-ms 5000; on-scroll-up \"foot\" \"top\"; } }",
            // Action changed.
            "bar { widget \"load\" { exec \"cut\" \"--delimiter= \" \"--fields=1-3\" \"/proc/loadavg\"; interval-ms 5000; on-click \"foot\" \"htop\"; } }",
        ] {
            assert_ne!(h(other), base, "{other}");
        }
    }

    /// The parser never yields an empty action argv, but the encoding keeps
    /// absent and empty apart anyway.
    #[test]
    fn absent_and_empty_actions_differ() {
        let mut w = one(LOAD);
        w.on_scroll_down = None;
        let absent = hash(&w);
        w.on_scroll_down = Some(Vec::new());
        assert_ne!(hash(&w), absent);
    }

    #[test]
    fn source_widgets_have_no_hash() {
        assert_eq!(
            hash(&one("bar { widget \"cpu\" { source \"usage.cpu\"; } }")),
            None
        );
    }

    /// Fixed vectors: an approval recorded by one build must still match in
    /// the next. If this fails, the encoding changed; bump [`CONTEXT`] instead.
    #[test]
    fn fixed_vectors() {
        assert_eq!(
            h(LOAD).to_hex(),
            "309bbdfeb13352ed9701f886e1116d8deee724f09f25452cdd923ee6302d0717"
        );
        assert_eq!(
            h("bar { widget \"s\" { exec \"journalctl\" \"-f\"; stream #true; } }").to_hex(),
            "62a3f99e77d1f79798f5c95e03693aa1c4ac2422ce9198270f16c4d7a4c733fc"
        );
    }

    #[test]
    fn hex_round_trips_and_refuses_junk() {
        let x = h(LOAD);
        assert_eq!(WidgetHash::from_hex(&x.to_hex()), Some(x));
        for bad in ["", "00", &"g".repeat(64), &"A".repeat(64), &"0".repeat(65)] {
            assert_eq!(WidgetHash::from_hex(bad), None, "{bad}");
        }
    }

    #[test]
    fn model_command_hash_is_its_own_domain() {
        let argv: Vec<String> = ["journalctl", "-f"].iter().map(|s| (*s).to_owned()).collect();
        let m = model_command_hash(&argv);
        assert_eq!(m, model_command_hash(&["journalctl".to_owned(), "-f".to_owned()]));
        assert_ne!(m, model_command_hash(&argv[..1]));
        // The same argv as a stream widget hashes differently.
        assert_ne!(
            Some(m),
            hash(&one(
                "bar { widget \"s\" { exec \"journalctl\" \"-f\"; stream #true; } }"
            ))
        );
        // Argument boundaries count: ["a b"] is not ["a", "b"].
        assert_ne!(
            model_command_hash(&["a b".to_owned()]),
            model_command_hash(&["a".to_owned(), "b".to_owned()])
        );
    }
}
