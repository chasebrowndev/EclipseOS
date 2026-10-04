// SPDX-License-Identifier: AGPL-3.0-only

//! The daemon's state machine: chords in, annotations out (spec §2).
//!
//! Kept apart from `main.rs` and generic over [`Control`] so the whole loop —
//! a chord, a read, a model reply, what the compositor is told and when —
//! runs in tests against fake stages with no compositor, no tesseract and no
//! model.
//!
//! The model call is the one slow stage (seconds), so it runs on a worker
//! thread and the loop stays live for chords meanwhile. Before that, a
//! dismiss pressed while the model thought was only read after the answer
//! had been drawn, so the answer flashed up and vanished; a second select
//! queued behind the first. One call is in flight at a time (§3.4): a new
//! request, or a dismiss, cancels the running one and its reply is dropped.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::answer::Reply;
use crate::beacon::{Beacon, Eye};
use crate::config::Config;
use crate::fault::{Failure, Fault};
use crate::focus;
use crate::frame::Region;
use crate::hud::{Anchor, Control, Hud, Panel};
use crate::pipeline::{Job, Kind, Pipeline};

/// How often the loop looks for a reply while one is being fetched.
const REPLY_POLL: Duration = Duration::from_millis(20);

/// Automatic mode's back-off after a failure tops out at this many settle
/// intervals, so a capture that is denied is retried every few seconds, not
/// several times a second.
const MAX_BACKOFF_STEPS: u32 = 5;

/// What is on screen, and for how long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    /// An automatic answer or a failure: down at this instant.
    Until(Instant),
    /// A select or expand answer: up until the user dismisses it (§2.1).
    Held,
}

struct Inflight {
    job: Job,
    cancel: Arc<AtomicBool>,
    reply: Receiver<Result<Reply, Failure>>,
    /// Where a failure of this job is reported.
    near: Option<Region>,
}

pub struct Daemon {
    pipeline: Pipeline,
    hud: Hud,
    beacon: Beacon,
    /// The last state given to the beacon.
    eye: Eye,
    auto: bool,
    fail_ms: u64,
    poll_every: Duration,
    next_tick: Instant,
    /// The round-robin cursor, for when the compositor will not say which
    /// output has focus.
    next_output: usize,
    /// The gate's TTL and rate limit are durations, so they need a clock that
    /// only moves forward; wall clock could step under NTP.
    started: Instant,
    shown: Option<Shown>,
    inflight: Option<Inflight>,
    /// The automatic-mode failure last shown. The same failure again is not
    /// shown again: a denied capture used to flash its panel every two
    /// seconds for as long as automatic mode stayed on.
    auto_fault: Option<Failure>,
    backoff: u32,
}

impl Daemon {
    pub fn new(
        pipeline: Pipeline,
        beacon: Beacon,
        auto: bool,
        fail_ms: u64,
        settle_ms: u64,
        now: Instant,
    ) -> Daemon {
        let poll_every = Duration::from_millis(settle_ms.max(1));
        let mut d = Daemon {
            pipeline,
            hud: Hud::new(),
            beacon,
            eye: Eye::Off,
            auto,
            fail_ms,
            poll_every,
            next_tick: now + poll_every,
            next_output: 0,
            started: now,
            shown: None,
            inflight: None,
            auto_fault: None,
            backoff: 0,
        };
        d.rest();
        d
    }

    /// The compositor's `oracle-eyes` settings changed: the next call uses
    /// the new command and timeout, the next tick the new interval.
    pub fn reconfigure(&mut self, cfg: Config, now: Instant) {
        self.poll_every = Duration::from_millis(cfg.settle_ms.max(1));
        self.next_tick = self.next_tick.min(now + self.poll_every);
        self.pipeline.set_config(cfg);
    }

    /// Debug toggled live: the beacon's prefix follows at once.
    pub fn set_debug(&mut self, debug: bool) {
        self.beacon.set_debug(debug);
    }

    /// How long the loop may sleep before [`Daemon::step`] has work, `None`
    /// for "until a chord arrives". Nothing scheduled means no wake-ups.
    pub fn wake_in(&self, now: Instant) -> Option<Duration> {
        let mut at: Option<Instant> = None;
        let mut sooner = |t: Instant| at = Some(at.map_or(t, |a| a.min(t)));
        if self.inflight.is_some() {
            sooner(now + REPLY_POLL);
        }
        if let Some(Shown::Until(t)) = self.shown {
            sooner(t);
        }
        if self.auto_may_run() {
            sooner(self.next_tick);
        }
        at.map(|t| t.saturating_duration_since(now))
    }

