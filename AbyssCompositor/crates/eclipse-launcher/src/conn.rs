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
    let mut client = eclipse_ipc::Client::connect().ok()?;
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
    eclipse_ui::ipc::fetch_glass()
}
