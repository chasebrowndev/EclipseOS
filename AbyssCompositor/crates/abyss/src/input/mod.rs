// SPDX-License-Identifier: AGPL-3.0-only
//! Human input path (COMP-04 §2). M1: keyboard + pointer from a single
//! backend, one hardcoded quit binding, focus-follows-mouse.

use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent, KeyState,
        KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent, Switch, SwitchState,
        SwitchToggleEvent,
    },
    input::{
        keyboard::{FilterResult, Keysym, ModifiersState},
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, SERIAL_COUNTER},
};

use crate::state::AbyssState;

pub mod grabs;
pub mod idle;
pub mod inject;

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
    /// Send the focused window away (COMP-05 §4 `minimized`).
    Minimize,
    /// Bring back the last window sent away on the active workspace.
    Unminimize,
    ToggleLayout,
    Focus(Direction),
    /// Swap with the neighbour in this direction (nudges a floating window).
    Move(Direction),
    /// 1-based workspace index.
    SwitchWorkspace(usize),
    /// 1-based workspace index.
    MoveToWorkspace(usize),
    /// The override chord reserved by COMP-13 §1.1. Accepted and dispatched
    /// today so a config naming it is valid; it has nothing to revoke until
    /// agent seats exist (COMP-16 milestone 11).
    AgentOverride,
    /// The pending-decision-queue chord reserved by COMP-13 §1.1 (COMP-10
    /// §3.10). Dispatched today for the same reason as `AgentOverride`; the
    /// queue it opens arrives with the trusted UI (COMP-16 milestone 14).
    AgentAttention,
    /// One keypress consumed by the overscan calibration overlay (COMP-03 §2).
    /// Not bindable from config — the input filter synthesises it while a
    /// calibration session owns the seat.
    Calibrate(crate::outputs::calibrate::Step),
    /// One keypress consumed by the region selector (COMP-18 §1.3). Not
    /// bindable from config — the input filter synthesises it while a
    /// selection owns the seat.
    RegionSelect(crate::render::select::SelectKey),
    /// Chords an addon owns (COMP-18 §4). The compositor does not act on
    /// these itself; it forwards the name on the `keybind` event stream, so
    /// nothing happens when no addon is listening.
    ///
    /// `AnnotationSelect` is the exception: a rectangle is authority over what
    /// gets read, so the compositor runs the selection itself (COMP-18 §1.3)
    /// and forwards only the result.
    AnnotationSelect,
    AnnotationDismiss,
    AnnotationExpand,
}

/// A configured key binding.
#[derive(Debug, Clone)]
pub struct Bind {
    pub mods: Mods,
    pub key: Keysym,
    pub action: Action,
}

/// `linux/input-event-codes.h`. The selector drags with left and cancels with
/// right; every other button is swallowed.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

/// `XF86_Switch_VT_1` .. `XF86_Switch_VT_12`.
const VT_SWITCH_FIRST: u32 = 0x1008_FE01;
const VT_SWITCH_LAST: u32 = 0x1008_FE0C;

