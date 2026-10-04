// SPDX-License-Identifier: AGPL-3.0-only
//! The X11 window manager (COMP-07 §1–§3) and the selection bridge (§3).
//!
//! X11 clients are a single untrusted domain: X gives every client on the
//! display the ability to see and poke every other. abyss does not widen
//! that hole outward — an X client gets exactly what one focused, untrusted
//! Wayland client would get, and nothing that depends on identity we cannot
//! verify (ADR 0026).

use std::os::unix::io::OwnedFd;

use smithay::{
    desktop::Window,
    utils::{Logical, Rectangle, Size},
    wayland::selection::{
        data_device::{
            clear_data_device_selection, current_data_device_selection_userdata,
            request_data_device_client_selection, set_data_device_selection,
        },
        primary_selection::{
            clear_primary_selection, current_primary_selection_userdata, request_primary_client_selection,
            set_primary_selection,
        },
        SelectionTarget,
    },
    xwayland::{
        xwm::{Reorder, ResizeEdge, WmWindowProperty, X11Window, XwmId},
        X11Surface, X11Wm, XwmHandler,
    },
};

use crate::{
    shell,
    state::AbyssState,
    xwayland::security::{self, X11Class},
};

/// Sensitivity/trust class of every X11 window. Constant by construction:
/// X gives us no way to authenticate a per-window claim (COMP-07 §2).
fn class_of(surface: &X11Surface) -> X11Class {
    security::classify(
        surface.class().as_str(),
        // X11 has no trustworthy channel for a window to declare its own
        // sensitivity, so nothing is ever requested and nothing is granted.
        None,
        None,
    )
}

/// Is the focused window an X11 one? The clipboard bridge is gated on this.
fn x11_has_focus(state: &AbyssState) -> bool {
    state.focus.as_ref().is_some_and(|w| w.x11_surface().is_some())
}

fn window_for(state: &AbyssState, surface: &X11Surface) -> Option<Window> {
    // Not `space`: see `shell::owned_windows`.
    shell::owned_windows(state)
        .into_iter()
        .find(|w| w.x11_surface() == Some(surface))
}

/// `base` with whichever of x/y/w/h the client asked to change.
fn requested_rect(
    base: Rectangle<i32, Logical>,
    x: Option<i32>,
    y: Option<i32>,
    w: Option<u32>,
    h: Option<u32>,
) -> Rectangle<i32, Logical> {
    Rectangle::new(
        (x.unwrap_or(base.loc.x), y.unwrap_or(base.loc.y)).into(),
        Size::from((
            w.map(|v| v as i32).unwrap_or(base.size.w).max(1),
            h.map(|v| v as i32).unwrap_or(base.size.h).max(1),
        )),
    )
}

