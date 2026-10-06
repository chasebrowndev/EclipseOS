// SPDX-License-Identifier: AGPL-3.0-only
//! `eclipse_agent_seat_v1`: one virtual seat per agent (COMP-08 §4,
//! COMP-04 §3). Not TCB, and decides nothing: every request goes through
//! `policy::enforce::submit`, and this module only executes what that allows
//! and carries the answer back.
//!
//! # Shape
//!
//! - **Creation.** `eclipse_agent_v1.get_seat` makes the agent's seat the
//!   first time. A second `get_seat` on the same agent object is the
//!   protocol error `seat_exists` (one seat per agent is the model, so a
//!   second request is a client bug, not something to paper over).
//! - **The seat.** A smithay `Seat` named `agent-<id>` with its own
//!   keyboard, pointer and touch, registered on the *main* display so the
//!   clients it acts on receive its events through their own `wl_seat`
//!   binding. It lives in `AbyssState::agent_seats`, keyed by the agent's
//!   `u64` handle; nothing here is shared or locked.
//! - **Isolation.** Nothing in `input/` touches an agent seat and nothing
//!   here touches `AbyssState::seat`. The agent seat carries no compositor
//!   bindings (COMP-04 §5): keys go through a filter that always forwards.
//!   Agent focus is its own: it never calls `shell::focus_window`, so the
//!   human's focus, workspace and output focus never move.
//! - **Reach.** Whatever the policy decision, an act is only ever *delivered*
//!   to the window it was decided on: the pointer's focus is a surface of
//!   that window or nothing, so a window the agent could not see is not
//!   clickable by walking the pointer onto it.
//! - **Teardown.** [`remove`] releases every held key and button, leaves the
//!   focus, strips the seat's capabilities and removes its global.
//!
//! X11 windows are refused as agent focus targets: the X input focus is one
//! global thing, so focusing one from an agent seat would move the human's.
//! That is the COMP-07 X11 posture work (M13 `seat_compat=lock`), not here.

use std::io::Write;
use std::os::fd::{AsFd, FromRawFd};
use std::time::Instant;

use ec_protocols::agent::server::eclipse_agent_seat_v1::{self, EclipseAgentSeatV1};
use ec_protocols::agent::server::eclipse_agent_v1::Status;
use smithay::backend::input::{Axis, AxisSource, ButtonState, KeyState, TouchSlot};
use smithay::desktop::Window;
use smithay::input::keyboard::{FilterResult, KeyboardHandle, Keycode, Keysym};
use smithay::input::pointer::{AxisFrame, ButtonEvent, MotionEvent, PointerHandle, RelativeMotionEvent};
use smithay::input::touch::{DownEvent, MotionEvent as TouchMotionEvent, TouchHandle, UpEvent};
use smithay::input::Seat;
use smithay::reexports::wayland_server::{
    backend::ClientId, protocol::wl_surface::WlSurface, Client, DataInit, Dispatch, DisplayHandle, New,
    Resource,
};
use smithay::utils::{IsAlive, Logical, Point, SERIAL_COUNTER};
use smithay::wayland::text_input::TextInputSeat;

use super::AgentId;
use crate::policy::enforce::{self, Act, Request, Submitted};
use crate::protocols::standard::seat::KeyboardFocusTarget;
use crate::state::AbyssState;

/// evdev to xkb keycode offset.
const EVDEV_OFFSET: u32 = 8;
/// The highest evdev code a key request may name (`KEY_MAX`).
const KEY_MAX: u32 = 0x2ff;
/// `Shift_L`, `Return` and `Tab`.
const SHIFT_L: u32 = 0xffe1;
const RETURN: u32 = 0xff0d;
const TAB: u32 = 0xff09;

/// One agent's seat and what the compositor remembers about it.
pub struct AgentSeat {
    /// The `eclipse_agent_seat_v1` object, to answer on.
    resource: EclipseAgentSeatV1,
    seat: Seat<AbyssState>,
    keyboard: KeyboardHandle<AbyssState>,
    pointer: PointerHandle<AbyssState>,
    touch: TouchHandle<AbyssState>,
    /// The window this seat's keyboard focus is on.
    focus: Option<Window>,
    /// The window this seat's pointer last landed on.
    pointer_window: Option<Window>,
    /// Global logical coordinates.
    pointer_pos: Point<f64, Logical>,
    /// Buttons held, so teardown can release them.
    buttons: Vec<u32>,
}

