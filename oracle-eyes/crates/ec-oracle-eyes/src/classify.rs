// SPDX-License-Identifier: AGPL-3.0-only

//! The automatic-mode gate (spec §2.2, §3.3): decide whether a settled
//! screen region is worth spending a model call on.
//!
//! Three filters, cheapest first, all local and all pure:
//!
//! 1. **Triviality** — too few words, nothing alphabetic, or OCR confidence
//!    too low. Nav chrome, clocks and usernames die here for free.
//! 2. **Dedup** — a *near*-duplicate match with a TTL (spec §5: 5 min), so a
//!    paragraph is not re-answered every time its window regains focus. An
//!    exact hash was not enough: one character of OCR noise, a blinking
//!    cursor or a ticking clock changed it and bought another model call.
//!    Entries are sets of character-trigram hashes, compared by Jaccard
//!    against [`Policy::dedup_similarity`]. Character n-grams, not word
//!    ones: OCR noise is a *character* error, and a word-level shingle turns
//!    one misread letter into a whole window of misses — four of them, for a
//!    4-word window — which is exactly the false negative this replaced.
//!    Hashes only, never text: human input is
//!    never logged or held by content. A hit refreshes the entry, so a screen
//!    left up stays quiet, and it adopts the new shingles so slow drift does
//!    not eventually cross the threshold in one step.
//!    Entries are scoped: two monitors showing the same page are two
//!    entries, so one output cannot suppress the other.
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
use std::collections::HashSet;
use std::fmt;
use std::hash::{Hash, Hasher};

/// Characters per shingle. Three is short enough that a single misread
/// letter costs only three windows out of a sentence's fifty, and long
/// enough that unrelated prose still overlaps by a couple of percent rather
/// than a couple of tenths.
const SHINGLE: usize = 3;

/// Tuning values from spec §5. `Default` is that table; the caller is free
/// to override every field from `policy.kdl`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Policy {
    /// How long a seen screen suppresses a re-ask.
    pub dedup_ttl_ms: u64,
    /// Minimum gap between dispatched queries.
    pub min_query_interval_ms: u64,
    /// Fewer words than this is not a topic.
    pub min_words: usize,
    /// OCR mean confidence floor, 0.0–1.0.
    pub min_confidence: f32,
    /// Jaccard overlap of shingle sets at or above which two reads of a
    /// region are the same screen, 0.0–1.0.
    pub dedup_similarity: f32,
}

