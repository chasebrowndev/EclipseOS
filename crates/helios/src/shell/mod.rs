// SPDX-License-Identifier: AGPL-3.0-only
//! Window management: placement, layout, workspaces, focus (COMP-05).

pub mod layout;
pub mod workspace;

use smithay::{
    desktop::{layer_map_for_output, PopupKind, Window, WindowSurfaceType},
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, Rectangle, Size, SERIAL_COUNTER},
    wayland::{
        compositor::with_states,
        shell::{
            wlr_layer::{Layer, LayerSurfaceData},
            xdg::{SurfaceCachedState, XdgToplevelSurfaceData},
        },
    },
};

use crate::{
    config::LayoutKind, input::Direction, protocols::standard::fractional_scale, shell::workspace::Floating,
    state::HeliosState,
};

/// The `wl_surface` backing a window, xdg or X11 (COMP-07 §1).
pub fn window_surface(window: &Window) -> Option<WlSurface> {
    use smithay::desktop::WindowSurface;
    match window.underlying_surface() {
        WindowSurface::Wayland(t) => Some(t.wl_surface().clone()),
        WindowSurface::X11(s) => s.wl_surface(),
    }
}

/// The output that owns focus. Never `None` in practice: COMP-03 §5 guarantees
/// a fallback output exists whenever there is no connector.
pub fn focused_output(state: &HeliosState) -> Option<Output> {
    state.outputs.focused().map(|e| e.output.clone())
}

/// The output whose logical area contains `pos`, else the focused output.
pub fn output_at(state: &HeliosState, pos: Point<f64, Logical>) -> Option<Output> {
    state
        .space
        .output_under(pos)
        .next()
        .cloned()
        .or_else(|| focused_output(state))
}

/// The handle of the output whose workspaces hold `window`.
pub fn output_of_window(state: &HeliosState, window: &Window) -> Option<u64> {
    state
        .outputs
        .iter()
        .find(|e| {
            e.workspaces
                .iter()
                .any(|ws| ws.windows().contains(window) || ws.pending.contains(window))
        })
        .map(|e| e.id)
}

/// Usable tiling area: output geometry minus layer-shell exclusive zones minus
/// the outer gap.
fn tiling_area(state: &HeliosState, output: &Output) -> Rectangle<i32, Logical> {
    let geo = state.space.output_geometry(output).unwrap_or_default();
    let zone = layer_map_for_output(output).non_exclusive_zone();
    let g = &state.config.general;
    let loc = Point::from((
        geo.loc.x + zone.loc.x + g.gaps_out,
        geo.loc.y + zone.loc.y + g.gaps_out,
    ));
    // The layer map's zone can lag behind a mode change, so clamp it to the
    // output we actually have.
    let size = Size::from((
        (zone.size.w.min(geo.size.w - zone.loc.x) - 2 * g.gaps_out).max(1),
        (zone.size.h.min(geo.size.h - zone.loc.y) - 2 * g.gaps_out).max(1),
    ));
    Rectangle::new(loc, size)
}

fn shrink(r: Rectangle<i32, Logical>, by: i32) -> Rectangle<i32, Logical> {
    Rectangle::new(
        (r.loc.x + by, r.loc.y + by).into(),
        Size::from(((r.size.w - 2 * by).max(1), (r.size.h - 2 * by).max(1))),
    )
}

/// Clamp against the client's advertised min/max, loosely: a max smaller than
/// the tile wins, a min larger than the tile wins.
fn clamp_size(window: &Window, mut size: Size<i32, Logical>) -> Size<i32, Logical> {
    let Some(toplevel) = window.toplevel() else {
        if let Some(x11) = window.x11_surface() {
            let (min, max) = (x11.min_size(), x11.max_size());
            if let Some(max) = max {
                if max.w > 0 {
                    size.w = size.w.min(max.w);
                }
                if max.h > 0 {
                    size.h = size.h.min(max.h);
                }
            }
            if let Some(min) = min {
                size.w = size.w.max(min.w);
                size.h = size.h.max(min.h);
            }
            size.w = size.w.max(1);
            size.h = size.h.max(1);
        }
        return size;
    };
    with_states(toplevel.wl_surface(), |states| {
        let mut guard = states.cached_state.get::<SurfaceCachedState>();
        let d = *guard.current();
        if d.max_size.w > 0 {
            size.w = size.w.min(d.max_size.w);
        }
        if d.max_size.h > 0 {
            size.h = size.h.min(d.max_size.h);
        }
        size.w = size.w.max(d.min_size.w).max(1);
        size.h = size.h.max(d.min_size.h).max(1);
    });
    size
}

