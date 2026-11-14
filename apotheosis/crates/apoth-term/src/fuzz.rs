// SPDX-License-Identifier: AGPL-3.0-only
//! Stable entry points for the `osc_all` and `kitty_keyboard_decode` fuzz targets (T-18
//! §11). They panic only when a security invariant breaks:
//!
//! * a reply that is not in the closed reply grammar (T-18 §2): every reply is a run of
//!   `CSI <printable> <final>`, `OSC|DCS|APC <printable> (BEL | ESC \)`; no payload byte
//!   from the request can smuggle a control into it;
//! * a denied class with an observable effect or a reply (T-18 §3, §11);
//! * a title carrying a control character;
//! * kitty keyboard flags outside their five bits, or the published mirror disagreeing
//!   with the term;
//! * graphics (`kitty_apc`, `sixel`, T-15 §8): a store over its quota, a placement count
//!   over the cap, an image over the axis cap, or graphics with any effect while
//!   `graphics-direct` is denied.

use std::time::Instant;

use crate::{Term, TermConfig, deny};

/// OSC numbers the target addresses (byte 0 of the input picks one), and the deny bit
/// that gates each (0: not policy-gated).
const OSCS: &[(u32, u32)] = &[
    (0, deny::TITLE_SET),
    (1, deny::TITLE_SET),
    (2, deny::TITLE_SET),
    (4, deny::COLOUR),
    (7, 0),
    (8, deny::HYPERLINK),
    (9, deny::NOTIFICATION),
    (10, deny::COLOUR),
    (11, deny::COLOUR),
    (12, deny::COLOUR),
    (17, deny::COLOUR),
    (19, deny::COLOUR),
    (22, 0),
    (50, 0),
    (
        52,
        deny::CLIPBOARD_WRITE | deny::CLIPBOARD_READ | deny::PRIMARY,
    ),
    (99, deny::NOTIFICATION),
    (104, deny::COLOUR),
    (110, deny::COLOUR),
    (111, deny::COLOUR),
    (112, deny::COLOUR),
    (133, 0),
    (633, 0),
    (666, 0),
    (777, deny::NOTIFICATION),
    (1337, deny::USER_VAR),
    (3008, 0),
    (5522, 0),
];

/// Every deny bit.
const DENY_ALL: u32 = deny::TITLE_SET
    | deny::TITLE_REPORT
    | deny::WINDOW_REPORT
    | deny::COLOUR
    | deny::NOTIFICATION
    | deny::HYPERLINK
    | deny::CLIPBOARD_WRITE
    | deny::CLIPBOARD_READ
    | deny::PRIMARY
    | deny::USER_VAR;

fn term(deny_bits: u32) -> Term {
    Term::new(TermConfig {
        rows: 3,
        cols: 16,
        deny: deny_bits,
        ..TermConfig::default()
    })
}

type Observed = (Vec<String>, String, Vec<String>, Vec<u8>);

/// Everything a sequence can change that a user or program can observe.
fn observe(t: &mut Term) -> Observed {
    let rows = (0..t.rows()).map(|y| t.row_text(y)).collect();
    let mut ev = t.events_ref().render();
    for i in 0..=255u8 {
        ev.push(format!("{:?}", t.palette(i)));
    }
    ev.push(format!("{:?}", t.colors()));
    let link = t.grid().link_at(0, 0).map(|(_, u)| u).unwrap_or_default();
    ev.push(format!("link {link}"));
    let replies = t.replies().take_all();
    (rows, t.title().to_string(), ev, replies)
}

/// True when `r` is a concatenation of closed-grammar replies.
pub fn reply_is_closed(r: &[u8]) -> bool {
    let printable = |b: u8| (0x20..0x7f).contains(&b);
    let mut i = 0;
    while i < r.len() {
        if r[i] != 0x1b || i + 1 >= r.len() {
            return false;
        }
        match r[i + 1] {
            b'[' => {
                i += 2;
                loop {
                    match r.get(i) {
                        Some(&b) if (0x40..0x7f).contains(&b) => break,
                        Some(&b) if (0x20..0x40).contains(&b) => i += 1,
                        _ => return false,
                    }
                }
                i += 1;
            }
            b']' | b'P' | b'_' => {
                i += 2;
                loop {
                    match r.get(i) {
                        Some(0x07) => {
                            i += 1;
                            break;
                        }
                        Some(0x1b) if r.get(i + 1) == Some(&b'\\') => {
                            i += 2;
                            break;
                        }
                        Some(&b) if printable(b) => i += 1,
                        _ => return false,
                    }
                }
            }
            _ => return false,
        }
    }
    true
}

