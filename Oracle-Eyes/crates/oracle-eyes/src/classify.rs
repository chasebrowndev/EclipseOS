// SPDX-License-Identifier: AGPL-3.0-only

//! The automatic-mode gate (spec §2.2, §3.3): decide whether a settled
//! screen region is worth spending a model call on.
//!
//! Three filters, cheapest first, all local and all pure:
//!
//! 1. **Triviality** — too few words, nothing alphabetic, or OCR confidence
//!    too low. Nav chrome, clocks and usernames die here for free.
//! 2. **Dedup** — a hash of the *redacted* text with a TTL (spec §5: 5 min),
//!    so a paragraph is not re-answered every time its window regains focus.
//!    Hashes only, never text: human input is never logged or held by
//!    content. A hit refreshes the entry, so a screen left up stays quiet.
//! 3. **Rate limit** — at most one query per interval (spec §5: 3s).
//!    Extras are **dropped, not queued**: spec §3.3 is explicit that a queue
//!    would fight the dedup TTL, since the region has probably changed by
//!    the time a queued entry would be served.
//!
//! Every threshold lives on [`Policy`], which the caller constructs — the
//! §5 table is defaults, not constants. The clock is an argument on every
//! decision (`now_ms`, any monotonic millisecond source) so the whole gate
//! is testable without sleeping.
//!
//! Nothing here touches the `ocr` module: the input is a string and a
//! confidence, which keeps the two halves independently replaceable.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};

/// Tuning values from spec §5. `Default` is that table; the caller is free
/// to override every field from `policy.kdl`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Policy {
    /// How long a seen hash suppresses a re-ask.
    pub dedup_ttl_ms: u64,
    /// Minimum gap between dispatched queries.
    pub min_query_interval_ms: u64,
    /// Fewer words than this is not a topic.
    pub min_words: usize,
    /// OCR mean confidence floor, 0.0–1.0.
    pub min_confidence: f32,
}

impl Default for Policy {
    fn default() -> Policy {
        Policy {
            dedup_ttl_ms: 5 * 60 * 1000,
            min_query_interval_ms: 3000,
            min_words: 4,
            min_confidence: 0.55,
        }
    }
}

/// Why the gate did nothing. The daemon must be able to say this out loud —
/// "fail visibly" (CLAUDE.md) applies to the boring refusals too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Below [`Policy::min_words`].
    TooFewWords,
    /// Digits, punctuation or box-drawing noise, but no letters.
    NoAlphabetic,
    /// Below [`Policy::min_confidence`].
    LowConfidence,
    /// Same text seen inside the TTL.
    Duplicate,
    /// Inside [`Policy::min_query_interval_ms`] of the last dispatch.
    RateLimited,
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Reason::TooFewWords => "too few words",
            Reason::NoAlphabetic => "no alphabetic content",
            Reason::LowConfidence => "OCR confidence too low",
            Reason::Duplicate => "already answered recently",
            Reason::RateLimited => "rate limited",
        };
        f.write_str(s)
    }
}

/// The gate's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Ask,
    Skip(Reason),
}

/// The automatic-mode gate. Holds the dedup table and the last dispatch
/// time; owns no clock of its own.
#[derive(Debug)]
pub struct Gate {
    policy: Policy,
    last_query_ms: Option<u64>,
    seen: HashMap<u64, u64>,
}

impl Gate {
    pub fn new(policy: Policy) -> Gate {
        Gate {
            policy,
            last_query_ms: None,
            seen: HashMap::new(),
        }
    }

    /// Decide on one candidate. `text` must already be redacted — the dedup
    /// hash is taken over exactly what would be sent, so a page whose only
    /// change is inside a redacted span correctly counts as unchanged.
    ///
    /// `confidence` is OCR's mean confidence for the region, 0.0–1.0.
    /// `now_ms` is any monotonic millisecond clock.
    ///
    /// A `Verdict::Ask` records the dispatch: the caller is expected to
    /// actually make the call.
    #[cfg(test)]
    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    pub fn consider(&mut self, text: &str, confidence: f32, now_ms: u64) -> Verdict {
        if let Some(r) = self.triviality(text, confidence) {
            return Verdict::Skip(r);
        }

        self.expire(now_ms);
        let key = hash(text);
        if let Some(seen) = self.seen.get_mut(&key) {
            // Refresh rather than let a still-visible page age out and get
            // re-asked while nothing about it changed.
            *seen = now_ms;
            return Verdict::Skip(Reason::Duplicate);
        }

        if let Some(last) = self.last_query_ms {
            if now_ms.saturating_sub(last) < self.policy.min_query_interval_ms {
                // Dropped on the floor. Nothing is remembered, so the next
                // candidate after the window competes on its own merits.
                return Verdict::Skip(Reason::RateLimited);
            }
        }

        self.seen.insert(key, now_ms);
        self.last_query_ms = Some(now_ms);
        Verdict::Ask
    }

