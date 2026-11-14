// SPDX-License-Identifier: AGPL-3.0-only
//! Main-thread navigation and consent state (T-14 §5-§8, T-18 §3-§4): the search bar's
//! query and progress, hint and copy-mode ownership of the keyboard, the link tooltip, the
//! pending consent question and OSC 52 bookkeeping. The grid side lives in `view.rs` on the
//! render thread; the two talk through `ViewCmd` and `NavMsg`.
//!
//! The query, link and clipboard text are user content: nothing here logs them.

use std::hash::{Hash, Hasher};

use crate::pane::PaneKey;
use crate::search::{Case, Query, Status};
use crate::security::{Class, Consent};

/// Typing in the search bar waits this long before the search restarts (T-14 §6).
pub(crate) const DEBOUNCE_MS: u64 = 150;

/// The search bar.
#[derive(Debug)]
pub(crate) struct SearchUi {
    pub(crate) pane: PaneKey,
    pub(crate) query: Query,
    pub(crate) status: Status,
    /// The bar has the keyboard (typing edits the query).
    pub(crate) focused: bool,
    /// Bumped on every edit; a debounce timer only fires for the latest.
    pub(crate) generation: u64,
}

/// An `ask` decision waiting on the consent bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingConsent {
    pub(crate) pane: PaneKey,
    pub(crate) class: Class,
    /// OSC 52 selection letters of the read.
    pub(crate) selection: String,
    /// The term's token for a `graphics-file` question (0 for the clipboard).
    pub(crate) token: u64,
}

/// A link waiting for the user to confirm its full URI (it differs from the visible text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingOpen {
    pub(crate) uri: String,
}

#[derive(Debug, Default)]
pub(crate) struct Nav {
    pub(crate) search: Option<SearchUi>,
    /// Hint mode owns the keyboard for this pane.
    pub(crate) hints: Option<PaneKey>,
    /// Copy mode is on in this pane.
    pub(crate) copy: Option<PaneKey>,
    /// `cursor.jump-char` with no character: the next typed key names it.
    pub(crate) await_jump: Option<(bool, bool)>,
    /// The last jump (char, backward, till), for `cursor.jump-repeat`.
    pub(crate) last_jump: Option<(char, bool, bool)>,
    /// The hovered link's URI (tooltip).
    pub(crate) link_tip: Option<String>,
    /// The cell the pointer was last reported over (hover is sent only when it changes).
    pub(crate) hover_cell: Option<(PaneKey, Option<(usize, usize)>)>,
    pub(crate) consent: Option<PendingConsent>,
    pub(crate) consents: Consent,
    pub(crate) open: Option<PendingOpen>,
    /// Clipboard reads answering an OSC 52 query: transfer id, pane, selection letters.
    pub(crate) osc52_reads: Vec<(u64, PaneKey, String)>,
    /// Hash of the content an OSC 52 write last put on the clipboard, and whether the
    /// writer was a remote pane (T-18 §4 (e)).
    pub(crate) osc52_tag: Option<(u64, bool)>,
}

impl Nav {
    /// Search bar text: the query plus count, position and mode flags.
    pub(crate) fn search_line(s: &SearchUi) -> String {
        let mut flags = String::new();
        match s.query.case {
            Case::Smart => {}
            Case::Sensitive => flags.push_str(" [Aa]"),
            Case::Insensitive => flags.push_str(" [aa]"),
        }
        if s.query.regex {
            flags.push_str(" [.*]");
        }
        if s.query.whole_word {
            flags.push_str(" [w]");
        }
        if s.query.scope == crate::search::Scope::Screen {
            flags.push_str(" [screen]");
        }
        let count = if let Some(e) = &s.status.error {
            e.clone()
        } else if s.query.text.is_empty() {
            String::new()
        } else {
            let n = if s.status.capped {
                format!("{}+", s.status.count)
            } else {
                s.status.count.to_string()
            };
            let more = if s.status.done { "" } else { "…" };
            match s.status.current {
                Some(i) => format!("{i}/{n}{more}"),
                None if s.status.count == 0 && s.status.done => "No matches".to_string(),
                None => format!("{n}{more}"),
            }
        };
        format!("Find: {}{flags}   {count}", s.query.text)
    }
}

