// SPDX-License-Identifier: AGPL-3.0-only
//! GPU time of the glass pipeline (COMP-02, liquid-glass step 0): raw GL timer
//! queries (`GL_EXT_disjoint_timer_query`, `GL_TIME_ELAPSED_EXT`).
//!
//! **This is the only file in the crate that contains `unsafe`** (the crate root
//! is `#![deny(unsafe_code)]`; this module allows it). smithay 0.7.0's GLES
//! bindings do not include the extension, so its entry points are fetched with
//! `eglGetProcAddress` through [`smithay::backend::egl::get_proc_address`] and
//! called with the renderer's context current (`GlesRenderer::with_context`, or
//! `GlesFrame::with_context` inside a frame).
//!
//! Design, for review:
//! * **Off by default and free when off.** Nothing is queried unless
//!   `EC_GLASS_PROFILE` is set ([`set_wanted`], called from
//!   [`crate::glass_profile::GlassProfile`]) *and* the driver advertises the
//!   extension; otherwise every entry point is one `Cell<bool>` read.
//! * **Spans split, not nested.** `GL_TIME_ELAPSED` queries cannot nest, so a
//!   span is one or more queries, each bracketing a run of consecutive GL work of
//!   one [`Span`] kind. A span that begins inside another (a lower tier's glass
//!   drawn by the backdrop copy) ends the outer query, runs its own, and the
//!   outer resumes after it, so each kind keeps the time it really took. Room for
//!   those resumes is checked first (counted as skipped otherwise), and a frame
//!   that ends with spans open closes the query, so none leaks across frames.
//! * **Ring, no stall.** A fixed ring of [`FRAMES`] frame slots, each with
//!   [`SPANS_PER_FRAME`] pre-generated query objects (generated once, at
//!   init: nothing is allocated per frame). Results are read back at the start
//!   of a later frame, oldest first, and only if `QUERY_RESULT_AVAILABLE` says
//!   every query of the slot is ready: a result that is not ready yet is left
//!   for the next frame, never waited for. A slot still unread when the ring
//!   wraps is dropped (counted as lost).
//! * **Disjoint.** `GPU_DISJOINT_EXT` is checked before each batch is read; a
//!   disjoint batch (clock change, context reset) is discarded and counted.
//! * **State.** Queries touch no other GL state, so nothing needs restoring. The
//!   context is current whenever a query is begun or read: the callers hold the
//!   renderer or the frame.
//! * Single-threaded like the rest of the render core: the state is
//!   thread-local, no lock. With several outputs a "frame" is the interval
//!   between two [`begin_frame`] calls on any output, so per-output figures
//!   blur together; measure with one output.

#![allow(unsafe_code)]

use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_void, CStr};

use smithay::backend::egl::get_proc_address;
use smithay::backend::renderer::gles::{ffi, GlesFrame, GlesRenderer};

use crate::stats::GpuLoss;

/// What a timed span is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Span {
    /// The backdrop composite into level 0 (the copy of what lies behind).
    Backdrop = 0,
    /// The downsample passes.
    Down = 1,
    /// The upsample passes and the final write.
    Up = 2,
    /// Copying level 1 beside the result (chromatic aberration).
    Pack = 3,
    /// The final draw of a `blur` backdrop into the output frame.
    FinalBlur = 4,
    /// The final draw of a `frost` backdrop.
    FinalFrost = 5,
    /// The final draw of a `glass` backdrop, any preset.
    FinalGlass = 6,
    /// A keyed lens's label pass (ADR 0132): the window's keyed pixels labelled.
    LensLabel = 7,
    /// A keyed lens's distance pass: the distance to the nearest other label.
    LensDistance = 8,
    /// A keyed lens's pack pass: the backdrop beside the mask, one texture.
    LensPack = 9,
}

/// Number of [`Span`] kinds.
pub const SPAN_KINDS: usize = 10;

/// Frames of queries in flight before the oldest is dropped unread.
const FRAMES: usize = 6;
/// Queries per frame; spans past this are not timed (counted).
const SPANS_PER_FRAME: usize = 64;

const TIME_ELAPSED_EXT: u32 = 0x88BF;
const QUERY_RESULT_EXT: u32 = 0x8866;
const QUERY_RESULT_AVAILABLE_EXT: u32 = 0x8867;
const GPU_DISJOINT_EXT: u32 = 0x8FBB;

