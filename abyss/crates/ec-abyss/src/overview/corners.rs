// SPDX-License-Identifier: AGPL-3.0-only
//! Hot corners under `mode "de"` (COMP-05 §7.1, `general.hot-corners.*`): the
//! pointer dwelling in a screen corner runs an action.
//!
//! The corner rectangles are precomputed per output by [`rebuild`], which
//! `shell::arrange` calls, so the pointer path only compares a point against a
//! short list: [`motion`] allocates nothing. Only an outer corner counts -- one
//! with another output beside it is a place the pointer crosses, not a corner.
//!
//! **Dwell.** Entering a corner arms it and a calloop timer waits out
//! `general.hot-corner-dwell`; nothing blocks. Leaving before it fires
//! cancels the timer. Once an action has fired, the pointer must leave and
//! come back to fire again, and no corner fires until `general.hot-corner-cooldown`
//! has passed.
//!
//! **Suppressed** (nothing arms, an armed corner disarms) during fullscreen on
//! the output, any pointer grab or drag, while a button is held (which is a
//! grab), under the session lock, while a prompt or the region selector holds
//! the seat, and while the switcher, overview, preview or window menu is up.
//! Under a hot corner's own overlay the pointer is already the overlay's.

use std::time::{Duration, Instant};

use smithay::{
    reexports::calloop::{
        timer::{TimeoutAction, Timer},
        LoopHandle, RegistrationToken,
    },
    utils::{Logical, Point, Rectangle, Size},
};

use crate::{
    config::{HotCorner, CORNER_BOTTOM_LEFT, CORNER_BOTTOM_RIGHT, CORNER_TOP_LEFT, CORNER_TOP_RIGHT},
    state::AbyssState,
};

/// How far into the screen a corner reaches, logical px. The pointer is clamped
/// to the output, so a flick into the corner lands inside this square.
pub const SIZE: i32 = 3;

type Rect = Rectangle<i32, Logical>;

/// One output's corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CornerRect {
    pub output: u64,
    /// `CORNER_*`.
    pub corner: usize,
    pub rect: Rect,
}

#[derive(Debug)]
struct Armed {
    corner: usize,
    output: u64,
    /// Which arming a timer belongs to.
    serial: u64,
    fired: bool,
    timer: Option<RegistrationToken>,
}

/// The corners, the one that is armed and when the last one fired.
#[derive(Debug, Default)]
pub struct Corners {
    rects: Vec<CornerRect>,
    armed: Option<Armed>,
    last_fire: Option<Instant>,
    serial: u64,
}

/// What the pointer path should do after [`Corners::enter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arm {
    /// Nothing changed.
    Nothing,
    /// Wait `dwell`, then ask [`Corners::due`] with `serial`.
    Wait(Duration, u64),
    /// No dwell is configured: the action is due now.
    Now(usize),
}

impl Corners {
    pub fn rects(&self) -> &[CornerRect] {
        &self.rects
    }

    /// The serial of the current arming, for the tests that stand in for its
    /// timer.
    #[cfg(test)]
    pub(crate) fn serial_for_test(&self) -> u64 {
        self.armed.as_ref().map_or(0, |a| a.serial)
    }

    /// The corner currently armed and waiting.
    pub fn armed(&self) -> Option<usize> {
        self.armed.as_ref().filter(|a| !a.fired).map(|a| a.corner)
    }

    /// Replace the rectangles (the outputs changed). An armed corner whose
    /// rectangle is gone is dropped.
    pub fn set_rects(&mut self, rects: Vec<CornerRect>, handle: &LoopHandle<'static, AbyssState>) {
        if self
            .armed
            .as_ref()
            .is_some_and(|a| !rects.iter().any(|r| r.output == a.output && r.corner == a.corner))
        {
            self.disarm(handle);
        }
        self.rects = rects;
    }

    /// The corner whose rectangle holds `pos`. Allocates nothing.
    pub fn at(&self, pos: Point<f64, Logical>) -> Option<&CornerRect> {
        self.rects.iter().find(|c| c.rect.to_f64().contains(pos))
    }

    /// Stop waiting and forget the arming.
    pub fn disarm(&mut self, handle: &LoopHandle<'static, AbyssState>) {
        if let Some(a) = self.armed.take() {
            if let Some(t) = a.timer {
                handle.remove(t);
            }
        }
    }

    /// The pointer is in `corner` of `output` at `now`. Arms it when it was not
    /// already armed (or has fired and left). `dwell` and `cooldown` come from
    /// the config. The caller inserts the timer a `Wait` asks for and hands its
    /// token to [`Corners::set_timer`].
    pub fn enter(
        &mut self,
        output: u64,
        corner: usize,
        dwell: Duration,
        cooldown: Duration,
        now: Instant,
    ) -> Arm {
        if self
            .armed
            .as_ref()
            .is_some_and(|a| a.corner == corner && a.output == output)
        {
            return Arm::Nothing;
        }
        if self
            .last_fire
            .is_some_and(|t| now.saturating_duration_since(t) < cooldown)
        {
            // In the cooldown the corner does not arm; the pointer must leave
            // and come back once it is over.
            self.armed = Some(Armed {
                corner,
                output,
                serial: 0,
                fired: true,
                timer: None,
            });
            return Arm::Nothing;
        }
        self.serial += 1;
        self.armed = Some(Armed {
            corner,
            output,
            serial: self.serial,
            fired: false,
            timer: None,
        });
        if dwell.is_zero() {
            if let Some(a) = &mut self.armed {
                a.fired = true;
            }
            self.last_fire = Some(now);
            Arm::Now(corner)
        } else {
            Arm::Wait(dwell, self.serial)
        }
    }

