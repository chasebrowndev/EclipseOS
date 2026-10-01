// SPDX-License-Identifier: AGPL-3.0-only
//! One fail-soft read of the compositor's glass — `decoration.rounding` and
//! `decoration.blur.mode` — at startup, exactly the same shape as
//! `ec-launcher`'s `fetch_terminal_command` (TERM-01).
//!
//! The panel has no other reason to open the control socket — it is a menu
//! spawned by a keybind that goes away in seconds and has no live-reconfigure
//! channel today (its subscription only watches the system status bus), so
//! this is a startup-only read; a `decoration` change picked up here does not
//! reach an already-open panel, only the next one a keybind opens.

/// The radius and whether blur is on; each `None` on any failure, and the
/// caller keeps its compile-time default.
pub fn fetch_glass() -> (Option<f32>, Option<bool>) {
    ec_ui::ipc::fetch_glass()
}
