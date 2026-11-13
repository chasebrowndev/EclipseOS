// SPDX-License-Identifier: Apache-2.0
//! T-01 §6 / VTCORE-56: differential test against `vte` (dev-dependency only) on 7-bit
//! streams without APC/SOS/PM. Every known difference is a named class in
//! `tests/DIVERGENCES.txt`; the test fails on an unlisted difference and on a listed
//! class that no longer occurs (a stale entry is a lie).

use apoth_vt::{Params, Parser, Sink, StrEnd, StrHead, StrKind, StrRoute};
use std::collections::BTreeSet;

mod common;
use common::Rng;

type Groups = Vec<Vec<u32>>;
/// kind, command digits, params, intermediates, final, payload.
type OurStr = (StrKind, Vec<u8>, Groups, Vec<u8>, u8, Vec<u8>);

/// What both parsers are reduced to.
#[derive(Clone, Debug, PartialEq, Eq)]
enum C {
    Print(String),
    Exec(u8),
    Esc(Vec<u8>, u8),
    Csi(Vec<Vec<u32>>, Vec<u8>, u8),
    /// Joined `;` payload, BEL-terminated?
    Osc(Vec<u8>, bool),
    Dcs(Vec<Vec<u32>>, Vec<u8>, u8, Vec<u8>),
}

/// Normalisation, not divergence: vte reports "no parameters" as one default parameter;
/// ours reports none.
fn groups_default(g: Vec<Vec<u32>>) -> Vec<Vec<u32>> {
    if g.is_empty() { vec![vec![0]] } else { g }
}

/// Normalisation: ours consumes the OSC `;` into the head, so "5;" and "5" are the same
/// to it; both sides drop one trailing `;` before comparing.
fn osc(mut s: Vec<u8>, bel: bool) -> C {
    if s.last() == Some(&b';') {
        s.pop();
    }
    C::Osc(s, bel)
}

fn push_print(out: &mut Vec<C>, s: &str) {
    if let Some(C::Print(p)) = out.last_mut() {
        p.push_str(s);
    } else {
        out.push(C::Print(s.to_string()));
    }
}

// ------------------------------------------------------------------ ours

#[derive(Default)]
struct Ours {
    out: Vec<C>,
    aborted: Vec<Vec<u8>>, // payloads of strings we aborted (vte dispatches them)
    cur: Option<OurStr>,
}

