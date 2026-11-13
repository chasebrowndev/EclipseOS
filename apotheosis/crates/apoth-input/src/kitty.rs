// SPDX-License-Identifier: Apache-2.0
//! Kitty keyboard protocol: encoder, functional-key PUA table, and the per-screen flag
//! stack data types (INPUT-01, -03, -04, -05, -48).
//!
//! The parser-side handling of `CSI = f;m u`, `CSI > f u`, `CSI < n u`, `CSI ? u` is
//! `apoth-term`'s (T-03); it drives [`KittyState`] and publishes `current()` to the mirror.

use crate::buf::{W, digits};
use crate::key::{Key, KeyEvent, KeyKind, Kp, Mods, Named};
use crate::legacy;
use crate::modes::InputModes;

/// Flag bits.
pub mod flags {
    /// Disambiguate escape codes.
    pub const DISAMBIGUATE: u32 = 1;
    /// Report event types (press/repeat/release).
    pub const EVENT_TYPES: u32 = 2;
    /// Report alternate keys.
    pub const ALTERNATE_KEYS: u32 = 4;
    /// Report all keys as escape codes.
    pub const ALL_KEYS: u32 = 8;
    /// Report associated text.
    pub const ASSOCIATED_TEXT: u32 = 16;
    /// All defined bits.
    pub const ALL: u32 = 31;
}

/// Functional-key private-use codepoints (INPUT-03); range 57344..=63743.
pub mod pua {
    /// First codepoint of the kitty functional-key range.
    pub const FIRST: u32 = 57344;
    /// Last codepoint of the range.
    pub const LAST: u32 = 63743;
    /// Caps Lock.
    pub const CAPS_LOCK: u32 = 57358;
    /// Scroll Lock.
    pub const SCROLL_LOCK: u32 = 57359;
    /// Num Lock.
    pub const NUM_LOCK: u32 = 57360;
    /// Print Screen.
    pub const PRINT_SCREEN: u32 = 57361;
    /// Pause.
    pub const PAUSE: u32 = 57362;
    /// Menu.
    pub const MENU: u32 = 57363;
    /// F13; F13..F35 are `F13 + (n - 13)`.
    pub const F13: u32 = 57376;
    /// KP_0; KP_0..KP_9 are `KP_0 + d`.
    pub const KP_0: u32 = 57399;
    /// KP_DECIMAL.
    pub const KP_DECIMAL: u32 = 57409;
    /// KP_DIVIDE.
    pub const KP_DIVIDE: u32 = 57410;
    /// KP_MULTIPLY.
    pub const KP_MULTIPLY: u32 = 57411;
    /// KP_SUBTRACT.
    pub const KP_SUBTRACT: u32 = 57412;
    /// KP_ADD.
    pub const KP_ADD: u32 = 57413;
    /// KP_ENTER.
    pub const KP_ENTER: u32 = 57414;
    /// KP_EQUAL.
    pub const KP_EQUAL: u32 = 57415;
    /// KP_SEPARATOR.
    pub const KP_SEPARATOR: u32 = 57416;
    /// KP_LEFT.
    pub const KP_LEFT: u32 = 57417;
    /// KP_RIGHT.
    pub const KP_RIGHT: u32 = 57418;
    /// KP_UP.
    pub const KP_UP: u32 = 57419;
    /// KP_DOWN.
    pub const KP_DOWN: u32 = 57420;
    /// KP_PAGE_UP.
    pub const KP_PAGE_UP: u32 = 57421;
    /// KP_PAGE_DOWN.
    pub const KP_PAGE_DOWN: u32 = 57422;
    /// KP_HOME.
    pub const KP_HOME: u32 = 57423;
    /// KP_END.
    pub const KP_END: u32 = 57424;
    /// KP_INSERT.
    pub const KP_INSERT: u32 = 57425;
    /// KP_DELETE.
    pub const KP_DELETE: u32 = 57426;
    /// KP_BEGIN.
    pub const KP_BEGIN: u32 = 57427;
    /// MEDIA_PLAY.
    pub const MEDIA_PLAY: u32 = 57428;
    /// MEDIA_PAUSE.
    pub const MEDIA_PAUSE: u32 = 57429;
    /// MEDIA_PLAY_PAUSE.
    pub const MEDIA_PLAY_PAUSE: u32 = 57430;
    /// MEDIA_REVERSE.
    pub const MEDIA_REVERSE: u32 = 57431;
    /// MEDIA_STOP.
    pub const MEDIA_STOP: u32 = 57432;
    /// MEDIA_FAST_FORWARD.
    pub const MEDIA_FAST_FORWARD: u32 = 57433;
    /// MEDIA_REWIND.
    pub const MEDIA_REWIND: u32 = 57434;
    /// MEDIA_TRACK_NEXT.
    pub const MEDIA_TRACK_NEXT: u32 = 57435;
    /// MEDIA_TRACK_PREVIOUS.
    pub const MEDIA_TRACK_PREVIOUS: u32 = 57436;
    /// MEDIA_RECORD.
    pub const MEDIA_RECORD: u32 = 57437;
    /// LOWER_VOLUME.
    pub const LOWER_VOLUME: u32 = 57438;
    /// RAISE_VOLUME.
    pub const RAISE_VOLUME: u32 = 57439;
    /// MUTE_VOLUME.
    pub const MUTE_VOLUME: u32 = 57440;
    /// LEFT_SHIFT.
    pub const LEFT_SHIFT: u32 = 57441;
    /// LEFT_CONTROL.
    pub const LEFT_CONTROL: u32 = 57442;
    /// LEFT_ALT.
    pub const LEFT_ALT: u32 = 57443;
    /// LEFT_SUPER.
    pub const LEFT_SUPER: u32 = 57444;
    /// LEFT_HYPER.
    pub const LEFT_HYPER: u32 = 57445;
    /// LEFT_META.
    pub const LEFT_META: u32 = 57446;
    /// RIGHT_SHIFT.
    pub const RIGHT_SHIFT: u32 = 57447;
    /// RIGHT_CONTROL.
    pub const RIGHT_CONTROL: u32 = 57448;
    /// RIGHT_ALT.
    pub const RIGHT_ALT: u32 = 57449;
    /// RIGHT_SUPER.
    pub const RIGHT_SUPER: u32 = 57450;
    /// RIGHT_HYPER.
    pub const RIGHT_HYPER: u32 = 57451;
    /// RIGHT_META.
    pub const RIGHT_META: u32 = 57452;
    /// ISO_LEVEL3_SHIFT.
    pub const ISO_LEVEL3_SHIFT: u32 = 57453;
    /// ISO_LEVEL5_SHIFT.
    pub const ISO_LEVEL5_SHIFT: u32 = 57454;

