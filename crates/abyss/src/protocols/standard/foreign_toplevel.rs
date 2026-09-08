// SPDX-License-Identifier: AGPL-3.0-only
//! `ext_foreign_toplevel_list_v1` (COMP-06 §1) — the read-only window list a
//! taskbar or a switcher needs, and the one `registryd` reads.
//!
//! Read-only is the whole point: this protocol lists titles, app ids and stable
//! identifiers, and offers no way to focus, close or move anything. The
//! management protocols that do offer that (`wlr_foreign_toplevel_management`)
//! are deliberately absent (COMP-06 §2) — window control goes through the
//! audited agent protocol, not an ambient global.
//!
//! Handles are owned by the window (`Window::user_data`), so a window's entry
//! dies exactly when the window does.

use smithay::{
    desktop::Window,
    wayland::foreign_toplevel_list::{
        ForeignToplevelHandle, ForeignToplevelListHandler, ForeignToplevelListState,
    },
};

use crate::state::AbyssState;

impl ForeignToplevelListHandler for AbyssState {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelListState {
        &mut self.foreign_toplevel_list
    }
}

/// Identity as the rest of the compositor sees it (see `ipc::methods`).
fn identity(window: &Window) -> (String, String) {
    let (app_id, title) = crate::ipc::methods::identity_of(window);
    (title.unwrap_or_default(), app_id.unwrap_or_default())
}

/// Announce a newly mapped window. Called from the single map funnel so the
/// XWayland path is covered by the same code.
pub fn window_mapped(state: &mut AbyssState, window: &Window) {
    let (title, app_id) = identity(window);
    let handle = state
        .foreign_toplevel_list
        .new_toplevel::<AbyssState>(title, app_id);
    window.user_data().insert_if_missing(|| handle);
}

/// Push a title/app_id change. Smithay drops unchanged values, so this is cheap
/// enough to call on every commit.
pub fn window_updated(window: &Window) {
    let Some(handle) = window.user_data().get::<ForeignToplevelHandle>() else {
        return;
    };
    let (title, app_id) = identity(window);
    if handle.title() == title && handle.app_id() == app_id {
        return;
    }
    handle.send_title(&title);
    handle.send_app_id(&app_id);
    handle.send_done();
}

/// The window is gone for good — not merely off the visible workspace.
pub fn window_closed(window: &Window) {
    if let Some(handle) = window.user_data().get::<ForeignToplevelHandle>() {
        handle.send_closed();
    }
}

smithay::delegate_foreign_toplevel_list!(AbyssState);
