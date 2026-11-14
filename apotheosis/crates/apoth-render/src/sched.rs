// SPDX-License-Identifier: AGPL-3.0-only
//! The frame scheduler (T-07 §2, §3.1; T-23 §3 rule 3; RENDERING-06, -70, -71; PERFORMANCE-01
//! to -06, -26, -34, -67): pure, fake-clock-testable state machines. Every method takes the
//! current time as an argument; nothing here reads a clock, arms a timer or touches Wayland.
//! `apoth-wl` and the render thread feed it events and arm the single one-shot timer that
//! [`Poll::Wait`] names.
//!
//! * Zero periodic wakeups: [`Scheduler::poll`] returns [`Poll::Idle`] on a static screen and
//!   [`Poll::Wait`] carries the one deadline (if any) the caller must arm. There is no other
//!   timer.
//! * Frame gate: `Idle -> Requested -> Received`. A frame renders when the window is dirty and
//!   no callback is pending ([`Gate::Requested`] blocks, nothing else does). The callback is
//!   requested by [`Scheduler::rendered`], which the caller invokes only after the commit
//!   (never a callback without a commit). The gate is off under `Fifo`, and when the
//!   compositor offers no frame callbacks.
//! * Coalescing: the first dirtying read since the last frame arms the upper timer (half the
//!   measured refresh interval, at most 8 ms; 8 ms until presentation feedback arrived);
//!   every further read re-arms the lower timer (0.5 ms). Render on whichever fires first.
//!   A lone read therefore waits for the upper timer.
//! * Echo bypass: damage within `render.echo-window` of a local key skips both timers but
//!   not the gate.
//! * Mode 2026: [`Scheduler::sync_begin`] holds presentation; the hold ends at
//!   [`Scheduler::sync_end`] or after the sync timeout (default 1 s, configurable;
//!   `performance { sync-timeout-ms }`), whichever is first. Release renders at once (gate
//!   permitting).
//! * Occluded or suspended surfaces render nothing ([`Scheduler::set_suspended`]).
//!
//! DELIBERATE READING of "the gate is off under Fifo": with `Fifo` the gate is off even when
//! the compositor offers frame callbacks, because the WSI paces and the render thread blocks in
//! `get_current_texture` (T-07 §2 step 4).

use crate::surface::PresentKind;
use std::cell::Cell;
use std::time::{Duration, Instant};

/// Monotonic nanoseconds, supplied by the caller.
pub type Ns = u64;

fn ns(d: Duration) -> Ns {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

/// A monotonic clock, injectable so budgets and timers test without sleeping.
pub trait Clock {
    /// Time since an arbitrary fixed origin.
    fn now(&self) -> Duration;
}

/// The real clock.
#[derive(Clone, Copy, Debug)]
pub struct SystemClock {
    origin: Instant,
}

impl Default for SystemClock {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}

/// A clock moved by hand (tests, benches).
#[derive(Debug, Default)]
pub struct ManualClock(Cell<Duration>);

impl ManualClock {
    /// Moves the clock forward.
    pub fn advance(&self, d: Duration) {
        self.0.set(self.0.get() + d);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Duration {
        self.0.get()
    }
}

/// How rendering is paced (the ladder of T-07 §2, minus the explicit-timing step, which is
/// M38).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pacing {
    /// `wl_surface.frame` callbacks gate rendering.
    FrameCallback,
    /// No callbacks; present mode `Mailbox`: never block in acquire or present.
    Mailbox,
    /// `Fifo`: the WSI paces, the gate is off, the render thread may block in acquire.
    FifoUngated,
}

impl Pacing {
    /// Picks the first available step. `Fifo` turns the gate off whatever the compositor
    /// offers.
    #[must_use]
    pub fn choose(frame_callbacks: bool, present: PresentKind) -> Self {
        if present == PresentKind::Fifo {
            Self::FifoUngated
        } else if frame_callbacks {
            Self::FrameCallback
        } else {
            Self::Mailbox
        }
    }

    /// Is the frame gate on?
    #[must_use]
    pub fn gated(self) -> bool {
        self == Self::FrameCallback
    }
}

/// The frame-callback state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    /// No callback has been requested.
    Idle,
    /// A callback is in flight.
    Requested,
    /// The callback fired.
    Received,
}

