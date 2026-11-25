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

/// `animations.toast`, over one short-lived connection: the style and
/// motion the compositor resolved for a card arriving or leaving. `None`
/// when nothing answers or the compositor predates `animations`, and the
/// caller keeps what it had (at first, no motion: cards land as they always
/// did).
pub fn fetch_anim() -> Option<ec_ui::ipc::EventAnim> {
    let mut client = ec_ipc::Client::connect().ok()?;
    ec_ui::ipc::fetch_animation(&mut client, "toast")
}

/// Whether the taskbar sits on the bottom edge (`bar.position`), so the stack
/// is offset from the bar on that edge. `None` when nothing answers.
pub fn fetch_bar_bottom() -> Option<bool> {
    let mut client = ec_ipc::Client::connect().ok()?;
    let reply = client
        .call("get_config", serde_json::json!({ "path": "bar.position" }))
        .ok()?;
    let v = reply.get("keys")?.as_array()?.first()?.get("value")?.as_str()?;
    Some(v == "bottom")
}

/// The focused output's width in logical px, so cards fit a narrow screen.
/// `None` when nothing answers or the output has no mode.
pub fn fetch_output_width() -> Option<f32> {
    let mut client = ec_ipc::Client::connect().ok()?;
    let reply = client.call("get_outputs", serde_json::json!({})).ok()?;
    parse_output_width(&reply)
}

/// The focused output's (else the first one's) mode width over its scale.
pub fn parse_output_width(reply: &serde_json::Value) -> Option<f32> {
    let rows = reply.as_array()?;
    let row = rows
        .iter()
        .find(|r| r.get("focused").and_then(|f| f.as_bool()) == Some(true))
        .or_else(|| rows.first())?;
    let w = row.get("mode")?.get("width")?.as_f64()?;
    let scale = row
        .get("scale")
        .and_then(|s| s.as_f64())
        .filter(|s| *s > 0.0)
        .unwrap_or(1.0);
    Some((w / scale) as f32)
}
