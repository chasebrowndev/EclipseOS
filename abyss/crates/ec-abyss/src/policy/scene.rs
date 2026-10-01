// SPDX-License-Identifier: AGPL-3.0-only
//! The agent scene filter: the single choke point between the compositor's
//! windows and anything an agent can learn about them (COMP-08 §3, S-01 §3,
//! COMP-15 §2). TCB.
//!
//! Every agent-facing path that names a window (listings, `get_toplevel`,
//! `hit_test`, and later events, captures and `wait_for`) goes through
//! [`list`], [`resolve`] or [`hit`]. None of them decides visibility itself;
//! they all end in [`SceneView::visible`], so a leak cannot come from two
//! paths disagreeing.
//!
//! A window the agent cannot see is indistinguishable from one that never
//! existed (F-07): [`resolve`] returns `None` for both by the same path, and
//! [`hit`] returns `None` (handle 0) when the topmost window is invisible,
//! rather than reaching through it to one beneath.

use policy_eval::scope::{OutputFacts, WorkspaceFacts};
use policy_eval::{Class, SceneView, WindowFacts};
use smithay::desktop::{Window, WindowSurface};
use smithay::utils::{IsAlive, Logical, Point};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;

use crate::outputs::OutputKind;
use crate::state::AbyssState;

/// The class an agent sees `window` as.
///
/// COMP-11's table does not exist until M16, and with no policy table loaded
/// every surface is `secret` to agents (S-05 §8). The manual `sensitive`
/// flag can only raise, so it changes nothing below `secret`. This is the one
/// place the table plugs in.
fn class_of(_state: &AbyssState, _window: &Window) -> Class {
    #[cfg(test)]
    if let Some(c) = test_class::get(_window) {
        return c;
    }
    Class::Secret
}

/// The output and workspace `window` is on. Every workspace is a human
/// workspace until agent workspaces exist (M24).
fn place<'a>(state: &'a AbyssState, window: &Window) -> Option<(OutputFacts<'a>, WorkspaceFacts)> {
    for entry in state.outputs.iter() {
        for (i, ws) in entry.workspaces.iter().enumerate() {
            if ws.all_windows().iter().any(|w| w == window) {
                let output = OutputFacts {
                    id: entry.id,
                    name: &entry.connector,
                    is_virtual: entry.kind == OutputKind::Virtual,
                };
                // 1-based, the number the human sees and `get_windows` reports.
                let workspace = WorkspaceFacts {
                    id: i as u64 + 1,
                    human: true,
                    own: false,
                };
                return Some((output, workspace));
            }
        }
    }
    None
}