impl AbyssState {
    pub fn process_input_event<B: InputBackend>(&mut self, event: InputEvent<B>) {
        // Activity only — never the event's content (COMP-04, F-02).
        idle::on_activity(self);
        match event {
            InputEvent::Keyboard { event } => self.on_keyboard::<B>(event),
            InputEvent::PointerMotion { event } => self.on_pointer_motion::<B>(event),
            InputEvent::PointerMotionAbsolute { event } => self.on_pointer_motion_absolute::<B>(event),
            InputEvent::PointerButton { event } => self.on_pointer_button::<B>(event),
            InputEvent::PointerAxis { event } => self.on_pointer_axis::<B>(event),
            InputEvent::SwitchToggle { event } => self.on_switch::<B>(event),
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
                // Locked: no binding may act on the session behind the lock.
                // Keys still reach the locker, which holds keyboard focus.
                if state.lock.locked {
                    return FilterResult::Forward;
                }
                // Calibration owns the seat outright while it runs: every key
                // is consumed, including ones that are bound to something else.
                if let Some(step) = crate::outputs::calibrate::step_for(state, mods, sym) {
                    return FilterResult::Intercept(Action::Calibrate(step));
                }
                // So does the region selector: a modal grab that leaked a
                // chord through would let the human act on the session while
                // the screen says it is picking a rectangle.
                if state.region_select.active() {
                    let chord = matches!(state.config.action_for(mods, sym), Some(Action::AnnotationSelect));
                    return FilterResult::Intercept(Action::RegionSelect(crate::render::select::key(
                        sym, chord,
                    )));
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
            Action::Minimize => shell::minimize_focused(self),
            Action::Unminimize => shell::unminimize_last(self),
            Action::ToggleLayout => shell::toggle_layout(self),
            Action::Focus(dir) => shell::focus_direction(self, dir),
            Action::Move(dir) => shell::move_direction(self, dir),
            Action::SwitchWorkspace(n) => shell::switch_workspace(self, n),
            Action::MoveToWorkspace(n) => shell::move_to_workspace(self, n),
            Action::AgentOverride => self.agent_override(),
            Action::AgentAttention => self.agent_attention(),
            Action::Calibrate(step) => {
                crate::outputs::calibrate::apply(self, step);
            }
            Action::RegionSelect(key) => self.region_select_key(key),
            Action::AnnotationSelect => self.region_select_start(),
            Action::AnnotationDismiss => self.emit_keybind("annotation-dismiss"),
            Action::AnnotationExpand => self.emit_keybind("annotation-expand"),
        }
    }

    /// COMP-04 §6: hand the seat back to the human, unconditionally. There are
    /// no agent seats yet, so there is nothing to take back — but the chord must
    /// already work, because a chord that silently does nothing on the day it is
    /// needed is worse than one that was never bound.
    fn agent_override(&mut self) {
        tracing::warn!("agent override chord pressed; no agent seats exist yet");
    }

    /// COMP-10 §3.10: opens the pending decision queue. Both agent chords are
    /// evaluated on the human seat only — agent seats carry no bindings
    /// (COMP-04 §5), so injected keys can never reach here.
    fn agent_attention(&mut self) {
        tracing::warn!("agent attention chord pressed; no pending decision queue exists yet");
    }

    /// COMP-18 §4: forward a chord to whoever subscribed, and do nothing
    /// else. The compositor learns no state from an addon being there, which
    /// is what keeps Oracle-Eyes outside the TCB.
    fn emit_keybind(&mut self, action: &str) {
        crate::ipc::emit(self, "keybind", serde_json::json!({ "action": action }));
    }

    /// COMP-18 §1.3: enter modal region selection. The chord toggles, so the
    /// same key that started it gets the human back out.
    fn region_select_start(&mut self) {
        if self.region_select.active() {
            self.region_select.cancel();
        } else {
            self.region_select.start(self.pointer_location.to_i32_round());
        }
        crate::backend::damage_all(self);
    }

    fn region_select_key(&mut self, key: crate::render::select::SelectKey) {
        use crate::render::select::SelectKey;
        match key {
            // Cancelling emits nothing at all: an addon must not be able to
            // tell a refused selection from one that never started.
            SelectKey::Cancel => {
                self.region_select.cancel();
                crate::backend::damage_all(self);
            }
            SelectKey::Ignored => {}
        }
    }

    /// A pointer button while the selector owns the seat. No client sees it.
    fn region_select_button(&mut self, pressed: bool) {
        let pos = self.pointer_location.to_i32_round();
        if pressed {
            self.region_select.press(pos);
        } else if let Some(rect) = self.region_select.release(pos) {
            let payload = crate::render::select::payload(rect);
            crate::ipc::emit(self, "keybind", payload);
        }
        crate::backend::damage_all(self);
    }

    fn switch_vt(&mut self, vt: i32) {
        if let Some(backend) = self.backend_mut() {
            backend.change_vt(vt);
        }
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
    pub(crate) fn pointer_moved(&mut self, pos: Point<f64, Logical>, time: u32) {
        let pos = self.clamp_to_outputs(pos);
        // The cursor is compositor-drawn, so it keeps moving during a
        // selection; nothing under it hears about that until the drag commits.
        if self.region_select.active() {
            self.pointer_location = pos;
            if self.region_select.motion(pos.to_i32_round()) {
                crate::backend::damage_all(self);
            }
            return;
        }
        let current = self.pointer_location;
        let pointer = self.seat.get_pointer().unwrap();
        let focus = pointer.current_focus();
        drop(pointer);
        // A `zwp_pointer_constraints_v1` lock/confine on the currently
        // focused surface overrides raw device motion (COMP-06 §1):
        // smithay does no clamping itself, only tracks activation state.
        let pos = if let Some(surface) = &focus {
            let origin = crate::shell::surface_under(self, current)
                .filter(|(s, _)| s == surface)
                .map(|(_, p)| p.to_i32_round());
            crate::protocols::standard::pointer_constraints::apply_pointer_constraint(
                self,
                Some(surface),
                origin,
                current,
                pos,
            )
        } else {
            pos
        };
        self.pointer_location = pos;
        if self.lock.locked {
            // The pointer still moves (the cursor is compositor-drawn) but no
            // surface under the lock ever sees it.
            let serial = SERIAL_COUNTER.next_serial();
            let pointer = self.seat.get_pointer().unwrap();
            pointer.motion(
                self,
                None,
                &MotionEvent {
                    location: pos,
                    serial,
                    time,
                },
            );
            pointer.frame(self);
            self.last_pointer_focus = None;
            return;
        }
        // Pointer motion moves output focus, so a new window opens where the
        // human is looking (COMP-03 §3).
        let under_output = self
            .space
            .output_under(pos)
            .next()
            .and_then(|o| self.outputs.by_output(o))
            .map(|e| e.id);
        if let Some(id) = under_output {
            if self.outputs.set_focused(id) {
                // Only on a real transition: the unchanged path must not
                // allocate, and this is pointer motion.
                let name = self
                    .outputs
                    .get(id)
                    .map(|e| e.connector.clone())
                    .unwrap_or_default();
                crate::ipc::emit(self, "output", serde_json::json!({ "focused": id, "name": name }));
            }
        }
        let serial = SERIAL_COUNTER.next_serial();
        let under = self.surface_under(pos);

        // Focus-follows-mouse (decided 2026-09-05), config-gated. The rules
        // themselves live in one pure function (ADR 0042); this path only
        // reads the context, then applies the verdict. Compute the action
        // while the borrow is shared, then drop it before applying.
        if self.config.general.focus_follows_mouse {
            let action =
                crate::shell::focus::decide_pointer_focus(&crate::shell::focus::pointer_focus_ctx(self, pos));
            crate::shell::focus::apply_focus(self, action, crate::shell::focus::FocusCause::Pointer);
        }

        let new_focus = under.as_ref().map(|(s, _)| s.clone());
        self.last_pointer_focus = under.as_ref().map(|(s, p)| (s.clone(), p.to_i32_round()));
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
        // Smithay deactivates a constraint on the surface being left
        // automatically (`PointerTarget::leave`); activating one held by the
        // surface being entered is the compositor's half (COMP-06 §1).
        crate::protocols::standard::pointer_constraints::update_pointer_constraint_focus(
            self,
            focus.as_ref(),
            new_focus.as_ref(),
        );
    }

    /// Re-evaluate what the pointer is over when the *scene* changed under a
    /// stationary pointer: a window moved or resized, a subsurface slid
    /// under/out from under it, or an input region shrank away (COMP-04 §6).
    /// Device motion goes through `pointer_moved`; this is its scene-driven
    /// twin. It moves keyboard focus too when `general.refocus-on-scene-change`
    /// is set (the default), so a window closing under a stationary cursor
    /// hands focus to whatever is now underneath instead of leaving it stale
    /// until the mouse is jiggled (ADR 0042).
    pub(crate) fn refresh_pointer_focus(&mut self) {
        if self.lock.locked {
            return;
        }
        // An interactive move/resize deliberately clears pointer focus for the
        // duration of the drag, so there is nothing to refresh — and refreshing
        // would be fatal: this is called from `shell::place_at`, which the move
        // grab calls from inside its own `motion` callback, and smithay holds
        // the pointer's internal mutex across that callback. Re-entering
        // `PointerHandle::motion` there deadlocks the compositor thread.
        //
        // This reads like a redundant subset of `grabs::drag_active` and is
        // not: that predicate calls `PointerHandle::is_grabbed`, which takes
        // the very mutex smithay is holding on the re-entrant path. This bail
        // must stay a plain flag read, and must stay first.
        if self.pointer_grab_active {
            return;
        }
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let under = self.surface_under(self.pointer_location);
        let next = under.as_ref().map(|(s, p)| (s.clone(), p.to_i32_round()));
        // Smithay forwards a same-focus refresh as an unconditional
        // `wl_pointer.motion`, so only deliver when something actually moved.
        if next == self.last_pointer_focus {
            return;
        }
        // The scene changed under a stationary pointer, so re-derive keyboard
        // focus from the same rules a real motion would use. Gated: with
        // `refocus-on-scene-change` off, only pointer focus is refreshed.
        let ctx = crate::shell::focus::pointer_focus_ctx(self, self.pointer_location);
        if ctx.refocus_on_scene_change {
            let action = crate::shell::focus::decide_pointer_focus(&ctx);
            crate::shell::focus::apply_focus(self, action, crate::shell::focus::FocusCause::WindowUnmap);
        }

        let old_focus = self.last_pointer_focus.take().map(|(s, _)| s);
        let new_focus = next.as_ref().map(|(s, _)| s.clone());
        self.last_pointer_focus = next;
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.start_time.elapsed().as_millis() as u32;
        let location = self.pointer_location;
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location,
                serial,
                time,
            },
        );
        pointer.frame(self);
        crate::protocols::standard::pointer_constraints::update_pointer_constraint_focus(
            self,
            old_focus.as_ref(),
            new_focus.as_ref(),
        );
    }

