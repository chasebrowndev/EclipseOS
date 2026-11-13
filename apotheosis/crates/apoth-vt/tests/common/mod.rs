// SPDX-License-Identifier: Apache-2.0
#![allow(dead_code, unreachable_pub)]
//! Shared test sink: records events, normalises chunk-dependent edges, renders text.

use apoth_vt::{Params, Parser, Sink, StrEnd, StrHead, StrKind, StrRoute};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ev {
    Print(String),
    Exec(u8),
    Esc(Vec<u8>, u8),
    Csi {
        params: Vec<Option<u32>>,
        colon: u64,
        overflow: bool,
        inter: Vec<u8>,
        private: u8,
        fin: u8,
    },
    Start(StrKind, String),
    Put(Vec<u8>),
    End(StrEnd),
}

#[derive(Clone, Debug)]
pub struct Rec {
    pub events: Vec<Ev>,
    pub cap_osc: u32,
    pub cap_dcs: u32,
    pub cap_apc: u32,
}

impl Default for Rec {
    fn default() -> Self {
        Self::with_caps(1024, 1024, 1024)
    }
}

impl Rec {
    pub fn with_caps(cap_osc: u32, cap_dcs: u32, cap_apc: u32) -> Self {
        Self {
            events: Vec::new(),
            cap_osc,
            cap_dcs,
            cap_apc,
        }
    }
}

pub fn render_params(p: &Params) -> String {
    let mut s = String::new();
    for i in 0..p.len() {
        if let Some(v) = p.get(i) {
            s.push_str(&v.to_string());
        }
        if i + 1 < p.len() {
            s.push(if p.colon_after(i) { ':' } else { ';' });
        }
    }
    if p.overflow() {
        s.push('!');
    }
    s
}

fn render_head(kind: StrKind, h: &StrHead) -> String {
    let mut s = String::new();
    match kind {
        StrKind::Osc => {
            s.push_str(&String::from_utf8_lossy(h.osc_command_digits()));
            s.push('/');
            s.push_str(&String::from_utf8_lossy(h.osc_selector()));
        }
        StrKind::Dcs => {
            if h.dcs_private() != 0 {
                s.push(h.dcs_private() as char);
            }
            s.push('[');
            s.push_str(&render_params(h.params()));
            s.push(']');
            s.push_str(&String::from_utf8_lossy(h.dcs_intermediates()));
            if let Some(f) = h.dcs_final() {
                s.push(f as char);
            }
        }
        StrKind::Apc => s.push_str(&String::from_utf8_lossy(h.bytes())),
        StrKind::Pm | StrKind::Sos => {}
    }
    if !h.complete() {
        s.push('~');
    }
    if h.truncated() {
        s.push('T');
    }
    s
}

impl Sink for Rec {
    fn print_run(&mut self, text: &str) {
        assert!(!text.is_empty());
        assert!(
            text.chars().all(|c| (c as u32) >= 0x20 && c as u32 != 0x7F),
            "print_run with C0/DEL: {text:?}"
        );
        self.events.push(Ev::Print(text.to_string()));
    }
    fn execute(&mut self, c0: u8) {
        assert!(c0 < 0x20, "execute({c0:#x})");
        self.events.push(Ev::Exec(c0));
    }
    fn esc(&mut self, inter: &[u8], fin: u8) {
        self.events.push(Ev::Esc(inter.to_vec(), fin));
    }
    fn csi(&mut self, p: &Params, inter: &[u8], private: u8, fin: u8) {
        self.events.push(Ev::Csi {
            params: p.iter().collect(),
            colon: p.colon_mask(),
            overflow: p.overflow(),
            inter: inter.to_vec(),
            private,
            fin,
        });
    }
    fn string_start(&mut self, kind: StrKind, head: &StrHead) -> StrRoute {
        self.events.push(Ev::Start(kind, render_head(kind, head)));
        StrRoute {
            cap: match kind {
                StrKind::Osc => self.cap_osc,
                StrKind::Dcs => self.cap_dcs,
                StrKind::Apc => self.cap_apc,
                StrKind::Pm | StrKind::Sos => 0,
            },
        }
    }
    fn string_put(&mut self, chunk: &[u8]) {
        assert!(!chunk.is_empty());
        self.events.push(Ev::Put(chunk.to_vec()));
    }
    fn string_end(&mut self, end: StrEnd) {
        self.events.push(Ev::End(end));
    }
}

