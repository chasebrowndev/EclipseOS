// SPDX-License-Identifier: Apache-2.0
//! Legacy xterm/VT220 key encoding with DECCKM, DECKPAM, DECBKM and modifyOtherKeys.
//!
//! Not built: HP/SCO/Sun function keys, Win32 input mode, always-on fixterms, 8-bit meta.

use crate::buf::W;
use crate::key::{Key, KeyEvent, KeyKind, Kp, Mods, Named};
use crate::modes::{InputModes, bit};

/// xterm modifier parameter: 1 + shift 1, alt 2, ctrl 4, meta 8 (INPUT-13).
/// Super and Meta both map to 8.
pub(crate) fn mod_param(m: Mods) -> u32 {
    let mut p = 1;
    if m.contains(Mods::SHIFT) {
        p += 1;
    }
    if m.contains(Mods::ALT) {
        p += 2;
    }
    if m.contains(Mods::CTRL) {
        p += 4;
    }
    if m.0 & (Mods::META.0 | Mods::SUPER.0) != 0 {
        p += 8;
    }
    p
}

/// Control byte for `c` under Ctrl (INPUT-70): `@`..`~` and digits map to C0 bytes.
pub fn ctrl_byte(c: char) -> Option<u8> {
    match c {
        '\u{40}'..='\u{7e}' => Some(c as u8 & 0x1f),
        ' ' | '2' => Some(0),
        '3' => Some(0x1b),
        '4' => Some(0x1c),
        '5' => Some(0x1d),
        '6' => Some(0x1e),
        '7' | '/' => Some(0x1f),
        '8' | '?' => Some(0x7f),
        _ => None,
    }
}

pub(crate) fn encode(ev: &KeyEvent<'_>, modes: InputModes, w: &mut W<'_>) {
    if ev.kind == KeyKind::Release {
        return;
    }
    match ev.key {
        Key::Char(c) => char_key(ev, c, modes, w),
        Key::Named(n) => named_key(ev, n, modes, w),
    }
}

fn char_key(ev: &KeyEvent<'_>, c: char, modes: InputModes, w: &mut W<'_>) {
    let m = ev.mods.without_locks();
    let ctrl = m.contains(Mods::CTRL);
    let alt = m.contains(Mods::ALT);
    let level = modes.mok_level();
    let key_for_ctrl = if c.is_ascii() {
        Some(c)
    } else {
        ev.ctrl_fallback
    };
    let cbyte = key_for_ctrl.and_then(ctrl_byte);
    let modified = m.0 & (Mods::CTRL.0 | Mods::ALT.0 | Mods::META.0 | Mods::SUPER.0) != 0;
    let mok = match level {
        0 => false,
        1 => ctrl && cbyte.is_none(),
        _ => modified,
    };
    if mok {
        let code = match ev.shifted {
            Some(s) if m.contains(Mods::SHIFT) => s as u32,
            _ => c as u32,
        };
        other_key(w, modes, mod_param(m), code);
        return;
    }
    let alt_prefix = alt && modes.has(bit::ALT_ESC);
    if alt_prefix {
        w.byte(0x1b);
    }
    if ctrl && let Some(b) = cbyte {
        w.byte(b);
    } else if ev.text.is_empty() {
        w.ch(c);
    } else {
        w.str_fit(ev.text);
    }
}

/// modifyOtherKeys sequence: `CSI 27;m;code ~`, or `CSI code;m u` with formatOtherKeys.
fn other_key(w: &mut W<'_>, modes: InputModes, mp: u32, code: u32) {
    w.csi();
    if modes.has(bit::FORMAT_OTHER_KEYS) {
        w.num(code);
        w.byte(b';');
        w.num(mp);
        w.byte(b'u');
    } else {
        w.bytes(b"27;");
        w.num(mp);
        w.byte(b';');
        w.num(code);
        w.byte(b'~');
    }
}

/// `CSI 1;m X` / `SS3 X` / `CSI X`.
fn cursor(w: &mut W<'_>, modes: InputModes, fin: u8, mp: u32) {
    if mp == 1 {
        if modes.has(bit::DECCKM) {
            w.ss3();
        } else {
            w.csi();
        }
    } else {
        w.csi();
        w.bytes(b"1;");
        w.num(mp);
    }
    w.byte(fin);
}

/// `CSI n ; m ~`.
fn tilde(w: &mut W<'_>, n: u32, mp: u32) {
    w.csi();
    w.num(n);
    if mp != 1 {
        w.byte(b';');
        w.num(mp);
    }
    w.byte(b'~');
}