fn configure(window: &Window, rect: Rectangle<i32, Logical>, activated: bool) {
    let size = rect.size;
    let Some(toplevel) = window.toplevel() else {
        // X11 windows are positioned in the X root's coordinate space, so
        // they need the whole rectangle, not just a size.
        if let Some(x11) = window.x11_surface() {
            x11.set_activated(activated).ok();
            let _ = x11.configure(rect);
        }
        return;
    };
    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;
    toplevel.with_pending_state(|s| {
        s.size = Some(size);
        if activated {
            s.states.set(State::Activated);
        } else {
            s.states.unset(State::Activated);
        }
    });
    toplevel.send_pending_configure();
}

/// Re-run the layout for every output's active workspace.
pub fn arrange(state: &mut HeliosState) {
    let ids: Vec<u64> = state.outputs.iter().map(|e| e.id).collect();
    for id in ids {
        arrange_output(state, id);
    }
}

/// Lay out one output's active workspace and remap it into the space.
pub fn arrange_output(state: &mut HeliosState, id: u64) {
    let Some(entry) = state.outputs.get(id) else {
        return;
    };
    let output = entry.output.clone();
    let ws = entry.active;
    layer_map_for_output(&output).arrange();
    let area = tiling_area(state, &output);

    // Adopt anything handed over by an output that went away.
    let pending: Vec<Window> = {
        let entry = state.outputs.get_mut(id).expect("checked above");
        std::mem::take(&mut entry.workspaces[ws].pending)
    };
    for w in pending {
        let entry = state.outputs.get_mut(id).expect("checked above");
        entry.workspaces[ws].tiled.insert(w, None, area);
    }

    let entry = state.outputs.get(id).expect("checked above");
    let kind = entry.workspaces[ws]
        .layout
        .unwrap_or_else(|| state.config.layout_for(ws + 1));
    let gap = state.config.general.gaps_in;
    let border = state.config.general.border_size;

    let entry = state.outputs.get_mut(id).expect("checked above");
    let tiled: Vec<(Window, Rectangle<i32, Logical>)> = match kind {
        LayoutKind::Dwindle => entry.workspaces[ws].tiled.dwindle(area, gap),
        LayoutKind::Master => entry.workspaces[ws].tiled.master(area, gap),
    };
    let floating: Vec<(Window, Rectangle<i32, Logical>)> = entry.workspaces[ws]
        .floating
        .iter()
        .map(|f| (f.window.clone(), f.rect))
        .collect();
    let focus = state.focus.clone();

    for (w, rect) in tiled {
        let inner = shrink(rect, border);
        let size = clamp_size(&w, inner.size);
        configure(&w, Rectangle::new(inner.loc, size), focus.as_ref() == Some(&w));
        fractional_scale::update_window_scale(&w, &output);
        state.space.map_element(w, inner.loc, false);
    }
    for (w, rect) in floating {
        let inner = shrink(rect, border);
        let size = clamp_size(&w, inner.size);
        configure(&w, Rectangle::new(inner.loc, size), focus.as_ref() == Some(&w));
        fractional_scale::update_window_scale(&w, &output);
        state.space.map_element(w.clone(), inner.loc, false);
        state.space.raise_element(&w, false);
    }
}

