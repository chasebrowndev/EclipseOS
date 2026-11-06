// SPDX-License-Identifier: AGPL-3.0-only
//! Window operations under `mode "de"` (COMP-05 §3.1, §7.1, C-24, ADR 0090):
//! keep above, keep below, shade and sticky ("on all desktops").
//!
//! All four are per-window flags the human sets from the window menu
//! ([`super::winmenu`]). They live here as plain `Window` lists owned by
//! `AbyssState`, like `fullscreen` and `urgent`: nothing shares state behind a
//! cell, and none of them touches a workspace's own lists except sticky, which
//! moves a window between the workspaces of its output ([`sync_sticky`]).
//!
//! The flags are `de`-only. They are cleared when the mode is left
//! ([`super::de`]) and when a window closes ([`WinOps::forget`], called from
//! `shell::unmap_window`); [`WinOps::prune`] drops anything whose window died
//! without either. They survive minimize, a workspace move and an output move,
//! because none of those is a reason to forget what the human asked for.
//!
//! **Z-bands.** A workspace keeps its floating windows in stacking order
//! (`Workspace::floating`, last is topmost) and focusing raises by moving the
//! window to the end. `arrange_output` lays them out through [`sort_by_band`],
//! a *stable* sort by [`effective_band`] (a dialog takes its parent's band), so a keep-above window stays over every normal
//! window and a keep-below one under them, whatever was raised last; within a
//! band the order is the focus order. Fullscreen and trusted UI ordering are
//! not touched: `raise_fullscreen` runs after the bands, and trusted UI is not
//! in the space.
//!
//! **Shade.** A shaded window is rolled up to its bar. *What the client gets:*
//! nothing. The window keeps its frame rectangle and the client keeps the size
//! it was configured with, so no configure is sent when a window is shaded or
//! unshaded (in particular never a zero size, which a client may treat as "pick
//! your own"). The compositor draws only the bar and takes no pointer input in
//! the content area ([`element_under`]); the content is still there, unseen.
//! A shaded window cannot be resized ([`can_resize`]); maximize and fullscreen
//! unshade it ([`unshade`]).
//!
//! **Sticky.** A sticky window is shown on every workspace of *its output*. It
//! is not copied: it is the one entry in one workspace's lists, and
//! [`sync_sticky`] (run by every layout pass) moves it into the output's active
//! workspace, so every existing path (focus history, minimize, close, output
//! moves) sees exactly one window and nothing can go stale. Agent workspaces
//! (COMP-05 §3.2) are not among the ten human workspaces and are never touched.

use smithay::{
    desktop::Window,
    utils::{IsAlive, Logical, Point},
};

use crate::state::AbyssState;

use super::{arrange, de, titlebar};

/// Where a floating window sits in the stack relative to normal windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Band {
    Below,
    Normal,
    Above,
}

/// The per-window flags. See the module docs.
#[derive(Debug, Default)]
pub struct WinOps {
    above: Vec<Window>,
    below: Vec<Window>,
    sticky: Vec<Window>,
    shaded: Vec<Window>,
}

fn set(list: &mut Vec<Window>, window: &Window, on: bool) -> bool {
    let has = list.contains(window);
    match (on, has) {
        (true, false) => list.push(window.clone()),
        (false, true) => list.retain(|w| w != window),
        _ => return false,
    }
    true
}

impl WinOps {
    /// The band of `window`. No allocation.
    pub fn band(&self, window: &Window) -> Band {
        if self.above.contains(window) {
            Band::Above
        } else if self.below.contains(window) {
            Band::Below
        } else {
            Band::Normal
        }
    }

    pub fn is_sticky(&self, window: &Window) -> bool {
        self.sticky.contains(window)
    }

    /// No allocation: pointer motion asks this.
    pub fn is_shaded(&self, window: &Window) -> bool {
        !self.shaded.is_empty() && self.shaded.contains(window)
    }

