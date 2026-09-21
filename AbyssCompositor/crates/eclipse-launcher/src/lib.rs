// SPDX-License-Identifier: AGPL-3.0-only
//! The application launcher: type, pick, run.

pub mod app;
pub mod view;

/// Width of the launcher's surface, including its outer padding.
pub const WIDTH: u32 = 560;

/// How many result rows the surface is tall enough to hold.
///
/// A constant for the same reason the control center's row set is: a layer
/// surface fixes its size before the boot fn runs, so the height cannot
/// depend on how many entries matched. Fewer matches leave the tail empty;
/// more are cut off, and the view says how many.
pub const MAX_ROWS: usize = 8;
