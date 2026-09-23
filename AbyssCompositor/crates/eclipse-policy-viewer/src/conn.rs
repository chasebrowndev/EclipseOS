// SPDX-License-Identifier: AGPL-3.0-only
//! One fail-soft read of `decoration.rounding` at startup, the same shape as
//! `eclipse-launcher`'s `fetch_terminal_command` (TERM-01).
//!
//! This is the only thing the viewer ever asks the control socket — it reads
//! policy straight off disk (`read::load`) precisely because "the compositor
//! is never asked" for policy is the point of this pane. It is a plain
//! `xdg_toplevel` with no live-reconfigure channel, so this is a
//! startup-only read; a `decoration.rounding` change reaches it on the next
//! launch, not the current one.

/// `None` on any failure — the caller keeps its compile-time token default.
pub fn fetch_glass_radius() -> Option<f32> {
    let mut client = eclipse_ipc::Client::connect().ok()?;
    eclipse_ui::ipc::fetch_config_radius(&mut client, "decoration.rounding")
}
