// SPDX-License-Identifier: AGPL-3.0-only
//! Animation shedding (COMP-14 §6): when the frame budget cannot be met,
//! motion is the first thing to go that costs the human nothing they asked for.
//!
//! [`ShedMonitor`] is a pure state machine fed one frame time per composited
//! frame by the DRM backend. It steps a [`ShedLevel`] down after a sustained
//! run of overruns and back up after a longer run of headroom, so a single slow
//! frame never changes anything and a level never flaps at the threshold. The
//! level reaches the engine through `AnimStore::set_shed`, and
//! [`super::resolve`] applies it. No allocation, no clock: the caller passes
//! the durations.
//!
//! COMP-14 §6 orders the degradation blur, then shadows, then animations. Neither
//! blur nor shadow shedding exists yet, so until they do this ladder is the only
//! one and animations shed first.

use std::time::Duration;

/// How much of the animation system is still running. Ordered: a higher level
/// sheds more.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum ShedLevel {
    /// Everything resolves as configured.
    #[default]
    Full,
    /// Add-on styles fall back to the built-in ones. The built-in catalog is
    /// all there is today, so this changes nothing until add-on styles exist.
    BuiltinOnly,
    /// Motion styles become a short fade; events with no fade stop.
    FadeOnly,
    /// Every event resolves to `none`.
    Off,
}

impl ShedLevel {
    fn down(self) -> ShedLevel {
        match self {
            ShedLevel::Full => ShedLevel::BuiltinOnly,
            ShedLevel::BuiltinOnly => ShedLevel::FadeOnly,
            ShedLevel::FadeOnly | ShedLevel::Off => ShedLevel::Off,
        }
    }

    fn up(self) -> ShedLevel {
        match self {
            ShedLevel::Off => ShedLevel::FadeOnly,
            ShedLevel::FadeOnly => ShedLevel::BuiltinOnly,
            ShedLevel::BuiltinOnly | ShedLevel::Full => ShedLevel::Full,
        }
    }
}

/// Consecutive overrunning frames before the level steps down: about a tenth of
/// a second at 144 Hz, long enough that a shader compile does not shed.
pub const DOWN_AFTER: u32 = 12;
/// Consecutive frames with headroom before the level steps back up. Several
/// times [`DOWN_AFTER`]: shedding fast and restoring slowly is the hysteresis
/// that keeps a borderline machine from flapping.
pub const UP_AFTER: u32 = 240;

/// What one frame says about the budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sample {
    /// Took longer than the frame period: a missed frame.
    Over,
    /// Took under half the period.
    Headroom,
    /// In between: neither counts, and either run is broken.
    Neutral,
}

impl Sample {
    pub fn of(frame: Duration, period: Duration) -> Sample {
        if frame > period {
            Sample::Over
        } else if frame * 2 < period {
            Sample::Headroom
        } else {
            Sample::Neutral
        }
    }
}

/// Run lengths, and the level they have earned.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShedMonitor {
    level: ShedLevel,
    over: u32,
    headroom: u32,
}

impl ShedMonitor {
    pub fn level(&self) -> ShedLevel {
        self.level
    }

    /// Account one frame. Returns the new level when it changed.
    pub fn observe(&mut self, frame: Duration, period: Duration) -> Option<ShedLevel> {
        self.step(Sample::of(frame, period))
    }

    /// [`ShedMonitor::observe`] on a classified sample: the pure core.
    pub fn step(&mut self, sample: Sample) -> Option<ShedLevel> {
        match sample {
            Sample::Over => {
                self.headroom = 0;
                self.over += 1;
            }
            Sample::Headroom => {
                self.over = 0;
                self.headroom += 1;
            }
            Sample::Neutral => {
                self.over = 0;
                self.headroom = 0;
            }
        }
        let before = self.level;
        if self.over >= DOWN_AFTER {
            self.level = self.level.down();
            self.over = 0;
        } else if self.headroom >= UP_AFTER {
            self.level = self.level.up();
            self.headroom = 0;
        }
        (self.level != before).then_some(self.level)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(m: &mut ShedMonitor, s: Sample, n: u32) {
        for _ in 0..n {
            m.step(s);
        }
    }

    #[test]
    fn classifies_against_the_period() {
        let p = Duration::from_millis(16);
        assert_eq!(Sample::of(Duration::from_millis(17), p), Sample::Over);
        assert_eq!(Sample::of(Duration::from_millis(16), p), Sample::Neutral);
        assert_eq!(Sample::of(Duration::from_millis(9), p), Sample::Neutral);
        assert_eq!(Sample::of(Duration::from_millis(7), p), Sample::Headroom);
    }

    #[test]
    fn sustained_overruns_step_down_one_level_at_a_time() {
        let mut m = ShedMonitor::default();
        run(&mut m, Sample::Over, DOWN_AFTER - 1);
        assert_eq!(m.level(), ShedLevel::Full);
        assert_eq!(m.step(Sample::Over), Some(ShedLevel::BuiltinOnly));
        run(&mut m, Sample::Over, DOWN_AFTER);
        assert_eq!(m.level(), ShedLevel::FadeOnly);
        run(&mut m, Sample::Over, DOWN_AFTER);
        assert_eq!(m.level(), ShedLevel::Off);
        // The floor holds, and reports no change.
        assert_eq!(
            (0..DOWN_AFTER * 2).filter_map(|_| m.step(Sample::Over)).count(),
            0
        );
        assert_eq!(m.level(), ShedLevel::Off);
    }

    #[test]
    fn one_slow_frame_or_a_broken_run_sheds_nothing() {
        let mut m = ShedMonitor::default();
        for _ in 0..100 {
            run(&mut m, Sample::Over, DOWN_AFTER - 1);
            m.step(Sample::Neutral);
        }
        assert_eq!(m.level(), ShedLevel::Full);
        for _ in 0..100 {
            run(&mut m, Sample::Over, DOWN_AFTER - 1);
            m.step(Sample::Headroom);
        }
        assert_eq!(m.level(), ShedLevel::Full);
    }

    #[test]
    fn headroom_restores_slowly_and_a_slow_frame_restarts_the_run() {
        let mut m = ShedMonitor::default();
        run(&mut m, Sample::Over, DOWN_AFTER * 3);
        assert_eq!(m.level(), ShedLevel::Off);
        run(&mut m, Sample::Headroom, UP_AFTER - 1);
        assert_eq!(m.level(), ShedLevel::Off);
        // A neutral frame breaks the run: it starts over.
        m.step(Sample::Neutral);
        run(&mut m, Sample::Headroom, UP_AFTER - 1);
        assert_eq!(m.level(), ShedLevel::Off);
        assert_eq!(m.step(Sample::Headroom), Some(ShedLevel::FadeOnly));
        run(&mut m, Sample::Headroom, UP_AFTER);
        assert_eq!(m.level(), ShedLevel::BuiltinOnly);
        run(&mut m, Sample::Headroom, UP_AFTER);
        assert_eq!(m.level(), ShedLevel::Full);
        assert_eq!(
            (0..UP_AFTER * 2).filter_map(|_| m.step(Sample::Headroom)).count(),
            0
        );
    }

    #[test]
    fn restoring_is_slower_than_shedding() {
        const { assert!(UP_AFTER > DOWN_AFTER * 4) };
    }

    #[test]
    fn levels_are_ordered_by_how_much_they_shed() {
        assert!(ShedLevel::Full < ShedLevel::BuiltinOnly);
        assert!(ShedLevel::BuiltinOnly < ShedLevel::FadeOnly);
        assert!(ShedLevel::FadeOnly < ShedLevel::Off);
    }
}