/// Hash of clipboard content (to recognise OSC 52 content at paste time; never stored as
/// text).
pub(crate) fn content_hash(b: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    b.hash(&mut h);
    h.finish()
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding.
pub(crate) fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// The OSC 52 answer to a permitted read: `ESC ] 52 ; <sel> ; <base64> ESC \`. The
/// selection letters are the term's filtered set (closed alphabet); content over `max`
/// bytes is answered empty rather than truncated.
pub(crate) fn osc52_reply(selection: &str, data: &[u8], max: usize) -> Vec<u8> {
    let sel: String = selection
        .chars()
        .filter(|c| matches!(c, 'c' | 'p' | 'q' | 's' | '0'..='7'))
        .take(8)
        .collect();
    let body = if data.len() > max {
        String::new()
    } else {
        base64(data)
    };
    format!("\x1b]52;{sel};{body}\x1b\\").into_bytes()
}

/// A search-bar edit. Returns true when the query text changed.
pub(crate) fn edit_query(q: &mut String, key: apoth_input::Key, text: Option<&str>) -> bool {
    use apoth_input::{Key, Named};
    match key {
        Key::Named(Named::Backspace) => q.pop().is_some(),
        _ => match text {
            Some(t) if !t.is_empty() && !t.chars().any(char::is_control) => {
                if q.len() + t.len() > crate::search::MAX_QUERY {
                    return false;
                }
                q.push_str(t);
                true
            }
            _ => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648_vectors() {
        for (i, o) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(i.as_bytes()), o);
        }
    }

    #[test]
    fn osc52_reply_is_closed_grammar() {
        assert_eq!(osc52_reply("c", b"hi", 10), b"\x1b]52;c;aGk=\x1b\\");
        // Hostile selection text cannot reach the reply.
        // Only the selection alphabet survives (`c` and `0` here).
        assert_eq!(osc52_reply("c;\x1b]0;x", b"", 10), b"\x1b]52;c0;\x1b\\");
        // Over the cap: answered empty, never truncated.
        assert_eq!(osc52_reply("p", b"0123456789A", 10), b"\x1b]52;p;\x1b\\");
        // Binary content is base64: no ESC or BEL can appear in the body.
        let r = osc52_reply("c", b"\x1b\x07\x9c", 10);
        assert!(!r[2..r.len() - 2].contains(&0x1b) && !r.contains(&0x07));
    }

    #[test]
    fn search_line_shows_progress_and_errors() {
        let mut s = SearchUi {
            pane: 1,
            query: Query {
                text: "foo".into(),
                ..Query::default()
            },
            status: Status {
                count: 3,
                current: Some(2),
                done: true,
                ..Status::default()
            },
            focused: true,
            generation: 0,
        };
        assert!(Nav::search_line(&s).ends_with("2/3"));
        s.status.done = false;
        assert!(Nav::search_line(&s).ends_with("2/3…"));
        s.status = Status {
            done: true,
            ..Status::default()
        };
        assert!(Nav::search_line(&s).ends_with("No matches"));
        s.status.error = Some("Pattern too expensive".into());
        assert!(Nav::search_line(&s).ends_with("Pattern too expensive"));
        s.query.regex = true;
        assert!(Nav::search_line(&s).contains("[.*]"));
        s.query.scope = crate::search::Scope::Screen;
        assert!(Nav::search_line(&s).contains("[screen]"));
    }

    #[test]
    fn query_edits_refuse_controls_and_cap_length() {
        use apoth_input::{Key, Named};
        let mut q = String::new();
        assert!(edit_query(&mut q, Key::Char('a'), Some("a")));
        assert!(!edit_query(&mut q, Key::Char('x'), Some("\x1b")));
        assert!(edit_query(&mut q, Key::Named(Named::Backspace), None));
        assert!(q.is_empty());
        let big = "x".repeat(crate::search::MAX_QUERY + 1);
        assert!(!edit_query(&mut q, Key::Char('x'), Some(&big)));
    }
}
