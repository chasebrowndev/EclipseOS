// SPDX-License-Identifier: AGPL-3.0-only
//! The Eclipse bar.
//!
//! A layer surface that lists windows over the control socket and acts over
//! it too. `wlr_foreign_toplevel_management` is deliberately absent from the
//! compositor (VOL1 §2), so there is no window handle here that is not a
//! `u64` the compositor handed us by name.

pub mod app;
pub mod audio;
pub mod center;
pub mod clock;
pub mod conn;
pub mod icons;
pub mod launcher;
pub mod model;
pub mod toasts;
pub mod view;

/// Bar height in logical pixels, and therefore its exclusive zone.
///
/// The number itself is `tokens::bar::HEIGHT` — the layer surface wants a
/// `u32` and the view wants an `f32`, and only one of them may be the source.
pub const HEIGHT: u32 = eclipse_ui::tokens::bar::HEIGHT as u32;

/// How `main` tells [`app::App::new`] which connector this process is bound
/// to. The daemon builder takes a bare `fn() -> App`, so there is nowhere to
/// pass it as an argument; the supervisor sets it on each child instead.
pub const OUTPUT_ENV: &str = "ECLIPSE_BAR_OUTPUT";
