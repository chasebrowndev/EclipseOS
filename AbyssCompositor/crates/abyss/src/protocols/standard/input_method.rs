// SPDX-License-Identifier: AGPL-3.0-only
//! `zwp_input_method_v2` (COMP-06 §1): the IME side of text input.
//!
//! The compositor owns the input-method popup: it is positioned under the
//! text cursor rectangle the client reported and drawn above everything the
//! shell draws. IME content (preedit, commits) is human input — never logged.

use smithay::{
    delegate_input_method_manager,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Rectangle},
    wayland::input_method::{InputMethodHandler, PopupSurface},
};

use crate::state::AbyssState;

impl InputMethodHandler for AbyssState {
    fn new_popup(&mut self, surface: PopupSurface) {
        self.input_method_popup = Some(surface);
    }

    fn popup_repositioned(&mut self, surface: PopupSurface) {
        self.input_method_popup = Some(surface);
    }

    fn dismiss_popup(&mut self, surface: PopupSurface) {
        if self.input_method_popup.as_ref() == Some(&surface) {
            self.input_method_popup = None;
        }
    }

    /// Geometry of the surface the IME is attached to, in global coordinates.
    fn parent_geometry(&self, parent: &WlSurface) -> Rectangle<i32, Logical> {
        self.space
            .elements()
            .find(|w| w.toplevel().map(|t| t.wl_surface() == parent).unwrap_or(false))
            .and_then(|w| self.space.element_geometry(w))
            .unwrap_or_default()
    }
}

delegate_input_method_manager!(AbyssState);

/// Top-left of the popup in global logical coordinates, or `None` if it is
/// gone or has no parent yet.
pub fn popup_location(popup: &PopupSurface) -> Option<smithay::utils::Point<i32, Logical>> {
    if !popup.alive() {
        return None;
    }
    let parent = popup.get_parent()?;
    let rect = popup.text_input_rectangle();
    let offset: smithay::utils::Point<i32, Logical> = (rect.loc.x, rect.loc.y + rect.size.h).into();
    Some(parent.location.loc + offset)
}
