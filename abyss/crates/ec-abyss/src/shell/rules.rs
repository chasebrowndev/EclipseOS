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
//! classifier: a rule can raise a window's class, never lower it. Whichever
//! origin raised it is recorded as the window's `class_source` (COMP-05 §1).
//!
//! `irreversible_capable` (COMP-05 §1, S-06 §3.3) is a rule fact too: an
//! `irreversible-capable` rule decides it, and without one it defaults from
//! the app's desktop entry (see [`category_default`]).

use std::cell::Cell;
use std::hash::{Hash, Hasher};

use smithay::desktop::Window;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;

use crate::config::{Matchers, RuleAction, WindowRule};
use crate::state::AbyssState;
use crate::xwayland::security::{AppTrust, SeatCompat};

// Read by the renderer, written by `apply`; defined in `ec-abyss-render::userdata`.
pub use ec_abyss_render::userdata::{blur_of, opacity_of, RuleBlur, RuleOpacity};

/// Trust class pinned by an `app-trust` rule (COMP-05 §4). Consumed once
/// COMP-08 gates agent actions on it.
pub struct RuleTrust(pub Cell<AppTrust>);

/// Seat concurrency pinned by a `seat-compat` rule (COMP-04 §8).
pub struct RuleSeat(pub Cell<SeatCompat>);

/// The window's `irreversible_capable` fact (COMP-05 §1, S-06 §3.3), set on
/// every evaluation. The desktop-entry category default is cached per id
/// (keyed by a hash of it), so the file I/O happens at most once per window
/// per distinct id, and at most [`DEFAULT_CACHE`] times per window: a client
/// cycling `set_app_id` before every commit cannot drive a lookup per commit.
/// An id seen once the cache is full gets no lookup and defaults to false,
/// the same as an app without a desktop entry.
#[derive(Default)]
pub struct RuleIrreversible {
    value: Cell<bool>,
    cache: Cell<[(u64, bool); DEFAULT_CACHE]>,
    cached: Cell<usize>,
}

/// Distinct ids per window whose category default is looked up.
const DEFAULT_CACHE: usize = 4;

impl RuleIrreversible {
    /// Settle `value` for `id`: the rule if one decides, otherwise the
    /// category default from `lookup`, called only for an id not cached yet
    /// and only while the cache has room.
    fn update(&self, id: &str, rule: Option<bool>, lookup: impl FnOnce(&str) -> bool) -> bool {
        // The default is only needed when no rule decides, and the lookup is
        // file I/O: skip it then.
        let value = match rule {
            Some(v) => v,
            None => {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                id.hash(&mut h);
                let key = h.finish();
                let mut cache = self.cache.get();
                let n = self.cached.get();
                match cache[..n].iter().find(|(k, _)| *k == key) {
                    Some(&(_, v)) => v,
                    None if n < DEFAULT_CACHE => {
                        let v = lookup(id);
                        cache[n] = (key, v);
                        self.cache.set(cache);
                        self.cached.set(n + 1);
                        v
                    }
                    None => false,
                }
            }
        };
        self.value.set(value);
        value
    }
}

/// Where a window's sensitivity class came from (COMP-05 §1 `class_source`),
/// for audit and debugging only: nothing decides on it. Raise-only semantics
/// are untouched; this records which origin did the raise.
///
/// - [`CLASS_SOURCE_DEFAULT`]: never raised; the class is the `private` floor.
/// - [`CLASS_SOURCE_X11`]: an X11 window, whose class the COMP-07 §2
///   classifier pins (`xwayland::security::classify`) and nothing raised.
/// - [`CLASS_SOURCE_CAPTURE_POLICY`]: `capture.redact-app-id` named the app.
/// - [`CLASS_SOURCE_RULE_BASE`]` + i`: the `sensitivity` windowrule at index
///   `i` of the loaded rule list (file order). Indices past
///   `255 - CLASS_SOURCE_RULE_BASE` all read 255.
///
/// Values 3..16 are reserved for origins that do not exist yet (an
/// app-declared raise over `eclipse_semantic_v1`, COMP-09).
pub const CLASS_SOURCE_DEFAULT: u8 = 0;
/// See [`CLASS_SOURCE_DEFAULT`].
pub const CLASS_SOURCE_X11: u8 = 1;
/// See [`CLASS_SOURCE_DEFAULT`].
pub const CLASS_SOURCE_CAPTURE_POLICY: u8 = 2;
/// See [`CLASS_SOURCE_DEFAULT`].
pub const CLASS_SOURCE_RULE_BASE: u8 = 16;