impl AgentSeat {
    /// How many keys the seat holds down.
    #[cfg(test)]
    pub(crate) fn keyboard_pressed(&self) -> usize {
        self.keyboard.pressed_keys().len()
    }
}

impl std::fmt::Debug for AgentSeat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentSeat")
            .field("seat", &self.seat.name())
            .finish_non_exhaustive()
    }
}

/// Cheap clones of one seat's handles, taken so the state can be borrowed
/// mutably while events are delivered.
struct Handles {
    keyboard: KeyboardHandle<AbyssState>,
    pointer: PointerHandle<AbyssState>,
    touch: TouchHandle<AbyssState>,
    seat: Seat<AbyssState>,
    pointer_pos: Point<f64, Logical>,
}

fn entry(state: &AbyssState, agent: u64) -> Option<&AgentSeat> {
    state
        .agent_seats
        .iter()
        .find(|(i, _)| *i == agent)
        .map(|(_, s)| s)
}

fn entry_mut(state: &mut AbyssState, agent: u64) -> Option<&mut AgentSeat> {
    state
        .agent_seats
        .iter_mut()
        .find(|(i, _)| *i == agent)
        .map(|(_, s)| s)
}

fn handles(state: &AbyssState, agent: u64) -> Option<Handles> {
    entry(state, agent).map(|s| Handles {
        keyboard: s.keyboard.clone(),
        pointer: s.pointer.clone(),
        touch: s.touch.clone(),
        seat: s.seat.clone(),
        pointer_pos: s.pointer_pos,
    })
}

fn now_ms(state: &AbyssState) -> u32 {
    state.start_time.elapsed().as_millis() as u32
}

// ------------------------------------------------------------ entry points

/// The window agent `agent`'s seat has focus on: its keyboard focus, or, with
/// none, the window its pointer last landed on. A window that has since gone
/// is no focus at all.
pub fn focused_window(state: &AbyssState, agent: u64) -> Option<Window> {
    let s = entry(state, agent)?;
    s.focus
        .iter()
        .chain(s.pointer_window.iter())
        .find(|w| w.alive())
        .cloned()
}

