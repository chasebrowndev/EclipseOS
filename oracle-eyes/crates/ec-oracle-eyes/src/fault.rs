// SPDX-License-Identifier: AGPL-3.0-only

//! Why a pass ended without an answer, sorted by what the user can do
//! about it (§2.1 "fail visibly").
//!
//! The panel says one plain sentence per class. The raw detail — a path, an
//! exit status, the CLI's own error text — goes to the log only: it is for
//! whoever debugs the daemon, and a binary path or an API error string on
//! screen tells the person reading the panel nothing they can act on.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// The model service turned the call away for load or quota.
    Busy,
    /// The model command could not be started, or exited badly.
    Command,
    /// The screen could not be grabbed or recognised.
    Capture,
    /// The read worked and found no text.
    NoText,
    /// The model did not answer within the timeout.
    Timeout,
    /// The model answered with something that is not a reply.
    Reply,
    /// Expand with nothing selected before it.
    NothingSelected,
    /// The user moved on; never shown.
    Cancelled,
}

impl Fault {
    /// The panel body for this class.
    pub fn sentence(self) -> &'static str {
        match self {
            Fault::Busy => "The model is busy. Try again in a minute.",
            Fault::Command => {
                "The model command didn't run. Check the model command in Settings > Oracle Eyes."
            }
            Fault::Capture => "Couldn't read the screen.",
            Fault::NoText => "No readable text in that region.",
            Fault::Timeout => "The model took too long.",
            Fault::Reply => "The model's reply couldn't be read.",
            Fault::NothingSelected => "Select a region first, then expand it.",
            Fault::Cancelled => "Cancelled.",
        }
    }
}

/// A failure: its class, for the panel, and its detail, for the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub fault: Fault,
    pub detail: String,
}

impl Failure {
    pub fn new(fault: Fault, detail: impl Into<String>) -> Failure {
        Failure {
            fault,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.fault, self.detail)
    }
}

/// Whether the CLI's own error text means "busy" rather than "broken". The
/// CLI reports a rate limit, an exhausted usage window and an overloaded API
/// as `is_error` results with text, not with a code of their own.
pub fn is_busy(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    [
        "rate limit",
        "rate_limit",
        "usage limit",
        "limit reached",
        "overloaded",
        "429",
        "529",
        "too many requests",
    ]
    .iter()
    .any(|m| t.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_class_has_its_one_sentence() {
        assert_eq!(
            Fault::Busy.sentence(),
            "The model is busy. Try again in a minute."
        );
        assert_eq!(
            Fault::Command.sentence(),
            "The model command didn't run. Check the model command in Settings > Oracle Eyes."
        );
        assert_eq!(Fault::Capture.sentence(), "Couldn't read the screen.");
        assert_eq!(Fault::NoText.sentence(), "No readable text in that region.");
        assert_eq!(Fault::Timeout.sentence(), "The model took too long.");
        assert_eq!(
            Fault::Reply.sentence(),
            "The model's reply couldn't be read."
        );
    }

    #[test]
    fn busy_is_told_apart_from_broken() {
        assert!(is_busy("API Error: 529 overloaded"));
        assert!(is_busy(
            "Claude usage limit reached. Your limit will reset at 5pm"
        ));
        assert!(is_busy("Rate limit exceeded"));
        assert!(!is_busy("Credit balance is too low"));
        assert!(!is_busy("Invalid API key"));
    }
}