/// Runs `f` over the window's facts, borrowed in place. The xdg identity is
/// read under its lock without copying; X11 properties are owned strings in
/// Smithay, so that branch copies.
fn with_facts<R>(state: &AbyssState, window: &Window, f: impl FnOnce(&WindowFacts<'_>) -> R) -> R {
    let placed = place(state, window);
    let base = WindowFacts {
        app_id: "",
        title: "",
        // A window never named over IPC has no handle, and so no `handle:`
        // scope can name it. 0 is never minted.
        handle: state.ipc.existing_handle(window).unwrap_or(0),
        workspace: placed.map(|(_, w)| w),
        output: placed.map(|(o, _)| o),
        class: class_of(state, window),
        // Agent launches arrive with the launcher (M24); nothing is
        // launched by an agent before then.
        launched_by_self: false,
        no_agent: crate::shell::rules::hidden_from_agents(window),
    };
    match window.underlying_surface() {
        WindowSurface::Wayland(t) => with_states(t.wl_surface(), |states| {
            let data = states.data_map.get::<XdgToplevelSurfaceData>();
            match data.and_then(|d| d.lock().ok()) {
                Some(d) => f(&WindowFacts {
                    app_id: d.app_id.as_deref().unwrap_or(""),
                    title: d.title.as_deref().unwrap_or(""),
                    ..base
                }),
                None => f(&base),
            }
        }),
        WindowSurface::X11(s) => {
            let (app_id, title) = (s.class(), s.title());
            f(&WindowFacts {
                app_id: &app_id,
                title: &title,
                ..base
            })
        }
    }
}

/// No capability lists a `secret` toplevel: S-01 §2.1 has no
/// `scene.list.secret`, and S-05 §2 delivers secret content to no agent
/// without a `*.secret` capability and a per-request prompt. A `class:secret`
/// scope narrows; it never makes one visible.
fn visible(state: &AbyssState, view: &SceneView, window: &Window) -> bool {
    window.alive() && with_facts(state, window, |w| w.class != Class::Secret && view.visible(w))
}

/// `list_toplevels`: every window that exists for this agent, with its
/// handle, in output then workspace then stacking order.
pub fn list(state: &mut AbyssState, view: &SceneView) -> Vec<(u64, Window)> {
    let mut out = Vec::new();
    for entry in state.outputs.iter() {
        for ws in &entry.workspaces {
            for w in ws.all_windows() {
                if visible(state, view, &w) {
                    out.push(w);
                }
            }
        }
    }
    out.into_iter().map(|w| (state.ipc.handle_for(&w), w)).collect()
}

/// `get_toplevel` and every request naming a handle. `None` covers "never
/// existed", "gone" and "out of scope" alike; the caller answers all three
/// with `invalid_argument`, detail `handle` (F-07).
pub fn resolve(state: &AbyssState, view: &SceneView, handle: u64) -> Option<Window> {
    state.ipc.window_for(handle).filter(|w| visible(state, view, w))
}

/// Whether a window [`resolve`] or [`hit`] returned may be read in detail
/// (`get_toplevel`, `hit_test`; S-01 §2.1). `scene.read` reaches `public`
/// and `private` windows within its scopes. A `secret` window needs
/// `scene.read.secret` and a per-request prompt, and there is no prompt path
/// yet, so it is never readable whatever the grant says; [`visible`]
/// already refuses it.
pub fn readable(state: &AbyssState, read: &SceneView, window: &Window) -> bool {
    visible(state, read, window)
}

/// The class a window [`list`], [`resolve`] or [`hit`] returned carries on
/// the wire. Never `secret`: those are never returned.
pub fn class(state: &AbyssState, window: &Window) -> Class {
    class_of(state, window)
}

/// `hit_test`: the topmost toplevel at `pos` if it exists for this agent,
/// with the point in its local coordinates. A layer surface (bar, overlay)
/// on top, or an invisible window on top, is `None` (handle 0): the agent
/// is told what a click would hit, never what lies beneath it.
pub fn hit(
    state: &AbyssState,
    view: &SceneView,
    pos: Point<f64, Logical>,
) -> Option<(Window, Point<f64, Logical>)> {
    if crate::shell::layer_at(state, pos).is_some() {
        return None;
    }
    let (window, loc) = state.space.element_under(pos)?;
    let window = window.clone();
    visible(state, view, &window).then(|| (window, pos - loc.to_f64()))
}

/// Test-only stand-in for the M16 policy table, so the leakage suite can
/// put `public` and `private` windows beside `secret` ones. Absent from
/// every non-test build; an unset window stays `secret`.
#[cfg(test)]
pub mod test_class {
    use std::cell::RefCell;

    use policy_eval::Class;
    use smithay::desktop::Window;

    thread_local! {
        static CLASSES: RefCell<Vec<(Window, Class)>> = const { RefCell::new(Vec::new()) };
    }

    pub fn set(window: &Window, class: Class) {
        CLASSES.with_borrow_mut(|v| {
            v.retain(|(w, _)| w != window);
            v.push((window.clone(), class));
        });
    }

    pub fn clear() {
        CLASSES.with_borrow_mut(Vec::clear);
    }

    pub(super) fn get(window: &Window) -> Option<Class> {
        CLASSES.with_borrow(|v| v.iter().find(|(w, _)| w == window).map(|(_, c)| *c))
    }
}
