// SPDX-License-Identifier: AGPL-3.0-only

//! Counting allocator (COMP-14 §3).
//!
//! Root invariant 11 forbids allocation in input delivery, `check()`, damage
//! accumulation, audit record construction and the per-frame render path.
//! COMP-14 §3 asks for that to be asserted rather than assumed —
//! "`#[no_alloc]`-style tests via a counting allocator in benches". This is
//! that allocator.
//!
//! It wraps the system allocator and counts every call. [`no_alloc`] takes a
//! snapshot, runs a closure, and reports how many allocations happened inside
//! it. The counters are process-wide, so a measured region must be
//! single-threaded — which every path this crate measures is, by invariant.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

/// The global allocator for this crate's binary. Counts, then delegates.
pub struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract, and we
        // forward `layout` unchanged to the system allocator.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from this allocator, which is `System` underneath,
        // with the same `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        // SAFETY: as `dealloc`, plus `new_size` is the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// What a measured region allocated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllocCount {
    /// Calls to `alloc`/`realloc`. Zero is the only passing value on a
    /// forbidden path.
    pub calls: u64,
    /// Bytes requested by those calls.
    pub bytes: u64,
}

impl AllocCount {
    /// Whether the region satisfied invariant 11.
    pub fn is_none(&self) -> bool {
        self.calls == 0
    }
}

/// Run `f`, reporting what it allocated.
///
/// Only meaningful when the binary installs [`Counting`] as its global
/// allocator; otherwise the counters never move and every region looks clean.
/// The harness binary does install it — see `main.rs`.
pub fn no_alloc<T>(f: impl FnOnce() -> T) -> (T, AllocCount) {
    let calls = ALLOCS.load(Ordering::Relaxed);
    let bytes = BYTES.load(Ordering::Relaxed);
    let out = f();
    let count = AllocCount {
        calls: ALLOCS.load(Ordering::Relaxed) - calls,
        bytes: BYTES.load(Ordering::Relaxed) - bytes,
    };
    (out, count)
}
