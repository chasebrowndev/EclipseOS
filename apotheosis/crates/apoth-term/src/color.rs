// SPDX-License-Identifier: AGPL-3.0-only
//! Palette defaults and colour spec parsing for OSC 4/10-12.

use crate::config::Rgb;

const ANSI16: [Rgb; 16] = [
    (0x00, 0x00, 0x00),
    (0xcd, 0x00, 0x00),
    (0x00, 0xcd, 0x00),
    (0xcd, 0xcd, 0x00),
    (0x00, 0x00, 0xee),
    (0xcd, 0x00, 0xcd),
    (0x00, 0xcd, 0xcd),
    (0xe5, 0xe5, 0xe5),
    (0x7f, 0x7f, 0x7f),
    (0xff, 0x00, 0x00),
    (0x00, 0xff, 0x00),
    (0xff, 0xff, 0x00),
    (0x5c, 0x5c, 0xff),
    (0xff, 0x00, 0xff),
    (0x00, 0xff, 0xff),
    (0xff, 0xff, 0xff),
];

/// The default 256-colour palette (16 ANSI, 6x6x6 cube, 24 greys).
pub(crate) fn default_palette() -> [Rgb; 256] {
    let mut p = [(0u8, 0u8, 0u8); 256];
    p[..16].copy_from_slice(&ANSI16);
    let lvl = [0u8, 0x5f, 0x87, 0xaf, 0xd7, 0xff];
    for i in 0..216usize {
        p[16 + i] = (lvl[i / 36], lvl[i / 6 % 6], lvl[i % 6]);
    }
    for i in 0..24usize {
        let v = (8 + 10 * i) as u8;
        p[232 + i] = (v, v, v);
    }
    p
}

fn scale(v: u32, digits: usize) -> u8 {
    let max = (1u32 << (4 * digits)) - 1;
    ((v * 255 + max / 2) / max) as u8
}

fn hex(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 4 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(s, 16).ok()
}

/// Parse `rgb:R/G/B` (1-4 hex digits each) or `#RGB`, `#RRGGBB`, `#RRRGGGBBB`,
/// `#RRRRGGGGBBBB`.
pub(crate) fn parse(spec: &str) -> Option<Rgb> {
    if let Some(rest) = spec.strip_prefix("rgb:") {
        let mut it = rest.split('/');
        let (r, g, b) = (it.next()?, it.next()?, it.next()?);
        if it.next().is_some() {
            return None;
        }
        return Some((
            scale(hex(r)?, r.len()),
            scale(hex(g)?, g.len()),
            scale(hex(b)?, b.len()),
        ));
    }
    let h = spec.strip_prefix('#')?;
    let n = h.len();
    if n == 0 || n % 3 != 0 || n > 12 || !h.is_ascii() {
        return None;
    }
    let d = n / 3;
    Some((
        scale(hex(&h[..d])?, d),
        scale(hex(&h[d..2 * d])?, d),
        scale(hex(&h[2 * d..])?, d),
    ))
}

/// `rgb:RRRR/GGGG/BBBB`, the 16-bit form used in query replies.
pub(crate) fn format16(c: Rgb) -> String {
    format!(
        "rgb:{:02x}{:02x}/{:02x}{:02x}/{:02x}{:02x}",
        c.0, c.0, c.1, c.1, c.2, c.2
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs() {
        assert_eq!(parse("rgb:ff/00/80"), Some((255, 0, 128)));
        assert_eq!(parse("rgb:f/0/8"), Some((255, 0, 136)));
        assert_eq!(parse("rgb:ffff/0000/8080"), Some((255, 0, 128)));
        assert_eq!(parse("#f00"), Some((255, 0, 0)));
        assert_eq!(parse("#102030"), Some((16, 32, 48)));
        assert_eq!(parse("#fff000000"), Some((255, 0, 0)));
        assert_eq!(parse("rgb:1/2"), None);
        assert_eq!(parse("rgb:gg/00/00"), None);
        assert_eq!(parse("#12"), None);
        assert_eq!(parse("red"), None);
    }

    #[test]
    fn palette_shape() {
        let p = default_palette();
        assert_eq!(p[16], (0, 0, 0));
        assert_eq!(p[231], (255, 255, 255));
        assert_eq!(p[232], (8, 8, 8));
        assert_eq!(p[255], (238, 238, 238));
        assert_eq!(format16((1, 2, 3)), "rgb:0101/0202/0303");
    }
}
