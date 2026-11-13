// SPDX-License-Identifier: AGPL-3.0-only
//! Tab stops (T-02 §3).

/// A bitset of tab stops, one bit per column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabStops {
    bits: Vec<u64>,
    cols: usize,
}

impl TabStops {
    /// Default stops every 8 columns.
    #[must_use]
    pub fn new(cols: usize) -> Self {
        let mut t = TabStops {
            bits: vec![0; cols.div_ceil(64).max(1)],
            cols,
        };
        t.reset();
        t
    }

    /// Restore the every-8 defaults (also what RIS does).
    pub fn reset(&mut self) {
        self.bits.fill(0);
        let mut x = 8;
        while x < self.cols {
            self.set(x);
            x += 8;
        }
    }

    /// Change the width, keeping existing stops and adding defaults to new columns.
    pub fn resize(&mut self, cols: usize) {
        let old = self.cols;
        self.bits.resize(cols.div_ceil(64).max(1), 0);
        self.cols = cols;
        let mut x = old.next_multiple_of(8).max(8);
        while x < cols {
            self.set(x);
            x += 8;
        }
        // Drop stops beyond the new width.
        for x in cols..self.bits.len() * 64 {
            self.clear(x);
        }
    }

    /// HTS.
    pub fn set(&mut self, x: usize) {
        if x < self.cols {
            self.bits[x / 64] |= 1 << (x % 64);
        }
    }

    /// TBC 0.
    pub fn clear(&mut self, x: usize) {
        if x / 64 < self.bits.len() {
            self.bits[x / 64] &= !(1 << (x % 64));
        }
    }

    /// TBC 3.
    pub fn clear_all(&mut self) {
        self.bits.fill(0);
    }

    /// Whether a stop is set at `x`.
    #[must_use]
    pub fn is_set(&self, x: usize) -> bool {
        x < self.cols && self.bits[x / 64] & (1 << (x % 64)) != 0
    }

    /// CHT: the column of the `n`th stop after `x`, or the last column.
    #[must_use]
    pub fn next(&self, x: usize, n: usize) -> usize {
        let mut x = x;
        for _ in 0..n.max(1) {
            x += 1;
            while x < self.cols && !self.is_set(x) {
                x += 1;
            }
            if x >= self.cols {
                return self.cols.saturating_sub(1);
            }
        }
        x
    }

    /// CBT: the column of the `n`th stop before `x`, or column 0.
    #[must_use]
    pub fn prev(&self, x: usize, n: usize) -> usize {
        let mut x = x;
        for _ in 0..n.max(1) {
            if x == 0 {
                return 0;
            }
            x -= 1;
            while x > 0 && !self.is_set(x) {
                x -= 1;
            }
        }
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_motion() {
        let t = TabStops::new(80);
        assert!(t.is_set(8) && t.is_set(72) && !t.is_set(0) && !t.is_set(7));
        assert_eq!(t.next(0, 1), 8);
        assert_eq!(t.next(8, 1), 16);
        assert_eq!(t.next(3, 2), 16);
        assert_eq!(t.next(75, 1), 79);
        assert_eq!(t.prev(9, 1), 8);
        assert_eq!(t.prev(8, 1), 0);
        assert_eq!(t.prev(30, 2), 16);
        assert_eq!(t.prev(0, 1), 0);
    }

    #[test]
    fn set_clear_resize() {
        let mut t = TabStops::new(20);
        t.set(5);
        t.clear(8);
        assert_eq!(t.next(0, 1), 5);
        assert_eq!(t.next(5, 1), 16);
        t.resize(40);
        assert!(t.is_set(5) && t.is_set(24) && !t.is_set(8) && t.is_set(16));
        t.resize(10);
        assert!(!t.is_set(16));
        t.clear_all();
        assert_eq!(t.next(0, 1), 9);
        t.reset();
        assert!(t.is_set(8));
    }
}
