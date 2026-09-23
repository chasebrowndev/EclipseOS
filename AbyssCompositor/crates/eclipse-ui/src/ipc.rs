// SPDX-License-Identifier: AGPL-3.0-only
//! One fail-soft read of a compositor-config radius over the control socket.
//!
//! Every pane's glass content radius is meant to live-sync to the
//! compositor's `decoration.rounding` (or, for hyperion's bar sheet,
//! `bar.rounding`) so the client-drawn glass can never drift from the blur
//! backdrop the compositor draws behind it. This mirrors
//! `eclipse-launcher`'s `fetch_terminal_command` (TERM-01): an unset key, a
//! socket nothing is listening on, and a denied query all collapse to the
//! same answer, `None`, so every caller falls back to its own compile-time
//! token ([`crate::tokens::radius::CARD`] or
//! [`crate::tokens::bar::RADIUS_SHEET`]).

use serde_json::json;

/// Read `path` (e.g. `"decoration.rounding"`) as an `f32`. `None` on any
/// failure — no connection, no such key, or a value that is not a number.
pub fn fetch_config_radius(client: &mut eclipse_ipc::Client, path: &str) -> Option<f32> {
    let reply = client.call("get_config", json!({ "path": path })).ok()?;
    let value = reply.get("keys")?.as_array()?.first()?.get("value")?;
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|i| i as f64))
        .map(|v| v as f32)
}
