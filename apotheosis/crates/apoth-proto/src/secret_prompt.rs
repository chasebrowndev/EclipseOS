// SPDX-License-Identifier: Apache-2.0
//! The Terminal target of `ec-secret-prompt` (T-22 §3, ADR 0053 amendment of 2026-11-15):
//! what the instance writes to `ec-secret-prompt terminal`'s stdin and how it reads the
//! answer back from its stdout. Framing only.
//!
//! Request: one JSON object of at most [`MAX_CONTEXT`] bytes, then EOF. Every string is
//! cut to its budget (T-18 §5) by [`Context::bounded`] before encoding.
//!
//! Answer: one status byte ([`ANSWERED`] or [`CANCELLED`]), then for [`ANSWERED`] the
//! secret (at most [`crate::askpass::MAX_SECRET`] bytes, no newline), then EOF. EOF with no
//! byte is a cancel.
//!
//! The context carries no secret but does carry the requester's argv, which is never
//! logged, persisted or audited: `Debug` here prints counts only.

use serde::{Deserialize, Serialize};

use crate::askpass::MAX_SECRET;

/// Largest request on the prompt's stdin.
pub const MAX_CONTEXT: usize = 64 * 1024;
/// Budget of each free-text field (titles, exe, cwd, prompt, host).
pub const MAX_TEXT: usize = 4096;
/// Most argv entries carried, and their byte budget together.
pub const MAX_ARGV: usize = 64;
/// Most pids in the ancestry chain.
pub const MAX_ANCESTRY: usize = 64;
/// Status byte: answered, the secret follows.
pub const ANSWERED: u8 = 0;
/// Status byte: cancelled.
pub const CANCELLED: u8 = 1;
/// The context's version.
pub const VERSION: u32 = 1;

/// How the request reached the instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Via {
    /// An armed askpass channel (`apoth ssh`, `sudo -A`).
    Askpass,
    /// The focused pane's PTY entered a getpass-style read (ADR 0115 predicate); the answer
    /// is typed into the pane.
    Pty,
}

/// Facts the instance read from the kernel.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verified {
    /// The window title as shown.
    pub window: String,
    /// The tab title as shown.
    pub tab: String,
    /// The pane title as shown.
    pub pane: String,
    /// The pane, `None` for a mux link with no pane.
    pub pane_id: Option<u64>,
    /// The requester's process group is the pane's foreground group (`tcgetpgrp`).
    pub in_foreground: bool,
    /// Pids from the requester up to the armed process (or the pane's child).
    pub ancestry: Vec<u32>,
    /// The requester (the program that ran the askpass client, or the pane's foreground
    /// group leader).
    pub pid: u32,
    /// `/proc/<pid>/exe`; `None` when unreadable (setuid `sudo`).
    pub exe: Option<String>,
    /// Real uid.
    pub uid: u32,
    /// Effective uid.
    pub euid: u32,
}

/// What the program says about itself.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unverified {
    /// `/proc/<pid>/cmdline`.
    pub argv: Vec<String>,
    /// `/proc/<pid>/cwd`; `None` when unreadable.
    pub cwd: Option<String>,
    /// The prompt the program printed or passed.
    pub prompt: String,
    /// The host an `apoth ssh` named (`None` for a pane).
    pub host: Option<String>,
}

/// The whole request.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub v: u32,
    pub via: Via,
    pub verified: Verified,
    pub unverified: Unverified,
}

impl std::fmt::Debug for Verified {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Verified")
            .field("pane_id", &self.pane_id)
            .field("in_foreground", &self.in_foreground)
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Unverified {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Unverified")
            .field("argc", &self.argv.len())
            .field("prompt_len", &self.prompt.len())
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("via", &self.via)
            .field("pane_id", &self.verified.pane_id)
            .field("argc", &self.unverified.argv.len())
            .field("prompt_len", &self.unverified.prompt.len())
            .finish_non_exhaustive()
    }
}

/// Cut `s` to at most `max` bytes on a character boundary.
#[must_use]
pub fn cut(mut s: String, max: usize) -> String {
    if s.len() > max {
        let mut i = max;
        while !s.is_char_boundary(i) {
            i -= 1;
        }
        s.truncate(i);
    }
    s
}

impl Context {
    /// Apply every budget.
    #[must_use]
    pub fn bounded(mut self) -> Context {
        let v = &mut self.verified;
        for s in [&mut v.window, &mut v.tab, &mut v.pane] {
            *s = cut(std::mem::take(s), MAX_TEXT);
        }
        v.exe = v.exe.take().map(|s| cut(s, MAX_TEXT));
        v.ancestry.truncate(MAX_ANCESTRY);
        let u = &mut self.unverified;
        u.prompt = cut(std::mem::take(&mut u.prompt), MAX_TEXT);
        u.cwd = u.cwd.take().map(|s| cut(s, MAX_TEXT));
        u.host = u.host.take().map(|s| cut(s, MAX_TEXT));
        u.argv.truncate(MAX_ARGV);
        let mut left = MAX_TEXT;
        let mut kept = Vec::with_capacity(u.argv.len());
        for a in std::mem::take(&mut u.argv) {
            if left == 0 {
                break;
            }
            let a = cut(a, left);
            left -= a.len();
            kept.push(a);
        }
        u.argv = kept;
        self
    }