type GenQueries = unsafe extern "system" fn(i32, *mut u32);
type BeginQuery = unsafe extern "system" fn(u32, u32);
type EndQuery = unsafe extern "system" fn(u32);
type GetQueryU32 = unsafe extern "system" fn(u32, u32, *mut u32);
type GetQueryU64 = unsafe extern "system" fn(u32, u32, *mut u64);

#[derive(Clone, Copy)]
struct Fns {
    gen: GenQueries,
    begin: BeginQuery,
    end: EndQuery,
    get_u32: GetQueryU32,
    get_u64: GetQueryU64,
}

/// What [`take_stats`] hands the profile: GPU nanoseconds per span kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuStats {
    /// Frames whose queries were read back.
    pub frames: u32,
    /// Sum over those frames, per span kind.
    pub sum_ns: [u64; SPAN_KINDS],
    /// Largest single frame, per span kind.
    pub max_ns: [u64; SPAN_KINDS],
    /// Largest frame total over all kinds.
    pub max_total_ns: u64,
    /// Frames dropped because their results were not ready in time.
    pub lost: u32,
    /// Batches discarded for `GPU_DISJOINT`.
    pub disjoint: u32,
    /// Spans not timed (ring full, or no room to split an enclosing span).
    pub skipped: u32,
}

#[derive(Clone, Copy)]
struct Slot {
    ids: [u32; SPANS_PER_FRAME],
    kinds: [u8; SPANS_PER_FRAME],
    used: usize,
    pending: bool,
}

const EMPTY_SLOT: Slot = Slot {
    ids: [0; SPANS_PER_FRAME],
    kinds: [0; SPANS_PER_FRAME],
    used: 0,
    pending: false,
};

#[derive(PartialEq, Eq)]
enum Init {
    Unknown,
    Off,
    On,
}

/// Deepest nesting of spans kept (a backdrop copy holding a few glass draws).
const NEST: usize = 8;

struct Timer {
    init: Init,
    fns: Option<Fns>,
    slots: [Slot; FRAMES],
    cur: usize,
    /// The open spans, outermost first (`Span as u8`), `depth` of them.
    stack: [u8; NEST],
    depth: usize,
    /// A `TIME_ELAPSED` query is running: the innermost open span's segment.
    running: bool,
    stats: GpuStats,
}

thread_local! {
    static WANTED: Cell<bool> = const { Cell::new(false) };
    /// True once the extension is there and the queries exist: the one flag the
    /// hot paths read.
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static TIMER: RefCell<Timer> = const { RefCell::new(Timer {
        init: Init::Unknown,
        fns: None,
        slots: [EMPTY_SLOT; FRAMES],
        cur: 0,
        stack: [0; NEST],
        depth: 0,
        running: false,
        stats: GpuStats { frames: 0, sum_ns: [0; SPAN_KINDS], max_ns: [0; SPAN_KINDS], max_total_ns: 0, lost: 0, disjoint: 0, skipped: 0 },
    }) };
}

/// Ask for GPU timing (from `EC_GLASS_PROFILE`). Takes effect at the next
/// [`begin_frame`].
pub fn set_wanted(wanted: bool) {
    if wanted {
        WANTED.with(|w| w.set(true));
    }
}

/// Whether timer queries are running (wanted, and the extension is present).
pub fn active() -> bool {
    ACTIVE.with(Cell::get)
}

/// Timing is wanted by `EC_GLASS_PROFILE` or by `--stats` (the frame stats line
/// reports the stage times too).
fn wanted() -> bool {
    WANTED.with(Cell::get) || crate::stats::glass_on()
}

/// The profile's view of the timer: whether it was asked for but the driver
/// has no extension (so the log can say so instead of printing zeros).
pub fn unavailable() -> bool {
    wanted() && TIMER.with_borrow(|t| t.init == Init::Off)
}

/// How the timer stands, for the log: `timer`, `no-extension`, or `off`
/// when nothing asked for it.
pub fn mode_name() -> &'static str {
    if !wanted() {
        "off"
    } else if active() {
        "timer"
    } else {
        "no-extension"
    }
}

