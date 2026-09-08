// SPDX-License-Identifier: AGPL-3.0-only
//! COMP-05 §4: the `windowrule` engine.
//!
//! Rules are matched once at map time and re-evaluated when a window's title
//! changes. Placement actions (`float`, `tile`, `workspace`) only apply at map
//! time — a window that jumps outputs because a page title changed would be
//! worse than the rule not firing — while the property actions (`opacity`,
//! `sensitivity`, `no-agent`) are re-applied on every re-evaluation.
//!
//! `sensitivity` goes through the same raise-only path as the capture
//! classifier: a rule can raise a window's class, never lower it.

use std::cell::Cell;

use smithay::desktop::Window;
use smithay::reexports::wayland_server::Resource;

use crate::config::{Matchers, RuleAction};
use crate::state::AbyssState;

/// Per-window opacity set by a matched `opacity` rule, read by the renderer.
pub struct RuleOpacity(pub Cell<f32>);

/// Marker for `windowrule "no-agent"`: the window is absent from every agent's
/// scene. Nothing consumes it until COMP-08 lands `list_toplevels`.
pub struct NoAgent;

/// What the caller still has to act on, because only it knows the placement.
#[derive(Debug, Default, PartialEq)]
pub struct Placement {
    pub float: bool,
    /// 1-based workspace, as written in the config.
    pub workspace: Option<i32>,
    pub no_focus_steal: bool,
}

/// The opacity a matched rule pinned on this window, if any.
pub fn opacity_of(window: &Window) -> Option<f32> {
    window.user_data().get::<RuleOpacity>().map(|o| o.0.get())
}

/// Whether any window carries an opacity override, i.e. whether the renderer
/// has to take the per-window path even with default decoration.
pub fn any_opacity_override<'a>(mut windows: impl Iterator<Item = &'a Window>) -> bool {
    windows.any(|w| w.user_data().get::<RuleOpacity>().is_some())
}

/// COMP-08 will consult this before putting a window in an agent scene.
#[allow(dead_code)]
pub fn hidden_from_agents(window: &Window) -> bool {
    window.user_data().get::<NoAgent>().is_some()
}

/// Evaluate every rule against a newly mapped window.
pub fn apply(state: &mut AbyssState, window: &Window) -> Placement {
    evaluate(state, window, true)
}

/// Re-evaluate after a title change. Placement is left alone; see the module
/// comment.
pub fn reevaluate(state: &mut AbyssState, window: &Window) {
    if state.config.window_rules.is_empty() {
        return;
    }
    evaluate(state, window, false);
}

fn evaluate(state: &mut AbyssState, window: &Window, placing: bool) -> Placement {
    let mut placement = Placement::default();
    if state.config.window_rules.is_empty() {
        return placement;
    }
    let facts = Facts::gather(state, window);
    // Rules apply in file order, so a later rule wins on the same property.
    for i in 0..state.config.window_rules.len() {
        if !facts.matches(&state.config.window_rules[i].matchers) {
            continue;
        }
        match state.config.window_rules[i].action.clone() {
            RuleAction::Float if placing => placement.float = true,
            RuleAction::Tile if placing => placement.float = false,
            RuleAction::Workspace(n) if placing => placement.workspace = Some(n),
            RuleAction::NoFocusSteal if placing => placement.no_focus_steal = true,
            RuleAction::Float | RuleAction::Tile | RuleAction::Workspace(_) | RuleAction::NoFocusSteal => {}
            RuleAction::Opacity(v) => {
                if let Some(o) = window.user_data().get::<RuleOpacity>() {
                    o.0.set(v);
                } else {
                    window.user_data().insert_if_missing(|| RuleOpacity(Cell::new(v)));
                }
            }
            RuleAction::NoAgent => {
                window.user_data().insert_if_missing(|| NoAgent);
            }
            RuleAction::Sensitivity(_) => {
                // Raise-only, and the class is already the strictest this
                // build represents; there is nothing to lower.
                if let Some(surface) = crate::shell::window_surface(window) {
                    state.sensitive.insert(surface);
                }
            }
        }
    }
    placement
}

/// Everything a matcher can ask about a window, read once.
struct Facts {
    app_id: String,
    title: String,
    pid: Option<i32>,
    xwayland: bool,
    output_connector: String,
    output_identity: String,
    workspace: i32,
}

impl Facts {
    fn gather(state: &AbyssState, window: &Window) -> Self {
        let (app_id, title) = crate::ipc::methods::identity_of(window);
        let pid = crate::shell::window_surface(window)
            .and_then(|s| s.client())
            .and_then(|c| c.get_credentials(&state.display_handle).ok())
            .map(|c| c.pid);
        let entry = crate::shell::output_of_window(state, window)
            .and_then(|id| state.outputs.get(id))
            .or_else(|| state.outputs.focused());
        Self {
            app_id: app_id.unwrap_or_default(),
            title: title.unwrap_or_default(),
            pid,
            xwayland: window.toplevel().is_none(),
            output_connector: entry.map(|e| e.connector.clone()).unwrap_or_default(),
            output_identity: entry.map(|e| e.identity.clone()).unwrap_or_default(),
            workspace: entry.map(|e| e.active as i32 + 1).unwrap_or(1),
        }
    }

    fn matches(&self, m: &Matchers) -> bool {
        if let Some(p) = &m.app_id {
            if !p.matches(&self.app_id) {
                return false;
            }
        }
        if let Some(p) = &m.title {
            if !p.matches(&self.title) {
                return false;
            }
        }
        if let Some(pid) = m.pid {
            if self.pid != Some(pid) {
                return false;
            }
        }
        if let Some(x) = m.xwayland {
            if self.xwayland != x {
                return false;
            }
        }
        if let Some(name) = &m.output {
            if !crate::config::glob_match(name, &self.output_connector)
                && !crate::config::glob_match(name, &self.output_identity)
            {
                return false;
            }
        }
        if let Some(ws) = m.workspace {
            if self.workspace != ws {
                return false;
            }
        }
        true
    }
}
