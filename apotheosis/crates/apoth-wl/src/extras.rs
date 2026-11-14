// SPDX-License-Identifier: AGPL-3.0-only
//! The optional globals of T-09 §3 that carry no event-heavy state: xdg-activation,
//! xdg-system-bell, xdg-toplevel-icon, xdg-toplevel-tag, content-type, idle-inhibit and
//! input-timestamps. Each is bound when advertised (also when it appears late), exposed
//! through a small request API on [`crate::WlHandle`], and a no-op when absent.
#![allow(unreachable_pub)] // `pub` fields of crate-private structs

use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::wp::content_type::v1::client::wp_content_type_manager_v1::WpContentTypeManagerV1;
use wayland_protocols::wp::content_type::v1::client::wp_content_type_v1::{
    Type as WireContentType, WpContentTypeV1,
};
use wayland_protocols::wp::idle_inhibit::zv1::client::zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1;
use wayland_protocols::wp::idle_inhibit::zv1::client::zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1;
use wayland_protocols::wp::input_timestamps::zv1::client::zwp_input_timestamps_manager_v1::ZwpInputTimestampsManagerV1;
use wayland_protocols::wp::input_timestamps::zv1::client::zwp_input_timestamps_v1::{
    self, ZwpInputTimestampsV1,
};
use wayland_protocols::xdg::activation::v1::client::xdg_activation_token_v1::{
    self, XdgActivationTokenV1,
};
use wayland_protocols::xdg::activation::v1::client::xdg_activation_v1::XdgActivationV1;
use wayland_protocols::xdg::system_bell::v1::client::xdg_system_bell_v1::XdgSystemBellV1;
use wayland_protocols::xdg::toplevel_icon::v1::client::xdg_toplevel_icon_manager_v1::XdgToplevelIconManagerV1;
use wayland_protocols::xdg::toplevel_icon::v1::client::xdg_toplevel_icon_v1::XdgToplevelIconV1;
use wayland_protocols::xdg::toplevel_tag::v1::client::xdg_toplevel_tag_manager_v1::XdgToplevelTagManagerV1;

use crate::event::{SeatId, Secret, WindowId, WlEvent};
use crate::state::WlState;
use crate::window::truncate_utf8;

/// Longest tag, description or icon name sent (bytes).
const MAX_STRING: usize = 1024;

/// `wp_content_type_v1.type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentType {
    /// No hint.
    None,
    /// A video is playing (T-09 §3: media placement in a fullscreen pane).
    Video,
}

impl ContentType {
    fn to_wire(self) -> WireContentType {
        match self {
            ContentType::None => WireContentType::None,
            ContentType::Video => WireContentType::Video,
        }
    }
}

/// Bound optional globals.
#[derive(Default)]
pub(crate) struct Extras {
    pub activation: Option<XdgActivationV1>,
    pub bell: Option<XdgSystemBellV1>,
    pub icon: Option<XdgToplevelIconManagerV1>,
    pub tag: Option<XdgToplevelTagManagerV1>,
    pub content_type: Option<WpContentTypeManagerV1>,
    pub idle_inhibit: Option<ZwpIdleInhibitManagerV1>,
    pub timestamps: Option<ZwpInputTimestampsManagerV1>,
}

/// Per-window objects, created on first use.
#[derive(Default)]
pub(crate) struct WindowExtras {
    pub content_type: Option<WpContentTypeV1>,
    pub inhibitor: Option<ZwpIdleInhibitorV1>,
}

/// What an activation token is for.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TokenPurpose {
    pub window: WindowId,
    /// Use the token on the window's own surface (urgency) instead of reporting it.
    pub urgent: bool,
}

impl Extras {
    /// Bind `interface` if it is one of ours. Returns the bound version.
    pub(crate) fn bind(
        &mut self,
        registry: &WlRegistry,
        qh: &QueueHandle<WlState>,
        name: u32,
        interface: &str,
        version: u32,
    ) -> Option<u32> {
        macro_rules! b {
            ($field:ident, $ty:ty) => {{
                if self.$field.is_some() {
                    return None;
                }
                let p = registry.bind::<$ty, _, _>(name, 1, qh, ());
                let v = p.version();
                self.$field = Some(p);
                Some(v)
            }};
        }
        if version == 0 {
            return None;
        }
        match interface {
            "xdg_activation_v1" => b!(activation, XdgActivationV1),
            "xdg_system_bell_v1" => b!(bell, XdgSystemBellV1),
            "xdg_toplevel_icon_manager_v1" => b!(icon, XdgToplevelIconManagerV1),
            "xdg_toplevel_tag_manager_v1" => b!(tag, XdgToplevelTagManagerV1),
            "wp_content_type_manager_v1" => b!(content_type, WpContentTypeManagerV1),
            "zwp_idle_inhibit_manager_v1" => b!(idle_inhibit, ZwpIdleInhibitManagerV1),
            "zwp_input_timestamps_manager_v1" => b!(timestamps, ZwpInputTimestampsManagerV1),
            _ => None,
        }
    }

