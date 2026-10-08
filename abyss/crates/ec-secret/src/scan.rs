// SPDX-License-Identifier: AGPL-3.0-only
//! Reading `claude setup-token`'s terminal output (pure, no I/O).
//!
//! `claude` draws a TUI, so the bytes are text interleaved with escapes:
//! words are placed with cursor-position escapes (`ESC [ <n> G`) instead of
//! spaces, and the authorize URL is wrapped in an OSC 8 hyperlink and also
//! printed. [`Scanner`] strips the escapes incrementally (a sequence may be
//! split across reads), treats a cursor-position escape as a space, and
//! reports three things once each: the URL, the `Paste code here` prompt (only
//! after the URL), and the `sk-ant-oat01-` token.
//!
//! The token passes through here, so the stripped text lives in a
//! `Zeroizing` buffer and [`Event::Token`] has a redacted `Debug`.

use zeroize::Zeroizing;

/// What the OAuth token starts with.
pub const TOKEN_PREFIX: &[u8] = b"sk-ant-oat01-";
/// A token tail shorter than this is not a token (something quoted the prefix).
const MIN_TAIL: usize = 16;
/// Bounds on what is kept: output is a few KiB in practice.
const MAX_TEXT: usize = 64 * 1024;
const KEEP_TEXT: usize = 32 * 1024;
const MAX_OSC: usize = 4096;

pub enum Event {
    Url(String),
    PromptReady,
    Token(Zeroizing<String>),
}

