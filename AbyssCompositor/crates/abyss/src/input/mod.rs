// SPDX-License-Identifier: AGPL-3.0-only
//! Human input path (COMP-04 §2). M1: keyboard + pointer from a single
//! backend, one hardcoded quit binding, focus-follows-mouse.

use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, GestureBeginEvent, GestureEndEvent,
        GesturePinchUpdateEvent as _, GestureSwipeUpdateEvent as _, InputBackend, InputEvent, KeyState,
        KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent, ProximityState, Switch,
        SwitchState, SwitchToggleEvent, TabletToolButtonEvent, TabletToolEvent, TabletToolProximityEvent,
        TabletToolTipEvent, TabletToolTipState, TouchEvent, TouchSlot,
    },
    input::{
        keyboard::{FilterResult, Keysym, ModifiersState},
        pointer::{
            AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
            GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
            GestureSwipeUpdateEvent, MotionEvent,
        },
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, Size, SERIAL_COUNTER},
    wayland::tablet_manager::{TabletDescriptor, TabletSeatTrait},
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
    /// Swap with the neighbour in this direction (tiled windows only; a
    /// floating window is moved with the mouse, see `MouseBind`).
    Move(Direction),
    /// Raise (positive) or lower the focused tiled window's weight in its
    /// Radiant container. A no-op for floating windows and other layouts.
    Priority(i32),
    /// 1-based workspace index.
    SwitchWorkspace(usize),
    /// The workspace after / before the active one on the focused output.
    WorkspaceNext,
    WorkspacePrev,
    /// 1-based workspace index.
    MoveToWorkspace(usize),
    /// Move the focused window to display `number`'s currently active
    /// workspace (ADR 0049). `number` is the compositor-assigned/configured
    /// display number, not a workspace index or an output handle.
    MoveToOutputWorkspace(u8),
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
    AnnotationAutoToggle,
}

/// A configured key binding.
#[derive(Debug, Clone)]
pub struct Bind {
    pub mods: Mods,
    pub key: Keysym,
    pub action: Action,
}

/// A configured touchpad swipe binding (COMP-04 §2): `fingers` moving in
/// `direction` runs `action` instead of reaching the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GestureBind {
    pub fingers: u32,
    pub direction: Direction,
    pub action: Action,
}

/// Mouse button a [`MouseBind`] listens for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

impl MouseButton {
    /// The `linux/input-event-codes.h` code libinput reports for this button.
    pub fn code(self) -> u32 {
        match self {
            MouseButton::Left => BTN_LEFT,
            MouseButton::Right => BTN_RIGHT,
            MouseButton::Middle => BTN_MIDDLE,
        }
    }
}

/// What a held-modifier mouse drag does to the window under the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    MoveWindow,
    ResizeWindow,
}

/// A configured modifier + mouse-button binding (COMP-04 §5, ADR 0057): with
/// exactly `mods` held, pressing `button` over a window starts `action` on it,
/// and the press never reaches the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseBind {
    pub mods: Mods,
    pub button: MouseButton,
    pub action: MouseAction,
}

/// A swipe the compositor has claimed at begin. Only the deltas are kept, so
/// the update path adds two floats and allocates nothing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GestureCapture {
    pub fingers: u32,
    pub dx: f64,
    pub dy: f64,
}

/// Distance, in libinput's touchpad-scaled pointer units, a claimed swipe has
/// to travel along its dominant axis before it counts. Below it the swipe was
/// a hesitation, not a command, and nothing runs.
pub const SWIPE_THRESHOLD: f64 = 100.0;

/// The direction a finished swipe went: whichever axis moved furthest, if it
/// moved at least `threshold`. Screen coordinates, so `dy > 0` is down.
pub fn swipe_direction(dx: f64, dy: f64, threshold: f64) -> Option<Direction> {
    let (ax, ay) = (dx.abs(), dy.abs());
    if ax.max(ay) < threshold {
        return None;
    }
    Some(if ax >= ay {
        if dx < 0.0 {
            Direction::Left
        } else {
            Direction::Right
        }
    } else if dy < 0.0 {
        Direction::Up
    } else {
        Direction::Down
    })
}

