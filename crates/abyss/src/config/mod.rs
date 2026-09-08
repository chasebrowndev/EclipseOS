// SPDX-License-Identifier: AGPL-3.0-only
//! KDL configuration (COMP-13 §1).
//!
//! Search path, later files overriding earlier ones:
//!   1. `/etc/eclipse/abyss.kdl`
//!   2. `$XDG_CONFIG_HOME/eclipse/abyss.kdl`
//!   3. `$XDG_CONFIG_HOME/eclipse/abyss.d/*.kdl` (sorted)
//!   4. `$XDG_CONFIG_HOME/abyss/config.kdl` (compatibility fallback)
//!      `--config <path>` replaces the whole search path with that one file.
//!
//! Parsing never fails hard: every malformed node is logged and skipped, and a
//! file that will not parse at all leaves the defaults in place.

pub mod watch;

use std::path::{Path, PathBuf};

use kdl::{KdlDocument, KdlNode, KdlValue};
use smithay::input::keyboard::{xkb, Keysym, ModifiersState};

use crate::input::{Action, Bind, Direction, Mods};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutKind {
    Dwindle,
    Master,
}

#[derive(Debug, Clone)]
pub struct General {
    pub gaps_in: i32,
    pub gaps_out: i32,
    pub border_size: i32,
    pub layout: LayoutKind,
    pub focus_follows_mouse: bool,
    pub col_active: [f32; 4],
    pub col_inactive: [f32; 4],
}

impl Default for General {
    fn default() -> Self {
        Self {
            gaps_in: 5,
            gaps_out: 10,
            border_size: 2,
            layout: LayoutKind::Dwindle,
            focus_follows_mouse: true,
            // eclipse amber on near-black
            col_active: [0.91, 0.64, 0.24, 1.0],
            col_inactive: [0.09, 0.09, 0.09, 1.0],
        }
    }
}

/// A live allowlist shared between `Config` and a Wayland global's filter.
///
/// Global visibility filters are `Fn(&Client) -> bool + Send + Sync + 'static`
/// callbacks owned by the display, so they cannot borrow `AbyssState` and
/// cannot see a later `Config`. This handle is the seam: `reload_now` writes
/// the new names, and the filter reads them on the next bind.
///
/// The `RwLock` is not on a hot path — it is touched at global-bind time and
/// on config reload, never in input delivery or a policy check — and the
/// process is single-threaded besides. It exists to satisfy `Sync`, not to
/// coordinate anything.
#[derive(Debug, Clone, Default)]
pub struct Allowlist(std::sync::Arc<std::sync::RwLock<Vec<String>>>);

impl Allowlist {
    pub fn new(names: Vec<String>) -> Self {
        Self(std::sync::Arc::new(std::sync::RwLock::new(names)))
    }

    /// Replace the names. Called from the config reload path only.
    pub fn set(&self, names: Vec<String>) {
        if let Ok(mut guard) = self.0.write() {
            *guard = names;
        }
    }

    /// Whether `name` is allowed. An empty list denies everything, and a
    /// poisoned lock denies too: fail-closed either way.
    pub fn contains(&self, name: &str) -> bool {
        self.0.read().is_ok_and(|g| g.iter().any(|a| a == name))
    }

    /// Whether the list is empty, i.e. nothing is allowed at all. A poisoned
    /// lock reports empty, which is the denying answer.
    pub fn is_empty(&self) -> bool {
        self.0.read().is_ok_and(|g| g.is_empty())
    }

    pub fn names(&self) -> Vec<String> {
        self.0.read().map(|g| g.clone()).unwrap_or_default()
    }
}

/// `clipboard { ... }` (COMP-06 §4).
#[derive(Debug, Clone, Default)]
pub struct Clipboard {
    /// Process names allowed to bind `zwlr_data_control_manager_v1`. Empty
    /// (the default) denies everyone — data control reads every selection.
    pub data_control_allow: Vec<String>,
}

/// `capture { ... }` (COMP-06 §3, COMP-02 §7). Fail-closed by construction:
/// every field defaults to "nothing is allowed, everything is redacted that
/// asks to be".
#[derive(Debug, Clone, Default)]
pub struct Capture {
    /// Process names allowed to bind `zwlr_screencopy_manager_v1`. Empty
    /// (the default) denies everyone — screen capture reads every pixel of an
    /// output, including other clients' windows.
    pub allow: Vec<String>,
    /// `app_id`s whose windows are `secret`: never composited into a capture
    /// target, only a solid placeholder (COMP-02 §7).
    pub redact_app_id: Vec<String>,
}

/// `xwayland { ... }` (COMP-07 §4, §7 open decision 2).
#[derive(Debug, Clone)]
pub struct Xwayland {
    /// Run an X server at all. Disabling it is the strongest isolation
    /// available: no X11 trust domain exists (COMP-07 §7).
    pub enable: bool,
    /// `false` (default): the compositor upscales X11 clients rendering at
    /// scale 1 — blurry but always correct. `true`: hand the clients DPI
    /// hints and let them render natively (COMP-07 §4).
    pub scaling_client: bool,
}

impl Default for Xwayland {
    fn default() -> Self {
        Self {
            enable: true,
            scaling_client: false,
        }
    }
}

/// `idle { ... }` (COMP-03 §7). Zero or absent disables a timeout.
#[derive(Debug, Clone, Default)]
pub struct Idle {
    /// Seconds of inactivity before outputs are powered off (DPMS).
    pub dpms_timeout: Option<u64>,
    /// Seconds of inactivity before `lock_command` is run.
    pub lock_timeout: Option<u64>,
    /// Locker to spawn on the lock timeout. Without one, the timeout is inert:
    /// the compositor never self-locks, because only a locker client can
    /// unlock an `ext_session_lock_v1` session.
    pub lock_command: Option<String>,
}

/// `misc { ... }` (COMP-13 §1.1).
#[derive(Debug, Clone, Default)]
pub struct Misc {
    /// `scripted-input`: whether the control socket may synthesise input
    /// (COMP-13 §2.2). Default **off**; only the socket owner can flip it,
    /// because only the socket owner can write the config.
    pub scripted_input: bool,
    /// `render-device`: `None` or `"auto"` means smithay's own primary-GPU
    /// query; otherwise a `/dev/dri/…` path or `pci:DDDD:BB:DD.F` address.
    /// Restart-only (COMP-13 §1.2) — the CLI flag and `ECLIPSE_RENDER_DEVICE`
    /// both override it.
    pub render_device: Option<String>,
}

/// One `output "<pattern>" { .. }` block (COMP-13 §4). Config wins over the
/// persisted layout.
#[derive(Debug, Clone, Default)]
pub struct OutputRule {
    /// Matched against the connector name and the output identity, `*` globbing.
    pub pattern: String,
    /// `WIDTHxHEIGHT` or `WIDTHxHEIGHT@REFRESH`, refresh in Hz or mHz.
    pub mode: Option<(i32, i32, i32)>,
    pub position: Option<(i32, i32)>,
    pub scale: Option<f64>,
    pub transform: Option<String>,
    pub enabled: Option<bool>,
    pub vrr: Option<bool>,
    /// `off` | `suspend` | `ignore` — what a lid-close does to this output
    /// (COMP-01 §4.1). Only meaningful on an internal panel.
    pub lid_close: Option<String>,
}

/// `render { ... }` (COMP-02 §2).
#[derive(Debug, Clone)]
pub struct Render {
    /// Allow the DRM backend to hand buffers straight to KMS planes.
    /// Composition is still forced for any frame containing a surface
    /// flagged sensitive (COMP-02 §7).
    pub direct_scanout: bool,
}

impl Default for Render {
    fn default() -> Self {
        Self { direct_scanout: true }
    }
}

