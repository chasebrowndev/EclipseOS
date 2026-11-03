// SPDX-License-Identifier: AGPL-3.0-only
//! Compat locks (COMP-04 §7, §8; COMP-08 §4 `compat_lock`). Not TCB.
//!
//! For an app flagged `seat-compat lock` (X11 windows always are), an agent
//! may take an exclusive lock on its toplevel. While it is held the *human's*
//! keys, pointer buttons and scrolls for that window are **queued, not
//! dropped**, and delivered in order on release, so an agent's multi-step
//! interaction is not broken by a client that assumes a single seat.
//!
//! - One lock per agent and per window. A second agent's lock on a locked
//!   window is `quota_exceeded` "compat_lock".
//! - It ends on `compat_unlock`, on its timeout (default 10 s, at most
//!   60 s), when the agent is paused (the human override must give the seat
//!   back at once), when the agent or its seat goes, or when the window dies.
//! - Release replays what was queued, but only to the window it was for: if
//!   the human's keyboard focus or the pointer has since left that window,
//!   the queued input is discarded rather than delivered to whatever has
//!   focus now. Misdelivered keystrokes are worse than lost ones.
//! - The human seat's keyboard, button, scroll, touch and tablet tip/button
//!   events are held. Pointer motion and tablet axes pass; they do not change
//!   what a client believes about seats' key state. A touch point is held from
//!   its down to its up, so a point that was already down at lock time goes on
//!   normally; on replay a point whose window is no longer under it is dropped.
//! - Compat fallback (COMP-04 §8): while an agent holds the lock, its acts on
//!   that window are delivered through the human seat ([`route`]), tagged
//!   `Origin::AgentCompat`, instead of through its own seat. They are never
//!   queued by the lock they run under. They go to the window only: the human's
//!   keyboard focus has to be there already (an agent seat cannot steal it).
//!
//! The hooks the input path calls ([`hold_key`], [`hold_button`],
//! [`hold_axis`], [`hold_touch_down`], [`hold_tablet_tip`] and so on) return at
//! once, with no allocation, while no lock exists.
//!
//! The trusted-UI indicator (COMP-10 §3.4) reads [`Locks::shown`]. The request itself
//! is decided by `policy::enforce::control` (scope and table for
//! `seat.compat_lock`).

use std::time::{Duration, Instant};

use ec_protocols::agent::server::eclipse_agent_v1::Status;
use smithay::backend::input::{ButtonState, KeyState, TabletToolDescriptor};
use smithay::desktop::Window;
use smithay::input::keyboard::Keycode;
use smithay::input::pointer::{AxisFrame, ButtonEvent};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::RegistrationToken;
use smithay::utils::{IsAlive, Logical, Point, SERIAL_COUNTER};

use crate::input::origin::Origin;
use crate::policy::enforce::Act;
use crate::protocols::standard::seat::KeyboardFocusTarget;
use crate::state::AbyssState;
use crate::xwayland::security::SeatCompat;

/// Lifetime when the request says 0.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(10_000);
/// The longest a lock may last.
pub const MAX_TIMEOUT_MS: u32 = 60_000;
/// Human events one lock queues before it gives up and releases.
const MAX_HELD: usize = 1024;

#[derive(Debug)]
enum Held {
    Key {
        code: Keycode,
        pressed: bool,
        time: u32,
    },
    Button {
        button: u32,
        pressed: bool,
        time: u32,
    },
    Axis(AxisFrame),
    TouchDown {
        slot: u32,
        pos: Point<f64, Logical>,
        time: u32,
    },
    TouchMove {
        slot: u32,
        pos: Point<f64, Logical>,
        time: u32,
    },
    TouchUp {
        slot: u32,
        time: u32,
    },
    TabletTip {
        tool: TabletToolDescriptor,
        down: bool,
        time: u32,
    },
    TabletButton {
        tool: TabletToolDescriptor,
        button: u32,
        state: ButtonState,
        time: u32,
    },
}

