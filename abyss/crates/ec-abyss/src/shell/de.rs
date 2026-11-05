// SPDX-License-Identifier: AGPL-3.0-only
//! `mode "de"` window management (COMP-17 §2, COMP-05 §3.1, C-24, ADR 0090).
//!
//! Under `de` windows stack and nothing is tiled. That follows the mode; it is
//! not a default a config key overrides. Moving between modes is live (hot
//! reload), so the flip is done here:
//!
//! - entering `de` floats every tiled window on every human workspace of every
//!   output at the rect it had, and keeps each workspace's tree aside
//!   ([`Workspace::stash`]) and a list of what it lifted ([`DeLift`]);
//! - leaving `de` first ends every maximize ([`super::maximize::clear_all`]:
//!   pre-maximize rectangle back, client told), then puts those windows back into the tree they came from, with
//!   their weights, and places the windows opened meanwhile the way
//!   `wm`/`hybrid` would have (dialogs and fixed-size windows stay floating).
//!
//! `wm` <-> `hybrid` touches no window. Windows are kept as `Window` keys,
//! like `AbyssState::fullscreen`; nothing here shares state behind a cell.

use smithay::{
    desktop::{layer_map_for_output, Window},
    utils::{Logical, Point, Rectangle, Size},
};

use crate::{
    config::{LayoutKind, Mode},
    state::AbyssState,
};

use super::{
    tiling_area,
    workspace::{self, Floating},
    RetileHold,
};

/// What leaving `de` has to undo. Cleared on every flip.
#[derive(Debug, Default)]
pub struct DeLift {
    /// Windows that were tiled (or meant to be: minimized or fullscreen out of
    /// a tile) when `de` began, each with the Radiant weight it had.
    lifted: Vec<(Window, Option<f64>)>,
    /// Windows mapped while `de` was on.
    opened: Vec<Window>,
    /// Maximized windows handed over by an output that went away, until the
    /// output that adopts them places them (`shell::arrange_output`).
    carried: Vec<(Window, workspace::Placement)>,
}

impl DeLift {
    /// A window is gone for good (`shell::unmap_window`).
    pub fn forget(&mut self, window: &Window) {
        self.lifted.retain(|(w, _)| w != window);
        self.opened.retain(|w| w != window);
        self.carried.retain(|(w, _)| w != window);
    }

    pub(crate) fn carry(&mut self, window: Window, placement: workspace::Placement) {
        self.carried.retain(|(w, _)| *w != window);
        self.carried.push((window, placement));
    }

    pub(super) fn take_carried(&mut self, window: &Window) -> Option<workspace::Placement> {
        let i = self.carried.iter().position(|(w, _)| w == window)?;
        Some(self.carried.remove(i).1)
    }

    pub(super) fn drop_carried(&mut self) {
        self.carried.clear();
    }

    fn note_lifted(&mut self, window: &Window, weight: Option<f64>) {
        if !self.lifted.iter().any(|(w, _)| w == window) {
            self.lifted.push((window.clone(), weight));
        }
    }

    pub(super) fn note_opened(&mut self, window: &Window) {
        if !self.opened.contains(window) {
            self.opened.push(window.clone());
        }
    }

    fn weight_of(&self, window: &Window) -> Option<f64> {
        self.lifted
            .iter()
            .find(|(w, _)| w == window)
            .and_then(|(_, wt)| *wt)
    }

    /// Nothing recorded: the state before `de` and after leaving it.
    #[cfg(test)]
    pub(crate) fn forget_is_clean(&self) -> bool {
        self.lifted.is_empty() && self.opened.is_empty() && self.carried.is_empty()
    }

    fn is_lifted(&self, window: &Window) -> bool {
        self.lifted.iter().any(|(w, _)| w == window)
    }
}

/// Do windows stack (no tiling at all) in the running mode?
pub fn stacking(state: &AbyssState) -> bool {
    state.config.mode.profile().stacking
}

/// Default floating rect for a window that has none: half the area, placed by
/// `general.floating-placement`. What `place_new_window` gives a new window.
pub(super) fn default_float_rect(
    state: &AbyssState,
    id: u64,
    area: Rectangle<i32, Logical>,
) -> Rectangle<i32, Logical> {
    let size = Size::from((area.size.w / 2, area.size.h / 2));
    let already = state
        .outputs
        .get(id)
        .map_or(0, |e| e.workspaces[e.active].floating.len());
    let loc = super::floating_origin(
        state.config.general.floating_placement,
        area,
        size,
        state.pointer_location,
        already,
    );
    Rectangle::new(loc, size)
}

