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

/// Per-window blur override set by a matched `blur` rule, read by the
/// renderer. Only changes the on/off decision; an opaque window still never
/// blurs (see `render::window_elements`).
pub struct RuleBlur(pub Cell<bool>);

/// Trust class pinned by an `app-trust` rule (COMP-05 §4). Consumed once
/// COMP-08 gates agent actions on it.
pub struct RuleTrust(pub Cell<AppTrust>);

/// Seat concurrency pinned by a `seat-compat` rule (COMP-04 §8).
pub struct RuleSeat(pub Cell<SeatCompat>);

/// Marker that a window's placement is final. An xdg_toplevel is created
/// before the client has sent any of its state — `app_id`, `title`,
/// `set_parent`, min/max size — so placement is re-decided on each commit
/// until the window settles, and never after: at the first commit whose
/// placement differs from the default (it is re-installed exactly once), or
/// at the first commit carrying a buffer (the map) once any rule has an
/// identity to match on. An X11 window has set its properties before it asks
/// to be mapped, so it settles at [`apply`] unless a rule still waits on a
/// name.
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

/// The blur override a matched rule pinned on this window, if any.
pub fn blur_of(window: &Window) -> Option<bool> {
    window.user_data().get::<RuleBlur>().map(|b| b.0.get())
}

/// Whether any window carries a blur override, i.e. whether the renderer has
/// to take the per-window path even with default decoration.
pub fn any_blur_override<'a>(mut windows: impl Iterator<Item = &'a Window>) -> bool {
    windows.any(|w| w.user_data().get::<RuleBlur>().is_some())
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
    let (placement, named) = evaluate(state, window, true);
    if window.toplevel().is_none() && (state.config.window_rules.is_empty() || named) {
        window.user_data().insert_if_missing(|| Placed);
    }
    placement
}

/// Re-evaluate after a commit. Property actions always re-apply; placement is
/// returned at most once, while the window settles (see [`Placed`]), and the
/// caller then has to re-install the window.
#[must_use]
pub fn reevaluate(state: &mut AbyssState, window: &Window) -> Option<Placement> {
    let rules = !state.config.window_rules.is_empty();
    if window.user_data().get::<Placed>().is_some() {
        if rules {
            evaluate(state, window, false);
        }
        return None;
    }
    let (placement, named) = evaluate(state, window, true);
    let mapped = window
        .toplevel()
        .is_none_or(|t| super::has_buffer(t.wl_surface()));
    let changed = placement != Placement::default();
    if settles(changed, mapped, rules, named) {
        window.user_data().insert_if_missing(|| Placed);
    }
    changed.then_some(placement)
}

/// Whether a window's placement freezes on this commit (see [`Placed`]).
/// `changed`: this pass moves it off the default tiled placement it was
/// installed with, so the caller re-installs it now and never again.
/// Otherwise it waits for the map, and past it for a name when rules exist.
/// A client that pins its size or sets a parent only after its initial commit
/// is therefore still floated, provided it does so before its first buffer.
fn settles(changed: bool, mapped: bool, rules: bool, named: bool) -> bool {
    changed || (mapped && (named || !rules))
}

