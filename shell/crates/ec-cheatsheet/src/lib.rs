// SPDX-License-Identifier: AGPL-3.0-only
//! The keybind cheat sheet: the chords the compositor is listening for, read
//! from the live config and grouped by what they do.

pub mod app;
pub mod instance;
pub mod model;
pub mod view;

/// The panel's surface: the window spec's 1120 x 720, centred on the output.
pub const WIDTH: u32 = 1120;
pub const HEIGHT: u32 = 720;