    /// Is `c` one of the modifier-only keys (LEFT_SHIFT..ISO_LEVEL5_SHIFT).
    pub fn is_modifier_key(c: u32) -> bool {
        (LEFT_SHIFT..=ISO_LEVEL5_SHIFT).contains(&c)
    }
}

/// How a key is spelled in kitty form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// `CSI number u`.
    U(u32),
    /// `CSI number ; mods ~`.
    Tilde(u32),
    /// `CSI 1 ; mods X`.
    Letter(u8),
}

/// The kitty form of a functional key; `None` for keys the protocol cannot spell
/// (F0, F36+, PUA codes outside the range).
pub fn form_of(n: Named) -> Option<Form> {
    use pua::*;
    Some(match n {
        Named::Escape => Form::U(27),
        Named::Enter => Form::U(13),
        Named::Tab => Form::U(9),
        Named::Backspace => Form::U(127),
        Named::Insert => Form::Tilde(2),
        Named::Delete => Form::Tilde(3),
        Named::PageUp => Form::Tilde(5),
        Named::PageDown => Form::Tilde(6),
        Named::Left => Form::Letter(b'D'),
        Named::Right => Form::Letter(b'C'),
        Named::Up => Form::Letter(b'A'),
        Named::Down => Form::Letter(b'B'),
        Named::Home => Form::Letter(b'H'),
        Named::End => Form::Letter(b'F'),
        Named::F(k) => match k {
            1 => Form::Letter(b'P'),
            2 => Form::Letter(b'Q'),
            3 => Form::Tilde(13),
            4 => Form::Letter(b'S'),
            5 => Form::Tilde(15),
            6 => Form::Tilde(17),
            7 => Form::Tilde(18),
            8 => Form::Tilde(19),
            9 => Form::Tilde(20),
            10 => Form::Tilde(21),
            11 => Form::Tilde(23),
            12 => Form::Tilde(24),
            13..=35 => Form::U(F13 + u32::from(k) - 13),
            _ => return None,
        },
        Named::Menu => Form::U(MENU),
        Named::CapsLock => Form::U(CAPS_LOCK),
        Named::ScrollLock => Form::U(SCROLL_LOCK),
        Named::NumLock => Form::U(NUM_LOCK),
        Named::PrintScreen => Form::U(PRINT_SCREEN),
        Named::Pause => Form::U(PAUSE),
        Named::Kp(k) => Form::U(match k {
            Kp::Digit(d) => KP_0 + u32::from(d.min(9)),
            Kp::Decimal => KP_DECIMAL,
            Kp::Divide => KP_DIVIDE,
            Kp::Multiply => KP_MULTIPLY,
            Kp::Subtract => KP_SUBTRACT,
            Kp::Add => KP_ADD,
            Kp::Enter => KP_ENTER,
            Kp::Equal => KP_EQUAL,
            Kp::Separator => KP_SEPARATOR,
            Kp::Left => KP_LEFT,
            Kp::Right => KP_RIGHT,
            Kp::Up => KP_UP,
            Kp::Down => KP_DOWN,
            Kp::PageUp => KP_PAGE_UP,
            Kp::PageDown => KP_PAGE_DOWN,
            Kp::Home => KP_HOME,
            Kp::End => KP_END,
            Kp::Insert => KP_INSERT,
            Kp::Delete => KP_DELETE,
            Kp::Begin => KP_BEGIN,
        }),
        Named::Pua(c) if (FIRST..=LAST).contains(&c) => Form::U(c),
        Named::Pua(_) => return None,
    })
}