    pub fn has_shaded(&self) -> bool {
        !self.shaded.is_empty()
    }

    pub fn has_bands(&self) -> bool {
        !self.above.is_empty() || !self.below.is_empty()
    }

    /// The window is gone for good (`shell::unmap_window`).
    pub fn forget(&mut self, window: &Window) {
        self.above.retain(|w| w != window);
        self.below.retain(|w| w != window);
        self.sticky.retain(|w| w != window);
        self.shaded.retain(|w| w != window);
    }

    /// Leaving `de`: every flag goes.
    pub fn clear(&mut self) {
        *self = WinOps::default();
    }

    /// Drop windows whose client is gone without `forget` having run.
    pub fn prune(&mut self) {
        for list in [
            &mut self.above,
            &mut self.below,
            &mut self.sticky,
            &mut self.shaded,
        ] {
            list.retain(|w| w.alive());
        }
    }

    pub fn is_empty(&self) -> bool {
        !self.has_bands() && self.sticky.is_empty() && self.shaded.is_empty()
    }
}

/// Stable sort of a bottom-to-top stack by band: below, then normal, then
/// above, each band keeping its own order.
pub fn sort_by_band<T>(stack: &mut [T], band: impl Fn(&T) -> Band) {
    stack.sort_by_key(|t| band(t));
}

/// The band `window` is stacked in: its own, or, when it has none, that of the
/// nearest ancestor that does. A dialog follows its parent, so a Save As box
/// of a keep-above editor is above the editor and not hidden behind it, and
/// a dialog of a keep-below window stays under the normal windows with it.
/// Bounded, so a malformed parent loop cannot hang the loop.
pub fn effective_band(state: &AbyssState, window: &Window) -> Band {
    let mut cur = window.clone();
    for _ in 0..8 {
        let own = state.winops.band(&cur);
        if own != Band::Normal {
            return own;
        }
        match super::parent_of(state, &cur) {
            Some(p) => cur = p,
            None => break,
        }
    }
    Band::Normal
}

fn on(state: &AbyssState) -> bool {
    de::stacking(state)
}

/// Re-lay out after a flag changed. Logs the flag and its new value, never the
/// window's title or app id.
fn changed(state: &mut AbyssState, flag: &'static str, value: bool) {
    tracing::info!(flag, value, "window operation");
    arrange(state);
    state.refresh_pointer_focus();
    crate::backend::damage_all(state);
}

/// Keep `window` above (or stop). Mutually exclusive with keep-below. False
/// when nothing changed (not `de`).
pub fn set_keep_above(state: &mut AbyssState, window: &Window, value: bool) -> bool {
    if !on(state) {
        return false;
    }
    let mut did = set(&mut state.winops.above, window, value);
    if value {
        did |= set(&mut state.winops.below, window, false);
    }
    if did {
        changed(state, "keep-above", value);
    }
    did
}

/// Keep `window` below (or stop). Mutually exclusive with keep-above.
pub fn set_keep_below(state: &mut AbyssState, window: &Window, value: bool) -> bool {
    if !on(state) {
        return false;
    }
    let mut did = set(&mut state.winops.below, window, value);
    if value {
        did |= set(&mut state.winops.above, window, false);
    }
    if did {
        changed(state, "keep-below", value);
    }
    did
}

/// Show `window` on every workspace of its output (or stop).
pub fn set_sticky(state: &mut AbyssState, window: &Window, value: bool) -> bool {
    if !on(state) {
        return false;
    }
    let did = set(&mut state.winops.sticky, window, value);
    if did {
        changed(state, "sticky", value);
    }
    did
}

/// Roll `window` up to its bar, or back. Rolling up needs a bar to roll up to
/// (a client-side-decorated window has none) and a window that is not
/// fullscreen; unrolling is always allowed.
pub fn set_shaded(state: &mut AbyssState, window: &Window, value: bool) -> bool {
    if !on(state) {
        return false;
    }
    if value
        && (state.fullscreen.contains_key(window)
            || titlebar::bar_height(state, window) == 0
            || super::is_minimized(state, window))
    {
        return false;
    }
    let did = set(&mut state.winops.shaded, window, value);
    if did {
        changed(state, "shade", value);
    }
    did
}

