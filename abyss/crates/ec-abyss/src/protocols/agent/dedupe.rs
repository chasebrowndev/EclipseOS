// SPDX-License-Identifier: AGPL-3.0-only
//! `(agent, req_id) -> result` for the dedupe window (COMP-08 §2.2, §10 step
//! 2). Not TCB: it remembers answers and replays them; it decides nothing.
//!
//! A repeated `req_id` inside the window gets the stored result with status
//! `duplicate` and the original status in `detail`, and executes nothing. A
//! request that was still waiting (a prompt, a deferral, a batch) when the
//! repeat arrived answers `duplicate` with detail `pending`: the original is
//! still going to run exactly once.
//!
//! Retention is `max(window, prompt_timeout + 30 s)` for a request that
//! entered the prompt path: a client retry after a slow prompt must not run
//! the action twice (A-05 §3).
//!
//! One table per agent, owned by [`super::AgentAux`]. Bounded: when
//! [`MAX_ENTRIES`] live entries are held a new `req_id` is refused as
//! `quota_exceeded` rather than evicting one, since an evicted entry is a
//! request that can run twice.

use std::time::{Duration, Instant};

use ec_protocols::agent::server::eclipse_agent_v1::Status;

/// COMP-08 §2.2 default.
pub const WINDOW: Duration = Duration::from_secs(60);

/// A prompt can take [`crate::trusted_ui::TIMEOUT`]; the retention outlives
/// it by this much (A-05 §3).
const PROMPT_SLACK: Duration = Duration::from_secs(30);

/// Live entries one agent may hold.
pub const MAX_ENTRIES: usize = 4096;

#[derive(Debug)]
struct Entry {
    req_id: u32,
    /// `None` while the request is still running or parked.
    outcome: Option<(u32, String)>,
    expires: Instant,
}

/// What [`Dedupe::admit`] decided.
#[derive(Debug, PartialEq)]
pub enum Admit {
    /// First sight of this `req_id`: run it.
    Fresh,
    /// Seen: answer with this `duplicate` status and detail, run nothing.
    Replay(String),
    /// The table is full of live entries.
    Full,
}

#[derive(Debug, Default)]
pub struct Dedupe {
    entries: Vec<Entry>,
}

impl Dedupe {
    /// Step 2 for `req_id` at `now`.
    pub fn admit(&mut self, req_id: u32, now: Instant) -> Admit {
        if let Some(e) = self
            .entries
            .iter()
            .find(|e| e.req_id == req_id && e.expires > now)
        {
            return Admit::Replay(match &e.outcome {
                None => "pending".to_owned(),
                Some((status, d)) if d.is_empty() => status.to_string(),
                Some((status, d)) => format!("{status}:{d}"),
            });
        }
        // Expired entries (this id's included) go before anything is added.
        if self.entries.len() >= MAX_ENTRIES || self.entries.iter().any(|e| e.req_id == req_id) {
            self.entries.retain(|e| e.expires > now);
        }
        if self.entries.len() >= MAX_ENTRIES {
            return Admit::Full;
        }
        self.entries.push(Entry {
            req_id,
            outcome: None,
            expires: now + WINDOW,
        });
        Admit::Fresh
    }

    /// `req_id` went to the prompt path (or any wait that can outlast the
    /// window): keep it for as long as it could be suspended.
    pub fn suspended(&mut self, req_id: u32, now: Instant) {
        if let Some(e) = self.entries.iter_mut().rev().find(|e| e.req_id == req_id) {
            let keep = now + WINDOW.max(crate::trusted_ui::TIMEOUT + PROMPT_SLACK);
            e.expires = e.expires.max(keep);
        }
    }

    /// The request's one `result` was sent: remember it.
    pub fn complete(&mut self, req_id: u32, status: Status, detail: &str, now: Instant) {
        if let Some(e) = self
            .entries
            .iter_mut()
            .rev()
            .find(|e| e.req_id == req_id && e.outcome.is_none())
        {
            e.outcome = Some((status as u32, detail.to_owned()));
            e.expires = e.expires.max(now + WINDOW);
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repeat_replays_the_original_status_and_never_runs() {
        let t = Instant::now();
        let mut d = Dedupe::default();
        assert_eq!(d.admit(7, t), Admit::Fresh);
        assert_eq!(d.admit(7, t), Admit::Replay("pending".into()), "still running");
        d.complete(7, Status::Ok, "", t);
        assert_eq!(d.admit(7, t), Admit::Replay("0".into()));
        d.complete(8, Status::Ok, "", t);
        assert_eq!(d.admit(8, t), Admit::Fresh, "an unknown id completes nothing");
        d.complete(8, Status::PolicyDenied, "rule-x", t);
        assert_eq!(d.admit(8, t), Admit::Replay("10:rule-x".into()));
    }

    #[test]
    fn the_window_expires_and_a_prompt_outlives_it() {
        let t = Instant::now();
        let mut d = Dedupe::default();
        d.admit(1, t);
        d.complete(1, Status::Ok, "", t);
        d.admit(2, t);
        d.suspended(2, t);
        d.complete(2, Status::PromptTimeout, "", t + Duration::from_secs(120));
        let later = t + WINDOW + Duration::from_secs(1);
        assert_eq!(d.admit(1, later), Admit::Fresh, "plain entry expired");
        assert_eq!(
            d.admit(2, later),
            Admit::Replay("12".into()),
            "a prompted request is kept past the plain window"
        );
        let much_later = t + Duration::from_secs(120 + 61);
        assert_eq!(d.admit(2, much_later), Admit::Fresh);
    }

    #[test]
    fn a_full_table_refuses_instead_of_evicting() {
        let t = Instant::now();
        let mut d = Dedupe::default();
        for i in 0..MAX_ENTRIES as u32 {
            assert_eq!(d.admit(i, t), Admit::Fresh);
        }
        assert_eq!(d.admit(u32::MAX, t), Admit::Full);
        assert_eq!(d.len(), MAX_ENTRIES);
        // Nothing evicted: the oldest is still a replay.
        assert!(matches!(d.admit(0, t), Admit::Replay(_)));
        // Once they expire the table frees itself.
        assert_eq!(
            d.admit(u32::MAX, t + WINDOW + Duration::from_secs(1)),
            Admit::Fresh
        );
    }
}