/// A brand-new toplevel joins the active workspace, tiled, next to the focus.
pub fn place_new_window(state: &mut HeliosState, window: Window) {
    // Classify before the window is ever composited (COMP-02 §7). Raise-only.
    crate::render::capture::mark_sensitive(state, &window);
    let Some(id) = state.outputs.focused().map(|e| e.id) else {
        return;
    };
    let output = state.outputs.get(id).expect("just resolved").output.clone();
    layer_map_for_output(&output).arrange();
    let area = tiling_area(state, &output);
    let entry = state.outputs.get_mut(id).expect("just resolved");
    let ws = entry.active;
    let near = state
        .focus
        .clone()
        .filter(|w| entry.workspaces[ws].tiled.contains(w));
    entry.workspaces[ws]
        .tiled
        .insert(window.clone(), near.as_ref(), area);
    state.focus = Some(window.clone());
    arrange(state);
    focus_window(state, &window);
    let handle = state.ipc.handle_for(&window);
    crate::ipc::emit(
        state,
        "window",
        serde_json::json!({"change": "opened", "handle": handle}),
    );
}

pub fn unmap_window(state: &mut HeliosState, window: &Window) {
    for entry in state.outputs.iter_mut() {
        for ws in entry.workspaces.iter_mut() {
            ws.remove(window);
        }
    }
    state.space.unmap_elem(window);
    state.borders.remove(window);
    let handle = state.ipc.handle_for(window);
    crate::ipc::emit(
        state,
        "window",
        serde_json::json!({"change": "closed", "handle": handle}),
    );
    if state.focus.as_ref() == Some(window) {
        state.focus = None;
    }
    arrange(state);
    refocus_topmost(state);
}

pub fn focus_window(state: &mut HeliosState, window: &Window) {
    let Some(surface) = window_surface(window) else {
        return;
    };
    // X11 focus is compositor-driven: activate, raise in the X stack, then
    // let the keyboard follow (COMP-07 §1).
    if let Some(x11) = window.x11_surface() {
        x11.set_activated(true).ok();
        if let Some(wm) = state.xwayland.wm.as_mut() {
            let _ = wm.raise_window(x11);
        }
        let others: Vec<Window> = state.space.elements().filter(|w| *w != window).cloned().collect();
        for w in others {
            if let Some(other) = w.x11_surface() {
                other.set_activated(false).ok();
            }
        }
    }
    state.focus = Some(window.clone());
    if let Some(id) = output_of_window(state, window) {
        state.outputs.set_focused(id);
    }
    let keyboard = state.seat.get_keyboard().unwrap();
    keyboard.set_focus(state, Some(surface), SERIAL_COUNTER.next_serial());
    arrange(state);
    let handle = state.ipc.handle_for(window);
    crate::ipc::emit(state, "focus", serde_json::json!({"handle": handle}));
}

pub fn focus_surface(state: &mut HeliosState, surface: Option<WlSurface>) {
    let keyboard = state.seat.get_keyboard().unwrap();
    keyboard.set_focus(state, surface, SERIAL_COUNTER.next_serial());
}

pub fn refocus_topmost(state: &mut HeliosState) {
    let here: Vec<Window> = state
        .outputs
        .focused()
        .map(|e| e.workspace().windows())
        .unwrap_or_default();
    let top = state
        .space
        .elements()
        .rfind(|w| here.contains(w))
        .cloned()
        .or_else(|| state.space.elements().next_back().cloned());
    match top {
        Some(w) => focus_window(state, &w),
        None => {
            state.focus = None;
            focus_surface(state, None);
        }
    }
}

