// SPDX-License-Identifier: AGPL-3.0-only
//! xdg_toplevel map tracking: unmap/remap detection, the map-time configure,
//! and the commit handler that drives placement (COMP-05 §3/§4).

use super::*;

/// Marker: this window has had its map-time configure sent (see `handle_commit`).
struct MapConfigured;

/// True once `surface` has a committed buffer, i.e. the window is mapped.
pub(super) fn has_buffer(surface: &WlSurface) -> bool {
    smithay::backend::renderer::utils::with_renderer_surface_state(surface, |s| s.buffer().is_some())
        .unwrap_or(false)
}

/// Per-surface xdg_toplevel map tracking, kept in the surface's data map so a
/// toplevel that unmapped (and so owns no `Window` any more) is still
/// recognised when it commits again.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct ToplevelMap {
    /// A buffer has been committed since the last (re)map.
    had_buffer: bool,
    /// Unmapped by a null-buffer commit; the next commit re-runs placement.
    unmapped: bool,
    /// Where the window floated when it unmapped, handed to the remap.
    pub(super) placement: Option<Remembered>,
}

/// A floating window's placement at the moment it unmapped: the output and
/// workspace it sat on and its stored (outer) rectangle. A remap puts it back
/// exactly there rather than placing it as a new window; a tiled window keeps
/// no memory and simply re-tiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Remembered {
    pub(super) output: u64,
    pub(super) workspace: usize,
    pub(super) rect: Rectangle<i32, Logical>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MapTransition {
    /// The toplevel had a buffer and just committed without one.
    Unmapped,
    /// First commit after an unmap: the client restarts the initial
    /// commit/configure sequence, so the window is placed again — where it
    /// was, if it floated.
    Remap(Option<Remembered>),
}

impl ToplevelMap {
    pub(super) fn step(&mut self, buffer: bool) -> Option<MapTransition> {
        if self.unmapped {
            let placement = self.placement.take();
            *self = Self {
                had_buffer: buffer,
                unmapped: false,
                placement: None,
            };
            return Some(MapTransition::Remap(placement));
        }
        if buffer {
            self.had_buffer = true;
            None
        } else if self.had_buffer {
            *self = Self {
                had_buffer: false,
                unmapped: true,
                placement: None,
            };
            Some(MapTransition::Unmapped)
        } else {
            None
        }
    }
}

/// Forget any unmap bookkeeping on `surface`. Called when a fresh
/// `xdg_toplevel` takes the surface; its initial commit places it, and a stale
/// `had_buffer`/`unmapped` flag would read that commit as an unmap or remap.
pub fn reset_toplevel_map(surface: &WlSurface) {
    with_states(surface, |states| {
        if let Some(m) = states.data_map.get::<Cell<ToplevelMap>>() {
            m.set(ToplevelMap::default());
        }
    });
}

/// Advance `surface`'s map tracking for this commit. Cheap and allocation
/// free: a role lookup, then one data-map lookup for xdg toplevels only.
fn toplevel_transition(surface: &WlSurface) -> Option<MapTransition> {
    if smithay::wayland::compositor::get_role(surface)
        != Some(smithay::wayland::shell::xdg::XDG_TOPLEVEL_ROLE)
    {
        return None;
    }
    let buffer = has_buffer(surface);
    with_states(surface, |states| {
        // Single-threaded core: a `Cell`, no lock. Boxed once per surface.
        states
            .data_map
            .insert_if_missing(|| Cell::new(ToplevelMap::default()));
        let cell = states.data_map.get::<Cell<ToplevelMap>>()?;
        let mut m = cell.get();
        let t = m.step(buffer);
        cell.set(m);
        t
    })
}

/// xdg-shell: attaching a null buffer unmaps the toplevel. Release its tile
/// (the sibling takes the space) and drop the `Window`; the client may remap
/// it later. A floating window's placement is remembered on the surface so
/// `map_toplevel` can put it back; a tiled one re-tiles.
fn unmap_toplevel(state: &mut AbyssState, surface: &WlSurface) {
    let window = window_for_surface(state, surface).or_else(|| {
        // Rare path: on a hidden workspace or minimized, so not in `space`.
        owned_windows(state)
            .into_iter()
            .find(|w| w.toplevel().map(|t| t.wl_surface() == surface).unwrap_or(false))
    });
    let Some(window) = window else { return };
    tracing::debug!("xdg_toplevel unmapped by null buffer; releasing its tile");
    if let Some(t) = window.toplevel() {
        // smithay's own reset runs in a post-commit hook that reads the
        // buffer state before `on_commit_buffer_handler` updates it, so it
        // lags one commit behind; the remap must not wait on it.
        t.reset_initial_configure_sent();
    }
    let placement = floating_placement(state, &window);
    with_states(surface, |states| {
        if let Some(cell) = states.data_map.get::<Cell<ToplevelMap>>() {
            let mut m = cell.get();
            m.placement = placement;
            cell.set(m);
        }
    });
    unmap_window(state, &window);
}

