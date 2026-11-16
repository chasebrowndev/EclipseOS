// SPDX-License-Identifier: AGPL-3.0-only
//! Deterministic corpora (T-23 §1): fixed seeds, identified by hash, never checked in.

/// The recorded-stream shapes of M17.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corpus {
    /// `cat` of a large text file: printable ASCII lines (`THR-ASCII`).
    CatLarge,
    /// `ls -R` listing: short path lines and columns (`THR-ASCII` family).
    LsR,
    /// A pager/editor scrolling in the alternate screen with a status line (`THR-CSI`).
    VimScroll,
    /// Cursor-motion and erase heavy TUI redraw (`THR-CSI`).
    CsiRedraw,
    /// Dense SGR + Unicode + wide cells (`THR-DENSE`).
    Dense,
    /// Unicode text, no styling: accents, Cyrillic, CJK, emoji, combining marks.
    Unicode,
}

impl Corpus {
    /// All corpora.
    pub const ALL: [Corpus; 6] = [
        Corpus::CatLarge,
        Corpus::LsR,
        Corpus::VimScroll,
        Corpus::CsiRedraw,
        Corpus::Dense,
        Corpus::Unicode,
    ];

    /// Stable name (CLI and result ids).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Corpus::CatLarge => "cat-large",
            Corpus::LsR => "ls-r",
            Corpus::VimScroll => "vim-scroll",
            Corpus::CsiRedraw => "csi-redraw",
            Corpus::Dense => "dense",
            Corpus::Unicode => "unicode",
        }
    }

    /// Parse a stable name.
    #[must_use]
    pub fn from_name(s: &str) -> Option<Corpus> {
        Corpus::ALL.into_iter().find(|c| c.name() == s)
    }

    fn seed(self) -> u64 {
        0x9E37_79B9_7F4A_7C15 ^ (self as u64 + 1).wrapping_mul(0x1000_0000_01B3)
    }
}

/// xorshift64*: tiny, deterministic, platform independent.
#[derive(Debug)]
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn pick<'a, T>(&mut self, s: &'a [T]) -> &'a T {
        &s[self.below(s.len() as u64) as usize]
    }
}

const WORDS: &[&str] = &[
    "the", "quick", "terminal", "buffer", "render", "of", "scroll", "grid", "parser", "cell",
    "frame", "input", "latency", "glass", "eclipse", "line", "wrap", "cursor", "history", "page",
];
const EXTS: &[&str] = &["rs", "toml", "md", "kdl", "txt", "so", "o"];
const UNI: &[&str] = &[
    "caf\u{e9}",
    "na\u{ef}ve",
    "\u{43f}\u{440}\u{438}\u{432}\u{435}\u{442}",
    "\u{4e16}\u{754c}",
    "\u{65e5}\u{672c}\u{8a9e}",
    "\u{1f600}",
    "\u{1f468}\u{200d}\u{1f469}",
    "e\u{301}",
    "\u{3b1}\u{3b2}\u{3b3}",
    "\u{d55c}\u{ae00}",
];

fn words(r: &mut Rng, out: &mut Vec<u8>, set: &[&str], n: u64) {
    for i in 0..n {
        if i > 0 {
            out.push(b' ');
        }
        out.extend_from_slice(r.pick(set).as_bytes());
    }
}

