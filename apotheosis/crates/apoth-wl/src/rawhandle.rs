// SPDX-License-Identifier: AGPL-3.0-only
//! `raw-window-handle` 0.6 handles for the Vulkan WSI (T-09 §7). The pointers come from the
//! system libwayland backend; wgpu attaches buffers to this surface, we never do.
//!
//! Drop order (INTEGRATION-61): swapchain first, then the roles
//! ([`crate::WlHandle::destroy_window`] destroys toplevel and `xdg_surface`), then the
//! `wl_surface`. This handle only holds proxy clones: dropping it destroys nothing.

use std::ffi::c_void;
use std::ptr::NonNull;

use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawDisplayHandle,
    RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle, WindowHandle,
};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Proxy};

/// Display and surface pointers for one window; `Send + Sync` so it can move to the render thread.
#[derive(Clone, Debug)]
pub struct WlSurfaceHandle {
    conn: Connection,
    surface: WlSurface,
}

impl WlSurfaceHandle {
    pub(crate) fn new(conn: Connection, surface: WlSurface) -> WlSurfaceHandle {
        WlSurfaceHandle { conn, surface }
    }

    /// The `wl_display*` of the connection.
    pub fn display_ptr(&self) -> Option<NonNull<c_void>> {
        NonNull::new(self.conn.backend().display_ptr().cast::<c_void>())
    }

    /// The `wl_surface*` of the window.
    pub fn surface_ptr(&self) -> Option<NonNull<c_void>> {
        NonNull::new(self.surface.id().as_ptr().cast::<c_void>())
    }
}

impl HasDisplayHandle for WlSurfaceHandle {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        let ptr = self.display_ptr().ok_or(HandleError::Unavailable)?;
        let raw = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(ptr));
        #[allow(unsafe_code)]
        // SAFETY: `ptr` is the live `wl_display*` of `self.conn`, which this handle keeps
        // alive (a `Connection` clone holds the backend). The pointer does not change for
        // the backend's lifetime, and the returned borrow cannot outlive `&self`.
        Ok(unsafe { DisplayHandle::borrow_raw(raw) })
    }
}

impl HasWindowHandle for WlSurfaceHandle {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let ptr = self.surface_ptr().ok_or(HandleError::Unavailable)?;
        let raw = RawWindowHandle::Wayland(WaylandWindowHandle::new(ptr));
        #[allow(unsafe_code)]
        // SAFETY: `ptr` is the `wl_proxy*` of `self.surface`, valid while the proxy is
        // alive; this handle holds a clone, and the window is destroyed only through
        // `WlHandle::destroy_window` after the renderer has dropped its swapchain.
        Ok(unsafe { WindowHandle::borrow_raw(raw) })
    }
}