fn check_term(t: &Term) {
    assert!(
        !t.title().chars().any(char::is_control),
        "a control reached the title"
    );
}

/// Input: byte 0 picks the OSC number, byte 1 the terminator and a query form, the rest is
/// the payload with the bytes that end a string (ESC, BEL, CAN, SUB) removed so it stays
/// inside the sequence.
pub fn fuzz_osc(data: &[u8]) {
    let [n, form, payload @ ..] = data else {
        return;
    };
    let (num, gate) = OSCS[usize::from(*n) % OSCS.len()];
    let body: Vec<u8> = payload
        .iter()
        .copied()
        .filter(|b| !matches!(b, 0x1b | 0x07 | 0x18 | 0x1a))
        .collect();
    // `OSC 9 ; 4` is ConEmu progress, not a notification (T-18 §3 gates notifications).
    let gate = if num == 9 && (body.starts_with(b"4;") || body == b"4") {
        0
    } else {
        gate
    };
    let mut seq = format!("\x1b]{num}").into_bytes();
    if form & 4 == 0 || !body.is_empty() {
        seq.push(b';');
    }
    seq.extend_from_slice(&body);
    if form & 8 != 0 {
        seq.extend_from_slice(b";?");
    }
    seq.extend_from_slice(if form & 1 == 0 { b"\x07" } else { b"\x1b\\" });
    if form & 2 != 0 {
        // Text after the sequence: a hyperlink or title must not leak beyond it.
        seq.extend_from_slice(b"ab\x1b]8;;\x1b\\");
    }

    let now = Instant::now();
    for bits in [0, DENY_ALL] {
        let mut t = term(bits);
        t.feed(&seq, now);
        check_term(&t);
        let seen = observe(&mut t);
        assert!(reply_is_closed(&seen.3), "reply outside the closed grammar");
        if bits == DENY_ALL && gate != 0 {
            let mut fresh = term(bits);
            if form & 2 != 0 {
                fresh.feed(b"ab\x1b]8;;\x1b\\", now);
            }
            assert!(seen.3.is_empty(), "a denied OSC {num} replied");
            let base = observe(&mut fresh);
            assert!(
                seen == base,
                "a denied OSC {num} had an effect: {seen:?} vs {base:?}"
            );
        }
    }
}

/// Input: a list of operations, two bytes each. Byte 0 picks the form (`CSI > f u` push,
/// `CSI < n u` pop, `CSI = f ; m u` set, `CSI ? u` query, alt-screen switch, RIS), byte 1
/// is the parameter (values past 255 are made from the high bit).
pub fn fuzz_kitty_keyboard(data: &[u8]) {
    let mut t = term(0);
    let now = Instant::now();
    for op in data.chunks(2) {
        let [form, p] = *op else { break };
        let p = if form & 0x80 != 0 {
            u32::from(p) * 65_537
        } else {
            u32::from(p)
        };
        let seq = match form % 7 {
            0 => format!("\x1b[>{p}u"),
            1 => format!("\x1b[<{p}u"),
            2 => format!("\x1b[={p};{}u", (form >> 3) % 5),
            3 => "\x1b[?u".to_string(),
            4 => "\x1b[?1049h".to_string(),
            5 => "\x1b[?1049l".to_string(),
            _ => format!("\x1b[>{p};{p}u\x1b[<u\x1b[={p}u"),
        };
        t.feed(seq.as_bytes(), now);
        let f = t.kitty_flags();
        assert!(f < 32, "kitty flags outside their five bits");
        assert_eq!(
            t.input_mirror().load().kitty_flags(),
            f,
            "published mirror disagrees with the term"
        );
        let r = t.replies().take_all();
        if form % 7 == 3 {
            assert_eq!(r, format!("\x1b[?{f}u").into_bytes(), "query reply");
        } else {
            assert!(reply_is_closed(&r));
        }
    }
}

