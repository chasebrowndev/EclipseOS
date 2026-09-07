// SPDX-License-Identifier: AGPL-3.0-only
//! Human input path (COMP-04 §2). M1: keyboard + pointer from a single
//! backend, one hardcoded quit binding, focus-follows-mouse.

use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent, KeyState,
        KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
    },
    input::{
        keyboard::{FilterResult, Keysym, ModifiersState},
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, SERIAL_COUNTER},
};

use crate::state::HeliosState;

/// Modifier set of a binding. Compared against the xkb modifier state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub logo: bool,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

impl Mods {
    pub fn matches(&self, state: &ModifiersState) -> bool {
        self.logo == state.logo
            && self.shift == state.shift
            && self.ctrl == state.ctrl
            && self.alt == state.alt
    }
}

/// Direction for focus/move actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// Compositor-level action bound to a chord (COMP-13 §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Quit,
    /// Ctrl+Alt+F1..F12 on the DRM backend.
    SwitchVt(i32),
    Spawn(String),
    Close,
    ToggleFloating,
    ToggleLayout,
    Focus(Direction),
    /// Swap with the neighbour in this direction (nudges a floating window).
    Move(Direction),
    /// 1-based workspace index.
    SwitchWorkspace(usize),
    /// 1-based workspace index.
    MoveToWorkspace(usize),
}

/// A configured key binding.
#[derive(Debug, Clone)]
pub struct Bind {
    pub mods: Mods,
    pub key: Keysym,
    pub action: Action,
}

/// `XF86_Switch_VT_1` .. `XF86_Switch_VT_12`.
const VT_SWITCH_FIRST: u32 = 0x1008_FE01;
const VT_SWITCH_LAST: u32 = 0x1008_FE0C;

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
        let action = keyboard.input(
            self,
            event.key_code(),
            event.state(),
            serial,
            time,
            |state, mods, handle| {
                if event.state() != KeyState::Pressed {
                    return FilterResult::Forward;
                }
                let sym = handle.modified_sym();
                let raw = sym.raw();
                if (VT_SWITCH_FIRST..=VT_SWITCH_LAST).contains(&raw) {
                    return FilterResult::Intercept(Action::SwitchVt((raw - VT_SWITCH_FIRST + 1) as i32));
                }
                match state.config.action_for(mods, sym) {
                    Some(a) => FilterResult::Intercept(a.clone()),
                    None => FilterResult::Forward,
                }
            },
        );
        if let Some(action) = action {
            self.run_action(action);
        }
    }

    fn run_action(&mut self, action: Action) {
        use crate::shell;
        match action {
            Action::Quit => self.quit(),
            Action::SwitchVt(vt) => self.switch_vt(vt),
            Action::Spawn(cmd) => shell::spawn(&cmd),
            Action::Close => shell::close_focused(self),
            Action::ToggleFloating => shell::toggle_floating(self),
            Action::ToggleLayout => shell::toggle_layout(self),
            Action::Focus(dir) => shell::focus_direction(self, dir),
            Action::Move(dir) => shell::move_direction(self, dir),
            Action::SwitchWorkspace(n) => shell::switch_workspace(self, n),
            Action::MoveToWorkspace(n) => shell::move_to_workspace(self, n),
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
        let geometries: Vec<_> = self
            .space
            .outputs()
            .filter_map(|o| self.space.output_geometry(o))
            .collect();
        if geometries.is_empty() || geometries.iter().any(|g| g.to_f64().contains(pos)) {
            return pos;
        }
        // Outside every output: clamp into whichever one is nearest.
        let clamp = |g: &smithay::utils::Rectangle<i32, Logical>| -> Point<f64, Logical> {
            let max_x = (g.loc.x + g.size.w - 1).max(g.loc.x) as f64;
            let max_y = (g.loc.y + g.size.h - 1).max(g.loc.y) as f64;
            (
                pos.x.clamp(g.loc.x as f64, max_x),
                pos.y.clamp(g.loc.y as f64, max_y),
            )
                .into()
        };
        geometries
            .iter()
            .map(clamp)
            .min_by(|a, b| {
                let d = |p: &Point<f64, Logical>| (p.x - pos.x).powi(2) + (p.y - pos.y).powi(2);
                d(a).total_cmp(&d(b))
            })
            .unwrap_or(pos)
    }

    /// Shared tail for both relative and absolute motion.
    fn pointer_moved(&mut self, pos: Point<f64, Logical>, time: u32) {
        let pos = self.clamp_to_outputs(pos);
        self.pointer_location = pos;
        // Pointer motion moves output focus, so a new window opens where the
        // human is looking (COMP-03 §3).
        if let Some(id) = self
            .space
            .output_under(pos)
            .next()
            .and_then(|o| self.outputs.by_output(o))
            .map(|e| e.id)
        {
            self.outputs.set_focused(id);
        }
        let serial = SERIAL_COUNTER.next_serial();
        let under = self.surface_under(pos);

        // Focus-follows-mouse (decided 2026-09-05), config-gated.
        if let Some(window) = self
            .config
            .general
            .focus_follows_mouse
            .then(|| self.space.element_under(pos).map(|(w, _)| w.clone()))
            .flatten()
        {
            let keyboard = self.seat.get_keyboard().unwrap();
            let target = window.toplevel().map(|t| t.wl_surface().clone());
            if keyboard.current_focus() != target {
                self.space.raise_element(&window, true);
                self.focus = Some(window.clone());
                keyboard.set_focus(self, target, serial);
                crate::shell::arrange(self);
            }
        }

        let pointer = self.seat.get_pointer().unwrap();
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pos,
                serial,
                time,
            },
        );
        pointer.frame(self);
    }

    fn on_pointer_motion<B: InputBackend>(&mut self, event: B::PointerMotionEvent) {
        let pos = self.pointer_location + event.delta();
        self.pointer_moved(pos, event.time_msec());
    }

    fn surface_under(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        crate::shell::surface_under(self, pos)
    }

    fn on_pointer_motion_absolute<B: InputBackend>(&mut self, event: B::PointerMotionAbsoluteEvent) {
        // Absolute devices are output-relative; use the output the pointer is
        // already on, else the focused one.
        let output = self
            .space
            .output_under(self.pointer_location)
            .next()
            .cloned()
            .or_else(|| self.outputs.focused().map(|e| e.output.clone()));
        let Some(output) = output else { return };
        let Some(geometry) = self.space.output_geometry(&output) else {
            return;
        };
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
