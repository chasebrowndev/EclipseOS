// SPDX-License-Identifier: AGPL-3.0-only
//! `zwp_tablet_manager_v2` (COMP-06 §1) — graphics tablets, styli and pads.
//!
//! The manager global is unconditional: a tablet seat only ever mirrors devices
//! the compositor already owns, so binding it grants nothing. Individual tablets
//! are advertised from the libinput device-added path (COMP-04); until a tablet
//! is plugged in the seat is simply empty, which is what the protocol expects.
//!
//! The one callback is a client asking to replace the stylus cursor image; it is
//! handled the same way `SeatHandler::cursor_image` is — the compositor draws the
//! cursor, so the request is recorded and nothing else.

use smithay::{backend::input::TabletToolDescriptor, wayland::tablet_manager::TabletSeatHandler};

use crate::state::AbyssState;

impl TabletSeatHandler for AbyssState {
    fn tablet_tool_image(
        &mut self,
        _tool: &TabletToolDescriptor,
        _image: smithay::input::pointer::CursorImageStatus,
    ) {
        // Cursor rendering is compositor-side (COMP-02); see SeatHandler::cursor_image.
    }
}

smithay::delegate_tablet_manager!(AbyssState);
