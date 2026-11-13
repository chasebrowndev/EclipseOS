// SPDX-License-Identifier: AGPL-3.0-only
//! `Dispatch` impls for everything except seats and selections (see `seat.rs`).

use wayland_client::globals::GlobalListContents;
use wayland_client::protocol::wl_callback::{self, WlCallback};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_region::WlRegion;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_shm::WlShm;
use wayland_client::protocol::wl_subcompositor::WlSubcompositor;
use wayland_client::protocol::wl_surface::{self, WlSurface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::ext::background_effect::v1::client::ext_background_effect_manager_v1::{
    self, ExtBackgroundEffectManagerV1,
};
use wayland_protocols::ext::background_effect::v1::client::ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1;
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::WpCursorShapeDeviceV1;
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::{
    self, WpFractionalScaleV1,
};
use wayland_protocols::wp::presentation_time::client::wp_presentation::{self, WpPresentation};
use wayland_protocols::wp::presentation_time::client::wp_presentation_feedback::{
    self, WpPresentationFeedback,
};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use wayland_protocols::xdg::decoration::zv1::client::zxdg_decoration_manager_v1::ZxdgDecorationManagerV1;
use wayland_protocols::xdg::decoration::zv1::client::zxdg_toplevel_decoration_v1::{
    self, ZxdgToplevelDecorationV1,
};
use wayland_protocols::xdg::shell::client::xdg_surface::{self, XdgSurface};
use wayland_protocols::xdg::shell::client::xdg_toplevel::{self, XdgToplevel};
use wayland_protocols::xdg::shell::client::xdg_wm_base::{self, XdgWmBase};
use wayland_protocols_plasma::blur::client::org_kde_kwin_blur::OrgKdeKwinBlur;
use wayland_protocols_plasma::blur::client::org_kde_kwin_blur_manager::OrgKdeKwinBlurManager;

use crate::clock::{Stamp, presentation_stamp};
use crate::event::{OutputEvent, OutputId, PresentedEvent, WindowId, WlEvent};
use crate::state::WlState;
use crate::window::{DecorationMode, WindowStates};

/// User data of a presentation feedback object.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FeedbackData {
    pub window: WindowId,
    pub id: u64,
}

delegate_noop!(WlState: ignore WlCompositor);
delegate_noop!(WlState: ignore WlSubcompositor);
delegate_noop!(WlState: ignore WlShm);
delegate_noop!(WlState: ignore WlRegion);
delegate_noop!(WlState: ignore ZxdgDecorationManagerV1);
delegate_noop!(WlState: ignore WpFractionalScaleManagerV1);
delegate_noop!(WlState: ignore WpViewporter);
delegate_noop!(WlState: ignore WpViewport);
delegate_noop!(WlState: ignore ExtBackgroundEffectSurfaceV1);
delegate_noop!(WlState: ignore OrgKdeKwinBlurManager);
delegate_noop!(WlState: ignore OrgKdeKwinBlur);
delegate_noop!(WlState: ignore WpCursorShapeManagerV1);
delegate_noop!(WlState: ignore WpCursorShapeDeviceV1);

impl Dispatch<WlRegistry, GlobalListContents> for WlState {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => {
                state.registry_names.insert(name, interface.clone());
                state.caps.set_present(&interface, version);
                state.bind_dynamic(registry, name, &interface, version);
                state.bind_extra(registry, name, &interface, version);
                // Blur managers may appear late: bind and re-evaluate the ladder.
                if interface == "ext_background_effect_manager_v1"
                    && state.globals.bg_effect.is_none()
                {
                    let m =
                        registry.bind::<ExtBackgroundEffectManagerV1, _, _>(name, 1, &state.qh, ());
                    state.caps.set_bound(&interface, 1);
                    state.globals.bg_effect = Some(m);
                } else if interface == "org_kde_kwin_blur_manager"
                    && state.globals.kwin_blur.is_none()
                {
                    let m = registry.bind::<OrgKdeKwinBlurManager, _, _>(name, 1, &state.qh, ());
                    state.caps.set_bound(&interface, 1);
                    state.globals.kwin_blur = Some(m);
                    for wi in 0..state.windows.len() {
                        state.reevaluate_blur(wi, false);
                    }
                }
                if crate::caps::spec(&interface).is_some() {
                    state.emit(WlEvent::CapabilitiesChanged);
                }
            }
            wl_registry::Event::GlobalRemove { name } => {
                let Some(interface) = state.registry_names.remove(&name) else {
                    return;
                };
                state.extras.unbind(&interface);
                match interface.as_str() {
                    "wl_seat" => crate::seat::remove_seat(state, name),
                    "wl_output" => state.remove_output(name),
                    "ext_background_effect_manager_v1" => {
                        state.globals.bg_effect = None;
                        state.ext_blur_cap = false;
                        for wi in 0..state.windows.len() {
                            state.reevaluate_blur(wi, false);
                        }
                    }
                    "org_kde_kwin_blur_manager" => {
                        state.globals.kwin_blur = None;
                        for wi in 0..state.windows.len() {
                            state.reevaluate_blur(wi, false);
                        }
                    }
                    _ => {}
                }
                state.caps.set_removed(&interface);
                state.emit(WlEvent::CapabilitiesChanged);
            }
            _ => {}
        }
    }
}

