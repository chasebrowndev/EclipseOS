// SPDX-License-Identifier: AGPL-3.0-only
//! Radiant drag-to-tile: drop candidates, aiming and the tile drag state
//! machine (COMP-05 §3.1).

use super::*;

/// Where a Radiant drag would land if released now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DropKey {
    /// Back to the window's own placeholder (or, for a floating window, off
    /// the output): nothing changes and the window snaps back.
    Stay,
    /// A screen-edge band: a full-height column or full-width row.
    Band(layout::Side),
    /// A zone of `TileDrag::tiles[i]`.
    Tile(usize, layout::Zone),
}

/// The drop target and the rectangle the window would get there, looked up
/// in `TileDrag::ghosts` when the target changes: motion never allocates.
#[derive(Debug, Clone, Copy)]
pub struct DropPreview {
    pub key: DropKey,
    /// The landing tile, global logical coordinates. `None` when nothing
    /// would tile (a floating window aimed off the output, or Super up).
    pub ghost: Option<Rectangle<i32, Logical>>,
}

/// One place a Radiant drag could land, precomputed at drag start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DropCandidate {
    pub key: DropKey,
    /// What the dragged window's centre is compared against: the landing
    /// rectangle's centre, or for an edge band the centre of the edge strip.
    pub aim: Point<f64, Logical>,
    /// Where the window lands, global logical coordinates.
    pub ghost: Rectangle<i32, Logical>,
}

/// Hysteresis for [`aim_drop`], logical px: a new candidate has to be this
/// much nearer than the current one before the target changes, so a window
/// resting between two candidates does not flicker.
pub const AIM_HYSTERESIS: f64 = 24.0;

/// A window being dragged onto the Radiant tree (COMP-05 §3.1).
///
/// A tiled window keeps its leaf as a placeholder for the whole drag, so the
/// other tiles hold still while the human aims; `tiles` is their layout
/// captured when the drag started. A floating window only gets one while the
/// main modifier (Super) is held.
#[derive(Debug)]
pub struct TileDrag {
    pub window: Window,
    /// The output the drag started on; drops land on its active workspace.
    pub output: Output,
    id: u64,
    ws: usize,
    /// The window was floating when the drag started.
    pub floating: bool,
    /// Whether a drop would tile it: always for a tiled window, and for a
    /// floating one only while Super is held. Releasing and re-pressing Super
    /// toggles this without recomputing anything.
    pub armed: bool,
    /// The output's global logical geometry.
    pub bounds: Rectangle<i32, Logical>,
    /// The tiling area the tiles were laid out in.
    pub area: Rectangle<i32, Logical>,
    /// Edge band width, logical px; 0 disables the bands.
    pub band: i32,
    pub tiles: Vec<(Window, Rectangle<i32, Logical>)>,
    /// Every place the drag could land, computed once at drag start. Motion
    /// only scans this.
    candidates: Vec<DropCandidate>,
    pub preview: DropPreview,
}

impl ec_abyss_render::drop::DropGuides for TileDrag {
    fn armed(&self) -> bool {
        self.armed
    }
    fn output(&self) -> &Output {
        &self.output
    }
    fn ghost(&self) -> Option<Rectangle<i32, Logical>> {
        self.preview.ghost
    }
    fn tiles(&self) -> &[(Window, Rectangle<i32, Logical>)] {
        &self.tiles
    }
    fn area(&self) -> Rectangle<i32, Logical> {
        self.area
    }
    fn band(&self) -> i32 {
        self.band
    }
}

fn main_mod_held(state: &AbyssState) -> bool {
    state.seat.get_keyboard().is_some_and(|k| k.modifier_state().logo)
}

fn centre(r: Rectangle<i32, Logical>) -> Point<f64, Logical> {
    Point::from((
        r.loc.x as f64 + r.size.w as f64 / 2.0,
        r.loc.y as f64 + r.size.h as f64 / 2.0,
    ))
}

/// The edge strip of `area`, `band` wide, that aims at a band drop.
fn band_strip(area: Rectangle<i32, Logical>, side: layout::Side, band: i32) -> Rectangle<i32, Logical> {
    let bw = band.min(area.size.w);
    let bh = band.min(area.size.h);
    let (x, y, w, h) = (area.loc.x, area.loc.y, area.size.w, area.size.h);
    let (loc, size) = match side {
        layout::Side::Left => ((x, y), (bw, h)),
        layout::Side::Right => ((x + w - bw, y), (bw, h)),
        layout::Side::Top => ((x, y), (w, bh)),
        layout::Side::Bottom => ((x, y + h - bh), (w, bh)),
    };
    Rectangle::new(Point::from(loc), Size::from(size))
}

