// SPDX-License-Identifier: AGPL-3.0-only
//! Hyperion, the Eclipse taskbar. Users see it as "the taskbar"; Hyperion is
//! the internal name.
//!
//! A layer surface that lists windows over the control socket and acts over
//! it too. `wlr_foreign_toplevel_management` is deliberately absent from the
//! compositor (VOL1 §2), so there is no window handle here that is not a
//! `u64` the compositor handed us by name.

pub mod app;
pub mod audio;
pub mod clock;
pub mod conn;
pub mod eye;
pub mod icons;
pub mod layout;
pub mod model;
pub mod motion;
pub mod pairing;
#[cfg(debug_assertions)]
pub mod preview;
pub mod radio;
pub mod services;
pub mod view;
pub mod widgets;

/// Bar height in logical pixels, and therefore its exclusive zone.
///
/// The number itself is `tokens::bar::HEIGHT` — the layer surface wants a
/// `u32` and the view wants an `f32`, and only one of them may be the source.
pub const HEIGHT: u32 = eclipse_ui::tokens::bar::HEIGHT as u32;