/// Small caps so the targets reach the limits quickly.
fn gfx_term(deny_bits: u32) -> Term {
    let mut cfg = TermConfig {
        rows: 6,
        cols: 20,
        // File media would touch the filesystem (temp files are consumed): always denied.
        deny: deny_bits | deny::GRAPHICS_FILE,
        ..TermConfig::default()
    };
    let l = &mut cfg.graphics.limits;
    l.max_axis = 512;
    l.quota = 2 << 20;
    l.quota_total = 4 << 20;
    l.max_decoded_bytes = 1 << 20;
    l.sixel_max_bytes = 64 << 10;
    l.sixel_max_pixels = 1 << 18;
    Term::new(cfg)
}

fn check_gfx(t: &mut Term) {
    let st = t.image_store();
    assert!(st.used() <= 2 << 20, "image store over its quota");
    assert!(
        st.placements().len() <= crate::apoth_image::store::MAX_PLACEMENTS,
        "placement cap broken"
    );
    for p in t.placements() {
        assert!(p.cols >= 1 && p.rows >= 1, "empty placement box");
    }
    let mut draws = Vec::new();
    let now = std::time::Instant::now();
    t.image_draws(t.primary().history_rows(), t.rows(), now, &mut draws);
    assert!(st_anim_ok(t), "animation quota broken");
    for d in &draws {
        assert!(
            d.image.width <= 512 && d.image.height <= 512,
            "axis cap broken"
        );
        let [x, y, w, h] = d.src;
        assert!(
            u64::from(x) + u64::from(w) <= u64::from(d.image.width)
                && u64::from(y) + u64::from(h) <= u64::from(d.image.height),
            "source rect outside the image"
        );
    }
    let r = t.replies().take_all();
    assert!(
        reply_is_closed(&r),
        "graphics reply outside the closed grammar"
    );
}

/// Strip the bytes that would end a string early.
fn inside(b: &[u8]) -> Vec<u8> {
    b.iter()
        .copied()
        .filter(|b| !matches!(b, 0x1b | 0x18 | 0x1a))
        .collect()
}

/// Input: APC bodies separated by `0xff`; each is sent as `ESC _ G <body> ESC \`, with a
/// line of text after every third. The same stream runs with `graphics-direct` denied,
/// which must look exactly like the stream with the APCs removed.
pub fn fuzz_kitty_apc(data: &[u8]) {
    let now = Instant::now();
    let mut stream = Vec::new();
    let mut plain = Vec::new();
    for (i, body) in data.split(|&b| b == 0xff).enumerate() {
        stream.extend_from_slice(b"\x1b_G");
        stream.extend_from_slice(&inside(body));
        stream.extend_from_slice(b"\x1b\\");
        if i % 3 == 2 {
            stream.extend_from_slice(b"txt\r\n");
            plain.extend_from_slice(b"txt\r\n");
        }
    }
    let mut t = gfx_term(0);
    // Arbitrary chunking: split the stream where the input says.
    let cut = data.first().map_or(1, |&b| usize::from(b) + 1);
    for c in stream.chunks(cut) {
        t.feed(c, now);
        check_gfx(&mut t);
    }
    assert!(!t.blocked(), "inline decode never blocks");
    let mut denied = gfx_term(deny::GRAPHICS_DIRECT);
    denied.feed(&stream, now);
    assert!(!denied.has_images(), "denied graphics placed an image");
    let seen = observe(&mut denied);
    let mut base = gfx_term(deny::GRAPHICS_DIRECT);
    base.feed(&plain, now);
    assert!(seen == observe(&mut base), "denied graphics had an effect");
}

/// Input: byte 0 picks the DCS parameters and modes (DECSDM, `?1070`, `?8452`), byte 1
/// the chunk size; the rest is sixel data (string-ending bytes removed), sent twice so
/// shared registers carry over, with text printed over the result.
pub fn fuzz_sixel(data: &[u8]) {
    let [sel, chunk, body @ ..] = data else {
        return;
    };
    let now = Instant::now();
    let mut stream = Vec::new();
    for (bit, mode) in [(1u8, 80u16), (2, 1070), (4, 8452)] {
        let hl = if sel & bit != 0 { 'h' } else { 'l' };
        stream.extend_from_slice(format!("\x1b[?{mode}{hl}").as_bytes());
    }
    let p1 = (sel >> 3) % 10;
    let p2 = (sel >> 6) % 3;
    let body = inside(body);
    for _ in 0..2 {
        stream.extend_from_slice(format!("\x1bP{p1};{p2};0q").as_bytes());
        stream.extend_from_slice(&body);
        stream.extend_from_slice(b"\x1b\\");
    }
    stream.extend_from_slice(b"\x1b[1;1Hover\x1b[2K\x1b[3X");
    let mut t = gfx_term(0);
    for c in stream.chunks(usize::from(*chunk) + 1) {
        t.feed(c, now);
        check_gfx(&mut t);
    }
    for p in t.placements() {
        assert!(p.sixel && p.rows == 1, "sixel stored as cell-row slices");
    }
    let mut denied = gfx_term(deny::GRAPHICS_DIRECT);
    denied.feed(&stream, now);
    assert!(!denied.has_images(), "denied sixel placed an image");
}