/// `decoration { ... }` (COMP-13 §1.1, COMP-02 §9). Every effect here is off by
/// default: the defaults below are the "no effect" values, so a config without a
/// `decoration` block renders exactly as it did before milestone 9b and keeps
/// direct scanout available. Opacity and `dim-inactive` are rendered today;
/// `rounding`, `blur` and `shadow` are parsed and validated but not yet drawn
/// (they need a shader mask, multi-pass framebuffers and a nine-slice texture
/// respectively) — see docs/STATUS.md.
#[derive(Debug, Clone)]
pub struct Decoration {
    /// Corner radius in logical pixels; 0 disables.
    pub rounding: i32,
    /// Alpha applied to the focused window, 0.0..=1.0.
    pub active_opacity: f32,
    /// Alpha applied to every unfocused window, 0.0..=1.0.
    pub inactive_opacity: f32,
    /// Strength of the darkening overlay on unfocused windows, 0.0..=1.0.
    pub dim_inactive: f32,
    pub blur: Blur,
    pub shadow: Shadow,
}

impl Default for Decoration {
    fn default() -> Self {
        Self {
            rounding: 0,
            active_opacity: 1.0,
            inactive_opacity: 1.0,
            dim_inactive: 0.0,
            blur: Blur::default(),
            shadow: Shadow::default(),
        }
    }
}

impl Decoration {
    /// Whether any per-window effect diverges from the plain path. When this is
    /// false the renderer keeps the single `space_render_elements` call, so
    /// damage tracking and direct scanout behave as they do with no config.
    pub fn any_window_effect(&self) -> bool {
        self.active_opacity < 1.0 || self.inactive_opacity < 1.0 || self.dim_inactive > 0.0
    }
}

/// `blur { enabled #false; size 8; passes 2 }`. Dual-Kawase (COMP-02 §9).
#[derive(Debug, Clone)]
pub struct Blur {
    pub enabled: bool,
    pub size: i32,
    pub passes: i32,
}

impl Default for Blur {
    fn default() -> Self {
        Self {
            enabled: false,
            size: 8,
            passes: 2,
        }
    }
}

/// `shadow { enabled #false; range 20 }`. Nine-slice (COMP-02 §9).
#[derive(Debug, Clone)]
pub struct Shadow {
    pub enabled: bool,
    pub range: i32,
}

impl Default for Shadow {
    fn default() -> Self {
        Self {
            enabled: false,
            range: 20,
        }
    }
}

/// `animations { ... }` (COMP-13 §1.1, COMP-02 §9). Animations are geometry-only
/// and must never change what an agent sees: `scene`/`get_tree` always report
/// target geometry, not the interpolated value (COMP-08).
#[derive(Debug, Clone, Default)]
pub struct Animations {
    pub enabled: bool,
    /// `animation` nodes in file order; a later node for the same name wins.
    pub curves: Vec<Animation>,
}

impl Animations {
    /// The configured curve for `name`, if animations are on and one was given.
    #[allow(dead_code)] // read once 9b geometry interpolation lands
    pub fn get(&self, name: &str) -> Option<&Animation> {
        if !self.enabled {
            return None;
        }
        self.curves.iter().rev().find(|a| a.name == name)
    }
}

#[derive(Debug, Clone)]
pub struct Animation {
    #[allow(dead_code)] // matched by Animations::get
    pub name: String,
    pub duration_ms: u32,
    pub curve: String,
}

/// One `windowrule "<action>" { <matchers> }` block (COMP-05 §4).
///
/// A rule applies only when every matcher present in the block matches. A rule
/// whose action or matcher set this build cannot honour is dropped whole at
/// parse time — never applied in part.
#[derive(Debug, Clone)]
pub struct WindowRule {
    pub action: RuleAction,
    pub matchers: Matchers,
}

#[derive(Debug, Clone, Default)]
pub struct Matchers {
    pub app_id: Option<Pattern>,
    pub title: Option<Pattern>,
    pub pid: Option<i32>,
    pub xwayland: Option<bool>,
    /// Glob against the output connector or its persistent identity.
    pub output: Option<String>,
    pub workspace: Option<i32>,
}

