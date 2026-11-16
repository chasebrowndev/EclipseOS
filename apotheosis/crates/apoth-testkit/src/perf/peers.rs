// SPDX-License-Identifier: AGPL-3.0-only
//! The T-23 §2 relative targets: the same corpus is fed through `alacritty_terminal`'s `Term`
//! (the target peer) and through `vte` with a no-op `Perform` (the informative parser-only
//! line), on the same machine in the same run. The peers are run, never read (T-00 §3.4).

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;

use super::bench::CHUNK;
use super::median_of;

const MIB: f64 = 1024.0 * 1024.0;

/// `alacritty_terminal` throughput in MiB/s: median of `runs` fresh-terminal feeds in the same
/// [`CHUNK`]s as ours.
#[must_use]
pub fn alacritty_throughput(data: &[u8], rows: usize, cols: usize, runs: usize) -> f64 {
    let size = TermSize::new(cols, rows);
    let d = median_of(1, runs, || {
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut parser: Processor = Processor::new();
        for chunk in data.chunks(CHUNK) {
            parser.advance(&mut term, chunk);
        }
    });
    data.len() as f64 / d.as_secs_f64().max(1e-9) / MIB
}

struct NoOp;

impl vte::Perform for NoOp {
    fn print(&mut self, _: char) {}
    fn execute(&mut self, _: u8) {}
}

/// `vte` parser-only throughput with a no-op `Perform`, MiB/s.
#[must_use]
pub fn vte_throughput(data: &[u8], runs: usize) -> f64 {
    let d = median_of(1, runs, || {
        let mut p = vte::Parser::new();
        let mut n = NoOp;
        for chunk in data.chunks(CHUNK) {
            p.advance(&mut n, chunk);
        }
    });
    data.len() as f64 / d.as_secs_f64().max(1e-9) / MIB
}