/// A config reload changed the mode: flip the windows (COMP-17 §2). Called with
/// the new config already installed, before the arrange that follows it.
pub fn mode_changed(state: &mut AbyssState, old: Mode, new: Mode) {
    match (old.profile().stacking, new.profile().stacking) {
        (false, true) => enter(state),
        (true, false) => leave(state),
        _ => {}
    }
}

fn output_ids(state: &AbyssState) -> Vec<u64> {
    state.outputs.iter().map(|e| e.id).collect()
}

fn area_of(state: &AbyssState, id: u64) -> Option<Rectangle<i32, Logical>> {
    let output = state.outputs.get(id)?.output.clone();
    layer_map_for_output(&output).arrange();
    Some(tiling_area(state, &output))
}

/// Float every tile of workspace `ws` on output `id` at the rect the layout
/// gives it, the same lift `toggle-floating` does, below any window already
/// floating there (a dialog stays above its parent). `keep_tree`: keep the
/// emptied tree as the workspace's stash, unless it already has one.
fn lift_workspace(
    state: &mut AbyssState,
    id: u64,
    ws: usize,
    area: Rectangle<i32, Logical>,
    keep_tree: bool,
) {
    let gap_x = state.config.general.gaps_in;
    let gap_y = state.config.general.gaps_in_y();
    let fallback = state.config.layout_for(ws + 1);
    let Some(entry) = state.outputs.get_mut(id) else {
        return;
    };
    let w = &mut entry.workspaces[ws];
    // A window minimized out of a tile has no rect to lift; it floats when
    // restored, and is tiled again on leaving.
    let from_minimized: Vec<Window> = w
        .minimized
        .iter()
        .filter(|m| m.was_floating.is_none())
        .map(|m| m.window.clone())
        .collect();
    let kind = w.layout.unwrap_or(fallback);
    let rects = match kind {
        LayoutKind::Radiant => w.tiled.radiant(area, gap_x, gap_y),
        LayoutKind::Dwindle => w.tiled.dwindle(area, gap_x, gap_y),
        LayoutKind::Master => w.tiled.master(area, gap_x, gap_y),
    };
    let lifted: Vec<(Floating, Option<f64>)> = rects
        .into_iter()
        .map(|(window, rect)| {
            let weight = w.tiled.weight_of(&window);
            (
                Floating {
                    window,
                    rect,
                    weight,
                    placement: workspace::Placement::Free,
                },
                weight,
            )
        })
        .collect();
    let tree = std::mem::take(&mut w.tiled);
    if keep_tree && !lifted.is_empty() && w.stash.is_none() {
        w.stash = Some(tree);
    }
    let noted: Vec<(Window, Option<f64>)> = lifted.iter().map(|(f, wt)| (f.window.clone(), *wt)).collect();
    w.floating.splice(0..0, lifted.into_iter().map(|(f, _)| f));
    for (window, weight) in noted {
        state.de.note_lifted(&window, weight);
    }
    for window in from_minimized {
        state.de.note_lifted(&window, None);
    }
}

/// `wm`/`hybrid` -> `de`.
fn enter(state: &mut AbyssState) {
    super::cancel_tile_drag(state);
    state.retile_hold = RetileHold::None;
    for id in output_ids(state) {
        let Some(area) = area_of(state, id) else { continue };
        for ws in 0..workspace::COUNT {
            lift_workspace(state, id, ws, area, true);
        }
    }
    // A fullscreen window that came out of a tile goes back to one.
    #[allow(clippy::mutable_key_type)]
    let from_tile: Vec<Window> = state
        .fullscreen
        .iter()
        .filter(|(_, restore)| restore.is_none())
        .map(|(w, _)| w.clone())
        .collect();
    for w in from_tile {
        state.de.note_lifted(&w, None);
    }
    tracing::info!(lifted = state.de.lifted.len(), "mode de: tiled windows floated");
}