#[derive(Debug)]
struct Lock {
    agent: u64,
    window: Window,
    token: u64,
    expires: Instant,
    timer: Option<RegistrationToken>,
    held: Vec<Held>,
    /// Touch slots whose down is held and whose up has not come.
    slots: Vec<u32>,
    /// What the agent's own acts, run through the human seat, still hold down.
    keys: Vec<Keycode>,
    buttons: Vec<u32>,
    /// Touch slots the agent put down through the human seat.
    agent_slots: Vec<u32>,
}

#[derive(Debug, Default)]
pub struct Locks {
    locks: Vec<Lock>,
    next: u64,
}

impl Locks {
    /// The windows currently locked, for the trusted-UI indicator.
    pub fn active(&self) -> impl Iterator<Item = (u64, &Window)> {
        self.locks.iter().map(|l| (l.agent, &l.window))
    }

    /// Each lock with when it ends and how many human events it holds, for
    /// the trusted-UI indicator (COMP-10 §3.4).
    pub fn shown(&self) -> impl Iterator<Item = (u64, &Window, Instant, usize)> {
        self.locks
            .iter()
            .map(|l| (l.agent, &l.window, l.expires, l.held.len()))
    }
}

/// `compat_lock`. The answer for the caller to send.
pub(super) fn lock(state: &mut AbyssState, agent: u64, handle: u64, timeout_ms: u32) -> (Status, String) {
    if state.policy_key.is_none() || crate::policy::lifecycle::is_paused(state, agent) {
        return (Status::Paused, String::new());
    }
    if timeout_ms > MAX_TIMEOUT_MS {
        return (Status::InvalidArgument, "timeout_ms".to_owned());
    }
    // Steps 3 to 7: `seat.compat_lock`'s scope reaches the window and the
    // table allows it (S-01 §2.2). A prompt or defer is refused, not waited.
    let window = match crate::policy::enforce::control(state, agent, "seat.compat_lock", handle) {
        Ok(w) => w,
        Err(e) => return e,
    };
    let flagged = window.x11_surface().is_some()
        || crate::shell::rules::seat_compat_of(&window) == Some(SeatCompat::Lock);
    if !flagged {
        return (Status::InvalidArgument, "not_compat".to_owned());
    }
    // One lock per agent: a new one replaces the old.
    release_agent(state, agent);
    if state.agents.locks.locks.iter().any(|l| l.window == window) {
        return (Status::QuotaExceeded, "compat_lock".to_owned());
    }
    let dur = match timeout_ms {
        0 => DEFAULT_TIMEOUT,
        ms => Duration::from_millis(u64::from(ms)),
    };
    state.agents.locks.next += 1;
    let token = state.agents.locks.next;
    let timer = state
        .loop_handle
        .insert_source(Timer::from_duration(dur), move |_, _, st| {
            expire(st, token);
            TimeoutAction::Drop
        });
    let Ok(timer) = timer else {
        // No timer means no bound on the lock: refuse.
        return (Status::QuotaExceeded, "timer".to_owned());
    };
    state.agents.locks.locks.push(Lock {
        agent,
        window,
        token,
        expires: Instant::now() + dur,
        timer: Some(timer),
        held: Vec::new(),
        slots: Vec::new(),
        keys: Vec::new(),
        buttons: Vec::new(),
        agent_slots: Vec::new(),
    });
    tracing::info!(agent, "compat lock taken");
    crate::trusted_ui::indicator::tick(state);
    (Status::Ok, String::new())
}

/// `compat_unlock`.
pub(super) fn unlock(state: &mut AbyssState, agent: u64) -> (Status, String) {
    if release_agent(state, agent) {
        (Status::Ok, String::new())
    } else {
        (Status::InvalidArgument, "no_lock".to_owned())
    }
}

