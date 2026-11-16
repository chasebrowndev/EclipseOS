// SPDX-License-Identifier: Apache-2.0
//! The `kbvm` [`KeyboardBackend`] (T-05 §1, ADR 0125, INPUT-61..-71), cargo feature `kbvm`.
//!
//! The `wl_keyboard.keymap` text is compiled once into a stateless `LookupTable`;
//! `wl_keyboard.modifiers` updates the components; `lookup` is a table walk that never
//! allocates. Keymap text and diagnostics are never logged or kept (T-05 §7): the
//! diagnostic handler discards everything and errors carry a static reason.
//!
//! Not in this module: Compose (INPUT-72..-76; needs the system X11 locale data, built
//! lazily off the main thread per T-05 §1) and the `xkbcommon` differential oracle
//! (T-05 §8, class CI-N, needs system xkb data). Dead keys and Multi_key therefore
//! produce no event (INPUT-08) until the Compose layer is wired.

use crate::backend::{Components, InlineText, KeyLookup, KeyboardBackend, KeymapError};
use crate::base_layout::base_layout_key;
use crate::key::{Key, Kp, Mods, Named};
use crate::kitty::pua;
use kbvm::lookup::LookupTable;
use kbvm::xkb::Context;
use kbvm::xkb::diagnostic::{Diagnostic, DiagnosticHandler};
use kbvm::{GroupIndex, Keycode, Keysym, ModifierMask};

/// Discards every diagnostic (they may quote keymap text).
struct Discard;

impl DiagnosticHandler for Discard {
    fn handle(&mut self, _diag: Diagnostic) {}
}

/// Kitty modifier bits resolved from the keymap by name (INPUT-68).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ModMap {
    shift: u32,
    ctrl: u32,
    alt: u32,
    sup: u32,
    hyper: u32,
    meta: u32,
    caps: u32,
    num: u32,
}

impl ModMap {
    fn resolve(map: &kbvm::xkb::Keymap) -> ModMap {
        let find = |name: &str| {
            map.virtual_modifiers()
                .find(|m| m.name().eq_ignore_ascii_case(name))
                .map(|m| m.mask().0)
                .filter(|m| *m != 0)
        };
        let alt = find("Alt").unwrap_or(ModifierMask::MOD1.0);
        let sup = find("Super").unwrap_or(ModifierMask::MOD4.0);
        // Meta and Hyper are usually aliases of Alt or Super's real modifier; a mask already
        // reported under another name is not reported twice.
        let meta = find("Meta").filter(|m| *m != alt && *m != sup).unwrap_or(0);
        let hyper = find("Hyper")
            .filter(|m| *m != alt && *m != sup && *m != meta)
            .unwrap_or(0);
        let num = find("NumLock").unwrap_or(ModifierMask::MOD2.0);
        ModMap {
            shift: ModifierMask::SHIFT.0,
            ctrl: ModifierMask::CONTROL.0,
            alt,
            sup,
            hyper,
            meta,
            caps: ModifierMask::LOCK.0,
            num,
        }
    }

    /// Non-lock bits from `remaining`, lock bits from `effective`.
    fn mods(&self, remaining: u32, effective: u32) -> Mods {
        let mut m = 0u8;
        let mut add = |mask: u32, bit: Mods, from: u32| {
            if mask != 0 && from & mask != 0 {
                m |= bit.0;
            }
        };
        add(self.shift, Mods::SHIFT, remaining);
        add(self.alt, Mods::ALT, remaining);
        add(self.ctrl, Mods::CTRL, remaining);
        add(self.sup, Mods::SUPER, remaining);
        add(self.hyper, Mods::HYPER, remaining);
        add(self.meta, Mods::META, remaining);
        add(self.caps, Mods::CAPS, effective);
        add(self.num, Mods::NUM, effective);
        Mods(m)
    }
}

/// The kbvm keyboard backend.
#[derive(Debug, Default)]
pub struct KbvmBackend {
    table: Option<LookupTable>,
    mods: ModMap,
    mods_effective: u32,
    group: u32,
    ctrl_fallback: bool,
}