impl Matchers {
    /// True when no matcher was given, i.e. the rule would hit every window.
    fn is_empty(&self) -> bool {
        self.app_id.is_none()
            && self.title.is_none()
            && self.pid.is_none()
            && self.xwayland.is_none()
            && self.output.is_none()
            && self.workspace.is_none()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RuleAction {
    Float,
    Tile,
    Workspace(i32),
    Opacity(f32),
    /// Raise-only: `secret` or `private`. `public` is refused at parse time
    /// because a rule may never lower a sensitivity class.
    Sensitivity(String),
    NoAgent,
    NoFocusSteal,
}

/// A COMP-05 §4 matcher pattern: a regular expression.
///
/// `regex` is used rather than a hand-rolled matcher because window titles are
/// client-controlled and this runs on the compositor's only thread — a
/// backtracking engine here would be a denial of service. A pattern that does
/// not compile is refused at parse time and takes its whole rule with it.
#[derive(Debug, Clone)]
pub struct Pattern(regex::Regex);

impl Pattern {
    /// Compile a COMP-05 §4 matcher. Unanchored, like every other regex — the
    /// spec's own examples (`^Meet —`) anchor explicitly when they mean to.
    pub fn parse(source: &str) -> Option<Self> {
        if source.is_empty() {
            return None;
        }
        match regex::Regex::new(source) {
            Ok(re) => Some(Self(re)),
            Err(err) => {
                tracing::warn!(pattern = source, %err, "bad matcher regex");
                None
            }
        }
    }

    pub fn matches(&self, text: &str) -> bool {
        self.0.is_match(text)
    }
}

/// Animation names and easing curves accepted by `animations`. Unknown values
/// are rejected at parse time rather than silently ignored at render time.
const ANIMATION_NAMES: [&str; 4] = ["windows", "workspaces", "fade", "border"];
const ANIMATION_CURVES: [&str; 4] = ["linear", "ease-in", "ease-out", "ease-in-out"];

/// `input { ... }` (COMP-13 §1.2, COMP-04). Keyboard settings are pushed to the
/// seat on reload; pointer settings are applied per libinput device as it
/// appears, and on reload to every device already open.
#[derive(Debug, Clone)]
pub struct Input {
    pub kb_layout: String,
    pub kb_variant: String,
    pub kb_options: Option<String>,
    pub repeat_rate: i32,
    pub repeat_delay: i32,
    /// `flat` | `adaptive`.
    pub accel_profile: String,
    pub touchpad: Touchpad,
}

impl Default for Input {
    fn default() -> Self {
        Self {
            kb_layout: "us".into(),
            kb_variant: String::new(),
            kb_options: None,
            repeat_rate: 40,
            repeat_delay: 300,
            accel_profile: "adaptive".into(),
            touchpad: Touchpad::default(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Touchpad {
    pub natural_scroll: bool,
    pub tap_to_click: bool,
    /// Disable-while-typing.
    pub dwt: bool,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub general: General,
    pub render: Render,
    pub decoration: Decoration,
    pub animations: Animations,
    pub clipboard: Clipboard,
    pub capture: Capture,
    pub xwayland: Xwayland,
    pub idle: Idle,
    pub misc: Misc,
    pub input: Input,
    pub binds: Vec<Bind>,
    /// Per-workspace layout overrides, indexed 1..=10.
    pub workspace_layout: [Option<LayoutKind>; 10],
    /// `output` blocks in file order; the last match wins.
    pub outputs: Vec<OutputRule>,
    /// `windowrule` blocks in file order; all matching rules apply, later ones
    /// overriding earlier ones for the same property.
    pub window_rules: Vec<WindowRule>,
    /// Files this config was built from, in load order. The hot-reload
    /// watcher watches these and the directories that would contain them.
    pub sources: Vec<PathBuf>,
    /// `--config <path>`, if one was given. Reload must honour it rather than
    /// falling back to the search path.
    pub explicit: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            general: General::default(),
            render: Render::default(),
            decoration: Decoration::default(),
            animations: Animations::default(),
            clipboard: Clipboard::default(),
            capture: Capture::default(),
            xwayland: Xwayland::default(),
            idle: Idle::default(),
            misc: Misc::default(),
            input: Input::default(),
            binds: default_binds(),
            workspace_layout: Default::default(),
            outputs: Vec::new(),
            window_rules: Vec::new(),
            sources: Vec::new(),
            explicit: None,
        }
    }
}

fn m(logo: bool, shift: bool, ctrl: bool, alt: bool) -> Mods {
    Mods {
        logo,
        shift,
        ctrl,
        alt,
    }
}

/// Hyprland-ish defaults. `Super+Escape` is bound here and nowhere else: it is
/// the trusted-UI override chord (COMP-04 §6), always present and not
/// rebindable — `parse_bind` refuses any config that names it.
pub fn default_binds() -> Vec<Bind> {
    let sup = m(true, false, false, false);
    let sup_shift = m(true, true, false, false);
    let mut b = vec![
        Bind {
            mods: sup,
            key: Keysym::Escape,
            action: Action::AgentOverride,
        },
        Bind {
            mods: sup,
            key: Keysym::space,
            action: Action::AgentAttention,
        },
        Bind {
            mods: sup,
            key: Keysym::Return,
            action: Action::Spawn("kitty".into()),
        },
        Bind {
            mods: sup,
            key: Keysym::q,
            action: Action::Close,
        },
        Bind {
            mods: sup,
            key: Keysym::v,
            action: Action::ToggleFloating,
        },
        Bind {
            mods: sup,
            key: Keysym::e,
            action: Action::ToggleLayout,
        },
        Bind {
            mods: sup,
            key: Keysym::h,
            action: Action::Focus(Direction::Left),
        },
        Bind {
            mods: sup,
            key: Keysym::j,
            action: Action::Focus(Direction::Down),
        },
        Bind {
            mods: sup,
            key: Keysym::k,
            action: Action::Focus(Direction::Up),
        },
        Bind {
            mods: sup,
            key: Keysym::l,
            action: Action::Focus(Direction::Right),
        },
        Bind {
            mods: sup_shift,
            key: Keysym::H,
            action: Action::Move(Direction::Left),
        },
        Bind {
            mods: sup_shift,
            key: Keysym::J,
            action: Action::Move(Direction::Down),
        },
        Bind {
            mods: sup_shift,
            key: Keysym::K,
            action: Action::Move(Direction::Up),
        },
        Bind {
            mods: sup_shift,
            key: Keysym::L,
            action: Action::Move(Direction::Right),
        },
        Bind {
            mods: sup_shift,
            key: Keysym::Q,
            action: Action::Quit,
        },
    ];
    const DIGITS: [Keysym; 10] = [
        Keysym::_1,
        Keysym::_2,
        Keysym::_3,
        Keysym::_4,
        Keysym::_5,
        Keysym::_6,
        Keysym::_7,
        Keysym::_8,
        Keysym::_9,
        Keysym::_0,
    ];
    // Shifted digits on a US layout; both forms are accepted so the bind fires
    // whether or not the client layout shifts the symbol.
    const SHIFTED: [Keysym; 10] = [
        Keysym::exclam,
        Keysym::at,
        Keysym::numbersign,
        Keysym::dollar,
        Keysym::percent,
        Keysym::asciicircum,
        Keysym::ampersand,
        Keysym::asterisk,
        Keysym::parenleft,
        Keysym::parenright,
    ];
    for (i, key) in DIGITS.iter().enumerate() {
        b.push(Bind {
            mods: sup,
            key: *key,
            action: Action::SwitchWorkspace(i + 1),
        });
        b.push(Bind {
            mods: sup_shift,
            key: *key,
            action: Action::MoveToWorkspace(i + 1),
        });
        b.push(Bind {
            mods: sup_shift,
            key: SHIFTED[i],
            action: Action::MoveToWorkspace(i + 1),
        });
    }
    b
}

/// Collect the string arguments of a list node, warning when the same node
/// appears twice in one block: the second occurrence replaces the first rather
/// than adding to it, and silently dropping names from an allowlist is the
/// failure direction that matters in a fail-closed path.
fn names(n: &KdlNode, seen: &mut bool) -> Vec<String> {
    if *seen {
        tracing::warn!(
            node = n.name().value(),
            "repeated node replaces the previous one; list every name on a single node"
        );
    }
    *seen = true;
    args(n)
        .into_iter()
        .filter_map(|v| v.as_string().map(str::to_string))
        .collect()
}

impl Config {
    /// Load from `explicit` if given, else from the search path. Errors are
    /// logged; the returned config is always usable.
    pub fn load(explicit: Option<&Path>) -> Self {
        let files: Vec<PathBuf> = match explicit {
            Some(p) => vec![p.to_path_buf()],
            None => search_path(),
        };
        let mut cfg = Config {
            explicit: explicit.map(|p| p.to_path_buf()),
            ..Config::default()
        };
        let mut binds_from_file = Vec::new();
        let mut any = false;
        for f in &files {
            let text = match std::fs::read_to_string(f) {
                Ok(t) => t,
                Err(e) => {
                    if explicit.is_some() {
                        tracing::error!(path = %f.display(), %e, "config unreadable, using defaults");
                    }
                    continue;
                }
            };
            let doc = match text.parse::<KdlDocument>() {
                Ok(d) => d,
                Err(e) => {
                    tracing::error!(path = %f.display(), error = %e, "config parse failed, file ignored");
                    continue;
                }
            };
            any = true;
            cfg.sources.push(f.clone());
            cfg.apply(&doc, &mut binds_from_file);
        }
        if !binds_from_file.is_empty() {
            cfg.binds = binds_from_file;
        }
        if !any {
            tracing::info!("no config found, using built-in defaults");
        } else {
            tracing::info!(sources = ?cfg.sources, binds = cfg.binds.len(), "config loaded");
        }
        cfg
    }

    /// Re-read the same sources. The inotify watcher (`config::watch`) calls
    /// this; `Config::load` never fails hard, so the result is always usable.
    pub fn reload(&self) -> Self {
        Self::load(self.explicit.as_deref())
    }

    fn apply(&mut self, doc: &KdlDocument, binds: &mut Vec<Bind>) {
        for node in doc.nodes() {
            match node.name().value() {
                "general" => self.apply_general(node),
                "bind" => match parse_bind(node) {
                    Ok(b) => binds.push(b),
                    Err(e) => tracing::warn!(error = %e, "ignoring bind"),
                },
                "workspace" => self.apply_workspace(node),
                "render" => self.apply_render(node),
                "clipboard" => self.apply_clipboard(node),
                "capture" => self.apply_capture(node),
                "xwayland" => self.apply_xwayland(node),
                "idle" => self.apply_idle(node),
                "misc" => self.apply_misc(node),
                "input" => self.apply_input(node),
                "output" => self.apply_output(node),
                "decoration" => self.apply_decoration(node),
                "animations" => self.apply_animations(node),
                "windowrule" => self.apply_windowrule(node),
                other => tracing::warn!(node = other, "unknown config node, ignored"),
            }
        }
    }

    fn apply_general(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            let name = n.name().value();
            match name {
                "gaps-in" => set_i32(&mut self.general.gaps_in, n),
                "gaps-out" => set_i32(&mut self.general.gaps_out, n),
                "border-size" => set_i32(&mut self.general.border_size, n),
                "focus-follows-mouse" => {
                    if let Some(b) = arg(n).and_then(KdlValue::as_bool) {
                        self.general.focus_follows_mouse = b;
                    }
                }
                "layout" => match arg(n).and_then(KdlValue::as_string) {
                    Some("dwindle") => self.general.layout = LayoutKind::Dwindle,
                    Some("master") => self.general.layout = LayoutKind::Master,
                    other => tracing::warn!(?other, "unknown layout, keeping default"),
                },
                "col-active-border" | "col-inactive-border" => {
                    match arg(n).and_then(KdlValue::as_string).and_then(parse_color) {
                        Some(c) if name.starts_with("col-active") => self.general.col_active = c,
                        Some(c) => self.general.col_inactive = c,
                        None => tracing::warn!(node = name, "bad color, keeping default"),
                    }
                }
                other => tracing::warn!(node = other, "unknown general key, ignored"),
            }
        }
    }

    fn apply_render(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "direct-scanout" => {
                    self.render.direct_scanout = arg(n).and_then(KdlValue::as_bool).unwrap_or(true);
                }
                other => tracing::warn!(node = other, "unknown render node, ignored"),
            }
        }
    }

    fn apply_clipboard(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        let mut seen_allow = false;
        for n in children.nodes() {
            match n.name().value() {
                "data-control-allow" => {
                    self.clipboard.data_control_allow = names(n, &mut seen_allow);
                }
                other => tracing::warn!(node = other, "unknown clipboard node, ignored"),
            }
        }
    }

    fn apply_capture(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        let mut seen_allow = false;
        let mut seen_redact = false;
        for n in children.nodes() {
            match n.name().value() {
                "allow" => {
                    self.capture.allow = names(n, &mut seen_allow);
                }
                "redact-app-id" => {
                    self.capture.redact_app_id = names(n, &mut seen_redact);
                }
                other => tracing::warn!(node = other, "unknown capture node, ignored"),
            }
        }
    }

    fn apply_xwayland(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            let name = n.name().value();
            match name {
                "enable" => {
                    if let Some(b) = arg(n).and_then(KdlValue::as_bool) {
                        self.xwayland.enable = b;
                    }
                }
                "scaling" => match arg(n).and_then(KdlValue::as_string) {
                    Some("client") => self.xwayland.scaling_client = true,
                    Some("compositor") => self.xwayland.scaling_client = false,
                    other => tracing::warn!(?other, "unknown xwayland scaling mode, keeping default"),
                },
                other => tracing::warn!(node = other, "unknown xwayland node, ignored"),
            }
        }
    }

