// SPDX-License-Identifier: AGPL-3.0-only

//! Capture -> pacer -> sink pump. Runs on a worker thread (never on a UI or
//! D-Bus task): the source blocks in `next_frame`.

use std::time::Duration;

use crate::pacing::{Decision, Pacer};
use crate::source::{CaptureError, CaptureSource, Frame};

pub trait FrameSink {
    /// Deliver a frame. `Err` ends the pump (e.g. PipeWire node gone).
    fn push(&mut self, frame: Frame) -> Result<(), String>;
}

#[derive(Debug, thiserror::Error)]
pub enum PumpError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error("sink closed: {0}")]
    Sink(String),
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub captured: u64,
    pub pushed: u64,
    /// Held frames superseded by a newer one before the pace allowed a push.
    pub coalesced: u64,
}

pub struct Pump<S, K> {
    src: S,
    sink: K,
    pacer: Pacer,
    clock: fn() -> u64,
    held: Option<Frame>,
    pub stats: Stats,
}

/// Idle poll interval so a stop request is noticed.
const IDLE: Duration = Duration::from_millis(100);

impl<S: CaptureSource, K: FrameSink> Pump<S, K> {
    pub fn new(src: S, sink: K, max_fps: u32, clock: fn() -> u64) -> Self {
        Self {
            src,
            sink,
            pacer: Pacer::new(max_fps),
            clock,
            held: None,
            stats: Stats::default(),
        }
    }

    fn push(&mut self, f: Frame) -> Result<(), PumpError> {
        self.sink.push(f).map_err(PumpError::Sink)?;
        self.stats.pushed += 1;
        Ok(())
    }

    /// One iteration; returns after at most ~`IDLE`.
    pub fn step(&mut self) -> Result<(), PumpError> {
        let wait = if self.held.is_some() {
            Duration::from_nanos(self.pacer.wait_ns((self.clock)())).min(IDLE)
        } else {
            IDLE
        };
        if let Some(mut f) = self.src.next_frame(wait)? {
            self.stats.captured += 1;
            // Merge damage of a superseded held frame so nothing is lost.
            if let Some(old) = self.held.take() {
                self.stats.coalesced += 1;
                if f.damage.is_empty() || old.damage.is_empty() {
                    f.damage.clear();
                } else {
                    f.damage.extend(old.damage);
                }
            }
            match self.pacer.on_frame((self.clock)()) {
                Decision::Push => self.push(f)?,
                Decision::Hold { .. } => self.held = Some(f),
            }
        }
        let now = (self.clock)();
        if self.held.is_some() && self.pacer.wait_ns(now) == 0 {
            let f = self.held.take().expect("held");
            self.pacer.mark_pushed(now);
            self.push(f)?;
        }
        Ok(())
    }

    /// Run until `stop()` is true or an error occurs.
    pub fn run(&mut self, stop: impl Fn() -> bool) -> Result<(), PumpError> {
        while !stop() {
            self.step()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::mock::{solid, MockSource};
    use crate::source::PixelFormat;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NOW: AtomicU64 = AtomicU64::new(0);
    fn clock() -> u64 {
        NOW.load(Ordering::SeqCst)
    }

    struct Rec(Vec<Frame>);
    impl FrameSink for &mut Rec {
        fn push(&mut self, f: Frame) -> Result<(), String> {
            self.0.push(f);
            Ok(())
        }
    }

    #[test]
    fn coalesces_bursts_and_flushes_latest() {
        NOW.store(0, Ordering::SeqCst);
        let mut frames = std::collections::VecDeque::new();
        for i in 0..3u8 {
            frames.push_back(solid(1, 1, PixelFormat::Argb8888, [i, 0, 0, 0]));
        }
        let mut rec = Rec(vec![]);
        let mut p = Pump::new(MockSource { frames }, &mut rec, 60, clock);
        p.step().unwrap(); // frame0 pushed at t=0
        p.step().unwrap(); // frame1 held
        p.step().unwrap(); // frame2 replaces frame1, still held
        assert_eq!(p.stats.coalesced, 1);
        NOW.store(20_000_000, Ordering::SeqCst);
        p.step().unwrap(); // no new frame, hold flushed
        assert_eq!(p.stats.pushed, 2);
        drop(p);
        assert_eq!(rec.0[1].data[0], 2, "newest frame wins");
    }

    #[test]
    fn sink_error_ends_pump() {
        struct Bad;
        impl FrameSink for Bad {
            fn push(&mut self, _: Frame) -> Result<(), String> {
                Err("gone".into())
            }
        }
        let mut frames = std::collections::VecDeque::new();
        frames.push_back(solid(1, 1, PixelFormat::Argb8888, [0; 4]));
        let mut p = Pump::new(MockSource { frames }, Bad, 0, || 0);
        assert!(matches!(p.step(), Err(PumpError::Sink(_))));
    }
}
