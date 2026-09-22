// SPDX-License-Identifier: AGPL-3.0-only
//! One fail-soft read of `misc.terminal-command` at startup (TERM-01).
//!
//! An unset key, a socket nothing is listening on, and a denied query all
//! collapse to the same answer: no terminal command, so `Terminal=true`
//! entries stay hidden. That is the same "empty when nothing is there"
//! behavior every other pane in this repo gives its control-socket read.

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
