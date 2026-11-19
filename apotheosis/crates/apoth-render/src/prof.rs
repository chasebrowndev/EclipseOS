// SPDX-License-Identifier: AGPL-3.0-only
//! Opt-in GPU timing for the renderer (`APOTHEOSIS_RENDER_STATS`).
//!
//! Timestamp queries bracket the grid passes and the present pass. With
//! `TIMESTAMP_QUERY_INSIDE_PASSES` the present pass is also split into image, chrome and
//! composite segments. Results are resolved into a small ring of readback buffers and
//! harvested a few frames later without blocking (`PollType::Poll`), so the profiler never
//! stalls the frame. Nothing here exists unless [`crate::renderer::Renderer::enable_profiling`]
//! was called; with it off no query set or buffer is created.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// What a timed span covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Group {
    /// The retained-grid redraw passes (summed over panes).
    Grid,
    /// Image quads inside the present pass.
    Image,
    /// Chrome and caret instances inside the present pass.
    Chrome,
    /// The grid-texture composite draws (and cursor) inside the present pass.
    Composite,
    /// The whole present pass.
    Present,
}

/// GPU milliseconds of one frame per group. `None`: the group was not measured.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GpuTimes {
    /// Grid redraw passes.
    pub grid: Option<f64>,
    /// Image draws (needs inside-pass timestamps).
    pub image: Option<f64>,
    /// Chrome and caret draws (needs inside-pass timestamps).
    pub chrome: Option<f64>,
    /// Composite draws (needs inside-pass timestamps).
    pub composite: Option<f64>,
    /// The whole present pass.
    pub present: Option<f64>,
}

impl GpuTimes {
    fn add(&mut self, g: Group, ms: f64) {
        let slot = match g {
            Group::Grid => &mut self.grid,
            Group::Image => &mut self.image,
            Group::Chrome => &mut self.chrome,
            Group::Composite => &mut self.composite,
            Group::Present => &mut self.present,
        };
        *slot = Some(slot.unwrap_or(0.0) + ms);
    }
}

/// Query slots per frame: two per span. Spans past this are dropped, not an error.
const QUERIES: u32 = 128;
const RING: usize = 3;

type Spans = Vec<(Group, u32, u32)>;

struct Slot {
    buf: wgpu::Buffer,
    /// Spans of the frame whose results this slot holds, and how many queries it has.
    pending: Option<(Spans, u32)>,
    /// `map_async` was issued; set by the callback when the map completed.
    mapping: Option<Arc<AtomicBool>>,
}

pub(crate) struct GpuProf {
    qs: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    ring: Vec<Slot>,
    period_ns: f64,
    inside: bool,
    next: u32,
    spans: Vec<(Group, u32, u32)>,
    done: Vec<GpuTimes>,
}

impl std::fmt::Debug for GpuProf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuProf")
            .field("inside", &self.inside)
            .finish_non_exhaustive()
    }
}

