// SPDX-License-Identifier: AGPL-3.0-only
//! COMP-02 §2 render deadline: render as late as is safe.
//!
//! After a page flip the compositor schedules the next repaint at
//! `next_vblank - render_estimate`, `render_estimate` being an EWMA of recent
//! frame render times per output that backs off when a frame misses its
//! vblank. Everything here is plain arithmetic on monotonic [`Duration`]s: no
//! clock, no timer, no state beyond the estimate (ADR 0105, step B1).

use std::time::Duration;

/// Slack added on top of the EWMA. The EWMA is CPU time (`render_frame` plus
/// `queue_frame`); the GPU finishes after it, and the kernel needs the fence
/// signalled before the vblank, not merely the commit queued.
pub const MARGIN: Duration = Duration::from_micros(1000);
/// The lead never drops below this, however fast recent frames were.
pub const MIN_LEAD: Duration = Duration::from_micros(1000);
/// EWMA weight of a new sample is `1 / 2^ALPHA_SHIFT`.
const ALPHA_SHIFT: u32 = 3;
/// A fixed-refresh vblank is extrapolated from the last observed one for at
/// most this many periods. Beyond it the nominal period's rounding error has
/// accumulated past usefulness, so the frame renders at once.
pub const MAX_EXTRAPOLATE: u32 = 32;

/// EWMA of one output's frame render time (COMP-02 §2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderEstimate {
    ewma_ns: Option<u64>,
}

impl RenderEstimate {
    /// Fold one frame's render time in. The first sample seeds the average.
    pub fn record(&mut self, took: Duration) {
        let sample = u64::try_from(took.as_nanos()).unwrap_or(u64::MAX);
        self.ewma_ns = Some(match self.ewma_ns {
            None => sample,
            Some(e) if sample >= e => e + ((sample - e) >> ALPHA_SHIFT),
            Some(e) => e - ((e - sample) >> ALPHA_SHIFT),
        });
    }

    /// The frame it queued landed a vblank late: start earlier next time. The
    /// bump is a quarter period, and the EWMA decays it as on-time samples
    /// arrive. Never above one period (a lead of a whole period is "render
    /// immediately"; more would mean nothing).
    pub fn missed(&mut self, period: Duration) {
        let period_ns = u64::try_from(period.as_nanos()).unwrap_or(u64::MAX);
        let base = self.ewma_ns.unwrap_or(period_ns / 2);
        self.ewma_ns = Some(base.saturating_add(period_ns / 4).min(period_ns));
    }

    /// How far before the vblank to start rendering: the EWMA plus
    /// [`MARGIN`], clamped to `[MIN_LEAD, period]`. Before any sample the
    /// guess is half a period.
    pub fn lead(&self, period: Duration) -> Duration {
        let est = self.ewma_ns.map_or(period / 2, Duration::from_nanos);
        (est + MARGIN).max(MIN_LEAD).min(period)
    }
}

/// The first vblank strictly after `now`, extrapolated from `last`, a
/// vblank timestamp the kernel gave. `None` when there is no period or `last`
/// is more than [`MAX_EXTRAPOLATE`] periods old.
pub fn next_vblank(last: Duration, period: Duration, now: Duration) -> Option<Duration> {
    if period.is_zero() {
        return None;
    }
    if now < last {
        return Some(last + period);
    }
    let periods = (now - last).as_nanos() / period.as_nanos() + 1;
    if periods > u128::from(MAX_EXTRAPOLATE) {
        return None;
    }
    Some(last + period * periods as u32)
}

/// How long from `now` to wait before rendering; [`Duration::ZERO`] means
/// render now. Variable refresh, no known vblank and a stale one all render at
/// once: with no vblank to aim at there is nothing to wait for.
pub fn delay(
    last_vblank: Option<Duration>,
    now: Duration,
    period: Duration,
    vrr: bool,
    lead: Duration,
) -> Duration {
    if vrr {
        return Duration::ZERO;
    }
    let Some(next) = last_vblank.and_then(|l| next_vblank(l, period, now)) else {
        return Duration::ZERO;
    };
    (next - now).saturating_sub(lead)
}