fn st_anim_ok(t: &Term) -> bool {
    t.image_store().anim_used() <= t.image_store_limits().animation_quota
}

/// Input: byte 0 picks the virtual placement's cell box, byte 1 the chunk size; the rest is
/// a cell script: each byte pair is (kind, value) printing a placeholder with 0-3 marks
/// taken from the value, a bare placeholder, a plain letter, a colour change (id or
/// placement through the foreground / underline colour), a cursor jump or a newline. Every
/// quad's source rect must stay inside its image and the run count below the cell count.
pub fn fuzz_placeholder(data: &[u8]) {
    use crate::apoth_image::placeholder::{DIACRITICS, PLACEHOLDER};
    let [boxsel, chunk, script @ ..] = data else {
        return;
    };
    let now = Instant::now();
    let (c, r) = (u32::from(boxsel & 7), u32::from((boxsel >> 3) & 7));
    let mut stream = format!(
        "\x1b_Ga=T,U=1,f=24,s=5,v=7,i=1,c={c},r={r},q=2;{}\x1b\\\x1b_Ga=T,U=1,f=24,s=3,v=2,i=2,p=4,q=2;{}\x1b\\",
        b64_encode(&[0x40; 5 * 7 * 3]),
        b64_encode(&[0x80; 3 * 2 * 3]),
    )
    .into_bytes();
    let ph = char::from_u32(PLACEHOLDER).unwrap_or(' ');
    for pair in script.chunks(2) {
        let (k, v) = (pair[0], pair.get(1).copied().unwrap_or(0));
        let d =
            |i: u8| char::from_u32(DIACRITICS[usize::from(i) % DIACRITICS.len()]).unwrap_or(' ');
        let s = match k % 8 {
            0 => format!("{ph}{}{}", d(v & 15), d(v >> 4)),
            1 => format!("{ph}{}", d(v)),
            2 => format!("{ph}{}{}{}", d(v & 7), d(v >> 3), d(v)),
            3 => ph.to_string(),
            4 => "x".to_string(),
            5 => format!("\x1b[38;5;{v}m\x1b[58;5;{}m", v % 5),
            6 => format!("\x1b[{};{}H", v % 7 + 1, v / 7 % 21 + 1),
            _ => "\r\n".to_string(),
        };
        stream.extend_from_slice(s.as_bytes());
    }
    let mut t = gfx_term(0);
    for ch in stream.chunks(usize::from(*chunk) + 1) {
        t.feed(ch, now);
    }
    check_gfx(&mut t);
    let mut draws = Vec::new();
    t.image_draws(t.primary().history_rows(), t.rows(), now, &mut draws);
    assert!(
        draws.len() <= t.rows() * t.cols() + 64,
        "more runs than cells"
    );
}

/// Input: byte 0 picks the `File=` arguments, the rest is the file (base64-encoded here),
/// so every container decoder sees arbitrary bytes through the OSC 1337 path.
pub fn fuzz_iterm(data: &[u8]) {
    let [sel, body @ ..] = data else {
        return;
    };
    let args = [
        "inline=1",
        "inline=1;width=3;height=2",
        "inline=1;width=50%;preserveAspectRatio=0",
        "inline=1;width=20px;height=auto",
        "inline=0",
        "inline=1;size=7",
        "inline=1;name=YQ==;width=999999999",
        "inline=1;height=100%",
    ][usize::from(sel % 8)];
    let now = Instant::now();
    let s = format!("\x1b]1337;File={args}:{}\x07after", b64_encode(body));
    let mut t = gfx_term(0);
    t.feed(s.as_bytes(), now);
    check_gfx(&mut t);
    let mut denied = gfx_term(deny::GRAPHICS_DIRECT);
    denied.feed(s.as_bytes(), now);
    assert!(!denied.has_images(), "denied iTerm2 image placed");
}