impl std::fmt::Debug for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Event::Url(u) => write!(f, "Url({u})"),
            Event::PromptReady => f.write_str("PromptReady"),
            Event::Token(_) => f.write_str("Token(<redacted>)"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum St {
    Ground,
    Escape,
    /// ESC followed by one of `( ) * + #`: swallow the next byte.
    Skip,
    Csi,
    Osc,
    OscEscape,
    /// DCS / SOS / PM / APC: dropped up to ST or BEL.
    Str,
    StrEscape,
}

pub struct Scanner {
    esc: St,
    /// CSI parameters (kept only so the final byte is judged), or OSC body.
    param: Vec<u8>,
    text: Zeroizing<Vec<u8>>,
    /// The first `https://` OSC 8 target, and whether its hyperlink closed.
    link: Option<String>,
    link_closed: bool,
    url_done: bool,
    prompt_done: bool,
    token_done: bool,
}

impl Default for Scanner {
    fn default() -> Self {
        Scanner::new()
    }
}

impl Scanner {
    pub fn new() -> Scanner {
        Scanner {
            esc: St::Ground,
            param: Vec::new(),
            text: Zeroizing::new(Vec::new()),
            link: None,
            link_closed: false,
            url_done: false,
            prompt_done: false,
            token_done: false,
        }
    }

    /// More output. Events come back in the order URL, prompt, token.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Event> {
        for &b in chunk {
            self.step(b);
        }
        self.scan(false)
    }

    /// The stream ended: a token at the very end is complete.
    pub fn finish(&mut self) -> Vec<Event> {
        self.scan(true)
    }

    fn step(&mut self, b: u8) {
        match self.esc {
            St::Ground => match b {
                0x1b => self.esc = St::Escape,
                b'\r' | b'\n' => self.text.push(b'\n'),
                b'\t' => self.text.push(b' '),
                0x00..=0x1f | 0x7f => {}
                _ => self.text.push(b),
            },
            St::Escape => {
                self.param.clear();
                self.esc = match b {
                    b'[' => St::Csi,
                    b']' => St::Osc,
                    b'P' | b'X' | b'^' | b'_' => St::Str,
                    b'(' | b')' | b'*' | b'+' | b'#' => St::Skip,
                    0x1b => St::Escape,
                    _ => St::Ground,
                };
            }
            St::Skip => self.esc = St::Ground,
            St::Csi => match b {
                0x1b => self.esc = St::Escape,
                // Cursor-position (G) and cursor-forward (C) put the next
                // word somewhere to the right: a space for matching.
                b'G' | b'C' => {
                    self.text.push(b' ');
                    self.esc = St::Ground;
                }
                0x40..=0x7e => self.esc = St::Ground,
                0x20..=0x3f if self.param.len() < MAX_OSC => self.param.push(b),
                _ => {}
            },
            St::Osc => match b {
                0x07 => self.end_osc(),
                0x1b => self.esc = St::OscEscape,
                _ if self.param.len() < MAX_OSC => self.param.push(b),
                _ => {}
            },
            St::OscEscape => {
                self.end_osc();
                if b != b'\\' {
                    // Not ST: the ESC began a new sequence.
                    self.esc = St::Escape;
                    self.step(b);
                }
            }
            St::Str => match b {
                0x07 => self.esc = St::Ground,
                0x1b => self.esc = St::StrEscape,
                _ => {}
            },
            St::StrEscape => {
                self.esc = if b == b'\\' { St::Ground } else { St::Escape };
                if self.esc == St::Escape {
                    self.step(b);
                }
            }
        }
    }

    /// `ESC ] 8 ; params ; URI` opens a hyperlink, an empty URI closes it.
    fn end_osc(&mut self) {
        self.esc = St::Ground;
        let body = std::mem::take(&mut self.param);
        let Some(rest) = body.strip_prefix(b"8;") else {
            return;
        };
        let Some(i) = rest.iter().position(|&c| c == b';') else {
            return;
        };
        let uri = &rest[i + 1..];
        if uri.is_empty() {
            if self.link.is_some() {
                self.link_closed = true;
            }
        } else if self.link.is_none()
            && uri.starts_with(b"https://")
            && uri.iter().all(|c| c.is_ascii_graphic())
        {
            self.link = Some(String::from_utf8_lossy(uri).into_owned());
        }
    }

    fn scan(&mut self, eof: bool) -> Vec<Event> {
        let mut out = Vec::new();
        if !self.url_done {
            if let Some(u) = self.find_url() {
                self.url_done = true;
                out.push(Event::Url(u));
            }
        }
        if self.url_done && !self.prompt_done && has_words(&self.text, &["Paste", "code", "here"]) {
            self.prompt_done = true;
            out.push(Event::PromptReady);
        }
        if !self.token_done {
            if let Some(t) = find_token(&self.text, eof) {
                self.token_done = true;
                out.push(Event::Token(t));
            }
        }
        if self.text.len() > MAX_TEXT {
            let cut = self.text.len() - KEEP_TEXT;
            self.text.drain(..cut);
        }
        out
    }

    fn find_url(&self) -> Option<String> {
        if let Some(l) = &self.link {
            return self.link_closed.then(|| l.clone());
        }
        // No hyperlink: the printed URL, complete once whitespace follows it.
        let t: &[u8] = &self.text;
        let at = find(t, b"https://")?;
        let len = t[at..].iter().take_while(|c| c.is_ascii_graphic()).count();
        (at + len < t.len()).then(|| String::from_utf8_lossy(&t[at..at + len]).into_owned())
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The words in order, separated by any run of whitespace.
fn has_words(text: &[u8], words: &[&str]) -> bool {
    let mut from = 0;
    while let Some(i) = find(&text[from..], words[0].as_bytes()).map(|i| i + from) {
        from = i + 1;
        let mut p = i + words[0].len();
        let mut ok = true;
        for w in &words[1..] {
            let ws = text[p..].iter().take_while(|c| c.is_ascii_whitespace()).count();
            if ws == 0 || !text[p + ws..].starts_with(w.as_bytes()) {
                ok = false;
                break;
            }
            p += ws + w.len();
        }
        if ok {
            return true;
        }
    }
    false
}

fn is_token_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
}

/// The first complete token: the prefix plus `[A-Za-z0-9_-]+`, ended by any
/// other byte (or by `eof`).
fn find_token(text: &[u8], eof: bool) -> Option<Zeroizing<String>> {
    let mut from = 0;
    while let Some(i) = find(&text[from..], TOKEN_PREFIX).map(|i| i + from) {
        let start = i + TOKEN_PREFIX.len();
        let tail = text[start..].iter().take_while(|&&c| is_token_char(c)).count();
        let end = start + tail;
        if end == text.len() && !eof {
            return None;
        }
        if tail >= MIN_TAIL {
            return Some(Zeroizing::new(
                String::from_utf8_lossy(&text[i..end]).into_owned(),
            ));
        }
        from = end.max(i + 1);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://claude.com/cai/oauth/authorize?code=true&client_id=abc-123&state=x_y-z";
    const TOKEN: &str = "sk-ant-oat01-AbC_def-0123456789AbC_def-0123456789xyz";

    /// The URL line and the prompt as `claude setup-token` draws them.
    fn real() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"\x1b[?25l\x1b[2J\x1b[H\x1b[1mWelcome to Claude Code\x1b[22m\r\n");
        v.extend_from_slice(
            format!("\x1b]8;id=p3gklx;{URL}\x07\x1b[38;5;246m{URL}\x1b[39m\x1b]8;;\x07\r\r\n").as_bytes(),
        );
        v.extend_from_slice(b"\x1b[2GPaste\x1b[8Gcode\x1b[13Ghere\x1b[18Gif\x1b[21Gprompted\x1b[30G>");
        v
    }

    fn token_screen() -> Vec<u8> {
        format!("\r\n\x1b[2K\x1b[2GYour\x1b[7Goauth\x1b[13Gtoken\x1b[19G(valid\x1b[26Gfor\x1b[30G1\x1b[32Gyear):\r\n\x1b[1m{TOKEN}\x1b[22m\r\n\x1b[2GStore\x1b[8Gthis").into_bytes()
    }

    fn run(chunks: &[&[u8]]) -> Vec<String> {
        let mut s = Scanner::new();
        let mut out = Vec::new();
        for c in chunks {
            for e in s.feed(c) {
                out.push(format!("{e:?}"));
            }
        }
        for e in s.finish() {
            out.push(format!("{e:?}"));
        }
        out
    }

    #[test]
    fn finds_the_url_and_the_prompt() {
        let r = run(&[&real()]);
        assert_eq!(r, [format!("Url({URL})"), "PromptReady".to_string()]);
    }

    #[test]
    fn every_split_point_gives_the_same_events() {
        let mut all = real();
        all.extend_from_slice(&token_screen());
        let want = run(&[&all]);
        assert_eq!(want.len(), 3, "{want:?}");
        for i in 0..all.len() {
            assert_eq!(run(&[&all[..i], &all[i..]]), want, "split at {i}");
        }
        // And one byte at a time.
        let bytes: Vec<&[u8]> = all.chunks(1).collect();
        assert_eq!(run(&bytes), want);
    }

    #[test]
    fn a_printed_url_without_a_hyperlink_is_found_when_a_newline_follows() {
        let plain = format!("visit {URL}");
        assert!(run(&[plain.as_bytes()]).is_empty(), "not complete yet");
        let r = run(&[plain.as_bytes(), b"\r\n"]);
        assert_eq!(r, [format!("Url({URL})")]);
    }

    #[test]
    fn a_hyperlink_that_never_closes_is_not_reported() {
        let open = format!("\x1b]8;;{URL}\x07text");
        assert!(run(&[open.as_bytes()]).is_empty());
    }

    #[test]
    fn the_prompt_waits_for_the_url() {
        let r = run(&[b"\x1b[2GPaste\x1b[8Gcode\x1b[13Ghere"]);
        assert!(r.is_empty());
        let mut v = b"\x1b[2GPaste\x1b[8Gcode\x1b[13Ghere".to_vec();
        v.extend_from_slice(format!("\x1b]8;;{URL}\x07{URL}\x1b]8;;\x07\r\n").as_bytes());
        assert_eq!(run(&[&v]), [format!("Url({URL})"), "PromptReady".to_string()]);
    }

    #[test]
    fn the_token_ends_at_any_other_byte_and_never_leaks_into_debug() {
        let r = run(&[&token_screen()]);
        assert_eq!(r, ["Token(<redacted>)"]);
        let mut s = Scanner::new();
        let ev = s.feed(&token_screen());
        let Some(Event::Token(t)) = ev.into_iter().next() else {
            panic!("no token")
        };
        assert_eq!(t.as_str(), TOKEN);
    }

    #[test]
    fn a_token_at_the_end_of_the_stream_needs_finish() {
        let mut s = Scanner::new();
        assert!(s.feed(format!("tok {TOKEN}").as_bytes()).is_empty());
        let Some(Event::Token(t)) = s.finish().into_iter().next() else {
            panic!("no token")
        };
        assert_eq!(t.as_str(), TOKEN);
    }

    #[test]
    fn a_quoted_prefix_is_not_a_token() {
        assert!(run(&[b"tokens start with sk-ant-oat01-... ok\r\n"]).is_empty());
    }

    #[test]
    fn escapes_strip_to_plain_words() {
        let mut s = Scanner::new();
        s.feed(b"\x1b[2GPaste\x1b[8Gcode\x1b[9G\x1b(B\x1b[1;31mhere\x1bP1$r\x1b\\\x1b[0m!");
        assert_eq!(&**s.text, b" Paste code here!");
    }
}
