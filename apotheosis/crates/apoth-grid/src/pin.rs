// SPDX-License-Identifier: AGPL-3.0-only
//! Tracked pins (T-02 §2, SCROLLBACKSEARCH-02).
//!
//! A pin is `(page serial, row handle, col)`. The row handle is the row's physical slot in
//! its page, so a pin follows its row's content when region scrolls rotate rows within a
//! page; cross-page moves retarget it. A pin whose row is evicted or discarded becomes
//! `Garbage` and its owner is notified through `Screen::take_garbage`.

/// Who owns a pin (informational; carried in garbage notifications).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum PinKind {
    /// Viewport top.
    Viewport,
    /// Cursor.
    Cursor,
    /// DECSC / alt-screen saved cursor.
    SavedCursor,
    /// Selection start.
    SelectionStart,
    /// Selection end.
    SelectionEnd,
    /// Search match.
    SearchMatch,
    /// Prompt or block mark.
    Mark,
    /// Image placement.
    ImagePlacement,
    /// Bookmark.
    Bookmark,
}

/// A stable row coordinate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Pin {
    /// Page serial.
    pub serial: u64,
    /// Row handle (physical slot in the page).
    pub slot: u16,
    /// Column.
    pub col: u16,
    /// Width the page was wrapped at when the pin was recorded (reflow seam, M12).
    pub wrap_width: u16,
}

/// Registration handle; generation-checked so a stale handle never aliases a new pin.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PinId {
    index: u32,
    genr: u32,
}

/// Pin state as seen by its owner.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PinState {
    /// Valid.
    Live(Pin),
    /// Its row is gone.
    Garbage,
}

/// Upper bound on simultaneously tracked pins.
pub const MAX_PINS: usize = 1 << 16;

#[derive(Clone, Copy, Debug)]
enum Slot {
    Free,
    Live(Pin, PinKind),
    Garbage,
}

#[derive(Clone, Debug)]
struct Entry {
    genr: u32,
    slot: Slot,
}

#[derive(Debug, Default)]
pub(crate) struct PinTable {
    entries: Vec<Entry>,
    free: Vec<u32>,
    garbage: Vec<(PinId, PinKind)>,
}

impl PinTable {
    pub(crate) fn track(&mut self, pin: Pin, kind: PinKind) -> Option<PinId> {
        if let Some(i) = self.free.pop() {
            let e = &mut self.entries[i as usize];
            e.slot = Slot::Live(pin, kind);
            return Some(PinId {
                index: i,
                genr: e.genr,
            });
        }
        if self.entries.len() >= MAX_PINS {
            return None;
        }
        self.entries.push(Entry {
            genr: 0,
            slot: Slot::Live(pin, kind),
        });
        Some(PinId {
            index: (self.entries.len() - 1) as u32,
            genr: 0,
        })
    }

    fn entry(&self, id: PinId) -> Option<&Entry> {
        self.entries
            .get(id.index as usize)
            .filter(|e| e.genr == id.genr)
    }

    pub(crate) fn untrack(&mut self, id: PinId) -> bool {
        let Some(e) = self.entries.get_mut(id.index as usize) else {
            return false;
        };
        if e.genr != id.genr || matches!(e.slot, Slot::Free) {
            return false;
        }
        e.slot = Slot::Free;
        e.genr = e.genr.wrapping_add(1);
        self.free.push(id.index);
        self.garbage.retain(|(g, _)| *g != id);
        true
    }

    pub(crate) fn get(&self, id: PinId) -> Option<PinState> {
        match self.entry(id)?.slot {
            Slot::Free => None,
            Slot::Live(p, _) => Some(PinState::Live(p)),
            Slot::Garbage => Some(PinState::Garbage),
        }
    }

    pub(crate) fn set_col(&mut self, id: PinId, col: u16) -> bool {
        match self.entries.get_mut(id.index as usize) {
            Some(e) if e.genr == id.genr => {
                if let Slot::Live(p, _) = &mut e.slot {
                    p.col = col;
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    fn kill_where(&mut self, mut pred: impl FnMut(&Pin) -> bool) {
        for (i, e) in self.entries.iter_mut().enumerate() {
            if let Slot::Live(p, k) = e.slot
                && pred(&p)
            {
                e.slot = Slot::Garbage;
                self.garbage.push((
                    PinId {
                        index: i as u32,
                        genr: e.genr,
                    },
                    k,
                ));
            }
        }
    }

    pub(crate) fn garbage_row(&mut self, serial: u64, slot: u16) {
        self.kill_where(|p| p.serial == serial && p.slot == slot);
    }

    pub(crate) fn garbage_page(&mut self, serial: u64) {
        self.kill_where(|p| p.serial == serial);
    }

    pub(crate) fn retarget(&mut self, from: (u64, u16), to: (u64, u16)) {
        for e in &mut self.entries {
            if let Slot::Live(p, _) = &mut e.slot
                && p.serial == from.0
                && p.slot == from.1
            {
                p.serial = to.0;
                p.slot = to.1;
            }
        }
    }

    pub(crate) fn take_garbage(&mut self, out: &mut Vec<(PinId, PinKind)>) {
        out.append(&mut self.garbage);
    }

    #[cfg(test)]
    pub(crate) fn live_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e.slot, Slot::Live(..)))
            .count()
    }

    #[cfg(test)]
    pub(crate) fn all_live(&self) -> Vec<(PinId, Pin)> {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| match e.slot {
                Slot::Live(p, _) => Some((
                    PinId {
                        index: i as u32,
                        genr: e.genr,
                    },
                    p,
                )),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(serial: u64, slot: u16) -> Pin {
        Pin {
            serial,
            slot,
            col: 0,
            wrap_width: 80,
        }
    }

    #[test]
    fn generation_guards_stale_ids() {
        let mut t = PinTable::default();
        let a = t.track(pin(1, 1), PinKind::Mark).unwrap();
        assert!(t.untrack(a));
        let b = t.track(pin(1, 2), PinKind::Mark).unwrap();
        assert_ne!(a, b);
        assert_eq!(t.get(a), None);
        assert!(matches!(t.get(b), Some(PinState::Live(_))));
        assert!(!t.untrack(a));
    }

    #[test]
    fn garbage_notifies_once() {
        let mut t = PinTable::default();
        let a = t.track(pin(1, 1), PinKind::Cursor).unwrap();
        let _b = t.track(pin(2, 1), PinKind::Cursor).unwrap();
        t.garbage_page(1);
        assert_eq!(t.get(a), Some(PinState::Garbage));
        let mut out = Vec::new();
        t.take_garbage(&mut out);
        assert_eq!(out, vec![(a, PinKind::Cursor)]);
        t.garbage_page(1);
        out.clear();
        t.take_garbage(&mut out);
        assert!(out.is_empty());
        assert_eq!(t.live_count(), 1);
    }

    #[test]
    fn retarget_moves_pins() {
        let mut t = PinTable::default();
        let a = t.track(pin(1, 1), PinKind::Mark).unwrap();
        t.retarget((1, 1), (2, 5));
        assert_eq!(t.get(a), Some(PinState::Live(pin(2, 5))));
    }
}