/// `de` -> `wm`/`hybrid`.
fn leave(state: &mut AbyssState) {
    // Maximize is a de-only placement: windows get their old rectangles back
    // before anything is re-tiled, so the tree sees where they really were.
    super::maximize::clear_all(state);
    super::cancel_tile_drag(state);
    state.retile_hold = RetileHold::None;
    let opened = std::mem::take(&mut state.de.opened);
    // Windows opened under `de` go through default placement now.
    let mut to_tile: Vec<Window> = Vec::new();
    for w in opened {
        if state.de.is_lifted(&w) || super::is_unmanaged_x11(state, &w) {
            continue;
        }
        if !super::rules::default_placement(state, &w).float {
            to_tile.push(w);
        }
    }
    for id in output_ids(state) {
        let Some(area) = area_of(state, id) else { continue };
        let pointer = state.pointer_location;
        for ws in 0..workspace::COUNT {
            let Some(entry) = state.outputs.get(id) else { break };
            // Everything this workspace holds that goes back into the tree:
            // floating, not fullscreen, and either lifted or newly tiled.
            let back: Vec<Window> = entry.workspaces[ws]
                .floating
                .iter()
                .map(|f| f.window.clone())
                .filter(|w| {
                    (state.de.is_lifted(w) || to_tile.contains(w)) && !state.fullscreen.contains_key(w)
                })
                .collect();
            let entry = state.outputs.get_mut(id).expect("checked above");
            let w = &mut entry.workspaces[ws];
            let strays = w.tiled.windows();
            let stash = w.stash.take();
            let had_stash = stash.is_some();
            let mut tree = stash.unwrap_or_else(|| std::mem::take(&mut w.tiled));
            // The stash only keeps what is still lifted and floating here.
            if had_stash {
                for win in tree.windows() {
                    if back.contains(&win) {
                        w.floating.retain(|f| f.window != win);
                    } else {
                        tree.remove(&win);
                    }
                }
            }
            for win in back {
                if tree.contains(&win) {
                    continue;
                }
                w.floating.retain(|f| f.window != win);
                tree.insert(win.clone(), None, area, pointer);
                if let Some(wt) = state.de.weight_of(&win) {
                    tree.set_weight(&win, wt);
                }
            }
            for win in strays {
                if !tree.contains(&win) {
                    tree.insert(win, None, area, pointer);
                }
            }
            w.tiled = tree;
            // Restoring a window minimized out of its tile tiles it again.
            for m in w.minimized.iter_mut() {
                if state.de.is_lifted(&m.window) || to_tile.contains(&m.window) {
                    m.was_floating = None;
                    m.weight = state.de.weight_of(&m.window);
                }
            }
        }
    }
    #[allow(clippy::mutable_key_type)]
    for (w, restore) in state.fullscreen.iter_mut() {
        if state.de.is_lifted(w) || to_tile.contains(w) {
            *restore = None;
        }
    }
    state.de = DeLift::default();
    // Keep above/below, shade and sticky are `de` only (`shell::winops`).
    state.winops.clear();
    state.winmenu.close();
    super::edge::grab_ended(state);
    tracing::info!("mode de left: tiled windows put back");
}

/// Float whatever is tiled on the active workspace of `id` at its layout rect.
/// Anything that tiles under `de` (a path nobody guarded) ends up here, so the
/// invariant holds whatever the caller did.
pub(super) fn lift_stray(state: &mut AbyssState, id: u64, area: Rectangle<i32, Logical>) {
    let Some(ws) = state.outputs.get(id).map(|e| e.active) else {
        return;
    };
    if state
        .outputs
        .get(id)
        .is_some_and(|e| e.workspaces[ws].tiled.is_empty())
    {
        return;
    }
    lift_workspace(state, id, ws, area, false);
}

/// Where a floating window lands on a new output's workspace: its rect moved
/// by the offset between the two outputs and kept inside `area`.
pub(super) fn carried_rect(
    rect: Rectangle<i32, Logical>,
    offset: Point<i32, Logical>,
    area: Rectangle<i32, Logical>,
) -> Rectangle<i32, Logical> {
    let max_x = (area.loc.x + area.size.w - rect.size.w).max(area.loc.x);
    let max_y = (area.loc.y + area.size.h - rect.size.h).max(area.loc.y);
    let loc = rect.loc + offset;
    Rectangle::new(
        Point::from((loc.x.clamp(area.loc.x, max_x), loc.y.clamp(area.loc.y, max_y))),
        rect.size,
    )
}