impl WlState {
    pub(crate) fn remove_output(&mut self, name: u32) {
        let Some(i) = self.outputs.iter().position(|o| o.info.id == name) else {
            return;
        };
        let o = self.outputs.remove(i);
        if o.proxy.version() >= 3 {
            o.proxy.release();
        }
        for wi in 0..self.windows.len() {
            if self.windows[wi].entered.contains(&name) {
                self.windows[wi].entered.retain(|n| *n != name);
                self.apply_scale(wi);
            }
        }
        self.emit(WlEvent::Output(OutputEvent::Removed(OutputId(name))));
    }
}

impl Dispatch<XdgWmBase, ()> for WlState {
    fn event(
        _: &mut Self,
        base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<WlSurface, WindowId> for WlState {
    fn event(
        state: &mut Self,
        _: &WlSurface,
        event: wl_surface::Event,
        id: &WindowId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(wi) = state.win_index(*id) else {
            return;
        };
        match event {
            wl_surface::Event::Enter { output } => {
                if let Some(OutputId(n)) = output.data::<OutputId>() {
                    let n = *n;
                    if !state.windows[wi].entered.contains(&n) {
                        state.windows[wi].entered.push(n);
                    }
                    state.apply_scale(wi);
                }
            }
            wl_surface::Event::Leave { output } => {
                if let Some(OutputId(n)) = output.data::<OutputId>() {
                    let n = *n;
                    state.windows[wi].entered.retain(|x| *x != n);
                    state.apply_scale(wi);
                }
            }
            wl_surface::Event::PreferredBufferScale { factor } => {
                state.windows[wi].preferred_buffer = Some(factor);
                state.apply_scale(wi);
            }
            _ => {}
        }
    }
}

impl Dispatch<XdgSurface, WindowId> for WlState {
    fn event(
        state: &mut Self,
        _: &XdgSurface,
        event: xdg_surface::Event,
        id: &WindowId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event
            && let Some(wi) = state.win_index(*id)
        {
            let ev = state.apply_configure(wi, serial);
            state.emit(WlEvent::Configure(ev));
        }
    }
}

impl Dispatch<XdgToplevel, WindowId> for WlState {
    fn event(
        state: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        id: &WindowId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(wi) = state.win_index(*id) else {
            return;
        };
        match event {
            xdg_toplevel::Event::Configure {
                width,
                height,
                states,
            } => {
                let w = &mut state.windows[wi];
                w.pending.size = (width > 0 && height > 0)
                    .then(|| crate::window::clamp_size((width as u32, height as u32)));
                w.pending.states = WindowStates::from_raw(&states);
            }
            xdg_toplevel::Event::ConfigureBounds { width, height } => {
                state.windows[wi].pending.bounds = (width > 0 && height > 0)
                    .then(|| crate::window::clamp_size((width as u32, height as u32)));
            }
            xdg_toplevel::Event::WmCapabilities { capabilities } => {
                state.windows[wi].wm_capabilities = capabilities
                    .chunks_exact(4)
                    .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                    .collect();
            }
            xdg_toplevel::Event::Close => state.emit(WlEvent::CloseRequested(*id)),
            _ => {}
        }
    }
}

impl Dispatch<ZxdgToplevelDecorationV1, WindowId> for WlState {
    fn event(
        state: &mut Self,
        _: &ZxdgToplevelDecorationV1,
        event: zxdg_toplevel_decoration_v1::Event,
        id: &WindowId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zxdg_toplevel_decoration_v1::Event::Configure { mode } = event
            && let Some(wi) = state.win_index(*id)
        {
            state.windows[wi].configured_mode = match mode {
                WEnum::Value(zxdg_toplevel_decoration_v1::Mode::ServerSide) => {
                    Some(DecorationMode::Server)
                }
                WEnum::Value(zxdg_toplevel_decoration_v1::Mode::ClientSide) => {
                    Some(DecorationMode::Client)
                }
                _ => None,
            };
        }
    }
}

impl Dispatch<WpFractionalScaleV1, WindowId> for WlState {
    fn event(
        state: &mut Self,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        id: &WindowId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event
            && let Some(wi) = state.win_index(*id)
        {
            state.windows[wi].fractional_pref = crate::scale::Scale::from_120ths(scale);
            state.apply_scale(wi);
        }
    }
}

impl Dispatch<WlCallback, WindowId> for WlState {
    fn event(
        state: &mut Self,
        _: &WlCallback,
        event: wl_callback::Event,
        id: &WindowId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { callback_data } = event {
            let stamp = state.stamp;
            state.emit(WlEvent::Frame {
                window: *id,
                callback_ms: callback_data,
                stamp,
            });
        }
    }
}

impl Dispatch<WpPresentation, ()> for WlState {
    fn event(
        state: &mut Self,
        _: &WpPresentation,
        event: wp_presentation::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_presentation::Event::ClockId { clk_id } = event {
            state.presentation_clock = Some(clk_id);
            if clk_id != crate::clock::CLOCK_MONOTONIC_ID {
                tracing::warn!(
                    clk_id,
                    "presentation clock is not CLOCK_MONOTONIC; LAT-PHOTON unavailable"
                );
            }
        }
    }
}

impl Dispatch<WpPresentationFeedback, FeedbackData> for WlState {
    fn event(
        state: &mut Self,
        _: &WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        data: &FeedbackData,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wp_presentation_feedback::Event::Presented {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                refresh,
                seq_hi,
                seq_lo,
                flags,
            } => {
                let time: Option<Stamp> =
                    presentation_stamp(state.presentation_clock, tv_sec_hi, tv_sec_lo, tv_nsec);
                let ev = PresentedEvent {
                    window: data.window,
                    feedback: data.id,
                    time,
                    refresh_ns: refresh,
                    seq: crate::clock::join64(seq_hi, seq_lo),
                    flags: u32::from(flags),
                };
                state.emit(WlEvent::Presented(ev));
            }
            wp_presentation_feedback::Event::Discarded => {
                state.emit(WlEvent::PresentationDiscarded {
                    window: data.window,
                    feedback: data.id,
                });
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtBackgroundEffectManagerV1, ()> for WlState {
    fn event(
        state: &mut Self,
        _: &ExtBackgroundEffectManagerV1,
        event: ext_background_effect_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_background_effect_manager_v1::Event::Capabilities { flags } = event {
            // Capability::Blur is bit 0; capabilities can change at runtime.
            let blur = u32::from(flags) & 1 != 0;
            state.set_ext_blur_capability(blur);
        }
    }
}

impl Dispatch<WlOutput, OutputId> for WlState {
    fn event(
        state: &mut Self,
        _: &WlOutput,
        event: wl_output::Event,
        id: &OutputId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(oi) = state.outputs.iter().position(|o| o.info.id == id.0) else {
            return;
        };
        match event {
            wl_output::Event::Geometry {
                x,
                y,
                physical_width,
                physical_height,
                ..
            } => {
                let i = &mut state.outputs[oi].info;
                i.position = (x, y);
                i.physical_mm = (physical_width, physical_height);
            }
            wl_output::Event::Mode {
                flags,
                width,
                height,
                refresh,
            } => {
                if matches!(flags, WEnum::Value(f) if f.contains(wl_output::Mode::Current)) {
                    let i = &mut state.outputs[oi].info;
                    i.mode = Some((width, height));
                    i.refresh_mhz = refresh;
                }
            }
            wl_output::Event::Scale { factor } => state.outputs[oi].info.scale = factor.max(1),
            wl_output::Event::Name { name } => state.outputs[oi].info.name = Some(name),
            wl_output::Event::Description { description } => {
                state.outputs[oi].info.description = Some(description)
            }
            wl_output::Event::Done => {
                let o = &mut state.outputs[oi];
                let info = o.info.clone();
                let ev = if o.announced {
                    OutputEvent::Changed(info)
                } else {
                    OutputEvent::Added(info)
                };
                o.announced = true;
                state.emit(WlEvent::Output(ev));
                // An output scale change moves integer-scale windows that sit on it.
                for wi in 0..state.windows.len() {
                    if state.windows[wi].entered.contains(&id.0) {
                        state.apply_scale(wi);
                    }
                }
            }
            _ => {}
        }
    }
}