/// A queued frame flipped more than half a period after the vblank it aimed
/// at: it missed.
pub fn is_miss(target: Duration, actual: Duration, period: Duration) -> bool {
    actual > target + period / 2
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: Duration = Duration::from_micros(16_667);

    fn us(n: u64) -> Duration {
        Duration::from_micros(n)
    }

    #[test]
    fn first_sample_seeds_the_average() {
        let mut e = RenderEstimate::default();
        e.record(us(2000));
        assert_eq!(e.lead(P), us(2000) + MARGIN);
    }

    #[test]
    fn unseeded_lead_is_half_a_period_plus_margin() {
        assert_eq!(RenderEstimate::default().lead(P), P / 2 + MARGIN);
    }

    #[test]
    fn ewma_moves_one_eighth_towards_each_sample() {
        let mut e = RenderEstimate::default();
        e.record(us(8000));
        e.record(us(0));
        assert_eq!(e.lead(P), us(7000) + MARGIN);
        e.record(us(15_000));
        // 7000 + (15000 - 7000) / 8
        assert_eq!(e.lead(P), us(8000) + MARGIN);
    }

    #[test]
    fn lead_is_clamped_to_one_period_and_a_floor() {
        let mut slow = RenderEstimate::default();
        slow.record(us(40_000));
        assert_eq!(slow.lead(P), P);
        let mut fast = RenderEstimate::default();
        fast.record(Duration::ZERO);
        assert_eq!(fast.lead(P), MIN_LEAD.max(MARGIN));
        // A period shorter than the floor still never exceeds the period.
        assert_eq!(fast.lead(us(500)), us(500));
    }

    #[test]
    fn a_miss_backs_off_a_quarter_period_and_stops_at_one() {
        let mut e = RenderEstimate::default();
        e.record(us(2000));
        e.missed(P);
        assert_eq!(e.lead(P), us(2000) + P / 4 + MARGIN);
        for _ in 0..10 {
            e.missed(P);
        }
        assert_eq!(e.lead(P), P);
    }

    #[test]
    fn backoff_decays_as_frames_land_on_time() {
        let mut e = RenderEstimate::default();
        e.record(us(2000));
        e.missed(P);
        let after_miss = e.lead(P);
        for _ in 0..40 {
            e.record(us(2000));
        }
        assert!(e.lead(P) < after_miss);
        assert!(e.lead(P) <= us(2100) + MARGIN);
    }

    #[test]
    fn next_vblank_is_strictly_after_now() {
        let last = us(1_000_000);
        assert_eq!(next_vblank(last, P, last), Some(last + P));
        assert_eq!(next_vblank(last, P, last + us(1)), Some(last + P));
        assert_eq!(next_vblank(last, P, last + P), Some(last + P * 2));
        assert_eq!(next_vblank(last, P, last + P * 5 + us(3)), Some(last + P * 6));
        // A timestamp from the future: aim one period past it.
        assert_eq!(next_vblank(last, P, last - us(5)), Some(last + P));
    }

    #[test]
    fn next_vblank_gives_up_on_a_stale_or_absent_period() {
        let last = us(1_000_000);
        assert_eq!(next_vblank(last, Duration::ZERO, last), None);
        assert!(next_vblank(last, P, last + P * (MAX_EXTRAPOLATE - 1)).is_some());
        assert_eq!(next_vblank(last, P, last + P * MAX_EXTRAPOLATE), None);
    }

    #[test]
    fn delay_waits_until_next_vblank_minus_lead() {
        let last = us(1_000_000);
        let lead = us(4000);
        // Right after the flip: the whole period minus the lead.
        assert_eq!(delay(Some(last), last, P, false, lead), P - lead);
        // Part-way through the period.
        assert_eq!(
            delay(Some(last), last + us(5000), P, false, lead),
            P - us(5000) - lead
        );
    }

    #[test]
    fn delay_is_zero_inside_the_lead_window() {
        let last = us(1_000_000);
        let lead = us(4000);
        // 2 ms before the vblank, 4 ms of lead needed: already late, go now.
        assert_eq!(
            delay(Some(last), last + P - us(2000), P, false, lead),
            Duration::ZERO
        );
        // Exactly at the deadline.
        assert_eq!(delay(Some(last), last + P - lead, P, false, lead), Duration::ZERO);
    }

    #[test]
    fn delay_is_zero_without_a_vblank_to_aim_at() {
        let now = us(5_000_000);
        assert_eq!(delay(None, now, P, false, us(4000)), Duration::ZERO);
        assert_eq!(delay(Some(us(1)), now, P, false, us(4000)), Duration::ZERO);
        assert_eq!(delay(Some(now), now, P, true, us(4000)), Duration::ZERO);
    }

    #[test]
    fn a_missed_vblank_is_a_flip_over_half_a_period_late() {
        let target = us(1_000_000);
        assert!(!is_miss(target, target, P));
        assert!(!is_miss(target, target + P / 2, P));
        assert!(is_miss(target, target + P / 2 + us(1), P));
        // Landed on the following vblank.
        assert!(is_miss(target, target + P, P));
        // Early is never a miss.
        assert!(!is_miss(target, target - us(100), P));
    }
}