/// Scheduler tunables.
#[derive(Clone, Copy, Debug)]
pub struct SchedConfig {
    /// `render.echo-window`.
    pub echo_window: Duration,
    /// The lower coalescing timer.
    pub lower: Duration,
    /// Upper bound of the upper timer, and its value until feedback has arrived.
    pub upper_cap: Duration,
    /// Mode 2026 sync timeout (`performance { sync-timeout-ms }`).
    pub sync_timeout: Duration,
}

impl Default for SchedConfig {
    fn default() -> Self {
        Self {
            echo_window: Duration::from_millis(30),
            lower: Duration::from_micros(500),
            upper_cap: Duration::from_millis(8),
            sync_timeout: Duration::from_secs(1),
        }
    }
}

/// What the render thread should do now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Poll {
    /// Nothing is pending and no timer is armed (`IDLE`).
    Idle,
    /// Not yet. Arm one one-shot timer for the deadline if there is one; with `None` the
    /// next event (frame callback, damage, sync end) wakes the loop.
    Wait(Option<Ns>),
    /// Render and present now.
    Render(RenderNow),
}

/// Details of a render decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderNow {
    /// The frame answers a local key (echo window): rasterise glyph misses inline.
    pub echo: bool,
    /// Send `wl_surface.frame` before presenting (gate on).
    pub request_frame: bool,
    /// A mode 2026 hold ended by timeout rather than by `sync_end`.
    pub sync_timed_out: bool,
}

/// The scheduler state machine. One per window.
#[derive(Clone, Debug)]
pub struct Scheduler {
    cfg: SchedConfig,
    pacing: Pacing,
    gate: Gate,
    dirty: bool,
    echo: bool,
    upper: Option<Ns>,
    lower: Option<Ns>,
    refresh: Option<Ns>,
    last_key: Option<Ns>,
    hold: Option<Ns>,
    hold_timed_out: bool,
    suspended: bool,
}

impl Scheduler {
    /// A clean, idle scheduler.
    #[must_use]
    pub fn new(cfg: SchedConfig, pacing: Pacing) -> Self {
        Self {
            cfg,
            pacing,
            gate: Gate::Idle,
            dirty: false,
            echo: false,
            upper: None,
            lower: None,
            refresh: None,
            last_key: None,
            hold: None,
            hold_timed_out: false,
            suspended: false,
        }
    }

    /// The pacing in use.
    #[must_use]
    pub fn pacing(&self) -> Pacing {
        self.pacing
    }

    /// The pacing changed (surface reconfigured, present mode fell back). Resets the gate.
    pub fn set_pacing(&mut self, pacing: Pacing) {
        self.pacing = pacing;
        self.gate = Gate::Idle;
    }

    /// The gate state (tests, diagnostics).
    #[must_use]
    pub fn gate(&self) -> Gate {
        self.gate
    }

    /// The window has pending damage.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// A local key event (starts the echo window).
    pub fn note_key(&mut self, now: Ns) {
        self.last_key = Some(now);
    }

    fn upper_interval(&self) -> Ns {
        let cap = ns(self.cfg.upper_cap);
        self.refresh.map_or(cap, |r| (r / 2).min(cap))
    }

    /// Output or any other damage arrived at `now`. Returns true on the clean to dirty
    /// transition: the only time the render thread is notified (edge-triggered wakeups).
    pub fn damage(&mut self, now: Ns) -> bool {
        let edge = !self.dirty;
        self.dirty = true;
        if self.suspended {
            return false;
        }
        let in_echo = self
            .last_key
            .is_some_and(|k| now >= k && now - k <= ns(self.cfg.echo_window));
        if in_echo {
            self.echo = true;
        } else {
            if self.upper.is_none() {
                self.upper = Some(now + self.upper_interval());
            } else {
                self.lower = Some(now + ns(self.cfg.lower));
            }
        }
        edge
    }