impl Timer {
    /// Look for the extension and make the queries. Runs once.
    fn setup(&mut self, renderer: &mut GlesRenderer) {
        self.init = Init::Off;
        let made = renderer.with_context(|gl| {
            // SAFETY: the renderer's context is current (`with_context` made it
            // so). `GetString(EXTENSIONS)` returns a NUL-terminated string owned
            // by the driver that stays valid for the context's life; it is
            // copied by `to_bytes` comparison only, never kept.
            let has = unsafe {
                let ptr = gl.GetString(ffi::EXTENSIONS) as *const c_char;
                !ptr.is_null()
                    && CStr::from_ptr(ptr)
                        .to_bytes()
                        .split(|b| *b == b' ')
                        .any(|e| e == b"GL_EXT_disjoint_timer_query")
            };
            if !has {
                return None;
            }
            // SAFETY: an EGL context is active (above). `get_proc_address` returns
            // either null or the entry point of the named function; each is
            // transmuted to the extension's documented C signature (a
            // `void (*)(...)` taking the GL types spelled out in the aliases) and
            // used only while the context that advertised the extension is current.
            let fns = unsafe {
                let load = |name: &str| get_proc_address(name);
                let (g, b, e, u, w) = (
                    load("glGenQueriesEXT"),
                    load("glBeginQueryEXT"),
                    load("glEndQueryEXT"),
                    load("glGetQueryObjectuivEXT"),
                    load("glGetQueryObjectui64vEXT"),
                );
                if [g, b, e, u, w].iter().any(|p| p.is_null()) {
                    return None;
                }
                Fns {
                    gen: std::mem::transmute::<*const c_void, GenQueries>(g),
                    begin: std::mem::transmute::<*const c_void, BeginQuery>(b),
                    end: std::mem::transmute::<*const c_void, EndQuery>(e),
                    get_u32: std::mem::transmute::<*const c_void, GetQueryU32>(u),
                    get_u64: std::mem::transmute::<*const c_void, GetQueryU64>(w),
                }
            };
            Some(fns)
        });
        let Ok(Some(fns)) = made else {
            return;
        };
        for slot in &mut self.slots {
            // SAFETY: context current (the closure above ran under
            // `with_context`, and nothing has changed it); `ids` has room for the
            // `SPANS_PER_FRAME` names requested.
            unsafe { (fns.gen)(SPANS_PER_FRAME as i32, slot.ids.as_mut_ptr()) };
        }
        self.fns = Some(fns);
        self.init = Init::On;
        ACTIVE.with(|a| a.set(true));
    }

    /// Move to the next frame slot and read back what is ready.
    fn next_frame(&mut self, renderer: &mut GlesRenderer) {
        let Some(fns) = self.fns else { return };
        if self.running {
            // A frame that ended with a span open: close its query now, so none
            // runs across the frame boundary.
            let _ = renderer.with_context(|_| {
                // SAFETY: the context is current (`with_context`), and `running`
                // says a `TIME_ELAPSED` query is active.
                unsafe { (fns.end)(TIME_ELAPSED_EXT) };
            });
            self.running = false;
        }
        self.depth = 0;
        self.slots[self.cur].pending = self.slots[self.cur].used > 0;
        self.cur = (self.cur + 1) % FRAMES;
        let slot = &mut self.slots[self.cur];
        if slot.pending {
            self.stats.lost += 1;
            crate::stats::note_gpu_loss(GpuLoss::Lost);
        }
        slot.pending = false;
        slot.used = 0;
        // Oldest first: the slot after `cur` is the oldest.
        let Ok(()) = renderer.with_context(|gl| {
            for age in 1..FRAMES {
                let i = (self.cur + age) % FRAMES;
                let slot = self.slots[i];
                if !slot.pending {
                    continue;
                }
                let ready = (0..slot.used).all(|j| {
                    let mut avail = 0u32;
                    // SAFETY: context current under `with_context`; `slot.ids[j]`
                    // was generated by `glGenQueriesEXT` and begun/ended;
                    // `avail` is a valid `*mut u32` for one result.
                    unsafe { (fns.get_u32)(slot.ids[j], QUERY_RESULT_AVAILABLE_EXT, &mut avail) };
                    avail != 0
                });
                if !ready {
                    // Newer slots finish later; try again next frame.
                    break;
                }
                let mut disjoint = 0i32;
                // SAFETY: context current; `disjoint` is a valid `*mut GLint`.
                unsafe { gl.GetIntegerv(GPU_DISJOINT_EXT, &mut disjoint) };
                self.slots[i].pending = false;
                if disjoint != 0 {
                    self.stats.disjoint += 1;
                    crate::stats::note_gpu_loss(GpuLoss::Disjoint);
                    continue;
                }
                let mut per_kind = [0u64; SPAN_KINDS];
                for j in 0..slot.used {
                    let mut ns = 0u64;
                    // SAFETY: as above, and the result is available; `ns` is a
                    // valid `*mut u64`.
                    unsafe { (fns.get_u64)(slot.ids[j], QUERY_RESULT_EXT, &mut ns) };
                    per_kind[(slot.kinds[j] as usize).min(SPAN_KINDS - 1)] += ns;
                }
                crate::stats::note_gpu_frame(&per_kind);
                let s = &mut self.stats;
                s.frames += 1;
                let mut total = 0;
                for (k, ns) in per_kind.iter().enumerate() {
                    s.sum_ns[k] += ns;
                    s.max_ns[k] = s.max_ns[k].max(*ns);
                    total += ns;
                }
                s.max_total_ns = s.max_total_ns.max(total);
            }
        }) else {
            return;
        };
    }