/// Where `window` floats, if it does. A maximized or fullscreen window's
/// rectangle is derived from its output on every arrange, not placed, so it
/// has nothing to remember.
fn floating_placement(state: &AbyssState, window: &Window) -> Option<Remembered> {
    if state.maximized.contains_key(window) || state.fullscreen.contains_key(window) {
        return None;
    }
    state.outputs.iter().find_map(|entry| {
        entry.workspaces.iter().enumerate().find_map(|(workspace, ws)| {
            ws.floating
                .iter()
                .find(|f| &f.window == window)
                .map(|f| Remembered {
                    output: entry.id,
                    workspace,
                    rect: f.rect,
                })
        })
    })
}

/// True on a commit of an xdg toplevel that is still owed its initial
/// configure: the client's initial commit, or the first after an unmap. No
/// allocation: one role lookup, then one data-map lookup for toplevels only.
fn awaiting_initial_configure(surface: &WlSurface) -> bool {
    if smithay::wayland::compositor::get_role(surface)
        != Some(smithay::wayland::shell::xdg::XDG_TOPLEVEL_ROLE)
    {
        return false;
    }
    with_states(surface, |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .is_some_and(|d| !d.lock().unwrap().initial_configure_sent)
    })
}

/// Map an xdg toplevel on the commit that asks for its initial configure —
/// its first commit ever, or the first after an unmap — and send that
/// configure with the size the shell chose (COMP-05 §3). `new_toplevel` only
/// prepares the surface: placing it there, before the client has sent its
/// `app_id`, `title`, `set_parent` or min/max size, tiled every dialog and
/// float-rule window first and squeezed its neighbours for one configure. By
/// the initial commit xdg-shell has all of that applied, so rules and the
/// dialog default see real facts, and a toplevel destroyed before it commits
/// never touches the layout. A window that floated before an unmap goes back
/// to the rectangle it left; anything else is placed as a new window.
fn map_toplevel(state: &mut AbyssState, surface: &WlSurface, placement: Option<Remembered>) {
    let Some(toplevel) = state
        .xdg_shell_state
        .toplevel_surfaces()
        .iter()
        .find(|t| t.wl_surface() == surface)
        .cloned()
    else {
        return;
    };
    // Never place a window twice.
    if owned_windows(state)
        .iter()
        .any(|w| w.toplevel().map(|t| t == &toplevel).unwrap_or(false))
    {
        return;
    }
    tracing::debug!(restored = placement.is_some(), "xdg_toplevel mapping");
    // A new toplevel breaks an active popup grab (COMP-06 §4).
    popup_grab_dismiss(state);
    // Fullscreen/maximize asked for before this commit had no window to act
    // on and were parked in the pending state (see `maximize_toplevel`).
    // Take them out so the placement configure does not carry them
    // unapplied, and replay them once the window exists.
    let (fullscreen, fullscreen_output, maximized) = toplevel.with_pending_state(|s| {
        use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;
        let requested = (
            s.states.unset(State::Fullscreen),
            s.fullscreen_output.take(),
            s.states.unset(State::Maximized),
        );
        s.states.set(State::Activated);
        requested
    });
    let window = Window::new_wayland_window(toplevel.clone());
    // The commit handler only refreshes windows it already found in the
    // space, which this one was not. A remap commit usually carries the
    // buffer, so without this the window would be placed with an empty
    // bounding box and take no input until its next commit.
    window.on_commit();
    match placement {
        Some(p) if restore_floating(state, &window, p) => {}
        _ => place_new_window(state, window),
    }
    if maximized {
        maximize_toplevel(state, &toplevel);
    }
    if fullscreen {
        fullscreen_toplevel(state, &toplevel, fullscreen_output.as_ref());
    }
    if !toplevel.is_initial_configure_sent() {
        toplevel.send_configure();
    }
}