/// `linux/input-event-codes.h`. The selector drags with left and cancels with
/// right; every other button is swallowed.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

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
            InputEvent::TouchDown { event } => self.on_touch_down::<B>(event),
            InputEvent::TouchMotion { event } => self.on_touch_motion::<B>(event),
            InputEvent::TouchUp { event } => {
                self.inject_touch_up(slot_id(event.slot()), event.time_msec());
            }
            InputEvent::TouchCancel { .. } => self.on_touch_cancel(),
            InputEvent::TouchFrame { .. } => {
                if let Some(touch) = self.seat.get_touch() {
                    touch.frame(self);
                }
            }
            InputEvent::GestureSwipeBegin { event } => {
                self.on_swipe_begin(event.fingers(), event.time_msec())
            }
            InputEvent::GestureSwipeUpdate { event } => {
                self.on_swipe_update(event.delta(), event.time_msec());
            }
            InputEvent::GestureSwipeEnd { event } => self.on_swipe_end(event.cancelled(), event.time_msec()),
            InputEvent::GesturePinchBegin { event } => {
                self.on_pinch_begin(event.fingers(), event.time_msec())
            }
            InputEvent::GesturePinchUpdate { event } => self.on_pinch_update(&GesturePinchUpdateEvent {
                time: event.time_msec(),
                delta: event.delta(),
                scale: event.scale(),
                rotation: event.rotation(),
            }),
            InputEvent::GesturePinchEnd { event } => self.on_pinch_end(event.cancelled(), event.time_msec()),
            InputEvent::GestureHoldBegin { event } => self.on_hold_begin(event.fingers(), event.time_msec()),
            InputEvent::GestureHoldEnd { event } => self.on_hold_end(event.cancelled(), event.time_msec()),
            InputEvent::TabletToolProximity { event } => self.on_tablet_proximity::<B>(event),
            InputEvent::TabletToolAxis { event } => self.on_tablet_axis::<B>(&event),
            InputEvent::TabletToolTip { event } => self.on_tablet_tip::<B>(event),
            InputEvent::TabletToolButton { event } => self.on_tablet_button::<B>(event),
            InputEvent::DeviceAdded { .. } | InputEvent::DeviceRemoved { .. } | InputEvent::Special(_) => {}
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
            Action::Priority(delta) => shell::adjust_priority(self, delta),
            Action::SwitchWorkspace(n) => shell::switch_workspace(self, n),
            Action::WorkspaceNext => shell::switch_workspace_relative(self, 1),
            Action::WorkspacePrev => shell::switch_workspace_relative(self, -1),
            Action::MoveToWorkspace(n) => shell::move_to_workspace(self, n),
            Action::MoveToOutputWorkspace(n) => shell::move_to_output_workspace(self, n),
            Action::AgentOverride => self.agent_override(),
            Action::AgentAttention => self.agent_attention(),
            Action::Calibrate(step) => {
                crate::outputs::calibrate::apply(self, step);
            }
            Action::RegionSelect(key) => self.region_select_key(key),
            Action::AnnotationSelect => self.region_select_start(),
            Action::AnnotationDismiss => self.emit_keybind("annotation-dismiss"),
            Action::AnnotationExpand => self.emit_keybind("annotation-expand"),
            Action::AnnotationAutoToggle => self.emit_keybind("annotation-auto-toggle"),
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
        if let Some(pos) = self.absolute_to_global(|size| event.position_transformed(size)) {
            self.pointer_moved(pos, event.time_msec());
        }
    }

    /// Map an absolute device position into global coordinates. `local` turns
    /// the chosen output's logical size into an output-local position — the
    /// device's own `position_transformed`. Shared by the absolute pointer,
    /// touch and the tablet tool, so all three pick the same output and get
    /// the same overscan correction.
    fn absolute_to_global(
        &self,
        local: impl FnOnce(Size<i32, Logical>) -> Point<f64, Logical>,
    ) -> Option<Point<f64, Logical>> {
        // Absolute devices are output-relative; use the output the pointer is
        // already on, else the focused one.
        let output = self
            .space
            .output_under(self.pointer_location)
            .next()
            .cloned()
            .or_else(|| self.outputs.focused().map(|e| e.output.clone()));
        let output = output?;
        let geometry = self.space.output_geometry(&output)?;
        // The event lands where the *panel* was touched; with overscan the
        // desktop is painted into an inset rect, so invert that map or the
        // pointer sits off by the margin (COMP-03 §2).
        let overscan = self
            .outputs
            .by_output(&output)
            .map(|e| e.overscan)
            .unwrap_or_default();
        let mut local = local(geometry.size);
        if !overscan.is_zero() {
            let (w, h) = (geometry.size.w.max(1) as f64, geometry.size.h.max(1) as f64);
            let unit = Point::<f64, Logical>::from((local.x / w, local.y / h));
            let unit = overscan.untransform_unit(unit, crate::outputs::mode_size(&output));
            local = (unit.x * w, unit.y * h).into();
        }
        Some(local + geometry.loc.to_f64())
    }

    /// The selector owns the seat (COMP-18 §1.3), so a touch cannot start or
    /// steer anything behind the dim. Up, cancel and frame still pass, so a
    /// point already down when the selection began is released normally.
    fn on_touch_down<B: InputBackend>(&mut self, event: B::TouchDownEvent) {
        if self.region_select.active() {
            return;
        }
        if let Some(pos) = self.absolute_to_global(|size| event.position_transformed(size)) {
            self.inject_touch_down(slot_id(event.slot()), pos, event.time_msec());
        }
    }

    fn on_touch_motion<B: InputBackend>(&mut self, event: B::TouchMotionEvent) {
        if self.region_select.active() {
            return;
        }
        if let Some(pos) = self.absolute_to_global(|size| event.position_transformed(size)) {
            self.inject_touch_motion(slot_id(event.slot()), pos, event.time_msec());
        }
    }

    /// libinput gave up on the sequence (a palm, or another grab took over).
    /// Every point is gone at once, so the down-time bookkeeping goes too.
    pub(crate) fn on_touch_cancel(&mut self) {
        self.touch_points.clear();
        if let Some(touch) = self.seat.get_touch() {
            touch.cancel(self);
        }
    }

    /// Touchpad swipe begin (COMP-04 §2). Who owns the swipe is decided here,
    /// once: a bound finger count is the compositor's from begin to end, and
    /// an unbound one is the app's, so no client ever sees half a gesture. The
    /// region selector claims every swipe, bound or not, for the same reason
    /// it swallows buttons.
    fn on_swipe_begin(&mut self, fingers: u32, time: u32) {
        // Nothing behind the lock sees a gesture, and none runs an action.
        if self.lock.locked {
            self.gesture_capture = None;
            return;
        }
        if self.region_select.active() || self.config.gesture_bound(fingers) {
            self.gesture_capture = Some(GestureCapture {
                fingers,
                dx: 0.0,
                dy: 0.0,
            });
            return;
        }
        self.gesture_capture = None;
        let pointer = self.seat.get_pointer().unwrap();
        pointer.gesture_swipe_begin(
            self,
            &GestureSwipeBeginEvent {
                serial: SERIAL_COUNTER.next_serial(),
                time,
                fingers,
            },
        );
        pointer.frame(self);
    }

    fn on_swipe_update(&mut self, delta: Point<f64, Logical>, time: u32) {
        if let Some(capture) = self.gesture_capture.as_mut() {
            capture.dx += delta.x;
            capture.dy += delta.y;
            return;
        }
        if self.lock.locked || self.region_select.active() {
            return;
        }
        let pointer = self.seat.get_pointer().unwrap();
        pointer.gesture_swipe_update(self, &GestureSwipeUpdateEvent { time, delta });
        pointer.frame(self);
    }

    fn on_swipe_end(&mut self, cancelled: bool, time: u32) {
        if let Some(capture) = self.gesture_capture.take() {
            // Re-checked at end: the lock or a selection may have come up
            // while the fingers were moving.
            if cancelled || self.lock.locked || self.region_select.active() {
                return;
            }
            let action = swipe_direction(capture.dx, capture.dy, SWIPE_THRESHOLD)
                .and_then(|dir| self.config.gesture_for(capture.fingers, dir))
                .cloned();
            if let Some(action) = action {
                self.run_action(action);
            }
            return;
        }
        // An app's swipe always gets its end, even under a selection, so it is
        // never left mid-gesture. Not under the lock: that fails closed.
        if self.lock.locked {
            return;
        }
        let pointer = self.seat.get_pointer().unwrap();
        pointer.gesture_swipe_end(
            self,
            &GestureSwipeEndEvent {
                serial: SERIAL_COUNTER.next_serial(),
                time,
                cancelled,
            },
        );
        pointer.frame(self);
    }

    /// Pinch and hold are never bound (the spec binds swipes only); they go
    /// to the pointer focus, except under the lock or a selection. That is
    /// decided at begin and remembered, like a swipe's claim, so a gesture
    /// dropped at begin has its end dropped too: no client sees an end
    /// without a begin.
    fn gesture_dropped_at_begin(&mut self) -> bool {
        self.gesture_dropped = self.lock.locked || self.region_select.active();
        self.gesture_dropped
    }

    /// The end of a pinch or hold is forwarded only if its begin was, and
    /// never under the lock (which fails closed even mid-gesture).
    fn gesture_end_dropped(&mut self) -> bool {
        std::mem::take(&mut self.gesture_dropped) || self.lock.locked
    }

    fn on_pinch_begin(&mut self, fingers: u32, time: u32) {
        if self.gesture_dropped_at_begin() {
            return;
        }
        let pointer = self.seat.get_pointer().unwrap();
        pointer.gesture_pinch_begin(
            self,
            &GesturePinchBeginEvent {
                serial: SERIAL_COUNTER.next_serial(),
                time,
                fingers,
            },
        );
        pointer.frame(self);
    }

    fn on_pinch_update(&mut self, event: &GesturePinchUpdateEvent) {
        if self.gesture_dropped || self.lock.locked || self.region_select.active() {
            return;
        }
        let pointer = self.seat.get_pointer().unwrap();
        pointer.gesture_pinch_update(self, event);
        pointer.frame(self);
    }

    fn on_pinch_end(&mut self, cancelled: bool, time: u32) {
        if self.gesture_end_dropped() {
            return;
        }
        let pointer = self.seat.get_pointer().unwrap();
        pointer.gesture_pinch_end(
            self,
            &GesturePinchEndEvent {
                serial: SERIAL_COUNTER.next_serial(),
                time,
                cancelled,
            },
        );
        pointer.frame(self);
    }

    fn on_hold_begin(&mut self, fingers: u32, time: u32) {
        if self.gesture_dropped_at_begin() {
            return;
        }
        let pointer = self.seat.get_pointer().unwrap();
        pointer.gesture_hold_begin(
            self,
            &GestureHoldBeginEvent {
                serial: SERIAL_COUNTER.next_serial(),
                time,
                fingers,
            },
        );
        pointer.frame(self);
    }

    fn on_hold_end(&mut self, cancelled: bool, time: u32) {
        if self.gesture_end_dropped() {
            return;
        }
        let pointer = self.seat.get_pointer().unwrap();
        pointer.gesture_hold_end(
            self,
            &GestureHoldEndEvent {
                serial: SERIAL_COUNTER.next_serial(),
                time,
                cancelled,
            },
        );
        pointer.frame(self);
    }

    /// The surface a tablet tool at `pos` talks to. `None` under the lock or
    /// a selection, which smithay turns into a proximity-out: the tool fails
    /// closed exactly like the pointer.
    fn tablet_focus(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        if self.lock.locked || self.region_select.active() {
            return None;
        }
        self.surface_under(pos)
    }

    /// Tool in or out of proximity (COMP-06 §1). The tool is added to the
    /// tablet seat on first sight; smithay keeps it, so later proximity-ins
    /// find the existing handle. The tablet is resolved here, once, and kept
    /// for the axis events that follow.
    fn on_tablet_proximity<B: InputBackend>(&mut self, event: B::TabletToolProximityEvent) {
        let tablet_seat = self.seat.tablet_seat();
        let time = event.time_msec();
        if event.state() == ProximityState::Out {
            if let Some(tool) = tablet_seat.get_tool(&event.tool()) {
                tool.proximity_out(time);
            }
            self.tablet_in_use = None;
            return;
        }
        let dh = self.display_handle.clone();
        tablet_seat.add_tool::<AbyssState>(self, &dh, &event.tool());
        self.tablet_in_use = tablet_seat.get_tablet(&TabletDescriptor::from(&event.device()));
        // Proximity-in carries a position; deliver it as the first motion,
        // which is also what sends `proximity_in` to the surface under it.
        self.on_tablet_axis::<B>(&event);
    }

    /// Motion plus whichever axes changed. The cursor follows the stylus: it
    /// is compositor-drawn, and the pointer path is what moves it (and applies
    /// the lock and the selector to it).
    fn on_tablet_axis<B: InputBackend>(&mut self, event: &impl TabletToolEvent<B>) {
        let Some(pos) = self.absolute_to_global(|size| event.position_transformed(size)) else {
            return;
        };
        let time = event.time_msec();
        self.pointer_moved(pos, time);
        let pos = self.pointer_location;
        let Some(tablet) = self.tablet_in_use.clone() else {
            return;
        };
        let Some(tool) = self.seat.tablet_seat().get_tool(&event.tool()) else {
            return;
        };
        if event.pressure_has_changed() {
            tool.pressure(event.pressure());
        }
        if event.distance_has_changed() {
            tool.distance(event.distance());
        }
        if event.tilt_has_changed() {
            tool.tilt(event.tilt());
        }
        if event.slider_has_changed() {
            tool.slider_position(event.slider_position());
        }
        if event.rotation_has_changed() {
            tool.rotation(event.rotation());
        }
        if event.wheel_has_changed() {
            tool.wheel(event.wheel_delta(), event.wheel_delta_discrete());
        }
        let focus = self.tablet_focus(pos);
        tool.motion(pos, focus, &tablet, SERIAL_COUNTER.next_serial(), time);
    }

    /// Tip down is the stylus's click: it focuses and raises what it lands on,
    /// through the same focus path as the pointer.
    fn on_tablet_tip<B: InputBackend>(&mut self, event: B::TabletToolTipEvent) {
        // The tool's focus may predate the lock or selection if it has not
        // moved since; refuse rather than deliver to it.
        if self.lock.locked || self.region_select.active() {
            return;
        }
        let Some(tool) = self.seat.tablet_seat().get_tool(&event.tool()) else {
            return;
        };
        match event.tip_state() {
            TabletToolTipState::Down => {
                let pos = self.pointer_location;
                let action = crate::shell::focus::decide_pointer_focus(
                    &crate::shell::focus::pointer_focus_ctx(self, pos),
                );
                crate::shell::focus::apply_focus(self, action, crate::shell::focus::FocusCause::Click);
                tool.tip_down(SERIAL_COUNTER.next_serial(), event.time_msec());
            }
            TabletToolTipState::Up => tool.tip_up(event.time_msec()),
        }
    }

    fn on_tablet_button<B: InputBackend>(&mut self, event: B::TabletToolButtonEvent) {
        if self.lock.locked || self.region_select.active() {
            return;
        }
        if let Some(tool) = self.seat.tablet_seat().get_tool(&event.tool()) {
            tool.button(
                event.button(),
                event.button_state(),
                SERIAL_COUNTER.next_serial(),
                event.time_msec(),
            );
        }
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
        // A `mousebind` press (Alt+drag by default) starts a compositor-owned
        // move/resize. The grab has to be in place *before* the seat sees the
        // press: it clears pointer focus, so the press goes into the grab and
        // the client under the pointer never receives it (ADR 0057). Whether it
        // fired or not, the press carries on to the seat and click-to-focus.
        if pressed {
            grabs::start_mouse_bind(self, event.button_code());
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

/// A libinput slot as the id `wl_touch` carries. A single-touch device has
/// no slot at all; it is the only point, so it is point 0.
fn slot_id(slot: TouchSlot) -> u32 {
    u32::try_from(i32::from(slot)).unwrap_or(0)
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

/// The swipe path end to end, through `process_input_event` on a live state:
/// a fake backend stands in for libinput, which is the only thing that can
/// produce a real swipe.
#[cfg(test)]
mod gesture_tests {
    use smithay::backend::input::{
        Device, DeviceCapability, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
        GesturePinchEndEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent,
        UnusedEvent,
    };

    use super::*;
    use crate::shell::focus::state_tests::harness;

    struct Fake;

    #[derive(PartialEq, Eq, Hash)]
    struct Pad;

    impl Device for Pad {
        fn id(&self) -> String {
            "pad".into()
        }
        fn name(&self) -> String {
            "pad".into()
        }
        fn has_capability(&self, _: DeviceCapability) -> bool {
            false
        }
        fn usb_id(&self) -> Option<(u32, u32)> {
            None
        }
        fn syspath(&self) -> Option<std::path::PathBuf> {
            None
        }
    }

    struct Begin(u32);
    struct Update(f64, f64);
    struct End(bool);

    macro_rules! event {
        ($($t:ty),*) => {$(
            impl Event<Fake> for $t {
                fn time(&self) -> u64 {
                    0
                }
                fn device(&self) -> Pad {
                    Pad
                }
            }
        )*};
    }
    event!(Begin, Update, End);

    impl GestureBeginEvent<Fake> for Begin {
        fn fingers(&self) -> u32 {
            self.0
        }
    }
    impl GestureSwipeBeginEvent<Fake> for Begin {}
    impl GestureSwipeUpdateEvent<Fake> for Update {
        fn delta_x(&self) -> f64 {
            self.0
        }
        fn delta_y(&self) -> f64 {
            self.1
        }
    }
    impl GestureEndEvent<Fake> for End {
        fn cancelled(&self) -> bool {
            self.0
        }
    }
    impl GestureSwipeEndEvent<Fake> for End {}
    impl GesturePinchBeginEvent<Fake> for Begin {}
    impl GesturePinchEndEvent<Fake> for End {}
    impl GestureHoldBeginEvent<Fake> for Begin {}
    impl GestureHoldEndEvent<Fake> for End {}

    impl InputBackend for Fake {
        type Device = Pad;
        type KeyboardKeyEvent = UnusedEvent;
        type PointerAxisEvent = UnusedEvent;
        type PointerButtonEvent = UnusedEvent;
        type PointerMotionEvent = UnusedEvent;
        type PointerMotionAbsoluteEvent = UnusedEvent;
        type GestureSwipeBeginEvent = Begin;
        type GestureSwipeUpdateEvent = Update;
        type GestureSwipeEndEvent = End;
        type GesturePinchBeginEvent = Begin;
        type GesturePinchUpdateEvent = UnusedEvent;
        type GesturePinchEndEvent = End;
        type GestureHoldBeginEvent = Begin;
        type GestureHoldEndEvent = End;
        type TouchDownEvent = UnusedEvent;
        type TouchUpEvent = UnusedEvent;
        type TouchMotionEvent = UnusedEvent;
        type TouchCancelEvent = UnusedEvent;
        type TouchFrameEvent = UnusedEvent;
        type TabletToolAxisEvent = UnusedEvent;
        type TabletToolProximityEvent = UnusedEvent;
        type TabletToolTipEvent = UnusedEvent;
        type TabletToolButtonEvent = UnusedEvent;
        type SwitchToggleEvent = UnusedEvent;
        type SpecialEvent = ();
    }

    /// Two updates, so the capture has to accumulate rather than keep the last.
    fn swipe(state: &mut AbyssState, fingers: u32, dx: f64, cancelled: bool) {
        state.process_input_event::<Fake>(InputEvent::GestureSwipeBegin {
            event: Begin(fingers),
        });
        state.process_input_event::<Fake>(InputEvent::GestureSwipeUpdate {
            event: Update(dx / 2.0, 0.0),
        });
        state.process_input_event::<Fake>(InputEvent::GestureSwipeUpdate {
            event: Update(dx / 2.0, 0.0),
        });
        state.process_input_event::<Fake>(InputEvent::GestureSwipeEnd {
            event: End(cancelled),
        });
    }

    fn active(state: &AbyssState) -> usize {
        state.outputs.focused().expect("focused output").active
    }

    #[test]
    fn a_bound_swipe_switches_workspace_and_is_never_forwarded() {
        let mut h = harness();
        let s = &mut h.state;
        let far = SWIPE_THRESHOLD * 1.5;
        assert_eq!(active(s), 0);

        // Fingers left: the next workspace. Right: back.
        swipe(s, 3, -far, false);
        assert_eq!(active(s), 1);
        swipe(s, 3, far, false);
        assert_eq!(active(s), 0);
        // Clamped at the first workspace, not wrapped to the last.
        swipe(s, 3, far, false);
        assert_eq!(active(s), 0);

        // Short of the threshold, or cancelled: nothing runs.
        swipe(s, 3, -(SWIPE_THRESHOLD - 1.0), false);
        swipe(s, 3, -far, true);
        assert_eq!(active(s), 0);

        // The capture is taken at begin and cleared at end.
        s.process_input_event::<Fake>(InputEvent::GestureSwipeBegin { event: Begin(3) });
        assert!(s.gesture_capture.is_some());
        s.process_input_event::<Fake>(InputEvent::GestureSwipeEnd { event: End(false) });
        assert!(s.gesture_capture.is_none());

        // An unbound finger count belongs to the app.
        s.process_input_event::<Fake>(InputEvent::GestureSwipeBegin { event: Begin(4) });
        assert!(s.gesture_capture.is_none());
        s.process_input_event::<Fake>(InputEvent::GestureSwipeEnd { event: End(false) });
        assert_eq!(active(s), 0);
    }

    #[test]
    fn no_swipe_acts_under_the_lock() {
        let mut h = harness();
        let s = &mut h.state;
        s.lock.locked = true;
        swipe(s, 3, -SWIPE_THRESHOLD * 2.0, false);
        assert_eq!(active(s), 0);
        assert!(s.gesture_capture.is_none());

        // The lock coming up mid-swipe cancels the action too.
        s.lock.locked = false;
        s.process_input_event::<Fake>(InputEvent::GestureSwipeBegin { event: Begin(3) });
        s.process_input_event::<Fake>(InputEvent::GestureSwipeUpdate {
            event: Update(-SWIPE_THRESHOLD * 2.0, 0.0),
        });
        s.lock.locked = true;
        s.process_input_event::<Fake>(InputEvent::GestureSwipeEnd { event: End(false) });
        assert_eq!(active(s), 0);
    }

    /// A pinch or hold dropped at begin has its end dropped too, even if the
    /// selection that dropped it is gone by then: no end without a begin.
    #[test]
    fn a_pinch_or_hold_dropped_at_begin_drops_its_end() {
        let mut h = harness();
        let s = &mut h.state;
        for hold in [false, true] {
            s.region_select.start((0, 0).into());
            if hold {
                s.process_input_event::<Fake>(InputEvent::GestureHoldBegin { event: Begin(2) });
            } else {
                s.process_input_event::<Fake>(InputEvent::GesturePinchBegin { event: Begin(2) });
            }
            assert!(s.gesture_dropped);
            s.region_select.cancel();
            assert!(s.gesture_end_dropped(), "the matching end is dropped");
            assert!(!s.gesture_dropped, "and the claim is spent");
            // The next one, begun with nothing in the way, is forwarded whole.
            s.process_input_event::<Fake>(InputEvent::GesturePinchBegin { event: Begin(2) });
            assert!(!s.gesture_dropped);
            s.process_input_event::<Fake>(InputEvent::GesturePinchEnd { event: End(false) });
        }
    }

    /// A finger down before the lock must not keep driving the app behind
    /// it: locking cancels the touch sequence, which releases smithay's grab
    /// holding the point to its down-time surface.
    #[test]
    fn locking_cancels_a_touch_already_down() {
        let mut h = harness();
        let s = &mut h.state;
        let at = s.pointer_location;
        s.inject_touch_down(0, at, 0);
        let touch = s.seat.get_touch().expect("touch capability");
        assert!(touch.is_grabbed(), "a point down holds the touch grab");
        s.engage_lock();
        assert!(!touch.is_grabbed(), "the lock cancelled the sequence");
        assert!(s.touch_points.is_empty());
        // Motion after the lock reaches nothing: no grab, and focus is `None`.
        s.inject_touch_motion(0, at, 1);
        assert!(!touch.is_grabbed());
    }
}
