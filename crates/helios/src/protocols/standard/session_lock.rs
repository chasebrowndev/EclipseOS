// SPDX-License-Identifier: AGPL-3.0-only
//! `ext_session_lock_v1` (COMP-06, COMP-10 §5).
//!
//! The lock is compositor-side and unbypassable: while [`LockState::locked`] is
//! set, each backend draws [`lock_elements`] and nothing else: the locker's
//! surface for that output, or a compositor-drawn fallback where the locker
//! has not provided one. A locker that dies keeps the session locked behind that
//! fallback (ADR 0024) — a crash must never be a way in.

use std::collections::HashMap;

use smithay::{
    backend::renderer::{
        element::{
            solid::{SolidColorBuffer, SolidColorRenderElement},
            surface::render_elements_from_surface_tree,
            Kind,
        },
        gles::GlesRenderer,
    },
    delegate_session_lock,
    output::Output,
    reexports::wayland_server::protocol::wl_output::WlOutput,
    utils::{Logical, Point, Scale, Size, SERIAL_COUNTER},
    wayland::session_lock::{LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker},
};

use crate::{render::HeliosRenderElement, state::HeliosState};

/// Fallback screen: pure black with a centred amber bar (eclipse trusted-UI
/// palette). Compositor-drawn — never a client surface.
const FALLBACK_BG: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
const FALLBACK_FG: [f32; 4] = [1.0, 0.72, 0.15, 1.0];
const FALLBACK_BAR_H: i32 = 4;
const FALLBACK_BAR_W: i32 = 220;

/// Lock surfaces and fallback quads, kept between frames.
#[derive(Default)]
pub struct LockState {
    pub locked: bool,
    surfaces: Vec<(Output, LockSurface)>,
    /// Per-output (background, bar) quads, keyed by output name.
    fallback: HashMap<String, (SolidColorBuffer, SolidColorBuffer)>,
}

impl LockState {
    /// The locker's surface for `output`, if it supplied one and it is alive.
    pub fn surface_for(&self, output: &Output) -> Option<&LockSurface> {
        self.surfaces
            .iter()
            .find(|(o, s)| o == output && s.alive())
            .map(|(_, s)| s)
    }

    /// The compositor-drawn fallback quads for `output`, sized to it.
    pub fn fallback_for(&mut self, output: &Output) -> &(SolidColorBuffer, SolidColorBuffer) {
        let size = logical_size(output);
        let bar = Size::<i32, Logical>::from((FALLBACK_BAR_W.min(size.w), FALLBACK_BAR_H));
        let entry = self.fallback.entry(output.name()).or_insert_with(|| {
            (
                SolidColorBuffer::new(size, FALLBACK_BG),
                SolidColorBuffer::new(bar, FALLBACK_FG),
            )
        });
        entry.0.update(size, FALLBACK_BG);
        entry.1.update(bar, FALLBACK_FG);
        entry
    }

    /// Drop state for an output that went away.
    pub fn forget_output(&mut self, output: &Output) {
        self.surfaces.retain(|(o, _)| o != output);
        self.fallback.remove(&output.name());
    }
}

/// Logical size of `output` at its current mode, scale and transform.
pub fn logical_size(output: &Output) -> Size<i32, Logical> {
    let Some(mode) = output.current_mode() else {
        return (0, 0).into();
    };
    output
        .current_transform()
        .transform_size(mode.size)
        .to_f64()
        .to_logical(output.current_scale().fractional_scale())
        .to_i32_round()
}

impl SessionLockHandler for HeliosState {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        self.lock.locked = true;
        tracing::info!("session locked");
        // Nothing behind the lock may keep focus, even for one frame.
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, None, SERIAL_COUNTER.next_serial());
        }
        crate::backend::damage_all(self);
        confirmation.lock();
    }

    fn unlock(&mut self) {
        self.lock.locked = false;
        self.lock.surfaces.clear();
        tracing::info!("session unlocked");
        crate::shell::refocus_topmost(self);
        crate::backend::damage_all(self);
    }

    fn new_surface(&mut self, surface: LockSurface, output: WlOutput) {
        let Some(output) = Output::from_resource(&output) else {
            tracing::warn!("lock surface for an unknown output");
            return;
        };
        let size = logical_size(&output);
        surface.with_pending_state(|state| {
            state.size = Some((size.w.max(0) as u32, size.h.max(0) as u32).into());
        });
        surface.send_configure();
        self.lock.surfaces.retain(|(o, _)| o != &output);
        let wl = surface.wl_surface().clone();
        self.lock.surfaces.push((output, surface));
        // Input goes to the locker and nowhere else.
        if let Some(keyboard) = self.seat.get_keyboard() {
            if keyboard.current_focus().is_none() {
                keyboard.set_focus(self, Some(wl), SERIAL_COUNTER.next_serial());
            }
        }
        crate::backend::damage_all(self);
    }
}

delegate_session_lock!(HeliosState);

/// Everything drawn on `output` while the session is locked: the locker's
/// surface if it gave us one, otherwise the compositor-drawn fallback. No
/// client content of any other kind is ever included here.
pub fn lock_elements(
    renderer: &mut GlesRenderer,
    lock: &mut LockState,
    output: &Output,
) -> Vec<HeliosRenderElement> {
    let scale = Scale::from(output.current_scale().fractional_scale());
    if let Some(surface) = lock.surface_for(output) {
        return render_elements_from_surface_tree(
            renderer,
            surface.wl_surface(),
            (0, 0),
            scale,
            1.0,
            Kind::Unspecified,
        );
    }

    let size = logical_size(output);
    let (bg, bar) = lock.fallback_for(output);
    let bar_w = FALLBACK_BAR_W.min(size.w);
    let bar_loc = Point::<i32, Logical>::from(((size.w - bar_w) / 2, (size.h - FALLBACK_BAR_H) / 2));
    vec![
        HeliosRenderElement::Solid(SolidColorRenderElement::from_buffer(
            bar,
            bar_loc.to_f64().to_physical_precise_round(scale),
            scale,
            1.0,
            Kind::Unspecified,
        )),
        HeliosRenderElement::Solid(SolidColorRenderElement::from_buffer(
            bg,
            (0, 0),
            scale,
            1.0,
            Kind::Unspecified,
        )),
    ]
}
