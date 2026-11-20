#![doc = include_str!("../README.md")]
// eclipse-patch: vendored, not held to our -D warnings gate (PATCHES.md hunk 3)
#![allow(warnings, clippy::all)]
pub mod actions;
pub mod build_pattern;
pub mod multi_window;
pub mod settings;

// eclipse-patch: surface-created hook (PATCHES.md hunk 4)
/// Called once for every lock surface's iced window, right after the surface
/// exists and before its first frame is drawn, with the window's id and its
/// `wl_surface`. Runs on the event-loop thread.
pub type SurfaceHook = fn(iced_core::window::Id, &sessionlockev::reexport::wayland_client::WlSurface);

// eclipse-patch: (PATCHES.md hunk 4)
static SURFACE_HOOK: std::sync::OnceLock<SurfaceHook> = std::sync::OnceLock::new();

// eclipse-patch: (PATCHES.md hunk 4)
/// Install the [`SurfaceHook`]. Call before running the application; only the
/// first call takes effect.
pub fn set_surface_hook(hook: SurfaceHook) {
    let _ = SURFACE_HOOK.set(hook);
}

// eclipse-patch: (PATCHES.md hunk 4)
pub(crate) fn surface_created(id: iced_core::window::Id, surface: &sessionlockev::reexport::wayland_client::WlSurface) {
    if let Some(hook) = SURFACE_HOOK.get() {
        hook(id, surface);
    }
}

mod clipboard;
mod conversion;
mod error;
mod event;
mod proxy;
mod user_interface;

pub use iced_sessionlock_macros::to_session_message;

pub use error::Error;

use iced_core::theme::Base as DefaultStyle;
use iced_core::theme::Style as Appearance;

/// Opt-out for clipboard initialization. Call this before starting the
/// runtime when your app has no text input and doesn't need paste/copy —
/// this skips spawning the always-on smithay-clipboard worker thread.
pub fn disable_clipboard() {
    clipboard::set_disabled();
}

pub type Result = std::result::Result<(), error::Error>;

pub use build_pattern::application;
pub use settings::Settings;
