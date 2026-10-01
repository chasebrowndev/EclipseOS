// SPDX-License-Identifier: AGPL-3.0-only
//! One fail-soft read of the compositor's glass — `decoration.rounding` and
//! `decoration.blur.mode` — at startup, exactly the same shape as
//! `ec-launcher`'s `fetch_terminal_command` (TERM-01).
//!
//! The stack has no other reason to open the control socket — it has no
//! live-reconfigure channel today (its only subscription is the tick that
//! drains the notification bus), so this is a startup-only read; a
//! `decoration` change picked up here does not reach an already-running
//! stack until it restarts.

/// The radius and whether blur is on; each `None` on any failure, and the
/// caller keeps its compile-time default.
pub fn fetch_glass() -> (Option<f32>, Option<bool>) {
    ec_ui::ipc::fetch_glass()
}
