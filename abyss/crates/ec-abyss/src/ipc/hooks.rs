// SPDX-License-Identifier: AGPL-3.0-only
//! Stubs for the trusted-UI side of the console wave's human-socket methods.
//!
//! The decision queue and the install review are TCB (`trusted_ui/`, the
//! policyd link). This module is non-TCB and must not reach into them, so each
//! seam is a marked stub the main thread replaces with the real call.

use std::path::Path;

use crate::state::AbyssState;

/// Open the decision queue (COMP-10 §3.13), Deny focused.
// TCB seam: replace the body with `crate::trusted_ui::queue::open(state)`.
pub(crate) fn open_decision_queue(state: &mut AbyssState) {
    crate::trusted_ui::queue::open(state);
}

/// How many consent prompts are parked right now.
// TCB seam: replace the body with the trusted_ui consent queue's parked count.
pub(crate) fn decisions_pending(state: &AbyssState) -> u32 {
    crate::trusted_ui::queue::pending(state)
}

/// Begin an agent install from `path` (already absolute, an existing
/// directory, canonicalised). The review itself happens in a trusted modal.
// TCB seam: forward to policyd as `install_begin {path}` (console-plan C1) and
// return the refusal reason as a static code; until then fail closed.
pub(crate) fn install_begin(state: &mut AbyssState, path: &Path) -> Result<(), &'static str> {
    crate::trusted_ui::install::begin(state, path)
}
