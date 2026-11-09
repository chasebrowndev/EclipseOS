// SPDX-License-Identifier: AGPL-3.0-only
//! Undo for writes that land the moment they are made. Settings has no
//! Apply button: a drag, a key, a toggle writes `abyss.kdl` at once, so the
//! way back is an undo — a toast that names what just happened with an Undo
//! on it, and Ctrl+Z for as long as the stack has entries.
//!
//! The stack is generic over what an entry restores, so a pane that undoes
//! something other than config values (Display's auto-revert) reuses it
//! as is. The toast is one at a time, and each push replaces it; it expires
//! by serial, so an old timer never takes down a newer toast.

/// How many changes the stack remembers. Older ones fall off the bottom.
pub const DEPTH: usize = 32;

/// The note on screen, and which push it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub serial: u64,
    pub note: String,
    /// Whether the Undo on it still has an entry to take back: a toast that
    /// reports an undo (or a refusal) has nothing under it.
    pub undoable: bool,
}

/// One change that can be taken back: its words, and what restores it.
#[derive(Debug, Clone)]
pub struct Entry<E> {
    pub note: String,
    pub restore: E,
}

#[derive(Debug, Clone)]
pub struct Stack<E> {
    entries: Vec<Entry<E>>,
    toast: Option<Toast>,
    serial: u64,
}

impl<E> Default for Stack<E> {
    fn default() -> Self {
        Stack {
            entries: Vec::new(),
            toast: None,
            serial: 0,
        }
    }
}

impl<E> Stack<E> {
    /// Remember a change, and toast it with its Undo.
    pub fn push(&mut self, note: impl Into<String>, restore: E) -> u64 {
        let note = note.into();
        self.entries.push(Entry {
            note: note.clone(),
            restore,
        });
        if self.entries.len() > DEPTH {
            self.entries.remove(0);
        }
        self.show(note, true)
    }

    /// The newest change, taken off the stack for the caller to restore.
    /// The toast it came from goes with it.
    pub fn pop(&mut self) -> Option<Entry<E>> {
        let e = self.entries.pop()?;
        self.toast = None;
        Some(e)
    }

    /// Toast a note with nothing to undo under it: what an undo did, or why
    /// a change was not made.
    pub fn say(&mut self, note: impl Into<String>) -> u64 {
        self.show(note.into(), false)
    }

    fn show(&mut self, note: String, undoable: bool) -> u64 {
        self.serial += 1;
        self.toast = Some(Toast {
            serial: self.serial,
            note,
            undoable,
        });
        self.serial
    }

    pub fn toast(&self) -> Option<&Toast> {
        self.toast.as_ref()
    }

    /// The serial of the toast now up, if any: a caller compares it before
    /// and after an update to know whether to start an expiry timer.
    pub fn serial(&self) -> Option<u64> {
        self.toast.as_ref().map(|t| t.serial)
    }

    /// Take the toast down if it is still the one `serial` put up.
    pub fn expire(&mut self, serial: u64) {
        if self.toast.as_ref().is_some_and(|t| t.serial == serial) {
            self.toast = None;
        }
    }

    pub fn dismiss(&mut self) {
        self.toast = None;
    }

    pub fn can_undo(&self) -> bool {
        !self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pop_returns_the_newest_change_first() {
        let mut s: Stack<u32> = Stack::default();
        s.push("one", 1);
        s.push("two", 2);
        assert_eq!(s.pop().map(|e| e.restore), Some(2));
        assert_eq!(s.pop().map(|e| e.restore), Some(1));
        assert!(s.pop().is_none());
    }

    #[test]
    fn an_old_timer_never_takes_down_a_newer_toast() {
        let mut s: Stack<u32> = Stack::default();
        let first = s.push("one", 1);
        let second = s.push("two", 2);
        s.expire(first);
        assert_eq!(s.serial(), Some(second));
        s.expire(second);
        assert!(s.toast().is_none());
    }

    #[test]
    fn a_said_note_has_nothing_to_undo_but_keeps_the_stack() {
        let mut s: Stack<u32> = Stack::default();
        s.push("one", 1);
        s.say("refused");
        assert!(!s.toast().unwrap().undoable);
        assert!(s.can_undo());
    }

    #[test]
    fn the_stack_forgets_past_its_depth() {
        let mut s: Stack<usize> = Stack::default();
        for i in 0..DEPTH + 5 {
            s.push("n", i);
        }
        assert_eq!(s.len(), DEPTH);
        let mut last = None;
        while let Some(e) = s.pop() {
            last = Some(e.restore);
        }
        assert_eq!(last, Some(5));
    }
}
