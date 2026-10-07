// SPDX-License-Identifier: AGPL-3.0-only
//! TOTP (S-08 §4): RFC 6238, HMAC-SHA1, 30-second step, six digits.
//!
//! The broker stores the seed and computes the code; the agent receives the
//! code, which is a capability to authenticate once and not a credential.
//! Issuance is rate limited to three per secret per five minutes.

use hmac::{Hmac, Mac};
use sha1::Sha1;
use std::collections::VecDeque;

pub const STEP: u64 = 30;
pub const DIGITS: u32 = 6;
pub const WINDOW_S: u64 = 300;
pub const MAX_IN_WINDOW: usize = 3;

/// The code for `seed` at `unix` seconds.
pub fn code(seed: &[u8], unix: u64, digits: u32) -> String {
    // HMAC accepts any key length, so this cannot fail.
    let mut mac = Hmac::<Sha1>::new_from_slice(seed).expect("hmac takes any key length");
    mac.update(&(unix / STEP).to_be_bytes());
    let h = mac.finalize().into_bytes();
    let off = (h[h.len() - 1] & 0x0f) as usize;
    let bin = u32::from_be_bytes([h[off] & 0x7f, h[off + 1], h[off + 2], h[off + 3]]);
    let v = bin % 10u32.pow(digits);
    format!("{v:0width$}", width = digits as usize)
}

/// Seconds until the code issued at `unix` stops being the current one.
pub fn life_left(unix: u64) -> u64 {
    STEP - unix % STEP
}

/// Issuance times for one secret.
#[derive(Debug, Default)]
pub struct IssueLog(VecDeque<u64>);

impl IssueLog {
    /// Records an issuance at `now` if the rate limit allows it.
    pub fn try_issue(&mut self, now: u64) -> bool {
        while self.0.front().is_some_and(|&t| now.saturating_sub(t) >= WINDOW_S) {
            self.0.pop_front();
        }
        if self.0.len() >= MAX_IN_WINDOW {
            return false;
        }
        self.0.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc6238_vectors_sha1() {
        // RFC 6238 Appendix B, seed "12345678901234567890", 8 digits.
        let seed = b"12345678901234567890";
        assert_eq!(code(seed, 59, 8), "94287082");
        assert_eq!(code(seed, 1111111109, 8), "07081804");
        assert_eq!(code(seed, 1234567890, 8), "89005924");
        // Six digits are the low six of the same value.
        assert_eq!(code(seed, 59, 6), "287082");
    }

    #[test]
    fn three_per_five_minutes() {
        let mut l = IssueLog::default();
        assert!(l.try_issue(0));
        assert!(l.try_issue(10));
        assert!(l.try_issue(20));
        assert!(!l.try_issue(299), "fourth inside the window");
        assert!(l.try_issue(300), "the first has aged out");
        assert!(!l.try_issue(301));
    }

    #[test]
    fn life_is_one_to_thirty() {
        assert_eq!(life_left(0), 30);
        assert_eq!(life_left(29), 1);
    }
}
