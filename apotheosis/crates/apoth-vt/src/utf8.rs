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

/// Index of the first C0 or DEL byte in `b` (or `b.len()`), and whether every byte
/// before it is ASCII. One word-at-a-time pass; no allocation, no unsafe.
#[inline]
pub(crate) fn scan_run(b: &[u8]) -> (usize, bool) {
    let mut i = 0;
    let mut acc = 0u64;
    let mut chunks = b.chunks_exact(8);
    for c in chunks.by_ref() {
        let Ok(a) = <[u8; 8]>::try_from(c) else { break };
        let w = u64::from_le_bytes(a);
        if word_has_ctl(w) {
            break;
        }
        acc |= w;
        i += 8;
    }
    let mut hi = (acc & HI) != 0;
    while i < b.len() && !is_ctl(b[i]) {
        hi |= b[i] >= 0x80;
        i += 1;
    }
    (i, !hi)
}

/// Index of the first C0 or DEL byte in `b`, or `b.len()`.
#[inline]
#[cfg(test)]
pub(crate) fn scan_ctl(b: &[u8]) -> usize {
    scan_run(b).0
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_run_matches_bytewise() {
        let mut v = Vec::new();
        for len in 0..40usize {
            for ctl_at in 0..=len {
                for hi_at in 0..=len {
                    v.clear();
                    v.extend((0..len).map(|k| b'a' + (k % 20) as u8));
                    if ctl_at < len {
                        v[ctl_at] = if ctl_at % 2 == 0 { 0x1B } else { 0x7F };
                    }
                    if hi_at < len && hi_at != ctl_at {
                        v[hi_at] = 0xC3;
                    }
                    let n = v.iter().position(|&b| is_ctl(b)).unwrap_or(len);
                    let ascii = v[..n].iter().all(|&b| b < 0x80);
                    assert_eq!(scan_run(&v), (n, ascii), "{v:?}");
                    assert_eq!(scan_ctl(&v), n);
                }
            }
        }
    }
}