fn groups_ours(p: &Params) -> Vec<Vec<u32>> {
    let mut g: Vec<Vec<u32>> = vec![];
    let mut cur = vec![];
    for i in 0..p.len() {
        cur.push(p.get(i).unwrap_or(0));
        if !p.colon_after(i) {
            g.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        g.push(cur);
    }
    g
}

impl Sink for Ours {
    fn print_run(&mut self, t: &str) {
        push_print(&mut self.out, t);
    }
    fn execute(&mut self, c: u8) {
        self.out.push(C::Exec(c));
    }
    fn esc(&mut self, i: &[u8], f: u8) {
        self.out.push(C::Esc(i.to_vec(), f));
    }
    fn csi(&mut self, p: &Params, i: &[u8], private: u8, f: u8) {
        let mut inter = vec![];
        if private != 0 {
            inter.push(private);
        }
        inter.extend_from_slice(i);
        self.out
            .push(C::Csi(groups_default(groups_ours(p)), inter, f));
    }
    fn string_start(&mut self, k: StrKind, h: &StrHead) -> StrRoute {
        let digits = h.osc_command_digits().to_vec();
        let (g, inter, fin) = if k == StrKind::Dcs {
            let mut inter = vec![];
            if h.dcs_private() != 0 {
                inter.push(h.dcs_private());
            }
            inter.extend_from_slice(h.dcs_intermediates());
            (
                groups_default(groups_ours(h.params())),
                inter,
                h.dcs_final().unwrap_or(0),
            )
        } else {
            (vec![], vec![], 0)
        };
        self.cur = Some((k, digits, g, inter, fin, vec![]));
        StrRoute { cap: 1 << 24 }
    }
    fn string_put(&mut self, c: &[u8]) {
        if let Some(cur) = &mut self.cur {
            cur.5.extend_from_slice(c);
        }
    }
    fn string_end(&mut self, end: StrEnd) {
        let Some((k, digits, g, inter, fin, payload)) = self.cur.take() else {
            return;
        };
        if !end.is_ok() {
            self.aborted.push(payload);
            return;
        }
        match k {
            StrKind::Osc => {
                let mut s = digits;
                if !payload.is_empty() {
                    if !s.is_empty() {
                        s.push(b';');
                    }
                    s.extend_from_slice(&payload);
                }
                self.out.push(osc(s, end == StrEnd::Bel));
            }
            StrKind::Dcs => self.out.push(C::Dcs(g, inter, fin, payload)),
            _ => unreachable!("APC/PM/SOS are excluded from the oracle"),
        }
    }
}

// ------------------------------------------------------------------- vte

#[derive(Default)]
struct Vte {
    /// Events with the index of the byte being fed when they fired.
    out: Vec<(usize, C)>,
    at: usize,
    dcs: Option<(Groups, Vec<u8>, u8, Vec<u8>)>,
    ignored: bool,
    flags: BTreeSet<&'static str>,
}

fn groups_vte(p: &vte::Params) -> Vec<Vec<u32>> {
    p.iter()
        .map(|g| g.iter().map(|&v| u32::from(v)).collect())
        .collect()
}

impl Vte {
    fn push(&mut self, c: C) {
        self.out.push((self.at, c));
    }
}

impl vte::Perform for Vte {
    fn print(&mut self, c: char) {
        if c == '\u{7f}' {
            // vte prints DEL in Ground; T-01 §3 forbids it in `print_run`: ours ignores it.
            self.flags.insert("del-in-ground");
            return;
        }
        let mut b = [0u8; 4];
        let s = c.encode_utf8(&mut b);
        let at = self.at;
        match self.out.last_mut() {
            Some((_, C::Print(p))) => p.push_str(s),
            _ => self.out.push((at, C::Print(s.to_string()))),
        }
    }
    fn execute(&mut self, b: u8) {
        self.push(C::Exec(b));
    }
    fn hook(&mut self, p: &vte::Params, i: &[u8], ignore: bool, a: char) {
        self.ignored |= ignore;
        self.dcs = Some((groups_vte(p), i.to_vec(), a as u8, vec![]));
    }
    fn put(&mut self, b: u8) {
        if let Some(d) = &mut self.dcs {
            d.3.push(b);
        }
    }
    fn unhook(&mut self) {
        if let Some((g, i, f, d)) = self.dcs.take() {
            self.push(C::Dcs(g, i, f, d));
        }
    }
    fn osc_dispatch(&mut self, p: &[&[u8]], bell: bool) {
        self.push(osc(p.join(&b';'), bell));
    }
    fn csi_dispatch(&mut self, p: &vte::Params, i: &[u8], ignore: bool, a: char) {
        self.ignored |= ignore;
        self.push(C::Csi(groups_vte(p), i.to_vec(), a as u8));
    }
    fn esc_dispatch(&mut self, i: &[u8], ignore: bool, b: u8) {
        self.ignored |= ignore;
        self.push(C::Esc(i.to_vec(), b));
    }
}

// ------------------------------------------------------------ normalising

/// Reduce vte's events to ours. Fed byte by byte, each event knows which byte fired it:
/// a string vte ends on an ESC that is followed by `\` was terminated by ST (and the
/// `ESC \` dispatch vte then emits is dropped); any other end is an abort, which ours
/// reports only as `StrEnd::Aborted`, so vte's dispatch of it is dropped and flagged
/// (`abort-dispatches-osc`, `abort-dispatches-dcs`).
fn fold_vte(v: Vec<(usize, C)>, bytes: &[u8], flags: &mut BTreeSet<&'static str>) -> Vec<C> {
    let mut out: Vec<C> = vec![];
    let mut swallow: Option<usize> = None;
    for (at, c) in v {
        if let (Some(k), C::Esc(i, b'\\')) = (swallow, &c)
            && at == k + 1
            && i.is_empty()
        {
            swallow = None;
            continue;
        }
        swallow = None;
        let flag = match &c {
            C::Osc(_, false) => Some("abort-dispatches-osc"),
            C::Dcs(..) => Some("abort-dispatches-dcs"),
            _ => None,
        };
        let Some(flag) = flag else {
            out.push(c);
            continue;
        };
        let st = bytes[at] == 0x1B && bytes.get(at + 1) == Some(&b'\\');
        if st {
            swallow = Some(at);
            out.push(c);
        } else {
            flags.insert(flag);
        }
    }
    out
}

fn run_ours(bytes: &[u8]) -> (Vec<C>, Vec<Vec<u8>>) {
    let mut s = Ours::default();
    let mut p = Parser::new();
    p.advance(bytes, &mut s);
    (s.out, s.aborted)
}

fn run_vte(bytes: &[u8]) -> Vte {
    let mut v = Vte::default();
    let mut p = vte::Parser::new();
    for (i, b) in bytes.iter().enumerate() {
        v.at = i;
        p.advance(&mut v, &[*b]);
    }
    v
}

/// Result of comparing one stream.
enum Verdict {
    Same,
    /// Skipped for a documented reason (class name).
    Skipped(&'static str),
    Differs(String),
}

/// Streams that are not comparable: vte has 2 intermediates (private marker included),
/// 16-bit params, 32 params. Ours has 4 intermediates; see DIVERGENCES.txt.
fn classify(ours: &[C], v: &Vte) -> Option<&'static str> {
    if v.ignored
        || ours.iter().any(|c| match c {
            C::Esc(i, _) | C::Csi(_, i, _) | C::Dcs(_, i, _, _) => i.len() > 2,
            _ => false,
        })
    {
        return Some("intermediate-limit");
    }
    None
}

fn has_osc_c0(bytes: &[u8]) -> bool {
    // A C0 (not BEL/ESC/CAN/SUB) or DEL inside an OSC body: vte drops it, ours delivers.
    let mut in_osc = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if !in_osc {
            if b == 0x1B && bytes.get(i + 1) == Some(&b']') {
                in_osc = true;
                i += 1;
            }
        } else if matches!(b, 0x07 | 0x18 | 0x1A | 0x1B) {
            in_osc = false;
            if b == 0x1B {
                i = i.saturating_sub(1);
            }
        } else if b < 0x20 || b == 0x7F {
            return true;
        }
        i += 1;
    }
    false
}