    /// A global went away.
    pub(crate) fn unbind(&mut self, interface: &str) {
        match interface {
            "xdg_activation_v1" => self.activation = None,
            "xdg_system_bell_v1" => self.bell = None,
            "xdg_toplevel_icon_manager_v1" => self.icon = None,
            "xdg_toplevel_tag_manager_v1" => self.tag = None,
            "wp_content_type_manager_v1" => self.content_type = None,
            "zwp_idle_inhibit_manager_v1" => self.idle_inhibit = None,
            "zwp_input_timestamps_manager_v1" => self.timestamps = None,
            _ => {}
        }
    }
}

delegate_noop!(WlState: ignore XdgActivationV1);
delegate_noop!(WlState: ignore XdgSystemBellV1);
delegate_noop!(WlState: ignore XdgToplevelIconManagerV1);
delegate_noop!(WlState: ignore XdgToplevelIconV1);
delegate_noop!(WlState: ignore XdgToplevelTagManagerV1);
delegate_noop!(WlState: ignore WpContentTypeManagerV1);
delegate_noop!(WlState: ignore WpContentTypeV1);
delegate_noop!(WlState: ignore ZwpIdleInhibitManagerV1);
delegate_noop!(WlState: ignore ZwpIdleInhibitorV1);
delegate_noop!(WlState: ignore ZwpInputTimestampsManagerV1);

impl Dispatch<XdgActivationTokenV1, TokenPurpose> for WlState {
    fn event(
        state: &mut Self,
        token_obj: &XdgActivationTokenV1,
        event: xdg_activation_token_v1::Event,
        purpose: &TokenPurpose,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_activation_token_v1::Event::Done { token } = event {
            if purpose.urgent {
                if let (Some(a), Some(wi)) =
                    (&state.extras.activation, state.win_index(purpose.window))
                {
                    a.activate(token, &state.windows[wi].surface);
                }
            } else {
                state.emit(WlEvent::ActivationToken {
                    window: purpose.window,
                    token: Secret(token),
                });
            }
            token_obj.destroy();
        }
    }
}

/// `tv_sec_hi:tv_sec_lo:tv_nsec` to nanoseconds, saturating.
pub(crate) fn timestamp_ns(hi: u32, lo: u32, nsec: u32) -> u64 {
    let sec = (u64::from(hi) << 32) | u64::from(lo);
    sec.saturating_mul(1_000_000_000)
        .saturating_add(u64::from(nsec.min(999_999_999)))
}

impl Dispatch<ZwpInputTimestampsV1, SeatId> for WlState {
    fn event(
        state: &mut Self,
        _: &ZwpInputTimestampsV1,
        event: zwp_input_timestamps_v1::Event,
        seat: &SeatId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_input_timestamps_v1::Event::Timestamp {
            tv_sec_hi,
            tv_sec_lo,
            tv_nsec,
        } = event
        {
            state.emit(WlEvent::InputTimestamp {
                seat: *seat,
                time_ns: timestamp_ns(tv_sec_hi, tv_sec_lo, tv_nsec),
                stamp: state.stamp,
            });
        }
    }
}

impl WlState {
    /// Ask for an activation token for `window` (focus hand-off to a launched child). The
    /// token arrives as [`WlEvent::ActivationToken`]. `false` without xdg-activation.
    pub(crate) fn request_activation_token(&mut self, window: WindowId) -> bool {
        self.new_token(window, false)
    }

    /// Request attention (urgency): token on our own surface, then `activate`.
    pub(crate) fn request_attention(&mut self, window: WindowId) -> bool {
        self.new_token(window, true)
    }

    fn new_token(&mut self, window: WindowId, urgent: bool) -> bool {
        let Some(wi) = self.win_index(window) else {
            return false;
        };
        let Some(a) = &self.extras.activation else {
            return false;
        };
        let t = a.get_activation_token(&self.qh, TokenPurpose { window, urgent });
        if let Some(s) = self.seats.first() {
            t.set_serial(s.last_serial, &s.seat);
        }
        t.set_surface(&self.windows[wi].surface);
        t.commit();
        true
    }