    fn apply_idle(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            let name = n.name().value();
            match name {
                "dpms-timeout-seconds" | "lock-timeout-seconds" => {
                    match arg(n).and_then(KdlValue::as_integer) {
                        Some(v) if v >= 0 => {
                            let v = (v as u64 != 0).then_some(v as u64);
                            if name == "dpms-timeout-seconds" {
                                self.idle.dpms_timeout = v;
                            } else {
                                self.idle.lock_timeout = v;
                            }
                        }
                        _ => tracing::warn!(node = name, "idle timeout must be a non-negative integer"),
                    }
                }
                "lock-command" => match arg(n).and_then(KdlValue::as_string) {
                    Some(c) => self.idle.lock_command = Some(c.to_owned()),
                    None => tracing::warn!("idle lock-command needs a string argument"),
                },
                other => tracing::warn!(node = other, "unknown idle node, ignored"),
            }
        }
    }

    /// `misc { scripted-input #false }`. Absent keys keep their defaults; the
    /// scripted-input default is `false` and stays `false` on a malformed value.
    fn apply_input(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "kb-layout" => {
                    if let Some(v) = arg(n).and_then(KdlValue::as_string) {
                        self.input.kb_layout = v.to_string();
                    }
                }
                "kb-variant" => {
                    if let Some(v) = arg(n).and_then(KdlValue::as_string) {
                        self.input.kb_variant = v.to_string();
                    }
                }
                "kb-options" => {
                    self.input.kb_options = arg(n)
                        .and_then(KdlValue::as_string)
                        .filter(|v| !v.is_empty())
                        .map(str::to_string);
                }
                "repeat-rate" => set_i32(&mut self.input.repeat_rate, n),
                "repeat-delay" => set_i32(&mut self.input.repeat_delay, n),
                "accel-profile" => match arg(n).and_then(KdlValue::as_string) {
                    Some(v @ ("flat" | "adaptive")) => self.input.accel_profile = v.to_string(),
                    other => tracing::warn!(?other, "unknown accel-profile, ignored"),
                },
                "touchpad" => self.apply_touchpad(n),
                other => tracing::warn!(node = other, "unknown input key, ignored"),
            }
        }
    }

    fn apply_touchpad(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            let name = n.name().value();
            let Some(b) = arg(n).and_then(KdlValue::as_bool) else {
                tracing::warn!(node = name, "touchpad key needs a boolean, ignored");
                continue;
            };
            match name {
                "natural-scroll" => self.input.touchpad.natural_scroll = b,
                "tap-to-click" => self.input.touchpad.tap_to_click = b,
                "dwt" => self.input.touchpad.dwt = b,
                other => tracing::warn!(node = other, "unknown touchpad key, ignored"),
            }
        }
    }

    /// `decoration { rounding 8; active-opacity 1.0; blur { ... } }`.
    /// Out-of-range values are warned about and dropped, keeping the default.
    fn apply_decoration(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            let name = n.name().value();
            match name {
                "rounding" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (0..=64).contains(&v) => self.decoration.rounding = v as i32,
                    _ => tracing::warn!("decoration rounding must be an integer 0..=64"),
                },
                "active-opacity" | "inactive-opacity" | "dim-inactive" => match arg(n).and_then(as_f64) {
                    Some(v) if (0.0..=1.0).contains(&v) => {
                        let v = v as f32;
                        match name {
                            "active-opacity" => self.decoration.active_opacity = v,
                            "inactive-opacity" => self.decoration.inactive_opacity = v,
                            _ => self.decoration.dim_inactive = v,
                        }
                    }
                    _ => tracing::warn!(node = name, "decoration key must be 0.0..=1.0"),
                },
                "blur" => self.apply_blur(n),
                "shadow" => self.apply_shadow(n),
                other => tracing::warn!(node = other, "unknown decoration key, ignored"),
            }
        }
    }

    fn apply_blur(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "enabled" => {
                    self.decoration.blur.enabled = arg(n).and_then(KdlValue::as_bool).unwrap_or(true);
                }
                "size" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (1..=64).contains(&v) => self.decoration.blur.size = v as i32,
                    _ => tracing::warn!("blur size must be an integer 1..=64"),
                },
                "passes" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (1..=6).contains(&v) => self.decoration.blur.passes = v as i32,
                    _ => tracing::warn!("blur passes must be an integer 1..=6"),
                },
                other => tracing::warn!(node = other, "unknown blur key, ignored"),
            }
        }
    }

    fn apply_shadow(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "enabled" => {
                    self.decoration.shadow.enabled = arg(n).and_then(KdlValue::as_bool).unwrap_or(true);
                }
                "range" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (0..=128).contains(&v) => self.decoration.shadow.range = v as i32,
                    _ => tracing::warn!("shadow range must be an integer 0..=128"),
                },
                other => tracing::warn!(node = other, "unknown shadow key, ignored"),
            }
        }
    }

    /// `animations { enabled #true; animation "windows" duration="150ms" curve="ease-out" }`.
    fn apply_animations(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "enabled" => {
                    self.animations.enabled = arg(n).and_then(KdlValue::as_bool).unwrap_or(true);
                }
                "animation" => self.apply_animation(n),
                other => tracing::warn!(node = other, "unknown animations key, ignored"),
            }
        }
    }

    fn apply_animation(&mut self, node: &KdlNode) {
        let Some(name) = arg(node).and_then(KdlValue::as_string) else {
            tracing::warn!("animation node needs a name argument");
            return;
        };
        if !ANIMATION_NAMES.contains(&name) {
            tracing::warn!(name, "unknown animation name, ignored");
            return;
        }
        let mut anim = Animation {
            name: name.to_owned(),
            duration_ms: 150,
            curve: "ease-out".to_owned(),
        };
        for e in node.entries() {
            let Some(key) = e.name().map(|k| k.value().to_owned()) else {
                continue; // the positional name argument
            };
            match key.as_str() {
                "duration" => match parse_duration_ms(e.value()) {
                    Some(ms) if ms <= 10_000 => anim.duration_ms = ms,
                    _ => {
                        tracing::warn!(name, "animation duration must be <= 10s, e.g. \"150ms\"");
                        return;
                    }
                },
                "curve" => match e.value().as_string() {
                    Some(c) if ANIMATION_CURVES.contains(&c) => anim.curve = c.to_owned(),
                    other => {
                        tracing::warn!(name, ?other, "unknown animation curve, rule ignored");
                        return;
                    }
                },
                other => tracing::warn!(node = other, "unknown animation property, ignored"),
            }
        }
        self.animations.curves.push(anim);
    }

    /// `windowrule "float" { app-id "pavucontrol|org.gnome.Calculator" }`
    /// (COMP-05 §4). An action or matcher this build cannot honour drops the
    /// whole rule with a warning, so a rule never applies in part.
    fn apply_windowrule(&mut self, node: &KdlNode) {
        let Some(action) = arg(node).and_then(KdlValue::as_string) else {
            tracing::warn!("windowrule needs an action argument");
            return;
        };
        let mut words = action.split_whitespace();
        let verb = words.next().unwrap_or_default();
        let param = words.next();
        let action = match (verb, param) {
            ("float", None) => RuleAction::Float,
            ("tile", None) => RuleAction::Tile,
            ("no-agent", None) => RuleAction::NoAgent,
            ("no-focus-steal", None) => RuleAction::NoFocusSteal,
            ("workspace", Some(p)) => match p.parse::<i32>() {
                Ok(n) if (1..=10).contains(&n) => RuleAction::Workspace(n),
                _ => {
                    tracing::warn!(action, "windowrule workspace must be 1..=10, rule ignored");
                    return;
                }
            },
            ("opacity", Some(p)) => match p.parse::<f32>() {
                Ok(v) if (0.0..=1.0).contains(&v) => RuleAction::Opacity(v),
                _ => {
                    tracing::warn!(action, "windowrule opacity must be 0.0..=1.0, rule ignored");
                    return;
                }
            },
            ("sensitivity", Some(p @ ("secret" | "private"))) => RuleAction::Sensitivity(p.to_owned()),
            ("sensitivity", Some(p)) => {
                // App-declared sensitivity may only raise a class, never lower.
                tracing::warn!(class = p, "windowrule sensitivity may only raise, rule ignored");
                return;
            }
            _ => {
                tracing::warn!(action, "unimplemented windowrule action, rule ignored");
                return;
            }
        };

        let mut matchers = Matchers::default();
        let Some(children) = node.children() else {
            tracing::warn!(
                ?action,
                "windowrule with no matchers would hit every window, ignored"
            );
            return;
        };
        for n in children.nodes() {
            let name = n.name().value();
            match name {
                "app-id" | "title" => {
                    let Some(pat) = arg(n).and_then(KdlValue::as_string).and_then(Pattern::parse) else {
                        tracing::warn!(
                            matcher = name,
                            "matcher pattern is not a valid regex, rule ignored"
                        );
                        return;
                    };
                    if name == "app-id" {
                        matchers.app_id = Some(pat);
                    } else {
                        matchers.title = Some(pat);
                    }
                }
                "pid" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if v > 0 => matchers.pid = Some(v as i32),
                    _ => {
                        tracing::warn!("windowrule pid must be a positive integer, rule ignored");
                        return;
                    }
                },
                "xwayland" => matchers.xwayland = Some(arg(n).and_then(KdlValue::as_bool).unwrap_or(true)),
                "output" => match arg(n).and_then(KdlValue::as_string) {
                    Some(v) => matchers.output = Some(v.to_owned()),
                    None => {
                        tracing::warn!("windowrule output needs a name pattern, rule ignored");
                        return;
                    }
                },
                "workspace" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (1..=10).contains(&v) => matchers.workspace = Some(v as i32),
                    _ => {
                        tracing::warn!("windowrule workspace matcher must be 1..=10, rule ignored");
                        return;
                    }
                },
                other => {
                    tracing::warn!(matcher = other, "unimplemented windowrule matcher, rule ignored");
                    return;
                }
            }
        }
        if matchers.is_empty() {
            tracing::warn!(
                ?action,
                "windowrule with no matchers would hit every window, ignored"
            );
            return;
        }
        self.window_rules.push(WindowRule { action, matchers });
    }

    fn apply_misc(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "scripted-input" => {
                    self.misc.scripted_input = arg(n).and_then(KdlValue::as_bool).unwrap_or(false);
                }
                "render-device" => match arg(n).and_then(KdlValue::as_string) {
                    Some("auto") => self.misc.render_device = None,
                    Some(v) => self.misc.render_device = Some(v.to_owned()),
                    None => tracing::warn!("render-device needs a string, ignored"),
                },
                // Restart-only knobs (COMP-13 §1.2); parsed elsewhere or not yet.
                "xwayland" => {}
                other => tracing::warn!(node = other, "unknown misc key, ignored"),
            }
        }
    }

    fn apply_workspace(&mut self, node: &KdlNode) {
        let Some(idx) = arg(node).and_then(KdlValue::as_integer) else {
            tracing::warn!("workspace node needs a number argument");
            return;
        };
        if !(1..=10).contains(&idx) {
            tracing::warn!(idx, "workspace out of range 1..=10, ignored");
            return;
        }
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            if n.name().value() == "layout" {
                let slot = &mut self.workspace_layout[idx as usize - 1];
                match arg(n).and_then(KdlValue::as_string) {
                    Some("dwindle") => *slot = Some(LayoutKind::Dwindle),
                    Some("master") => *slot = Some(LayoutKind::Master),
                    other => tracing::warn!(?other, "unknown workspace layout"),
                }
            }
        }
    }

    fn apply_output(&mut self, node: &KdlNode) {
        let Some(pattern) = arg(node).and_then(KdlValue::as_string).map(str::to_owned) else {
            tracing::warn!("output node needs a name pattern argument");
            return;
        };
        let mut rule = OutputRule {
            pattern,
            ..Default::default()
        };
        let Some(children) = node.children() else {
            self.outputs.push(rule);
            return;
        };
        for n in children.nodes() {
            let name = n.name().value();
            match name {
                "mode" => match arg(n)
                    .and_then(KdlValue::as_string)
                    .and_then(crate::outputs::persist::parse_mode)
                {
                    Some(m) => rule.mode = Some(m),
                    None => tracing::warn!("bad output mode, expected \"1920x1080@60\""),
                },
                "position" => {
                    let a = n.entries();
                    match (
                        a.first().and_then(|e| e.value().as_integer()),
                        a.get(1).and_then(|e| e.value().as_integer()),
                    ) {
                        (Some(x), Some(y)) => rule.position = Some((x as i32, y as i32)),
                        _ => tracing::warn!("output position takes two integers"),
                    }
                }
                "scale" => match arg(n).and_then(as_f64) {
                    Some(s) if s > 0.0 => rule.scale = Some(s),
                    _ => tracing::warn!("output scale must be a positive number"),
                },
                "transform" => match arg(n).and_then(KdlValue::as_string) {
                    Some(t) if crate::outputs::parse_transform(t).is_some() => {
                        rule.transform = Some(t.to_owned())
                    }
                    other => tracing::warn!(?other, "unknown output transform"),
                },
                "enabled" | "disabled" => match arg(n).and_then(KdlValue::as_bool) {
                    Some(v) => rule.enabled = Some(v == (name == "enabled")),
                    None => rule.enabled = Some(name == "enabled"),
                },
                "lid-close" => match arg(n).and_then(KdlValue::as_string) {
                    Some(v @ ("off" | "suspend" | "ignore")) => rule.lid_close = Some(v.to_owned()),
                    _ => tracing::warn!("output lid-close must be off, suspend or ignore"),
                },
                "vrr" | "adaptive-sync" => rule.vrr = arg(n).and_then(KdlValue::as_bool).or(Some(true)),
                other => tracing::warn!(node = other, "unknown output key, ignored"),
            }
        }
        self.outputs.push(rule);
    }

    /// The effective rule for an output, later blocks overriding earlier ones.
    pub fn output_rule(&self, connector: &str, identity: &str) -> OutputRule {
        let mut out = OutputRule::default();
        for r in &self.outputs {
            if !(glob_match(&r.pattern, connector) || glob_match(&r.pattern, identity)) {
                continue;
            }
            out.pattern = r.pattern.clone();
            out.mode = r.mode.or(out.mode);
            out.position = r.position.or(out.position);
            out.scale = r.scale.or(out.scale);
            out.transform = r.transform.clone().or(out.transform);
            out.enabled = r.enabled.or(out.enabled);
            out.lid_close = r.lid_close.clone().or(out.lid_close);
            out.vrr = r.vrr.or(out.vrr);
        }
        out
    }

    pub fn layout_for(&self, workspace: usize) -> LayoutKind {
        self.workspace_layout
            .get(workspace.saturating_sub(1))
            .copied()
            .flatten()
            .unwrap_or(self.general.layout)
    }

    pub fn action_for(&self, mods: &ModifiersState, key: Keysym) -> Option<&Action> {
        self.binds
            .iter()
            .find(|b| b.key == key && b.mods.matches(mods))
            .map(|b| &b.action)
    }
}