fn compare(bytes: &[u8], seen: &mut BTreeSet<&'static str>) -> Verdict {
    let (ours, _aborted) = run_ours(bytes);
    let v = run_vte(bytes);
    if let Some(c) = classify(&ours, &v) {
        return Verdict::Skipped(c);
    }
    let mut flags = v.flags.clone();
    let theirs = fold_vte(v.out, bytes, &mut flags);
    seen.extend(flags);
    if ours == theirs {
        return Verdict::Same;
    }
    if let Some(c) = explain(bytes, &ours, &theirs) {
        return Verdict::Skipped(c);
    }
    Verdict::Differs(format!(
        "input {bytes:02x?}\n ours  {ours:?}\n vte   {theirs:?}"
    ))
}

/// Name the documented class that accounts for a difference, if any.
fn explain(bytes: &[u8], ours: &[C], theirs: &[C]) -> Option<&'static str> {
    let st = |v: &[C]| -> Vec<C> {
        let mut out = vec![];
        for c in v {
            match c {
                C::Esc(i, b'\\') if i.is_empty() => {}
                C::Print(t) => push_print(&mut out, t),
                c => out.push(c.clone()),
            }
        }
        out
    };
    if bytes.windows(2).any(|w| w == b"\x1bP") && st(ours) == st(theirs) {
        return Some("dcs-ignore-st");
    }
    if ours
        .iter()
        .any(|c| matches!(c, C::Dcs(.., d) if d.contains(&0x7F)))
    {
        return Some("dcs-del-payload");
    }
    let non_numeric = theirs.iter().any(|c| match c {
        C::Osc(s, _) => {
            let d = s.iter().take_while(|b| b.is_ascii_digit()).count();
            (d == 0 && !s.is_empty()) || s.get(d).is_some_and(|&b| b != b';')
        }
        _ => false,
    });
    if non_numeric {
        return Some("osc-head-not-numeric");
    }
    if has_osc_c0(bytes) {
        return Some("osc-c0-payload");
    }
    None
}

// -------------------------------------------------------------- generation

const TEXT: &[&[u8]] = &[b"abc", b"hello world", b" ", b"~", b"x"];
const CTRL: &[u8] = &[
    0x00, 0x07, 0x08, 0x09, 0x0A, 0x0D, 0x0E, 0x0F, 0x18, 0x1A, 0x7F,
];

fn pick<'a, T>(r: &mut Rng, xs: &'a [T]) -> &'a T {
    &xs[r.below(xs.len())]
}