/// Coalesce adjacent prints and adjacent puts (the only chunk-dependent edges).
pub fn normalise(events: &[Ev]) -> Vec<Ev> {
    let mut out: Vec<Ev> = Vec::new();
    for e in events {
        match (out.last_mut(), e) {
            (Some(Ev::Print(a)), Ev::Print(b)) => a.push_str(b),
            (Some(Ev::Put(a)), Ev::Put(b)) => a.extend_from_slice(b),
            _ => out.push(e.clone()),
        }
    }
    out
}

pub fn run_with(rec: Rec, chunks: &[&[u8]]) -> (Vec<Ev>, Parser) {
    let mut p = Parser::new();
    let mut rec = rec;
    for c in chunks {
        p.advance(c, &mut rec);
    }
    (normalise(&rec.events), p)
}

pub fn run(bytes: &[u8]) -> Vec<Ev> {
    run_with(Rec::default(), &[bytes]).0
}

pub fn render(events: &[Ev]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for e in events {
        parts.push(match e {
            Ev::Print(s) => format!("P({s:?})"),
            Ev::Exec(c) => format!("X({c:02x})"),
            Ev::Esc(i, f) => format!("E({}{})", String::from_utf8_lossy(i), *f as char),
            Ev::Csi {
                params,
                colon,
                overflow,
                inter,
                private,
                fin,
            } => {
                let mut s = String::new();
                if *private != 0 {
                    s.push(*private as char);
                }
                for (i, p) in params.iter().enumerate() {
                    if let Some(v) = p {
                        s.push_str(&v.to_string());
                    }
                    if i + 1 < params.len() {
                        s.push(if colon >> i & 1 == 1 { ':' } else { ';' });
                    }
                }
                if *overflow {
                    s.push('!');
                }
                s.push_str(&String::from_utf8_lossy(inter));
                s.push(*fin as char);
                format!("C({s})")
            }
            Ev::Start(k, h) => {
                let k = match k {
                    StrKind::Osc => "osc",
                    StrKind::Dcs => "dcs",
                    StrKind::Apc => "apc",
                    StrKind::Pm => "pm",
                    StrKind::Sos => "sos",
                };
                if h.is_empty() {
                    format!("S({k})")
                } else {
                    format!("S({k}:{h})")
                }
            }
            Ev::Put(b) => format!("U({:?})", String::from_utf8_lossy(b)),
            Ev::End(e) => format!(
                "N({})",
                match e {
                    StrEnd::St => "st",
                    StrEnd::Bel => "bel",
                    StrEnd::Aborted => "abort",
                    StrEnd::OverCap => "cap",
                    StrEnd::Rejected => "rej",
                }
            ),
        });
    }
    parts.join(" ")
}

/// Tiny deterministic PRNG for the property tests.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const FRAGMENTS: &[&[u8]] = &[
    b"\x1b[",
    b"\x1b]",
    b"\x1bP",
    b"\x1b_",
    b"\x1b^",
    b"\x1bX",
    b"\x1b\\",
    b"\x1b",
    b"\x07",
    b"\x18",
    b"\x1a",
    b"\x7f",
    b"\n",
    b"\r",
    b"\t",
    b"\x08",
    b";",
    b":",
    b"?",
    b">",
    b"<",
    b"=",
    b"0",
    b"1",
    b"38",
    b"2",
    b"9999999999",
    b"m",
    b"H",
    b"h",
    b"q",
    b"$",
    b" ",
    b"!",
    b"\"",
    b"#",
    b"abc",
    b"hello world",
    "héllo".as_bytes(),
    "世界".as_bytes(),
    "😀".as_bytes(),
    b"\xc3",
    b"\xe2\x82",
    b"\xf0\x9f",
    b"\xff",
    b"\x80",
    b"\xed\xa0",
    b"0;title",
    b"8;;http://x",
    b"1337;File=",
    b"52;c;",
    b"Gi=1,a=T;",
    b"1$q",
];

/// Structure-aware stream: glued fragments with occasional raw bytes.
pub fn gen_stream(rng: &mut Rng, max_frags: usize) -> Vec<u8> {
    let n = 1 + rng.below(max_frags);
    let mut out = Vec::new();
    for _ in 0..n {
        if rng.below(10) == 0 {
            out.push(rng.next() as u8);
        } else {
            out.extend_from_slice(FRAGMENTS[rng.below(FRAGMENTS.len())]);
        }
    }
    out
}