/// Put a remapped window back on the output and workspace it floated on, at
/// the rectangle it left. False when that output or workspace is gone, and
/// the caller places it as a new window instead.
fn restore_floating(state: &mut AbyssState, window: &Window, p: Remembered) -> bool {
    let Some(ws) = state
        .outputs
        .get_mut(p.output)
        .and_then(|e| e.workspaces.get_mut(p.workspace))
    else {
        return false;
    };
    ws.floating.push(Floating {
        window: window.clone(),
        rect: p.rect,
        weight: None,
    });
    // Same bookkeeping as `place_new_window`, minus the placement decision.
    classify(state, window);
    crate::protocols::standard::foreign_toplevel::window_mapped(state, window);
    // Property rules (opacity, blur, trust) still apply to the new `Window`,
    // but its placement is final: a settle pass must not move it again.
    let _ = rules::apply(state, window);
    window.user_data().insert_if_missing(|| rules::Placed);
    if !state.lock.locked {
        state.focus = Some(window.clone());
    }
    arrange(state);
    focus_window(state, window);
    let handle = state.ipc.handle_for(window);
    crate::ipc::emit(
        state,
        "window",
        serde_json::json!({"change": "opened", "handle": handle}),
    );
    true
}

/// Send the initial configure for toplevels, layer surfaces and popups, and
/// keep the layer map arranged.
pub fn handle_commit(state: &mut AbyssState, surface: &WlSurface) {
    state.popups.commit(surface);

    match toplevel_transition(surface) {
        Some(MapTransition::Unmapped) => {
            unmap_toplevel(state, surface);
            return;
        }
        Some(MapTransition::Remap(placement)) => map_toplevel(state, surface, placement),
        None if awaiting_initial_configure(surface) => map_toplevel(state, surface, None),
        None => {}
    }

    let mapped = state
        .space
        .elements()
        .find(|w| window_surface(w).as_ref() == Some(surface))
        .cloned();
    if let Some(window) = mapped {
        // Title and app_id can change at any commit; the foreign-toplevel list
        // has no other notification path for them. The bar hears it over IPC.
        if crate::protocols::standard::foreign_toplevel::window_updated(&window) {
            let handle = state.ipc.handle_for(&window);
            crate::ipc::emit(
                state,
                "window",
                serde_json::json!({"change": "title", "handle": handle}),
            );
        }
        // Title-matching rules are re-evaluated on title change (COMP-05 §4).
        if let Some(placement) = rules::reevaluate(state, &window) {
            replace_window(state, &window, &placement);
        }
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
        } else if window.user_data().get::<MapConfigured>().is_none() && has_buffer(surface) {
            // COMP-05 §3: a toplevel gets a configure when it maps. The commit
            // that first attaches a buffer is that moment, and it is the only
            // point at which the client learns the size and state the shell
            // actually chose for a window it has already been placed into.
            // `configure` uses `send_pending_configure`, which suppresses a
            // configure whose content is unchanged since the initial one, so
            // without this the map configure never goes out.
            window.user_data().insert_if_missing(|| MapConfigured);
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
                let layer = layer.clone();
                // Arrange *before* the initial configure. `new_layer_surface`
                // fires at `get_layer_surface` time, i.e. before the client has
                // set its anchor, size or exclusive zone, so the geometry from
                // that first arrange is stale. Configuring from it hands the
                // client a wrong size that it may well act on (COMP-06 §3).
                arranged = map.arrange();
                if send_initial {
                    layer.layer_surface().send_configure();
                }
            }
        }
        // smithay's `arrange` reports only a layer whose *size* changed. A bar
        // that keeps its height but moves its exclusive zone or margin (a
        // hyperion fold landing) moves the tiling area all the same, and
        // without this the windows keep the stale one.
        let area_moved = state
            .outputs
            .iter()
            .find(|e| e.output == output)
            .is_some_and(|e| e.workspaces[e.active].last_area != Some(tiling_area(state, &output)));
        if arranged || send_initial || area_moved {
            arrange(state);
        }
        if send_initial {
            focus_layer_if_wanted(state, surface);
        }
        follow_interactivity(state, &output, surface, send_initial);
    }

    if let Some(popup) = state.popups.find_popup(surface) {
        let PopupKind::Xdg(ref xdg) = popup else { return };
        if !xdg.is_initial_configure_sent() && xdg.send_configure().is_err() {
            return;
        }
        // Smithay's popup post-commit hook resets `current` to the last acked
        // configure on *every* commit, so republishing only once at the
        // initial configure would be undone by the next commit (COMP-06 §4).
        publish_popup_geometry(xdg);
    }
}
