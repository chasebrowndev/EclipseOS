// SPDX-License-Identifier: AGPL-3.0-only

//! The COMP-14 §2.1 and §2.1b budget tables, as data.
//!
//! A benchmark names a [`Budget`] and the harness decides pass/fail against it,
//! so the numbers live in exactly one place and a spec change is a one-line
//! diff here. Every budget in the spec is stated as a p99 (§2.1's "hard fail"
//! column is the same percentile, not a maximum), so [`Budget::verdict`]
//! compares against the p99 of the samples.

use std::time::Duration;

/// One row of the spec's latency table.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    /// The spec section this row comes from, e.g. `"COMP-14 §2.1"`.
    pub cite: &'static str,
    /// What is being measured, in the spec's own words.
    pub what: &'static str,
    /// The target. Above this is a regression worth reporting.
    pub target: Duration,
    /// Above this the build fails. `None` where the spec states a target only.
    pub hard_fail: Option<Duration>,
}

/// The outcome of measuring a path against its budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// p99 at or under target.
    Pass,
    /// p99 over target but under the hard fail — reported, does not fail CI.
    Over,
    /// p99 over the hard fail, or over target where the spec states no hard
    /// fail. Fails the build.
    Fail,
}

impl Budget {
    const fn new(cite: &'static str, what: &'static str, target: Duration) -> Self {
        Self {
            cite,
            what,
            target,
            hard_fail: None,
        }
    }

    const fn hard(mut self, hard_fail: Duration) -> Self {
        self.hard_fail = Some(hard_fail);
        self
    }

    /// Judge a measured p99 against this row.
    pub fn verdict(&self, p99: Duration) -> Verdict {
        if p99 <= self.target {
            Verdict::Pass
        } else if self.hard_fail.is_some_and(|h| p99 <= h) {
            Verdict::Over
        } else {
            Verdict::Fail
        }
    }
}

const fn us(n: u64) -> Duration {
    Duration::from_micros(n)
}

const fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

// --- COMP-14 §2.1 — latency ------------------------------------------------

/// libinput event -> client dispatch.
pub const INPUT_DISPATCH: Budget = Budget::new("COMP-14 §2.1", "input event processing", us(200)).hard(ms(1));

/// The one milestone 16 gates on.
pub const POLICY_CHECK: Budget = Budget::new("COMP-14 §2.1", "policy check()", us(50)).hard(us(200));

/// `click` request -> result. The spec states "3 ms + 1 frame"; the frame is
/// measured by the frame benchmarks, so this row is the request half alone.
pub const AGENT_CLICK: Budget = Budget::new("COMP-14 §2.1", "click request to result", ms(3)).hard(ms(10));

/// `list_toplevels` over 50 windows.
pub const LIST_TOPLEVELS_50: Budget =
    Budget::new("COMP-14 §2.1", "list_toplevels, 50 windows", ms(1)).hard(ms(5));

/// `get_tree` native, 5k nodes.
pub const GET_TREE_5K: Budget = Budget::new("COMP-14 §2.1", "get_tree native, 5k nodes", ms(5)).hard(ms(20));

/// `get_text` over a 1k-node subtree.
pub const GET_TEXT_1K: Budget = Budget::new("COMP-14 §2.1", "get_text subtree, 1k nodes", ms(2)).hard(ms(10));

/// `wait_for` wake after its condition holds.
pub const WAIT_FOR_WAKE: Budget =
    Budget::new("COMP-14 §2.1", "wait_for wake after condition", ms(1)).hard(ms(5));

/// `capture_toplevel`, 1080p, to a dmabuf.
pub const CAPTURE_1080P: Budget =
    Budget::new("COMP-14 §2.1", "capture_toplevel 1080p to dmabuf", ms(8)).hard(ms(25));

/// Config hot reload, edit to applied.
pub const CONFIG_RELOAD: Budget = Budget::new("COMP-14 §2.1", "config hot reload", ms(50)).hard(ms(500));

// --- COMP-14 §2.1b / A-14 — policy-internal paths --------------------------
//
// These state a budget with no separate hard fail, so exceeding the budget is
// itself the failure.

/// `classify()` recompute for one surface (S-05 §5).
pub const CLASSIFY_SURFACE: Budget = Budget::new("COMP-14 §2.1b", "classify() one surface", us(50));

/// The irreversible matcher inside `check()` (S-06 §3.1).
pub const IRREVERSIBLE_MATCH: Budget =
    Budget::new("COMP-14 §2.1b", "irreversible matcher in check()", us(20));

/// Provenance resolution of up to 8 chain ids, summary lookup only.
pub const PROVENANCE_8: Budget = Budget::new("COMP-14 §2.1b", "provenance resolution, 8 chain ids", us(30));

/// Egress proxy added latency in splice mode.
pub const EGRESS_SPLICE: Budget = Budget::new("COMP-14 §2.1b", "egress proxy added latency, splice", ms(2));

/// Egress proxy added latency in MITM mode.
pub const EGRESS_MITM: Budget = Budget::new("COMP-14 §2.1b", "egress proxy added latency, MITM", ms(8));