    fn begin(&mut self, span: Span) {
        let Some(fns) = self.fns else { return };
        let slot = &mut self.slots[self.cur];
        // A nested begin needs its own segment, plus one resume segment per open
        // span (each `end` restarts the one it encloses): checked up front, so
        // a span is never left half split.
        let need = if self.depth == 0 { 1 } else { 2 * self.depth + 2 };
        if self.depth >= NEST || slot.used + need > SPANS_PER_FRAME {
            self.stats.skipped += 1;
            crate::stats::note_gpu_loss(GpuLoss::Skipped);
            return;
        }
        if self.running {
            // SAFETY: a `TIME_ELAPSED` query is active (`running`), and the
            // context is current: the enclosing span's segment ends here.
            unsafe { (fns.end)(TIME_ELAPSED_EXT) };
        }
        let id = slot.ids[slot.used];
        slot.kinds[slot.used] = span as u8;
        slot.used += 1;
        self.stack[self.depth] = span as u8;
        self.depth += 1;
        self.running = true;
        // SAFETY: called with the context current (the caller holds the
        // renderer or the frame); `id` is a live query from `glGenQueriesEXT`
        // and no `TIME_ELAPSED` query is active (`running` was just cleared or
        // was false).
        unsafe { (fns.begin)(TIME_ELAPSED_EXT, id) };
    }

    fn end(&mut self) {
        let Some(fns) = self.fns else { return };
        if self.depth == 0 {
            return;
        }
        if self.running {
            // SAFETY: a `TIME_ELAPSED` query was begun by `begin` (`running`) and
            // the context is still current.
            unsafe { (fns.end)(TIME_ELAPSED_EXT) };
            self.running = false;
        }
        self.depth -= 1;
        if self.depth == 0 {
            return;
        }
        // The enclosing span carries on from here, in a segment of its own.
        // `begin` reserved the room for it.
        let slot = &mut self.slots[self.cur];
        if slot.used >= SPANS_PER_FRAME {
            self.stats.skipped += 1;
            crate::stats::note_gpu_loss(GpuLoss::Skipped);
            return;
        }
        let id = slot.ids[slot.used];
        slot.kinds[slot.used] = self.stack[self.depth - 1];
        slot.used += 1;
        self.running = true;
        // SAFETY: as in `begin`: `id` is live and no query is active.
        unsafe { (fns.begin)(TIME_ELAPSED_EXT, id) };
    }
}

/// Start a frame: look for the extension on first use, move the ring on and read
/// back finished frames. Call once per output frame, before its first span.
pub fn begin_frame(renderer: &mut GlesRenderer) {
    if !wanted() {
        return;
    }
    TIMER.with_borrow_mut(|t| {
        if t.init == Init::Unknown {
            t.setup(renderer);
        }
        if t.init == Init::On {
            t.next_frame(renderer);
        }
    });
}

