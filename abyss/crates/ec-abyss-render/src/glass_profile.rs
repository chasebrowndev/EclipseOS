// SPDX-License-Identifier: AGPL-3.0-only
//! Glass/blur profiling counters (COMP-02, liquid-glass Phase 0).
//!
//! Plain integers, no allocation, bumped from the blur chain. Off unless the
//! `EC_GLASS_PROFILE` environment variable is set (read once, when a
//! [`crate::blur::BlurStore`] is created); then a one-line summary goes to the
//! journal every [`REPORT_FRAMES`] frames. The counters are always bumped
//! (a handful of integer adds per rebuild); only the logging is gated.
//!
//! GPU timer queries (`EXT_disjoint_timer_query`) are deliberately not here:
//! smithay 0.7.0's GLES bindings do not expose the extension, so using it would
//! need raw `eglGetProcAddress` function pointers and the crate's first
//! `unsafe`. Wall-clock CPU timings come from `--stats` (`stats.rs`); see
//! `docs/GLASS_PROFILING.md`.

/// Frames between summary lines.
pub const REPORT_FRAMES: u64 = 120;

/// What one chain rebuild cost, as the chain reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rebuild {
    /// Texels of the output redrawn into level 0 (the crop area).
    pub crop_texels: u64,
    /// Texels the age-0 backdrop render clears (the whole output).
    pub cleared_texels: u64,
    /// Down, up and final blits run.
    pub passes: u32,
}

impl Default for GlassProfile {
    /// Reads the environment: this is what a fresh store gets.
    fn default() -> Self {
        Self::from_env()
    }
}

/// Counters for one [`crate::blur::BlurStore`] (one output).
#[derive(Clone, Copy, Debug)]
pub struct GlassProfile {
    enabled: bool,
    /// Frames seen since the last report.
    frames: u64,
    /// Surfaces that asked for blur, summed over the window.
    surfaces: u64,
    rebuilds: u64,
    max_rebuilds_frame: u32,
    level0_clears: u64,
    crop_texels: u64,
    cleared_texels: u64,
    passes: u64,
    max_passes_rebuild: u32,
    /// Rebuilds in the frame being drawn.
    this_frame: u32,
}

impl GlassProfile {
    /// Counters with logging set from `EC_GLASS_PROFILE` (any value but empty
    /// or `0`).
    pub fn from_env() -> Self {
        let on = std::env::var_os("EC_GLASS_PROFILE").is_some_and(|v| !v.is_empty() && v != "0");
        Self::new(on)
    }

    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            frames: 0,
            surfaces: 0,
            rebuilds: 0,
            max_rebuilds_frame: 0,
            level0_clears: 0,
            crop_texels: 0,
            cleared_texels: 0,
            passes: 0,
            max_passes_rebuild: 0,
            this_frame: 0,
        }
    }

    /// One chain rebuild finished.
    pub fn rebuilt(&mut self, r: Rebuild) {
        self.rebuilds += 1;
        self.this_frame += 1;
        self.level0_clears += 1;
        self.crop_texels += r.crop_texels;
        self.cleared_texels += r.cleared_texels;
        self.passes += u64::from(r.passes);
        self.max_passes_rebuild = self.max_passes_rebuild.max(r.passes);
    }

    /// A frame ended with `surfaces` blurred surfaces; log when due.
    pub fn frame(&mut self, surfaces: usize) {
        self.frames += 1;
        self.surfaces += surfaces as u64;
        self.max_rebuilds_frame = self.max_rebuilds_frame.max(self.this_frame);
        self.this_frame = 0;
        if self.frames < REPORT_FRAMES {
            return;
        }
        if self.enabled {
            tracing::info!(
                frames = self.frames,
                avg_surfaces = self.surfaces / self.frames,
                rebuilds = self.rebuilds,
                max_rebuilds_per_frame = self.max_rebuilds_frame,
                level0_clears = self.level0_clears,
                crop_texels = self.crop_texels,
                cleared_texels = self.cleared_texels,
                passes = self.passes,
                max_passes_per_rebuild = self.max_passes_rebuild,
                "glass profile"
            );
        }
        *self = Self::new(self.enabled);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_fold_and_reset_at_the_interval() {
        let mut p = GlassProfile::new(false);
        let r = Rebuild {
            crop_texels: 100,
            cleared_texels: 1000,
            passes: 5,
        };
        p.rebuilt(r);
        p.rebuilt(r);
        p.frame(3);
        assert_eq!((p.rebuilds, p.max_rebuilds_frame, p.crop_texels), (2, 2, 200));
        assert_eq!((p.level0_clears, p.passes, p.surfaces), (2, 10, 3));
        for _ in 1..REPORT_FRAMES {
            p.frame(3);
        }
        assert_eq!((p.frames, p.rebuilds, p.passes), (0, 0, 0));
    }
}
