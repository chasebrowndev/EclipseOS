// SPDX-License-Identifier: Apache-2.0
//! Line framing (T-12 §1): newline-delimited, at most [`MAX_LINE_BYTES`] per line.
//! [`LineDecoder`] never buffers more than one maximal line, so a peer cannot make the
//! decoder grow without bound (SECURITY-32).

use serde::Serialize;

use crate::error::WireError;
use crate::limits::MAX_LINE_BYTES;

/// Reject a line (without its newline) over the limit.
pub fn check_line(line: &[u8]) -> Result<(), WireError> {
    if line.len() > MAX_LINE_BYTES {
        return Err(WireError::oversize(line.len()));
    }
    Ok(())
}

/// Serialise one message as a line including the trailing newline. A message that would
/// exceed the limit is an error here, so a server can never emit an oversize line.
pub fn encode_line<T: Serialize>(msg: &T) -> Result<Vec<u8>, WireError> {
    let mut out = serde_json::to_vec(msg)
        .map_err(|_| WireError::new(crate::ErrorCode::Internal, "message not serialisable"))?;
    check_line(&out)?;
    out.push(b'\n');
    Ok(out)
}

/// Incremental splitter. Chunking never changes the output: the same bytes in any chunks
/// give the same lines and the same oversize error.
#[derive(Debug, Default)]
pub struct LineDecoder {
    buf: Vec<u8>,
    poisoned: bool,
}

impl LineDecoder {
    /// A new decoder.
    pub fn new() -> Self {
        Self::default()
    }

    /// True after an oversize line; the connection must be closed. All later input is
    /// ignored.
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    /// Bytes of an incomplete line currently held (always ≤ the limit).
    pub fn pending(&self) -> usize {
        self.buf.len()
    }

    /// Feed bytes; returns the complete lines (newline stripped) and, if a line grew past
    /// the limit, one `line-too-long` error after them, then goes poisoned.
    pub fn feed(&mut self, mut data: &[u8]) -> Vec<Result<Vec<u8>, WireError>> {
        let mut out = Vec::new();
        if self.poisoned {
            return out;
        }
        while !data.is_empty() {
            match data.iter().position(|b| *b == b'\n') {
                Some(i) => {
                    if self.buf.len() + i > MAX_LINE_BYTES {
                        return self.poison(out, self.buf.len() + i);
                    }
                    self.buf.extend_from_slice(&data[..i]);
                    out.push(Ok(std::mem::take(&mut self.buf)));
                    data = &data[i + 1..];
                }
                None => {
                    if self.buf.len() + data.len() > MAX_LINE_BYTES {
                        return self.poison(out, self.buf.len() + data.len());
                    }
                    self.buf.extend_from_slice(data);
                    break;
                }
            }
        }
        out
    }

    fn poison(
        &mut self,
        mut out: Vec<Result<Vec<u8>, WireError>>,
        seen: usize,
    ) -> Vec<Result<Vec<u8>, WireError>> {
        self.poisoned = true;
        self.buf = Vec::new();
        out.push(Err(WireError::oversize(seen)));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;

    fn lines(d: &mut LineDecoder, chunks: &[&[u8]]) -> Vec<Result<Vec<u8>, ErrorCode>> {
        chunks
            .iter()
            .flat_map(|c| d.feed(c))
            .map(|r| r.map_err(|e| e.code))
            .collect()
    }

    #[test]
    fn splits_lines_across_chunks() {
        let mut d = LineDecoder::new();
        let got = lines(&mut d, &[b"{\"a\"", b":1}\n{\"b\":2}\n{\"c", b"\":3}\n"]);
        assert_eq!(
            got,
            vec![
                Ok(b"{\"a\":1}".to_vec()),
                Ok(b"{\"b\":2}".to_vec()),
                Ok(b"{\"c\":3}".to_vec())
            ]
        );
        assert_eq!(d.pending(), 0);
    }

    #[test]
    fn chunking_does_not_change_output() {
        let data = b"one\ntwo\n\nthree\npartial";
        let mut whole = LineDecoder::new();
        let a = lines(&mut whole, &[data]);
        let mut bytewise = LineDecoder::new();
        let chunks: Vec<&[u8]> = data.chunks(1).collect();
        let b = lines(&mut bytewise, &chunks);
        assert_eq!(a, b);
        assert_eq!(whole.pending(), bytewise.pending());
    }

    #[test]
    fn exactly_max_is_accepted_one_more_is_rejected() {
        let mut ok = vec![b'x'; MAX_LINE_BYTES];
        ok.push(b'\n');
        let mut d = LineDecoder::new();
        let got = d.feed(&ok);
        assert_eq!(got.len(), 1);
        assert!(got[0].is_ok());

        let mut big = vec![b'x'; MAX_LINE_BYTES + 1];
        big.push(b'\n');
        let mut d = LineDecoder::new();
        let got = d.feed(&big);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].as_ref().unwrap_err().code, ErrorCode::LineTooLong);
        assert!(d.is_poisoned());
        assert!(d.feed(b"{}\n").is_empty());
    }

    #[test]
    fn oversize_without_newline_is_cut_off_with_bounded_buffer() {
        let mut d = LineDecoder::new();
        let chunk = vec![b'y'; 64 * 1024];
        let mut errs = 0;
        for _ in 0..(MAX_LINE_BYTES / chunk.len() + 4) {
            assert!(d.pending() <= MAX_LINE_BYTES);
            for r in d.feed(&chunk) {
                if r.is_err() {
                    errs += 1;
                }
            }
        }
        assert_eq!(errs, 1);
        assert!(d.is_poisoned());
        assert_eq!(d.pending(), 0);
    }

    #[test]
    fn lines_before_the_oversize_are_still_delivered() {
        let mut data = b"ok\n".to_vec();
        data.extend(std::iter::repeat_n(b'z', MAX_LINE_BYTES + 5));
        let mut d = LineDecoder::new();
        let got = d.feed(&data);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].as_ref().unwrap(), b"ok");
        assert!(got[1].is_err());
    }

    #[test]
    fn check_line_and_encode() {
        assert!(check_line(&vec![b'a'; MAX_LINE_BYTES]).is_ok());
        assert_eq!(
            check_line(&vec![b'a'; MAX_LINE_BYTES + 1])
                .unwrap_err()
                .code,
            ErrorCode::LineTooLong
        );
        let l = encode_line(&serde_json::json!({"a": 1})).unwrap();
        assert_eq!(l, b"{\"a\":1}\n");
        let huge = serde_json::Value::String("x".repeat(MAX_LINE_BYTES));
        assert_eq!(encode_line(&huge).unwrap_err().code, ErrorCode::LineTooLong);
    }
}
