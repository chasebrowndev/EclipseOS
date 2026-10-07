// SPDX-License-Identifier: AGPL-3.0-only
//! One-button trusted notices (COMP-11 §2: a rejected table "raises a
//! trusted-UI error"). **TCB.** Fixed text only; nothing here takes runtime
//! text, so nothing here can be steered.

use super::modal::{Button, Modal, Role};
use crate::state::AbyssState;

/// Tokens for notices. Below `erase::TOKEN_BASE`, above `phrase::TOKEN`.
pub(super) const TOKEN: u64 = (1 << 61) | (1 << 60);

const TABLE_HEADING: &str = "Policy update rejected";
const TABLE_BODY: &str = "policyd sent a policy that did not verify or did not move forward. \
The policy already in force stays in force. The journal has the reason.";

fn notice(heading: &'static str, body: &'static str) -> Option<Modal> {
    Modal::new(
        TOKEN,
        heading,
        None,
        body,
        "Details:",
        "See: journalctl --user -t ec-abyss",
        vec![Button {
            label: "OK",
            role: Role::Safe,
        }],
    )
    .ok()
}

/// A pushed table was refused. Shown if the seat is free; the journal has
/// it either way, so a notice never queues behind a prompt that matters more.
pub fn table_rejected(state: &mut AbyssState) {
    if let Some(m) = notice(TABLE_HEADING, TABLE_BODY) {
        let _ = super::open(state, m);
    }
}

/// Notices carry one fixed token; there is nothing to look up.
pub fn owns(token: u64) -> bool {
    token == TOKEN
}