fn search_path() -> Vec<PathBuf> {
    let mut out = vec![PathBuf::from("/etc/eclipse/abyss.kdl")];
    let cfg_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    let Some(base) = cfg_home else { return out };
    out.push(base.join("eclipse/abyss.kdl"));
    if let Ok(dir) = std::fs::read_dir(base.join("eclipse/abyss.d")) {
        let mut drop_ins: Vec<PathBuf> = dir
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "kdl"))
            .collect();
        drop_ins.sort();
        out.extend(drop_ins);
    }
    // Compatibility with the path used by the M2 task brief.
    out.push(base.join("abyss/config.kdl"));
    out
}

fn arg(node: &KdlNode) -> Option<&KdlValue> {
    node.entries()
        .iter()
        .find(|e| e.name().is_none())
        .map(|e| e.value())
}

fn args(node: &KdlNode) -> Vec<&KdlValue> {
    node.entries()
        .iter()
        .filter(|e| e.name().is_none())
        .map(|e| e.value())
        .collect()
}

fn set_i32(slot: &mut i32, node: &KdlNode) {
    match arg(node).and_then(KdlValue::as_integer) {
        Some(v) => *slot = v.clamp(0, 512) as i32,
        None => tracing::warn!(node = node.name().value(), "expected an integer"),
    }
}