/// Topmost surface at `pos`, honouring the COMP-02 §4 stacking order:
/// overlay and top layers sit above toplevels, bottom and background below.
pub fn surface_under(
    state: &HeliosState,
    pos: Point<f64, Logical>,
) -> Option<(WlSurface, Point<f64, Logical>)> {
    let output = output_at(state, pos)?;
    let output_loc = state
        .space
        .output_geometry(&output)
        .map(|g| g.loc)
        .unwrap_or_default();
    let local = pos - output_loc.to_f64();
    {
        let map = layer_map_for_output(&output);
        for layer in [Layer::Overlay, Layer::Top] {
            if let Some(l) = map.layer_under(layer, local) {
                let geo = map.layer_geometry(l).unwrap_or_default();
                if let Some((s, p)) = l.surface_under(local - geo.loc.to_f64(), WindowSurfaceType::ALL) {
                    return Some((s, (output_loc + geo.loc + p).to_f64()));
                }
            }
        }
    }
    if let Some((window, loc)) = state.space.element_under(pos) {
        if let Some((s, p)) = window.surface_under(pos - loc.to_f64(), WindowSurfaceType::ALL) {
            return Some((s, (loc + p).to_f64()));
        }
    }
    let map = layer_map_for_output(&output);
    for layer in [Layer::Bottom, Layer::Background] {
        if let Some(l) = map.layer_under(layer, local) {
            let geo = map.layer_geometry(l).unwrap_or_default();
            if let Some((s, p)) = l.surface_under(local - geo.loc.to_f64(), WindowSurfaceType::ALL) {
                return Some((s, (output_loc + geo.loc + p).to_f64()));
            }
        }
    }
    None
}

/// Send the initial configure for toplevels, layer surfaces and popups, and
/// keep the layer map arranged.
pub fn handle_commit(state: &mut HeliosState, surface: &WlSurface) {
    state.popups.commit(surface);

    if let Some(window) = state
        .space
        .elements()
        .find(|w| window_surface(w).as_ref() == Some(surface))
        .cloned()
    {
        if window.toplevel().is_none() {
            // X11: no configure handshake to complete on this side.
            return;
        }
        let initial_sent = with_states(surface, |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .map(|d| d.lock().unwrap().initial_configure_sent)
                .unwrap_or(true)
        });
        if !initial_sent {
            if let Some(t) = window.toplevel() {
                t.send_configure();
            }
        }
        return;
    }

    let layer_output = state.outputs.iter().map(|e| e.output.clone()).find(|o| {
        layer_map_for_output(o)
            .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
            .is_some()
    });
    if let Some(output) = layer_output {
        let mut arranged = false;
        let mut send_initial = false;
        {
            let mut map = layer_map_for_output(&output);
            if let Some(layer) = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL) {
                send_initial = !with_states(surface, |states| {
                    states
                        .data_map
                        .get::<LayerSurfaceData>()
                        .map(|d| d.lock().unwrap().initial_configure_sent)
                        .unwrap_or(true)
                });
                if send_initial {
                    layer.layer_surface().send_configure();
                }
                arranged = map.arrange();
            }
        }
        if arranged || send_initial {
            arrange(state);
        }
        if send_initial {
            focus_layer_if_wanted(state, surface);
        }
    }

    if let Some(popup) = state.popups.find_popup(surface) {
        let PopupKind::Xdg(ref xdg) = popup else { return };
        if !xdg.is_initial_configure_sent() {
            let _ = xdg.send_configure();
        }
    }
}

// ---------------------------------------------------------------- actions

pub fn close_focused(state: &mut HeliosState) {
    if let Some(w) = state.focus.clone() {
        match w.underlying_surface() {
            smithay::desktop::WindowSurface::Wayland(t) => t.send_close(),
            smithay::desktop::WindowSurface::X11(x) => {
                if let Err(e) = x.close() {
                    tracing::warn!(error = %e, "could not close x11 window");
                }
            }
        }
    }
}