impl XwmHandler for AbyssState {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        self.xwayland
            .wm
            .as_mut()
            .expect("xwm callbacks only fire while the wm exists")
    }

    fn new_window(&mut self, _xwm: XwmId, window: X11Surface) {
        let class = class_of(&window);
        tracing::debug!(
            class = window.class(),
            sensitivity = ?class.sensitivity,
            "x11 window created"
        );
    }

    fn new_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        tracing::debug!(class = window.class(), "x11 override-redirect window created");
    }

    /// A managed window wants to be on screen: honour it, then place it
    /// through the same shell path every Wayland toplevel takes, so layout,
    /// workspaces and rules apply identically (COMP-07 §1).
    fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Err(e) = window.set_mapped(true) {
            tracing::warn!(error = %e, "could not map x11 window");
            return;
        }
        let element = Window::new_x11_window(window);
        shell::place_new_window(self, element);
    }

    /// Override-redirect: menus, tooltips, DnD icons. The client owns their
    /// geometry outright; the compositor only stacks and draws them.
    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        let geo = window.geometry();
        let element = Window::new_x11_window(window.clone());
        self.xwayland.unmanaged.push(window);
        self.space.map_element(element.clone(), geo.loc, true);
        self.space.raise_element(&element, true);
    }

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(element) = window_for(self, &window) {
            if !window.is_override_redirect() {
                shell::anim::close(self, &element);
            }
            shell::unmap_window(self, &element);
        }
        self.xwayland.unmanaged.retain(|s| *s != window);
        if !window.is_override_redirect() {
            let _ = window.set_mapped(false);
        }
    }

    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(element) = window_for(self, &window) {
            if !window.is_override_redirect() {
                shell::anim::close(self, &element);
            }
            shell::unmap_window(self, &element);
        }
        self.xwayland.unmanaged.retain(|s| *s != window);
    }

    /// Geometry requests. Tiled windows are held to the layout, so the
    /// compositor re-asserts it; floating windows get what they asked for
    /// (through the shell's float rect, so the layout keeps it); unmanaged
    /// windows get what they asked for outright.
    fn configure_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        let current = window.geometry();
        let element = window_for(self, &window);
        let rect = match &element {
            Some(element) => {
                let held = self.space.element_geometry(element).unwrap_or(current);
                if !shell::is_floating(self, element) {
                    held
                } else {
                    let want = requested_rect(held, x, y, w, h);
                    if want != held {
                        shell::place_at(self, element, want);
                    }
                    // `place_at` configures through the layout; a maximized or
                    // fullscreen window is not pinned, so re-assert what it has.
                    self.space.element_geometry(element).unwrap_or(want)
                }
            }
            None => requested_rect(current, x, y, w, h),
        };
        let _ = window.configure(rect);
    }

    fn maximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(w) = window_for(self, &window) {
            shell::set_x11_state(self, &w, false, true);
        }
    }

    fn unmaximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(w) = window_for(self, &window) {
            shell::set_x11_state(self, &w, false, false);
        }
    }

    fn fullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(w) = window_for(self, &window) {
            shell::set_x11_state(self, &w, true, true);
        }
    }

    fn unfullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(w) = window_for(self, &window) {
            shell::set_x11_state(self, &w, true, false);
        }
    }

    fn minimize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(w) = window_for(self, &window) {
            shell::set_minimized(self, &w, true);
        }
    }

    fn unminimize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(w) = window_for(self, &window) {
            shell::set_minimized(self, &w, false);
        }
    }

    /// Title/class/type/transient hints changed after the window was created:
    /// tell the bar and re-run the rules that wait on a name (COMP-05 §4).
    fn property_notify(&mut self, _xwm: XwmId, window: X11Surface, property: WmWindowProperty) {
        if !matches!(
            property,
            WmWindowProperty::Title
                | WmWindowProperty::Class
                | WmWindowProperty::TransientFor
                | WmWindowProperty::WindowType
        ) {
            return;
        }
        shell::x11_properties_changed(self, &window);
    }

    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        geometry: Rectangle<i32, Logical>,
        _above: Option<X11Window>,
    ) {
        if !window.is_override_redirect() {
            return;
        }
        let Some(element) = window_for(self, &window) else {
            return;
        };
        self.space.map_element(element.clone(), geometry.loc, true);
        self.space.raise_element(&element, true);
    }

    /// Interactive move/resize is compositor policy, not client policy: a
    /// tiled window cannot talk its way out of the layout (COMP-07 §1).
    fn resize_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32, _edge: ResizeEdge) {
        tracing::debug!(class = window.class(), "ignoring x11 interactive resize request");
    }

    fn move_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32) {
        tracing::debug!(class = window.class(), "ignoring x11 interactive move request");
    }

    // ------------------------------------------------------------ selection

    /// The capability check for the clipboard bridge (COMP-07 §3, ADR 0026).
    ///
    /// X has no per-client selection access control: granting the X server
    /// access grants every X client access. So the grant is scoped in time
    /// instead of by identity — the human's focus is the capability. A
    /// background X client cannot read the human's clipboard.
    fn allow_selection_access(&mut self, _xwm: XwmId, _selection: SelectionTarget) -> bool {
        let allowed = x11_has_focus(self);
        if !allowed {
            tracing::debug!("denied x11 selection access: no x11 window holds focus");
        }
        allowed
    }

    /// X11 wants to read the Wayland-owned selection. Only reachable once
    /// `allow_selection_access` returned true.
    fn send_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_type: String, fd: OwnedFd) {
        let seat = self.seat.clone();
        let res = match selection {
            SelectionTarget::Clipboard => {
                request_data_device_client_selection(&seat, mime_type, fd).map_err(|e| e.to_string())
            }
            SelectionTarget::Primary => {
                request_primary_client_selection(&seat, mime_type, fd).map_err(|e| e.to_string())
            }
        };
        if let Err(e) = res {
            tracing::warn!(error = e, ?selection, "could not forward a selection to xwayland");
        }
    }

    /// X11 took ownership of a selection. Advertise it to Wayland clients,
    /// recording the source as X11 so provenance stays honest (COMP-06 §4).
    fn new_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        let seat = self.seat.clone();
        match selection {
            SelectionTarget::Clipboard => {
                set_data_device_selection(&self.display_handle, &seat, mime_types.clone(), ());
                self.clipboard = Some(crate::protocols::standard::data_device::ClipboardSource {
                    surface: None,
                    app_id: Some("xwayland".to_string()),
                    mime_types,
                });
            }
            SelectionTarget::Primary => {
                set_primary_selection(&self.display_handle, &seat, mime_types, ());
            }
        }
    }

    fn cleared_selection(&mut self, _xwm: XwmId, selection: SelectionTarget) {
        let seat = self.seat.clone();
        match selection {
            SelectionTarget::Clipboard => {
                if current_data_device_selection_userdata(&seat).is_some() {
                    clear_data_device_selection(&self.display_handle, &seat);
                    self.clipboard = None;
                }
            }
            SelectionTarget::Primary => {
                if current_primary_selection_userdata(&seat).is_some() {
                    clear_primary_selection(&self.display_handle, &seat);
                }
            }
        }
    }

    fn disconnected(&mut self, _xwm: XwmId) {
        tracing::info!("xwayland disconnected; tearing down the X11 domain");
        let stale: Vec<Window> = shell::owned_windows(self)
            .into_iter()
            .filter(|w| w.x11_surface().is_some())
            .collect();
        for w in stale {
            shell::unmap_window(self, &w);
        }
        self.xwayland.unmanaged.clear();
        self.xwayland.wm = None;
        self.xwayland.advertised_scale = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), Size::from((w, h)))
    }

    #[test]
    fn configure_request_merges_only_what_was_asked() {
        let base = r(10, 20, 300, 200);
        assert_eq!(requested_rect(base, None, None, None, None), base);
        assert_eq!(
            requested_rect(base, Some(5), None, Some(640), None),
            r(5, 20, 640, 200)
        );
        assert_eq!(
            requested_rect(base, None, Some(-4), None, Some(0)),
            r(10, -4, 300, 1)
        );
    }
}