/// Open a span of `kind` with the renderer's context. No-op unless active.
pub fn begin(renderer: &mut GlesRenderer, kind: Span) {
    if !active() {
        return;
    }
    let _ = renderer.with_context(|_| TIMER.with_borrow_mut(|t| t.begin(kind)));
}

/// Close the open span. No-op unless active.
pub fn end(renderer: &mut GlesRenderer) {
    if !active() {
        return;
    }
    let _ = renderer.with_context(|_| TIMER.with_borrow_mut(Timer::end));
}

/// [`begin`] from inside a frame (an element's `draw`).
pub fn begin_in(frame: &mut GlesFrame<'_, '_>, kind: Span) {
    if !active() {
        return;
    }
    let _ = frame.with_context(|_| TIMER.with_borrow_mut(|t| t.begin(kind)));
}

/// [`end`] from inside a frame.
pub fn end_in(frame: &mut GlesFrame<'_, '_>) {
    if !active() {
        return;
    }
    let _ = frame.with_context(|_| TIMER.with_borrow_mut(Timer::end));
}

/// Take (and reset) the results read back since the last call; `None` while the
/// timer is not running.
pub fn take_stats() -> Option<GpuStats> {
    if !active() {
        return None;
    }
    Some(TIMER.with_borrow_mut(|t| std::mem::take(&mut t.stats)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_costs_nothing_and_reports_nothing() {
        // No `set_wanted`, no GL: every entry point is inert.
        assert!(!active());
        assert!(!unavailable());
        assert_eq!(take_stats(), None);
    }

    #[test]
    fn a_nested_span_splits_its_enclosing_span_and_a_full_ring_skips() {
        // Drive the bookkeeping with stub entry points (no GL context).
        unsafe extern "system" fn gen(n: i32, ids: *mut u32) {
            for i in 0..n {
                // SAFETY: the test passes a buffer of `n` ids.
                unsafe { *ids.add(i as usize) = i as u32 + 1 };
            }
        }
        unsafe extern "system" fn begin(_: u32, _: u32) {}
        unsafe extern "system" fn end(_: u32) {}
        unsafe extern "system" fn get32(_: u32, _: u32, out: *mut u32) {
            // SAFETY: valid out pointer.
            unsafe { *out = 1 };
        }
        unsafe extern "system" fn get64(_: u32, _: u32, out: *mut u64) {
            // SAFETY: valid out pointer.
            unsafe { *out = 1000 };
        }
        let fns = Fns {
            gen,
            begin,
            end,
            get_u32: get32,
            get_u64: get64,
        };
        let mut t = Timer {
            init: Init::On,
            fns: Some(fns),
            slots: [EMPTY_SLOT; FRAMES],
            cur: 0,
            stack: [0; NEST],
            depth: 0,
            running: false,
            stats: GpuStats::default(),
        };
        for s in &mut t.slots {
            // SAFETY: stub, buffer of SPANS_PER_FRAME ids.
            unsafe { gen(SPANS_PER_FRAME as i32, s.ids.as_mut_ptr()) };
        }
        t.begin(Span::Backdrop);
        assert_eq!((t.slots[0].used, t.depth, t.running), (1, 1, true));
        // Nested: the backdrop's segment ends, the glass one runs, and the
        // backdrop resumes in a segment of its own once the glass ends.
        t.begin(Span::FinalGlass);
        assert_eq!((t.slots[0].used, t.depth, t.running), (2, 2, true));
        t.end();
        assert_eq!((t.slots[0].used, t.depth, t.running), (3, 1, true));
        assert_eq!(
            &t.slots[0].kinds[..3],
            &[Span::Backdrop as u8, Span::FinalGlass as u8, Span::Backdrop as u8][..]
        );
        t.end();
        assert_eq!((t.depth, t.running, t.stats.skipped), (0, false, 0));
        t.end(); // a stray end is inert
        assert_eq!((t.slots[0].used, t.depth), (3, 0));
        // A full ring skips: 61 spans fill it from 3 used, the other 6 are skipped.
        for _ in 0..SPANS_PER_FRAME + 3 {
            t.begin(Span::FinalGlass);
            t.end();
        }
        assert_eq!(t.slots[0].used, SPANS_PER_FRAME);
        assert_eq!(t.stats.skipped, 6);
        assert!(!t.running);
    }
}