pub fn toggle_floating(state: &mut HeliosState) {
    let Some(window) = state.focus.clone() else { return };
    let Some(id) = output_of_window(state, &window).or_else(|| state.outputs.focused().map(|e| e.id)) else {
        return;
    };
    let output = state.outputs.get(id).expect("just resolved").output.clone();
    layer_map_for_output(&output).arrange();
    let area = tiling_area(state, &output);
    let geometry = state.space.element_geometry(&window);
    let entry = state.outputs.get_mut(id).expect("just resolved");
    let ws = entry.active;
    if entry.workspaces[ws].tiled.contains(&window) {
        let rect = geometry
            .unwrap_or_else(|| Rectangle::new(area.loc, Size::from((area.size.w / 2, area.size.h / 2))));
        let centered = Rectangle::new(
            (
                area.loc.x + (area.size.w - rect.size.w).max(0) / 2,
                area.loc.y + (area.size.h - rect.size.h).max(0) / 2,
            )
                .into(),
            rect.size,
        );
        entry.workspaces[ws].tiled.remove(&window);
        entry.workspaces[ws].floating.push(Floating {
            window,
            rect: centered,
        });
    } else if let Some(i) = entry.workspaces[ws]
        .floating
        .iter()
        .position(|f| f.window == window)
    {
        entry.workspaces[ws].floating.remove(i);
        entry.workspaces[ws].tiled.insert(window, None, area);
    }
    arrange(state);
}

pub fn toggle_layout(state: &mut HeliosState) {
    let Some(id) = state.outputs.focused().map(|e| e.id) else {
        return;
    };
    let ws = state.outputs.get(id).expect("just resolved").active;
    let fallback = state.config.layout_for(ws + 1);
    let entry = state.outputs.get_mut(id).expect("just resolved");
    let current = entry.workspaces[ws].layout.unwrap_or(fallback);
    let next = match current {
        LayoutKind::Dwindle => LayoutKind::Master,
        LayoutKind::Master => LayoutKind::Dwindle,
    };
    entry.workspaces[ws].layout = Some(next);
    tracing::info!(workspace = ws + 1, layout = ?next, "layout toggled");
    arrange(state);
}