/// Run an allowed act on `window`. `Ok` carries the result detail (the text
/// path taken, for `text`).
pub fn execute(
    state: &mut AbyssState,
    agent: u64,
    act: &Act,
    window: &Window,
) -> Result<String, (Status, String)> {
    let Some(h) = handles(state, agent) else {
        return Err((Status::ClientGone, String::new()));
    };
    if !window.alive() {
        return Err((Status::FocusLost, String::new()));
    }
    // No surface behind the lock screen sees agent input either.
    if state.lock.locked {
        return Err((Status::FocusLost, "locked".into()));
    }
    match *act {
        Act::Focus { .. } => {
            focus(state, agent, &h, window)?;
            Ok(String::new())
        }
        Act::Key { keycode, pressed, .. } => {
            let code = evdev(keycode)?;
            key(state, &h, code, pressed);
            Ok(String::new())
        }
        Act::Keysym { keysym, pressed } => {
            let seq = find_key(state, &h.keyboard, keysym).ok_or_else(|| invalid("keysym"))?;
            if pressed {
                seq.press(state, &h.keyboard);
            } else {
                seq.release(state, &h.keyboard);
            }
            Ok(String::new())
        }
        Act::Text { ref text, .. } => type_text(state, agent, &h, window, text),
        Act::PointerAbs { x, y } => {
            let pos = Point::from((f64::from(x), f64::from(y)));
            pointer_to(state, agent, &h, window, pos);
            Ok(String::new())
        }
        Act::PointerRel { dx, dy } => {
            let pos = clamp_to(state, window, h.pointer_pos + Point::from((dx, dy)));
            let under = target_under(state, window, pos);
            h.pointer.relative_motion(
                state,
                under.clone(),
                &RelativeMotionEvent {
                    delta: (dx, dy).into(),
                    delta_unaccel: (dx, dy).into(),
                    utime: u64::from(now_ms(state)) * 1000,
                },
            );
            pointer_to(state, agent, &h, window, pos);
            Ok(String::new())
        }
        Act::Button { button, pressed } => {
            button_event(state, agent, &h, window, button, pressed);
            Ok(String::new())
        }
        Act::Axis {
            axis,
            value,
            discrete,
            source,
        } => {
            let axis = match axis {
                0 => Axis::Vertical,
                1 => Axis::Horizontal,
                _ => return Err(invalid("axis")),
            };
            let source = match source {
                0 => AxisSource::Wheel,
                1 => AxisSource::Finger,
                2 => AxisSource::Continuous,
                3 => AxisSource::WheelTilt,
                _ => return Err(invalid("source")),
            };
            sync_pointer(state, agent, &h, window);
            let mut frame = AxisFrame::new(now_ms(state)).source(source).value(axis, value);
            if discrete != 0 {
                frame = frame.v120(axis, discrete.saturating_mul(120));
            }
            h.pointer.axis(state, frame);
            h.pointer.frame(state);
            Ok(String::new())
        }
        Act::TouchDown { id, x, y } => {
            let slot = slot(id)?;
            let pos = Point::from((f64::from(x), f64::from(y)));
            let under = target_under(state, window, pos);
            if under.is_none() {
                return Err((Status::OutOfScope, "obscured".into()));
            }
            let time = now_ms(state);
            h.touch.down(
                state,
                under,
                &DownEvent {
                    slot,
                    location: pos,
                    serial: SERIAL_COUNTER.next_serial(),
                    time,
                },
            );
            h.touch.frame(state);
            if let Some(s) = entry_mut(state, agent) {
                s.pointer_window = Some(window.clone());
            }
            Ok(String::new())
        }
        Act::TouchMotion { id, x, y } => {
            let slot = slot(id)?;
            let pos = Point::from((f64::from(x), f64::from(y)));
            let under = target_under(state, window, pos);
            let time = now_ms(state);
            h.touch.motion(
                state,
                under,
                &TouchMotionEvent {
                    slot,
                    location: pos,
                    time,
                },
            );
            h.touch.frame(state);
            Ok(String::new())
        }
        Act::TouchUp { id } => {
            // Lifting a point is always safe, whatever it landed on.
            let slot = slot(id)?;
            let time = now_ms(state);
            h.touch.up(
                state,
                &UpEvent {
                    slot,
                    serial: SERIAL_COUNTER.next_serial(),
                    time,
                },
            );
            h.touch.frame(state);
            Ok(String::new())
        }
        Act::Click {
            handle, x, y, button, ..
        } => {
            let pos = if handle == 0 {
                Point::from((f64::from(x), f64::from(y)))
            } else {
                let g = state.space.element_geometry(window).unwrap_or_default();
                Point::from((
                    f64::from(g.loc.x) + f64::from(g.size.w) / 2.0,
                    f64::from(g.loc.y) + f64::from(g.size.h) / 2.0,
                ))
            };
            // Nothing is mutated unless the click can land.
            if target_under(state, window, pos).is_none() {
                return Err((Status::OutOfScope, "obscured".into()));
            }
            focus(state, agent, &h, window)?;
            pointer_to(state, agent, &h, window, pos);
            button_event(state, agent, &h, window, button, true);
            button_event(state, agent, &h, window, button, false);
            Ok(String::new())
        }
    }
}

/// Send a delayed `result` for `req_id` on agent `agent`'s seat object.
pub fn reply(state: &mut AbyssState, agent: u64, req_id: u32, status: Status, detail: &str) {
    send_result(state, agent, req_id, status, detail, 0);
}

fn send_result(state: &AbyssState, agent: u64, req_id: u32, status: Status, detail: &str, latency_us: u32) {
    let Some(s) = entry(state, agent) else {
        return;
    };
    if !s.resource.is_alive() {
        return;
    }
    let focus = s
        .focus
        .as_ref()
        .and_then(|w| state.ipc.existing_handle(w))
        .and_then(|h| u32::try_from(h).ok())
        .unwrap_or(0);
    s.resource
        .result(req_id, status as u32, detail.to_owned(), focus, 0, latency_us);
}

