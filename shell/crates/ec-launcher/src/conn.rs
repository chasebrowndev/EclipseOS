// SPDX-License-Identifier: AGPL-3.0-only
//! One fail-soft read of `misc.terminal-command` at startup (TERM-01), plus
//! one of `decoration.rounding` (BLUR-06) alongside it.
//!
//! An unset key, a socket nothing is listening on, and a denied query all
//! collapse to the same answer: no terminal command, so `Terminal=true`
//! entries stay hidden. That is the same "empty when nothing is there"
//! behavior every other pane in this repo gives its control-socket read.
//!
//! The launcher is spawned fresh by every keybind press and never stays open
//! long enough to have a live-reconfigure channel, so `fetch_glass` (the
//! radius and the blur mode) is read once here too, at the same startup
//! moment as the terminal command; a `decoration` change reaches it on the
//! next launch, not the current one.

use serde_json::json;

/// `None` on any failure — the launcher does not distinguish "unset" from
/// "couldn't ask", since both mean the same thing to `apps::scan`/`launch`.
pub fn fetch_terminal_command() -> Option<String> {
    let mut client = ec_ipc::Client::connect().ok()?;
    let reply = client
        .call("get_config", json!({ "path": "misc.terminal-command" }))
        .ok()?;
    reply
        .get("keys")?
        .as_array()?
        .first()?
        .get("value")?
        .as_str()
        .map(str::to_owned)
}

/// `decoration.rounding` and whether blur is on; each `None` on any failure,
/// and the caller keeps its compile-time default.
pub fn fetch_glass() -> (Option<f32>, Option<bool>) {
    ec_ui::ipc::fetch_glass()
}

/// `bar.launcher-style` is `"menu"`: the taskbar's start menu is the launcher
/// on this desktop, so the keybind that spawns this binary should open that
/// menu rather than a second, centred one. `false` on any failure.
pub fn menu_style() -> bool {
    let Ok(mut client) = ec_ipc::Client::connect() else {
        return false;
    };
    client
        .call("get_config", json!({ "path": "bar.launcher-style" }))
        .ok()
        .and_then(|reply| {
            reply
                .get("keys")?
                .as_array()?
                .first()?
                .get("value")?
                .as_str()
                .map(|style| style == "menu")
        })
        .unwrap_or(false)
}

/// Ask the compositor to open the bar's menu on the focused output. `true`
/// only when a bar was listening to take it (`delivered`); anything else —
/// no socket, a refusal, no bar running — is `false`, and the caller falls
/// back to drawing the centred launcher itself.
pub fn open_bar_menu() -> bool {
    let Ok(mut client) = ec_ipc::Client::connect() else {
        return false;
    };
    client
        .call("open_launcher", json!({}))
        .ok()
        .and_then(|reply| reply.get("delivered")?.as_bool())
        .unwrap_or(false)
}