    /// One chord from the compositor's keybind event.
    pub fn chord(&mut self, c: &mut impl Control, data: &Value, now: Instant) {
        let action = data.get("action").and_then(Value::as_str);
        // The chord's action name and region only: never the keys behind it.
        tracing::debug!(action = action.unwrap_or("?"), region = ?region_of(data), "chord");
        match action {
            Some("annotation-select") => {
                self.cancel("a new selection");
                self.set_eye(Eye::Think);
                match region_of(data) {
                    Some(r) => match self.pipeline.select(r) {
                        Ok(job) => self.start(job, Some(r)),
                        Err(e) => self.fail(c, &e, Some(r), now),
                    },
                    // The compositor only sends the chord with a region on
                    // commit, so this is a protocol mismatch, not a cancel.
                    None => self.fail(
                        c,
                        &Failure::new(Fault::Capture, "select event carried no region"),
                        None,
                        now,
                    ),
                }
            }
            Some("annotation-expand") => {
                self.cancel("expand");
                self.set_eye(Eye::Think);
                match self.pipeline.expand() {
                    Ok(job) => self.start(job, self.pipeline.last_region()),
                    Err(e) => {
                        let near = self.pipeline.last_region();
                        self.fail(c, &e, near, now)
                    }
                }
            }
            Some("annotation-auto-toggle") => {
                self.auto = !self.auto;
                tracing::info!("automatic mode {}", if self.auto { "on" } else { "off" });
                if self.auto {
                    // A fresh look, now: switching it on is asking for one.
                    self.pipeline.reset_auto();
                    self.auto_fault = None;
                    self.backoff = 0;
                    self.next_tick = now;
                } else if self
                    .inflight
                    .as_ref()
                    .is_some_and(|f| f.job.kind == Kind::Auto)
                {
                    self.cancel("automatic mode off");
                }
                self.rest();
            }
            Some("annotation-dismiss") => {
                self.cancel("dismissed");
                self.shown = None;
                if let Err(e) = self.hud.dismiss(c) {
                    tracing::warn!("hud: {e}");
                }
                self.rest();
            }
            _ => {}
        }
    }

    /// Everything time-driven: a reply landing, an annotation expiring, an
    /// automatic pass.
    pub fn step(&mut self, c: &mut impl Control, now: Instant) {
        self.collect(c, now);

        if let Some(Shown::Until(t)) = self.shown {
            if now >= t {
                self.shown = None;
                tracing::debug!("annotation expired");
                if let Err(e) = self.hud.dismiss(c) {
                    tracing::warn!("hud: {e}");
                }
            }
        }

        if self.auto && now >= self.next_tick {
            self.next_tick = now + self.poll_every;
            if self.auto_may_run() {
                self.auto_pass(c, now);
            }
        }
    }

    /// Automatic mode never interrupts an answer the user is reading, and
    /// never asks while another question is out.
    fn auto_may_run(&self) -> bool {
        self.auto && self.shown.is_none() && self.inflight.is_none()
    }

    fn auto_pass(&mut self, c: &mut impl Control, now: Instant) {
        // The screen the user is actually facing (ADR 0042). If the
        // compositor will not say, fall back to round-robin rather than
        // fixing on one screen forever.
        let regions = match focus::focused_output(c) {
            Some(r) => vec![r],
            None => self.pipeline.output_regions(),
        };
        let region = if regions.is_empty() {
            None
        } else {
            let r = regions[self.next_output % regions.len()];
            self.next_output = self.next_output.wrapping_add(1);
            Some(r)
        };
        let elapsed = now.saturating_duration_since(self.started).as_millis() as u64;
        let pass = match region {
            Some(r) => self.pipeline.auto(r, elapsed),
            None => Err(Failure::new(
                Fault::Capture,
                "no screen to read: the compositor reported no outputs",
            )),
        };
        match pass {
            Ok(None) => self.backoff = 0,
            Ok(Some(job)) => {
                self.backoff = 0;
                self.set_eye(Eye::Think);
                self.start(job, region);
            }
            Err(e) => self.auto_failed(c, &e, now),
        }
    }

    /// Fail visibly (§2.1), once. Automatic mode is unprompted, so a
    /// failure that only reached the journal would be silent degradation;
    /// but the same failure every tick is a strobe, so it is shown once per
    /// distinct cause and retried with a growing back-off.
    fn auto_failed(&mut self, c: &mut impl Control, e: &Failure, now: Instant) {
        self.backoff = (self.backoff + 1).min(MAX_BACKOFF_STEPS);
        self.next_tick = now + self.poll_every * (1u32 << self.backoff);
        if e.fault == Fault::Cancelled {
            return;
        }
        if self.auto_fault.as_ref() == Some(e) {
            tracing::debug!(error = %e, backoff = self.backoff, "auto: same failure, not shown again");
            return;
        }
        self.auto_fault = Some(e.clone());
        tracing::warn!("{e}");
        // There is no subject to stand beside — the pass was about a whole
        // screen — so the failure is unanchored and the compositor places it.
        self.show(c, None, &Panel::failure(e), Some(self.fail_ms), now);
    }