/// Seat teardown (COMP-04 §3): release every key and button the seat holds
/// so nothing sticks, leave the focus, drop the capabilities and the global.
/// Safe to call for an agent with no seat.
pub(super) fn remove(state: &mut AbyssState, agent: u64) {
    let Some(i) = state.agent_seats.iter().position(|(a, _)| *a == agent) else {
        return;
    };
    let (_, mut s) = state.agent_seats.remove(i);
    let time = now_ms(state);
    for code in s.keyboard.pressed_keys() {
        s.keyboard.input::<(), _>(
            state,
            code,
            KeyState::Released,
            SERIAL_COUNTER.next_serial(),
            time,
            |_, _, _| FilterResult::Forward,
        );
    }
    for button in s.buttons.drain(..) {
        s.pointer.button(
            state,
            &ButtonEvent {
                serial: SERIAL_COUNTER.next_serial(),
                time,
                button,
                state: ButtonState::Released,
            },
        );
    }
    s.pointer.motion(
        state,
        None,
        &MotionEvent {
            location: s.pointer_pos,
            serial: SERIAL_COUNTER.next_serial(),
            time,
        },
    );
    s.pointer.frame(state);
    s.touch.cancel(state);
    s.keyboard.set_focus(state, None, SERIAL_COUNTER.next_serial());
    // Clients are told the seat has no capabilities, then the global goes.
    s.seat.remove_keyboard();
    s.seat.remove_pointer();
    s.seat.remove_touch();
    if let Some(g) = s.seat.global() {
        state.display_handle.remove_global::<AbyssState>(g);
    }
    tracing::info!(agent, "agent seat removed");
}

// ------------------------------------------------------------ delivery

fn invalid(detail: &str) -> (Status, String) {
    (Status::InvalidArgument, detail.to_owned())
}

fn evdev(code: u32) -> Result<Keycode, (Status, String)> {
    if code > KEY_MAX {
        return Err(invalid("keycode"));
    }
    Ok(Keycode::new(code + EVDEV_OFFSET))
}

fn slot(id: i32) -> Result<TouchSlot, (Status, String)> {
    u32::try_from(id)
        .map(|id| TouchSlot::from(Some(id)))
        .map_err(|_| invalid("touch_id"))
}

/// One key event on the agent's keyboard. No filter intercepts: agent seats
/// hold no compositor bindings (COMP-04 §5).
fn key(state: &mut AbyssState, h: &Handles, code: Keycode, pressed: bool) {
    let time = now_ms(state);
    h.keyboard.input::<(), _>(
        state,
        code,
        if pressed {
            KeyState::Pressed
        } else {
            KeyState::Released
        },
        SERIAL_COUNTER.next_serial(),
        time,
        |_, _, _| FilterResult::Forward,
    );
}

/// Keyboard focus on `window`, on this seat only.
fn focus(state: &mut AbyssState, agent: u64, h: &Handles, window: &Window) -> Result<(), (Status, String)> {
    if window.x11_surface().is_some() {
        return Err(invalid("x11"));
    }
    let Some(surface) = crate::shell::window_surface(window) else {
        return Err((Status::FocusLost, String::new()));
    };
    h.keyboard.set_focus(
        state,
        Some(KeyboardFocusTarget::Wl(surface)),
        SERIAL_COUNTER.next_serial(),
    );
    if let Some(s) = entry_mut(state, agent) {
        s.focus = Some(window.clone());
    }
    let handle = state
        .ipc
        .existing_handle(window)
        .and_then(|h| u32::try_from(h).ok())
        .unwrap_or(0);
    if let Some(s) = entry(state, agent) {
        s.resource.focus_changed(handle, 0);
    }
    Ok(())
}

/// The surface of `window` under `pos`, and its global origin, but only when
/// `window` is what is really topmost there: a layer surface, a lock or any
/// other window above it means the agent's pointer would be hitting that
/// instead, and so it hits nothing.
fn target_under(
    state: &AbyssState,
    window: &Window,
    pos: Point<f64, Logical>,
) -> Option<(WlSurface, Point<f64, Logical>)> {
    if crate::shell::layer_at(state, pos).is_some() {
        return None;
    }
    match state.space.element_under(pos) {
        Some((w, _)) if w == window => crate::shell::surface_under(state, pos),
        _ => None,
    }
}

/// `pos` pulled inside `window`'s geometry: relative motion cannot walk the
/// agent's pointer out of the window it was allowed to act on.
fn clamp_to(state: &AbyssState, window: &Window, pos: Point<f64, Logical>) -> Point<f64, Logical> {
    let Some(g) = state.space.element_geometry(window) else {
        return pos;
    };
    let (x0, y0) = (f64::from(g.loc.x), f64::from(g.loc.y));
    let (x1, y1) = (
        x0 + f64::from((g.size.w - 1).max(0)),
        y0 + f64::from((g.size.h - 1).max(0)),
    );
    (pos.x.clamp(x0, x1), pos.y.clamp(y0, y1)).into()
}