fn record(c: Corpus, r: &mut Rng, out: &mut Vec<u8>, n: u64) {
    match c {
        Corpus::CatLarge => {
            let n = 3 + r.below(14);
            words(r, out, WORDS, n);
            out.extend_from_slice(b"\r\n");
        }
        Corpus::LsR => {
            if n.is_multiple_of(40) {
                out.extend_from_slice(
                    format!("\r\n./d{}/s{}:\r\n", r.below(50), r.below(9)).as_bytes(),
                );
            }
            for _ in 0..1 + r.below(4) {
                out.extend_from_slice(
                    format!("file{:04}.{}  ", r.below(10_000), r.pick(EXTS)).as_bytes(),
                );
            }
            out.extend_from_slice(b"\r\n");
        }
        Corpus::VimScroll => {
            if n == 0 {
                out.extend_from_slice(b"\x1b[?1049h\x1b[2J\x1b[1;23r");
            }
            out.extend_from_slice(b"\x1b[23;1H\n\x1b[23;1H");
            let n = 2 + r.below(10);
            words(r, out, WORDS, n);
            out.extend_from_slice(b"\x1b[K\x1b[24;1H\x1b[7m");
            out.extend_from_slice(format!("file.rs [+] {}:{}", n, r.below(80)).as_bytes());
            out.extend_from_slice(b"\x1b[m\x1b[K");
        }
        Corpus::CsiRedraw => {
            let (row, col) = (1 + r.below(40), 1 + r.below(120));
            out.extend_from_slice(format!("\x1b[{row};{col}H").as_bytes());
            let n = 1 + r.below(3);
            words(r, out, WORDS, n);
            out.extend_from_slice(
                format!("\x1b[{}C\x1b[{}A\x1b[K", 1 + r.below(9), 1 + r.below(5)).as_bytes(),
            );
            if r.below(16) == 0 {
                out.extend_from_slice(b"\x1b[J");
            }
            out.extend_from_slice(
                format!("\x1b[{}B\x1b[{}D*", 1 + r.below(5), 1 + r.below(9)).as_bytes(),
            );
        }
        Corpus::Dense => {
            out.extend_from_slice(
                format!(
                    "\x1b[38;2;{};{};{}m",
                    r.below(256),
                    r.below(256),
                    r.below(256)
                )
                .as_bytes(),
            );
            out.extend_from_slice(format!("\x1b[48;5;{}m", r.below(256)).as_bytes());
            if r.below(3) == 0 {
                out.extend_from_slice(b"\x1b[1;4m");
            }
            for _ in 0..1 + r.below(6) {
                if r.below(2) == 0 {
                    out.extend_from_slice(r.pick(UNI).as_bytes());
                } else {
                    out.extend_from_slice(r.pick(WORDS).as_bytes());
                }
                out.push(b' ');
            }
            out.extend_from_slice(b"\x1b[0m");
            if r.below(4) == 0 {
                out.extend_from_slice(b"\r\n");
            }
        }
        Corpus::Unicode => {
            let n = 3 + r.below(10);
            words(r, out, UNI, n);
            out.extend_from_slice(b"\r\n");
        }
    }
}

/// Generate at least `min_bytes` of the corpus (whole records only). Same arguments, same
/// bytes, on every machine.
#[must_use]
pub fn generate(c: Corpus, min_bytes: usize) -> Vec<u8> {
    let mut r = Rng(c.seed());
    let mut out = Vec::with_capacity(min_bytes + 256);
    let mut n = 0u64;
    while out.len() < min_bytes {
        record(c, &mut r, &mut out, n);
        n += 1;
    }
    if c == Corpus::VimScroll {
        out.extend_from_slice(b"\x1b[?1049l");
    }
    out
}

/// `n` plain 80-column-ish text lines (`\r\n` terminated, exactly `cols` printable cells
/// each, so they fill a line and wrap-free reflow has real content).
#[must_use]
pub fn lines(n: usize, cols: usize) -> Vec<u8> {
    let mut r = Rng(0xA5A5_5A5A_1234_5678);
    let mut out = Vec::with_capacity(n * (cols + 2));
    for _ in 0..n {
        let start = out.len();
        while out.len() - start < cols {
            out.extend_from_slice(r.pick(WORDS).as_bytes());
            out.push(b' ');
        }
        out.truncate(start + cols);
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// FNV-1a 64-bit hash: how corpora are pinned (T-23 §1).
#[must_use]
pub fn hash(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}
