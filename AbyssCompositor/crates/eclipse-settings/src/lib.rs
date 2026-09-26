// SPDX-License-Identifier: AGPL-3.0-only
//! The Eclipse settings app (DP-5, COMP-17 §3).
//!
//! Every control on every pane is generated from `get_config {schema: true}`.
//! The one hand-written key list is the Taskbar pane's grouping
//! (`taskbar.rs`), and a `bar` key it does not name still lands in its
//! "other" column. `tests/coverage.rs` fails the build if the compositor
//! grows a key this app cannot render.
//!
//! The app keeps no persistent state of its own: window size and the selected
//! pane are not remembered. `abyss.kdl` is the only state there is.

pub mod app;
pub mod bar_preview;
pub mod conn;
pub mod editor;
pub mod network;
pub mod output;
pub mod pane;
pub mod schema;
pub mod taskbar;
pub mod tray;