impl KbvmBackend {
    /// An empty backend; call [`KeyboardBackend::load_keymap`] before `lookup`.
    pub fn new() -> Self {
        KbvmBackend {
            ctrl_fallback: true,
            ..Default::default()
        }
    }

    /// Enable or disable the Ctrl fallback for non-Latin layouts (INPUT-70).
    pub fn set_ctrl_fallback(&mut self, on: bool) {
        self.ctrl_fallback = on;
    }

    /// Compile keymap bytes (the contents of the `wl_keyboard.keymap` fd, trailing NULs
    /// allowed) like [`KeyboardBackend::load_keymap`].
    pub fn load_keymap_bytes(&mut self, bytes: &[u8]) -> Result<(), KeymapError> {
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        let bytes = &bytes[..end];
        if bytes.is_empty() {
            return Err(KeymapError("empty keymap"));
        }
        let mut cb = Context::builder();
        // A keymap from the compositor is self-contained: no host files, no environment.
        cb.enable_default_includes(false);
        cb.enable_environment(false);
        let ctx = cb.build();
        let map = ctx
            .keymap_from_bytes(Discard, None, bytes)
            .map_err(|_| KeymapError("keymap does not compile"))?;
        self.mods = ModMap::resolve(&map);
        self.table = Some(map.to_builder().build_lookup_table());
        self.mods_effective = 0;
        self.group = 0;
        Ok(())
    }
}

impl KeyboardBackend for KbvmBackend {
    fn load_keymap(&mut self, keymap_text: &str) -> Result<(), KeymapError> {
        self.load_keymap_bytes(keymap_text.as_bytes())
    }

    fn update_modifiers(&mut self, c: Components) {
        self.mods_effective = c.depressed | c.latched | c.locked;
        self.group = c.group;
    }

    fn lookup(&self, evdev: u32) -> Option<KeyLookup> {
        let table = self.table.as_ref()?;
        let kc = Keycode::from_evdev(evdev);
        let group = GroupIndex(self.group);
        let eff = ModifierMask(self.mods_effective);

        let cur = table.lookup(group, eff, kc).with_ctrl_transform(false);
        let remaining = cur.remaining_mods().0;
        let repeats = cur.repeats();
        let first = cur.into_iter().next()?;
        let sym = first.keysym();
        if is_dead(sym) {
            return None;
        }
        let mut mods = self.mods.mods(remaining, self.mods_effective);

        let (key, text, shifted) = if let Some((named, add_shift)) = named_of(sym) {
            if add_shift {
                mods = mods | Mods::SHIFT;
            }
            (Key::Named(named), InlineText::default(), None)
        } else {
            // INPUT-65: the unshifted, lower-cased symbol of the empty-mask lookup.
            let base_sym = table
                .lookup(group, ModifierMask::NONE, kc)
                .with_caps_transform(false)
                .with_ctrl_transform(false)
                .into_iter()
                .next()
                .map(|p| p.keysym())
                .unwrap_or(sym);
            let c = base_sym
                .to_lowercase()
                .char()
                .or_else(|| sym.char())
                .filter(|c| !c.is_control())?;
            // INPUT-66: the Shift-only symbol.
            let shifted = table
                .lookup(group, ModifierMask::SHIFT, kc)
                .with_caps_transform(false)
                .with_ctrl_transform(false)
                .into_iter()
                .next()
                .and_then(|p| p.keysym().char())
                .filter(|c| !c.is_control());
            let mut t = InlineText::default();
            let mut buf = [0u8; 16];
            let mut n = 0;
            for ch in table
                .lookup(group, eff, kc)
                .with_ctrl_transform(false)
                .into_iter()
                .filter_map(|p| p.char())
                .filter(|c| !c.is_control())
            {
                let l = ch.len_utf8();
                if n + l > buf.len() {
                    break;
                }
                ch.encode_utf8(&mut buf[n..]);
                n += l;
            }
            if let Ok(s) = core::str::from_utf8(&buf[..n]) {
                t = InlineText::new(s);
            }
            (Key::Char(c), t, shifted)
        };

        // INPUT-70: only probed when it can apply (Ctrl remaining).
        let ctrl_fallback = if self.ctrl_fallback && remaining & self.mods.ctrl != 0 {
            table
                .lookup(group, eff, kc)
                .into_iter()
                .next()
                .filter(|p| p.did_ctrl_fallback())
                .and_then(|p| char::from_u32(p.keysym().0))
        } else {
            None
        };

        Some(KeyLookup {
            key,
            shifted,
            base: base_layout_key(evdev),
            text,
            mods,
            ctrl_fallback,
            repeats,
        })
    }
}