/// The drop target for a dragged window centred at `aim` (COMP-05 §3.1):
/// the candidate whose aim point is nearest, keeping `current` unless another
/// is nearer by more than [`AIM_HYSTERESIS`]. Off the output is `Stay`. Pure
/// and allocation free: it runs on every motion.
pub(crate) fn aim_drop(
    bounds: Rectangle<i32, Logical>,
    candidates: &[DropCandidate],
    current: DropKey,
    aim: Point<f64, Logical>,
) -> DropKey {
    if !bounds.to_f64().contains(aim) {
        return DropKey::Stay;
    }
    let dist = |c: &DropCandidate| {
        let (dx, dy) = (c.aim.x - aim.x, c.aim.y - aim.y);
        (dx * dx + dy * dy).sqrt()
    };
    let Some(best) = candidates.iter().min_by(|a, b| dist(a).total_cmp(&dist(b))) else {
        return DropKey::Stay;
    };
    match candidates.iter().find(|c| c.key == current) {
        Some(cur) if dist(best) + AIM_HYSTERESIS >= dist(cur) => current,
        _ => best.key,
    }
}

/// Apply a drop to `tree`. False, with `tree` untouched, when nothing would
/// change or the target has gone away since the drag started.
pub(crate) fn apply_drop<W: Clone + PartialEq>(
    tree: &mut layout::Tree<W>,
    window: &W,
    key: DropKey,
    tiles: &[(W, Rectangle<i32, Logical>)],
) -> bool {
    use layout::{Side, Target};
    match key {
        DropKey::Stay => false,
        DropKey::Band(side) => {
            tree.remove(window);
            tree.insert_at(window.clone(), Target::Root, side, 1.0)
        }
        DropKey::Tile(i, zone) => {
            let Some((target, rect)) = tiles.get(i) else {
                return false;
            };
            if target == window || !tree.contains(target) {
                return false;
            }
            match zone.side() {
                // The centre swaps two tiles. A floating window has no tile to
                // swap into, so it splits the target dwindle-style instead.
                None if tree.contains(window) => {
                    tree.swap(window, target);
                    true
                }
                None => {
                    let side = if rect.size.w >= rect.size.h {
                        Side::Right
                    } else {
                        Side::Bottom
                    };
                    tree.insert_at(window.clone(), Target::Leaf(target), side, 1.0)
                }
                Some(side) => {
                    tree.remove(window);
                    tree.insert_at(window.clone(), Target::Leaf(target), side, 1.0)
                }
            }
        }
    }
}

/// Every place `window` could land: back home on its own tile (when it is
/// tiled), each other tile's five zones, and the four edge bands (when `band`
/// is non-zero). Drops that would change nothing are left out. `Stay` comes
/// first, so an exact tie goes home.
pub(crate) fn drop_candidates<W: Clone + PartialEq>(
    tree: &layout::Tree<W>,
    window: &W,
    tiles: &[(W, Rectangle<i32, Logical>)],
    area: Rectangle<i32, Logical>,
    gap_x: i32,
    gap_y: i32,
    band: i32,
) -> Vec<DropCandidate> {
    use layout::{Side, Zone};
    const ZONES: [Zone; 5] = [Zone::Left, Zone::Right, Zone::Top, Zone::Bottom, Zone::Center];
    const SIDES: [Side; 4] = [Side::Left, Side::Right, Side::Top, Side::Bottom];
    let home = tiles
        .iter()
        .find(|(w, _)| w == window)
        .map(|(_, r)| DropCandidate {
            key: DropKey::Stay,
            aim: centre(*r),
            ghost: *r,
        });
    let bands = SIDES.iter().filter(|_| band > 0).map(|s| DropKey::Band(*s));
    let on_tiles = tiles
        .iter()
        .enumerate()
        .filter(|(_, (w, _))| w != window)
        .flat_map(|(i, _)| ZONES.iter().map(move |z| DropKey::Tile(i, *z)));
    let drops = bands.chain(on_tiles).filter_map(|key| {
        let mut t = tree.clone();
        if !apply_drop(&mut t, window, key, tiles) {
            return None;
        }
        let ghost = t
            .radiant(area, gap_x, gap_y)
            .into_iter()
            .find(|(w, _)| w == window)?
            .1;
        let aim = match key {
            DropKey::Band(side) => centre(band_strip(area, side, band)),
            _ => centre(ghost),
        };
        Some(DropCandidate { key, aim, ghost })
    });
    home.into_iter().chain(drops).collect()
}