/// The container decoders straight (PNG/APNG, GIF, JPEG, WebP): never panic, never exceed
/// the axis, decoded-size and animation caps.
pub fn fuzz_image_formats(data: &[u8]) {
    let limits = crate::apoth_image::Limits {
        max_axis: 512,
        max_decoded_bytes: 1 << 20,
        ..Default::default()
    };
    let quota = 4 << 20;
    if let Ok(d) = crate::apoth_image::formats::decode_any(data, &limits, quota) {
        let mut extra = 0u64;
        for (i, (img, _)) in d.frames.iter().enumerate() {
            assert!(img.width <= 512 && img.height <= 512, "axis cap broken");
            assert_eq!(
                img.rgba.len() as u64,
                u64::from(img.width) * u64::from(img.height) * 4
            );
            if i > 0 {
                extra += img.bytes();
            }
        }
        assert!(extra <= quota, "animation quota broken");
        assert!(d.frames.len() <= crate::apoth_image::anim::MAX_FRAMES);
    }
}

fn b64_encode(b: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let v = u32::from(c[0]) << 16
            | u32::from(c.get(1).copied().unwrap_or(0)) << 8
            | u32::from(c.get(2).copied().unwrap_or(0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(char::from(A[(v >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus(name: &str, f: fn(&[u8])) -> usize {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("corpus")
            .join(name);
        let mut n = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            f(&std::fs::read(entry.unwrap().path()).unwrap());
            n += 1;
        }
        n
    }

    #[test]
    fn osc_corpus_seeds_hold_the_invariants() {
        assert!(corpus("osc_all", fuzz_osc) >= 8);
    }

    #[test]
    fn kitty_apc_corpus_seeds_hold_the_invariants() {
        assert!(corpus("kitty_apc", fuzz_kitty_apc) >= 8);
    }

    #[test]
    fn sixel_corpus_seeds_hold_the_invariants() {
        assert!(corpus("sixel", fuzz_sixel) >= 6);
    }

    #[test]
    fn placeholder_corpus_seeds_hold_the_invariants() {
        assert!(corpus("placeholder", fuzz_placeholder) >= 4);
    }

    #[test]
    fn iterm_corpus_seeds_hold_the_invariants() {
        assert!(corpus("iterm", fuzz_iterm) >= 4);
    }

    #[test]
    fn image_formats_corpus_seeds_hold_the_invariants() {
        assert!(corpus("image_formats", fuzz_image_formats) >= 4);
    }

    #[test]
    fn kitty_corpus_seeds_hold_the_invariants() {
        assert!(corpus("kitty_keyboard_decode", fuzz_kitty_keyboard) >= 4);
    }

    #[test]
    fn every_osc_with_hostile_payloads() {
        let payloads: &[&[u8]] = &[
            b"",
            b"?",
            b"c;?",
            b"p;aGk=",
            b"1;rgb:ff/00/00",
            b"1;?;2;?",
            b"notify;\xe2\x80\xaetitle;body\x9b31m",
            b"id=x;file:///etc/passwd",
            b"A;k=s",
            b"\x00\x01\x7f\x9c\x9d",
            b";;;;;;;;;;;;;;;;;;;;;;;;;;;;;;",
        ];
        for n in 0..OSCS.len() as u8 {
            for form in 0..16u8 {
                for p in payloads {
                    let mut d = vec![n, form];
                    d.extend_from_slice(p);
                    fuzz_osc(&d);
                }
            }
        }
    }

    #[test]
    fn closed_grammar_accepts_replies_and_rejects_injection() {
        assert!(reply_is_closed(b""));
        assert!(reply_is_closed(
            b"\x1b[?1;2c\x1b]11;rgb:0000/0000/0000\x1b\\"
        ));
        assert!(reply_is_closed(b"\x1bP1$r0m\x1b\\\x1b]l\x07"));
        assert!(!reply_is_closed(b"\x1b]2;a\x1b]0;b\x07"));
        assert!(!reply_is_closed(b"\x1b[1\x0d;2c"));
        assert!(!reply_is_closed(b"x"));
        assert!(!reply_is_closed(b"\x1b]52;c;\x9c"));
    }

    #[test]
    fn kitty_ops_cover_every_form() {
        let mut d = Vec::new();
        for form in 0..=255u8 {
            d.extend_from_slice(&[form, form.wrapping_mul(37)]);
        }
        fuzz_kitty_keyboard(&d);
    }
}