    /// The `wl_surface.frame` callback fired.
    pub fn frame_callback(&mut self) {
        if self.gate == Gate::Requested {
            self.gate = Gate::Received;
        }
    }

    /// `wp_presentation` feedback arrived with the measured refresh interval (0: unknown).
    pub fn presentation_feedback(&mut self, refresh: Duration) {
        if !refresh.is_zero() {
            self.refresh = Some(ns(refresh));
        }
    }

    /// Mode 2026 began: hold presentation until `sync_end` or the sync timeout.
    pub fn sync_begin(&mut self, now: Ns) {
        if self.hold.is_none() {
            self.hold = Some(now + ns(self.cfg.sync_timeout));
        }
    }

    /// Mode 2026 ended: render the held state at once (gate permitting).
    pub fn sync_end(&mut self, now: Ns) {
        if self.hold.take().is_some() && self.dirty {
            self.upper = Some(now);
        }
    }

    /// The surface became occluded or suspended (`true`) or visible again (`false`).
    /// Suspended surfaces render nothing; on resume the state is dirty, the gate is reset
    /// (callbacks stopped while hidden) and the next frame renders at once.
    pub fn set_suspended(&mut self, suspended: bool, now: Ns) {
        if suspended == self.suspended {
            return;
        }
        self.suspended = suspended;
        if suspended {
            self.upper = None;
            self.lower = None;
            self.echo = false;
        } else {
            self.gate = Gate::Idle;
            self.dirty = true;
            self.upper = Some(now);
        }
    }

    /// What to do at `now`.
    pub fn poll(&mut self, now: Ns) -> Poll {
        if self.suspended || !self.dirty {
            return Poll::Idle;
        }
        if let Some(deadline) = self.hold {
            if now < deadline {
                return Poll::Wait(Some(deadline));
            }
            self.hold = None;
            self.hold_timed_out = true;
            self.upper = Some(now);
        }
        if self.pacing.gated() && self.gate == Gate::Requested {
            return Poll::Wait(None);
        }
        let fired = |t: Option<Ns>| t.is_some_and(|t| now >= t);
        if self.echo || fired(self.upper) || fired(self.lower) {
            return Poll::Render(RenderNow {
                echo: self.echo,
                request_frame: self.pacing.gated(),
                sync_timed_out: self.hold_timed_out,
            });
        }
        let next = [self.upper, self.lower]
            .into_iter()
            .flatten()
            .filter(|t| *t > now)
            .min();
        Poll::Wait(next)
    }

    /// A frame was rendered and committed (call after `present` returns). Clears the damage
    /// and timers; with the gate on, a callback was requested with that commit.
    pub fn rendered(&mut self) {
        self.dirty = false;
        self.echo = false;
        self.upper = None;
        self.lower = None;
        self.hold_timed_out = false;
        if self.pacing.gated() {
            self.gate = Gate::Requested;
        }
    }

