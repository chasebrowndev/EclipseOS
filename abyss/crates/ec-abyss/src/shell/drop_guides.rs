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

/// One output a Radiant drag can land on, precomputed at drag start: its
/// tiling tree as it would be with the window in it. An output that does not
/// tile has no tiles or candidates, so a drop there floats the window.
#[derive(Debug)]
pub struct DropTarget {
    pub output: Output,
    id: u64,
    /// The workspace the drop lands on: the output's active one at drag start.
    ws: usize,
    /// The output's active layout is Radiant.
    radiant: bool,
    /// The output's global logical geometry.
    bounds: Rectangle<i32, Logical>,
    /// The tiling area the tiles were laid out in.
    area: Rectangle<i32, Logical>,
    tiles: Vec<(Window, Rectangle<i32, Logical>)>,
    /// Every place the drag could land on this output, computed once at drag
    /// start. Motion only scans this.
    candidates: Vec<DropCandidate>,
}

/// A window being dragged onto the Radiant tree (COMP-05 §3.1).
///
/// A tiled window keeps its leaf as a placeholder for the whole drag, so the
/// other tiles hold still while the human aims; the source output's tiles are
/// their layout captured when the drag started. A floating window only gets
/// one while the main modifier (Super) is held.
///
/// The drag aims at the output whose bounds hold the window's centre
/// (`targets[current]`). Its guides, ghost and drop follow that output, so a
/// window dragged across a seam lands on the tree it is over.
#[derive(Debug)]
pub struct TileDrag {
    pub window: Window,
    /// The output the drag started on; a cancelled drag goes back to it.
    id: u64,
    /// The window was floating when the drag started.
    pub floating: bool,
    /// Whether a drop would tile it: always for a tiled window, and for a
    /// floating one only while Super is held. Releasing and re-pressing Super
    /// toggles this without recomputing anything.
    pub armed: bool,
    /// Edge band width, logical px; 0 disables the bands.
    pub band: i32,
    /// The source output's entry in `targets`.
    source: usize,
    /// The output under the window's centre, an index into `targets`.
    current: usize,
    targets: Vec<DropTarget>,
    pub preview: DropPreview,
}

impl TileDrag {
    fn target(&self) -> &DropTarget {
        &self.targets[self.current]
    }
}