/// Move the agent's pointer to `pos`, focused on `window`'s surface there.
fn pointer_to(state: &mut AbyssState, agent: u64, h: &Handles, window: &Window, pos: Point<f64, Logical>) {
    let under = target_under(state, window, pos);
    let time = now_ms(state);
    h.pointer.motion(
        state,
        under,
        &MotionEvent {
            location: pos,
            serial: SERIAL_COUNTER.next_serial(),
            time,
        },
    );
    h.pointer.frame(state);
    if let Some(s) = entry_mut(state, agent) {
        s.pointer_pos = pos;
        s.pointer_window = Some(window.clone());
    }
}

/// Make the pointer's focus what `window` really shows under it now, so a
/// press or scroll cannot land on whatever the pointer last entered after
/// the scene moved.
fn sync_pointer(state: &mut AbyssState, agent: u64, h: &Handles, window: &Window) {
    let under = target_under(state, window, h.pointer_pos);
    let current = h.pointer.current_focus();
    if current.as_ref() != under.as_ref().map(|(s, _)| s) {
        pointer_to(state, agent, h, window, h.pointer_pos);
    }
}

fn button_event(
    state: &mut AbyssState,
    agent: u64,
    h: &Handles,
    window: &Window,
    button: u32,
    pressed: bool,
) {
    // A release goes wherever the press went (the implicit grab); only a
    // press has to be steered.
    if pressed {
        sync_pointer(state, agent, h, window);
    }
    let time = now_ms(state);
    h.pointer.button(
        state,
        &ButtonEvent {
            serial: SERIAL_COUNTER.next_serial(),
            time,
            button,
            state: if pressed {
                ButtonState::Pressed
            } else {
                ButtonState::Released
            },
        },
    );
    h.pointer.frame(state);
    if let Some(s) = entry_mut(state, agent) {
        if pressed {
            if !s.buttons.contains(&button) {
                s.buttons.push(button);
            }
        } else {
            s.buttons.retain(|b| *b != button);
        }
    }
}

// ------------------------------------------------------------ keysyms

/// The keycodes that produce one keysym: the key, and Shift if the symbol
/// sits on the second level.
#[derive(Debug, Clone, Copy, PartialEq)]
struct KeySeq {
    shift: Option<Keycode>,
    key: Keycode,
}

impl KeySeq {
    fn press(self, state: &mut AbyssState, kbd: &KeyboardHandle<AbyssState>) {
        for (code, pressed) in self
            .shift
            .map(|s| (s, true))
            .into_iter()
            .chain([(self.key, true)])
        {
            send_key(state, kbd, code, pressed);
        }
    }

    fn release(self, state: &mut AbyssState, kbd: &KeyboardHandle<AbyssState>) {
        for (code, pressed) in [(self.key, false)]
            .into_iter()
            .chain(self.shift.map(|s| (s, false)))
        {
            send_key(state, kbd, code, pressed);
        }
    }
}

fn send_key(state: &mut AbyssState, kbd: &KeyboardHandle<AbyssState>, code: Keycode, pressed: bool) {
    let time = now_ms(state);
    kbd.input::<(), _>(
        state,
        code,
        if pressed {
            KeyState::Pressed
        } else {
            KeyState::Released
        },
        SERIAL_COUNTER.next_serial(),
        time,
        |_, _, _| FilterResult::Forward,
    );
}

/// Find the key that types `sym` in the seat's active layout, at level one
/// (bare) or two (Shift).
fn find_key(state: &mut AbyssState, kbd: &KeyboardHandle<AbyssState>, sym: u32) -> Option<KeySeq> {
    kbd.with_xkb_state(state, |ctx| {
        let xkb = ctx.xkb().lock().ok()?;
        // SAFETY: the keymap reference does not outlive the guard.
        let keymap = unsafe { xkb.keymap() };
        let active = xkb.active_layout().0;
        let target = Keysym::new(sym);
        let shift = Keysym::new(SHIFT_L);
        let mut shift_key = None;
        let mut best: Option<(Keycode, u32)> = None;
        for raw in keymap.min_keycode().raw()..=keymap.max_keycode().raw() {
            let code = Keycode::new(raw);
            let layouts = keymap.num_layouts_for_key(code);
            if layouts == 0 {
                continue;
            }
            let layout = active.min(layouts - 1);
            for level in 0..keymap.num_levels_for_key(code, layout).min(2) {
                let syms = keymap.key_get_syms_by_level(code, layout, level);
                if level == 0 && shift_key.is_none() && syms == [shift] {
                    shift_key = Some(code);
                }
                if syms.contains(&target) && best.is_none_or(|(_, l)| level < l) {
                    best = Some((code, level));
                }
            }
        }
        let (key, level) = best?;
        match level {
            0 => Some(KeySeq { shift: None, key }),
            _ => Some(KeySeq {
                shift: Some(shift_key?),
                key,
            }),
        }
    })
}