    /// The bytes for the prompt's stdin (budgets applied).
    #[must_use]
    pub fn encode(self) -> Vec<u8> {
        serde_json::to_vec(&self.bounded()).unwrap_or_default()
    }

    /// Parse a request; `None` when it is over budget, malformed or another version.
    #[must_use]
    pub fn decode(b: &[u8]) -> Option<Context> {
        if b.len() > MAX_CONTEXT {
            return None;
        }
        serde_json::from_slice::<Context>(b)
            .ok()
            .filter(|c| c.v == VERSION)
    }
}

/// The prompt's answer as read from its stdout.
#[derive(PartialEq, Eq)]
pub enum Reply<'a> {
    Answered(&'a [u8]),
    Cancelled,
}

impl std::fmt::Debug for Reply<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Reply::Answered(s) => write!(f, "Answered({} bytes)", s.len()),
            Reply::Cancelled => f.write_str("Cancelled"),
        }
    }
}

/// Decode everything the prompt wrote. Anything malformed or over budget is a cancel.
#[must_use]
pub fn decode_reply(b: &[u8]) -> Reply<'_> {
    match b.split_first() {
        Some((&ANSWERED, rest)) if rest.len() <= MAX_SECRET => Reply::Answered(rest),
        _ => Reply::Cancelled,
    }
}

/// Fuzz body: arbitrary bytes through both decoders never panic, and a decoded context
/// stays within its budgets once bounded.
pub fn fuzz_secret_prompt(data: &[u8]) {
    let _ = decode_reply(data);
    if let Some(c) = Context::decode(data) {
        let again = c.bounded().encode();
        assert!(again.len() <= MAX_CONTEXT);
        assert!(Context::decode(&again).is_some());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Context {
        Context {
            v: VERSION,
            via: Via::Askpass,
            verified: Verified {
                window: "apotheosis".into(),
                tab: "zsh".into(),
                pane: "sudo".into(),
                pane_id: Some(3),
                in_foreground: true,
                ancestry: vec![40, 30, 20],
                pid: 40,
                exe: None,
                uid: 1000,
                euid: 0,
            },
            unverified: Unverified {
                argv: vec!["sudo".into(), "-A".into(), "pacman".into()],
                cwd: Some("/home/u".into()),
                prompt: "[sudo] password for u: ".into(),
                host: None,
            },
        }
    }

    /// Golden: the exact bytes `ec-secret-prompt` parses (its tests carry the same line).
    #[test]
    fn golden_context_bytes() {
        let want = r#"{"v":1,"via":"askpass","verified":{"window":"apotheosis","tab":"zsh","pane":"sudo","pane_id":3,"in_foreground":true,"ancestry":[40,30,20],"pid":40,"exe":null,"uid":1000,"euid":0},"unverified":{"argv":["sudo","-A","pacman"],"cwd":"/home/u","prompt":"[sudo] password for u: ","host":null}}"#;
        let got = sample().encode();
        assert_eq!(std::str::from_utf8(&got).unwrap(), want);
        assert_eq!(Context::decode(want.as_bytes()), Some(sample()));
    }

    #[test]
    fn budgets_hold_and_debug_shows_no_argv() {
        let mut c = sample();
        c.unverified.prompt = "é".repeat(MAX_TEXT);
        c.unverified.argv = vec!["x".repeat(3000); 200];
        c.verified.ancestry = vec![1; 500];
        let b = c.bounded();
        assert!(b.unverified.prompt.len() <= MAX_TEXT);
        assert!(b.unverified.argv.len() <= MAX_ARGV);
        assert!(b.unverified.argv.iter().map(String::len).sum::<usize>() <= MAX_TEXT);
        assert_eq!(b.verified.ancestry.len(), MAX_ANCESTRY);
        assert!(b.clone().encode().len() <= MAX_CONTEXT);
        let d = format!("{b:?}");
        assert!(!d.contains("xxx") && !d.contains("pacman"), "{d}");
    }

    #[test]
    fn decode_refuses_other_versions_unknown_fields_and_oversize() {
        let mut c = sample();
        c.v = 2;
        assert!(Context::decode(&serde_json::to_vec(&c).unwrap()).is_none());
        let extra = r#"{"v":1,"via":"pty","verified":{},"unverified":{},"x":1}"#;
        assert!(Context::decode(extra.as_bytes()).is_none());
        assert!(Context::decode(&vec![b' '; MAX_CONTEXT + 1]).is_none());
    }

    #[test]
    fn replies() {
        assert_eq!(decode_reply(b"\0hunter2"), Reply::Answered(b"hunter2"));
        assert_eq!(decode_reply(b"\0"), Reply::Answered(b""));
        assert_eq!(decode_reply(b"\x01"), Reply::Cancelled);
        assert_eq!(decode_reply(b""), Reply::Cancelled);
        assert_eq!(decode_reply(b"\x07x"), Reply::Cancelled);
        let mut over = vec![0u8];
        over.extend(std::iter::repeat_n(b'x', MAX_SECRET + 1));
        assert_eq!(decode_reply(&over), Reply::Cancelled);
        assert_eq!(
            format!("{:?}", decode_reply(b"\0hunter2")),
            "Answered(7 bytes)"
        );
    }

    #[test]
    fn fuzz_body_survives_its_seeds() {
        fuzz_secret_prompt(&sample().encode());
        fuzz_secret_prompt(b"{");
        fuzz_secret_prompt(&[0xff; 300]);
    }
}