impl ec_abyss_render::drop::DropGuides for TileDrag {
    fn armed(&self) -> bool {
        self.armed
    }
    fn output(&self) -> &Output {
        &self.target().output
    }
    fn ghost(&self) -> Option<Rectangle<i32, Logical>> {
        self.preview.ghost
    }
    fn tiles(&self) -> &[(Window, Rectangle<i32, Logical>)] {
        &self.target().tiles
    }
    fn area(&self) -> Rectangle<i32, Logical> {
        self.target().area
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

/// Every output a Radiant drag of `window` could land on, precomputed here
/// once per drag. The source is always in the list; another output is left
/// out only when it is powered off (COMP-03 §4).
fn drop_targets(state: &AbyssState, source: u64, window: &Window) -> Vec<DropTarget> {
    let g = &state.config.general;
    let (gap_x, gap_y, band) = (g.gaps_in, g.gaps_in_y(), g.drop_edge_band);
    state
        .outputs
        .iter()
        .filter(|e| e.id == source || e.powered)
        .filter_map(|e| {
            let bounds = state.space.output_geometry(&e.output)?;
            let (ws, kind) = active_layout(state, e.id)?;
            let radiant = matches!(kind, LayoutKind::Radiant);
            let (area, tiles, candidates) = if radiant {
                let area = tiling_area(state, &e.output);
                let tree = &e.workspaces[ws].tiled;
                let tiles = tree.radiant(area, gap_x, gap_y);
                let candidates = drop_candidates(tree, window, &tiles, area, gap_x, gap_y, band);
                (area, tiles, candidates)
            } else {
                (Rectangle::default(), Vec::new(), Vec::new())
            };
            Some(DropTarget {
                output: e.output.clone(),
                id: e.id,
                ws,
                radiant,
                bounds,
                area,
                tiles,
                candidates,
            })
        })
        .collect()
}

/// Start a Radiant drag for `window` if it qualifies: tiled (or floating with
/// Super held) on the active workspace of a Radiant output, and neither
/// fullscreen.
fn start_tile_drag(state: &mut AbyssState, window: &Window) {
    // Tile-on-drop, with or without Super, is a tiling feature (COMP-05 §3.1).
    if state.lock.locked || state.fullscreen.contains_key(window) || super::de::stacking(state) {
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
    // `drop_window` on release; motion only scans the candidates.
    let targets = drop_targets(state, id, window);
    let Some(source) = targets.iter().position(|t| t.id == id) else {
        return;
    };
    if !floating {
        // The tile crop would clip the window to where it used to be.
        set_tile_clip(window, None);
    }
    let band = state.config.general.drop_edge_band;
    state.tile_drag = Some(TileDrag {
        window: window.clone(),
        id,
        floating,
        armed: true,
        band,
        source,
        current: source,
        targets,
        preview: DropPreview {
            key: DropKey::Stay,
            ghost: None,
        },
    });
    tracing::debug!(floating, "radiant drag started");
    crate::backend::damage_all(state);
}

/// Re-aim the drag at a window centred at `aim`. The output under the centre
/// is the target; the nearest candidate on it is the drop. A scan of the
/// precomputed candidates. Crossing onto another output re-tells the window
/// its scale (an Arc clone, on the rare crossing only).
fn update_drop_target(state: &mut AbyssState, aim: Point<f64, Logical>) {
    let Some(drag) = state.tile_drag.as_mut() else {
        return;
    };
    let current = drag
        .targets
        .iter()
        .position(|t| t.bounds.to_f64().contains(aim))
        .unwrap_or(drag.source);
    let retarget = current != drag.current;
    if retarget {
        drag.current = current;
        // Hysteresis is per output: a candidate on another tree means nothing.
        drag.preview.key = DropKey::Stay;
    }
    let t = drag.target();
    let key = if drag.armed {
        aim_drop(t.bounds, &t.candidates, drag.preview.key, aim)
    } else {
        DropKey::Stay
    };
    let ghost = t
        .candidates
        .iter()
        .find(|c| c.key == key)
        .map(|c| c.ghost)
        .filter(|_| drag.armed);
    if retarget {
        fractional_scale::update_window_scale(&drag.window, &t.output);
    }
    if !retarget && key == drag.preview.key && ghost == drag.preview.ghost {
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
/// its placeholder. A floating window only tiles if Super is still held. A
/// drop on another output lands there: tiled on that output's tree when it
/// tiles, else floating. Always ends the drag.
pub fn drop_window(state: &mut AbyssState, window: &Window) {
    let Some(drag) = state.tile_drag.take() else {
        return;
    };
    if &drag.window == window && !state.lock.locked {
        let tile_it = !drag.floating || main_mod_held(state);
        let to = drag.target().id;
        if to == drag.id {
            if tile_it {
                tile_on_source(state, &drag);
            }
        } else if !(tile_it && land_tiled(state, &drag)) {
            move_to_output(state, window, to);
        }
    }
    focus_pointer_output(state);
    arrange(state);
}

/// Output focus follows the pointer again once a drag is over: while it ran,
/// pointer motion left focus alone (`input::pointer_moved`), so a drop that
/// leaves the window where it was would otherwise keep the old output focused
/// until the next motion.
fn focus_pointer_output(state: &mut AbyssState) {
    let Some(id) = state
        .space
        .output_under(state.pointer_location)
        .next()
        .and_then(|o| state.outputs.by_output(o))
        .map(|e| e.id)
    else {
        return;
    };
    if state.outputs.set_focused(id) {
        let name = state
            .outputs
            .get(id)
            .map(|e| e.connector.clone())
            .unwrap_or_default();
        crate::ipc::emit(
            state,
            "output",
            serde_json::json!({ "focused": id, "name": name }),
        );
    }
}

/// Apply a drop on the source output's tree: the window tiles where it aims,
/// or stays where it is when the drop changes nothing.
fn tile_on_source(state: &mut AbyssState, drag: &TileDrag) {
    let target = drag.target();
    let Some(entry) = state.outputs.get_mut(drag.id) else {
        return;
    };
    if entry.active != target.ws {
        return;
    }
    let ws = &mut entry.workspaces[target.ws];
    let held = if drag.floating {
        ws.floating.iter().any(|f| f.window == drag.window)
    } else {
        ws.tiled.contains(&drag.window)
    };
    // Applied to a copy and committed whole, so a drop whose target vanished
    // mid-drag changes nothing at all.
    let mut tree = ws.tiled.clone();
    if held && apply_drop(&mut tree, &drag.window, drag.preview.key, &target.tiles) {
        ws.tiled = tree;
        ws.floating.retain(|f| f.window != drag.window);
        tracing::debug!(key = ?drag.preview.key, "radiant drop");
    }
}

/// Tile the dragged window onto a Radiant output other than its own: the drop
/// applies to that output's tree, and the window leaves the source's. False,
/// with nothing changed, when the drop has no tile there.
fn land_tiled(state: &mut AbyssState, drag: &TileDrag) -> bool {
    let target = drag.target();
    if !target.radiant || drag.preview.key == DropKey::Stay {
        return false;
    }
    let Some(dst) = state.outputs.get(target.id) else {
        return false;
    };
    // Powered off mid-drag: the window would be arranged onto a dark output.
    if dst.active != target.ws || !dst.powered {
        return false;
    }
    let mut tree = dst.workspaces[target.ws].tiled.clone();
    if !apply_drop(&mut tree, &drag.window, drag.preview.key, &target.tiles) {
        return false;
    }
    let src_ws = drag.targets[drag.source].ws;
    let Some(src) = state.outputs.get_mut(drag.id) else {
        return false;
    };
    if !src.workspaces[src_ws].holds(&drag.window) {
        return false;
    }
    let src_output = src.output.clone();
    src.workspaces[src_ws].remove(&drag.window);
    let Some(dst) = state.outputs.get_mut(target.id) else {
        return false;
    };
    let dst_output = dst.output.clone();
    dst.workspaces[target.ws].tiled = tree;
    super::anim::leave_output(state, &drag.window, &src_output, &dst_output);
    state.space.unmap_elem(&drag.window);
    state.outputs.set_focused(target.id);
    arrange(state);
    super::anim::arrive_on_output(state, &drag.window);
    tracing::info!(output = target.id, "radiant drop onto another output");
    true
}

/// The output a window whose centre is `centre` lands on when released: the
/// output other than `from` whose logical geometry contains it. `None` when the
/// centre is still on `from` or on no output, and the window stays put. Pure.
pub(crate) fn landing_output(
    outputs: &[(u64, Rectangle<i32, Logical>)],
    from: u64,
    centre: Point<f64, Logical>,
) -> Option<u64> {
    outputs
        .iter()
        .find(|(id, geo)| *id != from && geo.to_f64().contains(centre))
        .map(|(id, _)| *id)
}

/// Move `window` from the output that holds it to output `to`'s active
/// workspace, floating, at the rectangle it has now (kept inside `to`'s usable
/// area). A tiled window's rectangle is the one it is drawn at. Focus goes
/// with the window (`COMP-05 §5.1`). False when there is nothing to move.
pub(crate) fn move_to_output(state: &mut AbyssState, window: &Window, to: u64) -> bool {
    let Some(from) = output_of_window(state, window) else {
        return false;
    };
    if from == to || !state.outputs.get(to).is_some_and(|e| e.powered) {
        return false;
    }
    let src = state.outputs.get(from).expect("just resolved");
    let (src_output, src_ws) = (src.output.clone(), src.active);
    let floating = src.workspaces[src_ws]
        .floating
        .iter()
        .find(|f| &f.window == window)
        .map(|f| (f.rect, f.placement));
    let (rect, placement) = match floating {
        Some(placed) => placed,
        None => {
            let Some(content) = state.space.element_geometry(window) else {
                return false;
            };
            let border = state.config.general.border_size;
            let bar = titlebar::bar_height(state, window);
            (outer_of_content(content, border, bar), Placement::Free)
        }
    };
    let dst = state.outputs.get(to).expect("checked above");
    let (dst_output, dst_ws) = (dst.output.clone(), dst.active);
    // A drag keeps the stored rectangle in global coordinates (`place_at`), so
    // it is already where the window was dropped: only clamp it to the target.
    layer_map_for_output(&dst_output).arrange();
    let area = tiling_area(state, &dst_output);
    let rect = super::de::carried_rect(rect, Point::default(), area);
    state.outputs.get_mut(from).expect("just resolved").workspaces[src_ws].remove(window);
    super::anim::leave_output(state, window, &src_output, &dst_output);
    state.space.unmap_elem(window);
    let dst = state.outputs.get_mut(to).expect("checked above");
    dst.workspaces[dst_ws].floating.push(super::workspace::Floating {
        window: window.clone(),
        rect,
        weight: None,
        placement,
    });
    state.outputs.set_focused(to);
    arrange(state);
    super::anim::arrive_on_output(state, window);
    tracing::info!(output = to, "floating window moved to output");
    true
}

/// A floating window released over another output moves to that output's
/// active workspace (`move_to_output`). Tiled windows keep their output here;
/// a drag onto another output is `drop_window`'s. Only for a move that ran no
/// tile drag.
pub fn land_on_output(state: &mut AbyssState, window: &Window) {
    if state.lock.locked {
        return;
    }
    let Some(from) = output_of_window(state, window) else {
        return;
    };
    let Some(geo) = state.space.element_geometry(window) else {
        return;
    };
    let floating = state.outputs.get(from).is_some_and(|e| {
        e.workspaces[e.active]
            .floating
            .iter()
            .any(|f| &f.window == window)
    });
    if !floating {
        return;
    }
    // A powered-off output takes no window (COMP-03 §4).
    let outputs: Vec<(u64, Rectangle<i32, Logical>)> = state
        .outputs
        .iter()
        .filter(|e| e.powered)
        .filter_map(|e| state.space.output_geometry(&e.output).map(|g| (e.id, g)))
        .collect();
    if let Some(to) = landing_output(&outputs, from, centre(geo)) {
        move_to_output(state, window, to);
    }
}

/// Abandon a drag in flight (grab cancelled, session locked): the window goes
/// back to its placeholder.
pub fn cancel_tile_drag(state: &mut AbyssState) {
    // The `de` screen-edge preview is a drag overlay too.
    super::quicktile::clear_preview(state);
    if state.tile_drag.take().is_some() {
        focus_pointer_output(state);
        arrange(state);
    }
}