fn neighbour(state: &HeliosState, from: &Window, dir: Direction) -> Option<Window> {
    let origin = state.space.element_geometry(from)?;
    let oc = (
        origin.loc.x as f64 + origin.size.w as f64 / 2.0,
        origin.loc.y as f64 + origin.size.h as f64 / 2.0,
    );
    state
        .space
        .elements()
        .filter(|w| *w != from)
        .filter_map(|w| state.space.element_geometry(w).map(|g| (w.clone(), g)))
        .filter_map(|(w, g)| {
            let c = (
                g.loc.x as f64 + g.size.w as f64 / 2.0,
                g.loc.y as f64 + g.size.h as f64 / 2.0,
            );
            let (dx, dy) = (c.0 - oc.0, c.1 - oc.1);
            let ok = match dir {
                Direction::Left => dx < -1.0 && dx.abs() >= dy.abs(),
                Direction::Right => dx > 1.0 && dx.abs() >= dy.abs(),
                Direction::Up => dy < -1.0 && dy.abs() > dx.abs(),
                Direction::Down => dy > 1.0 && dy.abs() > dx.abs(),
            };
            ok.then_some((w, dx * dx + dy * dy))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(w, _)| w)
}

pub fn focus_direction(state: &mut HeliosState, dir: Direction) {
    let Some(from) = state.focus.clone() else {
        refocus_topmost(state);
        return;
    };
    if let Some(target) = neighbour(state, &from, dir) {
        focus_window(state, &target);
    }
}

/// Swap the focused window with its neighbour in `dir` (tiled), or nudge a
/// floating window.
pub fn move_direction(state: &mut HeliosState, dir: Direction) {
    let Some(from) = state.focus.clone() else { return };
    let Some(id) = output_of_window(state, &from) else {
        return;
    };
    let ws = state.outputs.get(id).expect("just resolved").active;
    let entry = state.outputs.get_mut(id).expect("just resolved");
    if let Some(i) = entry.workspaces[ws]
        .floating
        .iter()
        .position(|f| f.window == from)
    {
        const STEP: i32 = 50;
        let r = &mut entry.workspaces[ws].floating[i].rect;
        match dir {
            Direction::Left => r.loc.x -= STEP,
            Direction::Right => r.loc.x += STEP,
            Direction::Up => r.loc.y -= STEP,
            Direction::Down => r.loc.y += STEP,
        }
        arrange(state);
        return;
    }
    let Some(target) = neighbour(state, &from, dir) else {
        return;
    };
    let entry = state.outputs.get_mut(id).expect("just resolved");
    if entry.workspaces[ws].tiled.contains(&target) {
        entry.workspaces[ws].tiled.swap(&from, &target);
        arrange(state);
    }
}

/// 1-based workspace index.
pub fn switch_workspace(state: &mut HeliosState, idx: usize) {
    if !(1..=workspace::COUNT).contains(&idx) {
        return;
    }
    let target = idx - 1;
    let Some(id) = state.outputs.focused().map(|e| e.id) else {
        return;
    };
    let entry = state.outputs.get(id).expect("just resolved");
    if target == entry.active {
        return;
    }
    for w in entry.workspace().windows() {
        state.space.unmap_elem(&w);
    }
    state.outputs.get_mut(id).expect("just resolved").active = target;
    state.focus = None;
    arrange(state);
    refocus_topmost(state);
    crate::ipc::emit(
        state,
        "workspace",
        serde_json::json!({"change": "switched", "output": id, "workspace": idx}),
    );
    tracing::info!(workspace = idx, "workspace switched");
}

pub fn move_to_workspace(state: &mut HeliosState, idx: usize) {
    if !(1..=workspace::COUNT).contains(&idx) {
        return;
    }
    let target = idx - 1;
    let Some(window) = state.focus.clone() else { return };
    let Some(id) = output_of_window(state, &window).or_else(|| state.outputs.focused().map(|e| e.id)) else {
        return;
    };
    let entry = state.outputs.get(id).expect("just resolved");
    if target == entry.active {
        return;
    }
    let output = entry.output.clone();
    layer_map_for_output(&output).arrange();
    let area = tiling_area(state, &output);
    let entry = state.outputs.get_mut(id).expect("just resolved");
    let active = entry.active;
    entry.workspaces[active].remove(&window);
    state.space.unmap_elem(&window);
    let entry = state.outputs.get_mut(id).expect("just resolved");
    entry.workspaces[target].tiled.insert(window, None, area);
    state.focus = None;
    arrange(state);
    refocus_topmost(state);
    tracing::info!(workspace = idx, "window moved to workspace");
}

/// Resize one window to an absolute logical size (COMP-13 §2.1).
///
/// Floating and tiled are different operations and both are handled here. A
/// floating window owns its rectangle, so the new size is simply stored. A
/// tiled window owns nothing but the split ratios above it, so the size is
/// expressed as ratios by [`layout::Tree::resize`] and may not be honoured
/// exactly. Errors are strings so the caller can hand them straight back over
/// the socket; nothing is mutated on an error path.
pub fn resize_window(
    state: &mut HeliosState,
    window: &Window,
    width: Option<i32>,
    height: Option<i32>,
) -> Result<bool, &'static str> {
    let Some(id) = output_of_window(state, window) else {
        return Err("window is not on any output");
    };
    let output = state.outputs.get(id).expect("just resolved").output.clone();
    let entry = state.outputs.get(id).expect("just resolved");
    let Some(ws) = entry
        .workspaces
        .iter()
        .position(|w| w.windows().iter().any(|w| w == window))
    else {
        return Err("window is not on any workspace");
    };
    let tiled = entry.workspaces[ws].tiled.contains(window);
    let kind = entry.workspaces[ws]
        .layout
        .unwrap_or_else(|| state.config.layout_for(ws + 1));
    // The master layout ignores the tree's shape (ADR 0031), so a ratio has
    // nowhere to land. Say so rather than silently doing nothing.
    if tiled && kind == LayoutKind::Master {
        return Err("a tiled window cannot be resized under the master layout");
    }
    let border = state.config.general.border_size;
    let gap = state.config.general.gaps_in;
    layer_map_for_output(&output).arrange();
    let area = tiling_area(state, &output);
    // Callers name the window's own size; the tree deals in tiles, which are
    // the window plus its border on each side.
    let want = (width.map(|w| w + 2 * border), height.map(|h| h + 2 * border));

    let entry = state.outputs.get_mut(id).expect("just resolved");
    let changed = if tiled {
        entry.workspaces[ws].tiled.resize(window, area, gap, want)
    } else if let Some(f) = entry.workspaces[ws]
        .floating
        .iter_mut()
        .find(|f| &f.window == window)
    {
        let before = f.rect.size;
        f.rect.size.w = want.0.unwrap_or(before.w).max(1);
        f.rect.size.h = want.1.unwrap_or(before.h).max(1);
        f.rect.size != before
    } else {
        return Err("window is neither tiled nor floating");
    };
    if changed {
        arrange_output(state, id);
    }
    Ok(changed)
}