/// The `class_source` of a raise by the windowrule at `index`.
pub fn rule_class_source(index: usize) -> u8 {
    u8::try_from(index)
        .ok()
        .and_then(|i| i.checked_add(CLASS_SOURCE_RULE_BASE))
        .unwrap_or(u8::MAX)
}

/// The recorded raise origin, kept on the surface beside the `sensitive` set
/// it explains: membership in that set is per surface and survives an unmap
/// and remap, which builds a new `Window`, so the provenance has to as well.
struct ClassSource(Cell<u8>);

/// Record that `source` raised `surface`'s class. Call only when the raise
/// actually changed it (`state.sensitive.insert` returned true), so the first
/// origin that raised a window keeps the record.
pub fn record_class_source(surface: &WlSurface, source: u8) {
    smithay::wayland::compositor::with_states(surface, |states| {
        if !states
            .data_map
            .insert_if_missing(|| ClassSource(Cell::new(source)))
        {
            if let Some(c) = states.data_map.get::<ClassSource>() {
                c.0.set(source);
            }
        }
    });
}

/// Which origin produced this window's sensitivity class (see
/// [`CLASS_SOURCE_DEFAULT`]).
pub fn class_source_of(window: &Window) -> u8 {
    let recorded = crate::shell::window_surface(window).and_then(|s| {
        smithay::wayland::compositor::with_states(&s, |states| {
            states.data_map.get::<ClassSource>().map(|c| c.0.get())
        })
    });
    resolve_class_source(recorded, window.x11_surface().is_some())
}

/// A recorded raise wins; otherwise an X11 window's class is the X11
/// classifier's (it never raises, so it is never recorded), and anything
/// else is the default.
fn resolve_class_source(recorded: Option<u8>, xwayland: bool) -> u8 {
    match recorded {
        Some(s) => s,
        None if xwayland => CLASS_SOURCE_X11,
        None => CLASS_SOURCE_DEFAULT,
    }
}

/// Marker that a window's placement is final. An xdg_toplevel is placed at
/// its initial commit, by which point xdg-shell has applied the `app_id`,
/// `title`, `set_parent` and min/max size sent before it, so [`apply`] sees
/// them and a window placed off the default settles right there. Two facts
/// can still arrive later: the fixed-size float default only counts once a
/// buffer is attached, and some clients name themselves after the initial
/// commit. So a window placed at the default is re-decided on each commit
/// until it settles, and never after: at the first commit whose placement
/// differs from the default (it is re-installed exactly once), or at the
/// first commit carrying a buffer (the map) once any rule has an identity to
/// match on. An X11 window has set its properties before it asks to be
/// mapped, so it settles at [`apply`] unless a rule still waits on a name.
pub struct Placed;

/// Marker for `windowrule "no-agent"`: the window is absent from every agent's
/// scene (`policy::scene`).
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

