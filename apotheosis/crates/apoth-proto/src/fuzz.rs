// SPDX-License-Identifier: Apache-2.0
//! Stable entry point for the `proto_json` fuzz target (T-12 §6, T-18 §11). Arbitrary
//! bytes go through the line decoder, the request decoder and the server-message decoder;
//! it panics only when an invariant breaks (a decoded request that does not re-decode to
//! itself, or a buffer past the limit).

use crate::framing::LineDecoder;
use crate::limits::MAX_LINE_BYTES;
use crate::message::{Request, ServerMessage};

/// Run every decoder over `data`. Never panics on any input.
pub fn fuzz_parse(data: &[u8]) {
    let mut dec = LineDecoder::new();
    let mut lines = Vec::new();
    for chunk in data.chunks(97) {
        lines.extend(dec.feed(chunk));
        assert!(dec.pending() <= MAX_LINE_BYTES);
    }
    lines.push(Ok(data.to_vec()));
    for line in lines.into_iter().flatten() {
        if let Ok(req) = Request::decode(&line)
            && let Ok(enc) = req.to_line()
        {
            let again = Request::decode(&enc[..enc.len() - 1]).expect("an encoded request decodes");
            assert_eq!(again, req);
        }
        if let Ok(msg) = ServerMessage::decode(&line)
            && let Ok(enc) = msg.to_line()
        {
            let _ = ServerMessage::decode(&enc[..enc.len() - 1]);
        }
    }
}