/// A key named by its XKB keysym (`Return`, `a`, `A`, `asciitilde`), resolved without a
/// keyboard for `input.send-keys` (T-12 §2): the logical key, the text it types, and whether
/// Shift is implied (an upper-case letter is its lower-case key plus Shift). Dead keys and
/// unknown names are `None`. With no keymap there is no base or shifted symbol, so a shifted
/// symbol is reported as itself (`asciitilde` is `~`, not Shift+`grave`).
#[must_use]
pub fn key_of_name(name: &str) -> Option<(Key, Option<char>, bool)> {
    let sym = Keysym::from_str(name)?;
    if is_dead(sym) {
        return None;
    }
    if let Some((named, add_shift)) = named_of(sym) {
        return Some((Key::Named(named), None, add_shift));
    }
    let c = sym.char().filter(|c| !c.is_control())?;
    let lower = sym.to_lowercase().char().unwrap_or(c);
    let shift = lower != c;
    Some((Key::Char(lower), Some(c), shift))
}

/// `dead_*` keysyms and Multi_key: never emitted (INPUT-08).
fn is_dead(s: Keysym) -> bool {
    matches!(s.0, 0xfe50..=0xfe93 | 0xff20)
}

/// Functional keysym to a [`Named`] key; the flag asks for Shift to be re-added because
/// ISO_Left_Tab consumed it.
fn named_of(s: Keysym) -> Option<(Named, bool)> {
    let v = s.0;
    let n = match v {
        0xff0d => Named::Enter,
        0xff09 => Named::Tab,
        0xfe20 => return Some((Named::Tab, true)),
        0xff08 => Named::Backspace,
        0xff1b => Named::Escape,
        0xff63 => Named::Insert,
        0xffff => Named::Delete,
        0xff51 => Named::Left,
        0xff52 => Named::Up,
        0xff53 => Named::Right,
        0xff54 => Named::Down,
        0xff55 => Named::PageUp,
        0xff56 => Named::PageDown,
        0xff50 => Named::Home,
        0xff57 => Named::End,
        0xffbe..=0xffe0 => Named::F((v - 0xffbe + 1) as u8),
        0xff67 => Named::Menu,
        0xffe5 => Named::CapsLock,
        0xff14 => Named::ScrollLock,
        0xff7f => Named::NumLock,
        0xff61 => Named::PrintScreen,
        0xff13 => Named::Pause,
        0xff8d => Named::Kp(Kp::Enter),
        0xffb0..=0xffb9 => Named::Kp(Kp::Digit((v - 0xffb0) as u8)),
        0xffae => Named::Kp(Kp::Decimal),
        0xffaf => Named::Kp(Kp::Divide),
        0xffaa => Named::Kp(Kp::Multiply),
        0xffad => Named::Kp(Kp::Subtract),
        0xffab => Named::Kp(Kp::Add),
        0xffbd => Named::Kp(Kp::Equal),
        0xffac => Named::Kp(Kp::Separator),
        0xff96 => Named::Kp(Kp::Left),
        0xff98 => Named::Kp(Kp::Right),
        0xff97 => Named::Kp(Kp::Up),
        0xff99 => Named::Kp(Kp::Down),
        0xff9a => Named::Kp(Kp::PageUp),
        0xff9b => Named::Kp(Kp::PageDown),
        0xff95 => Named::Kp(Kp::Home),
        0xff9c => Named::Kp(Kp::End),
        0xff9e => Named::Kp(Kp::Insert),
        0xff9f => Named::Kp(Kp::Delete),
        0xff9d => Named::Kp(Kp::Begin),
        0xffe1 => Named::Pua(pua::LEFT_SHIFT),
        0xffe2 => Named::Pua(pua::RIGHT_SHIFT),
        0xffe3 => Named::Pua(pua::LEFT_CONTROL),
        0xffe4 => Named::Pua(pua::RIGHT_CONTROL),
        0xffe9 => Named::Pua(pua::LEFT_ALT),
        0xffea => Named::Pua(pua::RIGHT_ALT),
        0xffeb => Named::Pua(pua::LEFT_SUPER),
        0xffec => Named::Pua(pua::RIGHT_SUPER),
        0xffed => Named::Pua(pua::LEFT_HYPER),
        0xffee => Named::Pua(pua::RIGHT_HYPER),
        0xffe7 => Named::Pua(pua::LEFT_META),
        0xffe8 => Named::Pua(pua::RIGHT_META),
        0xfe03 => Named::Pua(pua::ISO_LEVEL3_SHIFT),
        0xfe11 => Named::Pua(pua::ISO_LEVEL5_SHIFT),
        0x1008ff14 => Named::Pua(pua::MEDIA_PLAY),
        0x1008ff31 => Named::Pua(pua::MEDIA_PAUSE),
        0x1008ff15 => Named::Pua(pua::MEDIA_STOP),
        0x1008ff16 => Named::Pua(pua::MEDIA_TRACK_PREVIOUS),
        0x1008ff17 => Named::Pua(pua::MEDIA_TRACK_NEXT),
        0x1008ff3e => Named::Pua(pua::MEDIA_REWIND),
        0x1008ff97 => Named::Pua(pua::MEDIA_FAST_FORWARD),
        0x1008ff1c => Named::Pua(pua::MEDIA_RECORD),
        0x1008ff11 => Named::Pua(pua::LOWER_VOLUME),
        0x1008ff13 => Named::Pua(pua::RAISE_VOLUME),
        0x1008ff12 => Named::Pua(pua::MUTE_VOLUME),
        _ => return None,
    };
    Some((n, false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InputModes, KeyKind, encode_key};

    #[test]
    fn keys_by_keysym_name() {
        assert_eq!(
            key_of_name("Return"),
            Some((Key::Named(Named::Enter), None, false))
        );
        assert_eq!(key_of_name("a"), Some((Key::Char('a'), Some('a'), false)));
        assert_eq!(key_of_name("A"), Some((Key::Char('a'), Some('A'), true)));
        assert_eq!(
            key_of_name("asciitilde"),
            Some((Key::Char('~'), Some('~'), false))
        );
        assert_eq!(
            key_of_name("ISO_Left_Tab"),
            Some((Key::Named(Named::Tab), None, true))
        );
        assert_eq!(key_of_name("dead_acute"), None);
        assert_eq!(key_of_name("NoSuchKey"), None);
    }

    /// A self-contained keymap: group 1 us, group 2 de, group 3 ru on the keys the tests
    /// use. Keycodes are xkb codes (evdev + 8). No host files are involved.
    const KEYMAP: &str = r#"
xkb_keymap {
  xkb_keycodes "t" {
    minimum = 8; maximum = 255;
    <ESC> = 9; <AE02> = 11; <AE11> = 20; <AE12> = 21;
    <BKSP> = 22; <TAB> = 23; <AD01> = 24; <AD06> = 29; <AD11> = 34;
    <RTRN> = 36; <LCTL> = 37; <AC01> = 38; <LFSH> = 50; <AB01> = 52; <AB03> = 54;
    <SPCE> = 65; <CAPS> = 66; <FK01> = 67; <NMLK> = 77; <KP7> = 79; <LALT> = 64;
    <LEFT> = 113; <LVL3> = 92; <LWIN> = 133; <HYPR> = 207; <I253> = 253;
  };
  xkb_types "t" {
    virtual_modifiers NumLock, LevelThree;
    type "ONE_LEVEL" { modifiers = None; map[None] = Level1; };
    type "TWO_LEVEL" { modifiers = Shift; map[Shift] = Level2; };
    type "ALPHABETIC" {
      modifiers = Shift + Lock;
      map[Shift] = Level2; map[Lock] = Level2;
    };
    type "ALT_LEVEL3" {
      modifiers = Shift + LevelThree;
      map[Shift] = Level2; map[LevelThree] = Level3; map[Shift + LevelThree] = Level4;
    };
    type "KEYPAD" {
      modifiers = Shift + NumLock;
      map[None] = Level1; map[Shift] = Level2; map[NumLock] = Level2;
      map[Shift + NumLock] = Level1;
    };
  };
  xkb_compat "t" {
    virtual_modifiers NumLock, LevelThree, Alt, Super, Hyper;
    interpret.useModMapMods = AnyLevel;
    interpret.repeat = False;
    interpret Any + AnyOf(all) { action = SetMods(modifiers = modMapMods, clearLocks); };
    interpret Num_Lock + AnyOf(all) {
      virtualModifier = NumLock; action = LockMods(modifiers = NumLock);
    };
    interpret ISO_Level3_Shift + AnyOf(all) {
      virtualModifier = LevelThree; action = SetMods(modifiers = LevelThree);
    };
    interpret Alt_L + AnyOf(all) { virtualModifier = Alt; action = SetMods(modifiers = modMapMods); };
    interpret Super_L + AnyOf(all) { virtualModifier = Super; action = SetMods(modifiers = modMapMods); };
    interpret Hyper_L + AnyOf(all) { virtualModifier = Hyper; action = SetMods(modifiers = modMapMods); };
  };
  xkb_symbols "t" {
    name[Group1] = "us"; name[Group2] = "de"; name[Group3] = "ru";
    key.repeat = true;
    key <ESC> { [ Escape ] };
    key <AE02> { type = "TWO_LEVEL", symbols[Group1] = [ 2, at ], symbols[Group2] = [ 2, quotedbl ] };
    key <AE11> { type = "TWO_LEVEL", symbols[Group1] = [ minus, underscore ], symbols[Group2] = [ ssharp, question ] };
    key <AE12> { type = "TWO_LEVEL", symbols[Group1] = [ equal, plus ], symbols[Group2] = [ dead_acute, dead_grave ] };
    key <BKSP> { [ BackSpace ] };
    key <TAB> { type = "TWO_LEVEL", symbols[Group1] = [ Tab, ISO_Left_Tab ] };
    key <AD01> {
      type[Group1] = "ALPHABETIC", type[Group2] = "ALT_LEVEL3",
      symbols[Group1] = [ q, Q ], symbols[Group2] = [ q, Q, at, Q ]
    };
    key <AD06> { type = "ALPHABETIC", symbols[Group1] = [ y, Y ], symbols[Group2] = [ z, Z ] };
    key <AD11> { type = "ALPHABETIC", symbols[Group1] = [ bracketleft, braceleft ], symbols[Group2] = [ udiaeresis, Udiaeresis ] };
    key <RTRN> { [ Return ] };
    key <LCTL> { [ Control_L ] };
    key <AC01> { type = "ALPHABETIC", symbols[Group1] = [ a, A ] };
    key <LFSH> { [ Shift_L ] };
    key <AB01> { type = "ALPHABETIC", symbols[Group1] = [ z, Z ], symbols[Group2] = [ y, Y ] };
    key <AB03> {
      type = "ALPHABETIC",
      symbols[Group1] = [ c, C ], symbols[Group2] = [ c, C ],
      symbols[Group3] = [ Cyrillic_es, Cyrillic_ES ]
    };
    key <SPCE> { [ space ] };
    key <CAPS> { [ Caps_Lock ] };
    key <FK01> { [ F1 ] };
    key <NMLK> { [ Num_Lock ] };
    key <KP7> { type = "KEYPAD", symbols[Group1] = [ KP_Home, KP_7 ] };
    key <LALT> { [ Alt_L ] };
    key <LEFT> { [ Left ] };
    key <LVL3> { [ ISO_Level3_Shift ] };
    key <LWIN> { [ Super_L ] };
    key <HYPR> { [ Hyper_L ] };
    key <I253> { [ XF86AudioPlay ] };
    modifier_map Shift { <LFSH> };
    modifier_map Lock { <CAPS> };
    modifier_map Control { <LCTL> };
    modifier_map Mod1 { <LALT> };
    modifier_map Mod2 { <NMLK> };
    modifier_map Mod3 { <HYPR> };
    modifier_map Mod4 { <LWIN> };
    modifier_map Mod5 { <LVL3> };
  };
};
"#;

    const SHIFT: u32 = 0x01;
    const LOCK: u32 = 0x02;
    const CTRL: u32 = 0x04;
    const MOD1: u32 = 0x08;
    const MOD2: u32 = 0x10;
    const MOD3: u32 = 0x20;
    const MOD4: u32 = 0x40;
    const MOD5: u32 = 0x80;

    // evdev codes (xkb keycode - 8).
    const ESC: u32 = 1;
    const E2: u32 = 3;
    const E12: u32 = 13;
    const TAB: u32 = 15;
    const Q: u32 = 16;
    const Y_US: u32 = 21;
    const UE: u32 = 26;
    const RET: u32 = 28;
    const CTRL_L: u32 = 29;
    const A: u32 = 30;
    const SHIFT_L: u32 = 42;
    const Z_US: u32 = 44;
    const C: u32 = 46;
    const F1: u32 = 59;
    const KP7: u32 = 71;
    const LEFT: u32 = 105;
    const PLAY: u32 = 245;

    fn backend() -> KbvmBackend {
        let mut b = KbvmBackend::new();
        b.load_keymap(KEYMAP).expect("test keymap compiles");
        b
    }

    fn with(b: &mut KbvmBackend, depressed: u32, locked: u32, group: u32) {
        b.update_modifiers(Components {
            depressed,
            latched: 0,
            locked,
            group,
        });
    }

    #[test]
    fn us_layout_keysyms_text_and_shift() {
        let mut b = backend();
        let l = b.lookup(Y_US).unwrap();
        assert_eq!(l.key, Key::Char('y'));
        assert_eq!(l.text.as_str(), "y");
        assert_eq!(l.shifted, Some('Y'));
        assert_eq!(l.mods, Mods::NONE);
        assert!(l.repeats);
        assert_eq!(l.base, Some('y'));

        // Shift is consumed by the type: Shift+y is text "Y" with no Shift reported.
        with(&mut b, SHIFT, 0, 0);
        let l = b.lookup(Y_US).unwrap();
        assert_eq!(
            (l.key, l.text.as_str(), l.mods),
            (Key::Char('y'), "Y", Mods::NONE)
        );
        let l = b.lookup(E2).unwrap();
        assert_eq!((l.key, l.text.as_str()), (Key::Char('2'), "@"));
        assert_eq!(l.shifted, Some('@'));

        // Caps Lock: uppercase text, the lock stays visible as a modifier bit.
        with(&mut b, 0, LOCK, 0);
        let l = b.lookup(Y_US).unwrap();
        assert_eq!(
            (l.key, l.text.as_str(), l.mods),
            (Key::Char('y'), "Y", Mods::CAPS)
        );
    }

    #[test]
    fn de_layout_swaps_yz_and_has_umlauts() {
        let mut b = backend();
        with(&mut b, 0, 0, 1);
        assert_eq!(b.lookup(Y_US).unwrap().key, Key::Char('z'));
        assert_eq!(b.lookup(Y_US).unwrap().text.as_str(), "z");
        assert_eq!(b.lookup(Z_US).unwrap().key, Key::Char('y'));
        // base layout key is the US position, independent of layout (INPUT-67).
        assert_eq!(b.lookup(Y_US).unwrap().base, Some('y'));
        let l = b.lookup(UE).unwrap();
        assert_eq!(
            (l.key, l.text.as_str(), l.shifted),
            (Key::Char('ü'), "ü", Some('Ü'))
        );
        assert_eq!(b.lookup(E2).unwrap().shifted, Some('"'));
        with(&mut b, SHIFT, 0, 1);
        assert_eq!(b.lookup(UE).unwrap().text.as_str(), "Ü");
        // AltGr (LevelThree = Mod5) on q gives '@', with the modifier consumed.
        with(&mut b, MOD5, 0, 1);
        let l = b.lookup(Q).unwrap();
        assert_eq!(
            (l.key, l.text.as_str(), l.mods),
            (Key::Char('q'), "@", Mods::NONE)
        );
        // Back to us.
        with(&mut b, 0, 0, 0);
        assert_eq!(b.lookup(Y_US).unwrap().key, Key::Char('y'));
    }

    #[test]
    fn dead_keys_are_never_emitted() {
        let mut b = backend();
        assert_eq!(b.lookup(E12).unwrap().key, Key::Char('='));
        with(&mut b, 0, 0, 1);
        assert_eq!(b.lookup(E12), None);
    }

    #[test]
    fn modifier_masks_are_resolved_by_name() {
        let b = backend();
        assert_eq!(b.mods.alt, MOD1);
        assert_eq!(b.mods.sup, MOD4);
        assert_eq!(b.mods.hyper, MOD3);
        assert_eq!(b.mods.num, MOD2);
        assert_eq!(
            (b.mods.shift, b.mods.ctrl, b.mods.caps),
            (SHIFT, CTRL, LOCK)
        );
        let mut b = b;
        for (mask, want) in [
            (CTRL, Mods::CTRL),
            (MOD1, Mods::ALT),
            (MOD4, Mods::SUPER),
            (MOD3, Mods::HYPER),
            (CTRL | MOD1 | MOD4, Mods::CTRL | Mods::ALT | Mods::SUPER),
        ] {
            with(&mut b, mask, 0, 0);
            let l = b.lookup(A).unwrap();
            assert_eq!(l.mods, want, "mask {mask:#x}");
            assert_eq!(l.key, Key::Char('a'));
            assert_eq!(
                l.text.as_str(),
                "a",
                "ctrl transform must not leak into text"
            );
        }
        with(&mut b, 0, MOD2, 0);
        assert_eq!(b.lookup(A).unwrap().mods, Mods::NUM);
    }

    #[test]
    fn functional_keys() {
        let mut b = backend();
        let l = b.lookup(RET).unwrap();
        assert_eq!((l.key, l.text.as_str()), (Key::Named(Named::Enter), ""));
        assert_eq!(b.lookup(ESC).unwrap().key, Key::Named(Named::Escape));
        assert_eq!(b.lookup(F1).unwrap().key, Key::Named(Named::F(1)));
        assert_eq!(b.lookup(LEFT).unwrap().key, Key::Named(Named::Left));
        assert_eq!(
            b.lookup(SHIFT_L).unwrap().key,
            Key::Named(Named::Pua(pua::LEFT_SHIFT))
        );
        assert_eq!(
            b.lookup(CTRL_L).unwrap().key,
            Key::Named(Named::Pua(pua::LEFT_CONTROL))
        );
        assert_eq!(
            b.lookup(PLAY).unwrap().key,
            Key::Named(Named::Pua(pua::MEDIA_PLAY))
        );
        // Keypad 7: Home without NumLock, digit with it.
        assert_eq!(b.lookup(KP7).unwrap().key, Key::Named(Named::Kp(Kp::Home)));
        with(&mut b, 0, MOD2, 0);
        let l = b.lookup(KP7).unwrap();
        assert_eq!(l.key, Key::Named(Named::Kp(Kp::Digit(7))));
        assert_eq!(l.mods, Mods::NUM);
        // Shift+Tab arrives as ISO_Left_Tab: still Tab, Shift put back.
        with(&mut b, SHIFT, 0, 0);
        let l = b.lookup(TAB).unwrap();
        assert_eq!((l.key, l.mods), (Key::Named(Named::Tab), Mods::SHIFT));
    }

    #[test]
    fn ctrl_fallback_for_non_latin_group() {
        let mut b = backend();
        with(&mut b, CTRL, 0, 2);
        let l = b.lookup(C).unwrap();
        assert_eq!(l.ctrl_fallback, Some('c'));
        assert_eq!(l.mods, Mods::CTRL);
        assert_eq!(l.text.as_str(), "с"); // Cyrillic es, U+0441
        // Latin group: nothing to fall back from.
        with(&mut b, CTRL, 0, 0);
        assert_eq!(b.lookup(C).unwrap().ctrl_fallback, None);
        // Config switch off.
        b.set_ctrl_fallback(false);
        with(&mut b, CTRL, 0, 2);
        assert_eq!(b.lookup(C).unwrap().ctrl_fallback, None);
        // Without Ctrl the fallback is not probed.
        b.set_ctrl_fallback(true);
        with(&mut b, 0, 0, 2);
        assert_eq!(b.lookup(C).unwrap().ctrl_fallback, None);
    }

    #[test]
    fn lookup_feeds_the_encoders() {
        let mut b = backend();
        let mut out = [0u8; 64];
        let l = b.lookup(RET).unwrap();
        let n = encode_key(&l.event(KeyKind::Press), InputModes::default(), &mut out);
        assert_eq!(&out[..n], b"\r");
        with(&mut b, CTRL, 0, 0);
        let l = b.lookup(A).unwrap();
        let n = encode_key(&l.event(KeyKind::Press), InputModes::default(), &mut out);
        assert_eq!(&out[..n], [1]);
        // Kitty flag 1: Ctrl+a is CSI 97;5u.
        let n = encode_key(
            &l.event(KeyKind::Press),
            InputModes::default().with_kitty(1),
            &mut out,
        );
        assert_eq!(&out[..n], b"\x1b[97;5u");
        // German z on the US y position carries its own text through the encoder.
        with(&mut b, 0, 0, 1);
        let l = b.lookup(Y_US).unwrap();
        let n = encode_key(&l.event(KeyKind::Press), InputModes::default(), &mut out);
        assert_eq!(&out[..n], b"z");
    }

    #[test]
    fn bad_input_is_an_error_not_a_panic() {
        let mut b = KbvmBackend::new();
        assert_eq!(b.lookup(A), None, "no keymap loaded");
        assert!(b.load_keymap("").is_err());
        assert!(b.load_keymap("\0\0\0").is_err());
        assert!(b.load_keymap("xkb_keymap { this is not xkb").is_err());
        assert!(b.load_keymap("\u{0}garbage").is_err());
        // A failed load keeps the previous table.
        b.load_keymap(KEYMAP).unwrap();
        assert!(b.load_keymap("garbage").is_err());
        assert!(b.lookup(A).is_some());
        // The fd contents are NUL-terminated.
        let mut bytes = KEYMAP.as_bytes().to_vec();
        bytes.push(0);
        let mut c = KbvmBackend::new();
        c.load_keymap_bytes(&bytes).unwrap();
        assert_eq!(c.lookup(A).unwrap().key, Key::Char('a'));
        // Unknown keycodes and huge groups do not panic.
        assert_eq!(c.lookup(9999), None);
        c.update_modifiers(Components {
            depressed: u32::MAX,
            latched: u32::MAX,
            locked: u32::MAX,
            group: u32::MAX,
        });
        let _ = c.lookup(A);
        let _ = c.lookup(Q);
    }
}
