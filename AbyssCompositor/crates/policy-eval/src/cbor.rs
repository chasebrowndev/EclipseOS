// SPDX-License-Identifier: AGPL-3.0-only
//! Canonical CBOR, restricted to the profile in ADR 0044.
//!
//! Three surfaces in Phase 2 are CBOR — grants (S-01 §4), audit records
//! (S-04 §1) and the `policyd` socket (COMP-13 §3) — and two of them are
//! signed or hashed. That makes re-encoding a security operation: if a
//! decoder accepts a byte sequence that the encoder would have written
//! differently, then "what the record says" and "what the chain covers" are
//! two different things, and the gap between them is where a forged segment
//! lives.
//!
//! So the profile is RFC 8949 §4.2.1 core deterministic encoding, and the
//! decoder **refuses** anything outside it rather than normalising it:
//!
//! * integer heads in shortest form, always;
//! * definite lengths only, no indefinite items;
//! * map keys strictly increasing in encoded-byte order;
//! * no tags, no floats, no simple values but `false`, `true` and `null`.
//!
//! The encoder cannot express a non-canonical value, so `decode(encode(x))`
//! and `encode(decode(b)) == b` both hold by construction. There is no
//! "lenient" mode and there must never be one.

use std::fmt;

/// How deep a nested map may go before the decoder gives up. Every structure
/// we define is three or four levels; a bound here means a hostile blob
/// cannot make the key-ordering stack grow, and it is checked rather than
/// assumed.
const MAX_MAP_DEPTH: usize = 8;

const MAJOR_UINT: u8 = 0;
const MAJOR_NEGINT: u8 = 1;
const MAJOR_BYTES: u8 = 2;
const MAJOR_TEXT: u8 = 3;
const MAJOR_ARRAY: u8 = 4;
const MAJOR_MAP: u8 = 5;
const MAJOR_SIMPLE: u8 = 7;

/// Every way a byte string can fail to be a canonical-CBOR item we expect.
///
/// These are deliberately not merged into one `Invalid`: a truncated segment
/// tail is repaired at startup (ADR 0046) while a non-canonical one is
/// quarantined, and the store cannot tell them apart without this
/// distinction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The item ran off the end of the buffer. On a store tail this means a
    /// torn write, not an attack.
    Eof,
    /// Well-formed CBOR, but not in the profile: a long-form head for a small
    /// value, an indefinite length, or out-of-order map keys.
    NotCanonical,
    /// A tag, a float, or a simple value we do not use.
    Unsupported,
    /// The right shape, the wrong type — a text string where a map was due.
    Type,
    /// Bytes left over after a complete item.
    Trailing,
    /// Maps nested past [`MAX_MAP_DEPTH`].
    TooDeep,
    /// A map was left open, or closed twice, by the caller.
    Unbalanced,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Error::Eof => "truncated CBOR item",
            Error::NotCanonical => "CBOR is not in the canonical profile",
            Error::Unsupported => "CBOR item type is outside the profile",
            Error::Type => "unexpected CBOR type",
            Error::Trailing => "trailing bytes after CBOR item",
            Error::TooDeep => "CBOR maps nested too deeply",
            Error::Unbalanced => "unbalanced CBOR map",
        };
        f.write_str(s)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// Appends canonical CBOR to a byte buffer.