/// The keysym a character types, if it has one the seat can produce.
fn keysym_of(c: char) -> Option<u32> {
    match c {
        '\n' => Some(RETURN),
        '\t' => Some(TAB),
        c if c.is_control() => None,
        c => Some(Keysym::from_char(c).raw()),
    }
}

/// COMP-04 §3 `text`: a `text_input_v3` commit when the focused client has
/// an enabled field on this seat, else the string typed as keysyms, else
/// refused. The string is never logged.
fn type_text(
    state: &mut AbyssState,
    agent: u64,
    h: &Handles,
    window: &Window,
    text: &str,
) -> Result<String, (Status, String)> {
    // The text goes to the window the seat has keyboard focus on, so it
    // cannot reach a field the agent did not put focus in.
    let focused = entry(state, agent).and_then(|s| s.focus.clone());
    if focused.as_ref() != Some(window) {
        return Err((Status::FocusLost, String::new()));
    }
    let th = h.seat.text_input().clone();
    let mut committed = false;
    th.with_active_text_input(|ti, _| {
        ti.commit_string(Some(text.to_owned()));
        committed = true;
    });
    if committed {
        th.done(false);
        return Ok("text_input".into());
    }
    // Resolve every key before sending any, so a string that cannot be typed
    // types nothing.
    let mut seqs = Vec::with_capacity(text.len());
    for c in text.chars() {
        let seq = keysym_of(c).and_then(|s| find_key(state, &h.keyboard, s));
        match seq {
            Some(s) => seqs.push(s),
            None => return Err(invalid("unsupported")),
        }
    }
    for seq in seqs {
        seq.press(state, &h.keyboard);
        seq.release(state, &h.keyboard);
    }
    Ok("keysym".into())
}

// ------------------------------------------------------------ protocol

/// `eclipse_agent_v1.get_seat`.
pub(super) fn get_seat(
    state: &mut AbyssState,
    agent: &ec_protocols::agent::server::eclipse_agent_v1::EclipseAgentV1,
    aid: u64,
    id: New<EclipseAgentSeatV1>,
    data_init: &mut DataInit<'_, AbyssState>,
) {
    use ec_protocols::agent::server::eclipse_agent_v1::Error;
    // A seat that is refused still needs an object behind its id; id 0 is
    // the marker the dispatcher ignores.
    if aid == 0 || !state.agents.slots.iter().any(|(i, _)| *i == aid) {
        data_init.init(id, AgentId(0));
        agent.post_error(Error::InvalidGrant, "no such agent");
        return;
    }
    if entry(state, aid).is_some() {
        data_init.init(id, AgentId(0));
        agent.post_error(Error::SeatExists, "this agent already has a seat");
        return;
    }
    let resource = data_init.init(id, AgentId(aid));
    let name = format!("agent-{aid}");
    let dh = state.display_handle.clone();
    let mut seat = state.seat_state.new_wl_seat(&dh, name);
    let xkb = crate::input::xkb_config(&state.config.input);
    let (delay, rate) = (state.config.input.repeat_delay, state.config.input.repeat_rate);
    let keyboard = match seat.add_keyboard(xkb, delay, rate) {
        Ok(k) => k,
        Err(err) => {
            tracing::error!(?err, "agent seat: falling back to the default keymap");
            seat.add_keyboard(Default::default(), delay, rate)
                .expect("default xkb keymap must load")
        }
    };
    let pointer = seat.add_pointer();
    let touch = seat.add_touch();
    send_keymap(state, &keyboard, &resource);
    state.agent_seats.push((
        aid,
        AgentSeat {
            resource,
            seat,
            keyboard,
            pointer,
            touch,
            focus: None,
            pointer_window: None,
            pointer_pos: (0.0, 0.0).into(),
            buttons: Vec::new(),
        },
    ));
    tracing::info!(agent = aid, "agent seat created");
}