    /// Use a token received from a launcher to activate our window.
    pub(crate) fn activate_with_token(&mut self, window: WindowId, token: String) -> bool {
        let (Some(a), Some(wi)) = (&self.extras.activation, self.win_index(window)) else {
            return false;
        };
        a.activate(token, &self.windows[wi].surface);
        true
    }

    /// Ring the system bell (the surface is a hint for the compositor).
    pub(crate) fn ring_bell(&mut self, window: Option<WindowId>) -> bool {
        let Some(b) = &self.extras.bell else {
            return false;
        };
        let surface = window
            .and_then(|w| self.win_index(w))
            .map(|wi| &self.windows[wi].surface);
        b.ring(surface);
        true
    }

    /// Per-window icon by XDG icon name (`None` clears).
    pub(crate) fn set_icon_name(&mut self, wi: usize, name: Option<&str>) -> bool {
        let Some(m) = &self.extras.icon else {
            return false;
        };
        let toplevel = &self.windows[wi].toplevel;
        match name {
            Some(n) => {
                let icon = m.create_icon(&self.qh, ());
                icon.set_name(truncate_utf8(n, MAX_STRING).to_string());
                m.set_icon(toplevel, Some(&icon));
                icon.destroy();
            }
            None => m.set_icon(toplevel, None),
        }
        true
    }

    /// Stable tag and description for compositor rules.
    pub(crate) fn set_toplevel_tag(&mut self, wi: usize, tag: &str, description: &str) -> bool {
        let Some(m) = &self.extras.tag else {
            return false;
        };
        let t = &self.windows[wi].toplevel;
        m.set_toplevel_tag(t, truncate_utf8(tag, MAX_STRING).to_string());
        m.set_toplevel_description(t, truncate_utf8(description, MAX_STRING).to_string());
        true
    }

    /// Content type hint (`wp_content_type_v1`).
    pub(crate) fn set_content_type(&mut self, wi: usize, ty: ContentType) -> bool {
        let Some(m) = &self.extras.content_type else {
            return false;
        };
        let w = &mut self.windows[wi];
        let obj = w
            .extras
            .content_type
            .get_or_insert_with(|| m.get_surface_content_type(&w.surface, &self.qh, ()));
        obj.set_content_type(ty.to_wire());
        true
    }

    /// Inhibit or release idle (screensaver) for a window.
    pub(crate) fn set_idle_inhibit(&mut self, wi: usize, on: bool) -> bool {
        let Some(m) = &self.extras.idle_inhibit else {
            return false;
        };
        let w = &mut self.windows[wi];
        match (on, w.extras.inhibitor.is_some()) {
            (true, false) => {
                w.extras.inhibitor = Some(m.create_inhibitor(&w.surface, &self.qh, ()));
            }
            (false, true) => {
                if let Some(i) = w.extras.inhibitor.take() {
                    i.destroy();
                }
            }
            _ => {}
        }
        true
    }

    /// Attach nanosecond input timestamps to a seat's devices (idempotent per device).
    pub(crate) fn attach_timestamps(&mut self, si: usize) {
        let Some(m) = &self.extras.timestamps else {
            return;
        };
        let qh = &self.qh;
        let s = &mut self.seats[si];
        let id = s.id;
        if s.ts[0].is_none()
            && let Some(k) = &s.keyboard
        {
            s.ts[0] = Some(m.get_keyboard_timestamps(k, qh, id));
        }
        if s.ts[1].is_none()
            && let Some(p) = &s.pointer
        {
            s.ts[1] = Some(m.get_pointer_timestamps(p, qh, id));
        }
        if s.ts[2].is_none()
            && let Some(t) = &s.touch_dev
        {
            s.ts[2] = Some(m.get_touch_timestamps(t, qh, id));
        }
    }
}

/// Destroy window-owned extras before the surface.
pub(crate) fn destroy_window_extras(w: &mut WindowExtras) {
    if let Some(i) = w.inhibitor.take() {
        i.destroy();
    }
    if let Some(c) = w.content_type.take() {
        c.destroy();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_combines_and_saturates() {
        assert_eq!(timestamp_ns(0, 2, 5), 2_000_000_005);
        assert_eq!(timestamp_ns(1, 0, 0), 4_294_967_296_000_000_000);
        assert_eq!(timestamp_ns(u32::MAX, u32::MAX, 0), u64::MAX);
        assert_eq!(timestamp_ns(0, 0, u32::MAX), 999_999_999);
    }
}
