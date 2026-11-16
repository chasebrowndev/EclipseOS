// SPDX-License-Identifier: AGPL-3.0-only
//! Stream replay (T-25 §7). The spec names `.apothrec` but does not define its bytes; this
//! is the text format `apoth-testkit` reads until it does (reported as a spec gap):
//!
//! ```text
//! apothrec 0 rows=24 cols=80
//! # comment
//! d 0 1b5b316d4869        data: <ms> <hex bytes>
//! r 120 30 100            resize: <ms> <rows> <cols>
//! ```
//!
//! Only PTY output is recorded, never input. Replay is headless and unpaced: the `ms`
//! value becomes the driver clock, there is no sleeping.

use crate::runner::{Chunking, Driver, split};

/// One recorded event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rec {
    /// PTY output.
    Data {
        /// Clock, ms.
        ms: u64,
        /// The bytes.
        bytes: Vec<u8>,
    },
    /// A resize.
    Resize {
        /// Clock, ms.
        ms: u64,
        /// Rows.
        rows: usize,
        /// Columns.
        cols: usize,
    },
}

/// A parsed recording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recording {
    /// Initial rows.
    pub rows: usize,
    /// Initial columns.
    pub cols: usize,
    /// Events in order.
    pub events: Vec<Rec>,
}

/// Parse a recording.
///
/// # Errors
/// A malformed header or record, with its line number.
pub fn parse(src: &str) -> Result<Recording, String> {
    let mut lines = src
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty() && !l.starts_with('#'));
    let (_, head) = lines.next().ok_or("empty recording")?;
    let mut h = head.split_whitespace();
    if (h.next(), h.next()) != (Some("apothrec"), Some("0")) {
        return Err("line 1: expected `apothrec 0`".into());
    }
    let (mut rows, mut cols) = (0usize, 0usize);
    for kv in h {
        match kv.split_once('=') {
            Some(("rows", v)) => rows = v.parse().map_err(|_| "bad rows")?,
            Some(("cols", v)) => cols = v.parse().map_err(|_| "bad cols")?,
            _ => return Err(format!("unknown header field `{kv}`")),
        }
    }
    if rows == 0 || cols == 0 {
        return Err("header needs rows= and cols=".into());
    }
    let mut events = Vec::new();
    for (i, l) in lines {
        let n = i + 1;
        let f: Vec<&str> = l.split_whitespace().collect();
        match f.as_slice() {
            ["d", ms, hex] => events.push(Rec::Data {
                ms: ms.parse().map_err(|_| format!("line {n}: bad ms"))?,
                bytes: unhex(hex).ok_or(format!("line {n}: bad hex"))?,
            }),
            ["d", ms] => events.push(Rec::Data {
                ms: ms.parse().map_err(|_| format!("line {n}: bad ms"))?,
                bytes: Vec::new(),
            }),
            ["r", ms, r, c] => events.push(Rec::Resize {
                ms: ms.parse().map_err(|_| format!("line {n}: bad ms"))?,
                rows: r.parse().map_err(|_| format!("line {n}: bad rows"))?,
                cols: c.parse().map_err(|_| format!("line {n}: bad cols"))?,
            }),
            _ => return Err(format!("line {n}: unrecognised record")),
        }
    }
    Ok(Recording { rows, cols, events })
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Serialise a recording; `parse(serialize(r)) == r`.
#[must_use]
pub fn serialize(r: &Recording) -> String {
    let mut o = format!("apothrec 0 rows={} cols={}\n", r.rows, r.cols);
    for e in &r.events {
        match e {
            Rec::Data { ms, bytes } => {
                let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
                o.push_str(&format!("d {ms} {hex}\n"));
            }
            Rec::Resize { ms, rows, cols } => o.push_str(&format!("r {ms} {rows} {cols}\n")),
        }
    }
    o
}

/// Feed a recording to a fresh driver, cutting each data record by `chunking`.
#[must_use]
pub fn play<D: Driver>(rec: &Recording, chunking: Chunking) -> D {
    let mut d = D::create(rec.rows, rec.cols, None);
    for e in &rec.events {
        match e {
            Rec::Data { ms, bytes } => {
                for (a, b) in split(bytes.len(), chunking) {
                    d.feed(&bytes[a..b], *ms);
                }
            }
            Rec::Resize { rows, cols, .. } => d.resize(*rows, *cols),
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selftest::MiniTerm;

    const SRC: &str =
        "apothrec 0 rows=3 cols=10\n# c\nd 0 48690d0a\nd 5 1b5d323b746974\nd 6 6c6507\nr 9 2 8\n";

    #[test]
    fn round_trip_and_errors() {
        let src = "apothrec 0 rows=3 cols=10\nd 0 48690d0a\nr 9 2 8\nd 10 \n";
        let r = parse(src).unwrap();
        assert_eq!(r.events.len(), 3);
        assert_eq!(parse(&serialize(&r)).unwrap(), r);
        assert!(parse("").is_err());
        assert!(parse("apothrec 1 rows=1 cols=1\n").is_err());
        assert!(parse("apothrec 0 rows=1 cols=1\nd 0 zz\n").is_err());
        assert!(parse("apothrec 0 rows=1 cols=1\nq\n").is_err());
        assert!(parse("apothrec 0\n").is_err());
    }

    #[test]
    fn replay_is_chunking_invariant() {
        // The title sequence straddles two data records.
        let r = parse(SRC).unwrap();
        let whole = play::<MiniTerm>(&r, Chunking::Whole).snapshot();
        for c in [Chunking::Size(1), Chunking::Size(7), Chunking::Random(3)] {
            assert_eq!(play::<MiniTerm>(&r, c).snapshot(), whole);
        }
        assert_eq!(whole.rows, 2);
        assert_eq!(whole.grid_text()[0], "Hi");
    }
}
