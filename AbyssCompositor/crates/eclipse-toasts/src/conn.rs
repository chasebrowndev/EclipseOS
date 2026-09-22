// SPDX-License-Identifier: AGPL-3.0-only
//! One fail-soft read of `decoration.rounding` at startup, exactly the same
//! shape as `eclipse-launcher`'s `fetch_terminal_command` (TERM-01).
//!
//! The stack has no other reason to open the control socket — it has no
//! live-reconfigure channel today (its only subscription is the tick that
//! drains the notification bus), so this is a startup-only read; a
//! `decoration.rounding` change picked up here does not reach an
//! already-running stack until it restarts.

/// `None` on any failure — the caller keeps its compile-time token default.
pub fn fetch_glass_radius() -> Option<f32> {
    let mut client = eclipse_ipc::Client::connect().ok()?;
    eclipse_ui::ipc::fetch_config_radius(&mut client, "decoration.rounding")
}
