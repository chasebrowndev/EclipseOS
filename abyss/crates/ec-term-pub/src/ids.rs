// SPDX-License-Identifier: Apache-2.0
//! Per-surface node ids (ADR 0114 change (a), T-17 §4).
//!
//! The `eclipse_semantic_v1` tree is per surface, and one surface can hold many
//! terminals (tabs, splits). One [`NodeIds`] per surface hands out single ids
//! (the caller's own `window`, `tablist`, `tab` nodes) and disjoint
//! [`IdRange`]s (one per pane publisher) from one increasing counter, so no id
//! is ever handed out twice within the surface: a closed pane's ids are
//! retired with it, never recycled into the next one.

/// Ids a pane's range holds by default: its `terminal` node, one row slot per
/// [`crate::MAX_ROWS`] row, and about a million command blocks over its life.
pub const PANE_IDS: u32 = 1 << 20;

/// The id source of one surface. Id `0` is never handed out (COMP-09 uses
/// it for "no node").
#[derive(Clone, Debug)]
pub struct NodeIds {
    next: u32,
}

impl Default for NodeIds {
    fn default() -> Self {
        Self::new()
    }
}

impl NodeIds {
    pub fn new() -> Self {
        Self { next: 1 }
    }

    /// One fresh id; `None` once the surface's id space is spent.
    pub fn alloc(&mut self) -> Option<u32> {
        self.range(1).map(|r| r.start)
    }

    /// `len` fresh, consecutive ids; `None` for `len == 0` or once the
    /// surface's id space cannot hold `len` more.
    pub fn range(&mut self, len: u32) -> Option<IdRange> {
        if len == 0 {
            return None;
        }
        let start = self.next;
        let end = start.checked_add(len)?;
        self.next = end;
        Some(IdRange { start, len })
    }
}

/// A run of ids one publisher owns: `start .. start + len`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdRange {
    start: u32,
    len: u32,
}

impl IdRange {
    pub fn start(&self) -> u32 {
        self.start
    }

    pub fn len(&self) -> u32 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn contains(&self, id: u32) -> bool {
        id >= self.start && id - self.start < self.len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_are_disjoint_and_never_reused() {
        let mut ids = NodeIds::new();
        let w = ids.alloc().unwrap();
        let a = ids.range(PANE_IDS).unwrap();
        let t = ids.alloc().unwrap();
        let b = ids.range(10).unwrap();
        assert_eq!(w, 1);
        assert!(!a.contains(w) && !a.contains(t) && !b.contains(t));
        assert!(!a.contains(b.start()) && !b.contains(a.start() + a.len() - 1));
        assert_eq!(b.start(), t + 1);
        assert!(ids.range(0).is_none());
    }

    #[test]
    fn exhaustion_is_none_not_wraparound() {
        let mut ids = NodeIds::new();
        assert!(ids.range(u32::MAX - 2).is_some());
        assert!(ids.range(2).is_none());
        assert_eq!(ids.alloc(), Some(u32::MAX - 1));
        assert_eq!(ids.alloc(), None);
    }
}