///
/// The writer has no way to emit a non-canonical head, so correctness here is
/// structural rather than tested-for. The one obligation it cannot enforce
/// alone is map key ordering, which is why maps go through [`MapBuilder`]
/// instead of a bare length prefix.
#[derive(Debug, Default)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Writer { buf: Vec::new() }
    }

    pub fn with_capacity(n: usize) -> Self {
        Writer {
            buf: Vec::with_capacity(n),
        }
    }

    pub fn finish(self) -> Vec<u8> {
        self.buf
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buf
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// The shortest head that can carry `arg`. Every other writer method goes
    /// through this, so "shortest form always" is one place, not eleven.
    fn head(&mut self, major: u8, arg: u64) {
        let m = major << 5;
        if arg < 24 {
            self.buf.push(m | arg as u8);
        } else if arg <= u8::MAX as u64 {
            self.buf.push(m | 24);
            self.buf.push(arg as u8);
        } else if arg <= u16::MAX as u64 {
            self.buf.push(m | 25);
            self.buf.extend_from_slice(&(arg as u16).to_be_bytes());
        } else if arg <= u32::MAX as u64 {
            self.buf.push(m | 26);
            self.buf.extend_from_slice(&(arg as u32).to_be_bytes());
        } else {
            self.buf.push(m | 27);
            self.buf.extend_from_slice(&arg.to_be_bytes());
        }
    }

    pub fn u64(&mut self, v: u64) {
        self.head(MAJOR_UINT, v);
    }

    /// Negative integers are major 1 with `-1 - v` as the argument, which is
    /// why `i64::MIN` needs the unsigned negation rather than `-v`.
    pub fn i64(&mut self, v: i64) {
        if v >= 0 {
            self.head(MAJOR_UINT, v as u64);
        } else {
            self.head(MAJOR_NEGINT, (-(v as i128) - 1) as u64);
        }
    }

    pub fn bytes(&mut self, b: &[u8]) {
        self.head(MAJOR_BYTES, b.len() as u64);
        self.buf.extend_from_slice(b);
    }

    pub fn text(&mut self, s: &str) {
        self.head(MAJOR_TEXT, s.len() as u64);
        self.buf.extend_from_slice(s.as_bytes());
    }

    pub fn bool(&mut self, b: bool) {
        self.buf.push((MAJOR_SIMPLE << 5) | if b { 21 } else { 20 });
    }

    pub fn null(&mut self) {
        self.buf.push((MAJOR_SIMPLE << 5) | 22);
    }

    /// Opens a definite-length array. The caller writes exactly `len` items
    /// after it; nothing checks that, because every array we write has a
    /// literal length two lines from its elements.
    pub fn array(&mut self, len: usize) {
        self.head(MAJOR_ARRAY, len as u64);
    }

    /// Splices an already-encoded canonical item in. Used for the places
    /// where a value was built separately — a `MapBuilder` entry, a COSE
    /// payload — and must not be re-encoded on the way through.
    pub fn raw(&mut self, encoded: &[u8]) {
        self.buf.extend_from_slice(encoded);
    }
}

/// Builds a canonical map by collecting entries and sorting them at the end.
///
/// Sorting on `finish` rather than asking the caller to insert in order is the
/// difference between a profile violation being impossible and it being a code
/// review item. The comparison is on the *encoded* key bytes, which for text
/// keys orders by length first and then lexicographically — not the same as
/// ordering the strings, which is exactly the mistake this type exists to
/// prevent.
#[derive(Debug, Default)]
pub struct MapBuilder {
    entries: Vec<(Vec<u8>, Vec<u8>)>,
}

impl MapBuilder {
    pub fn new() -> Self {
        MapBuilder { entries: Vec::new() }
    }

    /// Adds `key -> value`, where `value` is a single encoded canonical item.
    ///
    /// A duplicate key is a programming error, not an input error, so it is a
    /// debug assertion: the wire decoder refuses duplicates on its own side.
    pub fn insert(&mut self, key: &str, value: Vec<u8>) {
        let mut k = Writer::new();
        k.text(key);
        let k = k.finish();
        debug_assert!(
            !self.entries.iter().any(|(e, _)| *e == k),
            "duplicate CBOR map key {key:?}"
        );
        self.entries.push((k, value));
    }

    /// Adds `key -> value` only when `value` is `Some`.
    ///
    /// S-04 §1 makes this normative rather than convenient: `chain_id` and
    /// `task_id` are **absent** where they do not apply, never zeroed. A
    /// zeroed id is a claim about a task that does not exist.
    pub fn insert_opt(&mut self, key: &str, value: Option<Vec<u8>>) {
        if let Some(v) = value {
            self.insert(key, v);
        }
    }

    /// Inserts an integer-keyed entry. COSE header labels are small
    /// integers (RFC 9052 §3.1), not strings; the ordering rule is the same
    /// because it is defined over the *encoded* key either way.
    pub fn insert_int(&mut self, key: i64, value: Vec<u8>) {
        let k = enc(|w| w.i64(key));
        debug_assert!(
            !self.entries.iter().any(|(e, _)| *e == k),
            "duplicate map key {key}"
        );
        self.entries.push((k, value));
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.entries.sort_by(|a, b| a.0.cmp(&b.0));
        let mut w = Writer::with_capacity(self.entries.len() * 16);
        w.head(MAJOR_MAP, self.entries.len() as u64);
        for (k, v) in &self.entries {
            w.raw(k);
            w.raw(v);
        }
        w.finish()
    }
}

