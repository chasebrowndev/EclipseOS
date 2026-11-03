// SPDX-License-Identifier: AGPL-3.0-only
//! The progress model: feed jobs from `ec-ctl progress`, scan jobs from
//! [`super::scan`], and the one merged list the widget shows.
//!
//! Pure state with an injected clock: every method that cares about time
//! takes `now`, so expiry and elapsed time are tested without sleeping.

use std::time::{Duration, Instant};

use ec_ipc::progress::{err, ok, Request};
use serde_json::{json, Value};

use super::scan::fraction;
use super::{Job, Source};

/// A feed job that has not been updated for this long is dropped. A tool
/// that dies without `done` therefore clears itself.
pub const FEED_TTL: Duration = Duration::from_secs(60);
/// Most feed jobs held at once.
pub const MAX_FEED: usize = 32;

#[derive(Debug, Clone)]
struct FeedJob {
    id: String,
    label: String,
    /// Bytes, when the job was started with `--total`.
    total: Option<u64>,
    fraction: Option<f32>,
    started: Instant,
    last_seen: Instant,
}

#[derive(Debug, Default)]
pub struct Model {
    feed: Vec<FeedJob>,
    scan: Vec<Job>,
}

impl Model {
    /// Replace the scan half. An empty list means nothing was found.
    pub fn set_scan(&mut self, jobs: Vec<Job>) {
        self.scan = jobs;
    }

    pub fn feed_active(&self) -> bool {
        !self.feed.is_empty()
    }

    /// Drop feed jobs that have gone quiet.
    pub fn expire(&mut self, now: Instant) {
        self.feed
            .retain(|j| now.saturating_duration_since(j.last_seen) < FEED_TTL);
    }

    /// Apply one feed request and return the reply object.
    pub fn apply(&mut self, req: Request, now: Instant) -> Value {
        match req {
            Request::Start { id, label, total } => {
                if total == Some(0) {
                    return err("total must be greater than 0");
                }
                let job = FeedJob {
                    id: id.clone(),
                    label,
                    total,
                    fraction: total.map(|_| 0.0),
                    started: now,
                    last_seen: now,
                };
                if let Some(slot) = self.feed.iter_mut().find(|j| j.id == id) {
                    *slot = job;
                } else if self.feed.len() >= MAX_FEED {
                    return err("too many jobs in progress");
                } else {
                    self.feed.push(job);
                }
                ok()
            }
            Request::Fraction { id, fraction } => self.update(&id, now, |j| {
                j.fraction = Some(fraction);
                Ok(())
            }),
            Request::Bytes { id, bytes } => self.update(&id, now, |j| match j.total {
                Some(total) => {
                    j.fraction = fraction(bytes, total);
                    Ok(())
                }
                None => Err("set with bytes needs a total: start the job with --total".into()),
            }),
            Request::Done { id } => {
                let before = self.feed.len();
                self.feed.retain(|j| j.id != id);
                if self.feed.len() == before {
                    err("no such job")
                } else {
                    ok()
                }
            }
            Request::List => self.list(now),
        }
    }

    fn update(
        &mut self,
        id: &str,
        now: Instant,
        change: impl FnOnce(&mut FeedJob) -> Result<(), String>,
    ) -> Value {
        let Some(job) = self.feed.iter_mut().find(|j| j.id == id) else {
            return err("no such job");
        };
        match change(job) {
            Ok(()) => {
                job.last_seen = now;
                ok()
            }
            Err(e) => err(e),
        }
    }

