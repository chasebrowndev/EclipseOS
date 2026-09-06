// SPDX-License-Identifier: AGPL-3.0-only
//! Human input path (COMP-04 §2). M1: keyboard + pointer from a single
//! backend, one hardcoded quit binding, focus-follows-mouse.

use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent, KeyState,
        KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
    },
    desktop::WindowSurfaceType,
    input::{
        keyboard::{FilterResult, Keysym, ModifiersState},
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, SERIAL_COUNTER},
};

use crate::state::HeliosState;

/// Compositor-level action bound to a chord. Bindings come from config in
/// M6; until then the set is fixed here.
#[derive(Debug, Clone, Copy)]
enum Action {
    Quit,
    /// Ctrl+Alt+F1..F12 on the DRM backend.
    SwitchVt(i32),
}

/// `XF86_Switch_VT_1` .. `XF86_Switch_VT_12`.
const VT_SWITCH_FIRST: u32 = 0x1008_FE01;
const VT_SWITCH_LAST: u32 = 0x1008_FE0C;

fn binding(mods: &ModifiersState, sym: Keysym) -> Option<Action> {
    let raw = sym.raw();
    if (VT_SWITCH_FIRST..=VT_SWITCH_LAST).contains(&raw) {
        return Some(Action::SwitchVt((raw - VT_SWITCH_FIRST + 1) as i32));
    }
    match sym {
        Keysym::q | Keysym::Q if mods.logo && mods.shift => Some(Action::Quit),
        _ => None,
    }
}

impl HeliosState {
    pub fn process_input_event<B: InputBackend>(&mut self, event: InputEvent<B>) {
        match event {
            InputEvent::Keyboard { event } => self.on_keyboard::<B>(event),
            InputEvent::PointerMotion { event } => self.on_pointer_motion::<B>(event),
            InputEvent::PointerMotionAbsolute { event } => self.on_pointer_motion_absolute::<B>(event),
            InputEvent::PointerButton { event } => self.on_pointer_button::<B>(event),
            InputEvent::PointerAxis { event } => self.on_pointer_axis::<B>(event),
            _ => {}
        }
    }

    fn on_keyboard<B: InputBackend>(&mut self, event: B::KeyboardKeyEvent) {
        let serial = SERIAL_COUNTER.next_serial();
        let time = Event::time_msec(&event);
        let keyboard = self.seat.get_keyboard().unwrap();
        let action = keyboard.input(self, event.key_code(), event.state(), serial, time, |_, mods, handle| {
            if event.state() == KeyState::Pressed {
                if let Some(a) = binding(mods, handle.modified_sym()) {
                    return FilterResult::Intercept(a);
                }
            }
            FilterResult::Forward
        });
        if let Some(action) = action {
            match action {
                Action::Quit => self.quit(),
                Action::SwitchVt(vt) => self.switch_vt(vt),
            }
        }
    }

    fn switch_vt(&mut self, vt: i32) {
        #[cfg(feature = "drm")]
        if let Some(drm) = self.drm.as_mut() {
            tracing::info!(vt, "switching VT");
            drm.change_vt(vt);
            return;
        }
        tracing::debug!(vt, "VT switch ignored (not on the DRM backend)");
    }

    /// Clamp a candidate pointer position into the union of output geometry.
    fn clamp_to_outputs(&self, pos: Point<f64, Logical>) -> Point<f64, Logical> {
        let Some(output) = self.space.outputs().next().cloned() else { return pos };
        let Some(geo) = self.space.output_geometry(&output) else { return pos };
        let max_x = (geo.loc.x + geo.size.w - 1) as f64;
        let max_y = (geo.loc.y + geo.size.h - 1) as f64;
        (
            pos.x.clamp(geo.loc.x as f64, max_x.max(geo.loc.x as f64)),
            pos.y.clamp(geo.loc.y as f64, max_y.max(geo.loc.y as f64)),
        )
            .into()
    }

    /// Shared tail for both relative and absolute motion.
    fn pointer_moved(&mut self, pos: Point<f64, Logical>, time: u32) {
        let pos = self.clamp_to_outputs(pos);
        self.pointer_location = pos;
        let serial = SERIAL_COUNTER.next_serial();
        let under = self.surface_under(pos);

        // Focus-follows-mouse (decided 2026-09-05).
        if let Some(window) = self.space.element_under(pos).map(|(w, _)| w.clone()) {
            let keyboard = self.seat.get_keyboard().unwrap();
            let target = window.toplevel().map(|t| t.wl_surface().clone());
            if keyboard.current_focus() != target {
                self.space.raise_element(&window, true);
                keyboard.set_focus(self, target, serial);
            }
        }

        let pointer = self.seat.get_pointer().unwrap();
        pointer.motion(self, under, &MotionEvent { location: pos, serial, time });
        pointer.frame(self);
    }

    fn on_pointer_motion<B: InputBackend>(&mut self, event: B::PointerMotionEvent) {
        let pos = self.pointer_location + event.delta();
        self.pointer_moved(pos, event.time_msec());
    }

    fn surface_under(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        let (window, loc) = self.space.element_under(pos)?;
        let (surface, surf_loc) = window.surface_under(pos - loc.to_f64(), WindowSurfaceType::ALL)?;
        Some((surface, (loc + surf_loc).to_f64()))
    }

    fn on_pointer_motion_absolute<B: InputBackend>(&mut self, event: B::PointerMotionAbsoluteEvent) {
        let Some(output) = self.space.outputs().next().cloned() else { return };
        let Some(geometry) = self.space.output_geometry(&output) else { return };
        let pos = event.position_transformed(geometry.size) + geometry.loc.to_f64();
        self.pointer_moved(pos, event.time_msec());
    }

    fn on_pointer_button<B: InputBackend>(&mut self, event: B::PointerButtonEvent) {
        let pointer = self.seat.get_pointer().unwrap();
        pointer.button(
            self,
            &ButtonEvent {
                serial: SERIAL_COUNTER.next_serial(),
                time: event.time_msec(),
                button: event.button_code(),
                state: match event.state() {
                    ButtonState::Pressed => ButtonState::Pressed,
                    ButtonState::Released => ButtonState::Released,
                },
            },
        );
        pointer.frame(self);
    }

    fn on_pointer_axis<B: InputBackend>(&mut self, event: B::PointerAxisEvent) {
        let source = event.source();
        let mut frame = AxisFrame::new(event.time_msec()).source(source);
        for axis in [Axis::Horizontal, Axis::Vertical] {
            let amount = event
                .amount(axis)
                .unwrap_or_else(|| event.amount_v120(axis).unwrap_or(0.0) * 15.0 / 120.0);
            if amount != 0.0 {
                frame = frame.value(axis, amount);
                if let Some(v120) = event.amount_v120(axis) {
                    frame = frame.v120(axis, v120 as i32);
                }
            } else if source == AxisSource::Finger {
                frame = frame.stop(axis);
            }
        }
        let pointer = self.seat.get_pointer().unwrap();
        pointer.axis(self, frame);
        pointer.frame(self);
    }
}
