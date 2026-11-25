// SPDX-License-Identifier: AGPL-3.0-only
//! The progress model: feed jobs from `ec-ctl progress`, scan jobs from
//! [`super::scan`], and the one merged list the widget shows.
//!
//! Pure state with an injected clock: every method that cares about time
//! takes `now`, so expiry, elapsed time and ETAs are tested without sleeping.
//!
//! # ETA
//!
//! A job's [`Eta`] comes from, in order of trust:
//!
//! 1. **Reported**: the tool sent `eta` with a recent `set`. It counts down
//!    from when it arrived and is trusted for [`REPORT_FRESH`].
//! 2. **Rate**: measured from the job's own fraction samples. A bounded ring
//!    of timestamped samples and a time-weighted moving average of the rate
//!    (fraction per second); `remaining = (1 - f) / rate`, counted down
//!    between samples. Needs [`MIN_SAMPLES`] samples spanning [`MIN_SPAN`]. A
//!    job that makes no progress for [`STALL`] loses its rate ETA rather than
//!    showing a growing lie.
//! 3. **History**: the median of past runs of the same label, minus elapsed;
//!    only for a job whose fraction is unknown or whose rate is not yet
//!    measurable (never for a stalled one).
//!
//! `None` when nothing qualifies, or the fraction is already 1.0.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use ec_ipc::progress::{err, ok, Request};
use serde_json::{json, Value};

use super::history::{self, History};
use super::scan::fraction;
use super::{Eta, EtaBasis, Job, Source};

/// A feed job that has not been updated for this long is dropped. A tool
/// that dies without `done` therefore clears itself.
pub const FEED_TTL: Duration = Duration::from_secs(60);
/// Most feed jobs held at once.
pub const MAX_FEED: usize = 32;

/// Samples kept per job.
pub const RING: usize = 32;
/// Fewest samples a rate ETA needs.
pub const MIN_SAMPLES: usize = 3;
/// Least time the samples must span for a rate ETA.
pub const MIN_SPAN: Duration = Duration::from_secs(2);
/// No progress for this long drops the rate ETA.
pub const STALL: Duration = Duration::from_secs(15);
/// Samples closer than this coalesce into one.
const MIN_STEP: Duration = Duration::from_millis(250);
/// Time constant of the rate average, in seconds.
const TAU: f64 = 10.0;
/// A rate ETA beyond this is no ETA at all.
pub const MAX_RATE_ETA: Duration = Duration::from_secs(24 * 3600);
/// How long a tool-reported ETA is trusted after it arrives.
pub const REPORT_FRESH: Duration = Duration::from_secs(30);

/// Fraction samples and the smoothed rate for one job.
#[derive(Debug, Clone, Default)]
pub struct RateTracker {
    samples: VecDeque<(Instant, f32)>,
    /// Fraction per second, time-weighted average.
    rate: Option<f64>,
    last_progress: Option<Instant>,
}

/// What a tracker can say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RateEta {
    Known(Duration),
    /// Too few samples, or too short a span, to say.
    Unknown,
    /// Not advancing, or advancing too slowly to mean anything.
    Stalled,
}

impl RateTracker {
    /// Record the fraction at `now`. A fraction that goes backwards means the
    /// job restarted, so measurement starts over.
    pub fn push(&mut self, now: Instant, f: f32) {
        let Some(&(t, last)) = self.samples.back() else {
            self.samples.push_back((now, f));
            self.last_progress = Some(now);
            return;
        };
        if f < last - 1e-4 {
            *self = Self::default();
            self.push(now, f);
            return;
        }
        if f <= last {
            return;
        }
        let dt = now.saturating_duration_since(t);
        self.last_progress = Some(now);
        if dt < MIN_STEP {
            if let Some(b) = self.samples.back_mut() {
                b.1 = f;
            }
            return;
        }
        let inst = f64::from(f - last) / dt.as_secs_f64();
        let alpha = 1.0 - (-dt.as_secs_f64() / TAU).exp();
        self.rate = Some(match self.rate {
            None => inst,
            Some(r) => r + alpha * (inst - r),
        });
        self.samples.push_back((now, f));
        if self.samples.len() > RING {
            self.samples.pop_front();
        }
    }

