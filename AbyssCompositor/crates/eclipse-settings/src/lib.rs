// SPDX-License-Identifier: AGPL-3.0-only
//! The Eclipse settings app (DP-5, COMP-17 §3).
//!
//! Every control on every pane is generated from `get_config {schema: true}`.
//! There is no hand-written key list anywhere in this crate, and
//! `tests/coverage.rs` fails the build if the compositor grows a key this app
//! cannot render.
//!
//! The app keeps no persistent state of its own: window size and the selected
//! pane are not remembered. `abyss.kdl` is the only state there is.

pub mod app;
pub mod conn;
pub mod output;
pub mod pane;
pub mod schema;
