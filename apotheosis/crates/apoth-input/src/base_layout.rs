// SPDX-License-Identifier: Apache-2.0
//! Static evdev keycode to US-layout key table (INPUT-67), for the kitty base-layout key.

/// The US-layout (unshifted) character at the physical position of `evdev` code
/// (`KEY_*` from `linux/input-event-codes.h`, not the xkb keycode which is +8).
pub const fn base_layout_key(evdev: u32) -> Option<char> {
    const ROW1: &[u8; 12] = b"1234567890-=";
    const ROW2: &[u8; 12] = b"qwertyuiop[]";
    const ROW3: &[u8; 11] = b"asdfghjkl;'";
    const ROW4: &[u8; 10] = b"zxcvbnm,./";
    match evdev {
        2..=13 => Some(ROW1[(evdev - 2) as usize] as char),
        16..=27 => Some(ROW2[(evdev - 16) as usize] as char),
        30..=40 => Some(ROW3[(evdev - 30) as usize] as char),
        41 => Some('`'),
        43 => Some('\\'),
        44..=53 => Some(ROW4[(evdev - 44) as usize] as char),
        57 => Some(' '),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows() {
        assert_eq!(base_layout_key(2), Some('1'));
        assert_eq!(base_layout_key(13), Some('='));
        assert_eq!(base_layout_key(16), Some('q'));
        assert_eq!(base_layout_key(27), Some(']'));
        assert_eq!(base_layout_key(30), Some('a'));
        assert_eq!(base_layout_key(40), Some('\''));
        assert_eq!(base_layout_key(41), Some('`'));
        assert_eq!(base_layout_key(43), Some('\\'));
        assert_eq!(base_layout_key(44), Some('z'));
        assert_eq!(base_layout_key(53), Some('/'));
        assert_eq!(base_layout_key(57), Some(' '));
        assert_eq!(base_layout_key(1), None);
        assert_eq!(base_layout_key(14), None);
        assert_eq!(base_layout_key(1000), None);
    }
}
