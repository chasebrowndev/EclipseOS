// SPDX-License-Identifier: AGPL-3.0-only
//! Pointer-side globals from the COMP-06 §1 table that smithay drives entirely
//! from the seat, with no compositor policy of their own.
//!
//! - `zwp_relative_pointer_v1` — unaccelerated deltas alongside absolute
//!   motion. Half of mouse-look; the other half is `pointer_constraints`.
//! - `zwp_pointer_gestures_v1` v3 — touchpad swipe/pinch/hold, forwarded from
//!   whatever the backend reports.
//! - `wp_cursor_shape_v1` — a client naming a cursor from the shape enum rather
//!   than uploading its own surface. Arrives at [`crate::state::AbyssState`] as
//!   an ordinary `SeatHandler::cursor_image` call, so the existing cursor path
//!   handles it unchanged.
//!
//! Delivery is smithay's: the pointer handle fans events out to whichever of
//! these a client has bound. There is nothing to gate — every one of them is a
//! reformulation of input the focused client already receives.

use crate::state::AbyssState;

smithay::delegate_relative_pointer!(AbyssState);
smithay::delegate_pointer_gestures!(AbyssState);
smithay::delegate_cursor_shape!(AbyssState);
