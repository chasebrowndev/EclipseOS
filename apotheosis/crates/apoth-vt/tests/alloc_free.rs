// SPDX-License-Identifier: Apache-2.0
//! T-01 §1/§6: `Parser::advance` never allocates, whatever the input. A counting global
//! allocator (per thread, so the test harness cannot disturb it) wraps every call.

mod common;
use apoth_vt::bench::{corpus_ascii, corpus_csi, corpus_dense};
use apoth_vt::{Params, Parser, Sink, StrEnd, StrHead, StrKind, StrRoute};
use common::{Rng, gen_stream};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

fn bump() {
    // `try_with`: the slot is gone during thread teardown; nothing to count then.
    let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
}

// SAFETY: every method forwards unchanged to `System`, which upholds the `GlobalAlloc`
// contract; the only addition is a thread-local counter increment that cannot allocate.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        bump();
        unsafe { System.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        bump();
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        bump();
        unsafe { System.realloc(p, l, n) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static A: Counting = Counting;

/// Touches every callback without allocating; caps force all string paths.
#[derive(Default)]
struct Sum {
    cap: u32,
    n: u64,
}

impl Sink for Sum {
    fn print_run(&mut self, t: &str) {
        self.n += t.len() as u64;
    }
    fn execute(&mut self, c: u8) {
        self.n += c as u64;
    }
    fn esc(&mut self, i: &[u8], f: u8) {
        self.n += i.len() as u64 + f as u64;
    }
    fn csi(&mut self, p: &Params, i: &[u8], pr: u8, f: u8) {
        self.n +=
            p.iter().flatten().map(u64::from).sum::<u64>() + i.len() as u64 + pr as u64 + f as u64;
    }
    fn string_start(&mut self, _k: StrKind, h: &StrHead) -> StrRoute {
        self.n += h.bytes().len() as u64;
        StrRoute { cap: self.cap }
    }
    fn string_put(&mut self, c: &[u8]) {
        self.n += c.len() as u64;
    }
    fn string_end(&mut self, _e: StrEnd) {
        self.n += 1;
    }
}

#[test]
fn advance_does_not_allocate() {
    // Build every input first: allocation is allowed here, not in the measured loop.
    let mut inputs: Vec<Vec<u8>> = vec![
        corpus_ascii(64 * 1024),
        corpus_dense(64 * 1024),
        corpus_csi(64 * 1024),
        // Oversized params, intermediates, a long OSC/DCS/APC over any cap.
        [b"\x1b[".as_slice(), &b"1;".repeat(200), b"m"].concat(),
        [b"\x1b[ !\"#$%&'(m".as_slice()].concat(),
        [b"\x1b]52;c;".as_slice(), &vec![b'A'; 100_000], b"\x07"].concat(),
        [b"\x1bP1$q".as_slice(), &vec![b'x'; 100_000], b"\x1b\\"].concat(),
        [b"\x1b_G".as_slice(), &vec![b'y'; 100_000], b"\x1b\\"].concat(),
        b"\xf0\x9f\x98".to_vec(),
    ];
    let mut rng = Rng(0xA110_C000_0000_0001);
    for _ in 0..300 {
        inputs.push(gen_stream(&mut rng, 40));
    }

    // The counter itself works (otherwise "no allocations" would pass vacuously).
    let probe = ALLOCS.with(Cell::get);
    drop(std::hint::black_box(Vec::<u8>::with_capacity(16)));
    assert!(
        ALLOCS.with(Cell::get) > probe,
        "counting allocator is not counting"
    );

    for cap in [0, 3, 1024, u32::MAX] {
        for chunk in [1usize, 7, 4096, usize::MAX] {
            let mut p = Parser::new();
            let mut s = Sum { cap, n: 0 };
            let before = ALLOCS.with(Cell::get);
            for inp in &inputs {
                for c in inp.chunks(chunk.min(inp.len().max(1))) {
                    p.advance(c, &mut s);
                }
            }
            let after = ALLOCS.with(Cell::get);
            assert_eq!(
                after, before,
                "advance allocated (cap {cap}, chunk {chunk})"
            );
            assert!(s.n > 0, "vacuous run");
        }
    }
}
