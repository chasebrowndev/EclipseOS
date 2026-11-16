// SPDX-License-Identifier: AGPL-3.0-only
//! `CLOCK_MONOTONIC` stamps and a timerfd for loop-driven timing (T-09 §2, T-23 §1 Clocks).
//!
//! Wayland event times (u32 ms, unspecified base) are never used for a budget; the client
//! stamp read here when the display fd becomes readable is.

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use rustix::time::{
    ClockId, Itimerspec, TimerfdClockId, TimerfdFlags, TimerfdTimerFlags, Timespec, clock_gettime,
    timerfd_create, timerfd_settime,
};

/// `wp_presentation.clock_id` value for `CLOCK_MONOTONIC` on Linux.
pub const CLOCK_MONOTONIC_ID: u32 = 1;

const NS_PER_SEC: u64 = 1_000_000_000;

/// Nanoseconds on `CLOCK_MONOTONIC`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Stamp(pub u64);

impl Stamp {
    /// Read the clock now.
    pub fn now() -> Stamp {
        let ts = clock_gettime(ClockId::Monotonic);
        Stamp::from_parts(ts.tv_sec as u64, ts.tv_nsec as u64)
    }

    /// Build from seconds and nanoseconds.
    pub fn from_parts(sec: u64, nsec: u64) -> Stamp {
        Stamp(sec.saturating_mul(NS_PER_SEC).saturating_add(nsec))
    }

    /// Nanoseconds.
    pub fn as_nanos(self) -> u64 {
        self.0
    }

    /// Add nanoseconds, saturating.
    pub fn plus_ns(self, ns: u64) -> Stamp {
        Stamp(self.0.saturating_add(ns))
    }

    /// Add milliseconds, saturating.
    pub fn plus_ms(self, ms: u64) -> Stamp {
        self.plus_ns(ms.saturating_mul(1_000_000))
    }
}

/// Join the hi/lo halves of a 64-bit protocol value.
pub fn join64(hi: u32, lo: u32) -> u64 {
    (u64::from(hi) << 32) | u64::from(lo)
}

/// A `wp_presentation_feedback.presented` timestamp as a [`Stamp`], but only when the
/// presentation clock is `CLOCK_MONOTONIC`. Any other clock yields `None`: LAT-PHOTON is
/// then recorded as unavailable, never converted (T-09 §2).
pub fn presentation_stamp(
    clock_id: Option<u32>,
    sec_hi: u32,
    sec_lo: u32,
    nsec: u32,
) -> Option<Stamp> {
    if clock_id != Some(CLOCK_MONOTONIC_ID) || nsec >= 1_000_000_000 {
        return None;
    }
    Some(Stamp::from_parts(join64(sec_hi, sec_lo), u64::from(nsec)))
}

/// A one-shot absolute timer on `CLOCK_MONOTONIC`, usable as a calloop fd.
#[derive(Debug)]
pub struct Timerfd {
    fd: OwnedFd,
}

impl Timerfd {
    /// Create a disarmed, non-blocking timerfd.
    pub fn new() -> io::Result<Timerfd> {
        let fd = timerfd_create(
            TimerfdClockId::Monotonic,
            TimerfdFlags::CLOEXEC | TimerfdFlags::NONBLOCK,
        )?;
        Ok(Timerfd { fd })
    }

    /// Arm for an absolute deadline.
    pub fn arm_at(&self, at: Stamp) -> io::Result<()> {
        let at = at.0.max(1);
        let spec = Itimerspec {
            it_interval: Timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            it_value: Timespec {
                tv_sec: (at / NS_PER_SEC) as _,
                tv_nsec: (at % NS_PER_SEC) as _,
            },
        };
        timerfd_settime(&self.fd, TimerfdTimerFlags::ABSTIME, &spec)?;
        Ok(())
    }

    /// Disarm.
    pub fn disarm(&self) -> io::Result<()> {
        let spec = Itimerspec {
            it_interval: Timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            it_value: Timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
        };
        timerfd_settime(&self.fd, TimerfdTimerFlags::empty(), &spec)?;
        Ok(())
    }

    /// Clear the readable state (a non-blocking read; `EAGAIN` is fine).
    pub fn drain(&self) {
        let mut buf = [0u8; 8];
        let _ = rustix::io::read(&self.fd, &mut buf[..]);
    }
}

impl AsFd for Timerfd {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_are_monotonic_and_add() {
        let a = Stamp::now();
        let b = Stamp::now();
        assert!(b >= a);
        assert_eq!(Stamp(5).plus_ms(2), Stamp(2_000_005));
        assert_eq!(Stamp::from_parts(2, 3), Stamp(2_000_000_003));
        assert_eq!(Stamp(u64::MAX).plus_ns(1), Stamp(u64::MAX));
    }

    #[test]
    fn presentation_stamps_only_on_the_monotonic_clock() {
        assert_eq!(join64(1, 2), (1u64 << 32) | 2);
        assert_eq!(
            presentation_stamp(Some(1), 0, 5, 7),
            Some(Stamp(5_000_000_007))
        );
        assert_eq!(
            presentation_stamp(Some(1), 1, 0, 0),
            Some(Stamp((1u64 << 32) * 1_000_000_000))
        );
        // CLOCK_REALTIME (0), boottime (7), unknown clock, or no clock event: unavailable
        assert_eq!(presentation_stamp(Some(0), 0, 5, 7), None);
        assert_eq!(presentation_stamp(Some(7), 0, 5, 7), None);
        assert_eq!(presentation_stamp(None, 0, 5, 7), None);
        // a malformed nanosecond field is refused
        assert_eq!(presentation_stamp(Some(1), 0, 5, 1_000_000_000), None);
    }

    #[test]
    fn timerfd_fires_for_a_past_deadline() {
        let t = Timerfd::new().unwrap();
        t.arm_at(Stamp(1)).unwrap();
        // A deadline in the past fires immediately: the read returns the expiry count.
        let mut buf = [0u8; 8];
        let mut got = false;
        for _ in 0..100 {
            if rustix::io::read(&t, &mut buf[..]).is_ok() {
                got = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(got);
        t.disarm().unwrap();
        t.drain();
    }
}