/// The seat's active keymap, as an xkb v1 text file in a memfd.
fn send_keymap(state: &mut AbyssState, kbd: &KeyboardHandle<AbyssState>, resource: &EclipseAgentSeatV1) {
    let text = kbd.with_xkb_state(state, |ctx| {
        let xkb = ctx.xkb().lock().ok()?;
        // SAFETY: the keymap reference does not outlive the guard.
        let keymap = unsafe { xkb.keymap() };
        Some(keymap.get_as_string(smithay::input::keyboard::xkb::KEYMAP_FORMAT_TEXT_V1))
    });
    let Some(text) = text else {
        return;
    };
    // SAFETY: memfd_create returns a fresh descriptor this function owns; it
    // is wrapped exactly once.
    let file = unsafe {
        let fd = libc::memfd_create(c"eclipse-agent-keymap".as_ptr(), libc::MFD_CLOEXEC);
        if fd < 0 {
            tracing::warn!("agent seat: no keymap memfd");
            return;
        }
        std::fs::File::from_raw_fd(fd)
    };
    let mut file = file;
    // The protocol's size includes the NUL xkbcommon text ends with.
    let mut bytes = text.into_bytes();
    bytes.push(0);
    if file.write_all(&bytes).is_err() {
        tracing::warn!("agent seat: writing the keymap");
        return;
    }
    resource.keymap(file.as_fd(), bytes.len() as u32);
}

fn pressed(res: &EclipseAgentSeatV1, state: u32) -> Option<bool> {
    match state {
        0 => Some(false),
        1 => Some(true),
        _ => {
            res.post_error(eclipse_agent_seat_v1::Error::BadState, "state is 0 or 1");
            None
        }
    }
}

/// Most targets one `preflight` may name (COMP-08 §4.2).
const MAX_TARGETS: usize = 1000;

/// `handles` and `nodes` are CBOR arrays of uint, zipped into
/// `(handle, node)`. Bad CBOR, trailing bytes, unequal lengths or more than
/// [`MAX_TARGETS`] is `None`.
pub(super) fn decode_targets(handles: &[u8], nodes: &[u8]) -> Option<Vec<(u64, u64)>> {
    fn uints(buf: &[u8]) -> Option<Vec<u64>> {
        let mut r = ec_policy_eval::cbor::Reader::new(buf);
        let n = usize::try_from(r.array_len().ok()?).ok()?;
        if n > MAX_TARGETS {
            return None;
        }
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(r.u64().ok()?);
        }
        r.finish().ok()?;
        Some(out)
    }
    let (h, n) = (uints(handles)?, uints(nodes)?);
    (h.len() == n.len()).then(|| h.into_iter().zip(n).collect())
}

/// `preflight`, decoded: `policy::batch` decides.
fn preflight(
    state: &mut AbyssState,
    agent: u64,
    req_id: u32,
    taxonomy: &str,
    targets: &[(u64, u64)],
    summary: &str,
) -> Submitted {
    crate::policy::batch::preflight(state, agent, req_id, taxonomy, targets, summary)
}

/// The human approved a preflight: tell the agent its token (COMP-08 §4.2).
pub fn send_batch_token(
    state: &mut AbyssState,
    agent: u64,
    req_id: u32,
    token: u64,
    covered: u32,
    expires_ms: u64,
) {
    if let Some(s) = entry(state, agent).filter(|s| s.resource.is_alive()) {
        s.resource.batch_token(
            req_id,
            u32::try_from(token).unwrap_or(u32::MAX),
            covered,
            u32::try_from(expires_ms).unwrap_or(u32::MAX),
        );
    }
}

/// `provenance_ids` is the empty CBOR array (or nothing at all).
fn provenance_empty(ids: &[u8]) -> bool {
    ids.is_empty() || ids == [0x80]
}