/// Encodes one value into a standalone byte string, for use as a
/// [`MapBuilder`] entry.
pub fn enc(f: impl FnOnce(&mut Writer)) -> Vec<u8> {
    let mut w = Writer::new();
    f(&mut w);
    w.finish()
}

/// A strict decoder over borrowed bytes.
///
/// Every accessor returns data borrowed from the input, so parsing a grant
/// allocates nothing beyond the map-ordering stack. The decoder refuses
/// non-canonical input at the point it sees it; there is no post-hoc
/// validation pass that a caller could forget to run.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    /// One frame per open map: the previously seen key's encoded bytes, used
    /// to enforce strictly increasing order, and the number of pairs left.
    maps: Vec<(Option<&'a [u8]>, u64)>,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Reader {
            buf,
            pos: 0,
            maps: Vec::new(),
        }
    }

    /// Consumes the reader, requiring that the input held exactly one item
    /// and that every map opened was closed.
    pub fn finish(self) -> Result<()> {
        if !self.maps.is_empty() {
            return Err(Error::Unbalanced);
        }
        if self.pos != self.buf.len() {
            return Err(Error::Trailing);
        }
        Ok(())
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(Error::Eof)?;
        let s = self.buf.get(self.pos..end).ok_or(Error::Eof)?;
        self.pos = end;
        Ok(s)
    }

    /// Reads one head, rejecting long forms that could have been short and
    /// the indefinite-length marker. This is the single chokepoint for
    /// canonicality on the read side.
    fn head(&mut self) -> Result<(u8, u64)> {
        let b = *self.take(1)?.first().ok_or(Error::Eof)?;
        let major = b >> 5;
        let low = b & 0x1f;
        // Major 7 with a payload is a float (25/26/27), a one-byte simple
        // value (24), or a break (31). The profile has no floats and no
        // indefinite items, so none of those are length-decoded at all: they
        // are unsupported, not mis-encoded, and the error says so.
        if major == MAJOR_SIMPLE && low >= 24 {
            return Err(Error::Unsupported);
        }
        let arg = match low {
            0..=23 => low as u64,
            24 => {
                let v = self.take(1)?[0] as u64;
                if v < 24 {
                    return Err(Error::NotCanonical);
                }
                v
            }
            25 => {
                let v = u16::from_be_bytes(self.take(2)?.try_into().unwrap()) as u64;
                if v <= u8::MAX as u64 {
                    return Err(Error::NotCanonical);
                }
                v
            }
            26 => {
                let v = u32::from_be_bytes(self.take(4)?.try_into().unwrap()) as u64;
                if v <= u16::MAX as u64 {
                    return Err(Error::NotCanonical);
                }
                v
            }
            27 => {
                let v = u64::from_be_bytes(self.take(8)?.try_into().unwrap());
                if v <= u32::MAX as u64 {
                    return Err(Error::NotCanonical);
                }
                v
            }
            // 28..=30 are reserved; 31 is the indefinite-length marker, which
            // the profile excludes outright.
            _ => return Err(Error::NotCanonical),
        };
        Ok((major, arg))
    }

    fn expect(&mut self, major: u8) -> Result<u64> {
        let (m, arg) = self.head()?;
        if m != major {
            return Err(Error::Type);
        }
        Ok(arg)
    }

    pub fn u64(&mut self) -> Result<u64> {
        self.expect(MAJOR_UINT)
    }

    pub fn i64(&mut self) -> Result<i64> {
        let (m, arg) = self.head()?;
        match m {
            MAJOR_UINT => i64::try_from(arg).map_err(|_| Error::Type),
            MAJOR_NEGINT => {
                let v = -(arg as i128) - 1;
                i64::try_from(v).map_err(|_| Error::Type)
            }
            _ => Err(Error::Type),
        }
    }

    pub fn bytes(&mut self) -> Result<&'a [u8]> {
        let n = self.expect(MAJOR_BYTES)?;
        self.take(usize::try_from(n).map_err(|_| Error::Eof)?)
    }

    /// A byte string of exactly `N` bytes — ids, hashes and keys are all
    /// fixed-width, and a length check at the parse site beats one at every
    /// use site.
    pub fn byte_array<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.bytes()?.try_into().map_err(|_| Error::Type)
    }

    pub fn text(&mut self) -> Result<&'a str> {
        let n = self.expect(MAJOR_TEXT)?;
        let s = self.take(usize::try_from(n).map_err(|_| Error::Eof)?)?;
        std::str::from_utf8(s).map_err(|_| Error::Type)
    }

    pub fn bool(&mut self) -> Result<bool> {
        let (m, arg) = self.head()?;
        match (m, arg) {
            (MAJOR_SIMPLE, 20) => Ok(false),
            (MAJOR_SIMPLE, 21) => Ok(true),
            _ => Err(Error::Type),
        }
    }

    pub fn array_len(&mut self) -> Result<u64> {
        self.expect(MAJOR_ARRAY)
    }

    /// Opens a map and returns its pair count. Must be matched by
    /// [`Reader::map_end`].
    pub fn map_begin(&mut self) -> Result<u64> {
        let n = self.expect(MAJOR_MAP)?;
        if self.maps.len() >= MAX_MAP_DEPTH {
            return Err(Error::TooDeep);
        }
        self.maps.push((None, n));
        Ok(n)
    }

    /// Reads the next key of the open map, enforcing strictly increasing
    /// encoded-byte order — which also rules out duplicate keys, since a
    /// repeat is not an increase.
    pub fn key(&mut self) -> Result<&'a str> {
        let start = self.pos;
        let s = self.text()?;
        let encoded = &self.buf[start..self.pos];
        let frame = self.maps.last_mut().ok_or(Error::Unbalanced)?;
        if frame.1 == 0 {
            return Err(Error::Unbalanced);
        }
        if let Some(prev) = frame.0 {
            if encoded <= prev {
                return Err(Error::NotCanonical);
            }
        }
        frame.0 = Some(encoded);
        frame.1 -= 1;
        Ok(s)
    }

    /// The integer-keyed form of [`Reader::key`], for COSE headers. Ordering
    /// is enforced over the encoded bytes exactly as it is for text keys.
    pub fn int_key(&mut self) -> Result<i64> {
        let start = self.pos;
        let v = self.i64()?;
        let encoded = &self.buf[start..self.pos];
        let frame = self.maps.last_mut().ok_or(Error::Unbalanced)?;
        if frame.1 == 0 {
            return Err(Error::Unbalanced);
        }
        if let Some(prev) = frame.0 {
            if encoded <= prev {
                return Err(Error::NotCanonical);
            }
        }
        frame.0 = Some(encoded);
        frame.1 -= 1;
        Ok(v)
    }

    /// Closes the open map, requiring that every pair was consumed. A decoder
    /// that stops early on a map it recognised would silently accept extra
    /// trailing keys.
    pub fn map_end(&mut self) -> Result<()> {
        let (_, left) = self.maps.pop().ok_or(Error::Unbalanced)?;
        if left != 0 {
            return Err(Error::Unbalanced);
        }
        Ok(())
    }

    /// Skips one complete item — the value of a key we do not recognise.
    ///
    /// Forward compatibility is a real requirement (milestone 12 adds record
    /// kinds to a store milestone 10 wrote), but skipping still validates:
    /// an unknown value is walked in full and refused if it is malformed.
    pub fn skip(&mut self) -> Result<()> {
        let (m, arg) = self.head()?;
        match m {
            MAJOR_UINT | MAJOR_NEGINT => Ok(()),
            MAJOR_BYTES | MAJOR_TEXT => {
                self.take(usize::try_from(arg).map_err(|_| Error::Eof)?)?;
                Ok(())
            }
            MAJOR_ARRAY => {
                for _ in 0..arg {
                    self.skip()?;
                }
                Ok(())
            }
            MAJOR_MAP => {
                if self.maps.len() >= MAX_MAP_DEPTH {
                    return Err(Error::TooDeep);
                }
                self.maps.push((None, arg));
                for _ in 0..arg {
                    self.key()?;
                    self.skip()?;
                }
                self.map_end()
            }
            MAJOR_SIMPLE if (20..=22).contains(&arg) => Ok(()),
            _ => Err(Error::Unsupported),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heads_are_shortest_form() {
        for (v, want) in [
            (0u64, vec![0x00]),
            (23, vec![0x17]),
            (24, vec![0x18, 0x18]),
            (255, vec![0x18, 0xff]),
            (256, vec![0x19, 0x01, 0x00]),
            (65536, vec![0x1a, 0x00, 0x01, 0x00, 0x00]),
        ] {
            let mut w = Writer::new();
            w.u64(v);
            assert_eq!(w.finish(), want, "u64 {v}");
        }
    }

    #[test]
    fn long_form_head_for_small_value_is_refused() {
        // 24 as a two-byte head is well-formed CBOR and not canonical.
        assert_eq!(Reader::new(&[0x18, 0x17]).u64(), Err(Error::NotCanonical));
        assert_eq!(Reader::new(&[0x19, 0x00, 0xff]).u64(), Err(Error::NotCanonical));
    }

    #[test]
    fn indefinite_length_is_refused() {
        // 0x5f: indefinite-length byte string.
        assert_eq!(Reader::new(&[0x5f, 0xff]).bytes(), Err(Error::NotCanonical));
    }

    #[test]
    fn tags_and_floats_are_refused() {
        let mut r = Reader::new(&[0xc0, 0x00]); // tag 0
        assert_eq!(r.skip(), Err(Error::Unsupported));
        let mut r = Reader::new(&[0xf9, 0x00, 0x00]); // half float
        assert_eq!(r.skip(), Err(Error::Unsupported));
    }

    #[test]
    fn map_keys_are_ordered_by_encoded_bytes_not_by_string() {
        let mut m = MapBuilder::new();
        // "z" sorts after "aa" as a string, but before it as an encoded key,
        // because the length byte leads.
        m.insert("aa", enc(|w| w.u64(1)));
        m.insert("z", enc(|w| w.u64(2)));
        let bytes = m.finish();
        let mut r = Reader::new(&bytes);
        assert_eq!(r.map_begin().unwrap(), 2);
        assert_eq!(r.key().unwrap(), "z");
        assert_eq!(r.u64().unwrap(), 2);
        assert_eq!(r.key().unwrap(), "aa");
        assert_eq!(r.u64().unwrap(), 1);
        r.map_end().unwrap();
        r.finish().unwrap();
    }

    #[test]
    fn out_of_order_and_duplicate_keys_are_refused() {
        let mut w = Writer::new();
        w.head(MAJOR_MAP, 2);
        w.text("b");
        w.u64(1);
        w.text("a");
        w.u64(2);
        let bytes = w.finish();
        let mut r = Reader::new(&bytes);
        r.map_begin().unwrap();
        r.key().unwrap();
        r.u64().unwrap();
        assert_eq!(r.key(), Err(Error::NotCanonical));

        let mut w = Writer::new();
        w.head(MAJOR_MAP, 2);
        w.text("a");
        w.u64(1);
        w.text("a");
        w.u64(2);
        let bytes = w.finish();
        let mut r = Reader::new(&bytes);
        r.map_begin().unwrap();
        r.key().unwrap();
        r.u64().unwrap();
        assert_eq!(r.key(), Err(Error::NotCanonical));
    }

    #[test]
    fn absent_is_not_zero() {
        // S-04 §1: an inapplicable id is absent. `insert_opt` is the only way
        // an optional field reaches the wire.
        let mut m = MapBuilder::new();
        m.insert_opt("task_id", None);
        m.insert("seq", enc(|w| w.u64(1)));
        let bytes = m.finish();
        let mut r = Reader::new(&bytes);
        assert_eq!(r.map_begin().unwrap(), 1);
        assert_eq!(r.key().unwrap(), "seq");
    }

    #[test]
    fn negative_integers_round_trip() {
        for v in [-1i64, -24, -25, -256, -65537, i64::MIN, 0, 1, i64::MAX] {
            let mut w = Writer::new();
            w.i64(v);
            let b = w.finish();
            let mut r = Reader::new(&b);
            assert_eq!(r.i64().unwrap(), v, "{v}");
            r.finish().unwrap();
        }
    }

    #[test]
    fn trailing_bytes_are_refused() {
        let mut r = Reader::new(&[0x01, 0x02]);
        r.u64().unwrap();
        assert_eq!(r.finish(), Err(Error::Trailing));
    }

    #[test]
    fn nesting_past_the_depth_bound_is_refused() {
        let mut w = Writer::new();
        for _ in 0..MAX_MAP_DEPTH + 1 {
            w.head(MAJOR_MAP, 1);
            w.text("k");
        }
        w.u64(0);
        let b = w.finish();
        let mut r = Reader::new(&b);
        assert_eq!(r.skip(), Err(Error::TooDeep));
    }

    #[test]
    fn truncation_is_eof_not_a_canonicality_error() {
        // The store repairs a torn tail and quarantines a non-canonical one,
        // so these two must stay distinguishable (ADR 0046).
        assert_eq!(Reader::new(&[0x43, 0x01]).bytes(), Err(Error::Eof));
    }
}
