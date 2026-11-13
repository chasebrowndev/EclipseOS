// SPDX-License-Identifier: Apache-2.0
//! `InputModes`: the `Copy` snapshot of a pane's input-path modes (T-00 §4.2, T-03 §2).
//!
//! The bit layout below is this crate's reading of T-03's mirror; T-03 owns the final
//! layout and must keep these constants (or re-export them) so both sides agree.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Mirror bit positions.
pub mod bit {
    /// DECCKM (`?1`): application cursor keys.
    pub const DECCKM: u64 = 1 << 0;
    /// DECKPAM / DECNKM (`?66`): application keypad.
    pub const DECKPAM: u64 = 1 << 1;
    /// Mode 1036: Alt sends ESC prefix (default set).
    pub const ALT_ESC: u64 = 1 << 2;
    /// Mode 2004: bracketed paste.
    pub const BRACKETED_PASTE: u64 = 1 << 3;
    /// Mode 9 (X10 compat). Never built (INPUT-20); mirrored only.
    pub const MOUSE_9: u64 = 1 << 4;
    /// Mode 1000.
    pub const MOUSE_1000: u64 = 1 << 5;
    /// Mode 1002.
    pub const MOUSE_1002: u64 = 1 << 6;
    /// Mode 1003.
    pub const MOUSE_1003: u64 = 1 << 7;
    /// Mode 1004: focus reporting.
    pub const FOCUS_1004: u64 = 1 << 8;
    /// Mode 1005 (UTF-8 mouse). Never built; mirrored only.
    pub const MOUSE_1005: u64 = 1 << 9;
    /// Mode 1006: SGR mouse encoding.
    pub const MOUSE_SGR_1006: u64 = 1 << 10;
    /// Mode 1015 (urxvt mouse). Never built; mirrored only.
    pub const MOUSE_1015: u64 = 1 << 11;
    /// Mode 1016: SGR-Pixels.
    pub const MOUSE_PIXELS_1016: u64 = 1 << 12;
    /// DECBKM (`?67`): backspace sends BS (0x08) when set.
    pub const DECBKM: u64 = 1 << 13;
    /// Alternate screen active.
    pub const ALT_SCREEN: u64 = 1 << 14;
    /// Mode 1007: alternate scroll (default set).
    pub const ALT_SCROLL_1007: u64 = 1 << 15;
    /// modifyOtherKeys level, two bits (`CSI > 4 ; n m`).
    pub const MOK_SHIFT: u32 = 16;
    /// formatOtherKeys = 1 (`CSI code ; m u` instead of `CSI 27 ; m ; code ~`).
    pub const FORMAT_OTHER_KEYS: u64 = 1 << 18;
}

/// Mouse reporting level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MouseMode {
    /// No reporting.
    None,
    /// 1000: press and release.
    Click,
    /// 1002: plus motion while a button is held.
    Drag,
    /// 1003: all motion.
    Any,
}

/// Snapshot of the input-path modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputModes {
    bits: u64,
    kitty: u32,
}

impl Default for InputModes {
    /// Power-on state: 1036 and 1007 set (T-05 §2, §5), everything else reset.
    fn default() -> Self {
        InputModes {
            bits: bit::ALT_ESC | bit::ALT_SCROLL_1007,
            kitty: 0,
        }
    }
}

