// SPDX-License-Identifier: AGPL-3.0-only
//! The seams to the trusted UI and the audit log (COMP-19 §5, §6).
//!
//! Everything here is a stub: the slot's card, preview, arming and commit are
//! drawn and decided in `trusted_ui/`, which is TCB, and the refused-input
//! record is `audit/`'s. Main replaces each body with the real call; the
//! signatures are the contract.
//!
//! The functions in `super` that the TCB calls back with (`send_geometry`,
//! `send_state`, `send_committed`, `send_cancelled`, `slot_rect`, `host_of`,
//! `slots`, `draft`) are the other half of the seam.

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;

use super::Kind;
use crate::input::Origin;
use crate::state::AbyssState;

/// The compositor-chosen size of a slot card, surface-local logical px
/// (COMP-19 §5: "size is the compositor's"). Sent as `geometry` when the slot
/// is created.
///
/// TCB-HOOK: answer from `trusted_ui::slot` (the card's size for the slot's
/// current preview). When the size changes later, call
/// [`super::send_geometry`].
pub fn slot_size(_kind: Kind) -> (i32, i32) {
    (360, 120)
}

/// The slot's draft was replaced (after rate limiting and coalescing, so at
/// most 10 a second per slot). The slot must disarm (§6) and a new preview
/// start (A-08 §5.2). Read the draft with [`super::draft`].
///
/// TCB-HOOK: `trusted_ui::slot::on_draft(state, slot_id)`.
pub fn draft_changed(_state: &mut AbyssState, _slot_id: u64) {}

/// The slot moved (`move`) and must disarm (§6). Read the new rectangle with
/// [`super::slot_rect`].
///
/// TCB-HOOK: `trusted_ui::slot::on_moved(state, slot_id)`.
pub fn slot_moved(_state: &mut AbyssState, _slot_id: u64) {}

/// The slot is going away: cancelled by the client, its protected object
/// destroyed, its surface or client gone. Called before the record is
/// removed, so [`super::draft`] and [`super::slot_rect`] still answer.
///
/// TCB-HOOK: `trusted_ui::slot::on_gone(state, slot_id)` drops the preview and
/// any armed state.
pub fn slot_gone(_state: &mut AbyssState, _slot_id: u64) {}

/// A physical, unmodified Return or KP_Enter press while keyboard focus is in
/// the slot's host protected surface (§6). The press and its release never
/// reach the client. Armed: commit. Not armed: drop it, pulse the slot, never
/// queue it. `now_ms` is the event time.
///
/// TCB-HOOK: `trusted_ui::slot::on_enter(state, slot_id, now_ms)`.
pub fn enter(_state: &mut AbyssState, _slot_id: u64, _now_ms: u32) {}

/// A physical pointer button press inside the slot's rectangle (§6). The
/// client never sees it. `local_x`, `local_y` are relative to the slot's top
/// left, logical px.
///
/// TCB-HOOK: `trusted_ui::slot::on_click(state, slot_id, x, y, now_ms)`.
pub fn click(_state: &mut AbyssState, _slot_id: u64, _local_x: i32, _local_y: i32, _now_ms: u32) {}

/// Non-physical input from an agent origin (`AgentSeat`, `AgentCompat`) was
/// dropped before delivery to the protected `surface` (COMP-19 §3). Called
/// once per dropped event, so the audit side decides how to coalesce. Carries
/// the origin and the surface, never the input's content.
///
/// TCB-HOOK: `audit::input_refused(state, origin, surface)` (F-13).
pub fn audit_refused(_state: &mut AbyssState, _origin: Origin, _surface: &WlSurface) {}
