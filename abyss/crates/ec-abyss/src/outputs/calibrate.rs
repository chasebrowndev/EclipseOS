// SPDX-License-Identifier: AGPL-3.0-only
//! The on-screen overscan calibration session (COMP-03 §2).
//!
//! One output at a time is "calibrating": its corner markers are drawn into the
//! framebuffer margin and every key on the human seat is consumed here instead
//! of reaching a client. That total grab is the point — the human is looking at
//! a TV that is eating the edges of the picture, and the arrow keys have to mean
//! *this* for as long as that lasts.
//!
//! Compositor-drawn by necessity, not by preference: the markers live in the
//! margin, which is exactly the region no client can ever address.
//!
//! Keymap (settled with the owner):
//!
//! ```text
//!   ← ↑ → ↓            that edge inward   1px
//!   Ctrl + arrow       that edge inward  10px
//!   Shift + arrow      that edge outward  1px
//!   Ctrl+Shift+arrow   that edge outward 10px
//!   -  /  +            all four edges outward / inward (Ctrl for 10px)
//!   Tab                move to the next enabled output
//!   Enter              commit and persist
//!   Esc                revert to the value we started with
//!   R                  reset this output to zero
//! ```
//!
//! `Shift` is outward, so the 10px modifier is `Ctrl` — binding both to Shift
//! would have made half the map unreachable.

use smithay::input::keyboard::{Keysym, ModifiersState};

use super::overscan::{Edge, Overscan};
use crate::state::AbyssState;

/// Resolved meaning of one keypress while calibrating. Computed in the input
/// filter, which only sees `&AbyssState`, and applied by [`apply`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Move one edge by `px`; positive is inward.
    Edge(Edge, i32),
    /// Move all four edges by `px`; positive is inward.
    All(i32),
    Next,
    Commit,
    Cancel,
    Reset,
    /// A key with no meaning here. Swallowed anyway — see the module docs.
    Ignored,
}

/// Interpret a keypress. Returns `None` when no calibration is running, which
/// is the signal to let the key through to the normal binding path.
pub fn step_for(state: &AbyssState, mods: &ModifiersState, sym: Keysym) -> Option<Step> {
    active(state)?;
    let px = if mods.ctrl { 10 } else { 1 };
    let dir = if mods.shift { -px } else { px };
    Some(match sym {
        Keysym::Up => Step::Edge(Edge::Top, dir),
        Keysym::Down => Step::Edge(Edge::Bottom, dir),
        Keysym::Left => Step::Edge(Edge::Left, dir),
        Keysym::Right => Step::Edge(Edge::Right, dir),
        Keysym::plus | Keysym::equal | Keysym::KP_Add => Step::All(px),
        Keysym::minus | Keysym::KP_Subtract => Step::All(-px),
        Keysym::Tab | Keysym::ISO_Left_Tab => Step::Next,
        Keysym::Return | Keysym::KP_Enter | Keysym::space => Step::Commit,
        Keysym::Escape => Step::Cancel,
        Keysym::r | Keysym::R => Step::Reset,
        _ => Step::Ignored,
    })
}

/// The output currently calibrating, if any.
pub fn active(state: &AbyssState) -> Option<u64> {
    state
        .outputs
        .iter()
        .find(|e| e.calibrating.is_some())
        .map(|e| e.id)
}

/// Begin calibrating one output. Any session already running is committed
/// first, so `Tab` and a second `calibrate` call behave the same way.
pub fn start(state: &mut AbyssState, id: u64) -> bool {
    if let Some(current) = active(state) {
        if current == id {
            return true;
        }
        commit(state);
    }
    let Some(entry) = state.outputs.get_mut(id) else {
        return false;
    };
    entry.calibrating = Some(super::Calibration {
        original: entry.overscan,
        last_input: std::time::Instant::now(),
    });
    arm_timeout(state);
    crate::backend::damage_all(state);
    emit(state, id, "started");
    true
}