    fn eta(&self, now: Instant) -> RateEta {
        let (Some(&(first, _)), Some(&(last_t, last_f))) = (self.samples.front(), self.samples.back()) else {
            return RateEta::Unknown;
        };
        if self.samples.len() < MIN_SAMPLES || last_t.saturating_duration_since(first) < MIN_SPAN {
            return RateEta::Unknown;
        }
        let quiet = self
            .last_progress
            .map_or(Duration::MAX, |p| now.saturating_duration_since(p));
        let Some(rate) = self.rate.filter(|r| *r > 1e-9) else {
            return RateEta::Stalled;
        };
        if quiet > STALL {
            return RateEta::Stalled;
        }
        let left = f64::from(1.0 - last_f) / rate - now.saturating_duration_since(last_t).as_secs_f64();
        if left > MAX_RATE_ETA.as_secs_f64() {
            return RateEta::Stalled;
        }
        RateEta::Known(whole_secs(left))
    }
}

/// Seconds rounded up, at least one: `0s left` is not something to promise.
fn whole_secs(secs: f64) -> Duration {
    Duration::from_secs(secs.max(0.0).ceil().max(1.0) as u64)
}

#[derive(Debug, Clone)]
struct FeedJob {
    id: String,
    label: String,
    /// Bytes, when the job was started with `--total`.
    total: Option<u64>,
    fraction: Option<f32>,
    started: Instant,
    last_seen: Instant,
    rate: RateTracker,
    /// The tool's own ETA and when it arrived.
    reported: Option<(u64, Instant)>,
}

#[derive(Debug, Default)]
pub struct Model {
    feed: Vec<FeedJob>,
    scan: Vec<Job>,
    scan_rates: HashMap<String, RateTracker>,
    history: History,
    history_dirty: bool,
}

impl Model {
    /// Install the stored history. Does not mark it dirty.
    pub fn set_history(&mut self, h: History) {
        self.history = h;
        self.history_dirty = false;
    }

    /// The history, once, if a run has been recorded since the last call.
    pub fn take_history_if_dirty(&mut self) -> Option<History> {
        std::mem::take(&mut self.history_dirty).then(|| self.history.clone())
    }

    /// Replace the scan half. An empty list means nothing was found. A scan
    /// job that has gone since the last scan is taken to have finished, and
    /// its run is recorded, unless a feed job reports the same work (the feed
    /// records that) or the tool has no history.
    pub fn set_scan(&mut self, jobs: Vec<Job>, now: Instant) {
        let feed_heads: Vec<String> = self.feed.iter().map(|j| head(&j.label)).collect();
        let gone: Vec<(String, Instant)> = self
            .scan
            .iter()
            .filter(|old| !jobs.iter().any(|j| j.id == old.id))
            .filter(|old| history::scan_eligible(&old.label) && !feed_heads.contains(&head(&old.label)))
            .map(|old| (old.label.clone(), old.started))
            .collect();
        for (label, started) in gone {
            self.record(&label, now.saturating_duration_since(started));
        }
        self.scan_rates.retain(|id, _| jobs.iter().any(|j| &j.id == id));
        for j in &jobs {
            if let Some(f) = j.fraction {
                self.scan_rates.entry(j.id.clone()).or_default().push(now, f);
            }
        }
        self.scan = jobs;
    }

    /// Drop every scan job without recording anything: scanning was turned
    /// off, which says nothing about whether the work finished.
    pub fn clear_scan(&mut self) {
        self.scan.clear();
        self.scan_rates.clear();
    }