fn gen_params(r: &mut Rng, out: &mut Vec<u8>) {
    let n = r.below(6);
    for k in 0..n {
        if r.below(4) != 0 {
            out.extend_from_slice(r.below(10_000).to_string().as_bytes());
        }
        if k + 1 < n {
            out.push(if r.below(5) == 0 { b':' } else { b';' });
        }
    }
}

fn gen_inter(r: &mut Rng, out: &mut Vec<u8>) {
    for _ in 0..r.below(3) {
        out.push(0x20 + r.below(16) as u8);
    }
}

fn gen_stream(r: &mut Rng) -> Vec<u8> {
    let mut o = vec![];
    for _ in 0..1 + r.below(8) {
        match r.below(12) {
            0..=2 => o.extend_from_slice(pick(r, TEXT)),
            3 => o.push(*pick(r, CTRL)),
            4..=6 => {
                o.extend_from_slice(b"\x1b[");
                if r.below(4) == 0 {
                    o.push(*pick(r, b"?><="));
                }
                gen_params(r, &mut o);
                gen_inter(r, &mut o);
                o.push(0x40 + r.below(63) as u8);
            }
            7 => {
                o.push(0x1B);
                gen_inter(r, &mut o);
                // Never an introducer: ESC P/X/[/]/^/_ start strings or CSI.
                let mut f = 0x30 + r.below(79) as u8;
                if matches!(f, 0x50 | 0x58 | 0x5B | 0x5D | 0x5E | 0x5F) {
                    f = b'c';
                }
                o.push(f);
            }
            8 | 9 => {
                o.extend_from_slice(b"\x1b]");
                o.extend_from_slice(r.below(1000).to_string().as_bytes());
                o.push(b';');
                for _ in 0..r.below(12) {
                    o.push(0x20 + r.below(95) as u8);
                }
                if r.below(2) == 0 {
                    o.push(0x07);
                } else {
                    o.extend_from_slice(b"\x1b\\");
                }
            }
            _ => {
                o.extend_from_slice(b"\x1bP");
                if r.below(3) == 0 {
                    o.push(*pick(r, b"?><="));
                }
                gen_params(r, &mut o);
                gen_inter(r, &mut o);
                o.push(0x40 + r.below(63) as u8);
                for _ in 0..r.below(12) {
                    o.push(0x20 + r.below(95) as u8);
                }
                o.extend_from_slice(b"\x1b\\");
            }
        }
    }
    // One 7-bit mutation: insert a control or structural byte at a random place.
    if r.below(2) == 0 {
        let at = r.below(o.len() + 1);
        let b = *pick(
            r,
            &[
                0x1B, 0x18, 0x1A, 0x07, b'[', b']', b'P', b'\\', b';', b'?', 0x7F, 0x0A,
            ],
        );
        o.insert(at, b);
    }
    o
}

// ------------------------------------------------------------------ tests

fn listed_classes() -> BTreeSet<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/DIVERGENCES.txt");
    let text = std::fs::read_to_string(path).expect("tests/DIVERGENCES.txt");
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#') && !l.starts_with(' '))
        .map(|l| l.split_whitespace().next().unwrap_or("").to_string())
        .collect()
}

#[test]
fn differential_against_vte() {
    let listed = listed_classes();
    let mut seen: BTreeSet<&'static str> = BTreeSet::new();
    let (mut same, mut skipped) = (0u32, 0u32);
    let mut rng = Rng(0x7E57_0000_D1FF_0001);
    let total = 20_000;
    for _ in 0..total {
        let s = gen_stream(&mut rng);
        // The oracle covers streams without APC/SOS/PM (vte cannot deliver them).
        if s.windows(2)
            .any(|w| w[0] == 0x1B && matches!(w[1], b'X' | b'^' | b'_'))
        {
            continue;
        }
        match compare(&s, &mut seen) {
            Verdict::Same => same += 1,
            Verdict::Skipped(c) => {
                seen.insert(c);
                skipped += 1;
            }
            Verdict::Differs(why) => panic!("unlisted divergence from vte:\n{why}"),
        }
    }
    assert!(
        same > total / 2,
        "too few comparable streams: {same} same, {skipped} skipped"
    );
    for c in &seen {
        assert!(
            listed.contains(*c),
            "class {c} occurs but is not in DIVERGENCES.txt"
        );
    }
    for c in &listed {
        assert!(
            seen.contains(c.as_str()),
            "DIVERGENCES.txt lists {c}, which never occurred"
        );
    }
}