fn named_key(ev: &KeyEvent<'_>, n: Named, modes: InputModes, w: &mut W<'_>) {
    let m = ev.mods.without_locks();
    let mp = mod_param(m);
    let alt_prefix = m.contains(Mods::ALT) && modes.has(bit::ALT_ESC);
    let shift = m.contains(Mods::SHIFT);
    let ctrl = m.contains(Mods::CTRL);
    let other_mods = m.0 & (Mods::CTRL.0 | Mods::ALT.0 | Mods::META.0 | Mods::SUPER.0) != 0;
    let mok2 = modes.mok_level() >= 2;
    match n {
        Named::Escape | Named::Enter | Named::Backspace | Named::Tab => {
            let code = match n {
                Named::Escape => 27,
                Named::Enter => 13,
                Named::Backspace => 127,
                _ => 9,
            };
            // Shift+Tab is CSI Z unless modifyOtherKeys level 2 claims modified keys.
            if n == Named::Tab && shift && !(mok2 && other_mods) {
                if mp == 2 {
                    w.bytes(b"\x1b[Z");
                } else {
                    cursor(w, modes, b'Z', mp);
                }
                return;
            }
            if mok2 && other_mods {
                other_key(w, modes, mp, code);
                return;
            }
            if alt_prefix {
                w.byte(0x1b);
            }
            let b = match n {
                Named::Escape => 0x1b,
                Named::Enter => b'\r',
                Named::Tab => b'\t',
                _ => {
                    // DECBKM set: BS; Ctrl inverts.
                    if modes.has(bit::DECBKM) != ctrl {
                        0x08
                    } else {
                        0x7f
                    }
                }
            };
            w.byte(b);
        }
        Named::Left | Named::Right | Named::Up | Named::Down | Named::Home | Named::End => {
            cursor(w, modes, nav_final(n), mp);
        }
        Named::Insert => tilde(w, 2, mp),
        Named::Delete => tilde(w, 3, mp),
        Named::PageUp => tilde(w, 5, mp),
        Named::PageDown => tilde(w, 6, mp),
        Named::Menu => tilde(w, 29, mp),
        Named::F(k) => fkey(w, k, mp),
        Named::Kp(k) => keypad(ev, k, modes, mp, w),
        Named::CapsLock
        | Named::ScrollLock
        | Named::NumLock
        | Named::PrintScreen
        | Named::Pause
        | Named::Pua(_) => {}
    }
}

fn nav_final(n: Named) -> u8 {
    match n {
        Named::Left => b'D',
        Named::Right => b'C',
        Named::Up => b'A',
        Named::Down => b'B',
        Named::Home => b'H',
        _ => b'F',
    }
}

fn fkey(w: &mut W<'_>, k: u8, mp: u32) {
    // F1-F4 are SS3 P..S unmodified, CSI 1;m P..S modified.
    if (1..=4).contains(&k) {
        let fin = b"PQRS"[usize::from(k - 1)];
        if mp == 1 {
            w.ss3();
        } else {
            w.csi();
            w.bytes(b"1;");
            w.num(mp);
        }
        w.byte(fin);
        return;
    }
    let n = match k {
        5 => 15,
        6 => 17,
        7 => 18,
        8 => 19,
        9 => 20,
        10 => 21,
        11 => 23,
        12 => 24,
        13 => 25,
        14 => 26,
        15 => 28,
        16 => 29,
        17 => 31,
        18 => 32,
        19 => 33,
        20 => 34,
        // F21..F35 have no legacy encoding.
        _ => return,
    };
    tilde(w, n, mp);
}

