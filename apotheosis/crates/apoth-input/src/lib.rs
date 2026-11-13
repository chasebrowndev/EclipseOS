// SPDX-License-Identifier: Apache-2.0
//! Input encoders: legacy and kitty keyboard, mouse reporting, bracketed paste.
//!
//! Governing spec: T-05 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`). Pure functions over the
//! `Copy` [`InputModes`] value loaded from a pane's lock-free mirror (T-00 §4.2); the key
//! path writes into a fixed buffer and never allocates. Content is never logged (T-05 §7);
//! this crate has no logging at all.
//!
//! Bindings are matched before encoding, in `apoth-wl` (T-11), not here.
#![forbid(unsafe_code)]

pub mod backend;
#[cfg(feature = "kbvm")]
pub mod backend_kbvm;
pub mod base_layout;
pub mod buf;
pub mod key;
pub mod kitty;
pub mod legacy;
pub mod modes;
pub mod mouse;
pub mod paste;

pub use backend::{Components, InlineText, KeyLookup, KeyboardBackend, KeymapError};
pub use key::{Key, KeyEvent, KeyKind, Kp, Mods, Named};
pub use kitty::{KittyFlagStack, KittyState};
pub use modes::{InputMirror, InputModes, MouseMode};
pub use mouse::{
    MouseButton, MouseEvent, MouseKind, MouseOutcome, MouseTracker, WheelAction, encode_focus,
    encode_mouse, encode_mouse_outcome, encode_wheel_key, selection_override, wheel_action,
};
pub use paste::{PasteError, PasteNewline, PasteOptions, encode_paste, encode_text};

/// Encode one key event into `out`; returns the byte count (0 = nothing to send).
///
/// Kitty flags non-zero select the kitty encoder, otherwise the legacy xterm encoder.
/// No allocation, no lock. Output is bounded by the 64-byte buffer.
pub fn encode_key(ev: &KeyEvent<'_>, modes: InputModes, out: &mut [u8; 64]) -> usize {
    let mut w = buf::W::new(out);
    if modes.kitty_flags() & kitty::flags::ALL != 0 {
        kitty::encode(ev, modes, &mut w);
    } else {
        legacy::encode(ev, modes, &mut w);
    }
    w.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Property: for every (key, mods, mode bits, kitty flags) the output is bounded and
    /// encoding is deterministic (T-05 §8).
    #[test]
    fn bounded_and_deterministic() {
        let keys = [
            Key::Char('a'),
            Key::Char(' '),
            Key::Named(Named::Tab),
            Key::Named(Named::Backspace),
            Key::Named(Named::F(7)),
            Key::Named(Named::Kp(Kp::Digit(3))),
            Key::Named(Named::Left),
        ];
        let bit_sets = [
            0u64,
            modes::bit::DECCKM | modes::bit::DECKPAM,
            modes::bit::DECBKM | modes::bit::ALT_ESC,
            modes::bit::FORMAT_OTHER_KEYS,
        ];
        for key in keys {
            for mods in (0..=255u8).step_by(3) {
                for bits in bit_sets {
                    for mok in 0..=2u8 {
                        for kf in [0u32, 1, 3, 31] {
                            let m = InputModes::from_raw(bits, kf).with_mok_level(mok);
                            let ev = KeyEvent::new(key).mods(Mods(mods)).text("é");
                            let (mut a, mut b) = ([0u8; 64], [0u8; 64]);
                            let n = encode_key(&ev, m, &mut a);
                            let n2 = encode_key(&ev, m, &mut b);
                            assert!(n <= 64);
                            assert_eq!(a[..n], b[..n2]);
                        }
                    }
                }
            }
        }
    }
}