/// Start a Radiant drag for `window` if it qualifies: tiled (or floating with
/// Super held) on the active workspace of a Radiant output, and neither
/// maximized nor fullscreen.
fn start_tile_drag(state: &mut AbyssState, window: &Window) {
    if state.lock.locked || state.maximized.contains_key(window) || state.fullscreen.contains_key(window) {
        return;
    }
    let Some(id) = output_of_window(state, window) else {
        return;
    };
    let Some((ws, LayoutKind::Radiant)) = active_layout(state, id) else {
        return;
    };
    let entry = state.outputs.get(id).expect("just resolved");
    let floating = if entry.workspaces[ws].tiled.contains(window) {
        false
    } else if entry.workspaces[ws].floating.iter().any(|f| &f.window == window) && main_mod_held(state) {
        true
    } else {
        return;
    };
    // Every allocation of the drag happens here, once per drag, and in
    // `drop_window` on release; motion only looks up `ghosts`. The float-on-
    // first-motion path this replaces allocated on every motion.
    let output = entry.output.clone();
    let area = tiling_area(state, &output);
    let bounds = state.space.output_geometry(&output).unwrap_or_default();
    let gap_x = state.config.general.gaps_in;
    let gap_y = state.config.general.gaps_in_y();
    let band = state.config.general.drop_edge_band;
    let tree = &state.outputs.get(id).expect("just resolved").workspaces[ws].tiled;
    let tiles = tree.radiant(area, gap_x, gap_y);
    let candidates = drop_candidates(tree, window, &tiles, area, gap_x, gap_y, band);
    if !floating {
        // The tile crop would clip the window to where it used to be.
        set_tile_clip(window, None);
    }
    state.tile_drag = Some(TileDrag {
        window: window.clone(),
        output,
        id,
        ws,
        floating,
        armed: true,
        bounds,
        area,
        band,
        tiles,
        candidates,
        preview: DropPreview {
            key: DropKey::Stay,
            ghost: None,
        },
    });
    tracing::debug!(floating, "radiant drag started");
    crate::backend::damage_all(state);
}

/// Re-aim the drag at a window centred at `aim`: a scan of the precomputed
/// candidates. No allocation.
fn update_drop_target(state: &mut AbyssState, aim: Point<f64, Logical>) {
    let Some(drag) = state.tile_drag.as_mut() else {
        return;
    };
    let key = if drag.armed {
        aim_drop(drag.bounds, &drag.candidates, drag.preview.key, aim)
    } else {
        DropKey::Stay
    };
    let ghost = drag
        .candidates
        .iter()
        .find(|c| c.key == key)
        .map(|c| c.ghost)
        .filter(|_| drag.armed);
    if key == drag.preview.key && ghost == drag.preview.ghost {
        return;
    }
    drag.preview = DropPreview { key, ghost };
    crate::backend::damage_all(state);
}

/// One motion of an interactive move (COMP-05 §3.1). Under Radiant a tiled
/// window is not floated: the grab draws it at `loc` over its placeholder
/// and the drop target follows the window's centre. Returns `true` when it handled the
/// motion; `false` means the caller floats/moves the window as before, which
/// is also what a floating window gets (with the guides aimed while Super is
/// held).
pub fn drag_tile(state: &mut AbyssState, window: &Window, loc: Point<i32, Logical>) -> bool {
    if !state.tile_drag.as_ref().is_some_and(|d| &d.window == window) {
        start_tile_drag(state, window);
    }
    let Some(drag) = state.tile_drag.as_ref().filter(|d| &d.window == window) else {
        return false;
    };
    let aim = centre(Rectangle::new(loc, window.geometry().size));
    if drag.floating {
        let armed = main_mod_held(state);
        if let Some(drag) = state.tile_drag.as_mut() {
            drag.armed = armed;
        }
        update_drop_target(state, aim);
        return false;
    }
    if state.lock.locked {
        return true;
    }
    update_drop_target(state, aim);
    state.space.map_element(window.clone(), loc, false);
    true
}

/// The end of an interactive move: land the window on the previewed drop
/// target, so the drop is exactly what the guides showed, or snap it back to
/// its placeholder. A floating window only tiles if Super is still held.
/// Always ends the drag.
pub fn drop_window(state: &mut AbyssState, window: &Window) {
    let Some(drag) = state.tile_drag.take() else {
        return;
    };
    let tile_it = &drag.window == window && !state.lock.locked && (!drag.floating || main_mod_held(state));
    if tile_it {
        let key = drag.preview.key;
        if let Some(entry) = state.outputs.get_mut(drag.id) {
            if entry.active == drag.ws {
                let ws = &mut entry.workspaces[drag.ws];
                let held = if drag.floating {
                    ws.floating.iter().any(|f| f.window == drag.window)
                } else {
                    ws.tiled.contains(&drag.window)
                };
                // Applied to a copy and committed whole, so a drop whose
                // target vanished mid-drag changes nothing at all.
                let mut tree = ws.tiled.clone();
                if held && apply_drop(&mut tree, &drag.window, key, &drag.tiles) {
                    ws.tiled = tree;
                    ws.floating.retain(|f| f.window != drag.window);
                    tracing::debug!(?key, "radiant drop");
                }
            }
        }
    }
    arrange(state);
}

/// Abandon a drag in flight (grab cancelled, session locked): the window goes
/// back to its placeholder.
pub fn cancel_tile_drag(state: &mut AbyssState) {
    if state.tile_drag.take().is_some() {
        arrange(state);
    }
}