/// `#rrggbb`, `#rrggbbaa`, `0xaarrggbb` (Hyprland's form) or `rrggbb`.
fn parse_color(s: &str) -> Option<[f32; 4]> {
    let t = s.trim();
    let (hex, argb) = if let Some(h) = t.strip_prefix('#') {
        (h, false)
    } else if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        (h, h.len() == 8)
    } else {
        (t, false)
    };
    let v = u32::from_str_radix(hex, 16).ok()?;
    let f = |b: u32| (b & 0xff) as f32 / 255.0;
    Some(match (hex.len(), argb) {
        (6, _) => [f(v >> 16), f(v >> 8), f(v), 1.0],
        (8, true) => [f(v >> 16), f(v >> 8), f(v), f(v >> 24)],
        (8, false) => [f(v >> 24), f(v >> 16), f(v >> 8), f(v)],
        _ => return None,
    })
}

fn parse_mods(s: &str) -> Result<Mods, String> {
    let mut mods = Mods::default();
    for part in s.split(['+', ' ', ',']).filter(|p| !p.is_empty()) {
        match part.to_ascii_uppercase().as_str() {
            "SUPER" | "MOD" | "LOGO" | "MOD4" => mods.logo = true,
            "SHIFT" => mods.shift = true,
            "CTRL" | "CONTROL" => mods.ctrl = true,
            "ALT" | "MOD1" => mods.alt = true,
            "NONE" | "" => {}
            other => return Err(format!("unknown modifier '{other}'")),
        }
    }
    Ok(mods)
}

fn parse_keysym(s: &str) -> Result<Keysym, String> {
    let k = xkb::keysym_from_name(s, xkb::KEYSYM_NO_FLAGS);
    let k = if k == Keysym::NoSymbol {
        xkb::keysym_from_name(s, xkb::KEYSYM_CASE_INSENSITIVE)
    } else {
        k
    };
    if k == Keysym::NoSymbol {
        Err(format!("unknown key '{s}'"))
    } else {
        Ok(k)
    }
}

/// `bind "SUPER" "Return" { spawn "kitty"; }`
fn parse_bind(node: &KdlNode) -> Result<Bind, String> {
    let a = args(node);
    let (mods, key) = match a.len() {
        1 => (Mods::default(), a[0].as_string().ok_or("key must be a string")?),
        2 => (
            parse_mods(a[0].as_string().ok_or("modifiers must be a string")?)?,
            a[1].as_string().ok_or("key must be a string")?,
        ),
        _ => return Err("bind takes [modifiers] key { action }".into()),
    };
    let key = parse_keysym(key)?;
    let children = node.children().ok_or("bind needs an action block")?;
    let action_node = children.nodes().first().ok_or("bind action block is empty")?;
    let action = parse_action(action_node)?;
    if mods == m(true, false, false, false) && key == Keysym::Escape {
        return Err("Super+Escape is reserved (COMP-04 §6) and cannot be bound".into());
    }
    if mods == m(true, false, false, false) && key == Keysym::space {
        return Err("Super+space is reserved (COMP-13 §1.1) and cannot be bound".into());
    }
    Ok(Bind { mods, key, action })
}

fn parse_action(node: &KdlNode) -> Result<Action, String> {
    let a = args(node);
    let text = || a.first().and_then(|v| v.as_string()).map(str::to_owned);
    let num = || a.first().and_then(|v| v.as_integer());
    Ok(match node.name().value() {
        "spawn" | "exec" => Action::Spawn(text().ok_or("spawn needs a command string")?),
        "close-window" | "killactive" => Action::Close,
        "toggle-floating" => Action::ToggleFloating,
        "toggle-layout" => Action::ToggleLayout,
        "focus-left" => Action::Focus(Direction::Left),
        "focus-right" => Action::Focus(Direction::Right),
        "focus-up" => Action::Focus(Direction::Up),
        "focus-down" => Action::Focus(Direction::Down),
        "move-left" => Action::Move(Direction::Left),
        "move-right" => Action::Move(Direction::Right),
        "move-up" => Action::Move(Direction::Up),
        "move-down" => Action::Move(Direction::Down),
        "workspace" => Action::SwitchWorkspace(workspace_arg(num())?),
        "move-to-workspace" => Action::MoveToWorkspace(workspace_arg(num())?),
        "agent-override" => Action::AgentOverride,
        "agent-attention" => Action::AgentAttention,
        "quit" | "exit" => Action::Quit,
        other => return Err(format!("unknown action '{other}'")),
    })
}

