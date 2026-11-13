// SPDX-License-Identifier: Apache-2.0
//! UTF-8 helpers: the control-byte scan and the lead-byte table used by the
//! scalar carry decoder (the bulk path uses `simdutf8`; both implement the
//! Unicode Table 3-7 maximal-subpart rule).

const LO: u64 = 0x0101_0101_0101_0101;
const HI: u64 = 0x8080_8080_8080_8080;

#[inline(always)]
fn word_has_ctl(w: u64) -> bool {
    // any byte < 0x20
    let lt = w.wrapping_sub(LO.wrapping_mul(0x20)) & !w & HI;
    // any byte == 0x7F
    let x = w ^ LO.wrapping_mul(0x7F);
    let eq = x.wrapping_sub(LO) & !x & HI;
    (lt | eq) != 0
}

#[inline(always)]
pub(crate) const fn is_ctl(b: u8) -> bool {
    b < 0x20 || b == 0x7F
}

/// Index of the first C0 or DEL byte in `b`, or `b.len()`.
#[inline]
pub(crate) fn scan_ctl(b: &[u8]) -> usize {
    let mut i = 0;
    let mut chunks = b.chunks_exact(8);
    for c in chunks.by_ref() {
        let Ok(a) = <[u8; 8]>::try_from(c) else { break };
        if word_has_ctl(u64::from_le_bytes(a)) {
            break;
        }
        i += 8;
    }
    while i < b.len() && !is_ctl(b[i]) {
        i += 1;
    }
    i
}

/// For a valid multi-byte lead: (total length, lowest and highest valid second byte).
pub(crate) const fn lead_info(lead: u8) -> Option<(usize, u8, u8)> {
    match lead {
        0xC2..=0xDF => Some((2, 0x80, 0xBF)),
        0xE0 => Some((3, 0xA0, 0xBF)),
        0xE1..=0xEC | 0xEE..=0xEF => Some((3, 0x80, 0xBF)),
        0xED => Some((3, 0x80, 0x9F)),
        0xF0 => Some((4, 0x90, 0xBF)),
        0xF1..=0xF3 => Some((4, 0x80, 0xBF)),
        0xF4 => Some((4, 0x80, 0x8F)),
        _ => None,
    }
}