    /// Hardware switches (COMP-01 §4.1). Lid close/open drives the internal
    /// output; tablet mode is logged and otherwise ignored for now.
    fn on_switch<B: InputBackend>(&mut self, event: B::SwitchToggleEvent) {
        match event.switch() {
            Some(Switch::Lid) => {
                let closed = event.state() == SwitchState::On;
                crate::outputs::power::lid_switch(self, closed);
            }
            Some(Switch::TabletMode) => {
                tracing::info!(on = event.state() == SwitchState::On, "tablet-mode switch");
            }
            other => tracing::debug!(?other, "unhandled switch"),
        }
    }

    fn on_pointer_motion<B: InputBackend>(&mut self, event: B::PointerMotionEvent) {
        let pos = self.pointer_location + event.delta();
        self.pointer_moved(pos, event.time_msec());
    }

    pub(crate) fn surface_under(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
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
        // The event lands where the *panel* was touched; with overscan the
        // desktop is painted into an inset rect, so invert that map or the
        // pointer sits off by the margin (COMP-03 §2).
        let overscan = self
            .outputs
            .by_output(&output)
            .map(|e| e.overscan)
            .unwrap_or_default();
        let mut local = event.position_transformed(geometry.size);
        if !overscan.is_zero() {
            let (w, h) = (geometry.size.w.max(1) as f64, geometry.size.h.max(1) as f64);
            let unit = Point::<f64, Logical>::from((local.x / w, local.y / h));
            let unit = overscan.untransform_unit(unit, crate::outputs::mode_size(&output));
            local = (unit.x * w, unit.y * h).into();
        }
        let pos = local + geometry.loc.to_f64();
        self.pointer_moved(pos, event.time_msec());
    }

