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
use crate::xwayland::security::{AppTrust, SeatCompat};

/// Per-window opacity set by a matched `opacity` rule, read by the renderer.
pub struct RuleOpacity(pub Cell<f32>);

/// Trust class pinned by an `app-trust` rule (COMP-05 §4). Consumed once
/// COMP-08 gates agent actions on it.
pub struct RuleTrust(pub Cell<AppTrust>);

/// Seat concurrency pinned by a `seat-compat` rule (COMP-04 §8).
pub struct RuleSeat(pub Cell<SeatCompat>);

/// Marker that a window has had its one placement pass with a known identity.
/// A client's `app_id`/`title` are usually still empty at map time, so the
/// placement actions get one deferred retry on the first commit that carries
/// an identity; after that placement is frozen (module comment).
pub struct Placed;

/// Marker for `windowrule "no-agent"`: the window is absent from every agent's
/// scene. Nothing consumes it until COMP-08 lands `list_toplevels`.
pub struct NoAgent;

/// What the caller still has to act on, because only it knows the placement.
#[derive(Debug, Default, PartialEq)]
pub struct Placement {
    pub float: bool,
    /// Map the window fullscreen; the caller applies it after placement.
    pub fullscreen: bool,
    /// 1-based workspace, as written in the config.
    pub workspace: Option<i32>,
    pub no_focus_steal: bool,
    /// Float geometry in logical pixels; either half implies `float`.
    pub size: Option<(i32, i32)>,
    pub position: Option<(i32, i32)>,
    /// Glob for the output to map on, matched against connector or identity.
    pub output: Option<String>,
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

/// The trust class a matched rule pinned on this window, if any.
#[allow(dead_code)] // consumed when COMP-08 gates on app trust
pub fn trust_of(window: &Window) -> Option<AppTrust> {
    window.user_data().get::<RuleTrust>().map(|t| t.0.get())
}

/// The seat concurrency a matched rule pinned on this window, if any.
#[allow(dead_code)] // consumed when COMP-04 §8 focus locks land
pub fn seat_compat_of(window: &Window) -> Option<SeatCompat> {
    window.user_data().get::<RuleSeat>().map(|s| s.0.get())
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

/// Re-evaluate after a commit. Property actions always re-apply; placement is
/// returned only for the one deferred pass described on [`Placed`], and the
/// caller then has to re-install the window.
#[must_use]
pub fn reevaluate(state: &mut AbyssState, window: &Window) -> Option<Placement> {
    if state.config.window_rules.is_empty() {
        return None;
    }
    if window.user_data().get::<Placed>().is_some() {
        evaluate(state, window, false);
        return None;
    }
    let placement = evaluate(state, window, true);
    if placement == Placement::default() {
        return None;
    }
    // A rule matched on something other than identity (pid, cgroup); freeze
    // placement here too, or the window would be re-installed on every commit.
    window.user_data().insert_if_missing(|| Placed);
    Some(placement)
}

fn evaluate(state: &mut AbyssState, window: &Window, placing: bool) -> Placement {
    let mut placement = Placement::default();
    if state.config.window_rules.is_empty() {
        return placement;
    }
    let facts = Facts::gather(state, window);
    // Identity arrives after the first commit for most clients; until it does,
    // a placement pass would match on empty strings. Hold the marker back so
    // the caller retries once the client has named itself.
    if placing && !(facts.app_id.is_empty() && facts.title.is_empty()) {
        window.user_data().insert_if_missing(|| Placed);
    }
    // Rules apply in file order, so a later rule wins on the same property.
    for i in 0..state.config.window_rules.len() {
        if !facts.matches(&state.config.window_rules[i].matchers) {
            continue;
        }
        match state.config.window_rules[i].action.clone() {
            RuleAction::Float if placing => placement.float = true,
            RuleAction::Tile if placing => placement.float = false,
            RuleAction::Fullscreen if placing => placement.fullscreen = true,
            RuleAction::Workspace(n) if placing => placement.workspace = Some(n),
            RuleAction::NoFocusSteal if placing => placement.no_focus_steal = true,
            RuleAction::Size(w, h) if placing => {
                placement.float = true;
                placement.size = Some((w, h));
            }
            RuleAction::Position(x, y) if placing => {
                placement.float = true;
                placement.position = Some((x, y));
            }
            RuleAction::Output(name) if placing => placement.output = Some(name),
            RuleAction::Float
            | RuleAction::Tile
            | RuleAction::Fullscreen
            | RuleAction::Workspace(_)
            | RuleAction::NoFocusSteal
            | RuleAction::Size(..)
            | RuleAction::Position(..)
            | RuleAction::Output(_) => {}
            RuleAction::Trust(trust) => {
                // COMP-07 §2: an X11 window never exceeds `standard`, whatever
                // the rule asks for.
                let trust = if facts.xwayland { AppTrust::Standard } else { trust };
                if let Some(slot) = window.user_data().get::<RuleTrust>() {
                    slot.0.set(trust);
                } else {
                    window
                        .user_data()
                        .insert_if_missing(|| RuleTrust(Cell::new(trust)));
                }
            }
            RuleAction::Seat(compat) => {
                // COMP-07 §6: X11 windows are always seat-locked.
                let compat = if facts.xwayland { SeatCompat::Lock } else { compat };
                if let Some(slot) = window.user_data().get::<RuleSeat>() {
                    slot.0.set(compat);
                } else {
                    window
                        .user_data()
                        .insert_if_missing(|| RuleSeat(Cell::new(compat)));
                }
            }
            RuleAction::IdleInhibit => {
                if let Some(surface) = crate::shell::window_surface(window) {
                    state.idle.add_rule_inhibitor(surface);
                }
            }
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
    cgroup: String,
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
            cgroup: pid.map(cgroup_of).unwrap_or_default(),
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
        if let Some(p) = &m.cgroup {
            if !p.matches(&self.cgroup) {
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

/// The client's cgroup path, or empty when the kernel will not say. Read once
/// per evaluation, off the input hot path.
fn cgroup_of(pid: i32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .map(|s| {
            // Unified hierarchy lines are `0::<path>`; take the path.
            s.lines()
                .find_map(|l| l.split("::").nth(1))
                .unwrap_or_default()
                .to_owned()
        })
        .unwrap_or_default()
}