impl Dispatch<EclipseAgentSeatV1, AgentId> for AbyssState {
    fn request(
        state: &mut Self,
        _client: &Client,
        res: &EclipseAgentSeatV1,
        request: eclipse_agent_seat_v1::Request,
        data: &AgentId,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use eclipse_agent_seat_v1::Request as R;
        if data.0 == 0 {
            return;
        }
        let (req_id, act, prov, batch_token) = match request {
            R::Focus {
                req_id,
                handle,
                batch_token,
                provenance_ids,
                ..
            } => (
                req_id,
                Act::Focus {
                    handle: u64::from(handle),
                },
                provenance_ids,
                batch_token,
            ),
            R::Key {
                req_id,
                keycode,
                state: st,
                mods,
                batch_token,
                provenance_ids,
            } => {
                let Some(p) = pressed(res, st) else {
                    return;
                };
                (
                    req_id,
                    Act::Key {
                        keycode,
                        pressed: p,
                        mods,
                    },
                    provenance_ids,
                    batch_token,
                )
            }
            R::Keysym {
                req_id,
                keysym,
                state: st,
                batch_token,
                provenance_ids,
            } => {
                let Some(p) = pressed(res, st) else {
                    return;
                };
                (
                    req_id,
                    Act::Keysym { keysym, pressed: p },
                    provenance_ids,
                    batch_token,
                )
            }
            R::Text {
                req_id,
                handle,
                text,
                batch_token,
                provenance_ids,
                ..
            } => (
                req_id,
                Act::Text {
                    handle: u64::from(handle),
                    text,
                },
                provenance_ids,
                batch_token,
            ),
            R::PointerAbs {
                req_id,
                x,
                y,
                batch_token,
                provenance_ids,
            } => (req_id, Act::PointerAbs { x, y }, provenance_ids, batch_token),
            R::PointerRel {
                req_id,
                dx,
                dy,
                batch_token,
                provenance_ids,
            } => (req_id, Act::PointerRel { dx, dy }, provenance_ids, batch_token),
            R::Button {
                req_id,
                button,
                state: st,
                batch_token,
                provenance_ids,
            } => {
                let Some(p) = pressed(res, st) else {
                    return;
                };
                (
                    req_id,
                    Act::Button { button, pressed: p },
                    provenance_ids,
                    batch_token,
                )
            }
            R::Axis {
                req_id,
                axis,
                value,
                discrete,
                source,
                batch_token,
                provenance_ids,
            } => (
                req_id,
                Act::Axis {
                    axis,
                    value,
                    discrete,
                    source,
                },
                provenance_ids,
                batch_token,
            ),
            R::TouchDown {
                req_id,
                touch_id,
                x,
                y,
                batch_token,
                provenance_ids,
            } => (
                req_id,
                Act::TouchDown { id: touch_id, x, y },
                provenance_ids,
                batch_token,
            ),
            R::TouchUp {
                req_id,
                touch_id,
                batch_token,
                provenance_ids,
            } => (req_id, Act::TouchUp { id: touch_id }, provenance_ids, batch_token),
            R::TouchMotion {
                req_id,
                touch_id,
                x,
                y,
                batch_token,
                provenance_ids,
            } => (
                req_id,
                Act::TouchMotion { id: touch_id, x, y },
                provenance_ids,
                batch_token,
            ),
            R::Click {
                req_id,
                handle,
                x,
                y,
                button,
                batch_token,
                provenance_ids,
                ..
            } => (
                req_id,
                Act::Click {
                    handle: u64::from(handle),
                    x,
                    y,
                    button,
                },
                provenance_ids,
                batch_token,
            ),
            R::Preflight {
                req_id,
                taxonomy_id,
                handles,
                nodes,
                summary,
            } => {
                let submitted = match decode_targets(&handles, &nodes) {
                    Some(targets) => preflight(state, data.0, req_id, &taxonomy_id, &targets, &summary),
                    None => Submitted::Answered(Status::InvalidArgument, "targets".into()),
                };
                if let Submitted::Answered(status, detail) = submitted {
                    send_result(state, data.0, req_id, status, &detail, 0);
                }
                return;
            }
            R::Destroy => {
                remove(state, data.0);
                return;
            }
            _ => return,
        };
        let started = Instant::now();
        let submitted = enforce::submit(
            state,
            Request {
                agent: data.0,
                req_id,
                act,
                provenance_empty: provenance_empty(&prov),
                batch_token: u64::from(batch_token),
            },
        );
        if let Submitted::Answered(status, detail) = submitted {
            let us = u32::try_from(started.elapsed().as_micros()).unwrap_or(u32::MAX);
            send_result(state, data.0, req_id, status, &detail, us);
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, _res: &EclipseAgentSeatV1, data: &AgentId) {
        remove(state, data.0);
    }
}