fn is_text(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c >= ' ' && c != '\u{7f}' && !('\u{80}'..='\u{9f}').contains(&c))
}

pub(crate) fn encode(ev: &KeyEvent<'_>, modes: InputModes, w: &mut W<'_>) {
    let f = modes.kitty_flags() & flags::ALL;
    let all = f & flags::ALL_KEYS != 0;
    let events = f & flags::EVENT_TYPES != 0;
    let release = ev.kind == KeyKind::Release;
    if release && !events {
        return;
    }
    if !all && f & flags::DISAMBIGUATE == 0 {
        // Flags 2/4/16 alone do not change how keys are spelled.
        legacy::encode(ev, modes, w);
        return;
    }
    let eff = ev.mods.without_locks();
    let event = match (events, ev.kind) {
        (false, _) | (_, KeyKind::Press) => 1,
        (true, KeyKind::Repeat) => 2,
        (true, KeyKind::Release) => 3,
    };
    match ev.key {
        Key::Char(c) => {
            let text_key = eff.0 & !Mods::SHIFT.0 == 0 && !ev.text.is_empty();
            if !all && text_key {
                // Plain text; repeat is text, release is not reported.
                if !release {
                    w.str_fit(ev.text);
                }
                return;
            }
            let shifted = if f & flags::ALTERNATE_KEYS != 0 && eff.contains(Mods::SHIFT) {
                ev.shifted.filter(|s| *s != c)
            } else {
                None
            };
            let base = if f & flags::ALTERNATE_KEYS != 0 {
                ev.base.filter(|b| *b != c)
            } else {
                None
            };
            let text = if f & flags::ASSOCIATED_TEXT != 0 && !release && is_text(ev.text) {
                ev.text
            } else {
                ""
            };
            emit(w, Form::U(c as u32), shifted, base, ev.mods, event, text);
        }
        Key::Named(n) => {
            let legacy_key = matches!(n, Named::Enter | Named::Tab | Named::Backspace);
            if legacy_key && !all && eff.is_empty() {
                if !release {
                    legacy::encode(ev, modes, w);
                }
                return;
            }
            if let Named::Pua(c) = n
                && pua::is_modifier_key(c)
                && !all
            {
                return;
            }
            if let Some(form) = form_of(n) {
                emit(w, form, None, None, ev.mods, event, "");
            }
        }
    }
}

fn emit(
    w: &mut W<'_>,
    form: Form,
    shifted: Option<char>,
    base: Option<char>,
    mods: Mods,
    event: u32,
    text: &str,
) {
    let m = u32::from(mods.0);
    let has_mods = m != 0 || event != 1;
    let has_text = !text.is_empty();
    w.csi();
    match form {
        Form::U(n) => {
            w.num(n);
            if shifted.is_some() || base.is_some() {
                w.byte(b':');
                if let Some(s) = shifted {
                    w.num(s as u32);
                }
                if let Some(b) = base {
                    w.byte(b':');
                    w.num(b as u32);
                }
            }
        }
        Form::Tilde(n) => w.num(n),
        Form::Letter(_) => {
            if has_mods {
                w.byte(b'1');
            }
        }
    }
    if has_mods || has_text {
        w.byte(b';');
        if has_mods {
            w.num(m + 1);
            if event != 1 {
                w.byte(b':');
                w.num(event);
            }
        }
        if has_text {
            w.byte(b';');
            let mut first = true;
            for ch in text.chars() {
                let need = digits(ch as u32) + usize::from(!first);
                if w.room() < need + 1 {
                    break;
                }
                if !first {
                    w.byte(b':');
                }
                w.num(ch as u32);
                first = false;
            }
        }
    }
    w.byte(match form {
        Form::U(_) => b'u',
        Form::Tilde(_) => b'~',
        Form::Letter(l) => l,
    });
}