/// Hand one output's workspace over to another output (COMP-13 §2.1).
///
/// Every output owns its own fixed set of workspaces, so this moves the
/// *windows* of `idx` on `from` into the same index on `to` — the slot itself
/// stays where it is, which is why emptying it can never strand an output.
/// Both outputs are validated before anything moves.
pub fn move_workspace_to_output(
    state: &mut HeliosState,
    from: u64,
    idx: usize,
    to: u64,
) -> Result<bool, &'static str> {
    if !(1..=workspace::COUNT).contains(&idx) {
        return Err("workspace is out of range");
    }
    let target = idx - 1;
    if state.outputs.get(from).is_none() {
        return Err("no such source output");
    }
    match state.outputs.get(to) {
        None => return Err("no such output"),
        Some(e) if !e.enabled => return Err("target output is disabled"),
        Some(_) => {}
    }
    if from == to {
        return Ok(false);
    }
    let src_origin = state
        .outputs
        .get(from)
        .and_then(|e| state.space.output_geometry(&e.output))
        .map(|g| g.loc)
        .unwrap_or_default();
    let dst_output = state.outputs.get(to).expect("checked above").output.clone();
    let dst_origin = state
        .space
        .output_geometry(&dst_output)
        .map(|g| g.loc)
        .unwrap_or_default();
    let entry = state.outputs.get_mut(from).expect("checked above");
    let tiled = entry.workspaces[target].tiled.windows();
    let floating: Vec<Floating> = entry.workspaces[target].floating.drain(..).collect();
    let pending: Vec<Window> = std::mem::take(&mut entry.workspaces[target].pending);
    for w in &tiled {
        entry.workspaces[target].tiled.remove(w);
    }
    if tiled.is_empty() && floating.is_empty() && pending.is_empty() {
        return Ok(false);
    }
    for w in tiled.iter().chain(floating.iter().map(|f| &f.window)) {
        state.space.unmap_elem(w);
    }
    layer_map_for_output(&dst_output).arrange();
    let area = tiling_area(state, &dst_output);
    let dst = state.outputs.get_mut(to).expect("checked above");
    for w in tiled.into_iter().chain(pending) {
        dst.workspaces[target].tiled.insert(w, None, area);
    }
    for mut f in floating {
        // The rectangle is in global logical coordinates, so carry it across
        // by the difference between the two outputs' origins.
        f.rect.loc += dst_origin - src_origin;
        dst.workspaces[target].floating.push(f);
    }
    state.focus = None;
    arrange(state);
    refocus_topmost(state);
    tracing::info!(from, to, workspace = idx, "workspace moved to output");
    Ok(true)
}

pub fn spawn(command: &str) {
    tracing::info!(command, "spawning");
    use std::os::unix::process::CommandExt;
    let mut cmd = std::process::Command::new("/bin/sh");
    cmd.arg("-c").arg(command);
    // The child must not inherit our controlling terminal or process group.
    cmd.process_group(0);
    if let Err(e) = cmd.spawn() {
        tracing::error!(command, %e, "spawn failed");
    }
}

/// Give keyboard focus to a newly mapped layer surface that asks for it.
pub fn focus_layer_if_wanted(state: &mut HeliosState, surface: &WlSurface) {
    let wants = state.outputs.iter().any(|e| {
        layer_map_for_output(&e.output)
            .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
            .is_some_and(|l| l.can_receive_keyboard_focus())
    });
    if wants {
        state.focus = None;
        focus_surface(state, Some(surface.clone()));
    }
}
