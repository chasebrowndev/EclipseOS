// SPDX-License-Identifier: AGPL-3.0-only
//! Bounded per-pane outbound write FIFO of whole units (T-00 §4.2, INPUT-58).
//!
//! A unit (a reply, a key's bytes, a whole paste with its bracket wrapper) is written
//! completely before the next one starts: units are queued whole and a partly written unit
//! is finished before the next begins. Only the main thread touches the queue.
//!
//! Bound (T-00 §4.2): "1 MiB plus the paste in flight". The 1 MiB counts every pending byte
//! of every queued unit, except the unit currently in flight when that unit is a paste (the
//! front unit, written or partly written): a paste being sent may be larger than the bound
//! and rides on top of it. A paste queued behind another unit is an ordinary pending unit and
//! counts. Overflow drops the NEW unit and bumps a counter; content is never kept or logged.

use std::collections::VecDeque;
use std::io;

/// Queue bound: 1 MiB of pending bytes, not counting a paste in flight.
pub const OUT_QUEUE_BOUND: usize = 1 << 20;

/// What `OutQueue::send` did with a unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enqueue {
    /// Fully written immediately.
    Written,
    /// (Remainder) queued; arm POLLOUT.
    Queued,
    /// Dropped because the bound was exceeded; counter incremented.
    Dropped,
}

struct Unit {
    buf: Vec<u8>,
    off: usize,
    paste: bool,
}

/// The FIFO. The writer is passed per call so the queue stays fd-free and testable.
pub struct OutQueue {
    units: VecDeque<Unit>,
    /// Pending bytes of all units.
    pending: usize,
    bound: usize,
    dropped: u64,
}

// Manual Debug: queued bytes are keystrokes and paste data and must never be printable.
impl std::fmt::Debug for OutQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutQueue")
            .field("units", &self.units.len())
            .field("pending", &self.pending)
            .field("bound", &self.bound)
            .field("dropped", &self.dropped)
            .finish()
    }
}

impl Default for OutQueue {
    fn default() -> Self {
        Self::new(OUT_QUEUE_BOUND)
    }
}

impl OutQueue {
    /// New queue with the given bound on ordinary pending bytes.
    pub fn new(bound: usize) -> Self {
        Self {
            units: VecDeque::new(),
            pending: 0,
            bound,
            dropped: 0,
        }
    }

    /// True while anything is queued: arm write interest only then (INPUT-58).
    pub fn wants_write(&self) -> bool {
        !self.units.is_empty()
    }

    /// Bytes not yet written, pastes included.
    pub fn pending_bytes(&self) -> usize {
        self.pending
    }

    /// Units dropped on overflow since creation (counter only, never content).
    pub fn dropped_units(&self) -> u64 {
        self.dropped
    }

    /// True while the unit at the front (in flight) is a paste.
    pub fn paste_in_flight(&self) -> bool {
        self.units.front().is_some_and(|u| u.paste)
    }

    /// Pending bytes the bound applies to: everything except a paste in flight.
    fn counted(&self) -> usize {
        match self.units.front() {
            Some(u) if u.paste => self.pending - (u.buf.len() - u.off),
            _ => self.pending,
        }
    }

    /// Submit one ordinary unit (a reply or a key's bytes). Writes immediately when the queue
    /// is empty. Dropped when it would take the counted pending bytes over the bound.
    pub fn send<W>(&mut self, unit: Vec<u8>, write: &mut W) -> io::Result<Enqueue>
    where
        W: FnMut(&[u8]) -> io::Result<usize>,
    {
        self.submit(unit, false, write)
    }

    /// Submit a whole paste (bracket wrapper included) as one unit. On an empty queue it is
    /// the unit in flight and exempt from the bound; behind other units it counts like any.
    pub fn send_paste<W>(&mut self, unit: Vec<u8>, write: &mut W) -> io::Result<Enqueue>
    where
        W: FnMut(&[u8]) -> io::Result<usize>,
    {
        self.submit(unit, true, write)
    }

