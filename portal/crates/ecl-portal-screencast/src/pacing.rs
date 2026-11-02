// SPDX-License-Identifier: AGPL-3.0-only

//! Damage-driven frame pacing. Frames only exist when pixels changed; the
//! pacer just caps the rate (`max_fps`) and coalesces bursts so the newest
//! frame is never dropped, only delayed. Pure; times are caller-supplied ns.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Push this frame now.
    Push,
    /// Too early; hold it (replacing any held frame) and push at `until_ns`.
    Hold { until_ns: u64 },
}

#[derive(Debug)]
pub struct Pacer {
    min_interval_ns: u64,
    last_push_ns: Option<u64>,
}

impl Pacer {
    /// `max_fps == 0` disables the cap.
    pub fn new(max_fps: u32) -> Self {
        Self {
            min_interval_ns: if max_fps == 0 {
                0
            } else {
                1_000_000_000 / u64::from(max_fps)
            },
            last_push_ns: None,
        }
    }

    pub fn on_frame(&mut self, now_ns: u64) -> Decision {
        match self.last_push_ns {
            Some(last) if now_ns < last.saturating_add(self.min_interval_ns) => Decision::Hold {
                until_ns: last + self.min_interval_ns,
            },
            _ => {
                self.last_push_ns = Some(now_ns);
                Decision::Push
            }
        }
    }

    /// For a held frame: ns still to wait (0 = push now; call
    /// [`Pacer::mark_pushed`]).
    pub fn wait_ns(&self, now_ns: u64) -> u64 {
        match self.last_push_ns {
            Some(last) => last
                .saturating_add(self.min_interval_ns)
                .saturating_sub(now_ns),
            None => 0,
        }
    }

    pub fn mark_pushed(&mut self, now_ns: u64) {
        self.last_push_ns = Some(now_ns);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_rate_and_holds() {
        let mut p = Pacer::new(60);
        assert_eq!(p.on_frame(0), Decision::Push);
        let iv = 1_000_000_000 / 60;
        assert_eq!(p.on_frame(5_000_000), Decision::Hold { until_ns: iv });
        assert_eq!(p.wait_ns(5_000_000), iv - 5_000_000);
        assert_eq!(p.wait_ns(iv), 0);
        p.mark_pushed(iv);
        assert_eq!(p.on_frame(iv + iv), Decision::Push);
    }

    #[test]
    fn uncapped_always_pushes() {
        let mut p = Pacer::new(0);
        assert_eq!(p.on_frame(1), Decision::Push);
        assert_eq!(p.on_frame(1), Decision::Push);
    }
}