    fn triviality(&self, text: &str, confidence: f32) -> Option<Reason> {
        if confidence < self.policy.min_confidence {
            return Some(Reason::LowConfidence);
        }
        if text.split_whitespace().count() < self.policy.min_words {
            return Some(Reason::TooFewWords);
        }
        if !text.chars().any(char::is_alphabetic) {
            return Some(Reason::NoAlphabetic);
        }
        None
    }

    fn expire(&mut self, now_ms: u64) {
        let ttl = self.policy.dedup_ttl_ms;
        self.seen.retain(|_, seen| now_ms.saturating_sub(*seen) < ttl);
    }
}

impl Default for Gate {
    fn default() -> Gate {
        Gate::new(Policy::default())
    }
}

fn hash(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "what does the annotation pass actually guarantee here";
    const OTHER: &str = "why is capture a separate capability from control";

    fn gate() -> Gate {
        Gate::default()
    }

    #[test]
    fn asks_for_a_real_paragraph() {
        assert_eq!(gate().consider(GOOD, 0.9, 0), Verdict::Ask);
    }

    #[test]
    fn skips_too_few_words() {
        assert_eq!(
            gate().consider("Settings", 0.99, 0),
            Verdict::Skip(Reason::TooFewWords)
        );
    }

    #[test]
    fn skips_non_alphabetic() {
        assert_eq!(
            gate().consider("12:04 99 -- 3.1 // 42", 0.99, 0),
            Verdict::Skip(Reason::NoAlphabetic)
        );
    }

    #[test]
    fn skips_low_confidence() {
        assert_eq!(
            gate().consider(GOOD, 0.2, 0),
            Verdict::Skip(Reason::LowConfidence)
        );
    }

    #[test]
    fn skips_duplicate_within_ttl() {
        let mut g = gate();
        assert_eq!(g.consider(GOOD, 0.9, 0), Verdict::Ask);
        assert_eq!(g.consider(GOOD, 0.9, 60_000), Verdict::Skip(Reason::Duplicate));
    }

    #[test]
    fn asks_again_after_ttl_expiry() {
        let mut g = gate();
        assert_eq!(g.consider(GOOD, 0.9, 0), Verdict::Ask);
        let past_ttl = g.policy().dedup_ttl_ms + 1;
        assert_eq!(g.consider(GOOD, 0.9, past_ttl), Verdict::Ask);
    }

    #[test]
    fn a_dedup_hit_refreshes_the_entry() {
        let mut g = gate();
        assert_eq!(g.consider(GOOD, 0.9, 0), Verdict::Ask);
        // Touched just before it would age out…
        assert_eq!(g.consider(GOOD, 0.9, 299_000), Verdict::Skip(Reason::Duplicate));
        // …so past the original deadline it is still suppressed.
        assert_eq!(g.consider(GOOD, 0.9, 301_000), Verdict::Skip(Reason::Duplicate));
    }

    #[test]
    fn changed_screen_after_a_dedup_hit_is_asked_again() {
        let mut g = gate();
        assert_eq!(g.consider(GOOD, 0.9, 0), Verdict::Ask);
        assert_eq!(g.consider(GOOD, 0.9, 100), Verdict::Skip(Reason::Duplicate));
        // New text, past the rate-limit window: the screen changed, ask.
        assert_eq!(g.consider(OTHER, 0.9, 4000), Verdict::Ask);
    }

    #[test]
    fn rate_limit_drops_rather_than_queues() {
        let mut g = gate();
        assert_eq!(g.consider(GOOD, 0.9, 0), Verdict::Ask);
        assert_eq!(g.consider(OTHER, 0.9, 500), Verdict::Skip(Reason::RateLimited));
        // If the drop had been queued, this third candidate would wait
        // behind OTHER. It does not: it is dispatched on its own, and OTHER
        // is simply gone.
        assert_eq!(
            g.consider("a third distinct question about policy", 0.9, 3100),
            Verdict::Ask
        );
        assert_eq!(
            g.consider(OTHER, 0.9, 7000),
            Verdict::Ask,
            "a dropped candidate is not remembered as answered"
        );
    }

    #[test]
    fn policy_is_configuration_not_constants() {
        let mut g = Gate::new(Policy {
            min_words: 1,
            min_query_interval_ms: 0,
            dedup_ttl_ms: 10,
            min_confidence: 0.0,
        });
        assert_eq!(g.consider("hi", 0.0, 0), Verdict::Ask);
        assert_eq!(g.consider("there", 0.0, 1), Verdict::Ask);
        assert_eq!(g.consider("hi", 0.0, 11), Verdict::Ask);
    }
}