    /// Every job the widget should show, oldest first.
    ///
    /// A scan job is hidden when a feed job already reports the same work:
    /// the label's head (text before any ` (`) matches case-insensitively.
    /// The feed's report is the one with the intended wording.
    pub fn snapshot(&self) -> Vec<Job> {
        let mut out: Vec<Job> = self
            .feed
            .iter()
            .map(|j| Job {
                id: j.id.clone(),
                label: j.label.clone(),
                source: Source::Feed,
                fraction: j.fraction,
                started: j.started,
            })
            .collect();
        let feed_heads: Vec<String> = out.iter().map(|j| head(&j.label)).collect();
        out.extend(
            self.scan
                .iter()
                .filter(|s| !feed_heads.contains(&head(&s.label)))
                .cloned(),
        );
        out.sort_by(|a, b| a.started.cmp(&b.started).then_with(|| a.id.cmp(&b.id)));
        out
    }

    fn list(&self, now: Instant) -> Value {
        let jobs: Vec<Value> = self
            .snapshot()
            .iter()
            .map(|j| {
                json!({
                    "id": j.id,
                    "label": j.label,
                    "source": j.source.as_str(),
                    "fraction": j.fraction,
                    "elapsed_ms": now.saturating_duration_since(j.started).as_millis() as u64,
                })
            })
            .collect();
        json!({"ok": true, "jobs": jobs})
    }
}

