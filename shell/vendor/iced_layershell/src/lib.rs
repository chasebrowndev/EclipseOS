#![doc = include_str!("../README.md")]
// eclipse-patch: vendored, not held to our -D warnings gate (PATCHES.md hunk 3)
#![allow(warnings, clippy::all)]
pub mod actions;
pub mod build_pattern;
mod clipboard;
mod conversion;
mod error;
mod event;
mod multi_window;
mod proxy;
mod user_interface;

pub mod settings;

// eclipse-patch: surface-created hook (PATCHES.md hunk 2)
/// Called once for every layer surface's iced window, right after the
/// surface exists and before its first frame is drawn, with the window's id
/// and its `wl_surface`. Runs on the event-loop thread.
pub type SurfaceHook = fn(iced_core::window::Id, &layershellev::reexport::wayland_client::WlSurface);

// eclipse-patch: (PATCHES.md hunk 2)
static SURFACE_HOOK: std::sync::OnceLock<SurfaceHook> = std::sync::OnceLock::new();

// eclipse-patch: (PATCHES.md hunk 2)
/// Install the [`SurfaceHook`]. Call before running the application; only
/// the first call takes effect.
pub fn set_surface_hook(hook: SurfaceHook) {
    let _ = SURFACE_HOOK.set(hook);
}

// eclipse-patch: (PATCHES.md hunk 2)
pub(crate) fn surface_created(
    id: iced_core::window::Id,
    surface: &layershellev::reexport::wayland_client::WlSurface,
) {
    if let Some(hook) = SURFACE_HOOK.get() {
        hook(id, surface);
    }
}

pub mod reexport {
    pub use layershellev::NewInputPanelSettings;
    pub use layershellev::NewLayerShellSettings;
    pub use layershellev::OutputOption;
    pub use layershellev::PopupPlacement;
    pub use layershellev::WithConnection;
    pub use layershellev::reexport::Anchor;
    pub use layershellev::reexport::KeyboardInteractivity;
    pub use layershellev::reexport::Layer;
    pub use layershellev::reexport::wayland_client::{WlRegion, wl_keyboard};
    pub use layershellev::reexport::xdg_positioner::{
        Anchor as PopupAnchor, ConstraintAdjustment as PopupConstraintAdjustment,
        Gravity as PopupGravity,
    };
    pub mod core {
        pub use iced_core::*;
    }
    pub use iced_core::window::Id as IcedId;
    pub use iced_runtime::Task;
}

mod ime_preedit;

pub use iced_layershell_macros::to_layer_message;

pub use error::Error;

/// Opt-out for clipboard initialization. Call this before starting the
/// runtime when your app has no text input and doesn't need paste/copy —
/// this skips spawning the always-on smithay-clipboard worker thread.
pub fn disable_clipboard() {
    clipboard::set_disabled();
}

pub type Result = std::result::Result<(), error::Error>;
use iced_core::theme::Style as Appearance;

use iced_core::theme::Base as DefaultStyle;

// layershell application
pub use build_pattern::application;
pub use build_pattern::daemon;

pub use settings::Settings;