fn workspace_arg(n: Option<i128>) -> Result<usize, String> {
    match n {
        Some(v) if (1..=10).contains(&v) => Ok(v as usize),
        _ => Err("workspace number must be 1..=10".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_input() {
        let doc: KdlDocument = r#"
            input {
                kb-layout "de"
                kb-options "compose:ralt"
                repeat-rate 25
                repeat-delay 400
                accel-profile "flat"
                touchpad { natural-scroll #true; tap-to-click #true; dwt #false }
            }
        "#
        .parse()
        .unwrap();
        let mut cfg = Config::default();
        let mut binds = Vec::new();
        cfg.apply(&doc, &mut binds);
        assert_eq!(cfg.input.kb_layout, "de");
        assert_eq!(cfg.input.kb_options.as_deref(), Some("compose:ralt"));
        assert_eq!((cfg.input.repeat_rate, cfg.input.repeat_delay), (25, 400));
        assert_eq!(cfg.input.accel_profile, "flat");
        assert!(cfg.input.touchpad.natural_scroll && cfg.input.touchpad.tap_to_click);
        assert!(!cfg.input.touchpad.dwt);
        // The override chord is built in, never taken from the config.
        assert!(binds.is_empty());
        assert!(default_binds()
            .iter()
            .any(|b| b.key == Keysym::Escape && matches!(b.action, crate::input::Action::AgentOverride)));
        assert!(default_binds()
            .iter()
            .any(|b| b.key == Keysym::space && matches!(b.action, crate::input::Action::AgentAttention)));
    }

    #[test]
    fn parses_decoration_and_animations() {
        let doc: KdlDocument = r#"
            decoration {
                rounding 8
                active-opacity 1.0
                inactive-opacity 0.95
                dim-inactive 0.2
                blur { enabled #false; size 12; passes 3 }
                shadow { enabled #true; range 20 }
            }
            animations {
                enabled #true
                animation "windows" duration="150ms" curve="ease-out"
                animation "workspaces" duration=200 curve="linear"
            }
        "#
        .parse()
        .unwrap();
        let mut cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        assert_eq!(cfg.decoration.rounding, 8);
        assert_eq!(cfg.decoration.active_opacity, 1.0);
        assert_eq!(cfg.decoration.inactive_opacity, 0.95);
        assert_eq!(cfg.decoration.dim_inactive, 0.2);
        assert!(!cfg.decoration.blur.enabled);
        assert_eq!((cfg.decoration.blur.size, cfg.decoration.blur.passes), (12, 3));
        assert!(cfg.decoration.shadow.enabled && cfg.decoration.shadow.range == 20);
        assert!(cfg.decoration.any_window_effect());
        let w = cfg.animations.get("windows").expect("windows curve");
        assert_eq!((w.duration_ms, w.curve.as_str()), (150, "ease-out"));
        assert_eq!(cfg.animations.get("workspaces").unwrap().duration_ms, 200);
        assert!(cfg.animations.get("nope").is_none());
    }

    #[test]
    fn decoration_defaults_are_no_effect() {
        let cfg = Config::default();
        assert!(!cfg.decoration.any_window_effect());
        assert!(!cfg.decoration.blur.enabled && !cfg.decoration.shadow.enabled);
        assert_eq!(cfg.decoration.rounding, 0);
        // Animations off means no curve resolves even if one were parsed.
        assert!(!cfg.animations.enabled);
    }

    #[test]
    fn rejects_bad_decoration_and_animation_values() {
        let doc: KdlDocument = r#"
            decoration {
                rounding 999
                active-opacity 4.0
                inactive-opacity "half"
                nonsense 1
            }
            animations {
                enabled #true
                animation "windows" curve="bounce"
                animation "nope" duration="10ms"
                animation "fade" duration="2h"
            }
        "#
        .parse()
        .unwrap();
        let mut cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        // Every bad value keeps its default rather than half-applying.
        assert_eq!(cfg.decoration.rounding, 0);
        assert_eq!(cfg.decoration.active_opacity, 1.0);
        assert_eq!(cfg.decoration.inactive_opacity, 1.0);
        assert!(cfg.animations.curves.is_empty());
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration_ms(&KdlValue::Integer(150)), Some(150));
        assert_eq!(parse_duration_ms(&KdlValue::String("150ms".into())), Some(150));
        assert_eq!(parse_duration_ms(&KdlValue::String("2s".into())), Some(2000));
        assert_eq!(parse_duration_ms(&KdlValue::String("1m".into())), Some(60_000));
        assert_eq!(parse_duration_ms(&KdlValue::String("2h".into())), None);
        assert_eq!(parse_duration_ms(&KdlValue::Integer(-1)), None);
    }

    #[test]
    fn parses_windowrules() {
        let doc: KdlDocument = r#"
            windowrule "float" { app-id "pavucontrol|org.gnome.Calculator"; }
            windowrule "workspace 3" { title "^Meet"; output "DP-*"; }
            windowrule "opacity 0.85" { app-id "kitty"; xwayland #false; }
            windowrule "sensitivity secret" { app-id "keepassxc"; }
            windowrule "no-agent" { pid 42; }
        "#
        .parse()
        .unwrap();
        let mut c = Config::default();
        c.apply(&doc, &mut Vec::new());
        assert_eq!(c.window_rules.len(), 5);
        assert_eq!(c.window_rules[1].action, RuleAction::Workspace(3));
        assert_eq!(c.window_rules[2].action, RuleAction::Opacity(0.85));
        assert_eq!(c.window_rules[4].action, RuleAction::NoAgent);
        let m = &c.window_rules[0].matchers;
        let app = m.app_id.as_ref().expect("app-id");
        assert!(app.matches("pavucontrol"));
        assert!(app.matches("org.gnome.Calculator"));
        assert!(!app.matches("firefox"));
        assert_eq!(c.window_rules[2].matchers.xwayland, Some(false));
        assert_eq!(c.window_rules[4].matchers.pid, Some(42));
    }

    #[test]
    fn rejects_unhonourable_windowrules() {
        let doc: KdlDocument = r#"
            windowrule "float" { }
            windowrule "float"
            windowrule "size 800 600" { app-id "a"; }
            windowrule "sensitivity public" { app-id "a"; }
            windowrule "workspace 99" { app-id "a"; }
            windowrule "opacity 2.0" { app-id "a"; }
            windowrule "float" { cgroup "x"; }
            windowrule "float" { title "^(a|b"; }
            windowrule "float" { pid 0; }
        "#
        .parse()
        .unwrap();
        let mut c = Config::default();
        c.apply(&doc, &mut Vec::new());
        assert!(c.window_rules.is_empty(), "{:?}", c.window_rules);
    }

    #[test]
    fn patterns_are_regexes() {
        let p = Pattern::parse("Meet").expect("literal");
        assert!(p.matches("Google Meet — call"));
        let p = Pattern::parse("^Meet").expect("anchored");
        assert!(p.matches("Meet — call"));
        assert!(!p.matches("Google Meet"));
        let p = Pattern::parse("^kitty$").expect("exact");
        assert!(p.matches("kitty"));
        assert!(!p.matches("kitty-dev"));
        let p = Pattern::parse("^org\\.gnome\\.").expect("escaped");
        assert!(p.matches("org.gnome.Calculator"));
        assert!(!p.matches("org-gnome-Calculator"));
        // The full syntax, not the old glob subset.
        assert!(Pattern::parse("a[bc]d").expect("class").matches("abd"));
        assert!(Pattern::parse("^(chromium|firefox)$")
            .expect("group")
            .matches("firefox"));
        assert!(Pattern::parse("x+y").expect("repeat").matches("xxy"));
        // A malformed regex is refused, never approximated.
        assert!(Pattern::parse("a[bc").is_none());
        assert!(Pattern::parse("").is_none());
    }

    #[test]
    fn parses_render_device() {
        let doc: KdlDocument = r#"
            misc { render-device "pci:0000:01:00.0" }
        "#
        .parse()
        .unwrap();
        let mut c = Config::default();
        c.apply(&doc, &mut Vec::new());
        assert_eq!(c.misc.render_device.as_deref(), Some("pci:0000:01:00.0"));

        // "auto" is the explicit spelling of the default.
        let doc: KdlDocument = r#"misc { render-device "auto" }"#.parse().unwrap();
        let mut c = Config::default();
        c.misc.render_device = Some("/dev/dri/card9".into());
        c.apply(&doc, &mut Vec::new());
        assert_eq!(c.misc.render_device, None);
    }

    #[test]
    fn colors() {
        assert_eq!(parse_color("#ff0000"), Some([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(parse_color("0x80ff0000"), Some([1.0, 0.0, 0.0, 0.5019608]));
        assert_eq!(parse_color("nope"), None);
    }

    #[test]
    fn parses_idle_and_lid() {
        let doc: KdlDocument = r#"
            idle { dpms-timeout-seconds 300; lock-timeout-seconds 600; lock-command "hyprlock" }
            output "eDP-1" { lid-close "ignore" }
        "#
        .parse()
        .unwrap();
        let mut cfg = Config::default();
        let mut binds = Vec::new();
        cfg.apply(&doc, &mut binds);
        assert_eq!(cfg.idle.dpms_timeout, Some(300));
        assert_eq!(cfg.idle.lock_timeout, Some(600));
        assert_eq!(cfg.idle.lock_command.as_deref(), Some("hyprlock"));
        assert_eq!(
            cfg.output_rule("eDP-1", "eDP-1").lid_close.as_deref(),
            Some("ignore")
        );
    }

    #[test]
    fn parses_general_and_binds() {
        let doc: KdlDocument = r#"
            general { gaps-in 3; layout "master"; border-size 4 }
            bind "SUPER SHIFT" "Return" { spawn "foot"; }
            workspace 2 { layout "dwindle" }
        "#
        .parse()
        .unwrap();
        let mut cfg = Config::default();
        let mut binds = Vec::new();
        cfg.apply(&doc, &mut binds);
        assert_eq!(cfg.general.gaps_in, 3);
        assert_eq!(cfg.general.border_size, 4);
        assert_eq!(cfg.general.layout, LayoutKind::Master);
        assert_eq!(cfg.layout_for(2), LayoutKind::Dwindle);
        assert_eq!(binds.len(), 1);
        assert!(matches!(binds[0].action, Action::Spawn(ref c) if c == "foot"));
        assert!(binds[0].mods.shift && binds[0].mods.logo);
    }

    #[test]
    fn capture_defaults_to_denying_everything() {
        let cfg = Config::default();
        assert!(cfg.capture.allow.is_empty());
        assert!(cfg.capture.redact_app_id.is_empty());
    }

    #[test]
    fn parses_capture() {
        let doc: KdlDocument = r#"
            capture { allow "grim" "xdg-desktop-portal-wlr"; redact-app-id "bitwarden" }
        "#
        .parse()
        .unwrap();
        let mut cfg = Config::default();
        let mut binds = Vec::new();
        cfg.apply(&doc, &mut binds);
        assert_eq!(cfg.capture.allow, ["grim", "xdg-desktop-portal-wlr"]);
        assert_eq!(cfg.capture.redact_app_id, ["bitwarden"]);
    }

    #[test]
    fn bad_nodes_are_skipped_not_fatal() {
        let doc: KdlDocument = "general { layout \"bogus\" }\nbind \"SUPER\" \"Escape\" { quit; }\n"
            .parse()
            .unwrap();
        let mut cfg = Config::default();
        let mut binds = Vec::new();
        cfg.apply(&doc, &mut binds);
        assert_eq!(cfg.general.layout, LayoutKind::Dwindle);
        assert!(binds.is_empty(), "Super+Escape must stay reserved");
    }

    #[test]
    fn both_agent_chords_stay_reserved() {
        let doc: KdlDocument =
            "bind \"SUPER\" \"Escape\" { quit; }\nbind \"SUPER\" \"space\" { quit; }\nbind \"SUPER\" \"F1\" { quit; }\n"
                .parse()
                .unwrap();
        let mut cfg = Config::default();
        let mut binds = Vec::new();
        cfg.apply(&doc, &mut binds);
        assert_eq!(binds.len(), 1, "only the unreserved bind survives");
        assert_eq!(binds[0].key, Keysym::F1);
    }

    #[test]
    fn agent_attention_action_parses() {
        let doc: KdlDocument = "bind \"CTRL\" \"space\" { agent-attention; }".parse().unwrap();
        let mut cfg = Config::default();
        let mut binds = Vec::new();
        cfg.apply(&doc, &mut binds);
        assert!(matches!(binds[0].action, crate::input::Action::AgentAttention));
    }
}

/// A duration written either as a bare integer of milliseconds or as a string
/// with a unit: `"150ms"`, `"2s"`, `"1m"`. KDL 2.0 has no duration literal, so
/// the unit form has to be quoted.
fn parse_duration_ms(v: &KdlValue) -> Option<u32> {
    if let Some(i) = v.as_integer() {
        return u32::try_from(i).ok();
    }
    let s = v.as_string()?.trim();
    let (num, mult) = if let Some(n) = s.strip_suffix("ms") {
        (n, 1)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1000)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60_000)
    } else {
        (s, 1)
    };
    num.trim().parse::<u32>().ok()?.checked_mul(mult)
}

fn as_f64(v: &KdlValue) -> Option<f64> {
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

/// `*` matches any run of characters; everything else is literal. Anchored.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let mut parts = pattern.split('*');
    let Some(first) = parts.next() else {
        return pattern == text;
    };
    if !text.starts_with(first) {
        return false;
    }
    if !pattern.contains('*') {
        return text.len() == first.len();
    }
    let mut rest = &text[first.len()..];
    let parts: Vec<&str> = parts.collect();
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty() {
            continue;
        }
        if i + 1 == parts.len() && !pattern.ends_with('*') {
            return rest.ends_with(p) && rest.len() >= p.len();
        }
        match rest.find(p) {
            Some(at) => rest = &rest[at + p.len()..],
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod output_tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match("DP-1", "DP-1"));
        assert!(!glob_match("DP-1", "DP-11"));
        assert!(glob_match("eDP-*", "eDP-1"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*Dell*", "Dell U2720Q ABC"));
        assert!(!glob_match("*Dell*", "LG U2720Q"));
    }

    #[test]
    fn later_blocks_win() {
        let mut c = Config::default();
        let doc: KdlDocument = "output \"*\" { scale 1.0 }\noutput \"DP-*\" { scale 2.0; position 100 0 }"
            .parse()
            .unwrap();
        let mut binds = Vec::new();
        c.apply(&doc, &mut binds);
        let r = c.output_rule("DP-1", "Dell X Y");
        assert_eq!(r.scale, Some(2.0));
        assert_eq!(r.position, Some((100, 0)));
        assert_eq!(c.output_rule("HDMI-A-1", "x").scale, Some(1.0));
    }
}

#[cfg(test)]
mod allowlist_tests {
    use super::Allowlist;

    #[test]
    fn empty_denies_everything() {
        let a = Allowlist::default();
        assert!(a.is_empty());
        assert!(!a.contains("grim"));
    }

    #[test]
    fn set_is_visible_to_an_existing_handle() {
        // The point of the type: the copy held by a global's bind filter sees
        // what the config reload path wrote, without a restart.
        let held = Allowlist::new(vec!["grim".to_string()]);
        let reload = held.clone();
        assert!(held.contains("grim"));

        reload.set(vec!["wf-recorder".to_string()]);
        assert!(!held.contains("grim"));
        assert!(held.contains("wf-recorder"));

        // Reloading to nothing revokes rather than leaving the old list live.
        reload.set(Vec::new());
        assert!(held.is_empty());
        assert!(!held.contains("wf-recorder"));
    }
}