/// Release `agent`'s lock, replaying what it held. Whether there was one.
pub(crate) fn release_agent(state: &mut AbyssState, agent: u64) -> bool {
    match state.agents.locks.locks.iter().find(|l| l.agent == agent) {
        Some(l) => {
            let token = l.token;
            release(state, token);
            true
        }
        None => false,
    }
}

fn expire(state: &mut AbyssState, token: u64) {
    // The timer is spent: do not try to remove it.
    if let Some(l) = state.agents.locks.locks.iter_mut().find(|l| l.token == token) {
        l.timer = None;
    }
    release(state, token);
}

fn release(state: &mut AbyssState, token: u64) {
    let Some(i) = state.agents.locks.locks.iter().position(|l| l.token == token) else {
        return;
    };
    let l = state.agents.locks.locks.remove(i);
    if let Some(t) = l.timer {
        state.loop_handle.remove(t);
    }
    tracing::info!(agent = l.agent, held = l.held.len(), "compat lock released");
    // What the agent left pressed on the human seat goes up before the
    // human's own input is replayed.
    state.with_origin(Origin::AgentCompat, |st| {
        for slot in l.agent_slots {
            let time = super::seat::now_ms(st);
            st.inject_touch_up(slot, time);
        }
        if let Some(k) = st.seat.get_keyboard() {
            for code in l.keys {
                let time = super::seat::now_ms(st);
                k.input_forward(
                    st,
                    code,
                    KeyState::Released,
                    SERIAL_COUNTER.next_serial(),
                    time,
                    true,
                );
            }
        }
        if let Some(p) = st.seat.get_pointer() {
            for button in l.buttons {
                let time = super::seat::now_ms(st);
                p.button(
                    st,
                    &ButtonEvent {
                        serial: SERIAL_COUNTER.next_serial(),
                        time,
                        button,
                        state: ButtonState::Released,
                    },
                );
                p.frame(st);
            }
        }
    });
    replay(state, &l.window, l.held);
}

/// Locks that have ended without anyone asking: timed out, the agent paused
/// or gone, the window dead.
fn sweep(state: &mut AbyssState) {
    let now = Instant::now();
    let ended: Vec<u64> = state
        .agents
        .locks
        .locks
        .iter()
        .filter(|l| {
            l.expires <= now
                || !l.window.alive()
                || crate::policy::lifecycle::is_paused(state, l.agent)
                || state.agents.peek_agent(l.agent).is_none()
        })
        .map(|l| l.token)
        .collect();
    for t in ended {
        release(state, t);
    }
}

fn focus_is(target: &KeyboardFocusTarget, window: &Window) -> bool {
    match target {
        KeyboardFocusTarget::Wl(s) => crate::shell::window_surface(window).as_ref() == Some(s),
        KeyboardFocusTarget::X11(x) => window.x11_surface() == Some(x),
    }
}

fn keyboard_on(state: &AbyssState, window: &Window) -> bool {
    state
        .seat
        .get_keyboard()
        .and_then(|k| k.current_focus())
        .is_some_and(|t| focus_is(&t, window))
}

/// The topmost thing under the human pointer is `window`.
fn pointer_on(state: &AbyssState, window: &Window) -> bool {
    over(state, window, state.pointer_location)
}

/// The topmost thing at `pos` is `window`.
fn over(state: &AbyssState, window: &Window, pos: Point<f64, Logical>) -> bool {
    crate::shell::layer_at(state, pos).is_none()
        && state.space.element_under(pos).is_some_and(|(w, _)| w == window)
}

