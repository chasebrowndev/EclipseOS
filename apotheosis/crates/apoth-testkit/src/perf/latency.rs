// SPDX-License-Identifier: AGPL-3.0-only
//! Key-to-commit latency hook (T-23 §1 Clocks, §2 `LAT-KEY` / `LAT-ECHO`, §4).
//!
//! # Interface the binary calls (design; the binary is not edited by this crate)
//!
//! The binary owns a [`LatencyProbe`] (behind `--trace` / `debug.trace`; absent means zero
//! cost: an `Option<LatencyProbe>` checked once per event). It reports three stamps per key,
//! all `CLOCK_MONOTONIC` nanoseconds that the *caller* reads (this crate has no clock and no
//! unsafe code):
//!
//! 1. [`LatencyProbe::key`]`(t)`: main stamps when the Wayland display fd became readable,
//!    before dispatch (the "client stamp"). Returns a [`KeyId`] carried with the key event.
//! 2. [`LatencyProbe::pty_written`]`(id, t)`: after the first successful `write()` carrying
//!    the key's bytes returns. `LAT-KEY` = this minus stamp 1.
//! 3. [`LatencyProbe::presented`]`(id, t)`: after `SurfaceTexture::present()` returns for the
//!    frame showing the echo. `LAT-ECHO` = this minus stamp 1. The binary decides which key a
//!    frame answers (the last un-presented key whose echo reached the grid).
//!
//! Probes are bounded (ring of [`CAPACITY`] keys, oldest overwritten) and store durations
//! only, never key content. [`LatencyProbe::summary`] yields p50/p99 for the `debug.stats`
//! IPC and the latency overlay; [`LatencyProbe::metrics`] yields `LAT-KEY` / `LAT-ECHO`
//! [`Metric`]s so the e2e runner (M17) uses the same budget check as the headless benches.
//! Presentation-time feedback (`LAT-PHOTON`) is out of scope here: it is recorded as
//! unavailable unless `wp_presentation.clock_id` is `CLOCK_MONOTONIC` (T-23 §1).

use super::{Kind, Metric};

/// Keys remembered at once.
pub const CAPACITY: usize = 4096;

/// Handle for a stamped key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyId(u64);

#[derive(Clone, Copy, Debug, Default)]
struct Slot {
    id: u64,
    start: u64,
    key_ns: Option<u64>,
    echo_ns: Option<u64>,
}

/// p50 / p99 in nanoseconds plus the sample count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Summary {
    /// Samples.
    pub n: usize,
    /// Median.
    pub p50_ns: u64,
    /// 99th percentile (nearest rank).
    pub p99_ns: u64,
}

/// Records key-to-commit latency; see the module docs.
#[derive(Debug)]
pub struct LatencyProbe {
    slots: Vec<Slot>,
    next: u64,
}

impl Default for LatencyProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl LatencyProbe {
    /// An empty probe (allocates its ring once).
    #[must_use]
    pub fn new() -> Self {
        LatencyProbe {
            slots: vec![Slot::default(); CAPACITY],
            next: 1,
        }
    }

    /// Stamp a key's arrival (client stamp, ns).
    pub fn key(&mut self, t_ns: u64) -> KeyId {
        let id = self.next;
        self.next += 1;
        self.slots[(id as usize) % CAPACITY] = Slot {
            id,
            start: t_ns,
            key_ns: None,
            echo_ns: None,
        };
        KeyId(id)
    }

    fn slot(&mut self, k: KeyId) -> Option<&mut Slot> {
        let s = &mut self.slots[(k.0 as usize) % CAPACITY];
        (s.id == k.0).then_some(s)
    }

    /// The first PTY write carrying the key returned at `t_ns`. Later calls are ignored.
    pub fn pty_written(&mut self, k: KeyId, t_ns: u64) {
        if let Some(s) = self.slot(k)
            && s.key_ns.is_none()
        {
            s.key_ns = Some(t_ns.saturating_sub(s.start));
        }
    }

    /// `present()` returned for the frame showing the key's echo. Later calls are ignored.
    pub fn presented(&mut self, k: KeyId, t_ns: u64) {
        if let Some(s) = self.slot(k)
            && s.echo_ns.is_none()
        {
            s.echo_ns = Some(t_ns.saturating_sub(s.start));
        }
    }

    fn collect(&self, f: impl Fn(&Slot) -> Option<u64>) -> Vec<u64> {
        let mut v: Vec<u64> = self
            .slots
            .iter()
            .filter(|s| s.id != 0)
            .filter_map(f)
            .collect();
        v.sort_unstable();
        v
    }

    fn summarize(v: &[u64]) -> Option<Summary> {
        if v.is_empty() {
            return None;
        }
        let rank = |p: f64| ((p * v.len() as f64).ceil() as usize).clamp(1, v.len()) - 1;
        Some(Summary {
            n: v.len(),
            p50_ns: v[rank(0.50)],
            p99_ns: v[rank(0.99)],
        })
    }

    /// Key press to PTY write.
    #[must_use]
    pub fn key_summary(&self) -> Option<Summary> {
        Self::summarize(&self.collect(|s| s.key_ns))
    }

    /// Key press to presented echo frame.
    #[must_use]
    pub fn echo_summary(&self) -> Option<Summary> {
        Self::summarize(&self.collect(|s| s.echo_ns))
    }

    /// `LAT-KEY` (p99), `LAT-ECHO` (p99) and `LAT-ECHO/p50` as metrics in ms; absent series are omitted.
    #[must_use]
    pub fn metrics(&self) -> Vec<Metric> {
        let mut out = Vec::new();
        for (id, s) in [
            ("LAT-KEY", self.key_summary()),
            ("LAT-ECHO", self.echo_summary()),
        ] {
            if let Some(s) = s {
                out.push(Metric {
                    id: id.into(),
                    value: s.p99_ns as f64 / 1e6,
                    unit: "ms",
                    kind: Kind::E2eP99,
                    higher_is_better: false,
                });
            }
        }
        // T-23 §2 `LAT-ECHO` also has a p50 limit (<= 4 ms).
        if let Some(s) = self.echo_summary() {
            out.push(Metric {
                id: "LAT-ECHO/p50".into(),
                value: s.p50_ns as f64 / 1e6,
                unit: "ms",
                kind: Kind::E2eMedian,
                higher_is_better: false,
            });
        }
        out
    }
}