/// Encode IME-committed text under kitty flags 8+16 as `CSI 0 ; ; cp:cp u`, appending to
/// `out`; returns `false` when the flags do not call for it (caller sends UTF-8).
pub(crate) fn encode_text(
    text: impl Iterator<Item = char>,
    modes: InputModes,
    out: &mut Vec<u8>,
) -> bool {
    let f = modes.kitty_flags();
    if f & flags::ALL_KEYS == 0 || f & flags::ASSOCIATED_TEXT == 0 {
        return false;
    }
    out.extend_from_slice(b"\x1b[0;;");
    let mut first = true;
    for ch in text {
        if !first {
            out.push(b':');
        }
        let mut t = [0u8; 10];
        let mut i = t.len();
        let mut v = ch as u32;
        loop {
            i -= 1;
            t[i] = b'0' + (v % 10) as u8;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        out.extend_from_slice(&t[i..]);
        first = false;
    }
    out.push(b'u');
    true
}

/// Stack depth per screen (INPUT-01).
pub const STACK_DEPTH: usize = 8;

/// One screen's flag stack: push on full evicts the oldest, pop on empty resets to 0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KittyFlagStack {
    entries: [u8; STACK_DEPTH],
    len: u8,
}

impl KittyFlagStack {
    /// Active flags (top of stack; 0 when empty).
    pub const fn current(&self) -> u32 {
        if self.len == 0 {
            0
        } else {
            self.entries[self.len as usize - 1] as u32
        }
    }
    /// Number of entries.
    pub const fn depth(&self) -> usize {
        self.len as usize
    }
    /// `CSI > f u`: push, evicting the oldest entry when full.
    pub fn push(&mut self, f: u32) {
        let f = (f & flags::ALL) as u8;
        if self.len as usize == STACK_DEPTH {
            self.entries.copy_within(1.., 0);
            self.entries[STACK_DEPTH - 1] = f;
        } else {
            self.entries[self.len as usize] = f;
            self.len += 1;
        }
    }
    /// `CSI < n u`: pop `n` entries (`n == 0` is 1). Popping more than are present resets
    /// to the empty stack, flags 0.
    pub fn pop(&mut self, n: u32) {
        let n = if n == 0 { 1 } else { n };
        if n as usize >= self.len as usize {
            self.len = 0;
        } else {
            self.len -= n as u8;
        }
    }
    /// `CSI = f ; m u`: mode 1 replaces, 2 sets bits, 3 clears bits. Other modes are
    /// ignored. On an empty stack the base entry is created.
    pub fn set(&mut self, f: u32, mode: u32) {
        let f = (f & flags::ALL) as u8;
        if !(1..=3).contains(&mode) {
            return;
        }
        if self.len == 0 {
            self.len = 1;
            self.entries[0] = 0;
        }
        let top = &mut self.entries[self.len as usize - 1];
        match mode {
            1 => *top = f,
            2 => *top |= f,
            _ => *top &= !f,
        }
    }
    /// Reset (RIS, DECSTR is not specified to clear it).
    pub fn clear(&mut self) {
        self.len = 0;
    }
}

/// Main and alternate stacks, swapped on screen switch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KittyState {
    main: KittyFlagStack,
    alt: KittyFlagStack,
    on_alt: bool,
}

