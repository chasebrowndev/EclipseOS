// SPDX-License-Identifier: AGPL-3.0-only
//! `eclipse_agent_seat_v1`: one virtual seat per agent (COMP-08 §4,
//! COMP-04 §3). Not TCB, and decides nothing: every request goes through
//! `policy::enforce::submit`, and this module only executes what that allows
//! and carries the answer back.
//!
//! Placeholder until the seat itself lands: no agent has a seat, so there is
//! no focus to act on, nothing executes, and there is no object to answer
//! on. `policy::enforce` is complete against these three entry points.

use ec_protocols::agent::server::eclipse_agent_v1::Status;
use smithay::desktop::Window;

use crate::policy::enforce::Act;
use crate::state::AbyssState;

/// The window agent `agent`'s seat has focus on.
pub fn focused_window(_state: &AbyssState, _agent: u64) -> Option<Window> {
    None
}

/// Run an allowed act on `window`. `Ok` carries the result detail (the text
/// path taken, for `text`).
pub fn execute(
    _state: &mut AbyssState,
    _agent: u64,
    _act: &Act,
    _window: &Window,
) -> Result<String, (Status, String)> {
    Err((Status::ClientGone, "agent seats are not built yet".into()))
}

/// Send a delayed `result` for `req_id` on agent `agent`'s seat object.
pub fn reply(_state: &mut AbyssState, _agent: u64, _req_id: u32, _status: Status, _detail: &str) {}