    fn submit<W>(&mut self, unit: Vec<u8>, paste: bool, write: &mut W) -> io::Result<Enqueue>
    where
        W: FnMut(&[u8]) -> io::Result<usize>,
    {
        if unit.is_empty() {
            return Ok(Enqueue::Written);
        }
        let admitted = if self.units.is_empty() {
            // It becomes the unit in flight: only a paste may exceed the bound.
            paste || unit.len() <= self.bound
        } else {
            self.counted().saturating_add(unit.len()) <= self.bound
        };
        if !admitted {
            self.dropped += 1;
            return Ok(Enqueue::Dropped);
        }
        let mut off = 0;
        if self.units.is_empty() {
            while off < unit.len() {
                match write(&unit[off..]) {
                    Ok(0) => break,
                    Ok(n) => off += n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e),
                }
            }
            if off == unit.len() {
                return Ok(Enqueue::Written);
            }
        }
        let left = unit.len() - off;
        self.pending += left;
        self.units.push_back(Unit {
            buf: unit,
            off,
            paste,
        });
        Ok(Enqueue::Queued)
    }

    /// Drain on POLLOUT. `Ok(true)` when the queue is empty (disarm write interest).
    pub fn drain<W>(&mut self, write: &mut W) -> io::Result<bool>
    where
        W: FnMut(&[u8]) -> io::Result<usize>,
    {
        while let Some(u) = self.units.front_mut() {
            match write(&u.buf[u.off..]) {
                Ok(0) => return Ok(false),
                Ok(n) => {
                    u.off += n;
                    self.pending -= n;
                    if u.off == u.buf.len() {
                        self.units.pop_front();
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                Err(e) => return Err(e),
            }
        }
        Ok(true)
    }

    /// Discard everything (pane closed / child gone). Not counted as drops.
    pub fn clear(&mut self) {
        self.units.clear();
        self.pending = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A writer accepting at most `cap` bytes per call and `budget` bytes in total.
    struct Sink {
        out: Vec<u8>,
        cap: usize,
        budget: usize,
    }
    impl Sink {
        fn w(&mut self) -> impl FnMut(&[u8]) -> io::Result<usize> + '_ {
            move |b| {
                if self.budget == 0 {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                let n = b.len().min(self.cap).min(self.budget);
                self.out.extend_from_slice(&b[..n]);
                self.budget -= n;
                Ok(n)
            }
        }
    }

    #[test]
    fn empty_queue_writes_immediately() {
        let mut q = OutQueue::new(100);
        let mut s = Sink {
            out: vec![],
            cap: 64,
            budget: 1000,
        };
        assert_eq!(
            q.send(b"abc".to_vec(), &mut s.w()).unwrap(),
            Enqueue::Written
        );
        assert!(!q.wants_write());
        assert_eq!(s.out, b"abc");
    }

    #[test]
    fn partial_write_queues_remainder_and_preserves_unit_order() {
        let mut q = OutQueue::new(100);
        let mut s = Sink {
            out: vec![],
            cap: 64,
            budget: 4,
        };
        assert_eq!(
            q.send(b"abcdefgh".to_vec(), &mut s.w()).unwrap(),
            Enqueue::Queued
        );
        assert_eq!(q.send(b"XY".to_vec(), &mut s.w()).unwrap(), Enqueue::Queued);
        assert!(q.wants_write());
        assert_eq!(q.pending_bytes(), 6);
        assert!(!q.drain(&mut s.w()).unwrap());
        s.budget = 2;
        assert!(!q.drain(&mut s.w()).unwrap());
        s.budget = 100;
        assert!(q.drain(&mut s.w()).unwrap());
        assert!(!q.wants_write());
        assert_eq!(s.out, b"abcdefghXY");
        assert_eq!(q.pending_bytes(), 0);
    }

    #[test]
    fn overflow_drops_new_unit_and_counts() {
        let mut q = OutQueue::new(10);
        let mut s = Sink {
            out: vec![],
            cap: 64,
            budget: 0,
        };
        assert_eq!(q.send(vec![1; 8], &mut s.w()).unwrap(), Enqueue::Queued);
        assert_eq!(q.send(vec![2; 3], &mut s.w()).unwrap(), Enqueue::Dropped);
        assert_eq!(q.dropped_units(), 1);
        assert_eq!(q.send(vec![3; 2], &mut s.w()).unwrap(), Enqueue::Queued);
        s.budget = 100;
        assert!(q.drain(&mut s.w()).unwrap());
        assert_eq!(s.out, [vec![1u8; 8], vec![3u8; 2]].concat());
    }

    #[test]
    fn bound_is_exact_and_oversized_ordinary_unit_is_dropped() {
        let mut q = OutQueue::new(10);
        let mut s = Sink {
            out: vec![],
            cap: 64,
            budget: 0,
        };
        // Exactly at the bound is accepted; one byte over is not, even on an empty queue.
        assert_eq!(q.send(vec![1; 11], &mut s.w()).unwrap(), Enqueue::Dropped);
        assert_eq!(q.send(vec![1; 10], &mut s.w()).unwrap(), Enqueue::Queued);
        assert_eq!(q.send(vec![1; 1], &mut s.w()).unwrap(), Enqueue::Dropped);
        assert_eq!(q.dropped_units(), 2);
        // Draining frees room again.
        s.budget = 4;
        assert!(!q.drain(&mut s.w()).unwrap());
        assert_eq!(q.send(vec![2; 4], &mut s.w()).unwrap(), Enqueue::Queued);
        assert_eq!(q.pending_bytes(), 10);
    }

    #[test]
    fn default_bound_is_one_mib() {
        assert_eq!(OUT_QUEUE_BOUND, 1024 * 1024);
        assert_eq!(OutQueue::default().bound, 1 << 20);
    }

    #[test]
    fn paste_in_flight_is_on_top_of_the_bound() {
        // T-00 §4.2: "1 MiB plus the paste in flight".
        let mut q = OutQueue::new(10);
        let mut s = Sink {
            out: vec![],
            cap: 64,
            budget: 0,
        };
        assert_eq!(
            q.send_paste(vec![9; 50], &mut s.w()).unwrap(),
            Enqueue::Queued
        );
        assert!(q.paste_in_flight());
        assert_eq!(q.pending_bytes(), 50);
        // The paste in flight does not eat the budget: 10 more bytes fit, the 11th does not.
        assert_eq!(q.send(vec![1; 10], &mut s.w()).unwrap(), Enqueue::Queued);
        assert_eq!(q.send(vec![1; 1], &mut s.w()).unwrap(), Enqueue::Dropped);
        // A second paste is a pending unit behind the first and counts against the bound.
        assert_eq!(
            q.send_paste(vec![8; 1], &mut s.w()).unwrap(),
            Enqueue::Dropped
        );
        assert_eq!(q.dropped_units(), 2);
        // Units come out whole and in arrival order, the paste first.
        s.budget = 1000;
        s.cap = 7;
        assert!(q.drain(&mut s.w()).unwrap());
        assert_eq!(s.out, [vec![9u8; 50], vec![1u8; 10]].concat());
        assert!(!q.paste_in_flight());
    }

    #[test]
    fn second_paste_fits_when_within_bound_and_oversized_one_is_dropped() {
        let mut q = OutQueue::new(10);
        let mut s = Sink {
            out: vec![],
            cap: 64,
            budget: 0,
        };
        q.send_paste(vec![1; 50], &mut s.w()).unwrap();
        assert_eq!(
            q.send_paste(vec![2; 10], &mut s.w()).unwrap(),
            Enqueue::Queued
        );
        assert_eq!(
            q.send_paste(vec![3; 11], &mut s.w()).unwrap(),
            Enqueue::Dropped
        );
        s.budget = 1000;
        assert!(q.drain(&mut s.w()).unwrap());
        assert_eq!(s.out, [vec![1u8; 50], vec![2u8; 10]].concat());
    }

    #[test]
    fn ordinary_unit_in_flight_counts_against_bound() {
        let mut q = OutQueue::new(10);
        let mut s = Sink {
            out: vec![],
            cap: 64,
            budget: 0,
        };
        q.send(vec![1; 10], &mut s.w()).unwrap();
        // A paste behind an ordinary unit counts: 10 + 1 > 10.
        assert_eq!(
            q.send_paste(vec![2; 1], &mut s.w()).unwrap(),
            Enqueue::Dropped
        );
    }

    #[test]
    fn debug_never_prints_bytes() {
        let mut q = OutQueue::new(100);
        let mut s = Sink {
            out: vec![],
            cap: 64,
            budget: 0,
        };
        q.send(b"hunter2secret".to_vec(), &mut s.w()).unwrap();
        let d = format!("{q:?}");
        assert!(!d.contains("hunter2") && !d.contains("104"), "{d}");
        assert!(d.contains("pending: 13"));
    }

    #[test]
    fn key_behind_a_half_written_paste_never_lands_inside_it() {
        let mut q = OutQueue::new(1 << 20);
        let mut s = Sink {
            out: vec![],
            cap: 5,
            budget: 12,
        };
        let paste: Vec<u8> = (0..40u8).collect();
        assert_eq!(
            q.send_paste(paste.clone(), &mut s.w()).unwrap(),
            Enqueue::Queued
        );
        assert_eq!(s.out.len(), 12);
        // A key and a reply arrive mid-paste: they must wait for the paste's last byte.
        assert_eq!(q.send(b"K".to_vec(), &mut s.w()).unwrap(), Enqueue::Queued);
        assert_eq!(q.send(b"R".to_vec(), &mut s.w()).unwrap(), Enqueue::Queued);
        s.budget = 1000;
        assert!(q.drain(&mut s.w()).unwrap());
        assert_eq!(s.out, [paste, b"KR".to_vec()].concat());
    }

    #[test]
    fn hard_error_surfaces() {
        let mut q = OutQueue::new(10);
        let r = q.send(b"x".to_vec(), &mut |_: &[u8]| {
            Err(io::Error::from_raw_os_error(5))
        });
        assert!(r.is_err());
        assert!(!q.wants_write());
    }
}