impl InputModes {
    /// Build from the two mirror words.
    pub const fn from_raw(bits: u64, kitty_flags: u32) -> Self {
        InputModes {
            bits,
            kitty: kitty_flags,
        }
    }
    /// The two mirror words.
    pub const fn raw(self) -> (u64, u32) {
        (self.bits, self.kitty)
    }
    /// Is any bit of `mask` set.
    pub const fn has(self, mask: u64) -> bool {
        self.bits & mask != 0
    }
    /// Copy with `mask` set or cleared.
    pub const fn with(self, mask: u64, on: bool) -> Self {
        let bits = if on {
            self.bits | mask
        } else {
            self.bits & !mask
        };
        InputModes {
            bits,
            kitty: self.kitty,
        }
    }
    /// Copy with the active kitty flags replaced.
    pub const fn with_kitty(self, flags: u32) -> Self {
        InputModes {
            bits: self.bits,
            kitty: flags,
        }
    }
    /// Active kitty keyboard flags.
    pub const fn kitty_flags(self) -> u32 {
        self.kitty
    }
    /// modifyOtherKeys level 0..=2.
    pub const fn mok_level(self) -> u8 {
        ((self.bits >> bit::MOK_SHIFT) & 3) as u8
    }
    /// Copy with the modifyOtherKeys level set (clamped to 2).
    pub const fn with_mok_level(self, level: u8) -> Self {
        let l = if level > 2 { 2u64 } else { level as u64 };
        InputModes {
            bits: (self.bits & !(3 << bit::MOK_SHIFT)) | (l << bit::MOK_SHIFT),
            kitty: self.kitty,
        }
    }
    /// Highest of 1000/1002/1003 that is set. Mode 9 is not built.
    pub const fn mouse_mode(self) -> MouseMode {
        if self.has(bit::MOUSE_1003) {
            MouseMode::Any
        } else if self.has(bit::MOUSE_1002) {
            MouseMode::Drag
        } else if self.has(bit::MOUSE_1000) {
            MouseMode::Click
        } else {
            MouseMode::None
        }
    }
}

/// The pane's lock-free mirror: written by the io thread (Release), read by main (Acquire).
#[derive(Debug)]
pub struct InputMirror {
    bits: AtomicU64,
    kitty: AtomicU32,
}

impl Default for InputMirror {
    fn default() -> Self {
        Self::new(InputModes::default())
    }
}

impl InputMirror {
    /// New mirror holding `m`.
    pub const fn new(m: InputModes) -> Self {
        InputMirror {
            bits: AtomicU64::new(m.bits),
            kitty: AtomicU32::new(m.kitty),
        }
    }
    /// Publish `m` (Release).
    pub fn store(&self, m: InputModes) {
        self.kitty.store(m.kitty, Ordering::Release);
        self.bits.store(m.bits, Ordering::Release);
    }
    /// Load a snapshot (Acquire).
    pub fn load(&self) -> InputModes {
        let bits = self.bits.load(Ordering::Acquire);
        let kitty = self.kitty.load(Ordering::Acquire);
        InputModes { bits, kitty }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let m = InputModes::default();
        assert!(m.has(bit::ALT_ESC) && m.has(bit::ALT_SCROLL_1007));
        assert!(!m.has(bit::DECCKM));
        assert_eq!(m.mouse_mode(), MouseMode::None);
        assert_eq!(m.mok_level(), 0);
    }

    #[test]
    fn mouse_mode_priority() {
        let m = InputModes::default().with(bit::MOUSE_1000, true);
        assert_eq!(m.mouse_mode(), MouseMode::Click);
        let m = m.with(bit::MOUSE_1002, true);
        assert_eq!(m.mouse_mode(), MouseMode::Drag);
        let m = m.with(bit::MOUSE_1003, true);
        assert_eq!(m.mouse_mode(), MouseMode::Any);
        assert_eq!(
            InputModes::default().with(bit::MOUSE_9, true).mouse_mode(),
            MouseMode::None
        );
    }

    #[test]
    fn mok_roundtrip() {
        assert_eq!(InputModes::default().with_mok_level(2).mok_level(), 2);
        assert_eq!(InputModes::default().with_mok_level(9).mok_level(), 2);
        assert_eq!(
            InputModes::default()
                .with_mok_level(2)
                .with_mok_level(0)
                .mok_level(),
            0
        );
    }

    #[test]
    fn mirror_roundtrip() {
        let mir = InputMirror::default();
        let m = InputModes::default().with(bit::DECCKM, true).with_kitty(9);
        mir.store(m);
        assert_eq!(mir.load(), m);
        assert_eq!(m.raw().1, 9);
        assert_eq!(InputModes::from_raw(m.raw().0, 9), m);
    }
}