impl KittyState {
    /// Stack of the active screen.
    pub fn active(&self) -> &KittyFlagStack {
        if self.on_alt { &self.alt } else { &self.main }
    }
    /// Mutable stack of the active screen.
    pub fn active_mut(&mut self) -> &mut KittyFlagStack {
        if self.on_alt {
            &mut self.alt
        } else {
            &mut self.main
        }
    }
    /// Switch screens; the target screen's stack is kept as it was.
    pub fn switch_screen(&mut self, alt: bool) {
        self.on_alt = alt;
    }
    /// Flags to publish to the mirror.
    pub fn current(&self) -> u32 {
        self.active().current()
    }
    /// `CSI ? u` reply: `CSI ? flags u`.
    pub fn query_reply(&self, out: &mut [u8; 16]) -> usize {
        let mut w = W::new(out);
        w.bytes(b"\x1b[?");
        w.num(self.current());
        w.byte(b'u');
        w.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode_key;
    use crate::key::KeyKind::{Press, Release, Repeat};

    fn enc(ev: KeyEvent<'_>, f: u32) -> String {
        let mut out = [0u8; 64];
        let n = encode_key(&ev, InputModes::default().with_kitty(f), &mut out);
        String::from_utf8_lossy(&out[..n]).into_owned()
    }
    fn k(c: char) -> KeyEvent<'static> {
        KeyEvent::new(Key::Char(c))
    }
    fn nk(n: Named) -> KeyEvent<'static> {
        KeyEvent::new(Key::Named(n))
    }
    const D: u32 = flags::DISAMBIGUATE;
    const E: u32 = flags::EVENT_TYPES;
    const A: u32 = flags::ALTERNATE_KEYS;
    const ALL: u32 = flags::ALL_KEYS;
    const T: u32 = flags::ASSOCIATED_TEXT;

    #[test]
    fn disambiguate_basics() {
        assert_eq!(enc(nk(Named::Escape), D), "\x1b[27u");
        assert_eq!(enc(nk(Named::Escape).mods(Mods::ALT), D), "\x1b[27;3u");
        assert_eq!(enc(k('a').text("a"), D), "a");
        assert_eq!(enc(k('a').text("A").mods(Mods::SHIFT), D), "A");
        assert_eq!(enc(k('a').mods(Mods::CTRL), D), "\x1b[97;5u");
        assert_eq!(enc(k('a').text("a").mods(Mods::ALT), D), "\x1b[97;3u");
        assert_eq!(
            enc(
                k('i').mods(Mods::CTRL | Mods::ALT | Mods::SHIFT | Mods::SUPER),
                D
            ),
            "\x1b[105;16u"
        );
        assert_eq!(enc(k(' ').mods(Mods::CTRL), D), "\x1b[32;5u");
    }

    #[test]
    fn locks_do_not_force_csi_u_for_text() {
        assert_eq!(enc(k('a').text("A").mods(Mods::CAPS), D), "A");
        assert_eq!(
            enc(k('a').mods(Mods::CTRL | Mods::CAPS | Mods::NUM), D),
            "\x1b[97;197u"
        );
        assert_eq!(enc(nk(Named::Up).mods(Mods::NUM), D), "\x1b[1;129A");
    }

    #[test]
    fn legacy_trio_keeps_bytes() {
        assert_eq!(enc(nk(Named::Enter), D), "\r");
        assert_eq!(enc(nk(Named::Tab), D), "\t");
        assert_eq!(enc(nk(Named::Backspace), D), "\x7f");
        // Any modifier (Alt included) takes the CSI u form once disambiguate is on.
        assert_eq!(enc(nk(Named::Enter).mods(Mods::ALT), D), "\x1b[13;3u");
        assert_eq!(enc(nk(Named::Tab).mods(Mods::ALT), D), "\x1b[9;3u");
        assert_eq!(enc(nk(Named::Backspace).mods(Mods::ALT), D), "\x1b[127;3u");
        assert_eq!(enc(nk(Named::Enter).mods(Mods::SHIFT), D), "\x1b[13;2u");
        assert_eq!(enc(nk(Named::Tab).mods(Mods::SHIFT), D), "\x1b[9;2u");
        assert_eq!(enc(nk(Named::Backspace).mods(Mods::CTRL), D), "\x1b[127;5u");
    }

    #[test]
    fn trio_release_only_under_flag_8() {
        for n in [Named::Enter, Named::Tab, Named::Backspace] {
            assert_eq!(enc(nk(n).kind(Release), D | E), "");
        }
        assert_eq!(
            enc(nk(Named::Enter).kind(Release), D | E | ALL),
            "\x1b[13;1:3u"
        );
        assert_eq!(
            enc(nk(Named::Tab).kind(Release), D | E | ALL),
            "\x1b[9;1:3u"
        );
        assert_eq!(
            enc(nk(Named::Backspace).kind(Release), D | E | ALL),
            "\x1b[127;1:3u"
        );
    }

    #[test]
    fn functional_keys() {
        assert_eq!(enc(nk(Named::Up), D), "\x1b[A");
        assert_eq!(enc(nk(Named::Up).mods(Mods::CTRL), D), "\x1b[1;5A");
        assert_eq!(enc(nk(Named::F(1)), D), "\x1b[P");
        assert_eq!(enc(nk(Named::F(1)).mods(Mods::SHIFT), D), "\x1b[1;2P");
        assert_eq!(enc(nk(Named::F(3)), D), "\x1b[13~");
        assert_eq!(enc(nk(Named::F(3)).mods(Mods::ALT), D), "\x1b[13;3~");
        assert_eq!(enc(nk(Named::F(4)), D), "\x1b[S");
        assert_eq!(enc(nk(Named::F(5)), D), "\x1b[15~");
        assert_eq!(enc(nk(Named::F(12)), D), "\x1b[24~");
        assert_eq!(enc(nk(Named::Insert), D), "\x1b[2~");
        assert_eq!(enc(nk(Named::PageDown), D), "\x1b[6~");
        assert_eq!(enc(nk(Named::Home), D), "\x1b[H");
        assert_eq!(enc(nk(Named::End), D), "\x1b[F");
        // DECCKM is ignored: SS3 becomes CSI.
        let m = InputModes::default()
            .with(crate::modes::bit::DECCKM, true)
            .with_kitty(D);
        let mut out = [0u8; 64];
        let n = encode_key(&nk(Named::Up), m, &mut out);
        assert_eq!(&out[..n], b"\x1b[A");
    }

    #[test]
    fn pua_table() {
        assert_eq!(enc(nk(Named::F(13)), D), "\x1b[57376u");
        assert_eq!(enc(nk(Named::F(35)), D), "\x1b[57398u");
        assert_eq!(enc(nk(Named::CapsLock), D), "\x1b[57358u");
        assert_eq!(enc(nk(Named::ScrollLock), D), "\x1b[57359u");
        assert_eq!(enc(nk(Named::NumLock), D), "\x1b[57360u");
        assert_eq!(enc(nk(Named::PrintScreen), D), "\x1b[57361u");
        assert_eq!(enc(nk(Named::Pause), D), "\x1b[57362u");
        assert_eq!(enc(nk(Named::Menu), D), "\x1b[57363u");
        assert_eq!(enc(nk(Named::Kp(Kp::Digit(0))), D), "\x1b[57399u");
        assert_eq!(enc(nk(Named::Kp(Kp::Digit(9))), D), "\x1b[57408u");
        assert_eq!(enc(nk(Named::Kp(Kp::Decimal)), D), "\x1b[57409u");
        assert_eq!(enc(nk(Named::Kp(Kp::Enter)), D), "\x1b[57414u");
        assert_eq!(enc(nk(Named::Kp(Kp::Begin)), D), "\x1b[57427u");
        assert_eq!(enc(nk(Named::Pua(pua::MEDIA_PLAY)), D), "\x1b[57428u");
        assert_eq!(enc(nk(Named::Pua(pua::MUTE_VOLUME)), D), "\x1b[57440u");
        assert_eq!(
            enc(nk(Named::Pua(pua::ISO_LEVEL5_SHIFT)), D | ALL),
            "\x1b[57454u"
        );
        assert_eq!(enc(nk(Named::Pua(70000)), D), "");
        assert_eq!(enc(nk(Named::F(36)), D), "");
        assert_eq!(enc(nk(Named::F(0)), D), "");
    }

    #[test]
    fn modifier_only_keys_need_flag_8() {
        let ev = nk(Named::Pua(pua::LEFT_SHIFT)).mods(Mods::SHIFT);
        assert_eq!(enc(ev, D), "");
        assert_eq!(enc(ev, D | ALL), "\x1b[57441;2u");
        let ev = nk(Named::Pua(pua::LEFT_CONTROL))
            .mods(Mods::CTRL)
            .kind(Release);
        assert_eq!(enc(ev, D | E | ALL), "\x1b[57442;5:3u");
        for c in 57441..=57454 {
            assert!(pua::is_modifier_key(c));
        }
        assert!(!pua::is_modifier_key(57440) && !pua::is_modifier_key(57455));
    }

    #[test]
    fn event_types() {
        assert_eq!(
            enc(k('a').text("a").kind(Repeat), D | E | ALL),
            "\x1b[97;1:2u"
        );
        assert_eq!(
            enc(k('a').text("a").kind(Release), D | E | ALL),
            "\x1b[97;1:3u"
        );
        assert_eq!(enc(k('a').text("a"), D | E | ALL), "\x1b[97u");
        assert_eq!(
            enc(k('a').mods(Mods::CTRL).kind(Release), D | E),
            "\x1b[97;5:3u"
        );
        assert_eq!(enc(nk(Named::Up).kind(Release), D | E), "\x1b[1;1:3A");
        assert_eq!(
            enc(nk(Named::Up).kind(Repeat).mods(Mods::SHIFT), D | E),
            "\x1b[1;2:2A"
        );
        assert_eq!(enc(nk(Named::F(3)).kind(Release), D | E), "\x1b[13;1:3~");
        // Text keys: release is not reported without flag 8.
        assert_eq!(enc(k('a').text("a").kind(Release), D | E), "");
        // Without flag 2 release is never reported, repeat is a press.
        assert_eq!(enc(k('a').mods(Mods::CTRL).kind(Release), D), "");
        assert_eq!(enc(k('a').mods(Mods::CTRL).kind(Repeat), D), "\x1b[97;5u");
        assert_eq!(
            enc(k('a').mods(Mods::CTRL).kind(Press), D | E),
            "\x1b[97;5u"
        );
    }

    #[test]
    fn all_keys_as_escapes() {
        assert_eq!(enc(k('a').text("a"), D | ALL), "\x1b[97u");
        assert_eq!(
            enc(k('a').text("A").mods(Mods::SHIFT), D | ALL),
            "\x1b[97;2u"
        );
        assert_eq!(enc(nk(Named::Enter), D | ALL), "\x1b[13u");
        assert_eq!(enc(nk(Named::Tab), D | ALL), "\x1b[9u");
        assert_eq!(enc(nk(Named::Backspace), D | ALL), "\x1b[127u");
        assert_eq!(enc(nk(Named::Escape), D | ALL), "\x1b[27u");
        assert_eq!(enc(nk(Named::Escape), ALL), "\x1b[27u");
    }

    #[test]
    fn alternate_keys() {
        let ev = k('a').text("A").mods(Mods::SHIFT).shifted('A');
        assert_eq!(enc(ev, D | ALL | A), "\x1b[97:65;2u");
        // Shifted key only while Shift is held.
        assert_eq!(enc(k('a').shifted('A'), D | ALL | A), "\x1b[97u");
        // Base layout key, only when it differs.
        let ev = k('с').base('c').mods(Mods::CTRL);
        assert_eq!(enc(ev, D | A), "\x1b[1089::99;5u");
        assert_eq!(enc(k('c').base('c').mods(Mods::CTRL), D | A), "\x1b[99;5u");
        // Both.
        let ev = k('й').shifted('Й').base('q').mods(Mods::SHIFT | Mods::CTRL);
        assert_eq!(enc(ev, D | A), "\x1b[1081:1049:113;6u");
        // Not reported without flag 4.
        assert_eq!(enc(ev, D), "\x1b[1081;6u");
    }

    #[test]
    fn associated_text() {
        assert_eq!(
            enc(k('a').text("A").mods(Mods::SHIFT), D | ALL | T),
            "\x1b[97;2;65u"
        );
        assert_eq!(enc(k('a').text("a"), D | ALL | T), "\x1b[97;;97u");
        assert_eq!(
            enc(
                k('a').text("a").shifted('A').mods(Mods::SHIFT),
                D | ALL | T | A
            ),
            "\x1b[97:65;2;97u"
        );
        assert_eq!(enc(k('e').text("é"), D | ALL | T), "\x1b[101;;233u");
        assert_eq!(enc(k('x').text("ab"), D | ALL | T), "\x1b[120;;97:98u");
        // No text on release, none for control text, none without flag 16.
        assert_eq!(
            enc(k('a').text("a").kind(Release), D | E | ALL | T),
            "\x1b[97;1:3u"
        );
        assert_eq!(enc(k('a').text("\x01"), D | ALL | T), "\x1b[97u");
        assert_eq!(enc(k('a').text("a"), D | ALL), "\x1b[97u");
        assert_eq!(
            enc(k('a').text("a").kind(Repeat), D | E | ALL | T),
            "\x1b[97;1:2;97u"
        );
        // Without flag 8 text is sent as text.
        assert_eq!(enc(k('a').text("a"), D | T), "a");
    }

    #[test]
    fn flags_without_disambiguate_or_all_use_legacy() {
        assert_eq!(enc(k('a').mods(Mods::CTRL), E), "\x01");
        assert_eq!(enc(nk(Named::Up), A | T), "\x1b[A");
        assert_eq!(enc(k('a').text("a").kind(Release), E), "");
    }

    #[test]
    fn output_is_bounded() {
        let long = "x".repeat(200);
        let n = enc(k('x').text(&long), D | ALL | T | A);
        assert!(n.len() <= 64 && n.ends_with('u'), "{n}");
        let long = "é".repeat(50);
        let n = enc(k('e').text(&long), D | ALL | T);
        assert!(n.len() <= 64 && n.ends_with('u'));
    }

    #[test]
    fn exhaustive_no_panic_bounded() {
        let keys = [
            Key::Char('a'),
            Key::Char('\u{10ffff}'),
            Key::Named(Named::F(35)),
            Key::Named(Named::Pua(63743)),
            Key::Named(Named::Kp(Kp::Digit(255))),
            Key::Named(Named::Enter),
            Key::Named(Named::Up),
        ];
        for key in keys {
            for mods in 0..=255u8 {
                for f in 0..=31u32 {
                    for kind in [Press, Repeat, Release] {
                        let ev = KeyEvent::new(key)
                            .mods(Mods(mods))
                            .kind(kind)
                            .text("é")
                            .shifted('\u{10ffff}')
                            .base('z');
                        let mut out = [0u8; 64];
                        let m = InputModes::default().with_kitty(f);
                        let n = encode_key(&ev, m, &mut out);
                        assert!(n <= 64);
                        let ev2 = ev.text("");
                        let n = encode_key(&ev2, m.with_mok_level(2), &mut out);
                        assert!(n <= 64);
                    }
                }
            }
        }
    }

    #[test]
    fn stack_push_pop() {
        let mut s = KittyFlagStack::default();
        assert_eq!(s.current(), 0);
        s.push(1);
        s.push(5);
        assert_eq!((s.current(), s.depth()), (5, 2));
        s.pop(1);
        assert_eq!(s.current(), 1);
        s.pop(1);
        assert_eq!((s.current(), s.depth()), (0, 0));
        // Pop on empty stays at zero.
        s.pop(1);
        s.pop(0);
        assert_eq!(s.current(), 0);
        // Pop count 0 means 1.
        s.push(3);
        s.push(7);
        s.pop(0);
        assert_eq!(s.current(), 3);
        // Pop more than present resets.
        s.push(9);
        s.pop(100);
        assert_eq!((s.current(), s.depth()), (0, 0));
        // Flags are masked to the defined bits.
        s.push(0xff);
        assert_eq!(s.current(), 31);
    }

    #[test]
    fn stack_evicts_oldest_when_full() {
        let mut s = KittyFlagStack::default();
        for i in 1..=8u32 {
            s.push(i);
        }
        assert_eq!(s.depth(), 8);
        s.push(16);
        assert_eq!((s.current(), s.depth()), (16, 8));
        // Oldest (1) is gone: the bottom entry is 2, and popping it empties the stack.
        for expect in [8, 7, 6, 5, 4, 3, 2] {
            s.pop(1);
            assert_eq!(s.current(), expect);
        }
        s.pop(1);
        assert_eq!((s.current(), s.depth()), (0, 0));
    }

    #[test]
    fn stack_set_modes() {
        let mut s = KittyFlagStack::default();
        s.set(1, 1);
        assert_eq!(s.current(), 1);
        s.set(4, 2);
        assert_eq!(s.current(), 5);
        s.set(1, 3);
        assert_eq!(s.current(), 4);
        s.set(31, 1);
        assert_eq!(s.current(), 31);
        s.set(0, 7);
        assert_eq!(s.current(), 31);
        assert_eq!(s.depth(), 1);
        s.clear();
        assert_eq!(s.current(), 0);
    }

    #[test]
    fn screens_are_separate() {
        let mut st = KittyState::default();
        st.active_mut().push(1);
        st.switch_screen(true);
        assert_eq!(st.current(), 0);
        st.active_mut().push(8);
        assert_eq!(st.current(), 8);
        st.switch_screen(false);
        assert_eq!(st.current(), 1);
        st.switch_screen(true);
        assert_eq!(st.current(), 8);
    }

    #[test]
    fn query_reply() {
        let mut st = KittyState::default();
        let mut out = [0u8; 16];
        let n = st.query_reply(&mut out);
        assert_eq!(&out[..n], b"\x1b[?0u");
        st.active_mut().push(27);
        let n = st.query_reply(&mut out);
        assert_eq!(&out[..n], b"\x1b[?27u");
    }

    #[test]
    fn ime_text() {
        let mut v = Vec::new();
        let m = InputModes::default().with_kitty(ALL | T);
        assert!(encode_text("日本".chars(), m, &mut v));
        assert_eq!(v, b"\x1b[0;;26085:26412u");
        assert!(!encode_text(
            "a".chars(),
            InputModes::default().with_kitty(D),
            &mut v
        ));
    }
}