/// Whether any window carries an opacity override, i.e. whether the renderer
/// has to take the per-window path even with default decoration.
pub fn any_opacity_override<'a>(mut windows: impl Iterator<Item = &'a Window>) -> bool {
    windows.any(|w| w.user_data().get::<RuleOpacity>().is_some())
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
pub fn seat_compat_of(window: &Window) -> Option<SeatCompat> {
    window.user_data().get::<RuleSeat>().map(|s| s.0.get())
}

/// Whether the S-06 §3.3 app-capable fallback covers this window. False for a
/// window no evaluation has reached yet (it is not mapped).
pub fn irreversible_capable_of(window: &Window) -> bool {
    window
        .user_data()
        .get::<RuleIrreversible>()
        .is_some_and(|r| r.value.get())
}

/// Whether the agent scene filter must treat the window as absent.
pub fn hidden_from_agents(window: &Window) -> bool {
    window.user_data().get::<NoAgent>().is_some()
}

/// Evaluate every rule against a newly mapped window.
pub fn apply(state: &mut AbyssState, window: &Window) -> Placement {
    // S-05 §5: class is recomputed on map.
    crate::policy::classes::recompute(state, window);
    let (placement, named) = evaluate(state, window, true);
    let rules = !state.config.window_rules.is_empty();
    let settled = match window.toplevel() {
        None => !rules || named,
        // Installed off the default at its initial commit, a toplevel is
        // settled now; a later pass returning the same placement would only
        // re-install it.
        Some(t) => settles(
            placement != Placement::default(),
            super::has_buffer(t.wl_surface()),
            rules,
            named,
        ),
    };
    if settled {
        window.user_data().insert_if_missing(|| Placed);
    }
    placement
}

/// Re-evaluate after a commit. Property actions always re-apply; placement is
/// returned at most once, while the window settles (see [`Placed`]), and the
/// caller then has to re-install the window.
#[must_use]
pub fn reevaluate(state: &mut AbyssState, window: &Window) -> Option<Placement> {
    // S-05 §5: and on every commit, which is how title and app_id changes
    // arrive.
    crate::policy::classes::recompute(state, window);
    let rules = !state.config.window_rules.is_empty();
    if window.user_data().get::<Placed>().is_some() {
        if rules {
            evaluate(state, window, false);
        } else {
            // No rule can decide, but the category default still follows the
            // current id: a client naming itself after placement must not
            // keep the default looked up for its old (often empty) id.
            let (app_id, _) = crate::ipc::methods::identity_of(window);
            set_irreversible(window, &desktop_id_of(window, app_id.unwrap_or_default()), None);
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
        set_irreversible(window, &facts.desktop_id, None);
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
            // Settled after the loop by `irreversible_rule`.
            RuleAction::IrreversibleCapable(_) => {}
            RuleAction::Sensitivity(_) => {
                // Raise-only, and the class is already the strictest this
                // build represents; there is nothing to lower.
                if let Some(surface) = crate::shell::window_surface(window) {
                    if state.sensitive.insert(surface.clone()) {
                        record_class_source(&surface, rule_class_source(i));
                    }
                }
            }
        }
    }
    let irreversible = irreversible_rule(&state.config.window_rules, &facts);
    set_irreversible(window, &facts.desktop_id, irreversible);
    (placement, named)
}

/// What the last matching `irreversible-capable` rule says, if any rule does
/// (file order, a later rule wins, as for every other property).
fn irreversible_rule(rules: &[WindowRule], facts: &Facts) -> Option<bool> {
    rules.iter().rev().find_map(|r| match r.action {
        RuleAction::IrreversibleCapable(v) if facts.matches(&r.matchers) => Some(v),
        _ => None,
    })
}

/// Settle this evaluation's `irreversible_capable`: the last matching rule,
/// or the category default. Re-run on every evaluation, so a rule that stops
/// matching (a title change) falls back to the default.
fn set_irreversible(window: &Window, desktop_id: &str, rule: Option<bool>) {
    window.user_data().insert_if_missing(RuleIrreversible::default);
    if let Some(cell) = window.user_data().get::<RuleIrreversible>() {
        cell.update(desktop_id, rule, category_default);
    }
}

/// An explicit rule wins, both ways; otherwise the category default.
#[cfg(test)]
fn irreversible_value(default: bool, rule: Option<bool>) -> bool {
    rule.unwrap_or(default)
}

/// Desktop-entry categories whose apps are `irreversible_capable` by default
/// (S-06 §3.3: browsers, mail clients, terminals, file managers).
const CAPABLE_CATEGORIES: &[&str] = &["WebBrowser", "Email", "TerminalEmulator", "FileManager"];

/// The S-06 §3.3 default for an app: true when its desktop entry, found by
/// id (`<id>.desktop`, then lowercased for an X11 class like `XTerm`) in the
/// XDG data dirs, carries one of [`CAPABLE_CATEGORIES`]. No entry, or no id,
/// is false. Blocking file reads: called at most once per window per id.
fn category_default(id: &str) -> bool {
    #[cfg(test)]
    if let Some(dirs) = TEST_DATA_DIRS.with(|d| d.borrow().clone()) {
        return category_default_in(&dirs, id);
    }
    category_default_in(&data_dirs(), id)
}

#[cfg(test)]
thread_local! {
    /// Data dirs for [`category_default`] in an in-process harness test, so
    /// it never reads the host's desktop entries or touches the environment.
    pub(crate) static TEST_DATA_DIRS: std::cell::RefCell<Option<Vec<std::path::PathBuf>>> =
        const { std::cell::RefCell::new(None) };
}

fn category_default_in(dirs: &[std::path::PathBuf], id: &str) -> bool {
    if id.is_empty() || id.contains('/') {
        return false;
    }
    let lower = id.to_lowercase();
    let names: &[&str] = if lower == id { &[id] } else { &[id, &lower] };
    for dir in dirs {
        for name in names {
            let path = dir.join("applications").join(format!("{name}.desktop"));
            if let Some(text) = read_entry(&path) {
                // The first entry found shadows later ones, as XDG says.
                return capable_categories(&text);
            }
        }
    }
    false
}

/// Largest desktop entry read; anything past it is ignored.
const ENTRY_MAX: u64 = 64 * 1024;

/// Read a desktop entry on the calloop thread without letting the file stall
/// or flood it: opened non-blocking (a FIFO would otherwise block the open),
/// accepted only if it is a regular file once symlinks are followed (so no
/// FIFO, socket or `/dev/zero`), and read up to [`ENTRY_MAX`] bytes.
fn read_entry(path: &std::path::Path) -> Option<String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut buf = Vec::new();
    file.take(ENTRY_MAX).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// `$XDG_DATA_HOME` then `$XDG_DATA_DIRS`, with the spec's defaults.
fn data_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    match std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        Some(d) => dirs.push(d.into()),
        None => {
            if let Some(home) = std::env::var_os("HOME") {
                dirs.push(std::path::PathBuf::from(home).join(".local/share"));
            }
        }
    }
    let system = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.extend(system.split(':').filter(|d| !d.is_empty()).map(Into::into));
    dirs
}