    fn on_pointer_button<B: InputBackend>(&mut self, event: B::PointerButtonEvent) {
        let pressed = event.state() == ButtonState::Pressed;
        // The selector holds the pointer as well as the keyboard: a press that
        // reached a client would focus or activate something behind the dim.
        if self.region_select.active() {
            match event.button_code() {
                BTN_LEFT => self.region_select_button(pressed),
                // Right-click cancels, the same as Escape: the pointer hand is
                // already on the mouse, so make it reachable from there too.
                BTN_RIGHT if pressed => self.region_select_key(crate::render::select::SelectKey::Cancel),
                _ => {}
            }
            return;
        }
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
        // Click-to-focus, the complement of focus-follows-mouse: the pointer
        // can come to rest on a window that is not focused — a surface opened
        // or closed under a still pointer, or a keyboard-exclusive layer
        // surface that held the keyboard — and then no motion event is coming
        // to fix it. A press says which window the human means. Not while a
        // popup grab is up: there the press is the dismissal, handled below.
        if pressed && self.popup_grabs.is_empty() {
            let pos = self.pointer_location;
            let action =
                crate::shell::focus::decide_pointer_focus(&crate::shell::focus::pointer_focus_ctx(self, pos));
            crate::shell::focus::apply_focus(self, action, crate::shell::focus::FocusCause::Click);
        }
        // A press outside the grab dismisses it, and must be delivered first:
        // xdg-shell forbids `popup_done` preceding the button that caused it.
        if pressed && !self.popup_grabs.is_empty() {
            let under = self.surface_under(self.pointer_location).map(|(s, _)| s);
            crate::shell::popup_grab_button_press(self, under.as_ref());
        }
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

/// Borrowed xkb settings from the `input` block (COMP-13 §1.2). Rules and model
/// stay at the xkb defaults; the spec exposes neither.
pub fn xkb_config(input: &crate::config::Input) -> smithay::input::keyboard::XkbConfig<'_> {
    smithay::input::keyboard::XkbConfig {
        layout: &input.kb_layout,
        variant: &input.kb_variant,
        options: input.kb_options.clone(),
        ..Default::default()
    }
}

/// Push the current `input` block onto the live seat and every open device.
/// Called once at startup and again on every config reload.
pub fn apply_config(state: &mut AbyssState) {
    let Some(kbd) = state.seat.get_keyboard() else {
        return;
    };
    let (rate, delay) = (state.config.input.repeat_rate, state.config.input.repeat_delay);
    kbd.change_repeat_info(rate, delay);
    // A bad layout must not take the keyboard away: the old keymap stays.
    let cfg = state.config.clone();
    if let Err(err) = kbd.set_xkb_config(state, xkb_config(&cfg.input)) {
        tracing::error!(?err, layout = cfg.input.kb_layout, "keeping the previous keymap");
    }
    #[cfg(feature = "drm")]
    if let Some(drm) = state.drm.as_mut() {
        for device in drm.input_devices.iter_mut() {
            configure_device(device, &cfg.input);
        }
    }
}

/// Apply the pointer half of the `input` block to one libinput device. Every
/// setter is optional on the device; a device that does not support a knob
/// simply reports failure and keeps its default.
#[cfg(feature = "drm")]
pub fn configure_device(device: &mut smithay::reexports::input::Device, input: &crate::config::Input) {
    use smithay::reexports::input::{AccelProfile, ClickMethod, ScrollMethod};
    let _ = device.config_accel_set_profile(match input.accel_profile.as_str() {
        "flat" => AccelProfile::Flat,
        _ => AccelProfile::Adaptive,
    });
    if device.config_tap_finger_count() > 0 {
        let _ = device.config_tap_set_enabled(input.touchpad.tap_to_click);
        let _ = device.config_click_set_method(ClickMethod::Clickfinger);
        let _ = device.config_dwt_set_enabled(input.touchpad.dwt);
        if device.config_scroll_methods().contains(&ScrollMethod::TwoFinger) {
            let _ = device.config_scroll_set_natural_scroll_enabled(input.touchpad.natural_scroll);
        }
    }
}
