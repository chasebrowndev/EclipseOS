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

use crate::{config::LayoutKind, input::Direction, shell::workspace::Floating, state::HeliosState};

/// The output everything is laid out on. Multi-output arrives in M3 (COMP-03).
pub fn primary_output(state: &HeliosState) -> Option<Output> {
    state.space.outputs().next().cloned()
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

fn configure(window: &Window, size: Size<i32, Logical>, activated: bool) {
    let Some(toplevel) = window.toplevel() else { return };
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

/// Re-run the layout for the active workspace and remap it into the space.
pub fn arrange(state: &mut HeliosState) {
    let Some(output) = primary_output(state) else {
        return;
    };
    layer_map_for_output(&output).arrange();
    let area = tiling_area(state, &output);
    let ws = state.active_workspace;
    let kind = state.workspaces[ws]
        .layout
        .unwrap_or_else(|| state.config.layout_for(ws + 1));
    let gap = state.config.general.gaps_in;
    let border = state.config.general.border_size;

    let tiled: Vec<(Window, Rectangle<i32, Logical>)> = match kind {
        LayoutKind::Dwindle => state.workspaces[ws].tiled.dwindle(area, gap),
        LayoutKind::Master => state.workspaces[ws].tiled.master(area, gap),
    };
    let floating: Vec<Rectangle<i32, Logical>> =
        state.workspaces[ws].floating.iter().map(|f| f.rect).collect();
    let floating_windows: Vec<Window> = state.workspaces[ws]
        .floating
        .iter()
        .map(|f| f.window.clone())
        .collect();
    let focus = state.focus.clone();

    for (w, rect) in tiled {
        let inner = shrink(rect, border);
        let size = clamp_size(&w, inner.size);
        configure(&w, size, focus.as_ref() == Some(&w));
        state.space.map_element(w, inner.loc, false);
    }
    for (w, rect) in floating_windows.into_iter().zip(floating) {
        let inner = shrink(rect, border);
        let size = clamp_size(&w, inner.size);
        configure(&w, size, focus.as_ref() == Some(&w));
        state.space.map_element(w.clone(), inner.loc, false);
        state.space.raise_element(&w, false);
    }
}

/// A brand-new toplevel joins the active workspace, tiled, next to the focus.
pub fn place_new_window(state: &mut HeliosState, window: Window) {
    let Some(output) = primary_output(state) else {
        return;
    };
    layer_map_for_output(&output).arrange();
    let area = tiling_area(state, &output);
    let ws = state.active_workspace;
    let near = state
        .focus
        .clone()
        .filter(|w| state.workspaces[ws].tiled.contains(w));
    state.workspaces[ws]
        .tiled
        .insert(window.clone(), near.as_ref(), area);
    state.focus = Some(window.clone());
    arrange(state);
    focus_window(state, &window);
}

pub fn unmap_window(state: &mut HeliosState, window: &Window) {
    for ws in state.workspaces.iter_mut() {
        ws.remove(window);
    }
    state.space.unmap_elem(window);
    state.borders.remove(window);
    if state.focus.as_ref() == Some(window) {
        state.focus = None;
    }
    arrange(state);
    refocus_topmost(state);
}

pub fn focus_window(state: &mut HeliosState, window: &Window) {
    let Some(surface) = window.toplevel().map(|t| t.wl_surface().clone()) else {
        return;
    };
    state.focus = Some(window.clone());
    let keyboard = state.seat.get_keyboard().unwrap();
    keyboard.set_focus(state, Some(surface), SERIAL_COUNTER.next_serial());
    arrange(state);
}

pub fn focus_surface(state: &mut HeliosState, surface: Option<WlSurface>) {
    let keyboard = state.seat.get_keyboard().unwrap();
    keyboard.set_focus(state, surface, SERIAL_COUNTER.next_serial());
}

pub fn refocus_topmost(state: &mut HeliosState) {
    let top = state.space.elements().next_back().cloned();
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
    let output = primary_output(state)?;
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
        .find(|w| w.toplevel().map(|t| t.wl_surface() == surface).unwrap_or(false))
        .cloned()
    {
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

    if let Some(output) = primary_output(state) {
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
        if let Some(t) = w.toplevel() {
            t.send_close();
        }
    }
}

pub fn toggle_floating(state: &mut HeliosState) {
    let Some(window) = state.focus.clone() else { return };
    let Some(output) = primary_output(state) else {
        return;
    };
    layer_map_for_output(&output).arrange();
    let area = tiling_area(state, &output);
    let ws = state.active_workspace;
    if state.workspaces[ws].tiled.contains(&window) {
        let rect = state
            .space
            .element_geometry(&window)
            .unwrap_or_else(|| Rectangle::new(area.loc, Size::from((area.size.w / 2, area.size.h / 2))));
        let centered = Rectangle::new(
            (
                area.loc.x + (area.size.w - rect.size.w).max(0) / 2,
                area.loc.y + (area.size.h - rect.size.h).max(0) / 2,
            )
                .into(),
            rect.size,
        );
        state.workspaces[ws].tiled.remove(&window);
        state.workspaces[ws].floating.push(Floating {
            window,
            rect: centered,
        });
    } else if let Some(i) = state.workspaces[ws]
        .floating
        .iter()
        .position(|f| f.window == window)
    {
        state.workspaces[ws].floating.remove(i);
        let near = state
            .focus
            .clone()
            .filter(|w| state.workspaces[ws].tiled.contains(w));
        state.workspaces[ws].tiled.insert(window, near.as_ref(), area);
    }
    arrange(state);
}

pub fn toggle_layout(state: &mut HeliosState) {
    let ws = state.active_workspace;
    let current = state.workspaces[ws]
        .layout
        .unwrap_or_else(|| state.config.layout_for(ws + 1));
    state.workspaces[ws].layout = Some(match current {
        LayoutKind::Dwindle => LayoutKind::Master,
        LayoutKind::Master => LayoutKind::Dwindle,
    });
    tracing::info!(workspace = ws + 1, layout = ?state.workspaces[ws].layout, "layout toggled");
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
    let ws = state.active_workspace;
    if let Some(i) = state.workspaces[ws]
        .floating
        .iter()
        .position(|f| f.window == from)
    {
        const STEP: i32 = 50;
        let r = &mut state.workspaces[ws].floating[i].rect;
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
    if state.workspaces[ws].tiled.contains(&target) {
        state.workspaces[ws].tiled.swap(&from, &target);
        arrange(state);
    }
}

/// 1-based workspace index.
pub fn switch_workspace(state: &mut HeliosState, idx: usize) {
    if !(1..=workspace::COUNT).contains(&idx) {
        return;
    }
    let target = idx - 1;
    if target == state.active_workspace {
        return;
    }
    for w in state.workspaces[state.active_workspace].windows() {
        state.space.unmap_elem(&w);
    }
    state.active_workspace = target;
    state.focus = None;
    arrange(state);
    refocus_topmost(state);
    tracing::info!(workspace = idx, "workspace switched");
}

pub fn move_to_workspace(state: &mut HeliosState, idx: usize) {
    if !(1..=workspace::COUNT).contains(&idx) {
        return;
    }
    let target = idx - 1;
    let Some(window) = state.focus.clone() else { return };
    if target == state.active_workspace {
        return;
    }
    let Some(output) = primary_output(state) else {
        return;
    };
    layer_map_for_output(&output).arrange();
    let area = tiling_area(state, &output);
    state.workspaces[state.active_workspace].remove(&window);
    state.space.unmap_elem(&window);
    state.workspaces[target].tiled.insert(window, None, area);
    state.focus = None;
    arrange(state);
    refocus_topmost(state);
    tracing::info!(workspace = idx, "window moved to workspace");
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
    let Some(output) = primary_output(state) else {
        return;
    };
    let wants = {
        let map = layer_map_for_output(&output);
        map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
            .is_some_and(|l| l.can_receive_keyboard_focus())
    };
    if wants {
        state.focus = None;
        focus_surface(state, Some(surface.clone()));
    }
}