/// Whether a desktop entry's `[Desktop Entry]` `Categories` names a capable
/// category. Other groups (`[Desktop Action …]`) are ignored.
fn capable_categories(text: &str) -> bool {
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        if let Some(v) = line.strip_prefix("Categories") {
            if let Some(v) = v.trim_start().strip_prefix('=') {
                return v
                    .split(';')
                    .map(str::trim)
                    .any(|c| CAPABLE_CATEGORIES.contains(&c));
            }
        }
    }
    false
}

/// The id a window's desktop entry is looked up by: its app id, or an X11
/// window's `WM_CLASS` class when it has none.
fn desktop_id_of(window: &Window, app_id: String) -> String {
    match window.x11_surface() {
        Some(x11) if app_id.is_empty() => x11.class(),
        _ => app_id,
    }
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
    /// The id its desktop entry is looked up by: the app id, or an X11
    /// window's `WM_CLASS` class.
    desktop_id: String,
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
            let area = entry
                .map(|e| crate::shell::tiling_area(state, &e.output).size)
                .unwrap_or_default();
            x11.is_transient_for().is_some()
                || x11_cannot_tile(
                    x11.min_size().unwrap_or_default(),
                    x11.max_size().unwrap_or_default(),
                    area,
                )
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
        let app_id = app_id.unwrap_or_default();
        let desktop_id = desktop_id_of(window, app_id.clone());
        Self {
            cgroup: pid.map(cgroup_of).unwrap_or_default(),
            app_id,
            desktop_id,
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

/// An X11 window that pins its size (WM_NORMAL_HINTS min == max) or whose
/// minimum cannot fit a half-area tile on either axis would be squashed or
/// stretched by the layout, so it floats instead. `area` is the output's
/// tiling area; an empty one (no output yet) never floats.
fn x11_cannot_tile(
    min: smithay::utils::Size<i32, smithay::utils::Logical>,
    max: smithay::utils::Size<i32, smithay::utils::Logical>,
    area: smithay::utils::Size<i32, smithay::utils::Logical>,
) -> bool {
    fixed_size(min, max) || (area.w > 0 && area.h > 0 && (min.w > area.w / 2 || min.h > area.h / 2))
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
    use super::{
        capable_categories, category_default_in, fixed_size, irreversible_rule, irreversible_value,
        resolve_class_source, rule_class_source, settles, x11_cannot_tile, Facts, RuleIrreversible,
        CLASS_SOURCE_CAPTURE_POLICY, CLASS_SOURCE_DEFAULT, CLASS_SOURCE_RULE_BASE, CLASS_SOURCE_X11,
        DEFAULT_CACHE,
    };
    use crate::config::{Matchers, Pattern, RuleAction, WindowRule};
    use smithay::utils::{Logical, Size};

    fn facts(app_id: &str, title: &str) -> Facts {
        Facts {
            app_id: app_id.into(),
            title: title.into(),
            pid: None,
            xwayland: false,
            cgroup: String::new(),
            output_connector: String::new(),
            output_identity: String::new(),
            workspace: 1,
            desktop_id: app_id.into(),
            is_dialog: false,
        }
    }

    fn rule(action: RuleAction, app_id: Option<&str>, title: Option<&str>) -> WindowRule {
        WindowRule {
            action,
            matchers: Matchers {
                app_id: app_id.and_then(Pattern::parse),
                title: title.and_then(Pattern::parse),
                ..Matchers::default()
            },
        }
    }

    /// An `applications/` dir holding the given `(id, Categories)` entries.
    fn data_dir(tag: &str, entries: &[(&str, &str)]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("abyss-rules-{tag}-{}", std::process::id()));
        let apps = dir.join("applications");
        std::fs::create_dir_all(&apps).expect("apps dir");
        for (id, cats) in entries {
            std::fs::write(
                apps.join(format!("{id}.desktop")),
                format!("[Desktop Entry]\nType=Application\nName={id}\nCategories={cats}\n"),
            )
            .expect("desktop entry");
        }
        dir
    }

    #[test]
    fn desktop_categories_decide_the_irreversible_default() {
        let dir = data_dir(
            "cats",
            &[
                ("firefox", "Network;WebBrowser;"),
                ("org.gnome.Evolution", "GNOME;GTK;Office;Email;Calendar;"),
                ("foot", "System;TerminalEmulator;"),
                ("xterm", "System;TerminalEmulator;"),
                ("org.gnome.Nautilus", "GNOME;GTK;Utility;Core;FileManager;"),
                ("mpv", "AudioVideo;Audio;Video;Player;"),
            ],
        );
        let dirs = [dir.clone()];
        assert!(category_default_in(&dirs, "firefox"));
        assert!(category_default_in(&dirs, "org.gnome.Evolution"));
        assert!(category_default_in(&dirs, "foot"));
        assert!(category_default_in(&dirs, "org.gnome.Nautilus"));
        // An X11 class is matched lowercased.
        assert!(category_default_in(&dirs, "XTerm"));
        assert!(!category_default_in(&dirs, "mpv"));
        // No entry, no id, or an id that would escape the dir: false.
        assert!(!category_default_in(&dirs, "unknown-app"));
        assert!(!category_default_in(&dirs, ""));
        assert!(!category_default_in(&dirs, "../applications/foot"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_non_regular_or_huge_entry_is_not_read() {
        let dir = data_dir("special", &[]);
        let apps = dir.join("applications");
        // A FIFO with no writer would block a plain open forever.
        let fifo = apps.join("fifo.desktop");
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).expect("path");
        // SAFETY: `c` is a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert!(!category_default_in(std::slice::from_ref(&dir), "fifo"));
        // A symlink to an endless device is not a regular file.
        std::os::unix::fs::symlink("/dev/zero", apps.join("zero.desktop")).expect("symlink");
        assert!(!category_default_in(std::slice::from_ref(&dir), "zero"));
        // A symlink to a regular entry still counts.
        std::fs::write(
            apps.join("real.desktop"),
            "[Desktop Entry]\nCategories=WebBrowser;\n",
        )
        .expect("entry");
        std::os::unix::fs::symlink(apps.join("real.desktop"), apps.join("link.desktop")).expect("symlink");
        assert!(category_default_in(std::slice::from_ref(&dir), "link"));
        // Only the first 64 KiB is read: a category past it is not seen.
        let mut big = String::from("[Desktop Entry]\n");
        big.push_str(&"#".repeat(64 * 1024));
        big.push_str("\nCategories=WebBrowser;\n");
        std::fs::write(apps.join("big.desktop"), big).expect("entry");
        assert!(!category_default_in(std::slice::from_ref(&dir), "big"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_late_name_gets_its_own_default() {
        // A window mapped with no id, which names itself a browser after
        // placement: the default follows the new id (the no-rules Placed
        // path in `reevaluate` runs this same update with the current id).
        let dir = data_dir("late", &[("firefox", "Network;WebBrowser;")]);
        let dirs = [dir.clone()];
        let slot = RuleIrreversible::default();
        assert!(!slot.update("", None, |id| category_default_in(&dirs, id)));
        assert!(slot.update("firefox", None, |id| category_default_in(&dirs, id)));
        assert!(slot.value.get());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn default_lookups_are_cached_and_capped_per_window() {
        let slot = RuleIrreversible::default();
        let lookups = std::cell::Cell::new(0);
        let lookup = |id: &str| {
            lookups.set(lookups.get() + 1);
            id == "firefox"
        };
        // Alternating ids every commit: one lookup per distinct id.
        for _ in 0..50 {
            assert!(slot.update("firefox", None, lookup));
            assert!(!slot.update("mpv", None, lookup));
        }
        assert_eq!(lookups.get(), 2);
        // A rule decides without a lookup.
        assert!(slot.update("other", Some(true), lookup));
        assert_eq!(lookups.get(), 2);
        // Past the cap, a new id gets no lookup and defaults to false.
        for i in 0..100 {
            slot.update(&format!("id{i}"), None, lookup);
        }
        assert_eq!(lookups.get(), DEFAULT_CACHE);
        assert!(!slot.update("firefox-late", None, |_| true));
        // Cached ids still answer.
        assert!(slot.update("firefox", None, lookup));
        assert_eq!(lookups.get(), DEFAULT_CACHE);
    }

    #[test]
    fn only_the_desktop_entry_group_counts() {
        assert!(capable_categories(
            "[Desktop Entry]\nCategories = Utility;FileManager\n"
        ));
        assert!(!capable_categories("[Desktop Entry]\nCategories=Utility;\n"));
        assert!(!capable_categories(
            "[Desktop Entry]\nName=x\n[Desktop Action new]\nCategories=WebBrowser;\n"
        ));
        // A substring is not a category.
        assert!(!capable_categories(
            "[Desktop Entry]\nCategories=WebBrowserLike;\n"
        ));
    }

    #[test]
    fn an_irreversible_rule_overrides_the_default_both_ways() {
        let off = [rule(RuleAction::IrreversibleCapable(false), Some("^foot$"), None)];
        let on = [rule(RuleAction::IrreversibleCapable(true), Some("^mpv$"), None)];
        // A terminal (default true) pinned false, a player (default false)
        // pinned true; a non-matching window keeps its default.
        assert!(!irreversible_value(
            true,
            irreversible_rule(&off, &facts("foot", ""))
        ));
        assert!(irreversible_value(
            false,
            irreversible_rule(&on, &facts("mpv", ""))
        ));
        assert!(irreversible_value(
            true,
            irreversible_rule(&on, &facts("foot", ""))
        ));
        assert!(!irreversible_value(
            false,
            irreversible_rule(&off, &facts("mpv", ""))
        ));
        // Other actions are not irreversible rules.
        let other = [rule(RuleAction::Float, Some("foot"), None)];
        assert_eq!(irreversible_rule(&other, &facts("foot", "")), None);
        // File order: the later matching rule wins.
        let both = [
            rule(RuleAction::IrreversibleCapable(false), Some("foot"), None),
            rule(RuleAction::IrreversibleCapable(true), Some("foot"), None),
        ];
        assert_eq!(irreversible_rule(&both, &facts("foot", "")), Some(true));
    }

    #[test]
    fn irreversible_is_re_evaluated_on_title_change() {
        // A browser whose banking tab is pinned false (say, a kiosk policy
        // turns the fallback off only there) returns to its default when the
        // title moves on; a rule matching the new title turns it back.
        let rules = [
            rule(
                RuleAction::IrreversibleCapable(false),
                Some("firefox"),
                Some("^Kiosk"),
            ),
            rule(RuleAction::IrreversibleCapable(true), Some("mpv"), Some("Admin")),
        ];
        let browser = |title| irreversible_value(true, irreversible_rule(&rules, &facts("firefox", title)));
        assert!(!browser("Kiosk — Firefox"));
        assert!(browser("Mail — Firefox"));
        let player = |title| irreversible_value(false, irreversible_rule(&rules, &facts("mpv", title)));
        assert!(!player("movie.mkv"));
        assert!(player("Admin panel"));
        assert!(!player("movie.mkv"));
    }

    #[test]
    fn class_source_names_each_origin() {
        // Unraised: the Wayland default, or the X11 classifier's pinned class.
        assert_eq!(resolve_class_source(None, false), CLASS_SOURCE_DEFAULT);
        assert_eq!(resolve_class_source(None, true), CLASS_SOURCE_X11);
        // A recorded raise wins over either, including on an X11 window.
        assert_eq!(
            resolve_class_source(Some(CLASS_SOURCE_CAPTURE_POLICY), false),
            CLASS_SOURCE_CAPTURE_POLICY
        );
        assert_eq!(
            resolve_class_source(Some(rule_class_source(3)), true),
            CLASS_SOURCE_RULE_BASE + 3
        );
        // The rule index is encoded, and saturates rather than wraps into
        // another origin's range.
        assert_eq!(rule_class_source(0), CLASS_SOURCE_RULE_BASE);
        assert_eq!(rule_class_source(239), u8::MAX);
        assert_eq!(rule_class_source(240), u8::MAX);
        assert_eq!(rule_class_source(100_000), u8::MAX);
        // The named origins are distinct and below the rule range.
        let named = [
            CLASS_SOURCE_DEFAULT,
            CLASS_SOURCE_X11,
            CLASS_SOURCE_CAPTURE_POLICY,
        ];
        assert!(named.iter().all(|s| *s < CLASS_SOURCE_RULE_BASE));
        assert_eq!(
            named.len(),
            named.iter().collect::<std::collections::HashSet<_>>().len()
        );
    }

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
    fn x11_fixed_or_oversized_min_floats() {
        let area = size(1920, 1000);
        // Fixed-size dialog.
        assert!(x11_cannot_tile(size(400, 300), size(400, 300), area));
        // Min larger than a half tile on one axis.
        assert!(x11_cannot_tile(size(1100, 100), size(0, 0), area));
        assert!(x11_cannot_tile(size(100, 600), size(0, 0), area));
        // Ordinary resizable window.
        assert!(!x11_cannot_tile(size(200, 100), size(0, 0), area));
        assert!(!x11_cannot_tile(size(0, 0), size(0, 0), area));
        // No output: never floats on the min rule.
        assert!(!x11_cannot_tile(size(5000, 5000), size(0, 0), size(0, 0)));
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
