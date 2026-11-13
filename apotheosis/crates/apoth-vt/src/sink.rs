// SPDX-License-Identifier: Apache-2.0
//! The consumer interface of the parser (T-01 §3).

use crate::params::{Params, StrEnd, StrHead, StrKind, StrRoute};

/// Receives the parser's action stream. Every method is called from `advance`;
/// the parser is monomorphised over the implementor.
pub trait Sink {
    /// A run of text: at least one printable scalar, no C0 and no DEL. Adjacent
    /// runs may be split at chunk boundaries; consumers must not depend on run edges.
    fn print_run(&mut self, text: &str);
    /// A C0 control (including CAN and SUB when they cancel a sequence).
    fn execute(&mut self, c0: u8);
    /// `ESC <inter> <fin>`.
    fn esc(&mut self, inter: &[u8], fin: u8);
    /// `CSI <private> <params> <inter> <fin>`; `private` is `0` when absent.
    fn csi(&mut self, p: &Params, inter: &[u8], private: u8, fin: u8);
    /// A string begins; the head is the routing copy. Answer with the payload cap.
    fn string_start(&mut self, kind: StrKind, head: &StrHead) -> StrRoute;
    /// Payload bytes. Slices from different calls concatenate to the payload.
    fn string_put(&mut self, chunk: &[u8]);
    /// The string finished. [`StrEnd`] says how, so replies can mirror the terminator
    /// (VTCORE-26); `!end.is_ok()` is "aborted, over cap, or rejected".
    fn string_end(&mut self, end: StrEnd);
}