fn keypad(ev: &KeyEvent<'_>, k: Kp, modes: InputModes, mp: u32, w: &mut W<'_>) {
    let nav = match k {
        Kp::Left => Some(Named::Left),
        Kp::Right => Some(Named::Right),
        Kp::Up => Some(Named::Up),
        Kp::Down => Some(Named::Down),
        Kp::Home => Some(Named::Home),
        Kp::End => Some(Named::End),
        Kp::PageUp => Some(Named::PageUp),
        Kp::PageDown => Some(Named::PageDown),
        Kp::Insert => Some(Named::Insert),
        Kp::Delete => Some(Named::Delete),
        _ => None,
    };
    if let Some(n) = nav {
        named_key(
            &KeyEvent {
                key: Key::Named(n),
                ..*ev
            },
            n,
            modes,
            w,
        );
        return;
    }
    if k == Kp::Begin {
        cursor(w, modes, b'E', mp);
        return;
    }
    let (num_byte, app_final) = match k {
        Kp::Digit(d) => (b'0' + d.min(9), b'p' + d.min(9)),
        Kp::Decimal => (b'.', b'n'),
        Kp::Divide => (b'/', b'o'),
        Kp::Multiply => (b'*', b'j'),
        Kp::Subtract => (b'-', b'm'),
        Kp::Add => (b'+', b'k'),
        Kp::Enter => (b'\r', b'M'),
        Kp::Equal => (b'=', b'X'),
        _ => (b',', b'l'),
    };
    if modes.has(bit::DECKPAM) {
        w.ss3();
        if mp != 1 {
            w.num(mp);
        }
        w.byte(app_final);
    } else {
        if m_alt(ev, modes) {
            w.byte(0x1b);
        }
        w.byte(num_byte);
    }
}