    pub fn set_timer(&mut self, serial: u64, token: RegistrationToken) {
        if let Some(a) = self.armed.as_mut().filter(|a| a.serial == serial) {
            a.timer = Some(token);
        }
    }

    /// The dwell timer of arming `serial` fired and the pointer is still in
    /// `still` (the corner under it now). The corner to run, once.
    pub fn due(&mut self, serial: u64, still: Option<(u64, usize)>, now: Instant) -> Option<usize> {
        let a = self.armed.as_mut().filter(|a| a.serial == serial && !a.fired)?;
        a.timer = None;
        if still != Some((a.output, a.corner)) {
            // The pointer is not there any more: this arming is over.
            self.armed = None;
            return None;
        }
        a.fired = true;
        self.last_fire = Some(now);
        Some(a.corner)
    }
}

/// Recompute the corner rectangles of every enabled output. Called by
/// `shell::arrange`.
pub fn rebuild(state: &mut AbyssState) {
    let geos: Vec<(u64, Rect)> = state
        .outputs
        .iter()
        .filter(|e| e.enabled)
        .filter_map(|e| state.space.output_geometry(&e.output).map(|g| (e.id, g)))
        .collect();
    let covered = |p: Point<i32, Logical>| geos.iter().any(|(_, g)| g.contains(p));
    let mut rects = Vec::with_capacity(geos.len() * 4);
    for (id, g) in &geos {
        let (l, t, r, b) = (g.loc.x, g.loc.y, g.loc.x + g.size.w, g.loc.y + g.size.h);
        let s = SIZE.min(g.size.w).min(g.size.h);
        // (corner, rectangle, the two points just outside along each edge)
        let corners = [
            (CORNER_TOP_LEFT, (l, t), (l - 1, t), (l, t - 1)),
            (CORNER_TOP_RIGHT, (r - s, t), (r, t), (r - 1, t - 1)),
            (CORNER_BOTTOM_LEFT, (l, b - s), (l - 1, b - 1), (l, b)),
            (CORNER_BOTTOM_RIGHT, (r - s, b - s), (r, b - 1), (r - 1, b)),
        ];
        for (corner, loc, side_a, side_b) in corners {
            if covered(side_a.into()) || covered(side_b.into()) {
                continue;
            }
            rects.push(CornerRect {
                output: *id,
                corner,
                rect: Rectangle::new(loc.into(), Size::from((s, s))),
            });
        }
    }
    let handle = state.loop_handle.clone();
    state.corners.set_rects(rects, &handle);
}

/// Whether hot corners may act right now.
fn suppressed(state: &AbyssState, output: u64) -> bool {
    !super::allowed(state)
        || super::any_open(state)
        || state.winmenu.is_open()
        || crate::input::grabs::drag_active(state)
        || state
            .outputs
            .get(output)
            .is_none_or(|e| crate::shell::output_has_fullscreen(state, &e.output))
}

/// The pointer is at `pos`. Called from the pointer path for every motion;
/// allocates nothing when no corner is involved.
pub fn motion(state: &mut AbyssState, pos: Point<f64, Logical>) {
    if state.corners.rects.is_empty() {
        return;
    }
    let Some(hit) = state.corners.at(pos).copied() else {
        if state.corners.armed.is_some() {
            let handle = state.loop_handle.clone();
            state.corners.disarm(&handle);
        }
        return;
    };
    if suppressed(state, hit.output) {
        let handle = state.loop_handle.clone();
        state.corners.disarm(&handle);
        return;
    }
    let g = &state.config.general;
    if g.corner_action(hit.corner, state.config.mode) == HotCorner::Off {
        return;
    }
    let dwell = Duration::from_millis(u64::from(g.hot_corner_dwell_ms));
    let cooldown = Duration::from_millis(u64::from(g.hot_corner_cooldown_ms));
    match state
        .corners
        .enter(hit.output, hit.corner, dwell, cooldown, Instant::now())
    {
        Arm::Nothing => {}
        Arm::Now(corner) => run(state, corner),
        Arm::Wait(dwell, serial) => {
            let token = state
                .loop_handle
                .insert_source(Timer::from_duration(dwell), move |_, _, state| {
                    crate::contain::contain(state, crate::contain::Feature::Overview, |s| fire(s, serial));
                    TimeoutAction::Drop
                })
                .ok();
            if let Some(token) = token {
                state.corners.set_timer(serial, token);
            }
        }
    }
}

/// The dwell timer of arming `serial` ran out.
pub fn fire(state: &mut AbyssState, serial: u64) {
    crate::contain::fault(crate::contain::Feature::Overview);
    let still = state
        .corners
        .at(state.pointer_location)
        .map(|c| (c.output, c.corner));
    if still.is_some_and(|(output, _)| suppressed(state, output)) {
        // Drop the arming, so the next motion in the corner arms it afresh.
        let handle = state.loop_handle.clone();
        state.corners.disarm(&handle);
        return;
    }
    if let Some(corner) = state.corners.due(serial, still, Instant::now()) {
        run(state, corner);
    }
}

fn run(state: &mut AbyssState, corner: usize) {
    match state.config.general.corner_action(corner, state.config.mode) {
        HotCorner::ToggleOverview => {
            tracing::info!(corner, "hot corner: overview");
            super::toggle(state);
        }
        HotCorner::SwitchWindows => {
            tracing::info!(corner, "hot corner: window switcher");
            super::switcher::open(state, false);
        }
        HotCorner::Off | HotCorner::Auto => {}
    }
}
