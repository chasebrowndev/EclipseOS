// SPDX-License-Identifier: AGPL-3.0-only
//! The Eclipse bar.
//!
//! A layer surface that lists windows over the control socket and acts over
//! it too. `wlr_foreign_toplevel_management` is deliberately absent from the
//! compositor (VOL1 §2), so there is no window handle here that is not a
//! `u64` the compositor handed us by name.

pub mod app;
pub mod clock;
pub mod conn;
pub mod model;
pub mod toasts;
pub mod view;

/// Bar height in logical pixels, and therefore its exclusive zone. Matches the
/// reference shell's `barHeight`; the cells were drawn around it.
pub const HEIGHT: u32 = 34;
