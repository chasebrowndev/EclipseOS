// SPDX-License-Identifier: AGPL-3.0-only
//! The desktop background: a `Layer::Background` surface on every output,
//! a solid colour with the configured picture on it.
//!
//! An ordinary layer-shell client outside the TCB. It reads the `wallpaper`
//! config node over the control socket and follows the output list the same
//! way the taskbar does; it holds no capability and grants none.

pub mod app;
pub mod config;
pub mod view;
