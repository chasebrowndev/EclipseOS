// SPDX-License-Identifier: AGPL-3.0-only
//! The password buffer.
//!
//! Root invariant: `password`-role values are never delivered, logged or
//! stored. So this type has no `Debug`, no `Display`, no `Clone`, and no way
//! to read the text back except [`Secret::as_bytes`], which only `pam` calls.
//!
//! The buffer is allocated once, at [`MAX_BYTES`], and never grows: a growing
//! `String` reallocates and leaves the old bytes behind in freed memory where
//! nothing can wipe them. Every byte of the allocation is overwritten on
//! [`Secret::clear`], on [`Secret::take`]'s hand-over and on drop, so a panic
//! that unwinds through the owner wipes it too.
//!
//! What this does not reach: the keystroke text iced and xkb hand us passes
//! through their own buffers first, as in `ec-secret-prompt`. What this
//! promises is that no copy *we* hold outlives its use.

/// The longest password accepted, in bytes. Linux-PAM caps a conversation
/// reply at `PAM_MAX_RESP_SIZE` (512) including the terminating NUL.
pub const MAX_BYTES: usize = 511;

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
    /// [`MAX_BYTES`] or holds a control character or NUL; nothing reallocates.
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
        for b in &mut self.bytes[at..] {
            *b = 0;
        }
        self.bytes.truncate(at);
    }

    /// How many characters are typed: what the view draws bullets for.
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

    /// Move the text out into a new `Secret`, leaving this one empty with a
    /// fresh allocation. The bytes are moved, not copied: the allocation goes
    /// with them.
    pub fn take(&mut self) -> Secret {
        std::mem::take(self)
    }

    /// The typed bytes. For the PAM conversation only.
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[cfg(test)]
    fn capacity(&self) -> usize {
        self.bytes.capacity()
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        wipe(&mut self.bytes);
    }
}

/// Overwrite every byte `bytes` owns, spare capacity included, then empty it.
/// `black_box` keeps the optimiser from proving the writes dead, the same
/// plain overwrite `ec-secret-prompt` uses (`zeroize` is not a dependency of
/// this workspace).
pub(crate) fn wipe(bytes: &mut Vec<u8>) {
    let capacity = bytes.capacity();
    bytes.clear();
    bytes.resize(capacity, 0);
    std::hint::black_box(&mut *bytes);
    bytes.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_reallocates() {
        let mut s = Secret::new();
        let cap = s.capacity();
        let ptr = s.bytes.as_ptr();
        for _ in 0..MAX_BYTES {
            assert!(s.push_str("a"));
        }
        assert!(!s.push_str("a"), "past the cap");
        assert_eq!(s.capacity(), cap);
        assert_eq!(s.bytes.as_ptr(), ptr, "no move to a new allocation");
    }

    #[test]
    fn refuses_controls_and_nul() {
        let mut s = Secret::new();
        assert!(!s.push_str("\u{0}"));
        assert!(!s.push_str("\r"));
        assert!(!s.push_str("\u{8}"));
        assert!(!s.push_str(""));
        assert!(s.is_empty());
        assert!(s.push_str("é"));
        assert_eq!(s.chars(), 1);
    }

    #[test]
    fn pop_removes_a_whole_character_and_zeroes_it() {
        let mut s = Secret::new();
        s.push_str("aé");
        s.pop();
        assert_eq!(s.as_bytes(), b"a");
        // The two bytes of `é` are still inside the allocation, zeroed.
        // SAFETY: bytes 1..3 lie inside `s`'s live allocation (capacity is
        // fixed) and were written by the push and then by the wipe.
        let spare = unsafe { std::slice::from_raw_parts(s.bytes.as_ptr().add(1), 2) };
        assert_eq!(spare, &[0, 0]);
        s.pop();
        s.pop();
        assert!(s.is_empty());
    }

    #[test]
    fn clear_zeroes_the_allocation() {
        let mut s = Secret::new();
        s.push_str("hunter2");
        let ptr = s.bytes.as_ptr();
        s.clear();
        assert!(s.is_empty());
        // SAFETY: the allocation is still owned by `s` (clear keeps capacity),
        // and every byte of it was initialised by the wipe.
        let all = unsafe { std::slice::from_raw_parts(ptr, s.capacity()) };
        assert!(all.iter().all(|b| *b == 0));
    }

    #[test]
    fn take_moves_and_leaves_an_empty_buffer() {
        let mut s = Secret::new();
        s.push_str("pw");
        let ptr = s.bytes.as_ptr();
        let t = s.take();
        assert!(s.is_empty());
        assert_eq!(t.as_bytes(), b"pw");
        assert_eq!(t.bytes.as_ptr(), ptr, "moved, not copied");
        assert_ne!(s.bytes.as_ptr(), ptr);
        assert!(
            s.capacity() >= MAX_BYTES,
            "the fresh buffer is sized up front too"
        );
    }

    #[test]
    fn wipe_zeroes_spare_capacity() {
        let mut v = Vec::with_capacity(16);
        v.extend_from_slice(b"secretsecret");
        v.truncate(3);
        let ptr = v.as_ptr();
        let cap = v.capacity();
        wipe(&mut v);
        // SAFETY: `v` still owns the allocation; the wipe initialised all of it.
        let all = unsafe { std::slice::from_raw_parts(ptr, cap) };
        assert!(all.iter().all(|b| *b == 0));
    }
}
