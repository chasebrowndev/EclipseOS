// SPDX-License-Identifier: AGPL-3.0-only
//! Handing the lock surfaces to the compositor's glass (ADR 0129).
//!
//! `iced_sessionlock` runs `window::run` for nobody, and shares no connection,
//! so the xdg path of `ec_ui::glass` cannot reach a lock surface. The vendored
//! copy (`shell/vendor/PATCHES.md`, hunk 4) calls [`hook`] with each lock
//! surface's `wl_surface` as the window is made, before its first frame. The
//! hook binds `eclipse_glass_v1` on a private queue of the surface's own
//! connection and gives the surface its glass object (`ec_ui::glass`).
//!
//! Drawing only. Fail-soft: not abyss, not allow-listed, not `glass` blur mode
//! or a failed bind all leave `ec_ui::glass::available()` false and the view
//! draws its opaque ground and glass-styled panes.

use iced::window;
use sessionlockev::reexport::wayland_client::WlSurface;

/// Install the surface hook. Call before the lock surfaces are made (the app's
/// boot does); only the first call takes effect.
pub fn install() {
    iced_sessionlock::set_surface_hook(hook);
}

fn hook(id: window::Id, surface: &WlSurface) {
    let on = ec_ui::glass::attach_surface(id, surface);
    // Nothing about the session: one line, so a run says which look it has.
    eprintln!("ec-lock: eclipse_glass_v1, compositor capability: {on}");
}