fn replay(state: &mut AbyssState, window: &Window, held: Vec<Held>) {
    if held.is_empty() {
        return;
    }
    let kbd = keyboard_on(state, window);
    let ptr = pointer_on(state, window);
    let mut dropped = 0usize;
    // Touch points whose down was delivered; their moves and up follow it.
    let mut live: Vec<u32> = Vec::new();
    for ev in held {
        match ev {
            Held::Key { code, pressed, time } if kbd || !pressed => {
                let Some(k) = state.seat.get_keyboard() else {
                    continue;
                };
                // The key was already run through the keymap and the binding
                // filter when it was held; this is only the delivery.
                let st = if pressed {
                    KeyState::Pressed
                } else {
                    KeyState::Released
                };
                k.input_forward(state, code, st, SERIAL_COUNTER.next_serial(), time, true);
            }
            Held::Button {
                button,
                pressed,
                time,
            } if ptr || !pressed => {
                let Some(p) = state.seat.get_pointer() else {
                    continue;
                };
                p.button(
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
                p.frame(state);
            }
            Held::Axis(frame) if ptr => {
                let Some(p) = state.seat.get_pointer() else {
                    continue;
                };
                p.axis(state, frame);
                p.frame(state);
            }
            Held::TouchDown { slot, pos, time } if over(state, window, pos) => {
                live.push(slot);
                state.inject_touch_down(slot, pos, time);
            }
            Held::TouchMove { slot, pos, time } if live.contains(&slot) => {
                state.inject_touch_motion(slot, pos, time);
            }
            Held::TouchUp { slot, time } if live.contains(&slot) => {
                live.retain(|s| *s != slot);
                state.inject_touch_up(slot, time);
            }
            Held::TabletTip { tool, down, time } if ptr || !down => state.tablet_tip(&tool, down, time),
            Held::TabletButton {
                tool,
                button,
                state: bs,
                time,
            } if ptr || bs == ButtonState::Released => state.tablet_button(&tool, button, bs, time),
            _ => dropped += 1,
        }
    }
    if dropped > 0 {
        tracing::info!(
            dropped,
            "held human input discarded: focus left the locked window"
        );
    }
}

/// The index of the lock whose window has the human's keyboard focus (or
/// pointer, for `pointer`), after ending any that are over.
fn holder(state: &mut AbyssState, pointer: bool) -> Option<usize> {
    if state.agents.locks.locks.is_empty() {
        return None;
    }
    sweep(state);
    state.agents.locks.locks.iter().position(|l| {
        if pointer {
            pointer_on(state, &l.window)
        } else {
            keyboard_on(state, &l.window)
        }
    })
}

/// Queue `ev`. False if the lock was full: it gives the seat back instead
/// (replaying what it held), and `ev` is not queued, so the caller lets it
/// through after that replay and nothing, a release above all, is lost.
fn push(state: &mut AbyssState, i: usize, ev: Held) -> bool {
    let l = &mut state.agents.locks.locks[i];
    if l.held.len() >= MAX_HELD {
        let token = l.token;
        release(state, token);
        return false;
    }
    l.held.push(ev);
    true
}

/// A human key that no binding took, about to be delivered to the human's
/// keyboard focus. True if the lock on that window held it instead.
pub fn hold_key(state: &mut AbyssState, code: Keycode, pressed: bool, time: u32) -> bool {
    let Some(i) = holder(state, false) else {
        return false;
    };
    push(state, i, Held::Key { code, pressed, time })
}

/// A human pointer button, about to be delivered. True if held.
pub fn hold_button(state: &mut AbyssState, button: u32, pressed: bool, time: u32) -> bool {
    let Some(i) = holder(state, true) else {
        return false;
    };
    push(
        state,
        i,
        Held::Button {
            button,
            pressed,
            time,
        },
    )
}

/// A human scroll frame, about to be delivered. True if held.
pub fn hold_axis(state: &mut AbyssState, frame: AxisFrame) -> bool {
    let Some(i) = holder(state, true) else {
        return false;
    };
    push(state, i, Held::Axis(frame))
}

/// A human touch-down at `pos`, about to be delivered. Held if a lock's window
/// is topmost there; its slot's later moves and up are held with it.
pub fn hold_touch_down(state: &mut AbyssState, slot: u32, pos: Point<f64, Logical>, time: u32) -> bool {
    if state.agents.locks.locks.is_empty() {
        return false;
    }
    sweep(state);
    let Some(i) = state
        .agents
        .locks
        .locks
        .iter()
        .position(|l| over(state, &l.window, pos))
    else {
        return false;
    };
    let l = &mut state.agents.locks.locks[i];
    if !l.slots.contains(&slot) {
        l.slots.push(slot);
    }
    push(state, i, Held::TouchDown { slot, pos, time })
}

/// The lock holding `slot`, after ending any that are over.
fn slot_holder(state: &mut AbyssState, slot: u32) -> Option<usize> {
    if state.agents.locks.locks.is_empty() {
        return None;
    }
    sweep(state);
    state
        .agents
        .locks
        .locks
        .iter()
        .position(|l| l.slots.contains(&slot))
}

/// A human touch-motion for a point whose down is held. True if held.
pub fn hold_touch_move(state: &mut AbyssState, slot: u32, pos: Point<f64, Logical>, time: u32) -> bool {
    let Some(i) = slot_holder(state, slot) else {
        return false;
    };
    push(state, i, Held::TouchMove { slot, pos, time })
}

/// A human touch-up for a point whose down is held. True if held.
pub fn hold_touch_up(state: &mut AbyssState, slot: u32, time: u32) -> bool {
    let Some(i) = slot_holder(state, slot) else {
        return false;
    };
    state.agents.locks.locks[i].slots.retain(|s| *s != slot);
    push(state, i, Held::TouchUp { slot, time })
}

/// A stylus tip down or up, about to be delivered to what the cursor is over.
/// True if held.
pub fn hold_tablet_tip(state: &mut AbyssState, tool: &TabletToolDescriptor, down: bool, time: u32) -> bool {
    let Some(i) = holder(state, true) else {
        return false;
    };
    push(
        state,
        i,
        Held::TabletTip {
            tool: tool.clone(),
            down,
            time,
        },
    )
}

/// A stylus button, about to be delivered. True if held.
pub fn hold_tablet_button(
    state: &mut AbyssState,
    tool: &TabletToolDescriptor,
    button: u32,
    bs: ButtonState,
    time: u32,
) -> bool {
    let Some(i) = holder(state, true) else {
        return false;
    };
    push(
        state,
        i,
        Held::TabletButton {
            tool: tool.clone(),
            button,
            state: bs,
            time,
        },
    )
}

// ------------------------------------------------------------ compat fallback

type Done = Result<String, (Status, String)>;

/// COMP-04 §8: an agent act on a window the agent holds a compat lock on goes
/// through the human seat. `None` if `agent` holds no lock on `window`, and
/// the act then runs on the agent's own seat as usual.
///
/// The act has already been decided and audited by `policy::enforce`; this is
/// only the delivery. Events carry `Origin::AgentCompat`, so a protected
/// surface refuses them (COMP-19 §3).
pub(super) fn route(state: &mut AbyssState, agent: u64, act: &Act, window: &Window) -> Option<Done> {
    let i = state
        .agents
        .locks
        .locks
        .iter()
        .position(|l| l.agent == agent && &l.window == window)?;
    // A trusted prompt or the region selector owns the human seat; an act
    // through it is refused the same as the human's own input is held back
    // (COMP-10 §3.10).
    if state.seat_held() {
        return Some(Err((Status::FocusLost, "seat_held".to_owned())));
    }
    Some(state.with_origin(Origin::AgentCompat, |st| deliver(st, i, act, window)))
}

fn protected(state: &mut AbyssState, window: &Window) -> bool {
    use crate::protocols::protected::{has_protected, is_protected, refuse};
    let Some(s) = crate::shell::window_surface(window) else {
        return false;
    };
    if is_protected(state, &s) || has_protected(state, &s) {
        refuse(state, Origin::AgentCompat, &s);
        return true;
    }
    false
}

fn deliver(state: &mut AbyssState, i: usize, act: &Act, window: &Window) -> Done {
    use super::seat::{evdev, find_key, keysym_of, now_ms};
    let obscured = || Err((Status::OutOfScope, "obscured".to_owned()));
    let not_focused = || Err((Status::FocusLost, "human_focus".to_owned()));
    let time = now_ms(state);
    let slot_id = |id: i32| u32::try_from(id).map_err(|_| (Status::InvalidArgument, "touch_id".to_owned()));
    let at = |x: i32, y: i32| Point::<f64, Logical>::from((f64::from(x), f64::from(y)));
    match *act {
        Act::Focus { .. } => {
            if keyboard_on(state, window) {
                Ok(String::new())
            } else {
                not_focused()
            }
        }
        Act::Key { keycode, pressed, .. } => {
            let code = evdev(keycode)?;
            // Lifting a key the agent itself pressed is always allowed.
            let lift = !pressed && state.agents.locks.locks[i].keys.contains(&code);
            if !lift && !keyboard_on(state, window) {
                return not_focused();
            }
            if !lift && protected(state, window) {
                return Err((Status::OutOfScope, "protected".to_owned()));
            }
            let Some(k) = state.seat.get_keyboard() else {
                return not_focused();
            };
            let st = if pressed {
                KeyState::Pressed
            } else {
                KeyState::Released
            };
            k.input_forward(state, code, st, SERIAL_COUNTER.next_serial(), time, true);
            let keys = &mut state.agents.locks.locks[i].keys;
            if pressed {
                if !keys.contains(&code) {
                    keys.push(code);
                }
            } else {
                keys.retain(|c| *c != code);
            }
            Ok(String::new())
        }
        Act::Keysym { keysym, pressed } => {
            if !keyboard_on(state, window) {
                return not_focused();
            }
            if protected(state, window) {
                return Err((Status::OutOfScope, "protected".to_owned()));
            }
            let Some(k) = state.seat.get_keyboard() else {
                return not_focused();
            };
            let seq = find_key(state, &k, keysym).ok_or((Status::InvalidArgument, "keysym".to_owned()))?;
            let keys = &mut state.agents.locks.locks[i].keys;
            if pressed {
                for c in seq.shift.into_iter().chain([seq.key]) {
                    if !keys.contains(&c) {
                        keys.push(c);
                    }
                }
                seq.press(state, &k);
            } else {
                keys.retain(|c| *c != seq.key && Some(*c) != seq.shift);
                seq.release(state, &k);
            }
            Ok(String::new())
        }
        Act::Text { ref text, .. } => {
            if !keyboard_on(state, window) {
                return not_focused();
            }
            if protected(state, window) {
                return Err((Status::OutOfScope, "protected".to_owned()));
            }
            let Some(k) = state.seat.get_keyboard() else {
                return not_focused();
            };
            // Every key resolved before any is sent: untypeable text types nothing.
            let mut seqs = Vec::with_capacity(text.len());
            for c in text.chars() {
                match keysym_of(c).and_then(|s| find_key(state, &k, s)) {
                    Some(s) => seqs.push(s),
                    None => return Err((Status::InvalidArgument, "unsupported".to_owned())),
                }
            }
            for s in seqs {
                s.press(state, &k);
                s.release(state, &k);
            }
            Ok("keysym".to_owned())
        }
        Act::PointerAbs { x, y } => {
            let pos = at(x, y);
            if !over(state, window, pos) {
                return obscured();
            }
            state.pointer_moved(pos, time);
            Ok(String::new())
        }
        Act::PointerRel { dx, dy } => {
            let pos = super::seat::clamp_to(state, window, state.pointer_location + Point::from((dx, dy)));
            if !over(state, window, pos) {
                return obscured();
            }
            state.pointer_moved(pos, time);
            Ok(String::new())
        }
        Act::Button { button, pressed } => human_button(state, i, window, button, pressed, time),
        Act::Click { x, y, button, .. } => {
            let pos = at(x, y);
            if !over(state, window, pos) {
                return obscured();
            }
            state.pointer_moved(pos, time);
            human_button(state, i, window, button, true, time)?;
            human_button(state, i, window, button, false, time)
        }
        Act::Axis {
            axis,
            value,
            discrete,
            source,
        } => {
            use smithay::backend::input::{Axis, AxisSource};
            let axis = match axis {
                0 => Axis::Vertical,
                1 => Axis::Horizontal,
                _ => return Err((Status::InvalidArgument, "axis".to_owned())),
            };
            let source = match source {
                0 => AxisSource::Wheel,
                1 => AxisSource::Finger,
                2 => AxisSource::Continuous,
                3 => AxisSource::WheelTilt,
                _ => return Err((Status::InvalidArgument, "source".to_owned())),
            };
            if !pointer_on(state, window) {
                return obscured();
            }
            if protected(state, window) {
                return Err((Status::OutOfScope, "protected".to_owned()));
            }
            let mut frame = AxisFrame::new(time).source(source).value(axis, value);
            if discrete != 0 {
                frame = frame.v120(axis, discrete.saturating_mul(120));
            }
            if let Some(p) = state.seat.get_pointer() {
                p.axis(state, frame);
                p.frame(state);
            }
            Ok(String::new())
        }
        Act::TouchDown { id, x, y } => {
            let slot = slot_id(id)?;
            let pos = at(x, y);
            if !over(state, window, pos) {
                return obscured();
            }
            if protected(state, window) {
                return Err((Status::OutOfScope, "protected".to_owned()));
            }
            let slots = &mut state.agents.locks.locks[i].agent_slots;
            if !slots.contains(&slot) {
                slots.push(slot);
            }
            state.inject_touch_down(slot, pos, time);
            Ok(String::new())
        }
        Act::TouchMotion { id, x, y } => {
            let slot = slot_id(id)?;
            // Only a point this agent put down.
            if !state.agents.locks.locks[i].agent_slots.contains(&slot) {
                return Err((Status::InvalidArgument, "touch_id".to_owned()));
            }
            let pos = at(x, y);
            if !over(state, window, pos) {
                return obscured();
            }
            if protected(state, window) {
                return Err((Status::OutOfScope, "protected".to_owned()));
            }
            state.inject_touch_motion(slot, pos, time);
            Ok(String::new())
        }
        Act::TouchUp { id } => {
            let slot = slot_id(id)?;
            let slots = &mut state.agents.locks.locks[i].agent_slots;
            if !slots.contains(&slot) {
                return Err((Status::InvalidArgument, "touch_id".to_owned()));
            }
            slots.retain(|s| *s != slot);
            state.inject_touch_up(slot, time);
            Ok(String::new())
        }
    }
}

/// One button on the human pointer, to the window under it.
fn human_button(
    state: &mut AbyssState,
    i: usize,
    window: &Window,
    button: u32,
    pressed: bool,
    time: u32,
) -> Done {
    // A release goes wherever the press went, and only a button the agent
    // itself pressed can be released; only a press is steered.
    if pressed {
        if !pointer_on(state, window) {
            return Err((Status::OutOfScope, "obscured".to_owned()));
        }
        if protected(state, window) {
            return Err((Status::OutOfScope, "protected".to_owned()));
        }
    } else if !state.agents.locks.locks[i].buttons.contains(&button) {
        return Err((Status::InvalidArgument, "button".to_owned()));
    }
    let Some(p) = state.seat.get_pointer() else {
        return Err((Status::FocusLost, String::new()));
    };
    p.button(
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
    p.frame(state);
    let held = &mut state.agents.locks.locks[i].buttons;
    if pressed {
        if !held.contains(&button) {
            held.push(button);
        }
    } else {
        held.retain(|b| *b != button);
    }
    Ok(String::new())
}