    /// Hand `job` to a worker thread for its model reply.
    fn start(&mut self, job: Job, near: Option<Region>) {
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let asker = self.pipeline.asker();
        let (lines, options) = (job.lines().to_vec(), job.options().to_vec());
        let flag = Arc::clone(&cancel);
        let spawned = std::thread::Builder::new()
            .name("oe-ask".into())
            .spawn(move || {
                let _ = tx.send(asker.ask(&lines, &options, &flag));
            });
        if let Err(e) = spawned {
            // Without a worker there is no reply; say so through the normal
            // path rather than dropping the request.
            let (tx, rx2) = mpsc::channel();
            let _ = tx.send(Err(Failure::new(
                Fault::Command,
                format!("could not start the model call: {e}"),
            )));
            self.inflight = Some(Inflight {
                job,
                cancel,
                reply: rx2,
                near,
            });
            return;
        }
        tracing::debug!(kind = ?job.kind, "daemon: model call started");
        self.inflight = Some(Inflight {
            job,
            cancel,
            reply: rx,
            near,
        });
    }

    /// Drop the call in flight, if any. Its worker kills the model process
    /// and its reply, if one still comes, goes nowhere.
    fn cancel(&mut self, why: &str) {
        if let Some(f) = self.inflight.take() {
            f.cancel.store(true, Ordering::Relaxed);
            tracing::debug!(kind = ?f.job.kind, why, "daemon: model call cancelled");
        }
    }

    /// Land a reply if the worker has one.
    fn collect(&mut self, c: &mut impl Control, now: Instant) {
        let Some(f) = &self.inflight else { return };
        let result = match f.reply.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(Failure::new(
                Fault::Command,
                "the model call ended without a reply",
            )),
        };
        let f = self.inflight.take().expect("checked above");
        self.rest();
        match (f.job.kind, result) {
            (Kind::Select | Kind::Expand, Ok(reply)) => {
                let a = self.pipeline.finish(&f.job, reply);
                self.show(c, Some(a.anchor), &a.panel, None, now);
            }
            (Kind::Auto, Ok(reply)) => {
                self.auto_fault = None;
                let a = self.pipeline.finish(&f.job, reply);
                self.show(c, Some(a.anchor), &a.panel, Some(a.hold_ms), now);
            }
            (Kind::Select | Kind::Expand, Err(e)) => self.fail(c, &e, f.near, now),
            (Kind::Auto, Err(e)) => self.auto_failed(c, &e, now),
        }
    }

    /// A failure the user asked for: shown beside what they pointed at.
    /// The panel carries the class sentence; the detail goes to the log only.
    /// With no region to stand beside it is unanchored.
    fn fail(&mut self, c: &mut impl Control, e: &Failure, near: Option<Region>, now: Instant) {
        self.rest();
        if e.fault == Fault::Cancelled {
            tracing::debug!("{e}");
            return;
        }
        tracing::warn!("{e}");
        self.show(
            c,
            near.map(Anchor::from),
            &Panel::failure(e),
            Some(self.fail_ms),
            now,
        );
    }

    /// Draw, and note how long it stays: `hold_ms` of `None` holds it until
    /// dismissed.
    fn show(
        &mut self,
        c: &mut impl Control,
        at: Option<Anchor>,
        panel: &Panel,
        hold_ms: Option<u64>,
        now: Instant,
    ) {
        match self.hud.show(c, at, panel) {
            Ok(_) => {
                self.shown = Some(match hold_ms {
                    Some(ms) => Shown::Until(now + Duration::from_millis(ms)),
                    None => Shown::Held,
                });
                tracing::debug!(held = hold_ms.is_none(), hold_ms, "daemon: shown");
            }
            // If the compositor will not draw for us there is nowhere left to
            // complain but the journal.
            Err(e) => tracing::warn!("hud: {e}"),
        }
    }

    #[cfg(test)]
    pub fn eye(&self) -> Eye {
        self.eye
    }

    #[cfg(test)]
    pub fn shown(&self) -> Option<Shown> {
        self.shown
    }

    #[cfg(test)]
    pub fn busy(&self) -> bool {
        self.inflight.is_some()
    }

    fn set_eye(&mut self, eye: Eye) {
        self.eye = eye;
        self.beacon.set(eye);
    }

    /// The eye with nothing in flight.
    fn rest(&mut self) {
        let eye = if self.inflight.is_some() {
            Eye::Think
        } else if self.auto {
            Eye::Watch
        } else {
            Eye::Off
        };
        self.set_eye(eye);
    }
}

pub fn region_of(data: &Value) -> Option<Region> {
    let r = data.get("region")?;
    let get = |k: &str| r.get(k)?.as_i64();
    let (x, y, w, h) = (get("x")?, get("y")?, get("w")?, get("h")?);
    if w <= 0 || h <= 0 {
        return None;
    }
    Some(Region {
        x: x as i32,
        y: y as i32,
        w: w as i32,
        h: h as i32,
    })
}
