// SPDX-License-Identifier: AGPL-3.0-only
//! The control center: what the session is doing, and how to end it.

pub mod app;
pub mod conn;
pub mod view;

/// Width of the panel's surface — the sheet itself, edge to edge.
pub const WIDTH: u32 = 360;