    fn record(&mut self, label: &str, took: Duration) {
        if let Some(k) = history::key(label) {
            if self.history.record(&k, took) {
                self.history_dirty = true;
            }
        }
    }

    pub fn feed_active(&self) -> bool {
        !self.feed.is_empty()
    }

    /// Drop feed jobs that have gone quiet. An expired job is not recorded.
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
                let mut rate = RateTracker::default();
                if total.is_some() {
                    rate.push(now, 0.0);
                }
                let job = FeedJob {
                    id: id.clone(),
                    label,
                    total,
                    fraction: total.map(|_| 0.0),
                    started: now,
                    last_seen: now,
                    rate,
                    reported: None,
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
            Request::Fraction { id, fraction, eta } => self.update(&id, now, eta, |j| {
                j.fraction = Some(fraction);
                Ok(())
            }),
            Request::Bytes { id, bytes, eta } => self.update(&id, now, eta, |j| match j.total {
                Some(total) => {
                    j.fraction = fraction(bytes, total);
                    Ok(())
                }
                None => Err("set with bytes needs a total: start the job with --total".into()),
            }),
            Request::Done { id } => match self.feed.iter().position(|j| j.id == id) {
                Some(i) => {
                    let job = self.feed.remove(i);
                    self.record(&job.label, now.saturating_duration_since(job.started));
                    ok()
                }
                None => err("no such job"),
            },
            Request::Abort { id } => {
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
        eta: Option<u64>,
        change: impl FnOnce(&mut FeedJob) -> Result<(), String>,
    ) -> Value {
        let Some(job) = self.feed.iter_mut().find(|j| j.id == id) else {
            return err("no such job");
        };
        match change(job) {
            Ok(()) => {
                job.last_seen = now;
                if let Some(f) = job.fraction {
                    job.rate.push(now, f);
                }
                // Each report replaces the last: a tool that stops sending
                // its own figure stops being quoted.
                job.reported = eta.map(|s| (s, now));
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
    pub fn snapshot(&self, now: Instant) -> Vec<Job> {
        let mut out: Vec<Job> = self
            .feed
            .iter()
            .map(|j| Job {
                id: j.id.clone(),
                label: j.label.clone(),
                source: Source::Feed,
                fraction: j.fraction,
                started: j.started,
                eta: self.eta(
                    now,
                    j.started,
                    j.fraction,
                    j.reported,
                    Some(&j.rate),
                    &j.label,
                    true,
                ),
            })
            .collect();
        let feed_heads: Vec<String> = out.iter().map(|j| head(&j.label)).collect();
        out.extend(
            self.scan
                .iter()
                .filter(|s| !feed_heads.contains(&head(&s.label)))
                .map(|s| Job {
                    eta: self.eta(
                        now,
                        s.started,
                        s.fraction,
                        None,
                        self.scan_rates.get(&s.id),
                        &s.label,
                        history::scan_eligible(&s.label),
                    ),
                    ..s.clone()
                }),
        );
        out.sort_by(|a, b| a.started.cmp(&b.started).then_with(|| a.id.cmp(&b.id)));
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn eta(
        &self,
        now: Instant,
        started: Instant,
        fraction: Option<f32>,
        reported: Option<(u64, Instant)>,
        rate: Option<&RateTracker>,
        label: &str,
        use_history: bool,
    ) -> Option<Eta> {
        if fraction.is_some_and(|f| f >= 1.0) {
            return None;
        }
        if let Some((secs, at)) = reported {
            let age = now.saturating_duration_since(at);
            let left = secs as f64 - age.as_secs_f64();
            if age < REPORT_FRESH && left > 0.0 {
                return Some(Eta {
                    remaining: whole_secs(left),
                    basis: EtaBasis::Reported,
                });
            }
        }
        let measured = match (fraction, rate) {
            (Some(_), Some(r)) => r.eta(now),
            _ => RateEta::Unknown,
        };
        match measured {
            RateEta::Known(remaining) => Some(Eta {
                remaining,
                basis: EtaBasis::Rate,
            }),
            RateEta::Stalled => None,
            RateEta::Unknown if use_history => {
                let key = history::key(label)?;
                let left = self
                    .history
                    .estimate(&key, now.saturating_duration_since(started))?;
                Some(Eta {
                    remaining: whole_secs(left.as_secs_f64()),
                    basis: EtaBasis::History,
                })
            }
            RateEta::Unknown => None,
        }
    }

    fn list(&self, now: Instant) -> Value {
        let jobs: Vec<Value> = self
            .snapshot(now)
            .iter()
            .map(|j| {
                json!({
                    "id": j.id,
                    "label": j.label,
                    "source": j.source.as_str(),
                    "fraction": j.fraction,
                    "elapsed_ms": now.saturating_duration_since(j.started).as_millis() as u64,
                    "eta_ms": j.eta.map(|e| e.remaining.as_millis() as u64),
                    "eta_basis": j.eta.map(|e| e.basis.as_str()),
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

    fn frac(m: &mut Model, id: &str, f: f32, eta: Option<u64>, now: Instant) -> Value {
        m.apply(
            Request::Fraction {
                id: id.into(),
                fraction: f,
                eta,
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
            eta: None,
        }
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn eta_of(m: &Model, id: &str, now: Instant) -> Option<Eta> {
        m.snapshot(now).into_iter().find(|j| j.id == id).unwrap().eta
    }

    /// A model whose history already knows `label` took these many seconds.
    fn with_history(label: &str, runs: &[u64]) -> Model {
        let mut h = History::default();
        let k = history::key(label).unwrap();
        for r in runs {
            assert!(h.record(&k, secs(*r)));
        }
        let mut m = Model::default();
        m.set_history(h);
        m
    }

    #[test]
    fn start_set_done_cycle() {
        let t = Instant::now();
        let mut m = Model::default();
        assert!(is_ok(&start(&mut m, "a", "copy", None, t)));
        let snap = m.snapshot(t);
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].source, Source::Feed);
        assert_eq!(snap[0].fraction, None, "indeterminate without a total");

        assert!(is_ok(&frac(&mut m, "a", 0.5, None, t)));
        assert_eq!(m.snapshot(t)[0].fraction, Some(0.5));

        assert!(is_ok(&m.apply(Request::Done { id: "a".into() }, t)));
        assert!(m.snapshot(t).is_empty());
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
                eta: None,
            },
            t,
        );
        assert!(!is_ok(&v));

        start(&mut m, "sized", "sized", Some(400), t);
        let get = |m: &Model| m.snapshot(t).into_iter().find(|j| j.id == "sized").unwrap();
        assert_eq!(get(&m).fraction, Some(0.0));
        let bytes = |m: &mut Model, b| {
            m.apply(
                Request::Bytes {
                    id: "sized".into(),
                    bytes: b,
                    eta: None,
                },
                t,
            )
        };
        bytes(&mut m, 100);
        assert_eq!(get(&m).fraction, Some(0.25));
        bytes(&mut m, 900);
        assert_eq!(get(&m).fraction, Some(1.0), "overshoot clamps");
    }

    #[test]
    fn unknown_ids_and_zero_totals_are_refused() {
        let t = Instant::now();
        let mut m = Model::default();
        assert!(!is_ok(&m.apply(Request::Done { id: "nope".into() }, t)));
        assert!(!is_ok(&frac(&mut m, "nope", 0.1, None, t)));
        assert!(!is_ok(&start(&mut m, "z", "z", Some(0), t)));
        assert!(m.snapshot(t).is_empty());
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
                eta: Some(40),
            },
            t,
        );
        let later = t + secs(1);
        start(&mut m, "a", "second", None, later);
        let jobs = m.snapshot(later);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].label, "second");
        assert_eq!(jobs[0].fraction, None);
        assert_eq!(jobs[0].started, later);
        assert_eq!(jobs[0].eta, None, "the old report does not survive a restart");
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

        let mid = t + FEED_TTL - secs(1);
        frac(&mut m, "busy", 0.1, None, mid);

        let later = t + FEED_TTL + secs(1);
        m.expire(later);
        let ids: Vec<String> = m.snapshot(later).into_iter().map(|j| j.id).collect();
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
        m.set_scan(
            vec![
                scan_job("pid:1", "cargo build (EclipseOS)", t),
                scan_job("pid:2", "pacman ripgrep", t),
            ],
            t,
        );
        let labels: Vec<String> = m.snapshot(t).into_iter().map(|j| j.label).collect();
        assert_eq!(
            labels,
            vec!["cargo build".to_string(), "pacman ripgrep".to_string()]
        );
    }

    #[test]
    fn snapshot_merges_both_sources_oldest_first() {
        let t = Instant::now();
        let mut m = Model::default();
        m.set_scan(vec![scan_job("pid:9", "cp → x", t + secs(2))], t);
        start(&mut m, "feed", "feed job", None, t + secs(5));
        start(&mut m, "early", "early feed", None, t + secs(1));
        let ids: Vec<String> = m.snapshot(t + secs(6)).into_iter().map(|j| j.id).collect();
        assert_eq!(ids, vec!["early", "pid:9", "feed"]);

        m.set_scan(vec![], t);
        assert_eq!(m.snapshot(t).len(), 2);
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
                eta: None,
            },
            t,
        );
        let v = m.apply(Request::List, t + Duration::from_millis(1500));
        let job = &v["jobs"][0];
        assert_eq!(job["id"], "a");
        assert_eq!(job["source"], "feed");
        assert_eq!(job["fraction"], json!(0.25));
        assert_eq!(job["elapsed_ms"], 1500);
        assert_eq!(job["eta_ms"], Value::Null);
        assert_eq!(job["eta_basis"], Value::Null);
    }

    // ---- ETA ----

    /// A job advancing 10% every 2 s, sampled at 2 s intervals.
    fn steady(m: &mut Model, t: Instant, upto: u32) {
        start(m, "j", "copy", Some(100), t);
        for i in 1..=upto {
            m.apply(
                Request::Bytes {
                    id: "j".into(),
                    bytes: u64::from(i) * 10,
                    eta: None,
                },
                t + secs(u64::from(i) * 2),
            );
        }
    }

    #[test]
    fn no_rate_eta_before_three_samples_spanning_two_seconds() {
        let t = Instant::now();
        let mut m = Model::default();
        steady(&mut m, t, 1);
        // Start (0%) and one update: two samples.
        assert_eq!(eta_of(&m, "j", t + secs(2)), None);
        m.apply(
            Request::Bytes {
                id: "j".into(),
                bytes: 20,
                eta: None,
            },
            t + secs(4),
        );
        assert!(eta_of(&m, "j", t + secs(4)).is_some(), "three samples over 4 s");

        // Three samples inside a second: not enough span.
        let mut m = Model::default();
        start(&mut m, "q", "q", Some(100), t);
        for (ms, b) in [(400, 10), (800, 20)] {
            m.apply(
                Request::Bytes {
                    id: "q".into(),
                    bytes: b,
                    eta: None,
                },
                t + Duration::from_millis(ms),
            );
        }
        assert_eq!(eta_of(&m, "q", t + Duration::from_millis(800)), None);
    }

    #[test]
    fn steady_progress_gives_a_rate_eta_that_counts_down() {
        let t = Instant::now();
        let mut m = Model::default();
        steady(&mut m, t, 3); // 30% at 6 s, 5%/s
        let e = eta_of(&m, "j", t + secs(6)).unwrap();
        assert_eq!(e.basis, EtaBasis::Rate);
        assert_eq!(e.remaining, secs(14));
        // Quiet for 3 s: counts down by itself.
        let e = eta_of(&m, "j", t + secs(9)).unwrap();
        assert_eq!(e.remaining, secs(11));
        assert_eq!(e.remaining.subsec_nanos(), 0, "whole seconds");
    }

    #[test]
    fn rate_is_smoothed_not_last_interval() {
        let t = Instant::now();
        let mut m = Model::default();
        steady(&mut m, t, 5); // 50% at 10 s, steady 5%/s
                              // One slow interval: 1% in 2 s.
        m.apply(
            Request::Bytes {
                id: "j".into(),
                bytes: 51,
                eta: None,
            },
            t + secs(12),
        );
        let e = eta_of(&m, "j", t + secs(12)).unwrap();
        // Raw last-interval rate would say 98 s; the average is far kinder.
        assert!(e.remaining < secs(60), "{:?}", e.remaining);
        assert!(e.remaining > secs(10), "{:?}", e.remaining);
    }

    #[test]
    fn stalled_job_drops_its_rate_eta() {
        let t = Instant::now();
        let mut m = Model::default();
        steady(&mut m, t, 3);
        let last = t + secs(6);
        assert!(eta_of(&m, "j", last + STALL).is_some());
        assert_eq!(eta_of(&m, "j", last + STALL + secs(1)), None);
        // Updates that do not advance the fraction do not revive it.
        frac(&mut m, "j", 0.3, None, last + STALL + secs(2));
        assert_eq!(eta_of(&m, "j", last + STALL + secs(3)), None);
        // Progress resumes: it returns.
        m.apply(
            Request::Bytes {
                id: "j".into(),
                bytes: 40,
                eta: None,
            },
            last + STALL + secs(4),
        );
        assert!(eta_of(&m, "j", last + STALL + secs(4)).is_some());
    }

    #[test]
    fn full_fraction_has_no_eta_and_a_slow_crawl_has_none() {
        let t = Instant::now();
        let mut m = Model::default();
        steady(&mut m, t, 10);
        assert_eq!(eta_of(&m, "j", t + secs(20)), None, "100% is done");

        // 0.001% per second: more than a day to go.
        let mut m = Model::default();
        start(&mut m, "s", "slow", Some(1_000_000), t);
        for i in 1..=4u64 {
            m.apply(
                Request::Bytes {
                    id: "s".into(),
                    bytes: i,
                    eta: None,
                },
                t + secs(i * 2),
            );
        }
        assert_eq!(eta_of(&m, "s", t + secs(8)), None);
    }

    #[test]
    fn a_fraction_that_goes_backwards_restarts_the_measurement() {
        let t = Instant::now();
        let mut m = Model::default();
        steady(&mut m, t, 4);
        assert!(eta_of(&m, "j", t + secs(8)).is_some());
        m.apply(
            Request::Bytes {
                id: "j".into(),
                bytes: 5,
                eta: None,
            },
            t + secs(10),
        );
        assert_eq!(eta_of(&m, "j", t + secs(10)), None, "one sample again");
    }

    #[test]
    fn sample_ring_is_bounded() {
        let mut r = RateTracker::default();
        let t = Instant::now();
        for i in 0..200u32 {
            r.push(t + secs(u64::from(i)), i as f32 / 1000.0);
        }
        assert_eq!(r.samples.len(), RING);
    }

    #[test]
    fn reported_eta_wins_and_counts_down_then_goes_stale() {
        let t = Instant::now();
        let mut m = Model::default();
        steady(&mut m, t, 3);
        let at = t + secs(6);
        m.apply(
            Request::Bytes {
                id: "j".into(),
                bytes: 40,
                eta: Some(100),
            },
            at + secs(2),
        );
        let e = eta_of(&m, "j", at + secs(2)).unwrap();
        assert_eq!((e.basis, e.remaining), (EtaBasis::Reported, secs(100)));
        let e = eta_of(&m, "j", at + secs(12)).unwrap();
        assert_eq!((e.basis, e.remaining), (EtaBasis::Reported, secs(90)));
        // Past the freshness window the tool's figure is no longer quoted. The
        // job has also been quiet past the stall limit, so nothing replaces it.
        assert_eq!(eta_of(&m, "j", at + secs(2) + REPORT_FRESH), None);
    }

    #[test]
    fn reported_eta_is_replaced_by_the_next_set_and_runs_out() {
        let t = Instant::now();
        let mut m = Model::default();
        start(&mut m, "a", "tool", None, t);
        frac(&mut m, "a", 0.5, Some(10), t);
        // Counted all the way down: nothing left to quote.
        assert_eq!(eta_of(&m, "a", t + secs(10)), None);
        frac(&mut m, "a", 0.6, Some(50), t + secs(1));
        assert!(eta_of(&m, "a", t + secs(2)).is_some());
        frac(&mut m, "a", 0.7, None, t + secs(3));
        let e = eta_of(&m, "a", t + secs(3));
        assert!(
            e.is_none_or(|e| e.basis != EtaBasis::Reported),
            "a bare set drops the report"
        );
    }

    #[test]
    fn reported_zero_is_not_quoted_and_full_fraction_hides_a_report() {
        let t = Instant::now();
        let mut m = Model::default();
        start(&mut m, "a", "tool", None, t);
        frac(&mut m, "a", 0.5, Some(0), t);
        assert_eq!(eta_of(&m, "a", t), None);
        frac(&mut m, "a", 1.0, Some(30), t + secs(1));
        assert_eq!(eta_of(&m, "a", t + secs(1)), None);
    }

    #[test]
    fn history_estimates_an_unknown_fraction_job_and_says_so() {
        let t = Instant::now();
        let mut m = with_history("cargo build (EclipseOS)", &[100, 120]);
        m.set_scan(vec![scan_job("pid:1", "cargo build (EclipseOS)", t)], t);
        let e = eta_of(&m, "pid:1", t + secs(30)).unwrap();
        assert_eq!(e.basis, EtaBasis::History);
        assert_eq!(e.remaining, secs(80));
        // A different project has no history.
        m.set_scan(vec![scan_job("pid:2", "cargo build (Other)", t)], t);
        assert_eq!(eta_of(&m, "pid:2", t + secs(30)), None);
    }

    #[test]
    fn history_is_not_used_for_copies_or_when_rate_is_stalled() {
        let t = Instant::now();
        let mut m = with_history("cp \u{2192} usb", &[100, 100]);
        m.set_scan(vec![scan_job("pid:1", "cp \u{2192} usb", t)], t);
        assert_eq!(eta_of(&m, "pid:1", t + secs(10)), None);

        let mut m = with_history("copy", &[100, 100]);
        steady(&mut m, t, 3);
        // Relabel not needed: the feed job is "copy" and eligible, but a
        // stalled measurement must not fall back to the median.
        assert_eq!(eta_of(&m, "j", t + secs(6) + STALL + secs(1)), None);
    }

    #[test]
    fn rate_beats_history_and_reported_beats_rate() {
        let t = Instant::now();
        let mut m = with_history("copy", &[1000, 1000]);
        steady(&mut m, t, 3);
        assert_eq!(eta_of(&m, "j", t + secs(6)).unwrap().basis, EtaBasis::Rate);
        m.apply(
            Request::Bytes {
                id: "j".into(),
                bytes: 40,
                eta: Some(7),
            },
            t + secs(8),
        );
        assert_eq!(eta_of(&m, "j", t + secs(8)).unwrap().basis, EtaBasis::Reported);
    }

    #[test]
    fn feed_done_records_history_once_after_three_seconds() {
        let t = Instant::now();
        let mut m = Model::default();
        start(&mut m, "a", "cargo build (X)", None, t);
        m.apply(Request::Done { id: "a".into() }, t + secs(2));
        assert!(m.take_history_if_dirty().is_none(), "under 3 s is not recorded");

        start(&mut m, "a", "cargo build (X)", None, t);
        m.apply(Request::Done { id: "a".into() }, t + secs(40));
        let h = m.take_history_if_dirty().expect("recorded");
        assert_eq!(h.samples("cargo build (x)"), &[40]);
        assert!(m.take_history_if_dirty().is_none(), "dirty flag is taken");
    }

    #[test]
    fn an_aborted_job_leaves_without_a_record() {
        let t = Instant::now();
        let mut m = Model::default();
        start(&mut m, "a", "cargo build (X)", None, t);
        assert!(is_ok(&m.apply(Request::Abort { id: "a".into() }, t + secs(90))));
        assert!(m.snapshot(t + secs(90)).is_empty());
        assert!(m.take_history_if_dirty().is_none());
        assert!(!is_ok(&m.apply(Request::Abort { id: "a".into() }, t)));
    }

    #[test]
    fn expired_feed_jobs_and_restarts_are_never_recorded() {
        let t = Instant::now();
        let mut m = Model::default();
        start(&mut m, "a", "x", None, t);
        m.expire(t + FEED_TTL + secs(1));
        start(&mut m, "b", "x", None, t);
        start(&mut m, "b", "x", None, t + secs(30));
        assert!(m.take_history_if_dirty().is_none());
        assert!(m.history.is_empty());
    }

    #[test]
    fn a_vanished_scan_job_is_recorded_unless_the_feed_covers_it() {
        let t = Instant::now();
        let mut m = Model::default();
        m.set_scan(vec![scan_job("pid:1", "cargo test (A)", t)], t + secs(1));
        m.set_scan(vec![], t + secs(21));
        let h = m.take_history_if_dirty().expect("recorded");
        assert_eq!(h.samples("cargo test (a)"), &[21]);

        // A copy has no history.
        m.set_scan(vec![scan_job("pid:2", "cp \u{2192} x", t)], t);
        m.set_scan(vec![], t + secs(60));
        assert!(m.take_history_if_dirty().is_none());

        // The feed already reports this work, so the scan does not double up.
        start(&mut m, "f", "cargo build", None, t);
        m.set_scan(vec![scan_job("pid:3", "cargo build (A)", t)], t);
        m.set_scan(vec![], t + secs(60));
        assert!(m.take_history_if_dirty().is_none());
    }

    #[test]
    fn turning_the_scan_off_records_nothing() {
        let t = Instant::now();
        let mut m = Model::default();
        m.set_scan(vec![scan_job("pid:1", "cargo build", t)], t);
        m.clear_scan();
        assert!(m.take_history_if_dirty().is_none());
        assert!(m.snapshot(t).is_empty());
    }

    #[test]
    fn scan_copy_with_a_fraction_gets_a_rate_eta() {
        let t = Instant::now();
        let mut m = Model::default();
        for i in 0..4u64 {
            let mut j = scan_job("pid:7", "cp \u{2192} x", t);
            j.fraction = Some(0.1 * (i + 1) as f32);
            m.set_scan(vec![j], t + secs(i * 2));
        }
        let e = eta_of(&m, "pid:7", t + secs(6)).unwrap();
        assert_eq!(e.basis, EtaBasis::Rate);
        assert!(e.remaining >= secs(10) && e.remaining <= secs(18), "{e:?}");
    }

    #[test]
    fn list_reply_carries_eta_and_basis() {
        let t = Instant::now();
        let mut m = Model::default();
        start(&mut m, "a", "tool", None, t);
        frac(&mut m, "a", 0.5, Some(90), t);
        let v = m.apply(Request::List, t);
        assert_eq!(v["jobs"][0]["eta_ms"], 90_000);
        assert_eq!(v["jobs"][0]["eta_basis"], "reported");
    }
}
