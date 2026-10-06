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
//! - Only the human seat's keyboard, button and scroll events are held. Pointer
//!   motion, touch and tablet pass; they do not change what a client believes
//!   about seats' key state.
//!
//! The hooks the input path calls ([`hold_key`], [`hold_button`],
//! [`hold_axis`]) return at once, with no allocation, while no lock exists.
//!
//! TCB-HOOK (COMP-10 §3.4): the trusted-UI focus-steal/compat-lock indicator
//! has to be drawn while [`Locks::active`] is non-empty. Enforcement of the
//! request itself (the table's `check()` for `seat.compat_lock`) is a
//! TCB-HOOK in [`lock`].

use std::time::{Duration, Instant};

use ec_protocols::agent::server::eclipse_agent_v1::Status;
use smithay::backend::input::{ButtonState, KeyState};
use smithay::desktop::Window;
use smithay::input::keyboard::Keycode;
use smithay::input::pointer::{AxisFrame, ButtonEvent};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::RegistrationToken;
use smithay::utils::{IsAlive, SERIAL_COUNTER};

use super::atomic;
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
    Key { code: Keycode, pressed: bool, time: u32 },
    Button { button: u32, pressed: bool, time: u32 },
    Axis(AxisFrame),
}

#[derive(Debug)]
struct Lock {
    agent: u64,
    window: Window,
    token: u64,
    expires: Instant,
    timer: Option<RegistrationToken>,
    held: Vec<Held>,
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
}

/// `compat_lock`. The answer for the caller to send.
pub(super) fn lock(state: &mut AbyssState, agent: u64, handle: u64, timeout_ms: u32) -> (Status, String) {
    if state.policy_key.is_none() || crate::policy::lifecycle::is_paused(state, agent) {
        return (Status::Paused, String::new());
    }
    if timeout_ms > MAX_TIMEOUT_MS {
        return (Status::InvalidArgument, "timeout_ms".to_owned());
    }
    // TCB-HOOK: run `check()` for capability `seat.compat_lock` (step 7) with
    // the target's facts, and add `seat.compat_lock` to `policy::SEAT_CAPS`
    // so its scope is compiled; honour prompt/defer. Until then the grant
    // must hold `seat.compat_lock` AND `seat.focus` reaching the window
    // (so a `seat.focus` scope bounds it), which is tighter than the spec.
    if !atomic::held(state, agent, "seat.compat_lock") {
        return (Status::NoCapability, "seat.compat_lock".to_owned());
    }
    let window = match atomic::visible_window(state, agent, "seat.focus", handle) {
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
    });
    tracing::info!(agent, "compat lock taken");
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
    let pos = state.pointer_location;
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
    for ev in held {
        match ev {
            Held::Key { code, pressed, time } if kbd => {
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
            } if ptr => {
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

fn push(state: &mut AbyssState, i: usize, ev: Held) {
    let l = &mut state.agents.locks.locks[i];
    if l.held.len() >= MAX_HELD {
        // A lock that would have to drop input gives the seat back instead.
        let token = l.token;
        release(state, token);
        return;
    }
    l.held.push(ev);
}

/// A human key that no binding took, about to be delivered to the human's
/// keyboard focus. True if the lock on that window held it instead.
pub fn hold_key(state: &mut AbyssState, code: Keycode, pressed: bool, time: u32) -> bool {
    let Some(i) = holder(state, false) else {
        return false;
    };
    push(state, i, Held::Key { code, pressed, time });
    true
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
    );
    true
}

/// A human scroll frame, about to be delivered. True if held.
pub fn hold_axis(state: &mut AbyssState, frame: AxisFrame) -> bool {
    let Some(i) = holder(state, true) else {
        return false;
    };
    push(state, i, Held::Axis(frame));
    true
}
