// SPDX-License-Identifier: AGPL-3.0-only
//! `wl_data_device`: human clipboard and drag-and-drop (COMP-06 §4).
//!
//! The compositor tracks *who* set the current clipboard — the source
//! toplevel's surface and its `app_id` — so that a later `clipboard.read`
//! policy check can classify the data. Clipboard **contents are never read,
//! logged or stored** here; only the mime type names and the source identity.

use std::os::unix::io::OwnedFd;

use smithay::{
    delegate_data_device,
    input::Seat,
    reexports::wayland_server::protocol::{wl_data_source::WlDataSource, wl_surface::WlSurface},
    wayland::{
        compositor::with_states,
        selection::{
            data_device::{ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler},
            SelectionHandler, SelectionSource, SelectionTarget,
        },
        shell::xdg::XdgToplevelSurfaceData,
    },
};

use crate::state::HeliosState;

/// Provenance of the current selection (COMP-06 §4).
///
/// Never holds clipboard data — only the identity of the source and the mime
/// types it advertised.
#[derive(Debug, Clone)]
#[allow(dead_code)] // read by the clipboard policy check (COMP-11, Phase 2)
pub struct ClipboardSource {
    /// The toplevel surface that owned keyboard focus when the selection was
    /// set. Acts as the source window handle until COMP-05 identity lands.
    pub surface: Option<WlSurface>,
    /// `xdg_toplevel.app_id` of that surface (the X11 "class" equivalent).
    pub app_id: Option<String>,
    /// Advertised mime types. Names only, never contents.
    pub mime_types: Vec<String>,
}

/// `app_id` of a toplevel surface, if it has one.
pub fn app_id_of(surface: &WlSurface) -> Option<String> {
    with_states(surface, |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|d| d.lock().ok().and_then(|d| d.app_id.clone()))
    })
}

impl HeliosState {
    /// Record who set the clipboard. Called from [`SelectionHandler`].
    fn record_selection(&mut self, source: Option<&SelectionSource>) {
        let Some(source) = source else {
            self.clipboard = None;
            return;
        };
        let surface = self
            .seat
            .get_keyboard()
            .and_then(|k| k.current_focus())
            .or_else(|| {
                self.focus
                    .as_ref()
                    .and_then(|w| w.toplevel().map(|t| t.wl_surface().clone()))
            });
        let app_id = surface.as_ref().and_then(app_id_of);
        let mime_types = source.mime_types();
        tracing::debug!(
            app_id = app_id.as_deref().unwrap_or("<unknown>"),
            mime_count = mime_types.len(),
            "clipboard selection set"
        );
        self.clipboard = Some(ClipboardSource {
            surface,
            app_id,
            mime_types,
        });
    }
}

impl SelectionHandler for HeliosState {
    type SelectionUserData = ();

    fn new_selection(&mut self, ty: SelectionTarget, source: Option<SelectionSource>, _seat: Seat<Self>) {
        if ty == SelectionTarget::Clipboard {
            self.record_selection(source.as_ref());
        }
        // Mirror the offer into X11 so its clients see the same clipboard
        // (COMP-07 §3). Names only; no contents cross here.
        if let Some(wm) = self.xwayland.wm.as_mut() {
            let mimes = source.as_ref().map(|s| s.mime_types());
            if let Err(e) = wm.new_selection(ty, mimes) {
                tracing::warn!(error = %e, "could not advertise the selection to xwayland");
            }
        }
    }

    /// A Wayland client is reading a selection owned by X11.
    fn send_selection(
        &mut self,
        ty: SelectionTarget,
        mime_type: String,
        fd: std::os::unix::io::OwnedFd,
        _seat: Seat<Self>,
        _user_data: &Self::SelectionUserData,
    ) {
        let handle = self.loop_handle.clone();
        let Some(wm) = self.xwayland.wm.as_mut() else {
            return;
        };
        if let Err(e) = wm.send_selection(ty, mime_type, fd, handle) {
            tracing::warn!(error = %e, "could not read a selection from xwayland");
        }
    }
}

impl DataDeviceHandler for HeliosState {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl ClientDndGrabHandler for HeliosState {
    fn started(&mut self, _source: Option<WlDataSource>, icon: Option<WlSurface>, _seat: Seat<Self>) {
        self.dnd_icon = icon;
    }

    fn dropped(&mut self, _target: Option<WlSurface>, _validated: bool, _seat: Seat<Self>) {
        self.dnd_icon = None;
    }
}

impl ServerDndGrabHandler for HeliosState {
    fn send(&mut self, _mime_type: String, _fd: OwnedFd, _seat: Seat<Self>) {
        // No compositor-originated drags yet (trusted UI, COMP-10).
    }
}

delegate_data_device!(HeliosState);
