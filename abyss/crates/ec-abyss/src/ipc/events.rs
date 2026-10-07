// SPDX-License-Identifier: AGPL-3.0-only
//! Console-wave events emitted on the human socket.

use serde_json::json;

use crate::state::AbyssState;

/// Announce `decisions_pending {count}` if the parked-prompt count differs from
/// the last one announced. The trusted-UI consent queue calls this whenever it
/// parks or resolves a prompt; calling it with no change is a no-op.
pub fn decisions_pending_changed(state: &mut AbyssState) {
    let count = super::hooks::decisions_pending(state);
    if count == state.ipc.last_pending {
        return;
    }
    state.ipc.last_pending = count;
    super::emit(state, "decisions_pending", json!({ "count": count }));
}
