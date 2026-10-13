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
        xwm::{Reorder, ResizeEdge, WmWindowProperty, WmWindowType, X11Window, XwmId},
        X11Surface, X11Wm, XwmHandler,
    },
};

use crate::{
    shell,
    state::AbyssState,
    xwayland::{
        self,
        security::{self, X11Class},
    },
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

/// KWin's "legacy fullscreen": an undecorated, non-transient window that sits
/// at its output's origin and covers the whole output is fullscreen, whatever
/// it said. The origin matters: a large CSD window (Steam) is placed elsewhere.
///
/// Wine/Proton and SDL ask for fullscreen *before* mapping by writing
/// `_NET_WM_STATE` straight onto the withdrawn window (EWMH §5.1 leaves no
/// other way). Smithay 0.7 never reads that property, so the request is
/// otherwise invisible: the game maps as a screen-sized window, gets tiled,
/// and fights the tile for its size.
///
/// A window whose size hints leave it resizable (see [`hints_resizable`]) is
/// never legacy-fullscreen: Steam/CEF maps undecorated at the origin with
/// its saved, screen-sized geometry and a min-size hint only, and is an
/// ordinary window. Games that mean it (SDL, Wine/Proton) pin their size with
/// min == max, or carry no size hints at all.
fn legacy_fullscreen(
    state: &AbyssState,
    x11: &X11Surface,
    element: &Window,
    rect: Rectangle<i32, Logical>,
) -> bool {
    let output = output_rect(state, element);
    let covers = covers_output(
        x11.is_override_redirect() || x11.is_transient_for().is_some(),
        x11.is_decorated(),
        rect,
        output,
    );
    if covers && hints_resizable(x11.min_size(), x11.max_size()) {
        tracing::debug!(
            class = x11.class(),
            "screen-sized undecorated x11 window has resizable size hints; not legacy fullscreen"
        );
        return false;
    }
    covers
}

/// Do `WM_NORMAL_HINTS` leave the window resizable? Only a window that pins
/// its size (`min == max`) or says nothing is a fixed-mode game; one with a
/// min (or a max) that does not pin it is a normal application window.
fn hints_resizable(min: Option<Size<i32, Logical>>, max: Option<Size<i32, Logical>>) -> bool {
    match (min, max) {
        (None, None) => false,
        (Some(min), Some(max)) => min != max,
        _ => true,
    }
}

/// The geometry of the output `element` is on, else of the focused one.
fn output_rect(state: &AbyssState, element: &Window) -> Option<Rectangle<i32, Logical>> {
    shell::output_of_window(state, element)
        .or_else(|| state.outputs.focused().map(|e| e.id))
        .and_then(|id| state.outputs.get(id))
        .and_then(|e| state.space.output_geometry(&e.output))
}

/// The rule behind [`legacy_fullscreen`], free of X. `client_decorated` is
/// smithay's `is_decorated`: Motif hints asked the WM for no decorations.
fn covers_output(
    secondary: bool,
    client_decorated: bool,
    rect: Rectangle<i32, Logical>,
    output: Option<Rectangle<i32, Logical>>,
) -> bool {
    !secondary
        && client_decorated
        && output.is_some_and(|o| rect.loc == o.loc && rect.size.w >= o.size.w && rect.size.h >= o.size.h)
}

/// A configure request can only turn a tiled window into a legacy-fullscreen
/// one if the client moved it. A resize-only request has x/y defaulted to the
/// held tile, which sits at the output origin when it is the only tile.
fn configure_may_fullscreen(x: Option<i32>, y: Option<i32>) -> bool {
    x.is_some() || y.is_some()
}

/// `_NET_WM_WINDOW_TYPE`s whose windows position themselves like
/// override-redirect ones: menus, tooltips, notifications.
fn popup_type(ty: Option<WmWindowType>) -> bool {
    matches!(
        ty,
        Some(
            WmWindowType::DropdownMenu
                | WmWindowType::PopupMenu
                | WmWindowType::Menu
                | WmWindowType::Tooltip
                | WmWindowType::Notification
        )
    )
}

fn is_popup_type(surface: &X11Surface) -> bool {
    popup_type(surface.window_type())
}

/// Remove `surface` from a parked-request list; true if it was there.
fn take(list: &mut Vec<X11Surface>, surface: &X11Surface) -> bool {
    let before = list.len();
    list.retain(|s| s != surface);
    list.len() != before
}

/// Park a state request from a window that is not mapped yet (`on`), or drop
/// a parked one (`!on`). Override-redirect windows have no such state.
fn park(list: &mut Vec<X11Surface>, surface: &X11Surface, on: bool) {
    take(list, surface);
    if on && !surface.is_override_redirect() {
        list.push(surface.clone());
    }
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
        self.xwayland.unrefreshed.push(window);
    }

    /// A managed window wants to be on screen: honour it, then place it
    /// through the same shell path every Wayland toplevel takes, so layout,
    /// workspaces and rules apply identically (COMP-07 §1). A fullscreen
    /// asked for before the map is applied once the window is placed.
    #[allow(clippy::mutable_key_type)]
    fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Err(e) = window.set_mapped(true) {
            tracing::warn!(error = %e, "could not map x11 window");
            return;
        }
        // At a fractional client scale smithay has not converted this yet
        // (see `creation_geometry`). Taken raw, an ordinary Steam window read
        // as screen-sized.
        let requested = xwayland::creation_geometry(window.geometry(), xwayland::client_scale(self));
        let element = Window::new_x11_window(window.clone());
        // Menus and tooltips place themselves: map them where they asked, like
        // override-redirect windows, outside rules, layout and focus.
        if is_popup_type(&window) {
            // Brings smithay's stored geometry in line with `requested`.
            let _ = window.configure(requested);
            self.xwayland.unmanaged.push(window);
            self.space.map_element(element.clone(), requested.loc, true);
            self.space.raise_element(&element, true);
            return;
        }
        // Decide the state before placing, so the first configure the client
        // sees is the final rect, not a tile it then stretches its swapchain to.
        // No maximized state: clear one the client set before the map.
        if window.is_maximized() {
            let _ = window.set_maximized(false);
        }
        let fullscreen = take(&mut self.xwayland.wants_fullscreen, &window)
            || window.is_fullscreen()
            || legacy_fullscreen(self, &window, &element, requested);
        tracing::info!(
            class = window.class(),
            ?requested,
            output = ?output_rect(self, &element),
            fullscreen,
            decorated = window.is_decorated(),
            transient = window.is_transient_for().is_some(),
            "x11 window map decision"
        );
        if fullscreen {
            self.xwayland.mapping = Some(element.clone());
        }
        shell::place_new_window(self, element.clone());
        self.xwayland.mapping = None;
        if fullscreen {
            tracing::info!(class = window.class(), "x11 window mapped fullscreen");
            shell::set_x11_fullscreen(self, &element, true);
        }
        if fullscreen && !self.fullscreen.contains_key(&element) {
            // The state did not stick; the placement configure was withheld.
            shell::arrange(self);
        }
    }

    /// Override-redirect: menus, tooltips, DnD icons. The client owns their
    /// geometry outright; the compositor only stacks and draws them.
    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        let mut geo = window.geometry();
        if self.xwayland.unrefreshed.contains(&window) {
            geo = xwayland::creation_geometry(geo, xwayland::client_scale(self));
        }
        let element = Window::new_x11_window(window.clone());
        self.xwayland.unmanaged.push(window);
        self.space.map_element(element.clone(), geo.loc, true);
        self.space.raise_element(&element, true);
    }

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(element) = window_for(self, &window) {
            if !shell::is_unmanaged_x11(self, &element) {
                shell::anim::close(self, &element);
            }
            shell::unmap_window(self, &element);
        }
        self.xwayland.unmanaged.retain(|s| *s != window);
        self.xwayland.unrefreshed.retain(|s| *s != window);
        self.xwayland.wants_fullscreen.retain(|s| *s != window);
        if !window.is_override_redirect() {
            let _ = window.set_mapped(false);
        }
    }

    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(element) = window_for(self, &window) {
            if !shell::is_unmanaged_x11(self, &element) {
                shell::anim::close(self, &element);
            }
            shell::unmap_window(self, &element);
        }
        self.xwayland.unmanaged.retain(|s| *s != window);
        self.xwayland.unrefreshed.retain(|s| *s != window);
        self.xwayland.wants_fullscreen.retain(|s| *s != window);
    }

    /// Geometry requests. Tiled windows are held to the layout, so the
    /// compositor re-asserts it, unless the request makes the window a
    /// [`legacy_fullscreen`] one (a game switching to a fullscreen
    /// resolution), which is honoured as fullscreen; floating windows get what they asked for
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
            // A popup-type window mapped outside the layout: it positions itself.
            Some(element) if self.xwayland.unmanaged.contains(&window) => {
                let want = requested_rect(current, x, y, w, h);
                self.space.map_element(element.clone(), want.loc, true);
                self.space.raise_element(element, true);
                want
            }
            Some(element) => {
                let held = self.space.element_geometry(element).unwrap_or(current);
                if !shell::is_floating(self, element) {
                    if configure_may_fullscreen(x, y)
                        && legacy_fullscreen(self, &window, element, requested_rect(held, x, y, w, h))
                    {
                        shell::set_x11_fullscreen(self, element, true);
                        self.space.element_geometry(element).unwrap_or(held)
                    } else {
                        held
                    }
                } else {
                    let want = requested_rect(held, x, y, w, h);
                    if want != held {
                        shell::place_at(self, element, want);
                    }
                    // `place_at` configures through the layout; a fullscreen
                    // window is not pinned, so re-assert what it has.
                    self.space.element_geometry(element).unwrap_or(want)
                }
            }
            None => requested_rect(current, x, y, w, h),
        };
        let _ = window.configure(rect);
    }

    /// Abyss has no maximized state: decline, and clear the property so the
    /// client's `_NET_WM_STATE` stays truthful.
    fn maximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        tracing::debug!(class = window.class(), "x11 maximize request declined");
        let _ = window.set_maximized(false);
    }

    fn unmaximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        tracing::debug!(class = window.class(), "x11 unmaximize request declined");
        let _ = window.set_maximized(false);
    }

    fn fullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        tracing::info!(
            class = window.class(),
            mapped = window.is_mapped(),
            "x11 fullscreen request"
        );
        match window_for(self, &window) {
            Some(w) => shell::set_x11_fullscreen(self, &w, true),
            None if !window.is_mapped() => park(&mut self.xwayland.wants_fullscreen, &window, true),
            None => {}
        }
    }

    fn unfullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        tracing::info!(
            class = window.class(),
            mapped = window.is_mapped(),
            "x11 unfullscreen request"
        );
        match window_for(self, &window) {
            // Wine/Proton drops fullscreen when it loses focus, which a
            // workspace switch causes. A fullscreen window on an unviewed
            // workspace stays fullscreen there; the client is told so.
            Some(w) if self.fullscreen.contains_key(&w) && !self.space.elements().any(|e| e == &w) => {
                let _ = window.set_fullscreen(true);
            }
            Some(w) => shell::set_x11_fullscreen(self, &w, false),
            None if !window.is_mapped() => park(&mut self.xwayland.wants_fullscreen, &window, false),
            None => {}
        }
    }

    fn minimize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        tracing::info!(
            class = window.class(),
            mapped = window.is_mapped(),
            "x11 minimize request"
        );
        if let Some(w) = window_for(self, &window) {
            shell::set_minimized(self, &w, true);
        }
    }

    fn unminimize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        tracing::info!(
            class = window.class(),
            mapped = window.is_mapped(),
            "x11 unminimize request"
        );
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
        self.xwayland.unrefreshed.retain(|s| *s != window);
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
        self.xwayland.unrefreshed.clear();
        self.xwayland.wm = None;
        self.xwayland.advertised_scale = None;
        self.xwayland.client_scale = None;
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

    #[test]
    fn an_undecorated_screen_sized_window_is_legacy_fullscreen() {
        let out = Some(r(0, 0, 1920, 1280));
        // A Proton game at the output's size, or larger (a saved 2880x1920
        // on a 1.5x panel), with Motif "no decorations".
        assert!(covers_output(false, true, r(0, 0, 1920, 1280), out));
        assert!(covers_output(false, true, r(0, 0, 2880, 1920), out));
        // Large but not at the origin: a CSD app window (Steam), not a game.
        assert!(!covers_output(false, true, r(40, 40, 1920, 1280), out));
        assert!(!covers_output(false, true, r(0, 40, 2000, 1300), out));
        // Smaller in either dimension: an ordinary window.
        assert!(!covers_output(false, true, r(0, 0, 1920, 1279), out));
        assert!(!covers_output(false, true, r(0, 0, 1280, 720), out));
        // A decorated window that happens to be screen-sized stays a window.
        assert!(!covers_output(false, false, r(0, 0, 1920, 1280), out));
        // Dialogs and popups never qualify, nor anything with no output.
        assert!(!covers_output(true, true, r(0, 0, 1920, 1280), out));
        assert!(!covers_output(false, true, r(0, 0, 1920, 1280), None));
    }

    #[test]
    fn only_pinned_or_unhinted_windows_may_be_legacy_fullscreen() {
        let sz = |w, h| Some(Size::<i32, Logical>::from((w, h)));
        // SDL / Wine: fixed size, or no hints.
        assert!(!hints_resizable(None, None));
        assert!(!hints_resizable(sz(1515, 1010), sz(1515, 1010)));
        // Steam/CEF: a min size, nothing pinning it.
        assert!(hints_resizable(sz(1000, 600), None));
        assert!(hints_resizable(sz(400, 300), sz(4000, 3000)));
        assert!(hints_resizable(None, sz(4000, 3000)));
    }

    #[test]
    fn a_resize_only_request_never_makes_legacy_fullscreen() {
        // Steam resizing itself while its tile sits at the output origin.
        assert!(!configure_may_fullscreen(None, None));
        assert!(configure_may_fullscreen(Some(0), None));
        assert!(configure_may_fullscreen(None, Some(0)));
        assert!(configure_may_fullscreen(Some(0), Some(0)));
    }

    #[test]
    fn menu_like_window_types_are_popups() {
        for ty in [
            WmWindowType::DropdownMenu,
            WmWindowType::PopupMenu,
            WmWindowType::Menu,
            WmWindowType::Tooltip,
            WmWindowType::Notification,
        ] {
            assert!(popup_type(Some(ty)), "{ty:?}");
        }
        assert!(!popup_type(Some(WmWindowType::Normal)));
        assert!(!popup_type(Some(WmWindowType::Dialog)));
        assert!(!popup_type(Some(WmWindowType::Splash)));
        assert!(!popup_type(None));
    }

    #[test]
    fn x11_rect_round_trips_at_every_scale() {
        // Identity at 1.0.
        let x = r(30, 50, 1280, 720);
        assert_eq!(xwayland::from_x11(x, 1.0), x);
        assert_eq!(xwayland::to_x11(x, 1.0), x);
        for scale in [1.0, 1.9, 2.0] {
            // Logical -> X -> logical is lossless (X has more pixels).
            let l = r(10, 20, 800, 600);
            assert_eq!(
                xwayland::from_x11(xwayland::to_x11(l, scale), scale),
                l,
                "{scale}"
            );
        }
        assert_eq!(
            xwayland::from_x11(r(190, 95, 1900, 950), 1.9),
            r(100, 50, 1000, 500)
        );
        assert_eq!(xwayland::from_x11(r(0, 0, 1, 1), 2.0), r(0, 0, 1, 1));
    }

    #[test]
    fn creation_geometry_redoes_smithays_truncated_division() {
        // 1.9 truncates to 1: smithay left X pixels untouched.
        assert_eq!(
            xwayland::creation_geometry(r(190, 95, 1900, 950), 1.9),
            r(100, 50, 1000, 500)
        );
        // 2.5 truncates to 2: smithay halved.
        assert_eq!(
            xwayland::creation_geometry(r(100, 50, 500, 250), 2.5),
            r(80, 40, 400, 200)
        );
        // Integral scales were already right.
        let g = r(10, 20, 300, 200);
        assert_eq!(xwayland::creation_geometry(g, 2.0), g);
        assert_eq!(xwayland::creation_geometry(g, 1.0), g);
    }
}