/// Returns the placement and whether the window has named itself yet.
fn evaluate(state: &mut AbyssState, window: &Window, placing: bool) -> (Placement, bool) {
    let mut placement = Placement::default();
    let facts = Facts::gather(state, window);
    // A dialog/utility window (xdg_toplevel with a parent, or an X11 window
    // carrying WM_TRANSIENT_FOR / a non-Normal window type) floats by
    // default — nothing declares this via windowrule today, so without this
    // every "Save As", preferences or confirmation popup would tile like a
    // primary window (COMP-05 §4 default is otherwise silent on this case).
    // A fixed-size xdg_toplevel (min == max, both non-zero, once it has a
    // buffer) floats for the same reason: tiling would stretch a window
    // that has said it cannot be resized — the secret prompt is one of
    // these. An explicit `tile`/`float` rule below still overrides it.
    if placing {
        placement.float = facts.is_dialog;
    }
    // Identity arrives after the first commit for some clients; until it
    // does, a placement pass matches on empty strings and the caller retries.
    let named = !(facts.app_id.is_empty() && facts.title.is_empty());
    if state.config.window_rules.is_empty() {
        return (placement, named);
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
            RuleAction::Blur(v) => {
                if let Some(b) = window.user_data().get::<RuleBlur>() {
                    b.0.set(v);
                } else {
                    window.user_data().insert_if_missing(|| RuleBlur(Cell::new(v)));
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
    (placement, named)
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
    /// True for a window that identifies itself as a dialog/utility rather
    /// than a primary toplevel: an xdg_toplevel with `parent` set, or an X11
    /// window carrying `WM_TRANSIENT_FOR` or a non-`Normal`
    /// `_NET_WM_WINDOW_TYPE`, or an xdg_toplevel that, once it has a buffer,
    /// has equal non-zero min and max size (see [`fixed_size`]). The size
    /// check is gated on a committed buffer so a client's pre-map size
    /// hints (some toolkits briefly clamp min==max to a not-yet-final
    /// natural size while probing layout) never false-positive. Nothing
    /// here is trusted for anything but the default float/tile choice — an
    /// untrusted client can always claim it.
    is_dialog: bool,
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
        let is_dialog = if let Some(toplevel) = window.toplevel() {
            // The fixed-size signal only means anything once the client has
            // put actual content on the surface: a toplevel with no buffer
            // yet is still mid-handshake, and some clients (Firefox's GTK
            // backend among them) briefly clamp min==max to their
            // not-yet-final natural size while probing layout, before ever
            // committing a buffer. Gating on `has_buffer` skips that
            // placeholder state without weakening the check for a window
            // that has truly pinned its size by the time it maps — min/max
            // are double-buffered together with the buffer attach, so a
            // genuinely fixed-size window (e.g. the secret prompt) still
            // reads `fixed_size == true` on the very commit that maps it.
            toplevel.parent().is_some()
                || (super::has_buffer(toplevel.wl_surface())
                    && smithay::wayland::compositor::with_states(toplevel.wl_surface(), |states| {
                        let mut guard = states
                            .cached_state
                            .get::<smithay::wayland::shell::xdg::SurfaceCachedState>();
                        let d = guard.current();
                        fixed_size(d.min_size, d.max_size)
                    }))
        } else if let Some(x11) = window.x11_surface() {
            x11.is_transient_for().is_some()
                || matches!(
                    x11.window_type(),
                    Some(
                        smithay::xwayland::xwm::WmWindowType::Dialog
                            | smithay::xwayland::xwm::WmWindowType::Utility
                            | smithay::xwayland::xwm::WmWindowType::Toolbar
                            | smithay::xwayland::xwm::WmWindowType::Splash
                    )
                )
        } else {
            false
        };
        Self {
            cgroup: pid.map(cgroup_of).unwrap_or_default(),
            app_id: app_id.unwrap_or_default(),
            title: title.unwrap_or_default(),
            pid,
            xwayland: window.toplevel().is_none(),
            output_connector: entry.map(|e| e.connector.clone()).unwrap_or_default(),
            output_identity: entry.map(|e| e.identity.clone()).unwrap_or_default(),
            workspace: entry.map(|e| e.active as i32 + 1).unwrap_or(1),
            is_dialog,
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

/// True when a toplevel has pinned its size: `min_size == max_size` with
/// both axes non-zero. Zero means "unconstrained" on that axis (xdg-shell), so
/// a zeroed pair is an ordinary resizable window, not a fixed one.
fn fixed_size(
    min: smithay::utils::Size<i32, smithay::utils::Logical>,
    max: smithay::utils::Size<i32, smithay::utils::Logical>,
) -> bool {
    min == max && min.w > 0 && min.h > 0
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

#[cfg(test)]
mod tests {
    use super::{fixed_size, settles};
    use smithay::utils::{Logical, Size};

    fn size(w: i32, h: i32) -> Size<i32, Logical> {
        Size::from((w, h))
    }

    #[test]
    fn only_an_equal_non_zero_min_max_is_fixed() {
        assert!(fixed_size(size(420, 180), size(420, 180)));
        // Unconstrained on both axes: an ordinary window.
        assert!(!fixed_size(size(0, 0), size(0, 0)));
        // Pinned on one axis only is still resizable on the other.
        assert!(!fixed_size(size(420, 0), size(420, 0)));
        assert!(!fixed_size(size(0, 180), size(0, 180)));
        // A range, however narrow, is resizable.
        assert!(!fixed_size(size(400, 180), size(420, 180)));
        // Only a minimum.
        assert!(!fixed_size(size(420, 180), size(0, 0)));
    }

    #[test]
    fn placement_settles_once_and_not_before_the_map() {
        // (changed, mapped, rules, named)
        // A client that pins its size after its bufferless initial commit:
        // nothing to do yet, and the window must not freeze tiled.
        assert!(!settles(false, false, false, true));
        assert!(!settles(false, false, true, true));
        // It becomes fixed-size before its first buffer: floated, and frozen.
        assert!(settles(true, false, false, true));
        // Mapped as an ordinary window: frozen tiled, no re-check per commit.
        assert!(settles(false, true, false, false));
        assert!(settles(false, true, true, true));
        // Mapped with rules but still nameless: wait for the name.
        assert!(!settles(false, true, true, false));
        // A rule that matched on something other than identity still freezes.
        assert!(settles(true, true, true, false));
    }
}