    /// The frame could not be presented (acquire timed out). Nothing was committed, so no
    /// callback was requested; the damage stays and a retry is armed as an ordinary read.
    pub fn skipped(&mut self, now: Ns) {
        self.upper = Some(now + self.upper_interval());
        self.lower = None;
        self.echo = false;
        self.hold_timed_out = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Ns = 1_000_000;

    fn sched(p: Pacing) -> Scheduler {
        Scheduler::new(SchedConfig::default(), p)
    }

    fn render(echo: bool, frame: bool) -> Poll {
        Poll::Render(RenderNow {
            echo,
            request_frame: frame,
            sync_timed_out: false,
        })
    }

    #[test]
    fn static_screen_has_no_timer() {
        let mut s = sched(Pacing::FrameCallback);
        assert_eq!(s.poll(0), Poll::Idle);
        assert_eq!(s.poll(10_000 * MS), Poll::Idle);
        s.damage(0);
        assert_eq!(s.poll(9 * MS), render(false, true));
        s.rendered();
        assert_eq!(s.poll(10 * MS), Poll::Idle, "nothing armed after the frame");
        s.frame_callback();
        assert_eq!(s.poll(500 * MS), Poll::Idle);
    }

    #[test]
    fn lone_read_waits_for_the_upper_timer() {
        let mut s = sched(Pacing::FrameCallback);
        assert!(s.damage(0));
        assert_eq!(s.poll(MS), Poll::Wait(Some(8 * MS)));
        assert_eq!(s.poll(8 * MS), render(false, true));
    }

    #[test]
    fn further_reads_arm_the_lower_timer_and_it_wins() {
        let mut s = sched(Pacing::FrameCallback);
        s.damage(0);
        s.damage(MS);
        // lower = 1.5 ms
        assert_eq!(s.poll(MS + 100_000), Poll::Wait(Some(MS + 500_000)));
        assert_eq!(s.poll(MS + 500_000), render(false, true));
    }

    #[test]
    fn steady_reads_hit_the_upper_clamp() {
        let mut s = sched(Pacing::FrameCallback);
        let mut t = 0;
        s.damage(t);
        let mut fired_at = None;
        while t <= 20 * MS {
            t += 400_000;
            s.damage(t);
            if let Poll::Render(_) = s.poll(t) {
                fired_at = Some(t);
                break;
            }
        }
        assert_eq!(
            fired_at,
            Some(8 * MS),
            "lower keeps re-arming; upper fires at 8 ms"
        );
    }

    #[test]
    fn upper_timer_is_half_refresh_clamped_to_8ms() {
        let mut s = sched(Pacing::FrameCallback);
        s.presentation_feedback(Duration::from_micros(33_333)); // 30 Hz: half is 16.7, clamp 8
        s.damage(0);
        assert_eq!(s.poll(0), Poll::Wait(Some(8 * MS)));
        let mut s = sched(Pacing::FrameCallback);
        s.presentation_feedback(Duration::from_micros(8_000)); // 125 Hz: half is 4
        s.damage(0);
        assert_eq!(s.poll(0), Poll::Wait(Some(4 * MS)));
        let mut s = sched(Pacing::FrameCallback);
        s.presentation_feedback(Duration::ZERO);
        s.damage(0);
        assert_eq!(
            s.poll(0),
            Poll::Wait(Some(8 * MS)),
            "zero refresh is ignored"
        );
    }

    #[test]
    fn echo_skips_the_timers_but_not_the_gate() {
        let mut s = sched(Pacing::FrameCallback);
        s.note_key(0);
        s.damage(10 * MS);
        assert_eq!(s.poll(10 * MS), render(true, true), "no timer wait");
        s.rendered();
        // A second echo while the callback is pending waits for it.
        s.note_key(12 * MS);
        s.damage(13 * MS);
        assert_eq!(s.poll(13 * MS), Poll::Wait(None));
        s.frame_callback();
        assert_eq!(s.poll(14 * MS), render(true, true));
        // Outside the window the timers apply again.
        s.rendered();
        s.frame_callback();
        s.damage(100 * MS);
        assert_eq!(s.poll(100 * MS), Poll::Wait(Some(108 * MS)));
    }

    #[test]
    fn gate_blocks_until_callback_and_flood_renders_once() {
        let mut s = sched(Pacing::FrameCallback);
        s.damage(0);
        assert_eq!(s.poll(8 * MS), render(false, true));
        s.rendered();
        assert_eq!(s.gate(), Gate::Requested);
        // Flood: many reads while the callback is pending.
        let mut t = 10 * MS;
        let mut edges = 0;
        for _ in 0..50 {
            if s.damage(t) {
                edges += 1;
            }
            t += MS;
            assert_eq!(
                s.poll(t),
                Poll::Wait(None),
                "no timer is armed while the gate holds the frame"
            );
        }
        assert_eq!(edges, 1, "one wakeup for the whole flood");
        s.frame_callback();
        assert_eq!(s.gate(), Gate::Received);
        assert_eq!(s.poll(t), render(false, true), "latest state renders once");
        s.rendered();
        assert_eq!(s.poll(t), Poll::Idle);
    }

    #[test]
    fn callback_requested_only_by_rendered() {
        let mut s = sched(Pacing::FrameCallback);
        s.damage(0);
        let _ = s.poll(8 * MS);
        assert_eq!(s.gate(), Gate::Idle, "polling never requests a callback");
        s.skipped(8 * MS);
        assert_eq!(s.gate(), Gate::Idle, "a skipped frame commits nothing");
        s.rendered();
        assert_eq!(s.gate(), Gate::Requested);
    }

    #[test]
    fn fifo_path_has_the_gate_off() {
        assert_eq!(Pacing::choose(true, PresentKind::Fifo), Pacing::FifoUngated);
        let mut s = sched(Pacing::FifoUngated);
        s.damage(0);
        assert_eq!(s.poll(8 * MS), render(false, false));
        s.rendered();
        assert_eq!(s.gate(), Gate::Idle);
        s.damage(20 * MS);
        assert_eq!(
            s.poll(28 * MS),
            render(false, false),
            "no callback to wait for"
        );
    }

    #[test]
    fn pacing_ladder() {
        assert_eq!(
            Pacing::choose(true, PresentKind::Mailbox),
            Pacing::FrameCallback
        );
        assert_eq!(Pacing::choose(false, PresentKind::Mailbox), Pacing::Mailbox);
        assert!(!Pacing::Mailbox.gated());
        let mut s = sched(Pacing::Mailbox);
        s.damage(0);
        assert_eq!(s.poll(8 * MS), render(false, false));
    }

    #[test]
    fn sync_hold_ends_at_sync_end_or_timeout() {
        let mut s = sched(Pacing::FrameCallback);
        s.sync_begin(0);
        s.damage(MS);
        assert_eq!(s.poll(500 * MS), Poll::Wait(Some(1000 * MS)));
        s.sync_end(600 * MS);
        assert_eq!(s.poll(600 * MS), render(false, true), "released at once");
        s.rendered();
        s.frame_callback();

        // Timeout path.
        s.sync_begin(2000 * MS);
        s.damage(2001 * MS);
        assert_eq!(s.poll(2999 * MS), Poll::Wait(Some(3000 * MS)));
        assert_eq!(
            s.poll(3000 * MS),
            Poll::Render(RenderNow {
                echo: false,
                request_frame: true,
                sync_timed_out: true
            })
        );
    }

    #[test]
    fn sync_timeout_is_configurable() {
        let cfg = SchedConfig {
            sync_timeout: Duration::from_millis(50),
            ..SchedConfig::default()
        };
        let mut s = Scheduler::new(cfg, Pacing::Mailbox);
        s.sync_begin(0);
        s.damage(0);
        assert_eq!(s.poll(10 * MS), Poll::Wait(Some(50 * MS)));
    }

    #[test]
    fn suspended_surfaces_render_nothing() {
        let mut s = sched(Pacing::FrameCallback);
        s.set_suspended(true, 0);
        assert!(!s.damage(MS), "no wakeup while suspended");
        assert_eq!(s.poll(100 * MS), Poll::Idle);
        s.set_suspended(false, 200 * MS);
        assert_eq!(s.gate(), Gate::Idle);
        assert_eq!(
            s.poll(200 * MS),
            render(false, true),
            "resume renders at once"
        );
    }

    #[test]
    fn wakeups_are_edge_triggered() {
        let mut s = sched(Pacing::Mailbox);
        assert!(s.damage(0));
        assert!(!s.damage(1));
        assert!(!s.damage(2));
        s.rendered();
        assert!(s.damage(3));
    }

    #[test]
    fn manual_clock_advances() {
        let c = ManualClock::default();
        assert_eq!(c.now(), Duration::ZERO);
        c.advance(Duration::from_millis(3));
        assert_eq!(c.now(), Duration::from_millis(3));
    }
}
