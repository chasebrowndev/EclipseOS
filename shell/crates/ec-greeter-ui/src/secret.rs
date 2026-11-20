// SPDX-License-Identifier: AGPL-3.0-only
//! The password buffer, and one key's worth of typed text on its way to it.
//!
//! Shaped after `ec-lock`'s `secret.rs` and `app::Typed`, copied because the
//! greeter's crate may not depend on the locker. Root invariant: a
//! `password`-role value is never delivered, logged or stored. So neither type
//! has a `Debug` that shows text, no `Display`, and no way to read the text back
//! but [`Secret::as_bytes`], which the auth backend calls and nothing else.
//!
//! The buffer is allocated once, at [`MAX_BYTES`], and never grows: a growing
//! `String` reallocates and leaves the old bytes in freed memory. Every byte is
//! overwritten on clear, on [`Secret::take`]'s hand-over and on drop.
//!
//! What this does not reach: the keystroke text iced and xkb hand us passes
//! through their own buffers first. What this promises is that no copy we hold
//! outlives its use.

/// The longest password accepted, in bytes: PAM's reply limit less the NUL.
pub const MAX_BYTES: usize = 511;

/// The typed password. Not `Debug`, not `Clone`, wiped on drop.
pub struct Secret {
    bytes: Vec<u8>,
}

impl Default for Secret {
    fn default() -> Self {
        Self::new()
    }
}

impl Secret {
    pub fn new() -> Self {
        Secret {
            bytes: Vec::with_capacity(MAX_BYTES),
        }
    }

    /// Append typed text. Refused whole, returning `false`, if it would pass
    /// [`MAX_BYTES`] or holds a control character; nothing reallocates.
    pub fn push_str(&mut self, text: &str) -> bool {
        if text.is_empty() || self.bytes.len() + text.len() > MAX_BYTES || text.chars().any(char::is_control)
        {
            return false;
        }
        self.bytes.extend_from_slice(text.as_bytes());
        true
    }

    /// Remove the last character. The freed bytes are zeroed.
    pub fn pop(&mut self) {
        let Ok(s) = std::str::from_utf8(&self.bytes) else {
            self.clear();
            return;
        };
        let Some((at, _)) = s.char_indices().next_back() else {
            return;
        };
        for b in self.bytes.iter_mut().skip(at) {
            *b = 0;
        }
        self.bytes.truncate(at);
    }

    /// How many characters are typed: what the view draws dots for.
    pub fn chars(&self) -> usize {
        std::str::from_utf8(&self.bytes).map_or(0, |s| s.chars().count())
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Overwrite the whole allocation, spare capacity included, and empty it.
    pub fn clear(&mut self) {
        wipe(&mut self.bytes);
    }

    /// Move the text out into a new `Secret`, leaving this one empty.
    pub fn take(&mut self) -> Secret {
        std::mem::take(self)
    }

    /// The typed bytes. For the auth backend's conversation only.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        wipe(&mut self.bytes);
    }
}

/// Overwrite every byte `bytes` owns, spare capacity included, then empty it.
/// `black_box` keeps the optimiser from proving the writes dead.
fn wipe(bytes: &mut Vec<u8>) {
    let capacity = bytes.capacity();
    bytes.clear();
    bytes.resize(capacity, 0);
    std::hint::black_box(&mut *bytes);
    bytes.clear();
}

/// One key's worth of typed text, moved through the iced runtime. A fixed
/// array, so it never touches the heap, and zeroed when dropped.
#[derive(Clone)]
pub struct Typed {
    bytes: [u8; Typed::CAP],
    len: u8,
}

impl Typed {
    /// Text longer than this from one key (a long compose sequence) is dropped.
    pub const CAP: usize = 16;

    pub fn new(text: &str) -> Option<Typed> {
        if text.is_empty() || text.len() > Self::CAP {
            return None;
        }
        let mut bytes = [0u8; Self::CAP];
        bytes.get_mut(..text.len())?.copy_from_slice(text.as_bytes());
        Some(Typed {
            bytes,
            len: text.len() as u8,
        })
    }

    pub fn as_str(&self) -> &str {
        self.bytes
            .get(..usize::from(self.len))
            .and_then(|b| std::str::from_utf8(b).ok())
            .unwrap_or("")
    }
}

impl std::fmt::Debug for Typed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Typed(<redacted>)")
    }
}

impl Drop for Typed {
    fn drop(&mut self) {
        for b in &mut self.bytes {
            // SAFETY: a valid, aligned, owned byte; volatile so the store
            // survives the value going dead.
            unsafe { std::ptr::write_volatile(b, 0) };
        }
        self.len = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_controls_and_empty_text() {
        let mut s = Secret::new();
        assert!(!s.push_str("\r"));
        assert!(!s.push_str("\u{0}"));
        assert!(!s.push_str(""));
        assert!(s.is_empty());
        assert!(s.push_str("é"));
        assert_eq!(s.chars(), 1);
    }

    #[test]
    fn never_grows_past_the_cap() {
        let mut s = Secret::new();
        let ptr = s.bytes.as_ptr();
        for _ in 0..MAX_BYTES {
            assert!(s.push_str("a"));
        }
        assert!(!s.push_str("a"));
        assert_eq!(s.bytes.as_ptr(), ptr, "no reallocation");
    }

    #[test]
    fn pop_removes_a_whole_character() {
        let mut s = Secret::new();
        s.push_str("aé");
        s.pop();
        assert_eq!(s.as_bytes(), b"a");
        s.pop();
        s.pop();
        assert!(s.is_empty());
    }

    #[test]
    fn take_moves_the_text_and_leaves_an_empty_buffer() {
        let mut s = Secret::new();
        s.push_str("pw");
        let t = s.take();
        assert!(s.is_empty());
        assert_eq!(t.as_bytes(), b"pw");
    }

    #[test]
    fn typed_text_is_never_debugged() {
        let t = Typed::new("hunter2").unwrap();
        assert_eq!(format!("{t:?}"), "Typed(<redacted>)");
        assert_eq!(t.as_str(), "hunter2");
        assert!(Typed::new("").is_none());
    }
}