/// Fires once a second while a session is live; reverts one that has gone
/// quiet. A TV left mid-calibration with an unreachable desktop has to recover
/// on its own — the human may not be able to see the keyboard hints any more.
fn arm_timeout(state: &mut AbyssState) {
    let handle = state.loop_handle.clone();
    let res = handle.insert_source(
        smithay::reexports::calloop::timer::Timer::from_duration(std::time::Duration::from_secs(1)),
        |_, _, state: &mut AbyssState| {
            use smithay::reexports::calloop::timer::TimeoutAction;
            let Some(id) = active(state) else {
                return TimeoutAction::Drop;
            };
            let expired = state
                .outputs
                .get(id)
                .and_then(|e| e.calibrating.as_ref())
                .map(|c| c.last_input.elapsed() >= super::CALIBRATION_TIMEOUT)
                .unwrap_or(true);
            if expired {
                cancel(state);
                return TimeoutAction::Drop;
            }
            TimeoutAction::ToDuration(std::time::Duration::from_secs(1))
        },
    );
    if let Err(err) = res {
        tracing::warn!(%err, "arming the calibration timeout");
    }
}

/// Apply one resolved [`Step`]. Returns false once the session has ended.
pub fn apply(state: &mut AbyssState, step: Step) -> bool {
    let Some(id) = active(state) else { return false };
    if let Some(entry) = state.outputs.get_mut(id) {
        if let Some(cal) = entry.calibrating.as_mut() {
            cal.last_input = std::time::Instant::now();
        }
    }
    match step {
        Step::Ignored => true,
        Step::Edge(edge, px) => {
            let mut value = super::overscan_of(state, id);
            value.nudge(edge, px.abs(), px < 0);
            // Not persisted: the preview has to be live on screen without
            // writing a file on every arrow key.
            super::set_overscan(state, id, value, false);
            true
        }
        Step::All(px) => {
            let mut value = super::overscan_of(state, id);
            value.nudge_all(px.abs(), px < 0);
            super::set_overscan(state, id, value, false);
            true
        }
        Step::Reset => {
            super::set_overscan(state, id, Overscan::default(), false);
            true
        }
        Step::Next => {
            next(state, id);
            true
        }
        Step::Commit => {
            commit(state);
            false
        }
        Step::Cancel => {
            cancel(state);
            false
        }
    }
}

/// Hand the session to the next enabled output, wrapping. With one output this
/// is a no-op, which is the right answer — there is nowhere else to go.
fn next(state: &mut AbyssState, from: u64) {
    let ids: Vec<u64> = state.outputs.iter().filter(|e| e.enabled).map(|e| e.id).collect();
    let Some(at) = ids.iter().position(|i| *i == from) else {
        return;
    };
    let to = ids[(at + 1) % ids.len()];
    if to != from {
        start(state, to);
    }
}

/// Keep the current value and write it out.
pub fn commit(state: &mut AbyssState) {
    let Some(id) = active(state) else { return };
    let value = super::overscan_of(state, id);
    if let Some(entry) = state.outputs.get_mut(id) {
        entry.calibrating = None;
    }
    super::set_overscan(state, id, value, true);
    crate::backend::damage_all(state);
    emit(state, id, "committed");
}

/// Put back the value the session started with.
pub fn cancel(state: &mut AbyssState) {
    let Some(id) = active(state) else { return };
    let original = state
        .outputs
        .get(id)
        .and_then(|e| e.calibrating.as_ref())
        .map(|c| c.original)
        .unwrap_or_default();
    if let Some(entry) = state.outputs.get_mut(id) {
        entry.calibrating = None;
    }
    super::set_overscan(state, id, original, false);
    crate::backend::damage_all(state);
    emit(state, id, "cancelled");
}

fn emit(state: &mut AbyssState, id: u64, phase: &str) {
    let value = super::overscan_of(state, id);
    crate::ipc::emit(
        state,
        "output",
        serde_json::json!({
            "change": "calibration",
            "phase": phase,
            "id": id,
            "top": value.top,
            "bottom": value.bottom,
            "left": value.left,
            "right": value.right,
        }),
    );
}