impl GpuProf {
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let bytes = u64::from(QUERIES) * u64::from(wgpu::QUERY_SIZE);
        let ring = (0..RING)
            .map(|_| Slot {
                buf: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("prof-readback"),
                    size: bytes,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                pending: None,
                mapping: None,
            })
            .collect();
        Self {
            qs: device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("prof-timestamps"),
                ty: wgpu::QueryType::Timestamp,
                count: QUERIES,
            }),
            resolve: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("prof-resolve"),
                size: bytes,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            ring,
            period_ns: f64::from(queue.get_timestamp_period()),
            inside: device
                .features()
                .contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES),
            next: 0,
            spans: Vec::new(),
            done: Vec::new(),
        }
    }

    pub(crate) fn begin_frame(&mut self) {
        self.next = 0;
        self.spans.clear();
    }

    fn alloc(&mut self, g: Group) -> Option<(u32, u32)> {
        if self.next + 2 > QUERIES {
            return None;
        }
        let a = self.next;
        self.next += 2;
        self.spans.push((g, a, a + 1));
        Some((a, a + 1))
    }

    /// Begin/end writes for a whole pass of group `g`.
    pub(crate) fn pass_writes(&mut self, g: Group) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        let (a, b) = self.alloc(g)?;
        Some(wgpu::RenderPassTimestampWrites {
            query_set: &self.qs,
            beginning_of_pass_write_index: Some(a),
            end_of_pass_write_index: Some(b),
        })
    }

    /// Starts an inside-pass span; returns the end index for [`GpuProf::end_span`].
    pub(crate) fn begin_span(&mut self, pass: &mut wgpu::RenderPass<'_>, g: Group) -> Option<u32> {
        if !self.inside {
            return None;
        }
        let (a, b) = self.alloc(g)?;
        pass.write_timestamp(&self.qs, a);
        Some(b)
    }

    pub(crate) fn end_span(&self, pass: &mut wgpu::RenderPass<'_>, end: Option<u32>) {
        if let Some(b) = end {
            pass.write_timestamp(&self.qs, b);
        }
    }

    /// Resolves this frame's queries into a free readback slot (recorded in `enc`).
    pub(crate) fn finish(&mut self, enc: &mut wgpu::CommandEncoder) {
        if self.next == 0 {
            return;
        }
        let Some(slot) = self.ring.iter_mut().find(|s| s.pending.is_none()) else {
            return; // every slot is still in flight: this frame goes unmeasured
        };
        let n = self.next;
        let bytes = u64::from(n) * u64::from(wgpu::QUERY_SIZE);
        enc.resolve_query_set(&self.qs, 0..n, &self.resolve, 0);
        enc.copy_buffer_to_buffer(&self.resolve, 0, &slot.buf, 0, bytes);
        slot.pending = Some((std::mem::take(&mut self.spans), n));
    }

    /// After the submission: starts the maps and collects finished ones, never blocking.
    pub(crate) fn after_submit(&mut self, device: &wgpu::Device) {
        for slot in &mut self.ring {
            if let (Some((_, n)), None) = (&slot.pending, &slot.mapping) {
                let flag = Arc::new(AtomicBool::new(false));
                let f = Arc::clone(&flag);
                slot.buf
                    .slice(0..u64::from(*n) * u64::from(wgpu::QUERY_SIZE))
                    .map_async(wgpu::MapMode::Read, move |r| {
                        f.store(r.is_ok(), Ordering::Release);
                    });
                slot.mapping = Some(flag);
            }
        }
        let _ = device.poll(wgpu::PollType::Poll);
        for slot in &mut self.ring {
            let ready = slot
                .mapping
                .as_ref()
                .is_some_and(|f| f.load(Ordering::Acquire));
            if !ready {
                continue;
            }
            let Some((spans, n)) = slot.pending.take() else {
                continue;
            };
            slot.mapping = None;
            let mut ticks = vec![0u64; n as usize];
            {
                let slice = slot
                    .buf
                    .slice(0..u64::from(n) * u64::from(wgpu::QUERY_SIZE));
                if let Ok(view) = slice.get_mapped_range() {
                    for (t, c) in ticks.iter_mut().zip(view.chunks_exact(8)) {
                        *t = u64::from_le_bytes(c.try_into().unwrap_or([0; 8]));
                    }
                }
            }
            slot.buf.unmap();
            self.done.push(frame_times(&spans, &ticks, self.period_ns));
        }
    }

    pub(crate) fn drain(&mut self, out: &mut Vec<GpuTimes>) {
        out.append(&mut self.done);
    }
}

/// Spans plus raw ticks to per-group milliseconds (pure; unit-tested).
pub(crate) fn frame_times(spans: &[(Group, u32, u32)], ticks: &[u64], period_ns: f64) -> GpuTimes {
    let mut t = GpuTimes::default();
    for &(g, a, b) in spans {
        let (Some(&x), Some(&y)) = (ticks.get(a as usize), ticks.get(b as usize)) else {
            continue;
        };
        let ns = y.saturating_sub(x) as f64 * period_ns;
        t.add(g, ns / 1.0e6);
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_sum_per_group_in_milliseconds() {
        let spans = [
            (Group::Grid, 0, 1),
            (Group::Grid, 2, 3),
            (Group::Present, 4, 5),
        ];
        // 1 tick = 1000 ns, so 1000 ticks = 1 ms.
        let ticks = [0, 1000, 5000, 7000, 10_000, 14_000];
        let t = frame_times(&spans, &ticks, 1000.0);
        assert_eq!(t.grid, Some(3.0));
        assert_eq!(t.present, Some(4.0));
        assert_eq!(t.image, None);
    }

    #[test]
    fn a_backwards_pair_counts_as_zero_and_missing_ticks_are_skipped() {
        let t = frame_times(&[(Group::Image, 0, 1), (Group::Chrome, 2, 3)], &[9, 3], 1.0);
        assert_eq!(t.image, Some(0.0));
        assert_eq!(t.chrome, None);
    }
}