/// The label up to any ` (` suffix, lowercased for comparison.
fn head(label: &str) -> String {
    label.split(" (").next().unwrap_or(label).to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(m: &mut Model, id: &str, label: &str, total: Option<u64>, now: Instant) -> Value {
        m.apply(
            Request::Start {
                id: id.into(),
                label: label.into(),
                total,
            },
            now,
        )
    }

    fn is_ok(v: &Value) -> bool {
        v.get("ok") == Some(&Value::Bool(true))
    }

    fn scan_job(id: &str, label: &str, at: Instant) -> Job {
        Job {
            id: id.into(),
            label: label.into(),
            source: Source::Scan,
            fraction: None,
            started: at,
        }
    }

    #[test]
    fn start_set_done_cycle() {
        let t = Instant::now();
        let mut m = Model::default();
        assert!(is_ok(&start(&mut m, "a", "copy", None, t)));
        let snap = m.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].source, Source::Feed);
        assert_eq!(snap[0].fraction, None, "indeterminate without a total");

        let v = m.apply(
            Request::Fraction {
                id: "a".into(),
                fraction: 0.5,
            },
            t,
        );
        assert!(is_ok(&v));
        assert_eq!(m.snapshot()[0].fraction, Some(0.5));

        assert!(is_ok(&m.apply(Request::Done { id: "a".into() }, t)));
        assert!(m.snapshot().is_empty());
    }

    #[test]
    fn bytes_need_a_total_and_map_to_a_fraction() {
        let t = Instant::now();
        let mut m = Model::default();
        start(&mut m, "bare", "no total", None, t);
        let v = m.apply(
            Request::Bytes {
                id: "bare".into(),
                bytes: 5,
            },
            t,
        );
        assert!(!is_ok(&v));

        start(&mut m, "sized", "sized", Some(400), t);
        assert_eq!(
            m.snapshot().iter().find(|j| j.id == "sized").unwrap().fraction,
            Some(0.0)
        );
        m.apply(
            Request::Bytes {
                id: "sized".into(),
                bytes: 100,
            },
            t,
        );
        let j = m.snapshot().into_iter().find(|j| j.id == "sized").unwrap();
        assert_eq!(j.fraction, Some(0.25));
        m.apply(
            Request::Bytes {
                id: "sized".into(),
                bytes: 900,
            },
            t,
        );
        let j = m.snapshot().into_iter().find(|j| j.id == "sized").unwrap();
        assert_eq!(j.fraction, Some(1.0), "overshoot clamps");
    }

    #[test]
    fn unknown_ids_and_zero_totals_are_refused() {
        let t = Instant::now();
        let mut m = Model::default();
        assert!(!is_ok(&m.apply(Request::Done { id: "nope".into() }, t)));
        assert!(!is_ok(&m.apply(
            Request::Fraction {
                id: "nope".into(),
                fraction: 0.1
            },
            t
        )));
        assert!(!is_ok(&start(&mut m, "z", "z", Some(0), t)));
        assert!(m.snapshot().is_empty());
    }

    #[test]
    fn restarting_an_id_resets_it() {
        let t = Instant::now();
        let mut m = Model::default();
        start(&mut m, "a", "first", Some(10), t);
        m.apply(
            Request::Bytes {
                id: "a".into(),
                bytes: 5,
            },
            t,
        );
        start(&mut m, "a", "second", None, t + Duration::from_secs(1));
        let jobs = m.snapshot();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].label, "second");
        assert_eq!(jobs[0].fraction, None);
        assert_eq!(jobs[0].started, t + Duration::from_secs(1));
    }

    #[test]
    fn feed_job_count_is_bounded() {
        let t = Instant::now();
        let mut m = Model::default();
        for i in 0..MAX_FEED {
            assert!(is_ok(&start(&mut m, &format!("j{i}"), "x", None, t)));
        }
        assert!(!is_ok(&start(&mut m, "one-too-many", "x", None, t)));
        // Restarting an existing id is still allowed at the cap.
        assert!(is_ok(&start(&mut m, "j0", "again", None, t)));
    }

    #[test]
    fn stale_feed_jobs_expire_and_updates_refresh_them() {
        let t = Instant::now();
        let mut m = Model::default();
        start(&mut m, "quiet", "quiet", None, t);
        start(&mut m, "busy", "busy", None, t);

        let mid = t + FEED_TTL - Duration::from_secs(1);
        m.apply(
            Request::Fraction {
                id: "busy".into(),
                fraction: 0.1,
            },
            mid,
        );

        let later = t + FEED_TTL + Duration::from_secs(1);
        m.expire(later);
        let ids: Vec<String> = m.snapshot().into_iter().map(|j| j.id).collect();
        assert_eq!(ids, vec!["busy".to_string()]);
        assert!(m.feed_active());

        m.expire(mid + FEED_TTL + Duration::from_millis(1));
        assert!(!m.feed_active());
    }

    #[test]
    fn scan_job_hidden_when_feed_reports_the_same_work() {
        let t = Instant::now();
        let mut m = Model::default();
        start(&mut m, "build", "cargo build", None, t);
        m.set_scan(vec![
            scan_job("pid:1", "cargo build (EclipseOS)", t),
            scan_job("pid:2", "pacman ripgrep", t),
        ]);
        let labels: Vec<String> = m.snapshot().into_iter().map(|j| j.label).collect();
        assert_eq!(
            labels,
            vec!["cargo build".to_string(), "pacman ripgrep".to_string()]
        );
    }

    #[test]
    fn snapshot_merges_both_sources_oldest_first() {
        let t = Instant::now();
        let mut m = Model::default();
        m.set_scan(vec![scan_job("pid:9", "cp → x", t + Duration::from_secs(2))]);
        start(&mut m, "feed", "feed job", None, t + Duration::from_secs(5));
        start(&mut m, "early", "early feed", None, t + Duration::from_secs(1));
        let ids: Vec<String> = m.snapshot().into_iter().map(|j| j.id).collect();
        assert_eq!(ids, vec!["early", "pid:9", "feed"]);

        m.set_scan(vec![]);
        assert_eq!(m.snapshot().len(), 2);
    }

    #[test]
    fn list_reply_carries_source_fraction_and_elapsed() {
        let t = Instant::now();
        let mut m = Model::default();
        start(&mut m, "a", "copy", Some(4), t);
        m.apply(
            Request::Bytes {
                id: "a".into(),
                bytes: 1,
            },
            t,
        );
        let v = m.apply(Request::List, t + Duration::from_millis(1500));
        let job = &v["jobs"][0];
        assert_eq!(job["id"], "a");
        assert_eq!(job["source"], "feed");
        assert_eq!(job["fraction"], json!(0.25));
        assert_eq!(job["elapsed_ms"], 1500);
    }
}
