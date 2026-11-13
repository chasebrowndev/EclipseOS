// SPDX-License-Identifier: Apache-2.0
//! Saturating fixed-buffer writer. Never allocates, never panics, never overflows.

pub(crate) struct W<'a> {
    buf: &'a mut [u8],
    n: usize,
}

impl<'a> W<'a> {
    pub(crate) fn new(buf: &'a mut [u8]) -> Self {
        W { buf, n: 0 }
    }
    pub(crate) fn len(&self) -> usize {
        self.n
    }
    pub(crate) fn room(&self) -> usize {
        self.buf.len() - self.n
    }
    pub(crate) fn byte(&mut self, b: u8) {
        if let Some(slot) = self.buf.get_mut(self.n) {
            *slot = b;
            self.n += 1;
        }
    }
    pub(crate) fn bytes(&mut self, s: &[u8]) {
        for &b in s {
            self.byte(b);
        }
    }
    pub(crate) fn num(&mut self, mut v: u32) {
        let mut t = [0u8; 10];
        let mut i = t.len();
        loop {
            i -= 1;
            t[i] = b'0' + (v % 10) as u8;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        self.bytes(&t[i..]);
    }
    pub(crate) fn csi(&mut self) {
        self.bytes(b"\x1b[");
    }
    pub(crate) fn ss3(&mut self) {
        self.bytes(b"\x1bO");
    }
    pub(crate) fn ch(&mut self, c: char) {
        let mut b = [0u8; 4];
        self.bytes(c.encode_utf8(&mut b).as_bytes());
    }
    /// Append whole characters of `s` while they fit.
    pub(crate) fn str_fit(&mut self, s: &str) {
        for c in s.chars() {
            if self.room() < c.len_utf8() {
                break;
            }
            self.ch(c);
        }
    }
}

pub(crate) fn digits(mut v: u32) -> usize {
    let mut d = 1;
    while v >= 10 {
        v /= 10;
        d += 1;
    }
    d
}
