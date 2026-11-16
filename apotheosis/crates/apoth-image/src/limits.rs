// SPDX-License-Identifier: AGPL-3.0-only
//! Every cap in one place. Defaults follow T-15 §4 where the spec gives a number; the
//! rest are reported as spec gaps and chosen conservatively.

/// Caps for one pane's graphics. Values are bytes unless named otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// Per-axis pixel cap (T-15 §4, GRAPHICS-04).
    pub max_axis: u32,
    /// Decoded-size cap per image (GRAPHICS-14: 400 MiB at most).
    pub max_decoded_bytes: u64,
    /// Largest base64 payload of one kitty APC chunk (T-15 §1: 4096).
    pub max_chunk_b64: usize,
    /// Cap on the total base64 bytes of one chunked transfer, independent of the chunk size.
    pub max_transfer_b64: usize,
    /// Concurrent half-received chunked transfers per pane.
    pub max_pending_transfers: usize,
    /// Stale timeout for a half-received transfer, milliseconds (GRAPHICS-03: 10 s).
    pub chunk_stale_ms: u64,
    /// `graphics.quota`: per-pane image budget (default 320 MiB).
    pub quota: u64,
    /// `graphics.quota-total`: per-process image budget (default 1 GiB).
    pub quota_total: u64,
    /// `graphics.animation-quota`: separate frame budget (parsed only in M25a).
    pub animation_quota: u64,
    /// Sixel: byte cap of the DCS stream.
    pub sixel_max_bytes: usize,
    /// Sixel: decoded pixel cap, independent of the byte cap.
    pub sixel_max_pixels: u64,
    /// Sixel colour registers (xterm/XTSMGRAPHICS ceiling 1024).
    pub sixel_max_registers: usize,
    /// Reply queue per pane (T-18 §5: 64 KiB).
    pub reply_queue_bytes: usize,
    /// Reply token bucket: burst size and refill per second.
    pub reply_burst: u32,
    pub reply_per_sec: u32,
}

impl Default for Limits {
    fn default() -> Self {
        const MIB: u64 = 1024 * 1024;
        Self {
            max_axis: 10_000,
            max_decoded_bytes: 400 * MIB,
            max_chunk_b64: 4096,
            max_transfer_b64: 128 * MIB as usize,
            max_pending_transfers: 8,
            chunk_stale_ms: 10_000,
            quota: 320 * MIB,
            quota_total: 1024 * MIB,
            animation_quota: 5 * 320 * MIB,
            sixel_max_bytes: 32 * MIB as usize,
            sixel_max_pixels: 64_000_000,
            sixel_max_registers: 1024,
            reply_queue_bytes: 64 * 1024,
            reply_burst: 64,
            reply_per_sec: 32,
        }
    }
}

impl Limits {
    /// The effective per-image decoded cap: no image may exceed the pane quota either.
    pub fn image_cap(&self) -> u64 {
        self.max_decoded_bytes.min(self.quota)
    }
}