/// Unroll `window` ahead of a maximize or fullscreen, which restore it. The
/// caller lays out afterwards. True if it was shaded.
pub(super) fn unshade(state: &mut AbyssState, window: &Window) -> bool {
    set(&mut state.winops.shaded, window, false)
}

/// May `window` be resized by the human? A shaded one may not: it has no
/// content to size.
pub fn can_resize(state: &AbyssState, window: &Window) -> bool {
    !state.winops.is_shaded(window)
}

/// Toggle forms, for the menu.
pub fn toggle_keep_above(state: &mut AbyssState, window: &Window) -> bool {
    let to = state.winops.band(window) != Band::Above;
    set_keep_above(state, window, to)
}

pub fn toggle_keep_below(state: &mut AbyssState, window: &Window) -> bool {
    let to = state.winops.band(window) != Band::Below;
    set_keep_below(state, window, to)
}

pub fn toggle_sticky(state: &mut AbyssState, window: &Window) -> bool {
    let to = !state.winops.is_sticky(window);
    set_sticky(state, window, to)
}

pub fn toggle_shaded(state: &mut AbyssState, window: &Window) -> bool {
    let to = !state.winops.is_shaded(window);
    set_shaded(state, window, to)
}

/// Move every sticky window of output `id` that sits on an inactive workspace
/// into the active one, minimized ones included, so a sticky window is on
/// whichever workspace is showing. Runs at the top of every layout pass of
/// the output, which makes it hold across a workspace switch, a move to a
/// workspace, an unminimize and an adoption from a departed output.
pub(super) fn sync_sticky(state: &mut AbyssState, id: u64) {
    if state.winops.sticky.is_empty() || !on(state) {
        return;
    }
    let sticky = &state.winops.sticky;
    let Some(entry) = state.outputs.get_mut(id) else {
        return;
    };
    let active = entry.active;
    let mut floating = Vec::new();
    let mut minimized = Vec::new();
    for (i, ws) in entry.workspaces.iter_mut().enumerate() {
        if i == active {
            continue;
        }
        let mut k = 0;
        while k < ws.floating.len() {
            if sticky.contains(&ws.floating[k].window) {
                floating.push(ws.floating.remove(k));
            } else {
                k += 1;
            }
        }
        let mut k = 0;
        while k < ws.minimized.len() {
            if sticky.contains(&ws.minimized[k].window) {
                minimized.push(ws.minimized.remove(k));
            } else {
                k += 1;
            }
        }
    }
    let ws = &mut entry.workspaces[active];
    ws.floating.extend(floating);
    ws.minimized.extend(minimized);
}

/// Whether `window` stays on screen across a workspace switch.
pub fn stays_on_switch(state: &AbyssState, window: &Window) -> bool {
    on(state) && state.winops.is_sticky(window)
}

/// The topmost window with a surface at `pos`, like `Space::element_under` but
/// blind to shaded windows: their content is not drawn, so it is not there to
/// be hit. Returns the window and the position its surfaces are rendered at.
/// No allocation.
pub fn element_under(state: &AbyssState, pos: Point<f64, Logical>) -> Option<(&Window, Point<i32, Logical>)> {
    use smithay::desktop::space::SpaceElement;
    if !state.winops.has_shaded() {
        return state.space.element_under(pos);
    }
    state.space.elements().rev().find_map(|w| {
        if state.winops.is_shaded(w) {
            return None;
        }
        let bbox = state.space.element_bbox(w)?;
        if !bbox.to_f64().contains(pos) {
            return None;
        }
        let at = state.space.element_location(w)? - w.geometry().loc;
        w.is_in_input_region(&(pos - at.to_f64())).then_some((w, at))
    })
}