impl Default for Policy {
    fn default() -> Policy {
        Policy {
            dedup_ttl_ms: 5 * 60 * 1000,
            min_query_interval_ms: 3000,
            min_words: 4,
            min_confidence: 0.55,
            dedup_similarity: 0.8,
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
    /// Near enough to something seen inside the TTL.
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

/// One remembered screen: whose it was, what it looked like, and when.
#[derive(Debug)]
struct Seen {
    scope: u64,
    shingles: HashSet<u64>,
    at_ms: u64,
}

/// The automatic-mode gate. Holds the dedup table and the last dispatch
/// time; owns no clock of its own.
#[derive(Debug)]
pub struct Gate {
    policy: Policy,
    last_query_ms: Option<u64>,
    seen: Vec<Seen>,
}

impl Gate {
    pub fn new(policy: Policy) -> Gate {
        Gate {
            policy,
            last_query_ms: None,
            seen: Vec::new(),
        }
    }

    #[cfg(test)]
    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// Decide on one candidate. `text` must already be redacted — the dedup
    /// shingles are taken over exactly what would be sent, so a page whose
    /// only change is inside a redacted span correctly counts as unchanged.
    ///
    /// `scope` identifies where the text came from — in practice a hash of
    /// the owning output's logical rect. Two outputs showing the same page
    /// are independent: neither suppresses the other.
    ///
    /// `confidence` is OCR's mean confidence for the region, 0.0–1.0.
    /// `now_ms` is any monotonic millisecond clock.
    ///
    /// A `Verdict::Ask` records the dispatch: the caller is expected to
    /// actually make the call.
    pub fn consider(&mut self, scope: u64, text: &str, confidence: f32, now_ms: u64) -> Verdict {
        if let Some(r) = self.triviality(text, confidence) {
            return Verdict::Skip(r);
        }

        self.expire(now_ms);
        let shingles = shingles(text);
        let threshold = self.policy.dedup_similarity;
        if let Some(entry) = self
            .seen
            .iter_mut()
            .find(|s| s.scope == scope && jaccard(&s.shingles, &shingles) >= threshold)
        {
            // Refresh rather than let a still-visible page age out and get
            // re-asked while nothing about it changed, and adopt the new
            // shingles so a slowly drifting page never accumulates enough
            // difference to read as new in a single step.
            entry.at_ms = now_ms;
            entry.shingles = shingles;
            return Verdict::Skip(Reason::Duplicate);
        }

        if let Some(last) = self.last_query_ms {
            if now_ms.saturating_sub(last) < self.policy.min_query_interval_ms {
                // Dropped on the floor. Nothing is remembered, so the next
                // candidate after the window competes on its own merits.
                return Verdict::Skip(Reason::RateLimited);
            }
        }

        self.seen.push(Seen {
            scope,
            shingles,
            at_ms: now_ms,
        });
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
        self.seen.retain(|s| now_ms.saturating_sub(s.at_ms) < ttl);
    }
}

impl Default for Gate {
    fn default() -> Gate {
        Gate::new(Policy::default())
    }
}

/// The hashed [`SHINGLE`]-character windows of `text`, case- and
/// punctuation-insensitive so OCR's usual wobble (a stray comma, a capital
/// read as lowercase, an `l` read as `I`) does not count as a different
/// screen. Text shorter than one window hashes whole, so short candidates
/// still compare.
fn shingles(text: &str) -> HashSet<u64> {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect();
    let flat: Vec<char> = words.join(" ").chars().collect();

    let mut out = HashSet::new();
    if flat.len() < SHINGLE {
        out.insert(hash(&flat.iter().collect::<String>()));
        return out;
    }
    for window in flat.windows(SHINGLE) {
        out.insert(hash(&window.iter().collect::<String>()));
    }
    out
}

/// Set overlap, 0.0–1.0. Two empty sets are identical.
fn jaccard(a: &HashSet<u64>, b: &HashSet<u64>) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let inter = a.intersection(b).count() as f32;
    let union = (a.len() + b.len()) as f32 - inter;
    if union == 0.0 {
        return 0.0;
    }
    inter / union
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

    /// One output. Tests that care about output identity say so.
    const ONE: u64 = 1;

    fn gate() -> Gate {
        Gate::default()
    }

    #[test]
    fn asks_for_a_real_paragraph() {
        assert_eq!(gate().consider(ONE, GOOD, 0.9, 0), Verdict::Ask);
    }

    #[test]
    fn skips_too_few_words() {
        assert_eq!(
            gate().consider(ONE, "Settings", 0.99, 0),
            Verdict::Skip(Reason::TooFewWords)
        );
    }

    #[test]
    fn skips_non_alphabetic() {
        assert_eq!(
            gate().consider(ONE, "12:04 99 -- 3.1 // 42", 0.99, 0),
            Verdict::Skip(Reason::NoAlphabetic)
        );
    }

    #[test]
    fn skips_low_confidence() {
        assert_eq!(
            gate().consider(ONE, GOOD, 0.2, 0),
            Verdict::Skip(Reason::LowConfidence)
        );
    }

    #[test]
    fn skips_duplicate_within_ttl() {
        let mut g = gate();
        assert_eq!(g.consider(ONE, GOOD, 0.9, 0), Verdict::Ask);
        assert_eq!(
            g.consider(ONE, GOOD, 0.9, 60_000),
            Verdict::Skip(Reason::Duplicate)
        );
    }

    #[test]
    fn asks_again_after_ttl_expiry() {
        let mut g = gate();
        assert_eq!(g.consider(ONE, GOOD, 0.9, 0), Verdict::Ask);
        let past_ttl = g.policy().dedup_ttl_ms + 1;
        assert_eq!(g.consider(ONE, GOOD, 0.9, past_ttl), Verdict::Ask);
    }

    #[test]
    fn a_dedup_hit_refreshes_the_entry() {
        let mut g = gate();
        assert_eq!(g.consider(ONE, GOOD, 0.9, 0), Verdict::Ask);
        // Touched just before it would age out…
        assert_eq!(
            g.consider(ONE, GOOD, 0.9, 299_000),
            Verdict::Skip(Reason::Duplicate)
        );
        // …so past the original deadline it is still suppressed.
        assert_eq!(
            g.consider(ONE, GOOD, 0.9, 301_000),
            Verdict::Skip(Reason::Duplicate)
        );
    }

    #[test]
    fn changed_screen_after_a_dedup_hit_is_asked_again() {
        let mut g = gate();
        assert_eq!(g.consider(ONE, GOOD, 0.9, 0), Verdict::Ask);
        assert_eq!(
            g.consider(ONE, GOOD, 0.9, 100),
            Verdict::Skip(Reason::Duplicate)
        );
        // New text, past the rate-limit window: the screen changed, ask.
        assert_eq!(g.consider(ONE, OTHER, 0.9, 4000), Verdict::Ask);
    }

    #[test]
    fn rate_limit_drops_rather_than_queues() {
        let mut g = gate();
        assert_eq!(g.consider(ONE, GOOD, 0.9, 0), Verdict::Ask);
        assert_eq!(
            g.consider(ONE, OTHER, 0.9, 500),
            Verdict::Skip(Reason::RateLimited)
        );
        // If the drop had been queued, this third candidate would wait
        // behind OTHER. It does not: it is dispatched on its own, and OTHER
        // is simply gone.
        assert_eq!(
            g.consider(ONE, "a third distinct question about policy", 0.9, 3100),
            Verdict::Ask
        );
        assert_eq!(
            g.consider(ONE, OTHER, 0.9, 7000),
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
            dedup_similarity: 0.8,
        });
        assert_eq!(g.consider(ONE, "hi", 0.0, 0), Verdict::Ask);
        assert_eq!(g.consider(ONE, "there", 0.0, 1), Verdict::Ask);
        assert_eq!(g.consider(ONE, "hi", 0.0, 11), Verdict::Ask);
    }

    #[test]
    fn one_character_of_ocr_noise_is_still_the_same_screen() {
        let mut g = gate();
        assert_eq!(g.consider(ONE, GOOD, 0.9, 0), Verdict::Ask);
        // Tesseract read one letter wrong and dropped the comma it invented.
        let noisy = "what does the annotation pass actualIy guarantee here";
        assert_eq!(
            g.consider(ONE, noisy, 0.9, 4000),
            Verdict::Skip(Reason::Duplicate),
            "an exact-hash dedup would have bought a second model call here"
        );
    }

    #[test]
    fn genuinely_different_text_is_not_absorbed_as_a_duplicate() {
        let mut g = gate();
        assert_eq!(g.consider(ONE, GOOD, 0.9, 0), Verdict::Ask);
        assert_eq!(g.consider(ONE, OTHER, 0.9, 4000), Verdict::Ask);
    }

    #[test]
    fn the_same_page_on_two_outputs_does_not_collide() {
        let (left, right) = (1, 2);
        let mut g = gate();
        assert_eq!(g.consider(left, GOOD, 0.9, 0), Verdict::Ask);
        // Same text, other monitor: its own entry, so the right-hand screen
        // is not permanently suppressed by the left-hand one.
        assert_eq!(g.consider(right, GOOD, 0.9, 4000), Verdict::Ask);
        assert_eq!(
            g.consider(left, GOOD, 0.9, 8000),
            Verdict::Skip(Reason::Duplicate)
        );
    }
}