fn m_alt(ev: &KeyEvent<'_>, modes: InputModes) -> bool {
    ev.mods.contains(Mods::ALT) && modes.has(bit::ALT_ESC)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode_key;

    fn enc(ev: KeyEvent<'_>, modes: InputModes) -> Vec<u8> {
        let mut out = [0u8; 64];
        let n = encode_key(&ev, modes, &mut out);
        out[..n].to_vec()
    }
    fn k(c: char) -> KeyEvent<'static> {
        KeyEvent::new(Key::Char(c))
    }
    fn nk(n: Named) -> KeyEvent<'static> {
        KeyEvent::new(Key::Named(n))
    }
    const D: fn() -> InputModes = InputModes::default;

    #[test]
    fn plain_text() {
        assert_eq!(enc(k('a').text("a"), D()), b"a");
        assert_eq!(enc(k('a').text("A").mods(Mods::SHIFT), D()), b"A");
        assert_eq!(enc(k('e').text("é"), D()), "é".as_bytes());
        assert_eq!(enc(k('a'), D()), b"a");
    }

    #[test]
    fn release_sends_nothing() {
        assert!(enc(k('a').text("a").kind(KeyKind::Release), D()).is_empty());
    }

    #[test]
    fn repeat_is_press() {
        assert_eq!(enc(k('a').text("a").kind(KeyKind::Repeat), D()), b"a");
    }

    #[test]
    fn ctrl_table() {
        for (c, b) in [
            ('a', 1u8),
            ('c', 3),
            ('z', 26),
            ('[', 0x1b),
            ('\\', 0x1c),
            (']', 0x1d),
            ('@', 0),
            ('^', 0x1e),
            ('_', 0x1f),
            ('~', 0x1e),
        ] {
            assert_eq!(enc(k(c).mods(Mods::CTRL), D()), [b], "ctrl+{c}");
        }
        assert_eq!(enc(k(' ').mods(Mods::CTRL), D()), [0]);
        assert_eq!(enc(k('2').mods(Mods::CTRL), D()), [0]);
        assert_eq!(enc(k('3').mods(Mods::CTRL), D()), [0x1b]);
        assert_eq!(enc(k('7').mods(Mods::CTRL), D()), [0x1f]);
        assert_eq!(enc(k('8').mods(Mods::CTRL), D()), [0x7f]);
        assert_eq!(enc(k('?').mods(Mods::CTRL), D()), [0x7f]);
        assert_eq!(enc(k('a').mods(Mods::CTRL | Mods::SHIFT), D()), [1]);
    }

    #[test]
    fn ctrl_fallback_cyrillic() {
        let ev = k('с').mods(Mods::CTRL).ctrl_fallback('c');
        assert_eq!(enc(ev, D()), [3]);
        // Without the fallback the character passes through.
        assert_eq!(enc(k('с').mods(Mods::CTRL).text("с"), D()), "с".as_bytes());
    }

    #[test]
    fn alt_prefix_and_1036() {
        assert_eq!(enc(k('x').text("x").mods(Mods::ALT), D()), b"\x1bx");
        assert_eq!(enc(k('x').mods(Mods::ALT | Mods::CTRL), D()), b"\x1b\x18");
        let off = D().with(bit::ALT_ESC, false);
        assert_eq!(enc(k('x').text("x").mods(Mods::ALT), off), b"x");
        assert_eq!(enc(nk(Named::Enter).mods(Mods::ALT), D()), b"\x1b\r");
    }

    #[test]
    fn enter_tab_bs_esc() {
        assert_eq!(enc(nk(Named::Enter), D()), b"\r");
        assert_eq!(enc(nk(Named::Tab), D()), b"\t");
        assert_eq!(enc(nk(Named::Backspace), D()), [0x7f]);
        assert_eq!(enc(nk(Named::Escape), D()), [0x1b]);
        assert_eq!(enc(nk(Named::Escape).mods(Mods::ALT), D()), [0x1b, 0x1b]);
    }

    #[test]
    fn shift_tab() {
        assert_eq!(enc(nk(Named::Tab).mods(Mods::SHIFT), D()), b"\x1b[Z");
        assert_eq!(
            enc(nk(Named::Tab).mods(Mods::SHIFT | Mods::CTRL), D()),
            b"\x1b[1;6Z"
        );
    }

    #[test]
    fn decbkm() {
        let m = D().with(bit::DECBKM, true);
        assert_eq!(enc(nk(Named::Backspace), m), [0x08]);
        assert_eq!(enc(nk(Named::Backspace).mods(Mods::CTRL), m), [0x7f]);
        assert_eq!(enc(nk(Named::Backspace).mods(Mods::CTRL), D()), [0x08]);
    }

    #[test]
    fn cursor_keys_decckm() {
        let app = D().with(bit::DECCKM, true);
        for (n, f) in [
            (Named::Up, 'A'),
            (Named::Down, 'B'),
            (Named::Right, 'C'),
            (Named::Left, 'D'),
            (Named::Home, 'H'),
            (Named::End, 'F'),
        ] {
            assert_eq!(enc(nk(n), D()), format!("\x1b[{f}").as_bytes());
            assert_eq!(enc(nk(n), app), format!("\x1bO{f}").as_bytes());
        }
    }

    #[test]
    fn modified_cursor_keys_always_csi() {
        let app = D().with(bit::DECCKM, true);
        assert_eq!(enc(nk(Named::Up).mods(Mods::SHIFT), app), b"\x1b[1;2A");
        assert_eq!(enc(nk(Named::Left).mods(Mods::CTRL), D()), b"\x1b[1;5D");
        assert_eq!(enc(nk(Named::Right).mods(Mods::ALT), D()), b"\x1b[1;3C");
        assert_eq!(
            enc(
                nk(Named::End).mods(Mods::CTRL | Mods::SHIFT | Mods::ALT),
                D()
            ),
            b"\x1b[1;8F"
        );
        assert_eq!(enc(nk(Named::Home).mods(Mods::META), D()), b"\x1b[1;9H");
        // Lock bits never reach legacy.
        assert_eq!(
            enc(nk(Named::Up).mods(Mods::CAPS | Mods::NUM), D()),
            b"\x1b[A"
        );
    }

    #[test]
    fn tilde_keys() {
        assert_eq!(enc(nk(Named::Insert), D()), b"\x1b[2~");
        assert_eq!(enc(nk(Named::Delete), D()), b"\x1b[3~");
        assert_eq!(enc(nk(Named::PageUp), D()), b"\x1b[5~");
        assert_eq!(enc(nk(Named::PageDown).mods(Mods::CTRL), D()), b"\x1b[6;5~");
        assert_eq!(enc(nk(Named::Delete).mods(Mods::SHIFT), D()), b"\x1b[3;2~");
        assert_eq!(enc(nk(Named::Menu), D()), b"\x1b[29~");
    }

    #[test]
    fn function_keys() {
        let expect: [(u8, &str); 12] = [
            (1, "\x1bOP"),
            (2, "\x1bOQ"),
            (3, "\x1bOR"),
            (4, "\x1bOS"),
            (5, "\x1b[15~"),
            (6, "\x1b[17~"),
            (7, "\x1b[18~"),
            (8, "\x1b[19~"),
            (9, "\x1b[20~"),
            (10, "\x1b[21~"),
            (11, "\x1b[23~"),
            (12, "\x1b[24~"),
        ];
        for (n, s) in expect {
            assert_eq!(enc(nk(Named::F(n)), D()), s.as_bytes(), "F{n}");
        }
        assert_eq!(enc(nk(Named::F(1)).mods(Mods::SHIFT), D()), b"\x1b[1;2P");
        assert_eq!(enc(nk(Named::F(5)).mods(Mods::CTRL), D()), b"\x1b[15;5~");
        assert_eq!(enc(nk(Named::F(13)), D()), b"\x1b[25~");
        assert_eq!(enc(nk(Named::F(20)), D()), b"\x1b[34~");
        assert!(enc(nk(Named::F(21)), D()).is_empty());
        assert!(enc(nk(Named::F(35)), D()).is_empty());
        assert!(enc(nk(Named::F(0)), D()).is_empty());
    }

    #[test]
    fn keypad_numeric_and_application() {
        let app = D().with(bit::DECKPAM, true);
        assert_eq!(enc(nk(Named::Kp(Kp::Digit(5))), D()), b"5");
        assert_eq!(enc(nk(Named::Kp(Kp::Enter)), D()), b"\r");
        assert_eq!(enc(nk(Named::Kp(Kp::Add)), D()), b"+");
        assert_eq!(enc(nk(Named::Kp(Kp::Digit(0))), app), b"\x1bOp");
        assert_eq!(enc(nk(Named::Kp(Kp::Digit(9))), app), b"\x1bOy");
        assert_eq!(enc(nk(Named::Kp(Kp::Enter)), app), b"\x1bOM");
        assert_eq!(enc(nk(Named::Kp(Kp::Decimal)), app), b"\x1bOn");
        assert_eq!(enc(nk(Named::Kp(Kp::Divide)), app), b"\x1bOo");
        assert_eq!(enc(nk(Named::Kp(Kp::Multiply)), app), b"\x1bOj");
        assert_eq!(enc(nk(Named::Kp(Kp::Subtract)), app), b"\x1bOm");
        assert_eq!(enc(nk(Named::Kp(Kp::Add)), app), b"\x1bOk");
        assert_eq!(enc(nk(Named::Kp(Kp::Equal)), app), b"\x1bOX");
        assert_eq!(enc(nk(Named::Kp(Kp::Separator)), app), b"\x1bOl");
        assert_eq!(
            enc(nk(Named::Kp(Kp::Digit(0))).mods(Mods::SHIFT), app),
            b"\x1bO2p"
        );
        assert_eq!(enc(nk(Named::Kp(Kp::Up)), D()), b"\x1b[A");
        assert_eq!(enc(nk(Named::Kp(Kp::Begin)), D()), b"\x1b[E");
        assert_eq!(enc(nk(Named::Kp(Kp::Delete)), D()), b"\x1b[3~");
    }

    #[test]
    fn no_legacy_form_keys_send_nothing() {
        for n in [
            Named::CapsLock,
            Named::NumLock,
            Named::ScrollLock,
            Named::PrintScreen,
            Named::Pause,
            Named::Pua(57441),
        ] {
            assert!(enc(nk(n), D()).is_empty());
        }
    }

    #[test]
    fn modify_other_keys() {
        let l1 = D().with_mok_level(1);
        let l2 = D().with_mok_level(2);
        // Level 1: only ctrl combos without a legacy byte.
        assert_eq!(enc(k('1').mods(Mods::CTRL), l1), b"\x1b[27;5;49~");
        assert_eq!(enc(k('a').mods(Mods::CTRL), l1), [1]);
        assert_eq!(enc(k(';').mods(Mods::CTRL), l1), b"\x1b[27;5;59~");
        // Level 2: every ctrl/alt-modified key.
        assert_eq!(enc(k('a').mods(Mods::CTRL), l2), b"\x1b[27;5;97~");
        assert_eq!(enc(k('a').text("a").mods(Mods::ALT), l2), b"\x1b[27;3;97~");
        assert_eq!(
            enc(k('a').mods(Mods::CTRL | Mods::SHIFT).shifted('A'), l2),
            b"\x1b[27;6;65~"
        );
        assert_eq!(enc(k('a').text("A").mods(Mods::SHIFT), l2), b"A");
        assert_eq!(enc(nk(Named::Enter).mods(Mods::CTRL), l2), b"\x1b[27;5;13~");
        assert_eq!(enc(nk(Named::Enter), l2), b"\r");
        // formatOtherKeys.
        let f = l2.with(bit::FORMAT_OTHER_KEYS, true);
        assert_eq!(enc(k('a').mods(Mods::CTRL), f), b"\x1b[97;5u");
        assert_eq!(enc(nk(Named::Tab).mods(Mods::CTRL), f), b"\x1b[9;5u");
    }

    #[test]
    fn output_bounded_for_huge_text() {
        let big = "é".repeat(100);
        let n = enc(k('e').text(&big), D());
        assert!(n.len() <= 64);
        assert!(core::str::from_utf8(&n).is_ok());
    }
}
