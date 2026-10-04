// SPDX-License-Identifier: AGPL-3.0-only
//! KDL configuration (COMP-13 §1).
//!
//! Search path, later files overriding earlier ones:
//!   0. `/usr/share/eclipse/widgets/*.kdl`, the premade widget catalog, only
//!      `bar { widget … }` and only while `taskbar-widgets` is on (ADR 0067)
//!   1. `/etc/eclipse/abyss.kdl`
//!   2. `$XDG_CONFIG_HOME/eclipse/abyss.kdl`
//!   3. `$XDG_CONFIG_HOME/eclipse/abyss.d/*.kdl` (sorted)
//!   4. `$XDG_CONFIG_HOME/abyss/config.kdl` (compatibility fallback)
//!      `--config <path>` replaces the whole search path with that one file.
//!
//! Validation is total (COMP-13 §1.2): an unknown key or a malformed value is
//! an error, not a warning. Refusals are collected in [`Config::errors`] as
//! [`ConfigError`]s carrying `file:line:col` and the offending token; the
//! caller decides what to do with them. Startup (COMP-01 §5 step 3, amended by
//! ADR 0064) drops each rejected node, logs the refusal and starts, unless one
//! of them is [`ConfigError::startup_fatal`] — see [`Config::startup`].
//! Hot-reload (`ec-abyss`'s `config::watch::reload_now`) keeps the last good
//! config and emits a `config-error` IPC event; it never half-applies.
//!
//! This crate is the pure half: schema, parser, editor, approvals and widget
//! hashes. It has no smithay and no compositor state, so `ec-settings` and
//! `ec-ctl` can link it. Applying a loaded config (`apply_loaded`), the widget
//! catalog, the file watcher and the approval withholding stay in `ec-abyss`'s
//! `config/`, which re-exports this crate as `crate::config`.

pub mod approvals;
pub mod catalog;
pub mod edit;
pub mod input;
pub mod outputs;
pub mod schema;
pub mod trust;
pub mod widget_hash;

use std::path::{Path, PathBuf};

use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};
use xkbcommon::xkb::{self, Keysym};

use crate::input::{
    Action, Bind, Direction, DragGesture, GestureBind, Mods, MouseAction, MouseBind, MouseButton,
};
use crate::trust::{AppTrust, SeatCompat};

/// Where a floating window lands when nothing else decides for it — no
/// `position` window rule, no remembered rectangle (COMP-05 §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatingPlacement {
    /// Centred in the output's tiling area.
    Centered,
    /// Top-left corner at the pointer, clamped to stay fully on the output.
    Pointer,
    /// Stepped down-right from the area's origin, one step per window already
    /// floating here, wrapping before it runs off the edge.
    Cascade,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutKind {
    /// Weighted n-ary tree: dwindle-style auto insert, drag-to-tile drop
    /// zones and per-window priority (COMP-05 §3.1). The default.
    Radiant,
    /// Classic dwindle over the in-order window sequence ("Dwindle Classic").
    Dwindle,
    Master,
}

#[derive(Debug, Clone)]
pub struct General {
    pub gaps_in: i32,
    pub gaps_out: i32,
    /// Vertical (stacked-split) inner gap, logical px. `None` mirrors
    /// [`General::gaps_in`] — the config never set `gaps-in-vertical`, so
    /// resolve through [`General::gaps_in_y`] rather than this field
    /// directly.
    pub gaps_in_vertical: Option<i32>,
    /// Vertical outer gap, logical px. `None` mirrors [`General::gaps_out`];
    /// resolve through [`General::gaps_out_y`].
    pub gaps_out_vertical: Option<i32>,
    pub border_size: i32,
    pub layout: LayoutKind,
    pub floating_placement: FloatingPlacement,
    pub focus_follows_mouse: bool,
    pub focus_follows_mouse_across_outputs: bool,
    pub unfocus_on_empty_workspace: bool,
    pub refocus_on_scene_change: bool,
    /// Warp the pointer onto a window a keybind just moved, so the cursor is
    /// never left behind on the workspace or display the window came from.
    pub cursor_follows_moved_window: bool,
    /// Follow a window sent to another workspace or display, rather than
    /// staying put and watching it leave.
    pub follow_window_to_workspace: bool,
    pub col_active: [f32; 4],
    pub col_inactive: [f32; 4],
    /// Draw the section outlines, edge bands and landing ghost while a tiled
    /// window is dragged (Radiant only).
    pub drop_guides: bool,
    pub drop_guide_color: [f32; 4],
    /// Width, logical px, of the screen-edge band that adds a full-height
    /// column or full-width row on drop.
    pub drop_edge_band: i32,
}

impl General {
    /// The vertical inner gap, resolving `gaps-in-vertical` against
    /// `gaps-in` (COMP-05 §3) when the config never set it — so a config that
    /// only ever touches `gaps-in` renders identically whether or not the
    /// vertical key exists.
    pub fn gaps_in_y(&self) -> i32 {
        self.gaps_in_vertical.unwrap_or(self.gaps_in)
    }

    /// The vertical outer gap, resolving `gaps-out-vertical` against
    /// `gaps-out` the same way.
    pub fn gaps_out_y(&self) -> i32 {
        self.gaps_out_vertical.unwrap_or(self.gaps_out)
    }
}

impl Default for General {
    fn default() -> Self {
        Self {
            gaps_in: 5,
            gaps_out: 3,
            gaps_in_vertical: Some(3),
            gaps_out_vertical: Some(7),
            border_size: 1,
            layout: LayoutKind::Radiant,
            floating_placement: FloatingPlacement::Centered,
            focus_follows_mouse: true,
            focus_follows_mouse_across_outputs: true,
            unfocus_on_empty_workspace: true,
            refocus_on_scene_change: true,
            cursor_follows_moved_window: true,
            follow_window_to_workspace: true,
            // #f2c33c73 / #ffffff1a: a translucent accent and a faint white
            // hairline, which glass mode turns into the bezel (C-15, C-16)
            col_active: [242.0 / 255.0, 195.0 / 255.0, 60.0 / 255.0, 115.0 / 255.0],
            col_inactive: [1.0, 1.0, 1.0, 26.0 / 255.0],
            drop_guides: true,
            drop_guide_color: [0.91, 0.64, 0.24, 1.0],
            drop_edge_band: 40,
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
    /// `(exe basename, layer-shell namespace)` pairs whose layer surfaces are
    /// omitted from every capture: shown on screen, absent from screenshots
    /// and screen shares. Only surfaces up to 64x64 logical px qualify
    /// (ADR 0056); that bound is enforced by the consumer.
    pub hide_layer: Vec<(String, String)>,
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
    /// `terminal-command`: the terminal emulator used to launch
    /// `Terminal=true` `.desktop` entries (`$term -e <argv>`). Unset (the
    /// default) means those entries are dropped from the app index rather
    /// than shown and then refused (TERM-01).
    pub terminal_command: Option<String>,
}

/// `mode` (COMP-17 §2, ADR 0062).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Wm,
    Hybrid,
    De,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Wm => "wm",
            Mode::Hybrid => "hybrid",
            Mode::De => "de",
        }
    }
}

/// `components { ... }` (COMP-17 §2.2): candidate ids, validated against the
/// `schema::COMPONENT_*` catalogs. Defaults are Standard's values; profiles
/// are a one-time seed and are never read back (COMP-17 §2.1).
#[derive(Debug, Clone)]
pub struct Components {
    pub bar: String,
    pub launcher: String,
    pub notifications: String,
    pub control_center: String,
}

impl Default for Components {
    fn default() -> Self {
        Self {
            bar: "ec-hyperion-bar".into(),
            launcher: "ec-launcher".into(),
            notifications: "ec-toasts".into(),
            control_center: "ec-center".into(),
        }
    }
}

/// `wallpaper { ... }`. Read over the socket by the `ec-wallpaper`
/// daemon; abyss draws nothing from it. `path` is not checked for existence:
/// a missing file is the daemon's to fall back from.
#[derive(Debug, Clone)]
pub struct Wallpaper {
    pub path: Option<String>,
    /// One of [`schema::WALLPAPER_MODES`].
    pub mode: String,
    pub color: [f32; 4],
    /// `output "<name>" { .. }` children in file order, one per name: a later
    /// block for the same name overrides an earlier one key by key.
    pub outputs: Vec<WallpaperOutput>,
}

impl Default for Wallpaper {
    fn default() -> Self {
        Self {
            path: None,
            mode: "fill".into(),
            color: schema::WALLPAPER_DEFAULT_COLOR,
            outputs: Vec::new(),
        }
    }
}

/// One per-output override inside `wallpaper`. Unset keys inherit the
/// global ones; the daemon does the inheriting.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WallpaperOutput {
    /// Connector name, as written; abyss does not match it against anything.
    pub name: String,
    pub path: Option<String>,
    pub mode: Option<String>,
    pub color: Option<[f32; 4]>,
}

/// `#rrggbbaa` as straight-alpha floats, for a `const` default.
const fn rgba8(r: u8, g: u8, b: u8, a: u8) -> [f32; 4] {
    [
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    ]
}

/// Lowest `annotations.panel-tint` alpha. A lighter tint lets a white page
/// show through the panel and the white body text on it stops being legible,
/// so a lower alpha is raised to this rather than refused.
pub const PANEL_TINT_MIN_ALPHA: f32 = 0.5;

/// `annotations { … }`: every colour the compositor's annotation HUD and
/// region selector draw with (COMP-18 §1.3; owner decision 6, "every Oracle
/// Eyes colour is user-settable"). The compositor owns them; the add-on
/// supplies only text and geometry and cannot restyle anything. Straight
/// alpha, each written `#rrggbb` or `#rrggbbaa`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnnotationColors {
    /// The pick tab, the pick rim, the selector band and the live taskbar eye.
    pub accent: [f32; 4],
    /// The pick letter inside the panel.
    pub accent_text: [f32; 4],
    /// Title and body text.
    pub text: [f32; 4],
    /// Panel fill over the blurred backdrop. Alpha never below
    /// [`PANEL_TINT_MIN_ALPHA`].
    pub panel_tint: [f32; 4],
    /// The panel's 1px rim.
    pub panel_rim: [f32; 4],
    /// The rule between title and body.
    pub hairline: [f32; 4],
    /// The rounded rim around the region an annotation is about.
    pub region_outline: [f32; 4],
    /// The line from the panel to its region.
    pub leader: [f32; 4],
    /// The dim over the screen outside a region selection.
    pub selection_dim: [f32; 4],
    /// The error dot of an `error` annotation.
    pub danger: [f32; 4],
}

impl AnnotationColors {
    pub const DEFAULT: AnnotationColors = AnnotationColors {
        accent: rgba8(0xf2, 0xc3, 0x3c, 0xff),
        accent_text: rgba8(0xf5, 0xcf, 0x5c, 0xff),
        text: rgba8(0xff, 0xff, 0xff, 0xff),
        panel_tint: rgba8(0x17, 0x14, 0x0f, 0xb8),
        panel_rim: rgba8(0xff, 0xff, 0xff, 0x1f),
        hairline: rgba8(0xff, 0xff, 0xff, 0x0f),
        region_outline: rgba8(0xff, 0xff, 0xff, 0x38),
        leader: rgba8(0xff, 0xff, 0xff, 0x47),
        selection_dim: rgba8(0x0b, 0x09, 0x06, 0x66),
        danger: rgba8(0xe0, 0x55, 0x3f, 0xff),
    };

    /// The field a key inside `annotations { }` names, or `None` for an
    /// unknown key.
    fn slot_mut(&mut self, key: &str) -> Option<&mut [f32; 4]> {
        Some(match key {
            "accent" => &mut self.accent,
            "accent-text" => &mut self.accent_text,
            "text" => &mut self.text,
            "panel-tint" => &mut self.panel_tint,
            "panel-rim" => &mut self.panel_rim,
            "hairline" => &mut self.hairline,
            "region-outline" => &mut self.region_outline,
            "leader" => &mut self.leader,
            "selection-dim" => &mut self.selection_dim,
            "danger" => &mut self.danger,
            _ => return None,
        })
    }
}

impl Default for AnnotationColors {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// `oracle-eyes.model-command` default: the program alone. The daemon always
/// appends its locked flags itself (oracle-eyes spec §3.4: tools disabled,
/// one turn, the answer schema), so nothing here can turn them off. Approved
/// by virtue of shipping; any other command waits for the owner (ADR 0067's
/// withhold path, `ec-abyss`'s `config::withhold`).
pub const ORACLE_EYES_MODEL_COMMAND: &[&str] = &["claude"];

/// `oracle-eyes { … }`: the Oracle Eyes add-on's settings (ADR 0066). They live
/// in `abyss.kdl` so Settings can edit them (COMP-17 §3); abyss only stores
/// and serves them over `get_config`, and the daemon reads them from there.
#[derive(Debug, Clone, PartialEq)]
pub struct OracleEyes {
    /// The model command, argv. Always `Some` from the parser; `None` on the
    /// live config only while the command is withheld awaiting the owner's
    /// approval, so `get_config` never serves an unapproved command.
    pub model_command: Option<Vec<String>>,
    /// Ceiling on one model call.
    pub timeout_ms: u32,
    /// Automatic mode: least time between two queries.
    pub auto_interval_ms: u32,
    /// Least time an answer stays on screen.
    pub hold_ms: u32,
    /// Debug mode: the taskbar eye turns red and shows in captures.
    pub debug: bool,
    /// `oracle-eyes { bind { … } }`: chords for the annotation actions.
    pub bind: OracleEyesBinds,
}

/// `oracle-eyes { bind { select "Super+A"; dismiss …; expand …; auto-toggle … } }`.
/// Same grammar as `launcher.bind`; each one is a default bind for its
/// `annotation-*` action, so a `bind` block on the same chord wins. None is
/// bound unless set (`none`).
#[derive(Debug, Clone, PartialEq)]
pub struct OracleEyesBinds {
    pub select: LauncherChord,
    pub dismiss: LauncherChord,
    pub expand: LauncherChord,
    pub auto_toggle: LauncherChord,
}

impl Default for OracleEyesBinds {
    fn default() -> Self {
        Self {
            select: LauncherChord::new("none"),
            dismiss: LauncherChord::new("none"),
            expand: LauncherChord::new("none"),
            auto_toggle: LauncherChord::new("none"),
        }
    }
}

/// The `oracle-eyes.bind.*` chords as default binds for their actions.
pub fn oracle_eyes_binds(oe: &OracleEyes) -> Vec<Bind> {
    let b = &oe.bind;
    [
        (&b.select, Action::AnnotationSelect),
        (&b.dismiss, Action::AnnotationDismiss),
        (&b.expand, Action::AnnotationExpand),
        (&b.auto_toggle, Action::AnnotationAutoToggle),
    ]
    .into_iter()
    .filter_map(|(c, action)| c.chord.map(|(mods, key)| Bind { mods, key, action }))
    .collect()
}

/// `oracle-eyes.*` integer ranges: `(min, max)`, the same in the schema
/// and the parser.
pub const ORACLE_EYES_TIMEOUT_MS: (u32, u32) = (1000, 300_000);
pub const ORACLE_EYES_AUTO_INTERVAL_MS: (u32, u32) = (1000, 600_000);
pub const ORACLE_EYES_HOLD_MS: (u32, u32) = (500, 60_000);

impl Default for OracleEyes {
    fn default() -> Self {
        Self {
            model_command: Some(
                ORACLE_EYES_MODEL_COMMAND
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect(),
            ),
            timeout_ms: 30_000,
            auto_interval_ms: 3000,
            hold_ms: 4000,
            debug: false,
            bind: OracleEyesBinds::default(),
        }
    }
}

/// One validated key of a `wallpaper` block or its `output` child.
enum WallpaperKey {
    Path(String),
    Mode(String),
    Color([f32; 4]),
}

/// `setup { ... }` (D-07 §4, COMP-17 §2.1). Written by `ec-setup` through
/// COMP-13 §1.4 and read by nothing at runtime: a record, not a layer.
#[derive(Debug, Clone)]
pub struct Setup {
    /// `profile`: one of [`schema::SETUP_PROFILES`]. Reference only.
    pub profile: String,
    /// `complete`: setup has applied. `ec-ctl setup reset` clears it.
    pub complete: bool,
    /// `pending-preset`: the Agentic policy preset was chosen and awaits
    /// loading on the installed system (D-07 §4.5).
    pub pending_preset: bool,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            profile: "standard".into(),
            complete: false,
            pending_preset: false,
        }
    }
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
    /// Per-edge inset in physical pixels for a panel that crops the signal
    /// (COMP-03 §2). Hand-written here wins over anything calibration saved.
    pub overscan: Option<crate::outputs::Overscan>,
    /// `off` | `suspend` | `ignore` — what a lid-close does to this output
    /// (COMP-01 §4.1). Only meaningful on an internal panel.
    pub lid_close: Option<String>,
    /// Explicit display number override (ADR 0049). Falls back to connection
    /// order when unset. Display-facing only — never derived from or fed
    /// back into identity (ADR 0023).
    pub number: Option<u8>,
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

/// `bar { ... }` (COMP-13 §1.1). The compositor does not draw the taskbar; it
/// owns the setting so one config file describes the whole desktop and the
/// bar reads it back over the control socket.
#[derive(Debug, Clone)]
pub struct Bar {
    /// Shrink the taskbar to a thin strip on outputs the pointer is not on.
    pub fold_when_inactive: bool,
    /// Height in logical px of that folded strip.
    pub fold_height: u32,
    /// Corner radius in logical px for the taskbar's own blur backdrop. The
    /// bar draws a pill at a different radius than every other pane's glass
    /// content, so it gets its own key instead of sharing `decoration.rounding`.
    pub rounding: u32,
    /// Also fold once the session has been idle for [`Self::idle_seconds`].
    /// Independent of [`Self::fold_when_inactive`]: both may hold at once, and
    /// either one folds.
    pub fold_when_idle: bool,
    /// Seconds without any human input that count as idle, for the bar alone.
    /// The `idle` block's own timeouts are unrelated and far longer.
    pub idle_seconds: u32,
    /// How long the fold slide takes. Height and exclusive zone animate together
    /// so tiled windows reflow with the bar; zero snaps.
    pub fold_duration_ms: u32,
    /// Easing for that slide, one of `ANIMATION_CURVES`.
    pub fold_curve: String,
    /// Which edge of every output the bar is anchored to. The layer surface's
    /// anchor is chosen once, at surface creation (COMP-13 §1.1's `restart`
    /// reload class), so a running bar keeps its old edge until relaunched.
    pub position: BarPosition,
    /// `tray { ... }`: which applets and StatusNotifierItems the bar shows.
    pub tray: BarTray,
    /// `clock { ... }`: how the bar's clock cell formats time and date.
    pub clock: BarClock,
    /// Where the bar's popups open: under the cell that was clicked, or at
    /// the pointer.
    pub popup_anchor: BarPopupAnchor,
    /// `eye`: whether the taskbar draws its status eye on the eclipse mark.
    /// Stored only; the taskbar sources the eye's state itself (ADR 0055).
    pub eye: bool,
    /// `widgets { ... }`: which widgets the bar draws, in what order (ADR 0065).
    pub widgets: BarWidgets,
    /// `motion { ... }`: how the bar's chips and widgets animate.
    pub motion: BarMotion,
    /// `widget "<name>" { ... }` blocks, in file order; a later block with
    /// the same name replaces the earlier one in place.
    pub custom_widgets: Vec<CustomWidget>,
}

/// `bar { clock { hour-12 …; date-mdy … } }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarClock {
    /// 12-hour clock with AM/PM; false is 24-hour.
    pub hour_12: bool,
    /// Month/day/year date order; false is day/month/year.
    pub date_mdy: bool,
}

impl Default for BarClock {
    fn default() -> Self {
        Self {
            hour_12: true,
            date_mdy: true,
        }
    }
}

/// `bar { popup-anchor "cell" | "pointer" }`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BarPopupAnchor {
    #[default]
    Cell,
    Pointer,
}

/// `launcher { style "centered" | "menu" }`. The deprecated
/// `bar { launcher-style … }` still sets it (see `apply_bar`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LauncherStyle {
    #[default]
    Centered,
    Menu,
}

impl LauncherStyle {
    /// The KDL spelling.
    pub fn name(self) -> &'static str {
        match self {
            LauncherStyle::Centered => "centered",
            LauncherStyle::Menu => "menu",
        }
    }
}

/// `launcher { centered { anchor … } }`: where the centred launcher sits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LauncherAnchor {
    #[default]
    Center,
    Top,
    Bottom,
}

impl LauncherAnchor {
    /// The KDL spelling, one of [`schema::LAUNCHER_ANCHORS`].
    pub fn name(self) -> &'static str {
        match self {
            LauncherAnchor::Center => "center",
            LauncherAnchor::Top => "top",
            LauncherAnchor::Bottom => "bottom",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "center" => Some(LauncherAnchor::Center),
            "top" => Some(LauncherAnchor::Top),
            "bottom" => Some(LauncherAnchor::Bottom),
            _ => None,
        }
    }
}

/// `launcher { … }` (COMP-13). The compositor stores these; the
/// launcher (`ec-launcher`) and the taskbar's start menu read them over
/// `get_config`. The two `bind` chords are the exception: abyss turns them
/// into key bindings itself (`defaults_with_launcher`).
#[derive(Debug, Clone, Default)]
pub struct Launcher {
    /// What Super+R and the eclipse button open.
    pub style: LauncherStyle,
    /// `centered { … }`: the standalone sheet.
    pub centered: LauncherCentered,
    /// `menu { … }`: the taskbar's start menu.
    pub menu: LauncherMenu,
    /// `search { … }`: what both launchers index and match.
    pub search: LauncherSearch,
    /// `bind { open …; run … }`: the chords that start the launcher.
    pub bind: LauncherBinds,
}

/// `launcher { centered { width …; max-rows …; anchor … } }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LauncherCentered {
    /// Logical px.
    pub width: u32,
    pub max_rows: u32,
    pub anchor: LauncherAnchor,
}

impl Default for LauncherCentered {
    fn default() -> Self {
        Self {
            width: 540,
            max_rows: 6,
            anchor: LauncherAnchor::Center,
        }
    }
}

/// `launcher { menu { max-rows … } }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LauncherMenu {
    pub max_rows: u32,
}

impl Default for LauncherMenu {
    fn default() -> Self {
        Self { max_rows: 8 }
    }
}

/// `launcher { search { … } }`, shared by both launcher styles: both search
/// through `ec_services::apps`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LauncherSearch {
    /// Offer executables on `$PATH` as well as desktop entries.
    pub path_binaries: bool,
    /// List `Terminal=true` entries. Only takes effect with
    /// `misc.terminal-command` set; without a terminal they are always left out.
    pub terminal_apps: bool,
    /// Match the query against an entry's `Comment` too.
    pub match_descriptions: bool,
    /// Rank applications the human launches often first, and remember
    /// launches (a count and a time per desktop id, never the query).
    pub frecency: bool,
}

impl Default for LauncherSearch {
    fn default() -> Self {
        Self {
            path_binaries: false,
            terminal_apps: true,
            match_descriptions: false,
            frecency: true,
        }
    }
}

/// One `launcher { bind { … } }` chord: the text as written, which is what
/// `get_config` reads back, and the binding it parsed to. `None` is `"none"`:
/// no chord.
#[derive(Debug, Clone, PartialEq)]
pub struct LauncherChord {
    pub text: String,
    pub chord: Option<(Mods, Keysym)>,
}

impl LauncherChord {
    fn new(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            chord: parse_chord(text).expect("built-in chord parses"),
        }
    }
}

/// `launcher { bind { open "Super+E"; run "Super+R" } }`.
#[derive(Debug, Clone, PartialEq)]
pub struct LauncherBinds {
    pub open: LauncherChord,
    pub run: LauncherChord,
}

impl Default for LauncherBinds {
    fn default() -> Self {
        Self {
            open: LauncherChord::new("Super+E"),
            run: LauncherChord::new("Super+R"),
        }
    }
}

/// `ui { … }`: preferences every DE pane shares. Stored only; each pane
/// reads them over `get_config`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ui {
    /// Show the keyboard hint line in the launcher and the start menu.
    pub show_key_hints: bool,
}

impl Default for Ui {
    fn default() -> Self {
        Self { show_key_hints: true }
    }
}

/// `settings { search { frecency } }`: the settings app's own preferences.
/// Stored only; the app reads them over `get_config`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsApp {
    /// Nudge search results the human has opened before up among near ties,
    /// and remember which results were opened (a path each, never the query).
    pub search_frecency: bool,
}

impl Default for SettingsApp {
    fn default() -> Self {
        Self {
            search_frecency: true,
        }
    }
}

/// `bar { tray { pinned …; hidden … } }`. Ids only — the compositor neither
/// knows nor checks which applets or tray items exist; the bar resolves them.
/// Built-in applet ids are `network`, `bluetooth`, `battery`, `volume`; a
/// StatusNotifierItem is named by its own `Id` property.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BarTray {
    /// Ids on the bar itself, in display order. `None` (no `pinned` node)
    /// leaves the order to the bar; `Some(vec![])` pins nothing.
    pub pinned: Option<Vec<String>>,
    /// Ids shown nowhere. Wins over `pinned`. Anything in neither list goes
    /// to the overflow drawer.
    pub hidden: Vec<String>,
}

/// The edge `hyperion`'s layer surface anchors to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarPosition {
    Top,
    Bottom,
}

/// `bar { widgets { … } }` (ADR 0065). Ids are checked here, against the
/// built-in list and the `widget` blocks; what each widget draws is the bar's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BarWidgets {
    /// Widgets after the task strip, left to right. Built-in ids and
    /// `custom:<name>`; each at most once.
    pub order: Vec<String>,
    /// Widgets that never compress.
    pub important: Vec<String>,
    pub now_playing: NowPlayingWidget,
    pub system_usage: SystemUsageWidget,
    pub volume: VolumeWidget,
}

impl Default for BarWidgets {
    fn default() -> Self {
        let own = |l: &[&str]| l.iter().map(|s| s.to_string()).collect();
        Self {
            order: own(schema::BAR_WIDGET_DEFAULT_ORDER),
            important: own(schema::BAR_WIDGET_DEFAULT_IMPORTANT),
            now_playing: NowPlayingWidget::default(),
            system_usage: SystemUsageWidget::default(),
            volume: VolumeWidget::default(),
        }
    }
}

/// `bar { widgets { now-playing { … } } }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NowPlayingWidget {
    pub art: bool,
    /// Whether the monitor tap may open at all (ADR 0065).
    pub visualizer: bool,
    /// Whether `https` cover art is fetched via `curl` (ADR 0065).
    pub remote_art: bool,
}

impl Default for NowPlayingWidget {
    fn default() -> Self {
        Self {
            art: true,
            visualizer: true,
            remote_art: true,
        }
    }
}

/// `bar { widgets { system-usage { … } } }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemUsageWidget {
    pub interval_ms: u32,
    pub gpu: bool,
    pub disk: bool,
    /// Absolute path; checked at parse time.
    pub disk_path: String,
}

impl Default for SystemUsageWidget {
    fn default() -> Self {
        Self {
            interval_ms: 1000,
            gpu: true,
            disk: true,
            disk_path: "/".to_owned(),
        }
    }
}

/// `bar { widgets { volume { … } } }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeWidget {
    pub step: u32,
    pub scroll: bool,
    pub max_percent: u32,
}

impl Default for VolumeWidget {
    fn default() -> Self {
        Self {
            step: 5,
            scroll: true,
            max_percent: 100,
        }
    }
}

/// `bar { motion { … } }` (ADR 0065): how chips and widgets move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BarMotion {
    pub enabled: bool,
    pub duration_ms: u32,
    /// One of `schema::BAR_MOTION_CURVES`.
    pub curve: String,
}

impl Default for BarMotion {
    fn default() -> Self {
        Self {
            enabled: true,
            duration_ms: 220,
            curve: "spring".to_owned(),
        }
    }
}

/// `bar { widget "<name>" { … } }` (ADR 0065). Stored and handed out through
/// `get_config`; the compositor never runs these. The taskbar's
/// `ec-services::custom` runner does, as the human.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomWidget {
    pub name: String,
    pub kind: CustomWidgetKind,
    pub icon: Option<String>,
    pub on_click: Option<Vec<String>>,
    pub on_scroll_up: Option<Vec<String>>,
    pub on_scroll_down: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomWidgetKind {
    /// `exec …` run every `interval_ms`.
    Exec { argv: Vec<String>, interval_ms: u32 },
    /// `exec …` with `stream #true`: run once, one update per line.
    Stream { argv: Vec<String> },
    /// `source …` rendered through `format`.
    Source { source: String, format: String },
}

/// A `custom:<name>` id seen in `bar.widgets.*`, checked against the `widget`
/// blocks once the whole file is read, so a block may follow its use.
#[derive(Debug, Clone)]
struct PendingCustom {
    name: String,
    offset: usize,
    len: usize,
}

impl Default for Bar {
    fn default() -> Self {
        Self {
            fold_when_inactive: false,
            fold_height: 4,
            rounding: 20,
            fold_when_idle: false,
            idle_seconds: 30,
            fold_duration_ms: 150,
            fold_curve: "ease-out".to_owned(),
            position: BarPosition::Top,
            tray: BarTray::default(),
            clock: BarClock::default(),
            popup_anchor: BarPopupAnchor::Cell,
            eye: true,
            widgets: BarWidgets::default(),
            motion: BarMotion::default(),
            custom_widgets: Vec::new(),
        }
    }
}

/// `decoration { ... }` (COMP-13 §1.1, COMP-02 §9). Every effect here is off by
/// default: the defaults below are the "no effect" values, so a config without a
/// `decoration` block renders exactly as it did before milestone 9b and keeps
/// direct scanout available. Opacity and `dim-inactive` are rendered today;
/// `shadow` and `glow` are one SDF pixel shader over the grown window rect and
/// `blur` a dual-Kawase chain behind translucent windows (COMP-02 §9); all draw
/// today.
/// `rounding` is drawn as a fragment-shader mask.
#[derive(Debug, Clone)]
pub struct Decoration {
    /// Corner radius in logical pixels; 0 disables. Matches ec-ui's
    /// `tokens::radius::CARD` (13px) so the compositor-drawn blur backdrop
    /// lines up with the client-drawn glass content on top of it.
    pub rounding: i32,
    /// Alpha applied to the focused window, 0.0..=1.0.
    pub active_opacity: f32,
    /// Alpha applied to every unfocused window, 0.0..=1.0.
    pub inactive_opacity: f32,
    /// Strength of the darkening overlay on unfocused windows, 0.0..=1.0.
    pub dim_inactive: f32,
    pub blur: Blur,
    pub shadow: Shadow,
    pub glow: Glow,
}

impl Default for Decoration {
    fn default() -> Self {
        Self {
            rounding: 9,
            active_opacity: 1.0,
            inactive_opacity: 1.0,
            dim_inactive: 0.0,
            blur: Blur::default(),
            shadow: Shadow::default(),
            glow: Glow::default(),
        }
    }
}

impl Decoration {
    /// Whether any per-window effect diverges from the plain path. When this is
    /// false the renderer keeps the single `space_render_elements` call, so
    /// damage tracking and direct scanout behave as they do with no config.
    pub fn any_window_effect(&self) -> bool {
        self.rounding > 0
            || self.active_opacity < 1.0
            || self.inactive_opacity < 1.0
            || self.dim_inactive > 0.0
            || self.glow.on()
    }
}

/// What is drawn behind a translucent surface (COMP-02 §9). Every mode but
/// `Off` runs the same dual-Kawase chain; `Frost` and `Glass` only swap the
/// program the blurred backdrop is finally drawn with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BlurMode {
    /// No backdrop pass at all.
    Off,
    /// The plain blurred backdrop, masked to the surface's corners.
    #[default]
    Blur,
    /// Blur with a tint mixed in and a fine grain over it.
    Frost,
    /// A saturated, smoked blur with a gentle bevel roll-off at the edge and
    /// a neutral hairline rim.
    Glass,
}

impl BlurMode {
    /// Every mode, in the order a GUI shows them.
    pub const NAMES: &'static [&'static str] = &["off", "blur", "frost", "glass"];

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "off" => Self::Off,
            "blur" => Self::Blur,
            "frost" => Self::Frost,
            "glass" => Self::Glass,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Blur => "blur",
            Self::Frost => "frost",
            Self::Glass => "glass",
        }
    }
}

/// `blur { mode "blur"; size 8; passes 4; glass { … }; frost { … } }`.
/// Dual-Kawase (COMP-02 §9). The legacy `enabled #true|#false` still parses
/// (as `mode "blur"` / `mode "off"`); `ec-ctl config migrate` rewrites it.
#[derive(Debug, Clone)]
pub struct Blur {
    pub mode: BlurMode,
    pub size: i32,
    pub passes: i32,
    pub glass: GlassBlur,
    pub frost: FrostBlur,
}

impl Default for Blur {
    fn default() -> Self {
        Self {
            mode: BlurMode::Glass,
            size: 8,
            passes: 4,
            glass: GlassBlur::default(),
            frost: FrostBlur::default(),
        }
    }
}

/// `blur { glass { refraction 4; bevel 16; dispersion 0; rim 0.5 } }`.
#[derive(Debug, Clone, PartialEq)]
pub struct GlassBlur {
    /// Largest displacement of the blurred backdrop, logical px, at the
    /// outermost edge of the bevel; the flat centre is never bent.
    pub refraction: i32,
    /// Width of the edge band the backdrop rolls off in, logical px.
    pub bevel: i32,
    /// Chromatic spread in the bevel, 0.0..=1.0; off by default.
    pub dispersion: f32,
    /// Strength of the neutral two-lobe hairline rim, 0.0..=1.0.
    pub rim: f32,
}

impl Default for GlassBlur {
    fn default() -> Self {
        Self {
            refraction: 4,
            bevel: 16,
            dispersion: 0.0,
            rim: 0.5,
        }
    }
}

/// Default `frost.tint`: STYLE.md's `#1a1712` surface at 40%, straight alpha.
pub const FROST_TINT: [f32; 4] = [0.102, 0.091, 0.071, 0.4];

/// `blur { frost { tint "#1a171266" } }`.
#[derive(Debug, Clone, PartialEq)]
pub struct FrostBlur {
    /// Mixed over the blurred backdrop; its alpha is the mix amount.
    pub tint: [f32; 4],
}

impl Default for FrostBlur {
    fn default() -> Self {
        Self { tint: FROST_TINT }
    }
}

/// `shadow { enabled #false; range 20 }`. An SDF ring outside the window, drawn
/// by a pixel shader (COMP-02 §9).
#[derive(Debug, Clone)]
pub struct Shadow {
    pub enabled: bool,
    pub range: i32,
}

impl Default for Shadow {
    fn default() -> Self {
        Self {
            enabled: true,
            range: 16,
        }
    }
}

/// `glow { enabled #false; active #true; inactive #true; strength 60 }`. The
/// shadow's SDF ring, tinted with the window's border colour (COMP-02 §9).
#[derive(Debug, Clone)]
pub struct Glow {
    pub enabled: bool,
    /// Glow on the focused window.
    pub active: bool,
    /// Glow on unfocused windows.
    pub inactive: bool,
    /// Peak intensity, percent 0..=100.
    pub strength: i32,
}

impl Default for Glow {
    fn default() -> Self {
        Self {
            enabled: false,
            active: true,
            inactive: true,
            strength: 60,
        }
    }
}

impl Glow {
    /// Whether any window can glow at all.
    pub fn on(&self) -> bool {
        self.enabled && self.strength > 0 && (self.active || self.inactive)
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
    /// Regex against the client's cgroup path, read once from
    /// `/proc/<pid>/cgroup`. This is how a rule targets "everything systemd
    /// started under this unit" without knowing the app id.
    pub cgroup: Option<Pattern>,
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
            && self.cgroup.is_none()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RuleAction {
    Float,
    Tile,
    /// Map the window fullscreen (COMP-05 §4). Applied after placement, so the
    /// rectangle it restores to on unfullscreen is whatever the other rules asked
    /// for.
    Fullscreen,
    Workspace(i32),
    /// Placement-time float geometry, in logical pixels. Both imply `float`:
    /// a tiled window's geometry belongs to the layout, not to a rule.
    Size(i32, i32),
    Position(i32, i32),
    /// Glob against an output connector or persistent identity.
    Output(String),
    /// COMP-07 §2 clamps this to `standard` for X11 windows.
    Trust(AppTrust),
    /// COMP-07 §6 clamps this to `lock` for X11 windows.
    Seat(SeatCompat),
    /// Hold the idle timers off while the window is mapped, for a client that
    /// does not speak `zwp_idle_inhibit_manager_v1` itself.
    IdleInhibit,
    Opacity(f32),
    /// Pick this window's blur mode, overriding `decoration.blur.mode`.
    /// Still gated by translucency at render time — an opaque window never
    /// blurs whatever the rule says.
    Blur(BlurRule),
    /// Raise-only: `secret` or `private`. `public` is refused at parse time
    /// because a rule may never lower a sensitivity class.
    Sensitivity(String),
    /// Pin the window's `irreversible_capable` fact (COMP-05 §1, S-06 §3.3)
    /// either way, overriding the desktop-category default.
    IrreversibleCapable(bool),
    NoAgent,
    NoFocusSteal,
}

/// A `windowrule "blur …"` value. `true` means "on, in the global mode" —
/// which is plain `blur` when the global mode is `off`; `false` is `off`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlurRule {
    On,
    Mode(BlurMode),
}

impl BlurRule {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "true" => Some(Self::On),
            "false" => Some(Self::Mode(BlurMode::Off)),
            other => BlurMode::parse(other).map(Self::Mode),
        }
    }

    /// The mode this rule selects under the global `decoration.blur.mode`.
    pub fn resolve(self, global: BlurMode) -> BlurMode {
        match self {
            Self::On if global == BlurMode::Off => BlurMode::Blur,
            Self::On => global,
            Self::Mode(m) => m,
        }
    }
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

/// `800x600` / `100,-40` for the geometry actions. Both halves must parse and
/// nothing may trail, so a typo drops its rule instead of half-applying.
fn parse_pair(source: &str, sep: char) -> Option<(i32, i32)> {
    let (a, b) = source.split_once(sep)?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

// Animation names and easing curves live in `schema` (ANIMATIONS,
// ANIMATION_CURVES). Unknown values are rejected at parse time rather than
// silently ignored at render time.
use schema::ANIMATION_CURVES;

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
    /// libinput's normalised speed, -1.0..=1.0; 0 is the device default.
    pub accel_speed: f64,
    /// Scroll method for pointers that are not touchpads: `default` (leave
    /// the device's own, which is how a trackpoint keeps scrolling), `none`
    /// or `on-button-down`.
    pub scroll_method: String,
    pub touchpad: Touchpad,
    /// `device "<libinput name>" { .. }` children in file order, one per
    /// name: a later block for the same name overrides an earlier one key by
    /// key. KDL-only.
    pub devices: Vec<InputDevice>,
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
            accel_speed: 0.0,
            scroll_method: "default".into(),
            touchpad: Touchpad::default(),
            devices: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Touchpad {
    pub natural_scroll: bool,
    pub tap_to_click: bool,
    /// Disable-while-typing.
    pub dwt: bool,
    /// `clickfinger` | `button-areas`.
    pub click_method: String,
    /// Tap, then hold the second tap down, to drag.
    pub tap_and_drag: bool,
    /// `two-finger` | `edge` | `none`.
    pub scroll_method: String,
}

/// One `input { device "<name>" { .. } }` block: any subset of the global
/// pointer keys, plus `calibration`, which only a device block may carry.
/// `None` inherits the global key.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InputDevice {
    /// The libinput device name, matched exactly.
    pub name: String,
    pub accel_profile: Option<String>,
    pub accel_speed: Option<f64>,
    pub scroll_method: Option<String>,
    pub touchpad: TouchpadOverride,
    /// libinput's 2x3 calibration matrix, row-major.
    pub calibration: Option<[f32; 6]>,
}

/// The `touchpad { }` half of a device block; `None` inherits.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TouchpadOverride {
    pub natural_scroll: Option<bool>,
    pub tap_to_click: Option<bool>,
    pub tap_and_drag: Option<bool>,
    pub dwt: Option<bool>,
    pub click_method: Option<String>,
    pub scroll_method: Option<String>,
}

/// One validated pointer key of `input` or of a `device` block.
enum PointerKey {
    AccelProfile(String),
    AccelSpeed(f64),
    ScrollMethod(String),
    Calibration([f32; 6]),
}

/// One validated key of a `touchpad` block.
enum TouchpadKey {
    NaturalScroll(bool),
    TapToClick(bool),
    TapAndDrag(bool),
    Dwt(bool),
    ClickMethod(String),
    ScrollMethod(String),
}

impl Default for Touchpad {
    fn default() -> Self {
        Self {
            natural_scroll: false,
            tap_to_click: false,
            dwt: false,
            click_method: "clickfinger".into(),
            tap_and_drag: true,
            scroll_method: "two-finger".into(),
        }
    }
}

/// Parse `text` as one abyss-owned file `a.kdl`, with no search path and no
/// widget catalog. For tests in this crate and in `ec-abyss`, which cannot
/// reach `Config`'s private parse state.
#[doc(hidden)]
pub fn parse_single_for_tests(text: &str) -> Config {
    let doc: KdlDocument = text.parse().unwrap();
    let mut cfg = Config {
        cur: Some((
            Source {
                path: PathBuf::from("a.kdl"),
                owner: schema::Owner::Abyss,
            },
            text.to_owned(),
        )),
        ..Config::default()
    };
    cfg.apply(&doc, &mut Vec::new());
    cfg
}

/// The one JSON shape a config refusal is reported in.
///
/// `watch::reload_now` emits it on the `config-error` event and the control
/// socket returns it from `validate_config`. One shape, one place: a GUI that
/// learns to render a hot-reload failure renders a rejected edit for free.
pub fn error_json(e: &ConfigError) -> serde_json::Value {
    serde_json::json!({
        "file": e.file.display().to_string(),
        "line": e.line,
        "col": e.col,
        "message": e.message,
        "snippet": e.snippet,
        "spanLen": e.span_len,
    })
}

/// The `config-error` event payload: every refusal, plus one line a human
/// reads at a glance.
///
/// Emitted by a failed hot reload and replayed to each new subscriber after a
/// startup that dropped nodes (ADR 0064). One event, not one per error: the
/// full list rides in `errors`, and `file`/`line`/`col`/`message` repeat the
/// first one for a consumer that shows a single problem.
pub fn error_event(errors: &[ConfigError], startup: bool) -> serde_json::Value {
    event(errors, startup, if startup { errors } else { &[] })
}

/// The `config-error` event for a failed hot reload. `live` is the refusal
/// list of the config still running: after a degraded start it carries the
/// [`FailSafe`] refusals, and while they stand the summary keeps leading with
/// them, so the fixed-id notification never trades "auto-lock is OFF" for a
/// milder "change not applied" (ADR 0064).
pub fn reload_error_event(errors: &[ConfigError], live: &[ConfigError]) -> serde_json::Value {
    event(errors, false, live)
}

fn event(errors: &[ConfigError], startup: bool, leads_from: &[ConfigError]) -> serde_json::Value {
    let first = errors.first();
    let mut v = serde_json::json!({
        "errors": errors.iter().map(error_json).collect::<Vec<_>>(),
        "startup": startup,
        "summary": summary(errors, startup, leads_from),
    });
    if let Some(e) = first {
        v["file"] = e.file.display().to_string().into();
        v["line"] = e.line.into();
        v["col"] = e.col.into();
        v["message"] = e.message.clone().into();
    }
    v
}

/// `abyss.kdl: 1 problem ignored — line 14: touchpad key needs a boolean: "drag-lock"`.
///
/// At startup a refusal that left a protection off ([`FailSafe`]) leads,
/// named for what it switched off — `abyss.kdl: auto-lock is OFF — line 3: …`
/// — so that "(and N more)" can never be where it hides.
///
/// After a failed reload the leads come from the *live* config (`leads_from`)
/// and go before the reload's own part:
/// `abyss.kdl: auto-lock is OFF — line 6: …; 1 problem, change not applied — line 2: …`.
fn summary(errors: &[ConfigError], startup: bool, leads_from: &[ConfigError]) -> String {
    let Some(first) = errors.first() else {
        return String::new();
    };
    let file = first
        .file
        .file_name()
        .map_or_else(|| "config".to_string(), |n| n.to_string_lossy().into_owned());
    let n = errors.len();
    let at = |e: &ConfigError| {
        if e.line > 0 {
            format!("line {}: {}", e.line, e.message)
        } else {
            e.message.clone()
        }
    };
    let leads: Vec<String> = [FailSafe::AutoLockOff, FailSafe::XwaylandOff]
        .into_iter()
        .filter_map(|g| {
            leads_from
                .iter()
                .find(|e| e.fail_safe == Some(g))
                .map(|e| format!("{} \u{2014} {}", g.label(), at(e)))
        })
        .collect();
    let plural = if n == 1 { "" } else { "s" };
    let (mut s, shown) = if startup && !leads.is_empty() {
        (format!("{file}: {}", leads.join("; ")), leads.len())
    } else if startup {
        (
            format!("{file}: {n} problem{plural} ignored \u{2014} {}", at(first)),
            1,
        )
    } else {
        let body = format!("{n} problem{plural}, change not applied \u{2014} {}", at(first));
        if leads.is_empty() {
            (format!("{file}: {body}"), 1)
        } else {
            (format!("{file}: {}; {body}", leads.join("; ")), 1)
        }
    };
    if n > shown {
        s.push_str(&format!(
            " (and {} more; `ec-ctl config validate` lists them)",
            n - shown
        ));
    }
    s
}

/// A refusal from config validation (COMP-13 §1.2). Carries the precise
/// `file:line:col` the spec requires so the message can be acted on directly.
#[derive(Debug, Clone)]
pub struct ConfigError {
    pub file: PathBuf,
    pub line: usize,
    pub col: usize,
    pub message: String,
    /// The offending source line, verbatim, and how many columns of it the
    /// token covers — so [`Display`] can point at it the way rustc does.
    pub snippet: Option<String>,
    pub span_len: usize,
    /// True when dropping this node at startup would leave a default that is
    /// not safe, so Abyss refuses to start rather than ignore it (ADR 0064):
    /// anything in `policy.kdl`, a policy-owned key in `abyss.kdl`, and
    /// `misc.render-device`.
    pub startup_fatal: bool,
    /// Set when this refusal is evidence that the owner tried to configure a
    /// protection and it did not take (ADR 0064). Startup still starts, but
    /// the summary leads with it, and for Xwayland the safe value (off) is
    /// used instead of the default. See [`Config::startup`].
    pub fail_safe: Option<FailSafe>,
}

/// A protection a dropped node may have been meant to configure (ADR 0064).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailSafe {
    /// A refused lock setting: the built-in default (never auto-lock) stays.
    /// Owner decision 2026-09-25: start anyway and say so plainly.
    AutoLockOff,
    /// A refused `xwayland` setting: Xwayland is forced off, the isolating
    /// value (ADR 0026), rather than the `enable #true` default.
    XwaylandOff,
}

impl FailSafe {
    fn label(self) -> &'static str {
        match self {
            FailSafe::AutoLockOff => "auto-lock is OFF",
            FailSafe::XwaylandOff => "Xwayland is OFF",
        }
    }
}

/// Which protection an *unknown top-level node* was evidently meant to be:
/// within edit distance 2 of `idle` or `xwayland` (`idel`, `xwyland`). Simple
/// and deterministic on purpose; a false hit only makes the notice louder
/// (and, for `xwayland`, starts without X11).
fn misspelt_guard(name: &str) -> Option<FailSafe> {
    if edit_distance(name, "idle") <= 2 {
        Some(FailSafe::AutoLockOff)
    } else if edit_distance(name, "xwayland") <= 2 {
        Some(FailSafe::XwaylandOff)
    } else {
        None
    }
}

/// What startup does with a loaded config (COMP-01 §5 step 3, amended by
/// ADR 0064).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Startup {
    /// Start. `ignored` rejected nodes were dropped; each setting they would
    /// have set keeps its default (or a lower-precedence file's value).
    Start { ignored: usize },
    /// At least one refusal is in the fail-closed set: do not start.
    Refuse { fatal: usize },
}

/// Nearest schema path by edit distance, when it is near enough to be worth
/// suggesting. Bounded at a third of the name's length so a wholly different
/// word never gets proposed as a typo.
fn did_you_mean(path: &str) -> Option<&'static str> {
    let budget = (path.len() / 3).max(1);
    schema::TABLE
        .iter()
        .map(|k| (edit_distance(path, k.path), k.path))
        .filter(|(d, _)| *d <= budget)
        .min_by_key(|(d, _)| *d)
        .map(|(_, p)| p)
}

/// [`did_you_mean`] over any candidate list: the nearest within a third of
/// `word`'s length, or nothing.
fn nearest<'a>(word: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let budget = (word.len() / 3).max(1);
    candidates
        .into_iter()
        .map(|c| (edit_distance(word, c), c))
        .filter(|(d, _)| *d <= budget)
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != *cb))
                .min(prev[j + 1] + 1)
                .min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Locate `offset` in `text`: 1-based line and column, plus the whole line it
/// falls on. `len` is clamped to what is left of that line so the caret run
/// never spills past the snippet.
fn locate(text: &str, offset: usize, len: usize) -> (usize, usize, String, usize) {
    let off = offset.min(text.len());
    let start = text[..off].rfind('\n').map_or(0, |i| i + 1);
    let end = text[off..].find('\n').map_or(text.len(), |i| off + i);
    let line = text[..off].matches('\n').count() + 1;
    (
        line,
        off - start + 1,
        text[start..end].to_string(),
        len.clamp(1, end - off),
    )
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}:{}: {}",
            self.file.display(),
            self.line,
            self.col,
            self.message
        )?;
        // Show the line and underline the token, so the position is actionable
        // without opening the file (COMP-13 §1.2 "the offending token").
        if let Some(src) = &self.snippet {
            let n = self.line.to_string();
            let pad = " ".repeat(n.len());
            write!(f, "\n{pad} |\n{n} | {src}\n{pad} | ")?;
            for (i, c) in src.char_indices() {
                if i + 1 >= self.col {
                    break;
                }
                // Keep tabs as tabs so the caret lines up in the user's terminal.
                f.write_str(if c == '\t' { "\t" } else { " " })?;
            }
            write!(f, "{}", "^".repeat(self.span_len.max(1)))?;
        }
        Ok(())
    }
}

/// One config file and the half of the surface it is allowed to set.
///
/// COMP-13 §1.3: `abyss.kdl` holds everything the human may retune freely;
/// `policy.kdl` holds the security surface (`schema::Owner::Policy`). Keeping
/// them apart is what lets the config GUI hold a write capability for one and
/// not the other, and what lets a deployment ship policy.kdl root-owned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub path: PathBuf,
    pub owner: schema::Owner,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub general: General,
    pub render: Render,
    pub bar: Bar,
    pub decoration: Decoration,
    pub animations: Animations,
    pub clipboard: Clipboard,
    pub capture: Capture,
    pub xwayland: Xwayland,
    pub idle: Idle,
    pub misc: Misc,
    pub setup: Setup,
    pub launcher: Launcher,
    pub ui: Ui,
    pub settings: SettingsApp,
    pub mode: Mode,
    pub components: Components,
    pub wallpaper: Wallpaper,
    pub annotations: AnnotationColors,
    pub oracle_eyes: OracleEyes,
    pub input: Input,
    pub binds: Vec<Bind>,
    /// Touchpad swipe bindings, one per `(fingers, direction)`: the defaults
    /// with every `gesture` node merged over them.
    pub gesture_binds: Vec<GestureBind>,
    /// Modifier + mouse-button bindings, one per `(mods, button)`: the
    /// defaults with every `mousebind` node merged over them (COMP-04 §5).
    pub mouse_binds: Vec<MouseBind>,
    /// Touchpad window drags, one per finger count: the default with every
    /// `gesture "drag"` node merged over it (COMP-04 §2, amended C-12).
    pub drag_gestures: Vec<DragGesture>,
    /// Per-workspace layout overrides, indexed 1..=10.
    pub workspace_layout: [Option<LayoutKind>; 10],
    /// `output` blocks in file order; the last match wins.
    pub outputs: Vec<OutputRule>,
    /// `windowrule` blocks in file order; all matching rules apply, later ones
    /// overriding earlier ones for the same property.
    pub window_rules: Vec<WindowRule>,
    /// Files this config was built from, in load order. The hot-reload
    /// watcher watches these and the directories that would contain them.
    pub sources: Vec<Source>,
    /// `--config <path>`, if one was given. Reload must honour it rather than
    /// falling back to the search path.
    pub explicit: Option<PathBuf>,
    /// The premade catalog was read as the lowest layer (ADR 0067). Set from
    /// the `taskbar-widgets` hook; reload keeps it.
    pub catalog_layer: bool,
    /// The catalog blocks as shipped, before any user block replaced one of
    /// the same name: the hash reference for `withhold::settle`.
    pub catalog: Vec<CustomWidget>,
    /// Validation refusals collected during the last load. On a freshly
    /// loaded config, non-empty means invalid: hot-reload keeps the last good
    /// one. On the *live* config it is non-empty only after a startup that
    /// dropped nodes (ADR 0064): those refusals stay here and are replayed to
    /// every `config-error` subscriber until a clean load replaces the config.
    pub errors: Vec<ConfigError>,
    /// The file being parsed, and its text, so `reject` can turn a KDL span
    /// into `file:line:col`. Cleared when the load finishes.
    cur: Option<(Source, String)>,
    /// `custom:<name>` ids in the file being parsed, checked at its end.
    pending_custom: Vec<PendingCustom>,
    /// Names of every named `widget` block seen, valid or refused. A refused
    /// block already has its own positioned error, so a `custom:` id naming
    /// it is not also reported missing.
    widget_blocks: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            general: General::default(),
            render: Render::default(),
            bar: Bar::default(),
            decoration: Decoration::default(),
            animations: Animations::default(),
            clipboard: Clipboard::default(),
            capture: Capture::default(),
            xwayland: Xwayland::default(),
            idle: Idle::default(),
            misc: Misc::default(),
            setup: Setup::default(),
            launcher: Launcher::default(),
            ui: Ui::default(),
            settings: SettingsApp::default(),
            mode: Mode::Hybrid,
            components: Components::default(),
            wallpaper: Wallpaper::default(),
            annotations: AnnotationColors::default(),
            oracle_eyes: OracleEyes::default(),
            input: Input::default(),
            binds: default_binds(),
            gesture_binds: default_gesture_binds(),
            mouse_binds: default_mouse_binds(),
            drag_gestures: default_drag_gestures(),
            workspace_layout: Default::default(),
            outputs: Vec::new(),
            window_rules: Vec::new(),
            sources: Vec::new(),
            explicit: None,
            catalog_layer: false,
            catalog: Vec::new(),
            errors: Vec::new(),
            cur: None,
            pending_custom: Vec::new(),
            widget_blocks: Vec::new(),
        }
    }
}

/// Merge config binds over the built-in defaults (COMP-13 §1.1).
///
/// Config binds *extend* the defaults; they never replace the table. A config
/// bind naming the same `(mods, key)` as a default replaces that default in
/// place, and a later config file replaces an earlier one the same way — the
/// result holds exactly one entry per chord, so `action_for`'s first-match
/// lookup cannot be shadowed by a default sitting ahead of a user bind.
///
/// `Super+Escape` (COMP-04 §6) and `Super+space` (COMP-13 §1.1) can never be
/// removed here: `parse_bind` refuses to produce them, so no config entry can
/// match those defaults and the defaults always survive the merge.
fn merge_binds(defaults: Vec<Bind>, from_file: Vec<Bind>) -> Vec<Bind> {
    let mut out: Vec<Bind> = Vec::with_capacity(defaults.len() + from_file.len());
    for b in from_file {
        match out.iter_mut().find(|o| o.mods == b.mods && o.key == b.key) {
            Some(slot) => *slot = b,
            None => out.push(b),
        }
    }
    for d in defaults {
        if !out.iter().any(|o| o.mods == d.mods && o.key == d.key) {
            out.push(d);
        }
    }
    out
}

/// The built-in binds, with the launcher's chords taken from
/// `launcher.bind` in place of the shipped Super+E / Super+R. They are still
/// defaults: a `bind` block on the same chord wins over them, and they win
/// over any other built-in on the chord the human picked (first default in
/// the list wins in `merge_binds`).
pub fn defaults_with_launcher(launcher: &Launcher) -> Vec<Bind> {
    let spawn = || Action::Spawn("ec-launcher".into());
    let mut out: Vec<Bind> = [&launcher.bind.open, &launcher.bind.run]
        .into_iter()
        .filter_map(|c| c.chord)
        .map(|(mods, key)| Bind {
            mods,
            key,
            action: spawn(),
        })
        .collect();
    out.extend(default_binds().into_iter().filter(|b| b.action != spawn()));
    out
}

/// The owner's Hyprland workspace swipe: three fingers moving left bring in
/// the next workspace, the way dragging a page left reveals the one after it.
pub fn default_gesture_binds() -> Vec<GestureBind> {
    vec![
        GestureBind {
            fingers: 3,
            direction: Direction::Left,
            action: Action::WorkspaceNext,
        },
        GestureBind {
            fingers: 3,
            direction: Direction::Right,
            action: Action::WorkspacePrev,
        },
    ]
}

/// Super + two-finger touchpad drag moves the window under the pointer
/// (ADR 0059). Super, not Alt: two-finger scroll with Alt held is a common
/// app gesture (zoom, horizontal scroll), and Super reaches no app.
pub fn default_drag_gestures() -> Vec<DragGesture> {
    vec![DragGesture {
        fingers: 2,
        mods: Mods {
            logo: true,
            ..Mods::default()
        },
    }]
}

/// Hyprland's `bindm` pair on Alt (ADR 0057): hold Alt, drag with the left
/// button to move a window and with the right button to resize it. Alt rather
/// than Super, which is kept free for a future launcher.
pub fn default_mouse_binds() -> Vec<MouseBind> {
    let alt = Mods {
        alt: true,
        ..Mods::default()
    };
    vec![
        MouseBind {
            mods: alt,
            button: MouseButton::Left,
            action: MouseAction::MoveWindow,
        },
        MouseBind {
            mods: alt,
            button: MouseButton::Right,
            action: MouseAction::ResizeWindow,
        },
    ]
}

fn m(logo: bool, shift: bool, ctrl: bool, alt: bool) -> Mods {
    Mods {
        logo,
        shift,
        ctrl,
        alt,
    }
}

/// The owner's Hyprland binds, key for key, so moving between the two
/// compositors costs no relearning (`~/.config/hypr/hyprland.lua`). Where
/// Hyprland spawns a Quickshell popup the equivalent here spawns our own
/// binary; where abyss has no matching action at all — fullscreen, pseudo,
/// the special workspace, the keybind cheatsheet, the dashboard, the overview
/// — the bind is simply absent rather than approximated.
///
/// `Super+Escape` is bound here and nowhere else: it is the trusted-UI
/// override chord (COMP-04 §6), always present and not rebindable —
/// `parse_bind` refuses any config that names it.
pub fn default_binds() -> Vec<Bind> {
    let sup = m(true, false, false, false);
    let sup_shift = m(true, true, false, false);
    let sup_ctrl = m(true, false, true, false);
    let none = m(false, false, false, false);
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
        // Applications. Every shipped bind names a binary EclipseOS installs:
        // a default pointing at something that is not there spawns, dies
        // silently, and reads to the user as a dead keybind (this is exactly
        // what kitty, dolphin and firefox did on the first real install).
        // Terminal is Super+Q and Super+Return both, the two chords people
        // reach for; anything else belongs in the user's own config.
        Bind {
            mods: sup,
            key: Keysym::q,
            action: Action::Spawn("foot".into()),
        },
        Bind {
            mods: sup,
            key: Keysym::Return,
            action: Action::Spawn("foot".into()),
        },
        Bind {
            mods: sup,
            key: Keysym::e,
            action: Action::Spawn("ec-launcher".into()),
        },
        // The desktop's own surfaces. Hyprland reaches these through
        // `qs -c eclipse ipc call ui toggle ...`; ours are separate binaries,
        // and each exits on Escape, so a second press of the bind is not a
        // toggle. Only the two that exist are bound.
        Bind {
            mods: sup,
            key: Keysym::r,
            action: Action::Spawn("ec-launcher".into()),
        },
        Bind {
            mods: sup,
            key: Keysym::n,
            action: Action::Spawn("ec-center".into()),
        },
        // Windows.
        Bind {
            mods: sup,
            key: Keysym::c,
            action: Action::Close,
        },
        Bind {
            mods: sup,
            key: Keysym::v,
            action: Action::ToggleFloating,
        },
        // Minimize is a pair, not a toggle: a window that has been sent away
        // holds no focus, so there is nothing for the same chord to act on.
        Bind {
            mods: sup,
            key: Keysym::h,
            action: Action::Minimize,
        },
        Bind {
            mods: sup_shift,
            key: Keysym::H,
            action: Action::Unminimize,
        },
        Bind {
            mods: sup,
            key: Keysym::j,
            action: Action::ToggleLayout,
        },
        Bind {
            mods: sup_shift,
            key: Keysym::Q,
            action: Action::Quit,
        },
        // Focus. Hyprland binds the arrows and nothing else, so neither do we.
        Bind {
            mods: sup,
            key: Keysym::Left,
            action: Action::Focus(Direction::Left),
        },
        Bind {
            mods: sup,
            key: Keysym::Right,
            action: Action::Focus(Direction::Right),
        },
        Bind {
            mods: sup,
            key: Keysym::Up,
            action: Action::Focus(Direction::Up),
        },
        Bind {
            mods: sup,
            key: Keysym::Down,
            action: Action::Focus(Direction::Down),
        },
        // Swapping a tiled window with its neighbour has no Hyprland bind to
        // copy; Shift over the focus arrows is the obvious pair and collides
        // with nothing. Floating windows move by mouse drag (`mousebind`).
        // Vertically, Shift+Up/Down change the window's Radiant priority
        // instead: a drag does the vertical swap, and `move-up`/`move-down`
        // stay bindable.
        Bind {
            mods: sup_shift,
            key: Keysym::Left,
            action: Action::Move(Direction::Left),
        },
        Bind {
            mods: sup_shift,
            key: Keysym::Right,
            action: Action::Move(Direction::Right),
        },
        Bind {
            mods: sup_shift,
            key: Keysym::Up,
            action: Action::Priority(1),
        },
        Bind {
            mods: sup_shift,
            key: Keysym::Down,
            action: Action::Priority(-1),
        },
        // Session.
        Bind {
            mods: sup_shift,
            key: Keysym::L,
            action: Action::Spawn("loginctl lock-session".into()),
        },
        // Media and brightness keys, unmodified, exactly as Hyprland has them.
        Bind {
            mods: none,
            key: Keysym::XF86_AudioRaiseVolume,
            action: Action::Spawn("wpctl set-volume -l 1 @DEFAULT_AUDIO_SINK@ 5%+".into()),
        },
        Bind {
            mods: none,
            key: Keysym::XF86_AudioLowerVolume,
            action: Action::Spawn("wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%-".into()),
        },
        Bind {
            mods: none,
            key: Keysym::XF86_AudioMute,
            action: Action::Spawn("wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle".into()),
        },
        Bind {
            mods: none,
            key: Keysym::XF86_AudioMicMute,
            action: Action::Spawn("wpctl set-mute @DEFAULT_AUDIO_SOURCE@ toggle".into()),
        },
        Bind {
            mods: none,
            key: Keysym::XF86_MonBrightnessUp,
            action: Action::Spawn("brightnessctl -e4 -n2 set 5%+".into()),
        },
        Bind {
            mods: none,
            key: Keysym::XF86_MonBrightnessDown,
            action: Action::Spawn("brightnessctl -e4 -n2 set 5%-".into()),
        },
        Bind {
            mods: none,
            key: Keysym::XF86_AudioNext,
            action: Action::Spawn("playerctl next".into()),
        },
        Bind {
            mods: none,
            key: Keysym::XF86_AudioPrev,
            action: Action::Spawn("playerctl previous".into()),
        },
        Bind {
            mods: none,
            key: Keysym::XF86_AudioPlay,
            action: Action::Spawn("playerctl play-pause".into()),
        },
        Bind {
            mods: none,
            key: Keysym::XF86_AudioPause,
            action: Action::Spawn("playerctl play-pause".into()),
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
        // Ctrl+Super+[1-9,0] (ADR 0049): move the focused window to display
        // (i + 1)'s currently active workspace. `DIGITS[9]` is the `0` key,
        // giving display 10, the same "0 wraps to the tenth slot" convention
        // `MoveToWorkspace` already uses above.
        b.push(Bind {
            mods: sup_ctrl,
            key: *key,
            action: Action::MoveToOutputWorkspace((i + 1) as u8),
        });
    }
    b
}

/// Collect the string arguments of a list node, warning when the same node
/// appears twice in one block: the second occurrence replaces the first rather
/// than adding to it, and silently dropping names from an allowlist is the
/// failure direction that matters in a fail-closed path.
fn names(n: &KdlNode, seen: &mut bool) -> Vec<String> {
    *seen = true;
    args(n)
        .into_iter()
        .filter_map(|v| v.as_string().map(str::to_string))
        .collect()
}

impl Config {
    /// Load from `explicit` if given, else from the search path. Validation is
    /// total (COMP-13 §1.2): any refusal lands in `errors`, and the caller
    /// decides — startup exits, hot-reload keeps the last good config.
    pub fn load(explicit: Option<&Path>) -> Self {
        Self::load_with(explicit, false)
    }

    /// [`Config::load`], with the premade widget catalog read first as the
    /// lowest layer when `catalog_layer` is set (ADR 0067). Its blocks count
    /// as defined for `custom:<name>` ids, and a later block of the same name
    /// replaces one, exactly like a block in `/etc` would be replaced.
    pub fn load_with(explicit: Option<&Path>, catalog_layer: bool) -> Self {
        // An explicit `--config` names one abyss.kdl; policy stays on the
        // search path, since a flag must not be able to swap the policy file.
        let files: Vec<Source> = match explicit {
            Some(p) => {
                let mut v = vec![Source {
                    path: p.to_path_buf(),
                    owner: schema::Owner::Abyss,
                }];
                v.extend(
                    search_path()
                        .into_iter()
                        .filter(|s| s.owner == schema::Owner::Policy),
                );
                v
            }
            None => search_path(),
        };
        let mut cfg = Config {
            explicit: explicit.map(|p| p.to_path_buf()),
            catalog_layer,
            ..Config::default()
        };
        if catalog_layer {
            cfg.catalog = catalog::load();
            cfg.bar.custom_widgets = cfg.catalog.clone();
            cfg.widget_blocks = cfg.catalog.iter().map(|w| w.name.clone()).collect();
        }
        let mut binds_from_file = Vec::new();
        let mut any = false;
        for f in &files {
            let text = match std::fs::read_to_string(&f.path) {
                Ok(t) => t,
                Err(e) => {
                    if explicit.is_some() && f.owner == schema::Owner::Abyss {
                        cfg.errors.push(ConfigError {
                            file: f.path.clone(),
                            line: 0,
                            col: 0,
                            message: format!("config unreadable: {e}"),
                            snippet: None,
                            span_len: 0,
                            // ADR 0064: an unreadable `--config` contributes
                            // nothing and Abyss starts on the rest.
                            startup_fatal: false,
                            fail_safe: None,
                        });
                    }
                    continue;
                }
            };
            let doc = match text.parse::<KdlDocument>() {
                Ok(d) => d,
                Err(e) => {
                    // Prefer the first diagnostic: it carries the position and the
                    // "expected ..." help, where the outer error is only generic.
                    let (off, message) = match e.diagnostics.first() {
                        Some(d) => {
                            let mut m = d.message.clone().unwrap_or_else(|| "invalid syntax".to_string());
                            if let Some(help) = &d.help {
                                m.push_str(" (");
                                m.push_str(help);
                                m.push(')');
                            }
                            ((d.span.offset(), d.span.len()), m)
                        }
                        None => ((0, 0), format!("{e}")),
                    };
                    let (line, col, snippet, span_len) = locate(&text, off.0, off.1);
                    cfg.errors.push(ConfigError {
                        file: f.path.clone(),
                        line,
                        col,
                        message,
                        snippet: Some(snippet),
                        span_len,
                        // ADR 0064: a file that is not KDL is dropped whole —
                        // unless it is `policy.kdl`, which stays fail-closed.
                        startup_fatal: f.owner == schema::Owner::Policy,
                        fail_safe: None,
                    });
                    continue;
                }
            };
            any = true;
            cfg.sources.push(f.clone());
            cfg.cur = Some((f.clone(), text));
            cfg.apply(&doc, &mut binds_from_file);
            cfg.cur = None;
        }
        let mut defaults = oracle_eyes_binds(&cfg.oracle_eyes);
        defaults.extend(defaults_with_launcher(&cfg.launcher));
        cfg.binds = merge_binds(defaults, binds_from_file);
        if !any {
            tracing::info!("no config found, using built-in defaults");
        } else {
            tracing::info!(sources = ?cfg.sources.iter().map(|s| &s.path).collect::<Vec<_>>(), binds = cfg.binds.len(), "config loaded");
        }
        cfg
    }

    /// Re-read the same sources. The inotify watcher (`config::watch`) calls
    /// this and must check `errors` before applying the result.
    pub fn reload(&self) -> Self {
        Self::load_with(self.explicit.as_deref(), self.catalog_layer)
    }

    /// Validate `text` as if it were the file at `path` owned by `owner`,
    /// without touching disk or the live config.
    ///
    /// This is what `validate_config` answers with, and it is deliberately the
    /// same code the loader runs: a GUI that previews an edit must be told
    /// exactly what a file edit would have been told, down to the wording.
    pub fn check_text(path: &Path, owner: schema::Owner, text: &str) -> Vec<ConfigError> {
        Self::check_text_with(path, owner, text, &[])
    }

    /// [`Config::check_text`], with `known` widget names (the premade
    /// catalog's) counting as defined for `custom:<name>` ids, as they do in
    /// the real load.
    pub fn check_text_with(
        path: &Path,
        owner: schema::Owner,
        text: &str,
        known: &[CustomWidget],
    ) -> Vec<ConfigError> {
        let doc = match text.parse::<KdlDocument>() {
            Ok(d) => d,
            Err(e) => {
                let (off, message) = match e.diagnostics.first() {
                    Some(d) => (
                        (d.span.offset(), d.span.len()),
                        d.message.clone().unwrap_or_else(|| "invalid syntax".to_string()),
                    ),
                    None => ((0, 0), format!("{e}")),
                };
                let (line, col, snippet, span_len) = locate(text, off.0, off.1);
                return vec![ConfigError {
                    file: path.to_path_buf(),
                    line,
                    col,
                    message,
                    snippet: Some(snippet),
                    span_len,
                    startup_fatal: owner == schema::Owner::Policy,
                    fail_safe: None,
                }];
            }
        };
        let mut cfg = Config {
            cur: Some((
                Source {
                    path: path.to_path_buf(),
                    owner,
                },
                text.to_owned(),
            )),
            widget_blocks: known.iter().map(|w| w.name.clone()).collect(),
            ..Config::default()
        };
        cfg.apply(&doc, &mut Vec::new());
        cfg.errors
    }

    /// Record a validation refusal against `node`'s position (COMP-13 §1.2).
    /// Also logged, so the journal shows the same text the caller gets.
    /// The parser's `unknown key` fallthrough, checked against the schema.
    ///
    /// This is the second of the three anti-drift sides in `schema.rs`: it runs
    /// at parse time, on the real user's file. A name the parser does not
    /// handle but the schema *does* claim is a wiring bug in this file, and the
    /// message says so rather than telling the human their config is wrong.
    fn unknown_key(&mut self, node: &KdlNode, prefix: &str, what: &str) {
        let name = node.name().value();
        let path = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}.{name}")
        };
        match schema::get_key(&path) {
            Some(_) => self.reject(
                node,
                format!("{path} is in the config schema but is not wired into the parser \u{2014} this is a bug in abyss, not in your config"),
            ),
            None => {
                let hint = did_you_mean(&path);
                self.reject(
                    node,
                    match hint {
                        Some(h) => format!("unknown {what} {name:?} (did you mean {h:?}?)"),
                        None => format!("unknown {what} {name:?}"),
                    },
                )
            }
        }
    }

    /// COMP-13 §1.3: refuse a key that belongs in the other file.
    ///
    /// Both directions. A policy key in `abyss.kdl` is the one that matters —
    /// `abyss.kdl` is writable by the config GUI and `policy.kdl` is not, so
    /// accepting it there would be a way around the write gate. The reverse is
    /// refused too, so `policy.kdl` stays small enough to read.
    ///
    /// The message names the file the key does belong in; a refusal the human
    /// cannot act on is a worse bug than the one it reports.
    fn owned_here(&mut self, node: &KdlNode, what: &str, owner: schema::Owner) -> bool {
        let Some((src, _)) = &self.cur else { return true };
        if src.owner == owner {
            return true;
        }
        let dest = match owner {
            schema::Owner::Abyss => "abyss.kdl",
            schema::Owner::Policy => "policy.kdl",
        };
        // ADR 0064 fail-closed case 2: a misplaced policy key is a protection
        // the owner meant to have; dropping it would silently discard it.
        self.reject_fatal(node, format!("{what} belongs in {dest}, not in this file"));
        false
    }

    fn reject(&mut self, node: &KdlNode, message: impl Into<String>) {
        // Underline the node name only; the node's own span runs to the end of
        // its children, which would drown the line in carets.
        self.reject_at(node.span().offset(), node.name().value().len(), message);
    }

    /// Refuse one argument of a node, underlining that argument rather than
    /// the node name: in `order "clock" "clokc"` the caret belongs under the
    /// typo. An entry's span may start at the whitespace before it.
    fn reject_entry(&mut self, entry: &KdlEntry, message: impl Into<String>) {
        let span = entry.span();
        let (offset, len) = match &self.cur {
            Some((_, text)) => {
                let raw = text.get(span.offset()..span.offset() + span.len()).unwrap_or("");
                let lead = raw.len() - raw.trim_start().len();
                (span.offset() + lead, raw.trim().len())
            }
            None => (span.offset(), span.len()),
        };
        self.reject_at(offset, len, message);
    }

    fn reject_at(&mut self, offset: usize, len: usize, message: impl Into<String>) {
        let message = message.into();
        let (file, line, col, snippet, span_len, policy) = match &self.cur {
            Some((f, text)) => {
                let (line, col, snippet, len) = locate(text, offset, len);
                (
                    f.path.clone(),
                    line,
                    col,
                    Some(snippet),
                    len,
                    f.owner == schema::Owner::Policy,
                )
            }
            None => (PathBuf::new(), 0, 0, None, 0, false),
        };
        tracing::error!(path = %file.display(), line, col, %message, "invalid config");
        self.errors.push(ConfigError {
            file,
            line,
            col,
            message,
            snippet,
            span_len,
            // ADR 0064 fail-closed case 1: nothing in `policy.kdl` is dropped
            // to a default at startup.
            startup_fatal: policy,
            fail_safe: None,
        });
    }

    /// Mark the refusal just recorded as a protection that did not take.
    fn fail_safe_last(&mut self, g: FailSafe) {
        if let Some(e) = self.errors.last_mut() {
            e.fail_safe = Some(g);
        }
    }

    /// [`Config::reject`] for a node whose default is not safe to fall back
    /// to: Abyss refuses to start on it rather than dropping it (ADR 0064).
    fn reject_fatal(&mut self, node: &KdlNode, message: impl Into<String>) {
        self.reject(node, message);
        if let Some(e) = self.errors.last_mut() {
            e.startup_fatal = true;
        }
    }

    /// What startup does with this config (COMP-01 §5 step 3, amended by
    /// ADR 0064): start with every rejected node dropped, or refuse when any
    /// refusal is in the fail-closed set.
    ///
    /// Starting also settles the [`FailSafe`] refusals: any refusal touching
    /// `xwayland` turns Xwayland off, and an auto-lock notice is kept only
    /// when auto-lock really is off (a lower-precedence file may still have
    /// set both lock keys), so the summary never claims what is not true.
    pub fn startup(&mut self) -> Startup {
        let fatal = self.errors.iter().filter(|e| e.startup_fatal).count();
        if fatal > 0 {
            return Startup::Refuse { fatal };
        }
        if self
            .errors
            .iter()
            .any(|e| e.fail_safe == Some(FailSafe::XwaylandOff))
        {
            self.xwayland.enable = false;
        }
        // A refused `widget` block is dropped whole, but its name was already
        // claimed, so a `custom:<name>` placed by `order`/`important` survived
        // validation. Drop those ids too: the bar never lists a widget that
        // has no block (ADR 0064 with ADR 0065). Withholding (ADR 0067) runs
        // later on what is left.
        let blocks: Vec<String> = self.bar.custom_widgets.iter().map(|w| w.name.clone()).collect();
        let listed = |id: &String| match id.strip_prefix(schema::BAR_WIDGET_CUSTOM_PREFIX) {
            Some(name) => blocks.iter().any(|b| b == name),
            None => true,
        };
        self.bar.widgets.order.retain(listed);
        self.bar.widgets.important.retain(listed);
        if self.idle.lock_command.is_some() && self.idle.lock_timeout.is_some() {
            for e in &mut self.errors {
                if e.fail_safe == Some(FailSafe::AutoLockOff) {
                    e.fail_safe = None;
                }
            }
        }
        Startup::Start {
            ignored: self.errors.len(),
        }
    }

    fn apply(&mut self, doc: &KdlDocument, binds: &mut Vec<Bind>) {
        let launcher_style_set = doc.nodes().iter().any(|n| {
            n.name().value() == "launcher"
                && n.children()
                    .is_some_and(|c| c.nodes().iter().any(|k| k.name().value() == "style"))
        });
        for node in doc.nodes() {
            let name = node.name().value();
            // Whole-node ownership, before the node is parsed at all. `misc`
            // and `windowrule` are mixed and check themselves, one key or one
            // action at a time.
            if let Some(owner) = schema::node_owner(name) {
                if !self.owned_here(node, &format!("{name:?}"), owner) {
                    continue;
                }
            }
            match name {
                "general" => self.apply_general(node),
                "bind" => match parse_bind(node) {
                    Ok(b) => binds.push(b),
                    Err(e) => self.reject(node, format!("ignoring bind (error={})", e)),
                },
                // Same merge rule as `merge_binds`, applied as each node
                // arrives: a later entry for the same `(fingers, direction)`
                // replaces the earlier one (default or file) in place.
                "gesture" => match parse_gesture(node) {
                    // A finger count is either swiped or dragged, never both:
                    // the begin could not tell them apart. Whichever node
                    // comes second is the one refused.
                    Ok(ParsedGesture::Swipe(g)) if self.drag_gestures.iter().any(|d| d.fingers == g.fingers) => {
                        self.reject(
                            node,
                            format!(
                                "ignoring gesture (error={n}-finger swipe collides with the {n}-finger drag gesture; disable that with gesture \"drag\" {n} {{ none; }})",
                                n = g.fingers
                            ),
                        )
                    }
                    Ok(ParsedGesture::Swipe(g)) => match self
                        .gesture_binds
                        .iter_mut()
                        .find(|o| o.fingers == g.fingers && o.direction == g.direction)
                    {
                        Some(slot) => *slot = g,
                        None => self.gesture_binds.push(g),
                    },
                    Ok(ParsedGesture::Drag(n, Some(_))) if self.gesture_bound(n) => self.reject(
                        node,
                        format!("ignoring gesture (error={n}-finger drag collides with a bound {n}-finger swipe)"),
                    ),
                    // Keyed on fingers: a later entry replaces, `none` removes.
                    Ok(ParsedGesture::Drag(fingers, mods)) => {
                        self.drag_gestures.retain(|d| d.fingers != fingers);
                        if let Some(mods) = mods {
                            self.drag_gestures.push(DragGesture { fingers, mods });
                        }
                    }
                    Err(e) => self.reject(node, format!("ignoring gesture (error={})", e)),
                },
                // Same replace-in-place rule, keyed on `(mods, button)`.
                "mousebind" => match parse_mousebind(node) {
                    Ok(b) => match self
                        .mouse_binds
                        .iter_mut()
                        .find(|o| o.mods == b.mods && o.button == b.button)
                    {
                        Some(slot) => *slot = b,
                        None => self.mouse_binds.push(b),
                    },
                    Err(e) => self.reject(node, format!("ignoring mousebind (error={})", e)),
                },
                "workspace" => self.apply_workspace(node),
                "render" => self.apply_render(node),
                "bar" => self.apply_bar(node, launcher_style_set),
                "launcher" => self.apply_launcher(node),
                "ui" => self.apply_ui(node),
                "settings" => self.apply_settings(node),
                "clipboard" => self.apply_clipboard(node),
                "capture" => self.apply_capture(node),
                "xwayland" => self.apply_xwayland(node),
                "idle" => self.apply_idle(node),
                "misc" => self.apply_misc(node),
                "setup" => self.apply_setup(node),
                "mode" => match arg(node).and_then(KdlValue::as_string) {
                    Some("wm") => self.mode = Mode::Wm,
                    Some("hybrid") => self.mode = Mode::Hybrid,
                    Some("de") => self.mode = Mode::De,
                    other => self.reject(
                        node,
                        format!(
                            "mode: unknown value (other={other:?}); expected one of {}",
                            schema::MODES.join(", ")
                        ),
                    ),
                },
                "components" => self.apply_components(node),
                "wallpaper" => self.apply_wallpaper(node),
                "annotations" => self.apply_annotations(node),
                "oracle-eyes" => self.apply_oracle_eyes(node),
                "input" => self.apply_input(node),
                "output" => self.apply_output(node),
                "decoration" => self.apply_decoration(node),
                "animations" => self.apply_animations(node),
                "windowrule" => self.apply_windowrule(node),
                _ => {
                    self.unknown_key(node, "", "config node");
                    if let Some(g) = misspelt_guard(name) {
                        self.fail_safe_last(g);
                    }
                }
            }
        }
        self.check_custom_widget_ids();
    }

    /// Every `custom:<name>` in `bar.widgets.*` must name a `widget` block
    /// from this file or one loaded before it. Run at the end of each file
    /// so a block may follow its use, and while `cur` still points at the
    /// file the id is in.
    fn check_custom_widget_ids(&mut self) {
        for p in std::mem::take(&mut self.pending_custom) {
            if self.widget_blocks.contains(&p.name) {
                continue;
            }
            let near = nearest(&p.name, self.bar.custom_widgets.iter().map(|w| w.name.as_str()));
            let message = match near {
                Some(n) => format!(
                    "custom:{} names no widget block (did you mean \"custom:{n}\"?)",
                    p.name
                ),
                None => format!(
                    "custom:{} names no widget block; define one inside bar: widget {:?} {{ exec \"…\" }}",
                    p.name, p.name
                ),
            };
            self.reject_at(p.offset, p.len, message);
        }
    }

    fn apply_general(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            let name = n.name().value();
            match name {
                "gaps-in" => {
                    if !set_i32(&mut self.general.gaps_in, n) {
                        self.reject(n, "gaps-in expects an integer");
                    }
                }
                "gaps-out" => {
                    if !set_i32(&mut self.general.gaps_out, n) {
                        self.reject(n, "gaps-out expects an integer");
                    }
                }
                "gaps-in-vertical" => {
                    if !set_opt_i32(&mut self.general.gaps_in_vertical, n) {
                        self.reject(n, "gaps-in-vertical expects an integer");
                    }
                }
                "gaps-out-vertical" => {
                    if !set_opt_i32(&mut self.general.gaps_out_vertical, n) {
                        self.reject(n, "gaps-out-vertical expects an integer");
                    }
                }
                "border-size" => {
                    if !set_i32(&mut self.general.border_size, n) {
                        self.reject(n, "border-size expects an integer");
                    }
                }
                "focus-follows-mouse" => {
                    if let Some(b) = arg(n).and_then(KdlValue::as_bool) {
                        self.general.focus_follows_mouse = b;
                    }
                }
                "focus-follows-mouse-across-outputs" => {
                    if let Some(b) = arg(n).and_then(KdlValue::as_bool) {
                        self.general.focus_follows_mouse_across_outputs = b;
                    }
                }
                "cursor-follows-moved-window" => {
                    if let Some(b) = arg(n).and_then(KdlValue::as_bool) {
                        self.general.cursor_follows_moved_window = b;
                    }
                }
                "follow-window-to-workspace" => {
                    if let Some(b) = arg(n).and_then(KdlValue::as_bool) {
                        self.general.follow_window_to_workspace = b;
                    }
                }
                "unfocus-on-empty-workspace" => {
                    if let Some(b) = arg(n).and_then(KdlValue::as_bool) {
                        self.general.unfocus_on_empty_workspace = b;
                    }
                }
                // Removed: hover never takes the keyboard from an on-demand
                // layer now, only a click does (wlr-layer-shell `on_demand`).
                "focus-follows-mouse-layers" => {
                    tracing::warn!("deprecated: general.focus-follows-mouse-layers is ignored");
                }
                "refocus-on-scene-change" => {
                    if let Some(b) = arg(n).and_then(KdlValue::as_bool) {
                        self.general.refocus_on_scene_change = b;
                    }
                }
                "layout" => match arg(n).and_then(KdlValue::as_string) {
                    Some("radiant") => self.general.layout = LayoutKind::Radiant,
                    Some("dwindle") => self.general.layout = LayoutKind::Dwindle,
                    Some("master") => self.general.layout = LayoutKind::Master,
                    other => self.reject(n, format!("unknown layout {other:?}")),
                },
                "floating-placement" => match arg(n).and_then(KdlValue::as_string) {
                    Some("centered") => self.general.floating_placement = FloatingPlacement::Centered,
                    Some("pointer") => self.general.floating_placement = FloatingPlacement::Pointer,
                    Some("cascade") => self.general.floating_placement = FloatingPlacement::Cascade,
                    other => self.reject(n, format!("unknown floating-placement {other:?}")),
                },
                "drop-guides" => {
                    if let Some(b) = arg(n).and_then(KdlValue::as_bool) {
                        self.general.drop_guides = b;
                    }
                }
                "drop-guide-color" => match arg(n).and_then(KdlValue::as_string).and_then(parse_color) {
                    Some(c) => self.general.drop_guide_color = c,
                    None => self.reject(n, "bad color for \"drop-guide-color\""),
                },
                "drop-edge-band" => {
                    if !set_i32(&mut self.general.drop_edge_band, n) {
                        self.reject(n, "drop-edge-band expects an integer");
                    }
                }
                "col-active-border" | "col-inactive-border" => {
                    match arg(n).and_then(KdlValue::as_string).and_then(parse_color) {
                        Some(c) if name.starts_with("col-active") => self.general.col_active = c,
                        Some(c) => self.general.col_inactive = c,
                        None => self.reject(n, format!("bad color for {name:?}")),
                    }
                }
                _ => self.unknown_key(n, "general", "general key"),
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
                _ => self.unknown_key(n, "render", "render node"),
            }
        }
    }

    /// `style_set`: this document sets `launcher.style`, which wins over the
    /// deprecated `bar.launcher-style` wherever either sits.
    fn apply_bar(&mut self, node: &KdlNode, style_set: bool) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "fold-when-inactive" => {
                    self.bar.fold_when_inactive = arg(n).and_then(KdlValue::as_bool).unwrap_or(false);
                }
                "fold-height" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) => self.bar.fold_height = v.clamp(2, 16) as u32,
                    None => self.reject(n, "fold-height expects an integer"),
                },
                "rounding" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) => self.bar.rounding = v.clamp(0, 64) as u32,
                    None => self.reject(n, "rounding expects an integer"),
                },
                "fold-when-idle" => {
                    self.bar.fold_when_idle = arg(n).and_then(KdlValue::as_bool).unwrap_or(false);
                }
                "idle-seconds" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) => self.bar.idle_seconds = v.clamp(5, 600) as u32,
                    None => self.reject(n, "idle-seconds expects an integer"),
                },
                "fold-duration-ms" => match arg(n).and_then(parse_duration_ms) {
                    Some(v) => self.bar.fold_duration_ms = v.min(1000),
                    None => self.reject(n, "fold-duration-ms expects a duration"),
                },
                "fold-curve" => match arg(n).and_then(KdlValue::as_string) {
                    Some(c) if ANIMATION_CURVES.contains(&c) => {
                        self.bar.fold_curve = c.to_owned();
                    }
                    other => self.reject(
                        n,
                        format!("unknown bar fold-curve, keeping default (other={:?})", other),
                    ),
                },
                "position" => match arg(n).and_then(KdlValue::as_string) {
                    Some("top") => self.bar.position = BarPosition::Top,
                    Some("bottom") => self.bar.position = BarPosition::Bottom,
                    other => self.reject(
                        n,
                        format!("unknown bar position, keeping default (other={:?})", other),
                    ),
                },
                "tray" => self.apply_bar_tray(n),
                "clock" => self.apply_bar_clock(n),
                "widgets" => self.apply_bar_widgets(n),
                "motion" => self.apply_bar_motion(n),
                "widget" => self.apply_bar_widget(n),
                "eye" => {
                    self.bar.eye = arg(n).and_then(KdlValue::as_bool).unwrap_or(true);
                }
                "popup-anchor" => match arg(n).and_then(KdlValue::as_string) {
                    Some("cell") => self.bar.popup_anchor = BarPopupAnchor::Cell,
                    Some("pointer") => self.bar.popup_anchor = BarPopupAnchor::Pointer,
                    other => self.reject(
                        n,
                        format!(
                            "bar popup-anchor must be \"cell\" or \"pointer\", keeping default (other={:?})",
                            other
                        ),
                    ),
                },
                // Deprecated alias of `launcher.style`: still loads so an
                // old file keeps its launcher, and is not in the schema, so
                // a GUI shows the one key. Writing `launcher.style` removes
                // it (`edit::SUPERSEDES`).
                "launcher-style" if style_set => {
                    tracing::warn!("deprecated: bar.launcher-style is ignored beside launcher.style");
                }
                "launcher-style" => match arg(n).and_then(KdlValue::as_string) {
                    Some(v @ ("centered" | "menu")) => {
                        tracing::warn!("deprecated: bar.launcher-style; write launcher.style instead");
                        self.launcher.style = if v == "menu" {
                            LauncherStyle::Menu
                        } else {
                            LauncherStyle::Centered
                        };
                    }
                    other => self.reject(
                        n,
                        format!(
                            "bar launcher-style must be \"centered\" or \"menu\", keeping default (other={:?})",
                            other
                        ),
                    ),
                },
                _ => self.unknown_key(n, "bar", "bar node"),
            }
        }
    }

    fn apply_bar_clock(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            let slot = match n.name().value() {
                "hour-12" => &mut self.bar.clock.hour_12,
                "date-mdy" => &mut self.bar.clock.date_mdy,
                _ => {
                    self.unknown_key(n, "bar.clock", "bar clock node");
                    continue;
                }
            };
            *slot = arg(n).and_then(KdlValue::as_bool).unwrap_or(true);
        }
    }

    fn apply_bar_tray(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        let mut seen_pinned = false;
        let mut seen_hidden = false;
        for n in children.nodes() {
            match n.name().value() {
                "pinned" => {
                    // A rejected node is dropped whole, never applied as well
                    // (ADR 0064): the first `pinned` stands.
                    if seen_pinned {
                        self.reject(n, "repeated pinned is ignored; list every id on a single node");
                        continue;
                    }
                    self.bar.tray.pinned = Some(names(n, &mut seen_pinned));
                }
                "hidden" => {
                    if seen_hidden {
                        self.reject(n, "repeated hidden is ignored; list every id on a single node");
                        continue;
                    }
                    self.bar.tray.hidden = names(n, &mut seen_hidden);
                }
                _ => self.unknown_key(n, "bar.tray", "bar tray node"),
            }
        }
        // The built-in applets became widgets (ADR 0065). Their old ids still
        // load so an existing file keeps working, but they mean nothing here
        // any more; say so once per id and point at the fix.
        let tray = &self.bar.tray;
        for id in tray.pinned.iter().flatten().chain(&tray.hidden) {
            if schema::LEGACY_TRAY_BUILTINS.contains(&id.as_str()) {
                tracing::warn!(
                    id = id.as_str(),
                    "deprecated: built-in applet id in bar.tray; list it in bar.widgets.order \
                     instead (ec-ctl config migrate does this)"
                );
            }
        }
    }

    /// `bar { widgets { … } }` (ADR 0065).
    fn apply_bar_widgets(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        let mut seen_order = false;
        let mut seen_important = false;
        for n in children.nodes() {
            match n.name().value() {
                "order" => {
                    if seen_order {
                        self.reject(
                            n,
                            "repeated order replaces the previous one; list every id on a single node",
                        );
                    }
                    seen_order = true;
                    self.bar.widgets.order = self.widget_ids(n);
                }
                "important" => {
                    if seen_important {
                        self.reject(
                            n,
                            "repeated important replaces the previous one; list every id on a single node",
                        );
                    }
                    seen_important = true;
                    self.bar.widgets.important = self.widget_ids(n);
                }
                "now-playing" => self.apply_bar_widget_now_playing(n),
                "system-usage" => self.apply_bar_widget_system_usage(n),
                "volume" => self.apply_bar_widget_volume(n),
                _ => self.unknown_key(n, "bar.widgets", "bar widgets node"),
            }
        }
    }

    /// The ids on an `order`/`important` node. Built-in ids are checked now;
    /// `custom:<name>` is queued for [`Self::check_custom_widget_ids`] at the
    /// end of the file. A refused id is dropped, never kept.
    fn widget_ids(&mut self, n: &KdlNode) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for e in n.entries() {
            if e.name().is_some() {
                self.reject_entry(e, "widget ids are plain strings, not properties");
                continue;
            }
            let Some(id) = e.value().as_string() else {
                self.reject_entry(e, "a widget id is a string");
                continue;
            };
            if out.iter().any(|o| o == id) {
                self.reject_entry(e, format!("widget {id:?} is listed twice"));
                continue;
            }
            if let Some(name) = id.strip_prefix(schema::BAR_WIDGET_CUSTOM_PREFIX) {
                if name.is_empty() {
                    self.reject_entry(e, "custom: needs a widget name, e.g. \"custom:weather\"");
                    continue;
                }
                let span = e.span();
                let (offset, len) = match &self.cur {
                    Some((_, text)) => {
                        let raw = text.get(span.offset()..span.offset() + span.len()).unwrap_or("");
                        let lead = raw.len() - raw.trim_start().len();
                        (span.offset() + lead, raw.trim().len())
                    }
                    None => (span.offset(), span.len()),
                };
                self.pending_custom.push(PendingCustom {
                    name: name.to_owned(),
                    offset,
                    len,
                });
            } else if !schema::BAR_WIDGET_IDS.contains(&id) {
                let message = match nearest(id, schema::BAR_WIDGET_IDS.iter().copied()) {
                    Some(h) => format!("unknown widget {id:?} (did you mean {h:?}?)"),
                    None => format!(
                        "unknown widget {id:?}; built-in widgets are {}, or custom:<name> for a widget block",
                        schema::BAR_WIDGET_IDS.join(", ")
                    ),
                };
                self.reject_entry(e, message);
                continue;
            }
            out.push(id.to_owned());
        }
        out
    }

    /// A bool node: bare is `#true`; anything but a bool is refused.
    fn flag(&mut self, n: &KdlNode) -> Option<bool> {
        match arg(n) {
            None => Some(true),
            Some(v) => match v.as_bool() {
                Some(b) => Some(b),
                None => {
                    self.reject(n, format!("{} expects #true or #false", n.name().value()));
                    None
                }
            },
        }
    }

    /// An integer node within `min..=max`. Out of range is refused, keeping
    /// the default, rather than clamped behind the human's back.
    fn int_in(&mut self, n: &KdlNode, min: u32, max: u32) -> Option<u32> {
        match arg(n).and_then(KdlValue::as_integer) {
            Some(v) if (min as i128..=max as i128).contains(&v) => Some(v as u32),
            Some(v) => {
                self.reject(
                    n,
                    format!(
                        "{} must be {min}..={max}, keeping default (got {v})",
                        n.name().value()
                    ),
                );
                None
            }
            None => {
                self.reject(n, format!("{} expects an integer", n.name().value()));
                None
            }
        }
    }

    /// A duration node (integer ms, or `"2s"`) within `min..=max` ms.
    fn ms_in(&mut self, n: &KdlNode, min: u32, max: u32) -> Option<u32> {
        match arg(n).and_then(parse_duration_ms) {
            Some(v) if (min..=max).contains(&v) => Some(v),
            Some(v) => {
                self.reject(
                    n,
                    format!(
                        "{} must be {min}..={max} ms, keeping default (got {v})",
                        n.name().value()
                    ),
                );
                None
            }
            None => {
                self.reject(n, format!("{} expects a duration", n.name().value()));
                None
            }
        }
    }

    fn apply_bar_widget_now_playing(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "art" => {
                    if let Some(b) = self.flag(n) {
                        self.bar.widgets.now_playing.art = b;
                    }
                }
                "visualizer" => {
                    if let Some(b) = self.flag(n) {
                        self.bar.widgets.now_playing.visualizer = b;
                    }
                }
                "remote-art" => {
                    if let Some(b) = self.flag(n) {
                        self.bar.widgets.now_playing.remote_art = b;
                    }
                }
                _ => self.unknown_key(n, "bar.widgets.now-playing", "now-playing node"),
            }
        }
    }

    fn apply_bar_widget_system_usage(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "interval-ms" => {
                    if let Some(v) = self.ms_in(n, 250, 10_000) {
                        self.bar.widgets.system_usage.interval_ms = v;
                    }
                }
                "gpu" => {
                    if let Some(b) = self.flag(n) {
                        self.bar.widgets.system_usage.gpu = b;
                    }
                }
                "disk" => {
                    if let Some(b) = self.flag(n) {
                        self.bar.widgets.system_usage.disk = b;
                    }
                }
                "disk-path" => match arg(n).and_then(KdlValue::as_string) {
                    Some(p) if p.starts_with('/') => self.bar.widgets.system_usage.disk_path = p.to_owned(),
                    _ => self.reject(n, "disk-path expects an absolute path, e.g. \"/home\""),
                },
                _ => self.unknown_key(n, "bar.widgets.system-usage", "system-usage node"),
            }
        }
    }

    fn apply_bar_widget_volume(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "step" => {
                    if let Some(v) = self.int_in(n, 1, 25) {
                        self.bar.widgets.volume.step = v;
                    }
                }
                "scroll" => {
                    if let Some(b) = self.flag(n) {
                        self.bar.widgets.volume.scroll = b;
                    }
                }
                "max-percent" => {
                    if let Some(v) = self.int_in(n, 100, 150) {
                        self.bar.widgets.volume.max_percent = v;
                    }
                }
                _ => self.unknown_key(n, "bar.widgets.volume", "volume node"),
            }
        }
    }

    /// `bar { motion { … } }` (ADR 0065).
    fn apply_bar_motion(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "enabled" => {
                    if let Some(b) = self.flag(n) {
                        self.bar.motion.enabled = b;
                    }
                }
                "duration-ms" => {
                    if let Some(v) = self.ms_in(n, 0, 2000) {
                        self.bar.motion.duration_ms = v;
                    }
                }
                "curve" => match arg(n).and_then(KdlValue::as_string) {
                    Some(c) if schema::BAR_MOTION_CURVES.contains(&c) => self.bar.motion.curve = c.to_owned(),
                    other => self.reject(
                        n,
                        format!(
                            "bar motion curve must be one of {}, keeping default (other={other:?})",
                            schema::BAR_MOTION_CURVES.join(", ")
                        ),
                    ),
                },
                _ => self.unknown_key(n, "bar.motion", "bar motion node"),
            }
        }
    }

    /// The string arguments of an argv node (`exec`, `on-click`, …): at least
    /// one, all strings. `None` after a refusal.
    fn argv(&mut self, n: &KdlNode) -> Option<Vec<String>> {
        let mut out = Vec::new();
        for e in n.entries() {
            match (e.name(), e.value().as_string()) {
                (None, Some(s)) => out.push(s.to_owned()),
                _ => {
                    self.reject_entry(e, format!("{} takes only string arguments", n.name().value()));
                    return None;
                }
            }
        }
        if out.is_empty() || out[0].is_empty() {
            self.reject(
                n,
                format!(
                    "{} needs a command: {} \"<argv0>\" …",
                    n.name().value(),
                    n.name().value()
                ),
            );
            return None;
        }
        Some(out)
    }

    /// `bar { widget "<name>" { … } }` (ADR 0065). The whole block is refused
    /// on any error, so a widget never runs half-configured.
    fn apply_bar_widget(&mut self, node: &KdlNode) {
        let name = match arg(node).and_then(KdlValue::as_string) {
            Some(s) if !s.trim().is_empty() => s.to_owned(),
            _ => {
                self.reject(node, "widget needs a name: widget \"<name>\" { … }");
                return;
            }
        };
        if !self.widget_blocks.contains(&name) {
            self.widget_blocks.push(name.clone());
        }
        let mut ok = true;
        let mut exec = None;
        let mut interval_ms = None;
        let mut stream = false;
        let mut source = None;
        let mut format = None;
        let mut icon = None;
        let mut on_click = None;
        let mut on_scroll_up = None;
        let mut on_scroll_down = None;
        let mut seen: Vec<&str> = Vec::new();
        for n in node.children().map(|c| c.nodes()).unwrap_or_default() {
            let key = n.name().value();
            let Some(form) = schema::find(schema::WIDGET_KEYS, key) else {
                let names = schema::WIDGET_KEYS.iter().map(|f| f.names[0]);
                self.reject(
                    n,
                    match nearest(key, names) {
                        Some(h) => format!("unknown widget node {key:?} (did you mean {h:?}?)"),
                        None => format!("unknown widget node {key:?}"),
                    },
                );
                ok = false;
                continue;
            };
            if seen.contains(&form.names[0]) {
                self.reject(n, format!("{key} given twice in widget {name:?}"));
                ok = false;
                continue;
            }
            seen.push(form.names[0]);
            let good = match form.names[0] {
                "exec" => self.argv(n).map(|a| exec = Some(a)).is_some(),
                "on-click" => self.argv(n).map(|a| on_click = Some(a)).is_some(),
                "on-scroll-up" => self.argv(n).map(|a| on_scroll_up = Some(a)).is_some(),
                "on-scroll-down" => self.argv(n).map(|a| on_scroll_down = Some(a)).is_some(),
                "interval-ms" => self
                    .ms_in(n, schema::WIDGET_MIN_INTERVAL_MS, schema::WIDGET_MAX_INTERVAL_MS)
                    .map(|v| interval_ms = Some(v))
                    .is_some(),
                "stream" => self.flag(n).map(|b| stream = b).is_some(),
                "source" => match arg(n).and_then(KdlValue::as_string) {
                    Some(s) if schema::WIDGET_SOURCES.contains(&s) => {
                        source = Some(s.to_owned());
                        true
                    }
                    other => {
                        let other = other.unwrap_or("");
                        self.reject(
                            n,
                            match nearest(other, schema::WIDGET_SOURCES.iter().copied()) {
                                Some(h) => format!("unknown widget source {other:?} (did you mean {h:?}?)"),
                                None => format!(
                                    "unknown widget source {other:?}; sources are {}",
                                    schema::WIDGET_SOURCES.join(", ")
                                ),
                            },
                        );
                        false
                    }
                },
                "format" | "icon" => match arg(n).and_then(KdlValue::as_string) {
                    Some(s) => {
                        let slot = if form.names[0] == "format" {
                            &mut format
                        } else {
                            &mut icon
                        };
                        *slot = Some(s.to_owned());
                        true
                    }
                    None => {
                        self.reject(n, format!("{key} expects a string"));
                        false
                    }
                },
                // Every WIDGET_KEYS form is matched above; the table and this
                // match are one list, and a form added to one alone must fail.
                other => {
                    self.reject(n, format!("widget node {other:?} is documented but not wired into the parser \u{2014} this is a bug in abyss, not in your config"));
                    false
                }
            };
            ok &= good;
        }
        let kind = match (exec, source) {
            (Some(_), Some(_)) => {
                self.reject(
                    node,
                    format!("widget {name:?} has both exec and source; give it one"),
                );
                return;
            }
            (None, None) => {
                self.reject(
                    node,
                    format!("widget {name:?} needs exec \"<argv0>\" … or source \"<source>\""),
                );
                return;
            }
            (Some(argv), None) => {
                if format.is_some() {
                    self.reject(
                        node,
                        format!("widget {name:?}: format is for source widgets, not exec"),
                    );
                    return;
                }
                match (stream, interval_ms) {
                    (true, Some(_)) => {
                        self.reject(
                            node,
                            format!("widget {name:?}: a stream runs once and never on an interval; drop interval-ms"),
                        );
                        return;
                    }
                    (true, None) => CustomWidgetKind::Stream { argv },
                    (false, i) => CustomWidgetKind::Exec {
                        argv,
                        interval_ms: i.unwrap_or(schema::WIDGET_DEFAULT_INTERVAL_MS),
                    },
                }
            }
            (None, Some(source)) => {
                if stream || interval_ms.is_some() {
                    self.reject(
                        node,
                        format!("widget {name:?}: stream and interval-ms are for exec widgets, not source"),
                    );
                    return;
                }
                CustomWidgetKind::Source {
                    source,
                    format: format.unwrap_or_else(|| "{}".to_owned()),
                }
            }
        };
        if !ok {
            return;
        }
        let w = CustomWidget {
            name,
            kind,
            icon,
            on_click,
            on_scroll_up,
            on_scroll_down,
        };
        match self.bar.custom_widgets.iter_mut().find(|o| o.name == w.name) {
            Some(slot) => *slot = w,
            None => self.bar.custom_widgets.push(w),
        }
    }

    fn apply_clipboard(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        let mut seen_allow = false;
        for n in children.nodes() {
            match n.name().value() {
                "data-control-allow" => {
                    if seen_allow {
                        self.reject(n, "repeated data-control-allow replaces the previous one; list every name on a single node");
                    }
                    self.clipboard.data_control_allow = names(n, &mut seen_allow);
                }
                _ => self.unknown_key(n, "clipboard", "clipboard node"),
            }
        }
    }

    fn apply_capture(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        let mut seen_allow = false;
        let mut seen_redact = false;
        let mut seen_hide = false;
        for n in children.nodes() {
            match n.name().value() {
                "allow" => {
                    if seen_allow {
                        self.reject(
                            n,
                            "repeated allow replaces the previous one; list every name on a single node",
                        );
                    }
                    self.capture.allow = names(n, &mut seen_allow);
                }
                "redact-app-id" => {
                    if seen_redact {
                        self.reject(n, "repeated redact-app-id replaces the previous one; list every name on a single node");
                    }
                    self.capture.redact_app_id = names(n, &mut seen_redact);
                }
                "hide-layer" => {
                    if seen_hide {
                        self.reject(
                            n,
                            "repeated hide-layer replaces the previous one; list every pair on a single node",
                        );
                    }
                    seen_hide = true;
                    let mut pairs = Vec::new();
                    for v in args(n) {
                        let pair = v
                            .as_string()
                            .and_then(|a| a.split_once(':'))
                            .filter(|(exe, ns)| !exe.is_empty() && !ns.is_empty());
                        match pair {
                            Some((exe, ns)) => pairs.push((exe.to_string(), ns.to_string())),
                            None => self.reject(n, format!("hide-layer entry {v} must be \"exe:namespace\" with both parts non-empty")),
                        }
                    }
                    self.capture.hide_layer = pairs;
                }
                _ => self.unknown_key(n, "capture", "capture node"),
            }
        }
    }

    /// Every refusal in here is [`FailSafe::XwaylandOff`] (ADR 0064): the
    /// owner was configuring X11 and it did not take, so start without it.
    fn apply_xwayland(&mut self, node: &KdlNode) {
        let before = self.errors.len();
        self.apply_xwayland_children(node);
        for e in &mut self.errors[before..] {
            e.fail_safe = Some(FailSafe::XwaylandOff);
        }
    }

    fn apply_xwayland_children(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            let name = n.name().value();
            match name {
                "enable" => match arg(n).and_then(KdlValue::as_bool) {
                    Some(b) => self.xwayland.enable = b,
                    None => self.reject(n, "xwayland enable needs a boolean (#true or #false)"),
                },
                "scaling" => match arg(n).and_then(KdlValue::as_string) {
                    Some("client") => self.xwayland.scaling_client = true,
                    Some("compositor") => self.xwayland.scaling_client = false,
                    other => self.reject(
                        n,
                        format!(
                            "unknown xwayland scaling mode, keeping default (other={:?})",
                            other
                        ),
                    ),
                },
                _ => self.unknown_key(n, "xwayland", "xwayland node"),
            }
        }
    }

    fn apply_idle(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            let name = n.name().value();
            let before = self.errors.len();
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
                        _ => self.reject(
                            n,
                            format!("idle timeout must be a non-negative integer (node={})", name),
                        ),
                    }
                }
                "lock-command" => match arg(n).and_then(KdlValue::as_string) {
                    Some(c) => self.idle.lock_command = Some(c.to_owned()),
                    None => self.reject(n, "idle lock-command needs a string argument"),
                },
                _ => self.unknown_key(n, "idle", "idle node"),
            }
            // ADR 0064, owner decision 2026-09-25: a refused lock setting no
            // longer refuses to start. Auto-lock stays at its default (off)
            // and the summary leads with that. Every refusal in `idle` counts,
            // a misspelt key included (`lock-timout-seconds`), except one about
            // DPMS (`dpms-*`), which is not a protection.
            if self.errors.len() > before && !name.starts_with("dpms") {
                self.fail_safe_last(FailSafe::AutoLockOff);
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
                "repeat-rate" => {
                    if !set_i32(&mut self.input.repeat_rate, n) {
                        self.reject(n, "repeat-rate expects an integer");
                    }
                }
                "repeat-delay" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (0..=5_000).contains(&v) => self.input.repeat_delay = v as i32,
                    _ => self.reject(n, "repeat-delay must be an integer 0..=5000"),
                },
                "touchpad" => {
                    for n in n.children().map(|c| c.nodes()).unwrap_or_default() {
                        let t = &mut self.input.touchpad;
                        match touchpad_key(n) {
                            Ok(TouchpadKey::NaturalScroll(b)) => t.natural_scroll = b,
                            Ok(TouchpadKey::TapToClick(b)) => t.tap_to_click = b,
                            Ok(TouchpadKey::TapAndDrag(b)) => t.tap_and_drag = b,
                            Ok(TouchpadKey::Dwt(b)) => t.dwt = b,
                            Ok(TouchpadKey::ClickMethod(m)) => t.click_method = m,
                            Ok(TouchpadKey::ScrollMethod(m)) => t.scroll_method = m,
                            Err(None) => self.unknown_key(n, "input.touchpad", "touchpad key"),
                            Err(Some(msg)) => self.reject(n, msg),
                        }
                    }
                }
                "device" => self.apply_input_device(n),
                "calibration" => self.reject(
                    n,
                    "calibration is per device: write it inside input { device \"<name>\" { … } }",
                ),
                _ => match pointer_key(n) {
                    Ok(PointerKey::AccelProfile(v)) => self.input.accel_profile = v,
                    Ok(PointerKey::AccelSpeed(v)) => self.input.accel_speed = v,
                    Ok(PointerKey::ScrollMethod(v)) => self.input.scroll_method = v,
                    // Only `calibration` yields a matrix, and it is refused above.
                    Ok(PointerKey::Calibration(_)) => {}
                    Err(None) => self.unknown_key(n, "input", "input key"),
                    Err(Some(msg)) => self.reject(n, msg),
                },
            }
        }
    }

    /// `input { device "<name>" { .. } }`: any subset of the pointer keys and
    /// of `touchpad { }`, plus `calibration`. A later block for the same name
    /// overrides key by key.
    fn apply_input_device(&mut self, node: &KdlNode) {
        let Some(name) = arg(node).and_then(KdlValue::as_string).filter(|s| !s.is_empty()) else {
            self.reject(
                node,
                "input.device needs a libinput device name, e.g. device \"ELAN0001:00 04F3:3140 Touchpad\" { … }",
            );
            return;
        };
        let mut d = self
            .input
            .devices
            .iter()
            .find(|d| d.name == name)
            .cloned()
            .unwrap_or_else(|| InputDevice {
                name: name.to_owned(),
                ..InputDevice::default()
            });
        for n in node.children().map(|c| c.nodes()).unwrap_or_default() {
            if n.name().value() == "touchpad" {
                for n in n.children().map(|c| c.nodes()).unwrap_or_default() {
                    let t = &mut d.touchpad;
                    match touchpad_key(n) {
                        Ok(TouchpadKey::NaturalScroll(b)) => t.natural_scroll = Some(b),
                        Ok(TouchpadKey::TapToClick(b)) => t.tap_to_click = Some(b),
                        Ok(TouchpadKey::TapAndDrag(b)) => t.tap_and_drag = Some(b),
                        Ok(TouchpadKey::Dwt(b)) => t.dwt = Some(b),
                        Ok(TouchpadKey::ClickMethod(m)) => t.click_method = Some(m),
                        Ok(TouchpadKey::ScrollMethod(m)) => t.scroll_method = Some(m),
                        Err(None) => self.reject(
                            n,
                            format!(
                                "unknown input.device.touchpad key {:?}; expected natural-scroll, \
                                 tap-to-click, tap-and-drag, dwt, click-method or scroll-method",
                                n.name().value()
                            ),
                        ),
                        Err(Some(msg)) => self.reject(n, msg),
                    }
                }
                continue;
            }
            match pointer_key(n) {
                Ok(PointerKey::AccelProfile(v)) => d.accel_profile = Some(v),
                Ok(PointerKey::AccelSpeed(v)) => d.accel_speed = Some(v),
                Ok(PointerKey::ScrollMethod(v)) => d.scroll_method = Some(v),
                Ok(PointerKey::Calibration(m)) => d.calibration = Some(m),
                Err(None) => self.reject(
                    n,
                    format!(
                        "unknown input.device key {:?}; expected accel-profile, accel-speed, \
                         scroll-method, calibration or touchpad",
                        n.name().value()
                    ),
                ),
                Err(Some(msg)) => self.reject(n, msg),
            }
        }
        match self.input.devices.iter_mut().find(|x| x.name == d.name) {
            Some(slot) => *slot = d,
            None => self.input.devices.push(d),
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
                    _ => self.reject(n, "decoration rounding must be an integer 0..=64"),
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
                    _ => self.reject(n, format!("{name} must be 0.0..=1.0")),
                },
                "blur" => self.apply_blur(n),
                "shadow" => self.apply_shadow(n),
                "glow" => self.apply_glow(n),
                _ => self.unknown_key(n, "decoration", "decoration key"),
            }
        }
    }

    fn apply_blur(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        // An explicit `mode` is the newer word and wins wherever it sits.
        let has_mode = children.nodes().iter().any(|n| n.name().value() == "mode");
        for n in children.nodes() {
            match n.name().value() {
                // Legacy (before `mode`): kept so an old file still loads;
                // `ec-ctl config migrate` rewrites it.
                "enabled" if has_mode => {}
                "enabled" => {
                    self.decoration.blur.mode = if arg(n).and_then(KdlValue::as_bool).unwrap_or(true) {
                        BlurMode::Blur
                    } else {
                        BlurMode::Off
                    };
                }
                "mode" => match arg(n).and_then(KdlValue::as_string).and_then(BlurMode::parse) {
                    Some(m) => self.decoration.blur.mode = m,
                    None => self.reject(n, "blur mode must be \"off\", \"blur\", \"frost\" or \"glass\""),
                },
                "glass" => self.apply_glass(n),
                "frost" => self.apply_frost(n),
                "size" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (1..=64).contains(&v) => self.decoration.blur.size = v as i32,
                    _ => self.reject(n, "blur size must be an integer 1..=64"),
                },
                "passes" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (1..=6).contains(&v) => self.decoration.blur.passes = v as i32,
                    _ => self.reject(n, "blur passes must be an integer 1..=6"),
                },
                _ => self.unknown_key(n, "decoration.blur", "blur key"),
            }
        }
    }

    fn apply_glass(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            let glass = &mut self.decoration.blur.glass;
            match n.name().value() {
                "refraction" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (0..=64).contains(&v) => glass.refraction = v as i32,
                    _ => self.reject(n, "glass refraction must be an integer 0..=64"),
                },
                "bevel" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (1..=128).contains(&v) => glass.bevel = v as i32,
                    _ => self.reject(n, "glass bevel must be an integer 1..=128"),
                },
                "dispersion" => match arg(n).and_then(as_f64) {
                    Some(v) if (0.0..=1.0).contains(&v) => glass.dispersion = v as f32,
                    _ => self.reject(n, "glass dispersion must be 0.0..=1.0"),
                },
                "rim" => match arg(n).and_then(as_f64) {
                    Some(v) if (0.0..=1.0).contains(&v) => glass.rim = v as f32,
                    _ => self.reject(n, "glass rim must be 0.0..=1.0"),
                },
                _ => self.unknown_key(n, "decoration.blur.glass", "glass key"),
            }
        }
    }

    fn apply_frost(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "tint" => match arg(n).and_then(KdlValue::as_string).and_then(parse_color) {
                    Some(c) => self.decoration.blur.frost.tint = c,
                    None => self.reject(n, "bad color for \"frost tint\""),
                },
                _ => self.unknown_key(n, "decoration.blur.frost", "frost key"),
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
                    _ => self.reject(n, "shadow range must be an integer 0..=128"),
                },
                _ => self.unknown_key(n, "decoration.shadow", "shadow key"),
            }
        }
    }

    fn apply_glow(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                name @ ("enabled" | "active" | "inactive") => {
                    let v = arg(n).and_then(KdlValue::as_bool).unwrap_or(true);
                    match name {
                        "enabled" => self.decoration.glow.enabled = v,
                        "active" => self.decoration.glow.active = v,
                        _ => self.decoration.glow.inactive = v,
                    }
                }
                "strength" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (0..=100).contains(&v) => self.decoration.glow.strength = v as i32,
                    _ => self.reject(n, "glow strength must be an integer 0..=100"),
                },
                _ => self.unknown_key(n, "decoration.glow", "glow key"),
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
                _ => self.unknown_key(n, "animations", "animations key"),
            }
        }
    }

    fn apply_animation(&mut self, node: &KdlNode) {
        let Some(name) = arg(node).and_then(KdlValue::as_string) else {
            self.reject(node, "animation node needs a name argument");
            return;
        };
        if schema::find(schema::ANIMATIONS, name).is_none() {
            self.reject(node, format!("unknown animation name {name:?}"));
            return;
        }
        let mut anim = Animation {
            name: name.to_owned(),
            duration_ms: schema::ANIMATION_DEFAULT_MS,
            curve: schema::ANIMATION_DEFAULT_CURVE.to_owned(),
        };
        for e in node.entries() {
            let Some(key) = e.name().map(|k| k.value().to_owned()) else {
                continue; // the positional name argument
            };
            match key.as_str() {
                "duration" => match parse_duration_ms(e.value()) {
                    Some(ms) if ms <= schema::ANIMATION_MAX_MS => anim.duration_ms = ms,
                    _ => {
                        self.reject(
                            node,
                            format!(
                                "animation duration must be <= 10s, e.g. \"150ms\" (name={})",
                                name
                            ),
                        );
                        return;
                    }
                },
                "curve" => match e.value().as_string() {
                    Some(c) if ANIMATION_CURVES.contains(&c) => anim.curve = c.to_owned(),
                    other => {
                        self.reject(
                            node,
                            format!("unknown animation curve (name={}, other={:?})", name, other),
                        );
                        return;
                    }
                },
                // Dropped whole, like a bad duration or curve: a rejected
                // node never applies in part (ADR 0064).
                other => {
                    self.reject(node, format!("unknown animation property {other:?}"));
                    return;
                }
            }
        }
        self.animations.curves.push(anim);
    }

    /// `windowrule "float" { app-id "pavucontrol|org.gnome.Calculator" }`
    /// (COMP-05 §4). An action or matcher this build cannot honour drops the
    /// whole rule with a warning, so a rule never applies in part.
    fn apply_windowrule(&mut self, node: &KdlNode) {
        let Some(action) = arg(node).and_then(KdlValue::as_string) else {
            self.reject(node, "windowrule needs an action argument");
            return;
        };
        let mut words = action.split_whitespace();
        let verb = words.next().unwrap_or_default();
        let param = words.next();
        // Ownership is per-action here, not per-node: `float` is cosmetic,
        // `sensitivity` and `no-agent` are the security surface. Refused in
        // both directions, same as a whole node.
        if let Some(owner) = schema::rule_owner(verb) {
            if !self.owned_here(node, &format!("windowrule {verb:?}"), owner) {
                return;
            }
        }
        let action = match (verb, param) {
            ("float", None) => RuleAction::Float,
            ("tile", None) => RuleAction::Tile,
            ("fullscreen", None) => RuleAction::Fullscreen,
            ("no-agent", None) => RuleAction::NoAgent,
            ("no-focus-steal", None) => RuleAction::NoFocusSteal,
            ("idle-inhibit", None) => RuleAction::IdleInhibit,
            ("size", Some(p)) => match parse_pair(p, 'x') {
                Some((w, h)) if w > 0 && h > 0 => RuleAction::Size(w, h),
                _ => {
                    self.reject(
                        node,
                        format!("windowrule size must be WxH in pixels (action={})", action),
                    );
                    return;
                }
            },
            ("position", Some(p)) => match parse_pair(p, ',') {
                Some((x, y)) => RuleAction::Position(x, y),
                None => {
                    self.reject(
                        node,
                        format!("windowrule position must be X,Y in pixels (action={})", action),
                    );
                    return;
                }
            },
            ("output", Some(p)) => RuleAction::Output(p.to_owned()),
            ("app-trust", Some("standard")) => RuleAction::Trust(AppTrust::Standard),
            ("app-trust", Some("trusted")) => RuleAction::Trust(AppTrust::Trusted),
            ("seat-compat", Some("lock")) => RuleAction::Seat(SeatCompat::Lock),
            ("seat-compat", Some("multi")) => RuleAction::Seat(SeatCompat::Multi),
            ("irreversible-capable", Some("true")) => RuleAction::IrreversibleCapable(true),
            ("irreversible-capable", Some("false")) => RuleAction::IrreversibleCapable(false),
            ("workspace", Some(p)) => match p.parse::<i32>() {
                Ok(n) if (1..=10).contains(&n) => RuleAction::Workspace(n),
                _ => {
                    self.reject(
                        node,
                        format!("windowrule workspace must be 1..=10 (action={})", action),
                    );
                    return;
                }
            },
            ("opacity", Some(p)) => match p.parse::<f32>() {
                Ok(v) if (0.0..=1.0).contains(&v) => RuleAction::Opacity(v),
                _ => {
                    self.reject(
                        node,
                        format!("windowrule opacity must be 0.0..=1.0 (action={})", action),
                    );
                    return;
                }
            },
            ("blur", Some(p)) => match BlurRule::parse(p) {
                Some(v) => RuleAction::Blur(v),
                None => {
                    self.reject(
                        node,
                        format!(
                            "windowrule blur must be true, false, off, blur, frost or glass (action={})",
                            action
                        ),
                    );
                    return;
                }
            },
            ("sensitivity", Some(p @ ("secret" | "private"))) => RuleAction::Sensitivity(p.to_owned()),
            ("sensitivity", Some(p)) => {
                // App-declared sensitivity may only raise a class, never lower.
                self.reject(
                    node,
                    format!("windowrule sensitivity may only raise (class={})", p),
                );
                return;
            }
            _ => {
                self.reject(
                    node,
                    format!("unimplemented windowrule action (action={})", action),
                );
                return;
            }
        };

        let mut matchers = Matchers::default();
        let Some(children) = node.children() else {
            self.reject(
                node,
                format!(
                    "windowrule with no matchers would hit every window (action={:?})",
                    action
                ),
            );
            return;
        };
        for n in children.nodes() {
            let name = n.name().value();
            if schema::find(schema::RULE_MATCHERS, name).is_none() {
                match schema::REFUSED_MATCHERS.iter().find(|(m, _)| *m == name) {
                    Some((_, why)) => self.reject(
                        n,
                        format!("refused windowrule matcher (matcher={}): {}", name, why),
                    ),
                    None => self.reject(n, format!("unimplemented windowrule matcher (matcher={})", name)),
                }
                return;
            }
            match name {
                "app-id" | "title" => {
                    let Some(pat) = arg(n).and_then(KdlValue::as_string).and_then(Pattern::parse) else {
                        self.reject(
                            n,
                            format!("matcher pattern is not a valid regex (matcher={})", name),
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
                        self.reject(n, "windowrule pid must be a positive integer");
                        return;
                    }
                },
                "xwayland" => matchers.xwayland = Some(arg(n).and_then(KdlValue::as_bool).unwrap_or(true)),
                "output" => match arg(n).and_then(KdlValue::as_string) {
                    Some(v) => matchers.output = Some(v.to_owned()),
                    None => {
                        self.reject(n, "windowrule output needs a name pattern");
                        return;
                    }
                },
                "cgroup" => match arg(n).and_then(KdlValue::as_string).and_then(Pattern::parse) {
                    Some(p) => matchers.cgroup = Some(p),
                    None => {
                        self.reject(n, "windowrule cgroup matcher is not a valid regex");
                        return;
                    }
                },
                "workspace" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (1..=10).contains(&v) => matchers.workspace = Some(v as i32),
                    _ => {
                        self.reject(n, "windowrule workspace matcher must be 1..=10");
                        return;
                    }
                },
                other => {
                    self.reject(n, format!("unimplemented windowrule matcher (matcher={})", other));
                    return;
                }
            }
        }
        if matchers.is_empty() {
            self.reject(
                node,
                format!(
                    "windowrule with no matchers would hit every window (action={:?})",
                    action
                ),
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
                    if !self.owned_here(n, "\"misc.scripted-input\"", schema::Owner::Policy) {
                        continue;
                    }
                    self.misc.scripted_input = arg(n).and_then(KdlValue::as_bool).unwrap_or(false);
                }
                "render-device" => match arg(n).and_then(KdlValue::as_string) {
                    Some("auto") => self.misc.render_device = None,
                    Some(v) => self.misc.render_device = Some(v.to_owned()),
                    // ADR 0064 / ADR 0033: dropping it would fall back to auto.
                    None => self.reject_fatal(n, "render-device needs a string"),
                },
                "terminal-command" => match arg(n).and_then(KdlValue::as_string) {
                    Some(c) => self.misc.terminal_command = Some(c.to_owned()),
                    None => self.reject(n, "terminal-command needs a string argument"),
                },
                // COMP-13 §1.1 (amended C-05): X11 is its own top-level node.
                // A refusal about X11 all the same: fail safe, as in
                // `apply_xwayland` (ADR 0064).
                "xwayland" => {
                    self.reject(
                        n,
                        "misc.xwayland is not a key; use the top-level `xwayland { enable #false }` node",
                    );
                    self.fail_safe_last(FailSafe::XwaylandOff);
                }
                _ => self.unknown_key(n, "misc", "misc key"),
            }
        }
    }

    fn apply_components(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            let name = n.name().value();
            let (catalog, slot) = match name {
                "bar" => (schema::COMPONENT_BARS, &mut self.components.bar),
                "launcher" => (schema::COMPONENT_LAUNCHERS, &mut self.components.launcher),
                "notifications" => (
                    schema::COMPONENT_NOTIFICATIONS,
                    &mut self.components.notifications,
                ),
                "control-center" => (
                    schema::COMPONENT_CONTROL_CENTERS,
                    &mut self.components.control_center,
                ),
                _ => {
                    self.unknown_key(n, "components", "components key");
                    continue;
                }
            };
            let value = arg(n).and_then(KdlValue::as_string).map(|v| {
                schema::LEGACY_COMPONENT_IDS
                    .iter()
                    .find_map(|&(old, new)| (old == v).then_some(new))
                    .unwrap_or(v)
            });
            match value {
                Some(v) if catalog.contains(&v) => *slot = v.to_owned(),
                other => {
                    let msg = format!(
                        "components.{name}: unknown value (other={other:?}); expected one of {}",
                        catalog.join(", ")
                    );
                    self.reject(n, msg);
                }
            }
        }
    }

    /// `annotations { accent "#f2c33c"; panel-tint "#17140fcc"; … }`. A bad
    /// colour is refused and that key keeps its value. `panel-tint` alpha is
    /// raised to [`PANEL_TINT_MIN_ALPHA`], not refused: the text on the panel
    /// is white, and on a white page a thinner tint leaves it unreadable.
    fn apply_annotations(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            let name = n.name().value();
            if self.annotations.slot_mut(name).is_none() {
                self.unknown_key(n, "annotations", "annotations key");
                continue;
            }
            match arg(n).and_then(KdlValue::as_string).and_then(parse_color) {
                Some(c) => {
                    if let Some(slot) = self.annotations.slot_mut(name) {
                        *slot = c;
                    }
                }
                None => self.reject(
                    n,
                    format!("bad color for \"annotations {name}\"; expected \"#rrggbb\" or \"#rrggbbaa\""),
                ),
            }
        }
        let tint = &mut self.annotations.panel_tint[3];
        *tint = tint.max(PANEL_TINT_MIN_ALPHA);
    }

    /// `oracle-eyes { model-command "claude"; timeout-ms 30000; … }`. A bad
    /// value is refused and that key keeps its value. Whether the model
    /// command may run is not decided here: `ec-abyss` withholds one that
    /// is neither the default nor approved (ADR 0067's path).
    fn apply_oracle_eyes(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "model-command" => {
                    let argv: Option<Vec<String>> = n
                        .entries()
                        .iter()
                        .map(|e| {
                            e.name()
                                .is_none()
                                .then(|| e.value().as_string().map(str::to_owned))
                                .flatten()
                        })
                        .collect();
                    match argv {
                        Some(argv) if argv.first().is_some_and(|a| !a.trim().is_empty()) => {
                            self.oracle_eyes.model_command = Some(argv)
                        }
                        _ => self.reject(
                            n,
                            "oracle-eyes model-command takes the program and its arguments as strings, \
                             e.g. model-command \"claude\"",
                        ),
                    }
                }
                "timeout-ms" => {
                    let (lo, hi) = ORACLE_EYES_TIMEOUT_MS;
                    if let Some(v) = self.int_in(n, lo, hi) {
                        self.oracle_eyes.timeout_ms = v;
                    }
                }
                "auto-interval-ms" => {
                    let (lo, hi) = ORACLE_EYES_AUTO_INTERVAL_MS;
                    if let Some(v) = self.int_in(n, lo, hi) {
                        self.oracle_eyes.auto_interval_ms = v;
                    }
                }
                "hold-ms" => {
                    let (lo, hi) = ORACLE_EYES_HOLD_MS;
                    if let Some(v) = self.int_in(n, lo, hi) {
                        self.oracle_eyes.hold_ms = v;
                    }
                }
                "debug" => {
                    if let Some(b) = self.flag(n) {
                        self.oracle_eyes.debug = b;
                    }
                }
                "bind" => self.apply_oracle_eyes_bind(n),
                _ => self.unknown_key(n, "oracle-eyes", "oracle-eyes key"),
            }
        }
    }

    fn apply_wallpaper(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            if n.name().value() == "output" {
                self.apply_wallpaper_output(n);
                continue;
            }
            match self.wallpaper_key(n, "wallpaper") {
                Some(WallpaperKey::Path(p)) => self.wallpaper.path = Some(p),
                Some(WallpaperKey::Mode(m)) => self.wallpaper.mode = m,
                Some(WallpaperKey::Color(c)) => self.wallpaper.color = c,
                None => {}
            }
        }
    }

    /// `wallpaper { output "<name>" { .. } }`: any subset of the global keys.
    /// A later block for the same name overrides key by key.
    fn apply_wallpaper_output(&mut self, node: &KdlNode) {
        let Some(name) = arg(node).and_then(KdlValue::as_string) else {
            self.reject(
                node,
                "wallpaper.output needs an output name, e.g. output \"DP-1\" { … }",
            );
            return;
        };
        let mut o = self
            .wallpaper
            .outputs
            .iter()
            .find(|o| o.name == name)
            .cloned()
            .unwrap_or_else(|| WallpaperOutput {
                name: name.to_owned(),
                ..WallpaperOutput::default()
            });
        for n in node.children().map(|c| c.nodes()).unwrap_or_default() {
            match self.wallpaper_key(n, "wallpaper.output") {
                Some(WallpaperKey::Path(p)) => o.path = Some(p),
                Some(WallpaperKey::Mode(m)) => o.mode = Some(m),
                Some(WallpaperKey::Color(c)) => o.color = Some(c),
                None => {}
            }
        }
        match self.wallpaper.outputs.iter_mut().find(|x| x.name == o.name) {
            Some(slot) => *slot = o,
            None => self.wallpaper.outputs.push(o),
        }
    }

    /// One `path` / `mode` / `color` node, validated; `None` after a refusal.
    fn wallpaper_key(&mut self, n: &KdlNode, prefix: &str) -> Option<WallpaperKey> {
        let name = n.name().value();
        let v = arg(n).and_then(KdlValue::as_string);
        let parsed = match name {
            "path" => v.map(|p| WallpaperKey::Path(p.to_owned())),
            "mode" => v
                .filter(|m| schema::WALLPAPER_MODES.contains(m))
                .map(|m| WallpaperKey::Mode(m.to_owned())),
            "color" => v.and_then(parse_color).map(WallpaperKey::Color),
            _ => {
                if prefix == "wallpaper" {
                    self.unknown_key(n, prefix, "wallpaper key");
                } else {
                    self.reject(
                        n,
                        format!("unknown wallpaper.output key {name:?}; expected path, mode or color"),
                    );
                }
                return None;
            }
        };
        if parsed.is_none() {
            let got = arg(n).map_or_else(|| "nothing".to_owned(), |v| v.to_string());
            let msg = match name {
                "path" => format!("{prefix}.path needs a string, got {got}"),
                "mode" => format!(
                    "{prefix}.mode: unknown value {got}; expected one of {}",
                    schema::WALLPAPER_MODES.join(", ")
                ),
                _ => format!("{prefix}.color: {got} is not a colour; expected \"#rrggbb\""),
            };
            self.reject(n, msg);
        }
        parsed
    }

    fn apply_setup(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "profile" => match arg(n).and_then(KdlValue::as_string) {
                    Some(v) if schema::SETUP_PROFILES.contains(&v) => self.setup.profile = v.to_owned(),
                    other => self.reject(
                        n,
                        format!(
                            "unknown setup profile (other={other:?}); expected one of {}",
                            schema::SETUP_PROFILES.join(", ")
                        ),
                    ),
                },
                "complete" => match arg(n).and_then(KdlValue::as_bool) {
                    Some(b) => self.setup.complete = b,
                    None => self.reject(n, "complete expects #true or #false"),
                },
                "pending-preset" => match arg(n).and_then(KdlValue::as_bool) {
                    Some(b) => self.setup.pending_preset = b,
                    None => self.reject(n, "pending-preset expects #true or #false"),
                },
                _ => self.unknown_key(n, "setup", "setup key"),
            }
        }
    }

    fn apply_launcher(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "style" => match arg(n).and_then(KdlValue::as_string) {
                    Some("centered") => self.launcher.style = LauncherStyle::Centered,
                    Some("menu") => self.launcher.style = LauncherStyle::Menu,
                    other => self.reject(
                        n,
                        format!("launcher style must be \"centered\" or \"menu\", keeping default (other={other:?})"),
                    ),
                },
                "centered" => self.apply_launcher_centered(n),
                "menu" => self.apply_launcher_menu(n),
                "search" => self.apply_launcher_search(n),
                "bind" => self.apply_launcher_bind(n),
                _ => self.unknown_key(n, "launcher", "launcher key"),
            }
        }
    }

    fn apply_launcher_centered(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "width" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (360..=1200).contains(&v) => self.launcher.centered.width = v as u32,
                    _ => self.reject(n, "launcher centered width must be an integer 360..=1200"),
                },
                "max-rows" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (3..=16).contains(&v) => self.launcher.centered.max_rows = v as u32,
                    _ => self.reject(n, "launcher centered max-rows must be an integer 3..=16"),
                },
                "anchor" => match arg(n)
                    .and_then(KdlValue::as_string)
                    .and_then(LauncherAnchor::parse)
                {
                    Some(a) => self.launcher.centered.anchor = a,
                    None => self.reject(
                        n,
                        format!(
                            "launcher centered anchor must be one of {}",
                            schema::LAUNCHER_ANCHORS.join(", ")
                        ),
                    ),
                },
                _ => self.unknown_key(n, "launcher.centered", "launcher centered key"),
            }
        }
    }

    fn apply_launcher_menu(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "max-rows" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (3..=16).contains(&v) => self.launcher.menu.max_rows = v as u32,
                    _ => self.reject(n, "launcher menu max-rows must be an integer 3..=16"),
                },
                _ => self.unknown_key(n, "launcher.menu", "launcher menu key"),
            }
        }
    }

    fn apply_launcher_search(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            let name = n.name().value();
            if !matches!(
                name,
                "path-binaries" | "terminal-apps" | "match-descriptions" | "frecency"
            ) {
                self.unknown_key(n, "launcher.search", "launcher search key");
                continue;
            }
            // A bare node is on, as for the other flags.
            let value = match arg(n).map(KdlValue::as_bool) {
                None => true,
                Some(Some(b)) => b,
                Some(None) => {
                    self.reject(n, format!("launcher search {name} expects #true or #false"));
                    continue;
                }
            };
            let search = &mut self.launcher.search;
            match name {
                "path-binaries" => search.path_binaries = value,
                "terminal-apps" => search.terminal_apps = value,
                "frecency" => search.frecency = value,
                _ => search.match_descriptions = value,
            }
        }
    }

    fn apply_oracle_eyes_bind(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            let name = n.name().value();
            if !matches!(name, "select" | "dismiss" | "expand" | "auto-toggle") {
                self.unknown_key(n, "oracle-eyes.bind", "oracle-eyes bind key");
                continue;
            }
            let Some(text) = arg(n).and_then(KdlValue::as_string) else {
                self.reject(
                    n,
                    format!("oracle-eyes bind {name} must be a chord string like \"Super+A\" or \"none\""),
                );
                continue;
            };
            match parse_chord(text) {
                Ok(chord) => {
                    let parsed = LauncherChord {
                        text: text.to_owned(),
                        chord,
                    };
                    let b = &mut self.oracle_eyes.bind;
                    match name {
                        "select" => b.select = parsed,
                        "dismiss" => b.dismiss = parsed,
                        "expand" => b.expand = parsed,
                        _ => b.auto_toggle = parsed,
                    }
                }
                Err(e) => {
                    let message = format!("oracle-eyes bind {name}: {e}");
                    self.reject(n, message);
                }
            }
        }
    }

    fn apply_launcher_bind(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            let name = n.name().value();
            if !matches!(name, "open" | "run") {
                self.unknown_key(n, "launcher.bind", "launcher bind key");
                continue;
            }
            let Some(text) = arg(n).and_then(KdlValue::as_string) else {
                self.reject(
                    n,
                    format!("launcher bind {name} must be a chord string like \"Super+R\" or \"none\""),
                );
                continue;
            };
            match parse_chord(text) {
                Ok(chord) => {
                    let parsed = LauncherChord {
                        text: text.to_owned(),
                        chord,
                    };
                    if name == "open" {
                        self.launcher.bind.open = parsed;
                    } else {
                        self.launcher.bind.run = parsed;
                    }
                }
                Err(e) => {
                    let message = format!("launcher bind {name}: {e}");
                    self.reject(n, message);
                }
            }
        }
    }

    fn apply_ui(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            match n.name().value() {
                "show-key-hints" => match arg(n).map(KdlValue::as_bool) {
                    None => self.ui.show_key_hints = true,
                    Some(Some(b)) => self.ui.show_key_hints = b,
                    Some(None) => self.reject(n, "ui show-key-hints expects #true or #false"),
                },
                _ => self.unknown_key(n, "ui", "ui key"),
            }
        }
    }

    fn apply_settings(&mut self, node: &KdlNode) {
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            if n.name().value() != "search" {
                self.unknown_key(n, "settings", "settings key");
                continue;
            }
            let Some(inner) = n.children() else { continue };
            for k in inner.nodes() {
                match k.name().value() {
                    "frecency" => match arg(k).map(KdlValue::as_bool) {
                        None => self.settings.search_frecency = true,
                        Some(Some(b)) => self.settings.search_frecency = b,
                        Some(None) => self.reject(k, "settings search frecency expects #true or #false"),
                    },
                    _ => self.unknown_key(k, "settings.search", "settings search key"),
                }
            }
        }
    }

    fn apply_workspace(&mut self, node: &KdlNode) {
        let Some(idx) = arg(node).and_then(KdlValue::as_integer) else {
            self.reject(node, "workspace node needs a number argument");
            return;
        };
        if !(1..=10).contains(&idx) {
            self.reject(node, format!("workspace out of range 1..=10 (idx={})", idx));
            return;
        }
        let Some(children) = node.children() else { return };
        for n in children.nodes() {
            if n.name().value() == "layout" {
                let slot = &mut self.workspace_layout[idx as usize - 1];
                match arg(n).and_then(KdlValue::as_string) {
                    Some("radiant") => *slot = Some(LayoutKind::Radiant),
                    Some("dwindle") => *slot = Some(LayoutKind::Dwindle),
                    Some("master") => *slot = Some(LayoutKind::Master),
                    other => self.reject(n, format!("unknown workspace layout {other:?}")),
                }
            }
        }
    }

    fn apply_output(&mut self, node: &KdlNode) {
        let Some(pattern) = arg(node).and_then(KdlValue::as_string).map(str::to_owned) else {
            self.reject(node, "output node needs a name pattern argument");
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
            if schema::find(schema::OUTPUT_KEYS, name).is_none() {
                self.reject(n, format!("unknown output key {name:?}"));
                continue;
            }
            match name {
                "mode" => match arg(n)
                    .and_then(KdlValue::as_string)
                    .and_then(crate::outputs::parse_mode)
                {
                    Some(m) => rule.mode = Some(m),
                    None => self.reject(n, "bad output mode, expected \"1920x1080@60\""),
                },
                "position" => {
                    let a = n.entries();
                    match (
                        a.first().and_then(|e| e.value().as_integer()),
                        a.get(1).and_then(|e| e.value().as_integer()),
                    ) {
                        (Some(x), Some(y)) => rule.position = Some((x as i32, y as i32)),
                        _ => self.reject(n, "output position takes two integers"),
                    }
                }
                "scale" => match arg(n).and_then(as_f64) {
                    Some(s) if s > 0.0 => rule.scale = Some(s),
                    _ => self.reject(n, "output scale must be a positive number"),
                },
                "transform" => match arg(n).and_then(KdlValue::as_string) {
                    Some(t) if crate::outputs::is_transform_name(t) => rule.transform = Some(t.to_owned()),
                    other => self.reject(n, format!("unknown output transform {other:?}")),
                },
                "enabled" | "disabled" => match arg(n).and_then(KdlValue::as_bool) {
                    Some(v) => rule.enabled = Some(v == (name == "enabled")),
                    None => rule.enabled = Some(name == "enabled"),
                },
                "lid-close" => match arg(n).and_then(KdlValue::as_string) {
                    Some(v) if schema::LID_CLOSE.contains(&v) => rule.lid_close = Some(v.to_owned()),
                    _ => self.reject(n, "output lid-close must be off, suspend or ignore"),
                },
                // `overscan 30` for all four edges, or any subset of
                // `overscan top=20 bottom=20 left=40 right=40`.
                "overscan" => {
                    let has_value = n.entries().iter().any(|e| e.value().as_integer().is_some());
                    let negative = n
                        .entries()
                        .iter()
                        .any(|e| e.value().as_integer().is_some_and(|i| i < 0));
                    if !has_value || negative {
                        self.reject(
                            n,
                            "output overscan takes non-negative pixels: `overscan 30` or \
                             `overscan top=20 left=40`",
                        );
                    } else {
                        rule.overscan = Some(crate::outputs::overscan_of(n));
                    }
                }
                "vrr" | "adaptive-sync" => rule.vrr = arg(n).and_then(KdlValue::as_bool).or(Some(true)),
                "number" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (1..=255).contains(&v) => rule.number = Some(v as u8),
                    _ => self.reject(n, "output number must be between 1 and 255"),
                },
                other => self.reject(n, format!("unknown output key {other:?}")),
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
            out.overscan = r.overscan.or(out.overscan);
            out.number = r.number.or(out.number);
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

    /// `mods` is the held modifier set (a binding matches on equality).
    pub fn action_for(&self, mods: &Mods, key: Keysym) -> Option<&Action> {
        self.binds
            .iter()
            .find(|b| b.key == key && b.mods == *mods)
            .map(|b| &b.action)
    }

    /// Whether any `fingers`-finger swipe is bound. Asked at swipe begin,
    /// before the direction is known, to decide who owns the whole swipe.
    pub fn gesture_bound(&self, fingers: u32) -> bool {
        self.gesture_binds.iter().any(|g| g.fingers == fingers)
    }

    /// Whether a `fingers`-finger touchpad drag with exactly `mods` held moves
    /// a window. Asked once per gesture, at its begin; allocates nothing.
    pub fn drag_gesture_bound(&self, fingers: u32, mods: &Mods) -> bool {
        self.drag_gestures
            .iter()
            .any(|d| d.fingers == fingers && d.mods == *mods)
    }

    /// The mouse binding for `button` pressed with exactly `mods` held.
    /// Called on every button press, so it borrows and allocates nothing.
    pub fn mouse_bind_for(&self, mods: &Mods, button: u32) -> Option<MouseAction> {
        self.mouse_binds
            .iter()
            .find(|b| b.button.code() == button && b.mods == *mods)
            .map(|b| b.action)
    }

    pub fn gesture_for(&self, fingers: u32, direction: Direction) -> Option<&Action> {
        self.gesture_binds
            .iter()
            .find(|g| g.fingers == fingers && g.direction == direction)
            .map(|g| &g.action)
    }
}

/// Every file that may contribute, in apply order.
///
/// Each directory contributes its `policy.kdl` after its `abyss.kdl`, so a
/// policy-owned key set in the wrong file is refused rather than quietly
/// shadowed. There are deliberately no `policy.d/` drop-ins: the security
/// surface is one file per directory, so "what is the policy here" has one
/// answer a human can read.
/// `$XDG_CONFIG_HOME` (or `$HOME/.config`), the tier a normal user can write.
/// `None` when neither is set, which is the case for a daemon with no home.
#[must_use]
pub fn user_config_base() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
}

fn search_path() -> Vec<Source> {
    let abyss = |p: PathBuf| Source {
        path: p,
        owner: schema::Owner::Abyss,
    };
    let policy = |p: PathBuf| Source {
        path: p,
        owner: schema::Owner::Policy,
    };
    let mut out = vec![
        abyss(PathBuf::from("/etc/eclipse/abyss.kdl")),
        policy(PathBuf::from("/etc/eclipse/policy.kdl")),
    ];
    let Some(base) = user_config_base() else {
        return out;
    };
    out.push(abyss(base.join("eclipse/abyss.kdl")));
    if let Ok(dir) = std::fs::read_dir(base.join("eclipse/abyss.d")) {
        let mut drop_ins: Vec<PathBuf> = dir
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "kdl"))
            .collect();
        drop_ins.sort();
        out.extend(drop_ins.into_iter().map(abyss));
    }
    out.push(policy(base.join("eclipse/policy.kdl")));
    // Compatibility with the path used by the M2 task brief.
    out.push(abyss(base.join("abyss/config.kdl")));
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

/// `false` when the node carries no integer, so the caller can reject it.
#[must_use]
fn set_i32(slot: &mut i32, node: &KdlNode) -> bool {
    match arg(node).and_then(KdlValue::as_integer) {
        Some(v) => {
            *slot = v.clamp(0, 512) as i32;
            true
        }
        None => false,
    }
}

/// As [`set_i32`], for a slot that defaults to mirroring another key rather
/// than to a literal (`gaps-in-vertical`, `gaps-out-vertical`).
#[must_use]
fn set_opt_i32(slot: &mut Option<i32>, node: &KdlNode) -> bool {
    match arg(node).and_then(KdlValue::as_integer) {
        Some(v) => {
            *slot = Some(v.clamp(0, 512) as i32);
            true
        }
        None => false,
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

/// A launcher chord as one string (`launcher.bind.*`): `"Super+R"`,
/// `"Super+Shift+Return"`, or `"none"` (or empty) for no chord. The last
/// `+`- or space-separated part is the xkb keysym and the rest are modifiers
/// as `bind` spells them. A single letter is case-folded, so `"Super+R"` is
/// the chord `bind "SUPER" "r"` makes. The reserved chords are refused as
/// `parse_bind` refuses them.
fn parse_chord(text: &str) -> Result<Option<(Mods, Keysym)>, String> {
    let text = text.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    let (mods, key) = match text.rfind(['+', ' ']) {
        Some(i) => (&text[..i], &text[i + 1..]),
        None => ("", text),
    };
    if key.is_empty() {
        return Err(format!("{text:?} names no key"));
    }
    let mods = parse_mods(mods)?;
    let key = if key.len() == 1 && key.as_bytes()[0].is_ascii_alphabetic() {
        parse_keysym(&key.to_ascii_lowercase())?
    } else {
        parse_keysym(key)?
    };
    if mods == m(true, false, false, false) && key == Keysym::Escape {
        return Err("Super+Escape is reserved (COMP-04 §6) and cannot be bound".into());
    }
    if mods == m(true, false, false, false) && key == Keysym::space {
        return Err("Super+space is reserved (COMP-13 §1.1) and cannot be bound".into());
    }
    Ok(Some((mods, key)))
}

/// `bind "SUPER" "Return" { spawn "foot"; }`
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

/// A parsed `gesture` node: a swipe binding, or a drag for a finger count
/// with its modifiers (`None` switches that finger count's drag off).
enum ParsedGesture {
    Swipe(GestureBind),
    Drag(u32, Option<Mods>),
}

/// `gesture "swipe" 3 "left" { workspace-next; }` or a drag (see
/// [`parse_drag_gesture`]).
fn parse_gesture(node: &KdlNode) -> Result<ParsedGesture, String> {
    let a = args(node);
    if a.first().and_then(|k| k.as_string()) == Some("drag") {
        return parse_drag_gesture(node);
    }
    let [kind, fingers, direction] = a[..] else {
        return Err("gesture takes \"swipe\" fingers direction { action }".into());
    };
    if kind.as_string() != Some("swipe") {
        return Err("only \"swipe\" and \"drag\" gestures can be bound".into());
    }
    let fingers = match fingers.as_integer() {
        Some(n @ (3 | 4)) => n as u32,
        _ => return Err("gesture fingers must be 3 or 4".into()),
    };
    let direction = match direction.as_string() {
        Some("left") => Direction::Left,
        Some("right") => Direction::Right,
        Some("up") => Direction::Up,
        Some("down") => Direction::Down,
        _ => return Err("gesture direction must be left, right, up or down".into()),
    };
    let children = node.children().ok_or("gesture needs an action block")?;
    let action_node = children.nodes().first().ok_or("gesture action block is empty")?;
    let action = parse_action(action_node)?;
    Ok(ParsedGesture::Swipe(GestureBind {
        fingers,
        direction,
        action,
    }))
}

/// `gesture "drag" 2 "Super" { move-window; }`, or `gesture "drag" 2 { none; }`
/// (modifiers optional) to switch that finger count's drag off.
fn parse_drag_gesture(node: &KdlNode) -> Result<ParsedGesture, String> {
    let a = args(node);
    let (fingers, mods) = match a[..] {
        [_, fingers] => (fingers, None),
        [_, fingers, mods] => (fingers, Some(mods)),
        _ => return Err("gesture takes \"drag\" fingers \"modifiers\" { move-window; }".into()),
    };
    let fingers = match fingers.as_integer() {
        Some(n @ 2..=4) => n as u32,
        _ => return Err("drag gesture fingers must be 2, 3 or 4".into()),
    };
    let mods = match mods {
        Some(m) => parse_mods(m.as_string().ok_or("modifiers must be a string")?)?,
        None => Mods::default(),
    };
    let children = node.children().ok_or("gesture needs an action block")?;
    let action_node = children.nodes().first().ok_or("gesture action block is empty")?;
    match action_node.name().value() {
        "none" => Ok(ParsedGesture::Drag(fingers, None)),
        // As with mousebind: a bare drag would take every scroll from the app.
        "move-window" if mods == Mods::default() => Err("drag gesture needs at least one modifier".into()),
        "move-window" => Ok(ParsedGesture::Drag(fingers, Some(mods))),
        other => Err(format!("unknown drag gesture action '{other}'")),
    }
}

/// `mousebind "Alt" "left" { move-window; }`
fn parse_mousebind(node: &KdlNode) -> Result<MouseBind, String> {
    let a = args(node);
    let [mods, button] = a[..] else {
        return Err("mousebind takes \"modifiers\" \"button\" { action }".into());
    };
    let mods = parse_mods(mods.as_string().ok_or("modifiers must be a string")?)?;
    let button = match button.as_string() {
        Some("left") => MouseButton::Left,
        Some("right") => MouseButton::Right,
        Some("middle") => MouseButton::Middle,
        _ => return Err("mousebind button must be left, right or middle".into()),
    };
    let children = node.children().ok_or("mousebind needs an action block")?;
    let action_node = children
        .nodes()
        .first()
        .ok_or("mousebind action block is empty")?;
    let action = match action_node.name().value() {
        "move-window" => MouseAction::MoveWindow,
        "resize-window" => MouseAction::ResizeWindow,
        other => return Err(format!("unknown mousebind action '{other}'")),
    };
    // A bare click is not a binding: it would take every press away from the
    // client under it.
    if mods == Mods::default() {
        return Err("mousebind needs at least one modifier".into());
    }
    Ok(MouseBind { mods, button, action })
}

fn parse_action(node: &KdlNode) -> Result<Action, String> {
    let a = args(node);
    let text = || a.first().and_then(|v| v.as_string()).map(str::to_owned);
    let num = || a.first().and_then(|v| v.as_integer());
    let name = node.name().value();
    if schema::find(schema::BIND_ACTIONS, name).is_none() {
        return Err(format!("unknown action '{name}'"));
    }
    Ok(match name {
        "spawn" | "exec" => Action::Spawn(text().ok_or("spawn needs a command string")?),
        "close-window" | "killactive" => Action::Close,
        "toggle-floating" => Action::ToggleFloating,
        "minimize" => Action::Minimize,
        "unminimize" | "restore" => Action::Unminimize,
        "toggle-layout" => Action::ToggleLayout,
        "focus-left" => Action::Focus(Direction::Left),
        "focus-right" => Action::Focus(Direction::Right),
        "focus-up" => Action::Focus(Direction::Up),
        "focus-down" => Action::Focus(Direction::Down),
        "move-left" => Action::Move(Direction::Left),
        "move-right" => Action::Move(Direction::Right),
        "move-up" => Action::Move(Direction::Up),
        "move-down" => Action::Move(Direction::Down),
        "priority-up" => Action::Priority(1),
        "priority-down" => Action::Priority(-1),
        "workspace" => Action::SwitchWorkspace(workspace_arg(num())?),
        "workspace-next" => Action::WorkspaceNext,
        "workspace-prev" => Action::WorkspacePrev,
        "move-to-workspace" => Action::MoveToWorkspace(workspace_arg(num())?),
        "move-to-output" => Action::MoveToOutputWorkspace(output_number_arg(num())?),
        "agent-override" => Action::AgentOverride,
        "agent-attention" => Action::AgentAttention,
        "annotation-select" => Action::AnnotationSelect,
        "annotation-dismiss" => Action::AnnotationDismiss,
        "annotation-expand" => Action::AnnotationExpand,
        "annotation-auto-toggle" => Action::AnnotationAutoToggle,
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

/// Display number for `move-to-output` (ADR 0049). Wider range than
/// `workspace_arg`'s 1..=10: this matches an output's configured/assigned
/// `number` (also 1..=255, see `OutputRule::number`), not a workspace index —
/// the default binds only ever go up to 10, but a config override is free to
/// name a higher display number.
fn output_number_arg(n: Option<i128>) -> Result<u8, String> {
    match n {
        Some(v) if (1..=255).contains(&v) => Ok(v as u8),
        _ => Err("output number must be 1..=255".into()),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    // The held-modifier set the compositor passes in; same shape as the
    // smithay `ModifiersState` these tests used to build.
    use crate::input::Mods as ModifiersState;

    fn abyss_src(p: &str) -> Source {
        Source {
            path: PathBuf::from(p),
            owner: schema::Owner::Abyss,
        }
    }

    fn policy_src(p: &str) -> Source {
        Source {
            path: PathBuf::from(p),
            owner: schema::Owner::Policy,
        }
    }

    /// COMP-13 §1.3: the security surface lives in `policy.kdl` and nowhere
    /// else. A policy key written into `abyss.kdl` is refused rather than
    /// honoured, because `abyss.kdl` is what the config GUI may write.
    #[test]
    fn ownership_is_refused_in_both_directions() {
        fn err(src: Source, text: &str) -> String {
            let doc: KdlDocument = text.parse().unwrap();
            let mut cfg = Config {
                cur: Some((src, text.to_owned())),
                ..Config::default()
            };
            cfg.apply(&doc, &mut Vec::new());
            assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
            cfg.errors[0].message.clone()
        }

        // Whole node, policy-owned, in the wrong file.
        let m = err(abyss_src("a.kdl"), "capture {\n    allow #true\n}\n");
        assert!(m.contains("policy.kdl"), "{m}");

        // Whole node, abyss-owned, in policy.kdl.
        let m = err(policy_src("p.kdl"), "general {\n    gaps-in 4\n}\n");
        assert!(m.contains("abyss.kdl"), "{m}");

        // `misc` is mixed: the policy key is refused, the abyss key beside it
        // is not.
        let m = err(
            abyss_src("a.kdl"),
            "misc {\n    render-device \"/dev/dri/card0\"\n    scripted-input #true\n}\n",
        );
        assert!(
            m.contains("misc.scripted-input") && m.contains("policy.kdl"),
            "{m}"
        );

        // `windowrule` is decided per action, not per node.
        let m = err(abyss_src("a.kdl"), "windowrule \"no-agent\" app-id=\"x\"\n");
        assert!(m.contains("no-agent") && m.contains("policy.kdl"), "{m}");
        let m = err(policy_src("p.kdl"), "windowrule \"float\" app-id=\"x\"\n");
        assert!(m.contains("float") && m.contains("abyss.kdl"), "{m}");
    }

    /// Each directory contributes its policy file after its abyss file, so a
    /// later abyss.d drop-in can never shadow policy.
    #[test]
    fn the_search_path_pairs_each_directory() {
        let path = search_path();
        let names: Vec<String> = path
            .iter()
            .map(|s| {
                format!(
                    "{}:{}",
                    s.path.display(),
                    if s.owner == schema::Owner::Policy {
                        "p"
                    } else {
                        "a"
                    }
                )
            })
            .collect();
        assert!(names[0].ends_with("/etc/eclipse/abyss.kdl:a"), "{names:?}");
        assert!(names[1].ends_with("/etc/eclipse/policy.kdl:p"), "{names:?}");
        let policies: Vec<_> = path.iter().filter(|s| s.owner == schema::Owner::Policy).collect();
        assert!(policies
            .iter()
            .all(|s| s.path.file_name().unwrap() == "policy.kdl"));
    }

    /// COMP-13 §1.2: validation is total — an unknown key is an error, not a
    /// warning, and it carries the position of the offending token.
    #[test]
    fn unknown_keys_are_errors_with_a_position() {
        let text = "general {\n    gaps-in 4\n    gaps-inn 4\n}\n";
        let doc: KdlDocument = text.parse().unwrap();
        let mut cfg = Config {
            cur: Some((abyss_src("/etc/eclipse/abyss.kdl"), text.to_owned())),
            ..Config::default()
        };
        cfg.apply(&doc, &mut Vec::new());
        assert_eq!(cfg.errors.len(), 1);
        let e = &cfg.errors[0];
        assert_eq!((e.line, e.col), (3, 5));
        assert!(e.message.contains("gaps-inn"), "{}", e.message);
        assert!(e.to_string().starts_with("/etc/eclipse/abyss.kdl:3:5: "));
        // The caret run underlines the offending token, on the right line.
        assert_eq!(
            e.to_string().lines().skip(1).collect::<Vec<_>>(),
            ["  |", "3 |     gaps-inn 4", "  |     ^^^^^^^^"]
        );
    }

    /// COMP-17 §2/§2.2, ADR 0062: `mode` and `components` parse, default to
    /// Standard's values, and refuse an unknown id naming the key.
    #[test]
    fn mode_and_components_parse_default_and_refuse() {
        let d = Config::default();
        assert_eq!(d.mode, Mode::Hybrid);
        assert_eq!(
            (
                d.components.bar.as_str(),
                d.components.launcher.as_str(),
                d.components.notifications.as_str(),
                d.components.control_center.as_str()
            ),
            ("ec-hyperion-bar", "ec-launcher", "ec-toasts", "ec-center")
        );
        for (m, want) in [("wm", Mode::Wm), ("hybrid", Mode::Hybrid), ("de", Mode::De)] {
            let doc: KdlDocument = format!("mode \"{m}\"\n").parse().unwrap();
            let mut cfg = Config::default();
            cfg.apply(&doc, &mut Vec::new());
            assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
            assert_eq!(cfg.mode, want);
        }
        let doc: KdlDocument = "components {\n    bar \"waybar\"\n    launcher \"fuzzel\"\n    notifications \"mako\"\n    control-center \"none\"\n}\n"
            .parse()
            .unwrap();
        let mut cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert_eq!(cfg.components.bar, "waybar");
        assert_eq!(cfg.components.launcher, "fuzzel");
        assert_eq!(cfg.components.notifications, "mako");
        assert_eq!(cfg.components.control_center, "none");
        assert_eq!(
            schema::get(&cfg, "components.bar"),
            Some(schema::Value::Str("waybar".into()))
        );

        // Pre-`ec-` ids load as their new names (ADR 0070).
        let doc: KdlDocument = "components {\n    bar \"hyperion\"\n    launcher \"eclipse-launcher\"\n    notifications \"eclipse-toasts\"\n    control-center \"eclipse-center\"\n}\n"
            .parse()
            .unwrap();
        let mut cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert_eq!(
            (
                cfg.components.bar.as_str(),
                cfg.components.launcher.as_str(),
                cfg.components.notifications.as_str(),
                cfg.components.control_center.as_str()
            ),
            ("ec-hyperion-bar", "ec-launcher", "ec-toasts", "ec-center")
        );

        let text = "mode \"tiling\"\ncomponents {\n    bar \"polybar\"\n    launcher \"ec-toasts\"\n    control-center \"mako\"\n    notifications 3\n    dock \"x\"\n}\n";
        let doc: KdlDocument = text.parse().unwrap();
        let mut cfg = Config {
            cur: Some((abyss_src("/etc/eclipse/abyss.kdl"), text.to_owned())),
            ..Config::default()
        };
        cfg.apply(&doc, &mut Vec::new());
        assert_eq!(cfg.errors.len(), 6, "{:?}", cfg.errors);
        assert!(
            cfg.errors[0].message.contains("mode"),
            "{}",
            cfg.errors[0].message
        );
        assert!(cfg.errors[1].message.contains("components.bar"));
        assert!(cfg.errors[2].message.contains("components.launcher"));
        assert!(cfg.errors[3].message.contains("components.control-center"));
        assert!(cfg.errors[4].message.contains("components.notifications"));
        assert_eq!(cfg.mode, Mode::Hybrid, "a refused value must not land");
        assert_eq!(cfg.components.bar, "ec-hyperion-bar");

        // abyss.kdl only.
        let text = "mode \"wm\"\n";
        let doc: KdlDocument = text.parse().unwrap();
        let mut cfg = Config {
            cur: Some((policy_src("/etc/eclipse/policy.kdl"), text.to_owned())),
            ..Config::default()
        };
        cfg.apply(&doc, &mut Vec::new());
        assert_eq!(cfg.errors.len(), 1);
        assert_eq!(cfg.mode, Mode::Hybrid);
    }

    /// Both keys hot-reload (COMP-17 §2, §2.2): `reload` is `Live`.
    #[test]
    fn mode_and_components_are_live() {
        for p in [
            "mode",
            "components.bar",
            "components.launcher",
            "components.notifications",
            "components.control-center",
        ] {
            let k = schema::get_key(p).expect(p);
            assert_eq!(k.reload, schema::Reload::Live, "{p}");
            assert_eq!(k.owner, schema::Owner::Abyss, "{p}");
        }
    }

    fn wallpaper_cfg(text: &str) -> Config {
        let doc: KdlDocument = text.parse().unwrap();
        let mut cfg = Config {
            cur: Some((abyss_src("/etc/eclipse/abyss.kdl"), text.to_owned())),
            ..Config::default()
        };
        cfg.apply(&doc, &mut Vec::new());
        cfg
    }

    /// No `wallpaper` node: no image, `fill`, `#0b0906`, no overrides.
    #[test]
    fn wallpaper_defaults_when_absent() {
        let cfg = wallpaper_cfg("general { gaps-in 4; }\n");
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert_eq!(cfg.wallpaper.path, None);
        assert_eq!(cfg.wallpaper.mode, "fill");
        assert_eq!(cfg.wallpaper.color, parse_color("#0b0906").unwrap());
        assert!(cfg.wallpaper.outputs.is_empty());
        assert_eq!(schema::get(&cfg, "wallpaper.path"), Some(schema::Value::Null));
    }

    #[test]
    fn wallpaper_parses_each_mode_and_keys() {
        for m in ["fill", "fit", "center"] {
            let cfg = wallpaper_cfg(&format!(
                "wallpaper {{\n    path \"~/Pictures/x.png\"\n    mode \"{m}\"\n    color \"#102030\"\n}}\n"
            ));
            assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
            assert_eq!(cfg.wallpaper.mode, m);
            // Not expanded and not checked for existence: the daemon's job.
            assert_eq!(cfg.wallpaper.path.as_deref(), Some("~/Pictures/x.png"));
            assert_eq!(cfg.wallpaper.color, parse_color("#102030").unwrap());
            assert_eq!(
                schema::get(&cfg, "wallpaper.mode"),
                Some(schema::Value::Str(m.into()))
            );
        }
    }

    #[test]
    fn wallpaper_refuses_bad_mode_and_color() {
        let cfg = wallpaper_cfg(
            "wallpaper {\n    mode \"stretch\"\n    color \"#0b09\"\n    path 3\n    size 2\n}\n",
        );
        assert_eq!(cfg.errors.len(), 4, "{:?}", cfg.errors);
        let m = &cfg.errors[0].message;
        assert!(
            m.contains("wallpaper.mode") && m.contains("fill, fit, center"),
            "{m}"
        );
        assert_eq!((cfg.errors[0].line, cfg.errors[0].col), (2, 5));
        let m = &cfg.errors[1].message;
        assert!(m.contains("wallpaper.color") && m.contains("#rrggbb"), "{m}");
        assert!(cfg.errors[2].message.contains("wallpaper.path"));
        assert!(cfg.errors[3].message.contains("size"));
        // A refused value never lands.
        assert_eq!(cfg.wallpaper.mode, "fill");
        assert_eq!(cfg.wallpaper.color, schema::WALLPAPER_DEFAULT_COLOR);
        assert_eq!(cfg.wallpaper.path, None);

        let cfg = wallpaper_cfg("wallpaper {\n    output \"DP-1\" { mode \"tile\"; }\n}\n");
        assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
        assert!(cfg.errors[0].message.contains("wallpaper.output.mode"));
        assert_eq!(cfg.wallpaper.outputs[0].mode, None);
    }

    #[test]
    fn wallpaper_per_output_override() {
        let cfg = wallpaper_cfg(
            "wallpaper {\n    path \"/a.png\"\n    output \"DP-1\" { mode \"fit\"; }\n    output \"HDMI-A-1\" { path \"/b.png\"; color \"#ffffff\"; }\n    output \"DP-1\" { color \"#000000\"; }\n}\n",
        );
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert_eq!(cfg.wallpaper.path.as_deref(), Some("/a.png"));
        assert_eq!(cfg.wallpaper.mode, "fill");
        // One entry per name, file order; the later DP-1 block adds a key
        // without dropping the earlier one's.
        assert_eq!(
            cfg.wallpaper.outputs,
            [
                WallpaperOutput {
                    name: "DP-1".into(),
                    path: None,
                    mode: Some("fit".into()),
                    color: parse_color("#000000"),
                },
                WallpaperOutput {
                    name: "HDMI-A-1".into(),
                    path: Some("/b.png".into()),
                    mode: None,
                    color: parse_color("#ffffff"),
                },
            ]
        );
        let cfg = wallpaper_cfg("wallpaper {\n    output { mode \"fit\"; }\n}\n");
        assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
        assert!(cfg.wallpaper.outputs.is_empty());
    }

    /// D-07 §4: `setup.*` parses, lives in `abyss.kdl` only, and an unknown or
    /// ill-typed key is refused with its position like any other.
    #[test]
    fn setup_keys_parse_and_refuse_junk() {
        let doc: KdlDocument =
            "setup {\n    profile \"agentic\"\n    complete #true\n    pending-preset #true\n}\n"
                .parse()
                .unwrap();
        let mut cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert_eq!(cfg.setup.profile, "agentic");
        assert!(cfg.setup.complete && cfg.setup.pending_preset);

        let text =
            "setup {\n    profile \"standard\"\n    mode \"wm\"\n    profile \"nope\"\n    complete 1\n}\n";
        let doc: KdlDocument = text.parse().unwrap();
        let mut cfg = Config {
            cur: Some((abyss_src("/etc/eclipse/abyss.kdl"), text.to_owned())),
            ..Config::default()
        };
        cfg.apply(&doc, &mut Vec::new());
        assert_eq!(cfg.errors.len(), 3, "{:?}", cfg.errors);
        assert!(cfg.errors[0]
            .to_string()
            .starts_with("/etc/eclipse/abyss.kdl:3:5: "));
        assert!(
            cfg.errors[0].message.contains("\"mode\""),
            "{}",
            cfg.errors[0].message
        );
        assert_eq!(cfg.errors[1].line, 4);
        assert_eq!(cfg.errors[2].line, 5);
        assert_eq!(cfg.setup.profile, "standard", "a refused value must not land");
        assert!(!cfg.setup.complete);

        // The whole node is abyss-owned: policy.kdl may not carry it.
        let text = "setup {\n    complete #true\n}\n";
        let doc: KdlDocument = text.parse().unwrap();
        let mut cfg = Config {
            cur: Some((policy_src("/etc/eclipse/policy.kdl"), text.to_owned())),
            ..Config::default()
        };
        cfg.apply(&doc, &mut Vec::new());
        assert_eq!(cfg.errors.len(), 1);
        assert!(
            cfg.errors[0].message.contains("abyss.kdl"),
            "{}",
            cfg.errors[0].message
        );
        assert!(!cfg.setup.complete);
    }

    /// Anti-drift side (b): the parser's reject path consults the schema, so a
    /// near-miss is named and a key the schema claims but the parser does not
    /// handle is reported as an abyss bug rather than as the human's mistake.
    #[test]
    fn unknown_keys_suggest_the_schema_path_they_nearly_are() {
        fn err(text: &str) -> String {
            let doc: KdlDocument = text.parse().unwrap();
            let mut cfg = Config::default();
            cfg.apply(&doc, &mut Vec::new());
            assert_eq!(cfg.errors.len(), 1);
            cfg.errors[0].message.clone()
        }

        let m = err("general {\n    gaps-inn 4\n}\n");
        assert!(m.contains("did you mean \"general.gaps-in\""), "{m}");

        // Nothing close enough: no suggestion rather than a misleading one.
        let m = err("general {\n    quux 4\n}\n");
        assert!(!m.contains("did you mean"), "{m}");
    }

    /// HW-04 rule: every shipped bind spawns a binary in `eclipseos-meta`'s
    /// dependency closure, or it reads to the user as a dead keybind. The
    /// closure is read from `packaging/pkg/eclipseos/PKGBUILD`, so it cannot drift.
    #[test]
    fn default_bind_spawns_name_shipped_binaries() {
        let pkgbuild = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packaging/pkg/eclipseos/PKGBUILD"
        ))
        .expect("PKGBUILD readable");
        // `eclipseos-meta`'s `depends=( … )`. Its `optdepends` (the hyperion
        // add-on, ADR 0066) are not the closure: a bind cannot rely on them.
        let meta = &pkgbuild[pkgbuild.find("package_eclipseos-meta()").expect("meta package")..];
        let depends = &meta[meta.find("depends=(").expect("meta depends") + 9..];
        let depends: Vec<&str> = depends[..depends.find(')').unwrap()].split_whitespace().collect();
        // Binaries the split packages in that closure install: `_bin NAME`
        // and `for b in A B; do _bin "$b"; done`, under `package_NAME()`.
        let mut ours: Vec<&str> = Vec::new();
        let mut in_closure = false;
        for line in pkgbuild.lines().map(str::trim) {
            if let Some(pkg) = line.strip_prefix("package_") {
                let pkg = pkg.split("()").next().unwrap_or_default();
                in_closure = depends.contains(&pkg);
            } else if !in_closure {
                continue;
            } else if let Some(names) = line.strip_prefix("for b in ") {
                ours.extend(names.split(';').next().unwrap_or_default().split_whitespace());
            } else if let Some(name) = line.strip_prefix("_bin ") {
                ours.push(name.split_whitespace().next().unwrap_or_default());
            }
        }
        assert!(ours.contains(&"ec-launcher"), "PKGBUILD parse: {ours:?}");
        assert!(
            !ours.contains(&"ec-hyperion-bar"),
            "hyperion is an add-on, not the closure"
        );
        // Third-party binaries: (argv0, providing package). `base` is the
        // Arch base every image has.
        const EXTERNAL: &[(&str, &str)] = &[
            ("foot", "foot"),
            ("wpctl", "wireplumber"),
            ("brightnessctl", "brightnessctl"),
            ("playerctl", "playerctl"),
            ("loginctl", "base"),
        ];
        for bind in default_binds() {
            let Action::Spawn(cmd) = &bind.action else {
                continue;
            };
            let argv0 = cmd.split_whitespace().next().unwrap_or_default();
            let shipped = ours.contains(&argv0)
                || EXTERNAL
                    .iter()
                    .any(|(bin, pkg)| *bin == argv0 && (*pkg == "base" || depends.contains(pkg)));
            assert!(
                shipped,
                "default bind {:?}+{:?} spawns {argv0:?}, which no EclipseOS package installs",
                bind.mods, bind.key
            );
        }
    }

    /// CFG-01: `misc { xwayland … }` was accepted and ignored; it is an error
    /// that points at the real node (COMP-13 §1.1, amended C-05).
    #[test]
    fn misc_xwayland_is_rejected_with_a_hint() {
        let doc: KdlDocument = "misc {\n    xwayland #false\n}\n".parse().unwrap();
        let mut cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
        let m = &cfg.errors[0].message;
        assert!(m.contains("xwayland { enable"), "{m}");
        assert!(cfg.xwayland.enable, "the misc key must not disable X11");
    }

    /// `bar.clock.*` and `bar.popup-anchor`: defaults, parse, and a bad
    /// anchor rejected without moving off the default.
    #[test]
    fn bar_clock_and_popup_anchor_parse() {
        fn cfg(text: &str) -> Config {
            let doc: KdlDocument = text.parse().unwrap();
            let mut cfg = Config::default();
            cfg.apply(&doc, &mut Vec::new());
            cfg
        }
        let d = Config::default();
        assert_eq!(
            d.bar.clock,
            BarClock {
                hour_12: true,
                date_mdy: true
            }
        );
        assert_eq!(d.bar.popup_anchor, BarPopupAnchor::Cell);

        let c =
            cfg("bar {\n    clock { hour-12 #false; date-mdy #false }\n    popup-anchor \"pointer\"\n}\n");
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        assert_eq!(
            c.bar.clock,
            BarClock {
                hour_12: false,
                date_mdy: false
            }
        );
        assert_eq!(c.bar.popup_anchor, BarPopupAnchor::Pointer);

        let c = cfg("bar { popup-anchor \"corner\" }\n");
        assert_eq!(c.errors.len(), 1);
        assert!(
            c.errors[0].message.contains("\"cell\" or \"pointer\""),
            "{}",
            c.errors[0].message
        );
        assert_eq!(c.bar.popup_anchor, BarPopupAnchor::Cell);

        let c = cfg("bar { clock { hour-24 #true } }\n");
        assert_eq!(c.errors.len(), 1, "{:?}", c.errors);
    }

    /// `bar.launcher-style` is the deprecated alias of `launcher.style`:
    /// centred by default, `menu` parses, and a bad style is rejected
    /// without moving off the default.
    #[test]
    fn bar_launcher_style_parses() {
        fn cfg(text: &str) -> Config {
            let doc: KdlDocument = text.parse().unwrap();
            let mut cfg = Config::default();
            cfg.apply(&doc, &mut Vec::new());
            cfg
        }
        assert_eq!(Config::default().launcher.style, LauncherStyle::Centered);

        let c = cfg("bar { launcher-style \"menu\" }\n");
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        assert_eq!(c.launcher.style, LauncherStyle::Menu);
        assert_eq!(c.launcher.style.name(), "menu");

        let c = cfg("bar { launcher-style \"sideways\" }\n");
        assert_eq!(c.errors.len(), 1);
        assert!(
            c.errors[0].message.contains("\"centered\" or \"menu\""),
            "{}",
            c.errors[0].message
        );
        assert_eq!(c.launcher.style, LauncherStyle::Centered);
    }

    /// `launcher.style` wins over the deprecated alias whichever comes first.
    #[test]
    fn launcher_style_beats_the_bar_alias() {
        fn cfg(text: &str) -> Config {
            let doc: KdlDocument = text.parse().unwrap();
            let mut cfg = Config::default();
            cfg.apply(&doc, &mut Vec::new());
            cfg
        }
        let c = cfg("bar { launcher-style \"menu\" }\nlauncher { style \"centered\" }\n");
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        assert_eq!(c.launcher.style, LauncherStyle::Centered);
        let c = cfg("launcher { style \"centered\" }\nbar { launcher-style \"menu\" }\n");
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        assert_eq!(c.launcher.style, LauncherStyle::Centered);
        let c = cfg("launcher { style \"menu\" }\n");
        assert_eq!(c.launcher.style, LauncherStyle::Menu);
    }

    /// The `launcher` block: sizes, anchor and search flags parse, and out of
    /// range values keep the default.
    #[test]
    fn launcher_block_parses() {
        fn cfg(text: &str) -> Config {
            let doc: KdlDocument = text.parse().unwrap();
            let mut cfg = Config::default();
            cfg.apply(&doc, &mut Vec::new());
            cfg
        }
        let d = Config::default();
        assert_eq!(d.launcher.centered.width, 540);
        assert_eq!(d.launcher.centered.max_rows, 6);
        assert_eq!(d.launcher.centered.anchor, LauncherAnchor::Center);
        assert_eq!(d.launcher.menu.max_rows, 8);
        assert!(!d.launcher.search.path_binaries);
        assert!(d.launcher.search.terminal_apps);
        assert!(!d.launcher.search.match_descriptions);
        assert!(d.launcher.search.frecency);
        assert!(d.ui.show_key_hints);
        assert!(d.settings.search_frecency);

        let c = cfg(concat!(
            "launcher {\n",
            "  centered { width 720; max-rows 12; anchor \"top\"; }\n",
            "  menu { max-rows 5; }\n",
            "  search { path-binaries; terminal-apps #false; match-descriptions #true; }\n",
            "}\n",
            "ui { show-key-hints #false; }\n",
        ));
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        assert_eq!(c.launcher.centered.width, 720);
        assert_eq!(c.launcher.centered.max_rows, 12);
        assert_eq!(c.launcher.centered.anchor, LauncherAnchor::Top);
        assert_eq!(c.launcher.menu.max_rows, 5);
        assert!(c.launcher.search.path_binaries);
        assert!(!c.launcher.search.terminal_apps);
        assert!(c.launcher.search.match_descriptions);
        assert!(!c.ui.show_key_hints);

        let c = cfg("settings { search { frecency #false; } }\n");
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        assert!(!c.settings.search_frecency);

        let c = cfg("launcher { centered { width 20; anchor \"left\"; }; menu { max-rows 99; } }\n");
        assert_eq!(c.errors.len(), 3, "{:?}", c.errors);
        assert_eq!(c.launcher.centered.width, 540);
        assert_eq!(c.launcher.centered.anchor, LauncherAnchor::Center);
        assert_eq!(c.launcher.menu.max_rows, 8);
    }

    /// Launcher chords: `Super+R` is the chord `bind "SUPER" "r"` makes,
    /// `none` is no chord, and the reserved chords are refused.
    #[test]
    fn launcher_chords_parse() {
        let sup = m(true, false, false, false);
        assert_eq!(parse_chord("Super+R"), Ok(Some((sup, Keysym::r))));
        assert_eq!(parse_chord("SUPER r"), Ok(Some((sup, Keysym::r))));
        assert_eq!(
            parse_chord("Super+Shift+Return"),
            Ok(Some((m(true, true, false, false), Keysym::Return)))
        );
        assert_eq!(parse_chord("F2"), Ok(Some((Mods::default(), Keysym::F2))));
        assert_eq!(parse_chord("none"), Ok(None));
        assert_eq!(parse_chord(""), Ok(None));
        assert!(parse_chord("Super+Escape").is_err());
        assert!(parse_chord("Super+space").is_err());
        assert!(parse_chord("Super+").is_err());
        assert!(parse_chord("Hyper+r").is_err());
    }

    /// `launcher.bind.*` replaces the shipped Super+E / Super+R launcher
    /// binds; a `bind` block on the same chord still wins.
    #[test]
    fn launcher_binds_replace_the_defaults() {
        let sup = m(true, false, false, false);
        let spawn = Action::Spawn("ec-launcher".into());
        let launcher_chords = |binds: &[Bind]| {
            let mut v: Vec<_> = binds
                .iter()
                .filter(|b| b.action == spawn)
                .map(|b| (b.mods, b.key))
                .collect();
            v.sort_by_key(|(_, k)| k.raw());
            v
        };

        let stock = defaults_with_launcher(&Launcher::default());
        assert_eq!(launcher_chords(&stock), vec![(sup, Keysym::e), (sup, Keysym::r)]);

        let doc: KdlDocument = "launcher { bind { open \"Super+o\"; run \"none\"; } }\n"
            .parse()
            .unwrap();
        let mut c = Config::default();
        c.apply(&doc, &mut Vec::new());
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        let binds = defaults_with_launcher(&c.launcher);
        assert_eq!(launcher_chords(&binds), vec![(sup, Keysym::o)]);

        let doc: KdlDocument = "launcher { bind { open \"Super+Escape\"; } }\n".parse().unwrap();
        let mut c = Config::default();
        c.apply(&doc, &mut Vec::new());
        assert_eq!(c.errors.len(), 1, "{:?}", c.errors);
        assert_eq!(c.launcher.bind.open.text, "Super+E");
    }

    /// `bar.eye`: on by default, a bare node is on, `#false` turns it off.
    #[test]
    fn bar_eye_parses() {
        fn cfg(text: &str) -> Config {
            let doc: KdlDocument = text.parse().unwrap();
            let mut cfg = Config::default();
            cfg.apply(&doc, &mut Vec::new());
            cfg
        }
        assert!(Config::default().bar.eye);
        let c = cfg("bar { eye #false }\n");
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        assert!(!c.bar.eye);
        let c = cfg("bar { eye }\n");
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        assert!(c.bar.eye);
    }

    /// `bar.tray`: an absent `pinned` is "the bar decides", a bare one pins
    /// nothing, and order is kept exactly as written.
    #[test]
    fn bar_tray_lists_keep_order_and_unset_differs_from_empty() {
        fn tray(text: &str) -> BarTray {
            let doc: KdlDocument = text.parse().unwrap();
            let mut cfg = Config::default();
            cfg.apply(&doc, &mut Vec::new());
            assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
            cfg.bar.tray
        }
        assert_eq!(tray("bar { position top }\n"), BarTray::default());
        assert_eq!(tray("bar { tray { pinned } }\n").pinned, Some(vec![]));
        let t = tray("bar { tray { pinned volume \"org.kde.x\" network; hidden battery } }\n");
        assert_eq!(
            t.pinned.as_deref(),
            Some(&["volume".to_string(), "org.kde.x".into(), "network".into()][..])
        );
        assert_eq!(t.hidden, ["battery"]);
    }

    /// Apply `text` as `a.kdl`, returning the config and its refusals.
    pub(crate) fn widgets_cfg(text: &str) -> Config {
        parse_single_for_tests(text)
    }

    /// `bar.widgets` and `bar.motion` (ADR 0065): defaults, a full block, and
    /// a `custom:` id resolved by a `widget` block written after it.
    #[test]
    fn bar_widgets_parse() {
        let d = Config::default().bar;
        assert_eq!(d.widgets.order, schema::BAR_WIDGET_DEFAULT_ORDER);
        assert_eq!(d.widgets.important, ["clock", "battery"]);
        assert_eq!(d.motion.curve, "spring");
        let cfg = widgets_cfg(
            "bar {\n    widgets {\n        order \"clock\" \"custom:cpu\" \"tray\"\n        important \"custom:cpu\"\n        \
             now-playing { art #false; visualizer; remote-art #false; }\n        system-usage { interval-ms \"2s\"; gpu #false; disk-path \"/home\"; }\n        \
             volume { step 10; scroll #false; max-percent 150; }\n    }\n    motion { enabled #false; duration-ms 300; curve \"ease-out\"; }\n    \
             widget \"cpu\" { source \"usage.cpu\"; format \"{}%\"; }\n}\n",
        );
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        let w = &cfg.bar.widgets;
        assert_eq!(w.order, ["clock", "custom:cpu", "tray"]);
        assert_eq!(w.important, ["custom:cpu"]);
        assert!(!w.now_playing.art && w.now_playing.visualizer && !w.now_playing.remote_art);
        assert!(Config::default().bar.widgets.now_playing.remote_art);
        assert_eq!(w.system_usage.interval_ms, 2000);
        assert!(!w.system_usage.gpu && w.system_usage.disk);
        assert_eq!(w.system_usage.disk_path, "/home");
        assert_eq!(
            (w.volume.step, w.volume.scroll, w.volume.max_percent),
            (10, false, 150)
        );
        let m = &cfg.bar.motion;
        assert_eq!(
            (m.enabled, m.duration_ms, m.curve.as_str()),
            (false, 300, "ease-out")
        );
        assert_eq!(
            cfg.bar.custom_widgets[0].kind,
            CustomWidgetKind::Source {
                source: "usage.cpu".into(),
                format: "{}%".into()
            }
        );
        // An empty order draws nothing and is not an error.
        let cfg = widgets_cfg("bar { widgets { order; } }\n");
        assert!(cfg.errors.is_empty() && cfg.bar.widgets.order.is_empty());
    }

    /// An unknown id is refused at its own position with a did-you-mean, and
    /// only that id is dropped.
    #[test]
    fn an_unknown_widget_id_is_refused_where_it_stands() {
        let cfg = widgets_cfg("bar {\n    widgets { order \"clock\" \"batery\"; }\n}\n");
        assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
        let e = &cfg.errors[0];
        assert!(e.message.contains("did you mean \"battery\""), "{}", e.message);
        assert_eq!((e.line, e.col, e.span_len), (2, 29, 8));
        assert_eq!(cfg.bar.widgets.order, ["clock"]);

        let cfg = widgets_cfg("bar { widgets { important \"clock\" \"clock\" \"zzzzzz\"; } }\n");
        assert_eq!(cfg.errors.len(), 2, "{:?}", cfg.errors);
        assert!(cfg.errors[0].message.contains("twice"));
        assert!(cfg.errors[1].message.contains("built-in widgets are"));
    }

    /// `custom:<name>` must name a `widget` block, wherever in the file it is.
    #[test]
    fn a_custom_id_must_name_a_widget_block() {
        let cfg = widgets_cfg(
            "bar {\n    widgets { order \"custom:wether\"; }\n    widget \"weather\" { exec \"curl\"; }\n}\n",
        );
        assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
        let e = &cfg.errors[0];
        assert!(e.message.contains("did you mean"), "{}", e.message);
        assert_eq!((e.line, e.col), (2, 21));
        let cfg = widgets_cfg("bar { widgets { order \"custom:\"; } }\n");
        assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
    }

    /// One bad field in a `widget` block is one error, at the field: the
    /// refused block is not also reported missing by a `custom:` id naming it.
    #[test]
    fn an_invalid_widget_block_is_not_also_missing() {
        let cfg = widgets_cfg(
            "bar {\n    widgets { order \"custom:a\"; }\n    widget \"a\" { exec \"x\"; interval-ms 10; }\n}\n",
        );
        assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
        let e = &cfg.errors[0];
        assert!(!e.message.contains("names no widget block"), "{}", e.message);
        assert_eq!(e.line, 3);
        assert!(cfg.bar.custom_widgets.is_empty());
    }

    /// An order entry whose block is truly absent is reported missing.
    #[test]
    fn an_absent_widget_block_is_reported_missing() {
        let cfg = widgets_cfg("bar {\n    widgets { order \"custom:a\"; }\n}\n");
        assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
        let e = &cfg.errors[0];
        assert!(
            e.message.contains("custom:a names no widget block"),
            "{}",
            e.message
        );
        assert_eq!((e.line, e.col), (2, 21));
    }

    /// A `widget` block is all or nothing, and a later one replaces an
    /// earlier one of the same name in place.
    #[test]
    fn widget_blocks_are_validated_whole() {
        for bad in [
            "widget \"a\" { }",
            "widget \"a\" { exec \"x\"; source \"usage.cpu\"; }",
            "widget \"a\" { exec \"x\"; stream; interval-ms 1000; }",
            "widget \"a\" { exec \"x\"; format \"{}\"; }",
            "widget \"a\" { source \"usage.cpu\"; interval-ms 1000; }",
            "widget \"a\" { source \"usage.cpux\"; }",
            "widget \"a\" { exec; }",
            "widget \"a\" { exec \"x\" 1; }",
            "widget \"a\" { exec \"x\"; interval-ms 10; }",
            "widget \"a\" { exec \"x\"; colour \"red\"; }",
            "widget \"a\" { exec \"x\"; exec \"y\"; }",
            "widget { exec \"x\"; }",
        ] {
            let cfg = widgets_cfg(&format!("bar {{ {bad} }}\n"));
            assert!(!cfg.errors.is_empty(), "{bad}");
            assert!(cfg.bar.custom_widgets.is_empty(), "{bad}");
        }
        let cfg = widgets_cfg(
            "bar { widget \"a\" { exec \"x\"; }; widget \"b\" { exec \"y\"; stream; }; widget \"a\" { exec \"z\"; interval-ms \"1m\"; } }\n",
        );
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        let names: Vec<_> = cfg.bar.custom_widgets.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["a", "b"]);
        assert_eq!(
            cfg.bar.custom_widgets[0].kind,
            CustomWidgetKind::Exec {
                argv: vec!["z".into()],
                interval_ms: 60_000
            }
        );
        assert_eq!(
            cfg.bar.custom_widgets[1].kind,
            CustomWidgetKind::Stream {
                argv: vec!["y".into()]
            }
        );
    }

    /// Every premade in `packaging/widgets/` (ADR 0067) loads through the same
    /// path as a user `widget` block: one block, no refusals, and a command
    /// widget (exec or stream), never a declarative `source`.
    #[test]
    fn premade_widgets_are_valid_command_widgets() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../packaging/widgets");
        let mut n = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "kdl") {
                continue;
            }
            n += 1;
            let text = std::fs::read_to_string(&path).unwrap();
            let cfg = widgets_cfg(&text);
            assert!(cfg.errors.is_empty(), "{}: {:?}", path.display(), cfg.errors);
            let [w] = &cfg.bar.custom_widgets[..] else {
                panic!("{}: expected exactly one widget block", path.display());
            };
            assert_eq!(Some(w.name.as_str()), path.file_stem().and_then(|s| s.to_str()));
            let argv = match &w.kind {
                CustomWidgetKind::Exec { argv, .. } | CustomWidgetKind::Stream { argv } => argv,
                CustomWidgetKind::Source { .. } => {
                    panic!("{}: premade must be a command widget", path.display())
                }
            };
            // Argv only: nothing handed to a shell or an interpreter.
            for a in [
                Some(argv),
                w.on_click.as_ref(),
                w.on_scroll_up.as_ref(),
                w.on_scroll_down.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                let argv0 = a[0].rsplit('/').next().unwrap();
                assert!(
                    !["sh", "bash", "zsh", "dash", "fish", "env", "python", "python3", "perl"]
                        .contains(&argv0),
                    "{}: {argv0} is a shell or interpreter",
                    path.display()
                );
                assert!(!a.iter().any(|s| s == "-c"), "{}: -c", path.display());
            }
        }
        assert!(n > 0, "no premade widgets in {}", dir.display());
    }

    /// Out-of-range and mistyped widget settings are refused, keeping the
    /// default; a bad motion curve too, and `animations` still refuses spring.
    #[test]
    fn widget_settings_out_of_range_keep_the_default() {
        let cfg = widgets_cfg(
            "bar { widgets { volume { step 0; max-percent 200; }; system-usage { interval-ms 100; disk-path \"home\"; }; }; motion { curve \"bounce\"; duration-ms 5000; } }\n",
        );
        assert_eq!(cfg.errors.len(), 6, "{:?}", cfg.errors);
        assert_eq!(cfg.bar.widgets, BarWidgets::default());
        assert_eq!(cfg.bar.motion, BarMotion::default());
        let cfg = widgets_cfg("animations { enabled #true; animation \"window-open\" curve=\"spring\"; }\n");
        assert!(!cfg.errors.is_empty());
    }

    /// Built-in applet ids in `bar.tray` still load: deprecated, not refused.
    #[test]
    fn legacy_tray_ids_still_load() {
        let cfg = widgets_cfg("bar { tray { pinned \"volume\" \"org.kde.x\"; hidden \"battery\"; } }\n");
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert_eq!(cfg.bar.tray.hidden, ["battery"]);
    }

    /// A tab-indented line keeps its tabs in the caret gutter so the run still
    /// lands under the token whatever tab width the terminal uses.
    #[test]
    fn the_caret_gutter_preserves_tabs() {
        let text = "general {\n\tgaps-inn 4\n}\n";
        let doc: KdlDocument = text.parse().unwrap();
        let mut cfg = Config {
            cur: Some((abyss_src("a.kdl"), text.to_owned())),
            ..Config::default()
        };
        cfg.apply(&doc, &mut Vec::new());
        assert_eq!(
            cfg.errors[0].to_string().lines().last().unwrap(),
            "  | \t^^^^^^^^"
        );
    }

    /// A valid config records no refusals, so hot-reload applies it.
    #[test]
    fn a_valid_config_has_no_errors() {
        let doc: KdlDocument = "general { gaps-in 4; layout \"master\" }\n".parse().unwrap();
        let mut cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    }

    /// An unparseable file is a refusal, not a silent fall back to defaults —
    /// which used to wipe the live config on hot-reload.
    #[test]
    fn an_unparseable_file_is_refused_not_defaulted() {
        let dir = std::env::temp_dir().join(format!("abyss-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("abyss.kdl");
        std::fs::write(&f, "general { gaps-in \"unterminated\n").unwrap();
        let cfg = Config::load(Some(&f));
        assert!(!cfg.errors.is_empty());
        assert_eq!(cfg.errors[0].file, f);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A config that declares binds extends the defaults; it does not replace
    /// the table. The live regression this covers: a 3-bind user config left a
    /// 3-bind table, dropping the Super+Escape override chord.
    #[test]
    fn config_binds_extend_defaults() {
        let dir = std::env::temp_dir().join(format!("abyss-cfg-merge-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("abyss.kdl");
        std::fs::write(&f, "bind \"SUPER\" \"F1\" { quit; }\n").unwrap();
        let cfg = Config::load(Some(&f));
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert_eq!(cfg.binds.len(), default_binds().len() + 1);
        assert!(cfg.binds.iter().any(|b| b.key == Keysym::F1));

        // The two reserved chords survive any config (COMP-04 §6, COMP-13 §1.1).
        let sup = m(true, false, false, false);
        assert!(cfg
            .binds
            .iter()
            .any(|b| b.mods == sup && b.key == Keysym::Escape && matches!(b.action, Action::AgentOverride)));
        assert!(cfg
            .binds
            .iter()
            .any(|b| b.mods == sup && b.key == Keysym::space && matches!(b.action, Action::AgentAttention)));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A config bind on a chord the defaults already use wins the lookup.
    #[test]
    fn config_bind_overrides_default_chord() {
        let dir = std::env::temp_dir().join(format!("abyss-cfg-override-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("abyss.kdl");
        std::fs::write(&f, "bind \"SUPER\" \"q\" { spawn \"alacritty\"; }\n").unwrap();
        let cfg = Config::load(Some(&f));
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert_eq!(cfg.binds.len(), default_binds().len(), "override, not addition");
        let mods = ModifiersState {
            logo: true,
            ..Default::default()
        };
        match cfg.action_for(&mods, Keysym::q) {
            Some(Action::Spawn(cmd)) => assert_eq!(cmd, "alacritty"),
            other => panic!("expected the config spawn, got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A later source replaces an earlier one for the same chord.
    #[test]
    fn later_config_bind_wins() {
        let binds = merge_binds(
            default_binds(),
            vec![
                Bind {
                    mods: m(true, false, false, false),
                    key: Keysym::F1,
                    action: Action::Spawn("first".into()),
                },
                Bind {
                    mods: m(true, false, false, false),
                    key: Keysym::F1,
                    action: Action::Spawn("second".into()),
                },
            ],
        );
        assert_eq!(binds.len(), default_binds().len() + 1);
        let f1 = binds.iter().find(|b| b.key == Keysym::F1).unwrap();
        assert!(matches!(&f1.action, Action::Spawn(c) if c == "second"));
    }

    #[test]
    fn parses_input() {
        let doc: KdlDocument = r#"
            input {
                kb-layout "de"
                kb-options "compose:ralt"
                repeat-rate 25
                repeat-delay 400
                accel-profile "flat"
                touchpad { natural-scroll #true; tap-to-click #true; dwt #false; click-method "button-areas" }
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
        assert_eq!(cfg.input.touchpad.click_method, "button-areas");
        assert_eq!(Config::default().input.touchpad.click_method, "clickfinger");
        let d = Config::default().input;
        assert_eq!((d.accel_speed, d.scroll_method.as_str()), (0.0, "default"));
        assert!(d.touchpad.tap_and_drag && d.devices.is_empty());
        assert_eq!(d.touchpad.scroll_method, "two-finger");
        // The override chord is built in, never taken from the config.
        assert!(binds.is_empty());
        assert!(default_binds()
            .iter()
            .any(|b| b.key == Keysym::Escape && matches!(b.action, crate::input::Action::AgentOverride)));
        assert!(default_binds()
            .iter()
            .any(|b| b.key == Keysym::space && matches!(b.action, crate::input::Action::AgentAttention)));
    }

    /// Repeat delay takes the schema's full 0..=5000 ms, unclamped, and
    /// refuses anything past it.
    #[test]
    fn repeat_delay_range() {
        for (text, want, errs) in [("600", 600, 0), ("5000", 5000, 0), ("5001", 300, 1)] {
            let doc: KdlDocument = format!("input {{ repeat-delay {text} }}").parse().unwrap();
            let mut cfg = Config::default();
            cfg.apply(&doc, &mut Vec::new());
            assert_eq!(cfg.input.repeat_delay, want, "repeat-delay {text}");
            assert_eq!(cfg.errors.len(), errs, "repeat-delay {text}");
        }
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
                glow { enabled #true; inactive #false; strength 80 }
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
        assert_eq!(cfg.decoration.blur.mode, BlurMode::Off, "legacy `enabled #false`");
        assert_eq!((cfg.decoration.blur.size, cfg.decoration.blur.passes), (12, 3));
        assert!(cfg.decoration.shadow.enabled && cfg.decoration.shadow.range == 20);
        let g = &cfg.decoration.glow;
        assert_eq!(
            (g.enabled, g.active, g.inactive, g.strength),
            (true, true, false, 80)
        );
        assert!(cfg.decoration.any_window_effect());
        let w = cfg.animations.get("windows").expect("windows curve");
        assert_eq!((w.duration_ms, w.curve.as_str()), (150, "ease-out"));
        assert_eq!(cfg.animations.get("workspaces").unwrap().duration_ms, 200);
        assert!(cfg.animations.get("nope").is_none());
    }

    #[test]
    fn decoration_defaults_round_but_are_otherwise_no_effect() {
        let cfg = Config::default();
        // Rounding (9px) and the shadow ship on (C-16), so any_window_effect()
        // is already true out of the box. Content never fades: both
        // opacities ship at 1.0, and blur follows the surface's opaque region
        // instead; dim is untouched.
        assert!(cfg.decoration.any_window_effect());
        assert!(cfg.decoration.shadow.enabled);
        assert_eq!(cfg.decoration.shadow.range, 16);
        assert!(!cfg.decoration.glow.on());
        assert_eq!(cfg.decoration.rounding, 9);
        assert_eq!(cfg.decoration.active_opacity, 1.0);
        assert_eq!(cfg.decoration.inactive_opacity, 1.0);
        assert_eq!(cfg.decoration.dim_inactive, 0.0);
        // Animations off means no curve resolves even if one were parsed.
        assert!(!cfg.animations.enabled);
    }

    /// Blur ships on, in glass mode (C-16): translucent surfaces get the
    /// Liquid Glass pass without being asked. It costs a render pass, so it
    /// is called out here rather than folded into the no-effect test above
    /// --- if this flips, the schema default column and `docs/CONFIG.md` flip
    /// with it.
    #[test]
    fn blur_ships_enabled() {
        assert_eq!(Config::default().decoration.blur.mode, BlurMode::Glass);
    }

    fn blur_cfg(text: &str) -> Config {
        let doc: KdlDocument = text.parse().unwrap();
        let mut cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        cfg
    }

    #[test]
    fn blur_mode_parses_every_mode() {
        for (name, want) in [
            ("off", BlurMode::Off),
            ("blur", BlurMode::Blur),
            ("frost", BlurMode::Frost),
            ("glass", BlurMode::Glass),
        ] {
            let cfg = blur_cfg(&format!("decoration {{ blur {{ mode {name:?} }} }}\n"));
            assert!(cfg.errors.is_empty(), "{name}: {:?}", cfg.errors);
            assert_eq!(cfg.decoration.blur.mode, want);
            assert_eq!(want.name(), name);
        }
        assert_eq!(BlurMode::NAMES, ["off", "blur", "frost", "glass"]);
        let cfg = blur_cfg("decoration { blur { mode \"liquid\" } }\n");
        assert_eq!(cfg.errors.len(), 1);
        assert_eq!(
            cfg.decoration.blur.mode,
            BlurMode::Glass,
            "a bad mode keeps the default"
        );
    }

    /// The pre-`mode` bool still loads: false is `off`, true is plain `blur`.
    #[test]
    fn legacy_blur_enabled_maps_onto_mode() {
        let cfg = blur_cfg("decoration { blur { enabled #false } }\n");
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert_eq!(cfg.decoration.blur.mode, BlurMode::Off);
        let cfg = blur_cfg("decoration { blur { enabled #true } }\n");
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert_eq!(cfg.decoration.blur.mode, BlurMode::Blur);
    }

    /// An explicit `mode` wins over a legacy `enabled` in either order.
    #[test]
    fn blur_mode_wins_over_enabled_regardless_of_order() {
        for text in [
            "decoration { blur { mode \"glass\"; enabled #false } }\n",
            "decoration { blur { enabled #false; mode \"glass\" } }\n",
        ] {
            let cfg = blur_cfg(text);
            assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
            assert_eq!(cfg.decoration.blur.mode, BlurMode::Glass, "{text}");
        }
        let cfg = blur_cfg("decoration { blur { mode \"off\"; enabled #true } }\n");
        assert_eq!(cfg.decoration.blur.mode, BlurMode::Off);
    }

    #[test]
    fn glass_and_frost_tunables_parse_and_bad_ones_keep_defaults() {
        let cfg = blur_cfg(
            "decoration { blur {\n  glass { refraction 20; bevel 8; dispersion 0.5; rim 1.0 }\n  frost { tint \"#10203040\" }\n} }\n",
        );
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        let g = &cfg.decoration.blur.glass;
        assert_eq!((g.refraction, g.bevel, g.dispersion, g.rim), (20, 8, 0.5, 1.0));
        assert_eq!(cfg.decoration.blur.frost.tint[3], 0x40 as f32 / 255.0);
        let cfg = blur_cfg(
            "decoration { blur {\n  glass { refraction 99; bevel 0; dispersion 2.0; rim -1.0; nope 1 }\n  frost { tint \"gold\" }\n} }\n",
        );
        assert_eq!(cfg.errors.len(), 6, "{:?}", cfg.errors);
        assert_eq!(cfg.decoration.blur.glass, GlassBlur::default());
        assert_eq!(cfg.decoration.blur.frost, FrostBlur::default());
    }

    #[test]
    fn annotation_colours_parse_and_bad_ones_keep_defaults() {
        let cfg = parse_single_for_tests(
            "annotations {\n  accent \"#112233\"\n  danger \"#ff000080\"\n  selection-dim \"#00000099\"\n}\n",
        );
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        let a = &cfg.annotations;
        assert_eq!(a.accent, rgba8(0x11, 0x22, 0x33, 0xff));
        assert_eq!(a.danger, rgba8(0xff, 0, 0, 0x80));
        assert_eq!(a.selection_dim, rgba8(0, 0, 0, 0x99));
        assert_eq!(a.text, AnnotationColors::DEFAULT.text);

        let cfg =
            parse_single_for_tests("annotations {\n  accent \"gold\"\n  accnet \"#112233\"\n  text 7\n}\n");
        assert_eq!(cfg.errors.len(), 3, "{:?}", cfg.errors);
        assert!(cfg.errors[1].to_string().contains("accnet"), "{:?}", cfg.errors);
        assert_eq!(cfg.annotations, AnnotationColors::DEFAULT);
    }

    #[test]
    fn annotation_panel_tint_alpha_is_raised_to_the_floor() {
        let cfg = parse_single_for_tests("annotations { panel-tint \"#ffffff10\"; }\n");
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        let t = cfg.annotations.panel_tint;
        assert_eq!(t[..3], [1.0, 1.0, 1.0]);
        assert_eq!(t[3], PANEL_TINT_MIN_ALPHA);
        let cfg = parse_single_for_tests("annotations { panel-tint \"#000000e0\"; }\n");
        assert_eq!(cfg.annotations.panel_tint[3], 0xe0 as f32 / 255.0);
        const { assert!(AnnotationColors::DEFAULT.panel_tint[3] >= PANEL_TINT_MIN_ALPHA) };
    }

    #[test]
    fn oracle_eyes_parses_and_bad_values_keep_defaults() {
        let cfg = parse_single_for_tests(
            "oracle-eyes {\n  model-command \"claude\" \"--model\" \"haiku\"\n  timeout-ms 45000\n  \
             auto-interval-ms 5000\n  hold-ms 6000\n  debug #true\n}\n",
        );
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert_eq!(
            cfg.oracle_eyes,
            OracleEyes {
                model_command: Some(vec!["claude".into(), "--model".into(), "haiku".into()]),
                timeout_ms: 45_000,
                auto_interval_ms: 5000,
                hold_ms: 6000,
                debug: true,
                bind: OracleEyesBinds::default(),
            }
        );

        let cfg = parse_single_for_tests(
            "oracle-eyes {\n  model-command\n  timeout-ms 10\n  auto-interval-ms \"3s\"\n  hold-ms 999999\n  \
             debug \"yes\"\n  colour \"red\"\n}\n",
        );
        assert_eq!(cfg.errors.len(), 6, "{:?}", cfg.errors);
        assert_eq!(cfg.oracle_eyes, OracleEyes::default());

        for bad in [
            "oracle-eyes { model-command \"\"; }",
            "oracle-eyes { model-command \"claude\" 3; }",
            "oracle-eyes { model-command \"claude\" flag=\"x\"; }",
        ] {
            let cfg = parse_single_for_tests(bad);
            assert_eq!(cfg.errors.len(), 1, "{bad}: {:?}", cfg.errors);
            assert_eq!(cfg.oracle_eyes, OracleEyes::default(), "{bad}");
        }
    }

    /// Settings writes these through `edit` and reads them back through
    /// `schema::get`: both ends name the same thing, and the file stays one
    /// block per section.
    #[test]
    fn oracle_eyes_binds_become_default_binds() {
        let c = Config::default();
        assert!(
            oracle_eyes_binds(&c.oracle_eyes).is_empty(),
            "nothing bound by default"
        );
        let c = parse_single_for_tests(
            "oracle-eyes { bind { select \"Super+Shift+A\"; dismiss \"Super+Shift+D\"; \
             expand \"none\"; auto-toggle \"Super+Shift+T\"; } }",
        );
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        let binds = oracle_eyes_binds(&c.oracle_eyes);
        assert_eq!(binds.len(), 3);
        assert_eq!(binds[0].action, Action::AnnotationSelect);
        assert_eq!(binds[0].key, Keysym::a);
        assert_eq!(binds[2].action, Action::AnnotationAutoToggle);
        assert_eq!(c.oracle_eyes.bind.expand.text, "none");

        let c = parse_single_for_tests(
            "oracle-eyes { bind { select \"Super+Escape\"; frob \"x\"; dismiss 3; } }",
        );
        assert_eq!(c.errors.len(), 3, "{:?}", c.errors);
        assert_eq!(c.oracle_eyes.bind, OracleEyesBinds::default());
    }

    #[test]
    fn annotation_and_oracle_eyes_edits_round_trip() {
        let text = edit::set_value("", "annotations.leader", &KdlValue::String("#12345678".into())).unwrap();
        let text = edit::set_value(
            &text,
            "annotations.panel-tint",
            &KdlValue::String("#00000020".into()),
        )
        .unwrap();
        let text = edit::set_value(&text, "oracle-eyes.hold-ms", &KdlValue::Integer(7000)).unwrap();
        let text = edit::set_value(&text, "oracle-eyes.debug", &KdlValue::Bool(true)).unwrap();
        let argv = [KdlValue::String("ollama".into()), KdlValue::String("run".into())];
        let text = edit::set_list(&text, "oracle-eyes.model-command", &argv).unwrap();
        let text = edit::set_value(&text, "annotations.accent", &KdlValue::String("#abcdef".into())).unwrap();
        assert_eq!(text.matches("annotations").count(), 1, "{text}");
        assert_eq!(text.matches("oracle-eyes").count(), 1, "{text}");

        let cfg = parse_single_for_tests(&text);
        assert!(cfg.errors.is_empty(), "{text}\n{:?}", cfg.errors);
        use schema::Value as V;
        assert_eq!(
            schema::get(&cfg, "annotations.leader"),
            Some(V::Color(rgba8(0x12, 0x34, 0x56, 0x78)))
        );
        assert_eq!(
            schema::get(&cfg, "annotations.accent"),
            Some(V::Color(rgba8(0xab, 0xcd, 0xef, 0xff)))
        );
        assert_eq!(
            schema::get(&cfg, "annotations.panel-tint"),
            Some(V::Color([0.0, 0.0, 0.0, PANEL_TINT_MIN_ALPHA]))
        );
        assert_eq!(schema::get(&cfg, "oracle-eyes.hold-ms"), Some(V::Int(7000)));
        assert_eq!(schema::get(&cfg, "oracle-eyes.debug"), Some(V::Bool(true)));
        assert_eq!(
            schema::get(&cfg, "oracle-eyes.model-command"),
            Some(V::List(vec!["ollama".into(), "run".into()]))
        );

        // A withheld command reads as unset, never as its argv.
        let mut cfg = cfg;
        cfg.oracle_eyes.model_command = None;
        assert_eq!(schema::get(&cfg, "oracle-eyes.model-command"), Some(V::Null));
    }

    #[test]
    fn windowrule_blur_takes_a_mode_or_a_bool() {
        for (value, want) in [
            ("true", BlurRule::On),
            ("false", BlurRule::Mode(BlurMode::Off)),
            ("off", BlurRule::Mode(BlurMode::Off)),
            ("blur", BlurRule::Mode(BlurMode::Blur)),
            ("frost", BlurRule::Mode(BlurMode::Frost)),
            ("glass", BlurRule::Mode(BlurMode::Glass)),
        ] {
            let cfg = blur_cfg(&format!("windowrule \"blur {value}\" {{ app-id \"a\"; }}\n"));
            assert!(cfg.errors.is_empty(), "{value}: {:?}", cfg.errors);
            assert!(
                matches!(cfg.window_rules[0].action, RuleAction::Blur(got) if got == want),
                "{value}: {:?}",
                cfg.window_rules[0].action
            );
        }
        let cfg = blur_cfg("windowrule \"blur maybe\" { app-id \"a\"; }\n");
        assert!(cfg.window_rules.is_empty());
        // `true` follows the global mode, falling back to plain blur when that is off.
        assert_eq!(BlurRule::On.resolve(BlurMode::Glass), BlurMode::Glass);
        assert_eq!(BlurRule::On.resolve(BlurMode::Off), BlurMode::Blur);
        assert_eq!(
            BlurRule::Mode(BlurMode::Off).resolve(BlurMode::Glass),
            BlurMode::Off
        );
        assert_eq!(
            BlurRule::Mode(BlurMode::Frost).resolve(BlurMode::Off),
            BlurMode::Frost
        );
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
        assert_eq!(cfg.decoration.rounding, 9);
        assert_eq!(cfg.decoration.active_opacity, 1.0);
        assert_eq!(cfg.decoration.inactive_opacity, 1.0);
        assert!(cfg.animations.curves.is_empty());
    }

    #[test]
    fn parses_geometry_and_classification_rules() {
        let doc: KdlDocument = r#"
            windowrule "size 800x600"        { app-id "mpv" }
            windowrule "position 40,-20"     { app-id "mpv" }
            windowrule "output DP-*"         { app-id "mpv" }
            windowrule "app-trust trusted"   { cgroup "app-remnant" }
            windowrule "seat-compat multi"   { app-id "mpv" }
            windowrule "idle-inhibit"        { app-id "mpv" }
            windowrule "size 800"            { app-id "mpv" }
            windowrule "position left"       { app-id "mpv" }
            windowrule "app-trust root"      { app-id "mpv" }
            windowrule "seat-compat none"    { app-id "mpv" }
            windowrule "cgroup-typo"         { cgroup "(" }
            windowrule "irreversible-capable true"  { app-id "foot" }
            windowrule "irreversible-capable false" { app-id "mpv" }
            windowrule "irreversible-capable maybe" { app-id "mpv" }
            windowrule "irreversible-capable"       { app-id "mpv" }
            windowrule "irreversable-capable true"  { app-id "mpv" }
        "#
        .parse()
        .unwrap();
        let mut cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        // Every malformed rule above is a config error, not a warning that
        // lets it through: the five bad shapes plus the three bad
        // `irreversible-capable` forms (milestone 9e).
        assert_eq!(cfg.errors.len(), 8, "{:?}", cfg.errors);
        assert!(cfg
            .errors
            .iter()
            .any(|e| e.message.contains("irreversable-capable")));
        let actions: Vec<&RuleAction> = cfg.window_rules.iter().map(|r| &r.action).collect();
        assert_eq!(
            actions,
            vec![
                &RuleAction::Size(800, 600),
                &RuleAction::Position(40, -20),
                &RuleAction::Output("DP-*".to_owned()),
                &RuleAction::Trust(AppTrust::Trusted),
                &RuleAction::Seat(SeatCompat::Multi),
                &RuleAction::IdleInhibit,
                &RuleAction::IrreversibleCapable(true),
                &RuleAction::IrreversibleCapable(false),
            ]
        );
        assert!(cfg.window_rules[3].matchers.cgroup.is_some());
    }

    #[test]
    fn pairs() {
        assert_eq!(parse_pair("800x600", 'x'), Some((800, 600)));
        assert_eq!(parse_pair(" 40 , -20 ", ','), Some((40, -20)));
        assert_eq!(parse_pair("800x600x1", 'x'), None);
        assert_eq!(parse_pair("800", 'x'), None);
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
            windowrule "fullscreen" { app-id "mpv"; }
        "#
        .parse()
        .unwrap();
        let mut c = Config::default();
        c.apply(&doc, &mut Vec::new());
        assert_eq!(c.window_rules.len(), 6);
        assert_eq!(c.window_rules[1].action, RuleAction::Workspace(3));
        assert_eq!(c.window_rules[2].action, RuleAction::Opacity(0.85));
        assert_eq!(c.window_rules[4].action, RuleAction::NoAgent);
        assert_eq!(c.window_rules[5].action, RuleAction::Fullscreen);
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
            windowrule "float" { launching-principal "x"; }
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

    /// The vertical gaps ship set (C-16), independent of the horizontal
    /// ones. Unset, `gaps-in-vertical`/`gaps-out-vertical` still mirror their
    /// horizontal counterpart, including a horizontal value already
    /// customised away from the default — the resolver, not a hardcoded
    /// literal, is what makes that true.
    #[test]
    fn vertical_gaps_mirror_horizontal_until_set() {
        let d = General::default();
        assert_eq!((d.gaps_in_y(), d.gaps_out_y()), (3, 7));

        let doc: KdlDocument = "general { gaps-in 3; gaps-out 12 }\n".parse().unwrap();
        let mut cfg = Config::default();
        cfg.general.gaps_in_vertical = None;
        cfg.general.gaps_out_vertical = None;
        cfg.apply(&doc, &mut Vec::new());
        assert_eq!(cfg.general.gaps_in_vertical, None);
        assert_eq!(cfg.general.gaps_out_vertical, None);
        assert_eq!(
            cfg.general.gaps_in_y(),
            3,
            "mirrors the customised horizontal value"
        );
        assert_eq!(
            cfg.general.gaps_out_y(),
            12,
            "mirrors the customised horizontal value"
        );

        let doc: KdlDocument =
            "general { gaps-in 3; gaps-in-vertical 8; gaps-out 12; gaps-out-vertical 1 }\n"
                .parse()
                .unwrap();
        cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        assert_eq!(cfg.general.gaps_in_vertical, Some(8));
        assert_eq!(cfg.general.gaps_in_y(), 8, "an explicit vertical value wins");
        assert_eq!(cfg.general.gaps_out_vertical, Some(1));
        assert_eq!(cfg.general.gaps_out_y(), 1, "an explicit vertical value wins");
    }

    /// Both follow behaviours are on out of the box and both can be turned
    /// off: a move you cannot see reads as a move that did not happen, but
    /// someone who wants the cursor to stay put must be able to say so.
    #[test]
    fn the_follow_behaviours_default_on_and_parse_off() {
        let d = General::default();
        assert!(d.cursor_follows_moved_window);
        assert!(d.follow_window_to_workspace);

        let doc: KdlDocument = r#"
            general { cursor-follows-moved-window #false; follow-window-to-workspace #false }
        "#
        .parse()
        .unwrap();
        let mut cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert!(!cfg.general.cursor_follows_moved_window);
        assert!(!cfg.general.follow_window_to_workspace);
    }

    #[test]
    fn capture_defaults_to_denying_everything() {
        let cfg = Config::default();
        assert!(cfg.capture.allow.is_empty());
        assert!(cfg.capture.redact_app_id.is_empty());
        assert!(cfg.capture.hide_layer.is_empty());
    }

    #[test]
    fn parses_capture_hide_layer() {
        let doc: KdlDocument = r#"
            capture { hide-layer "hyperion:eclipse-eye" "foo:ns:with:colons" }
        "#
        .parse()
        .unwrap();
        let mut cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert_eq!(
            cfg.capture.hide_layer,
            [
                ("hyperion".to_string(), "eclipse-eye".to_string()),
                ("foo".to_string(), "ns:with:colons".to_string()),
            ]
        );
    }

    #[test]
    fn malformed_hide_layer_entries_are_rejected_and_dropped() {
        let doc: KdlDocument = r#"
            capture { hide-layer "hyperion:eclipse-eye" "nocolon" ":ns" "exe:" 7 }
        "#
        .parse()
        .unwrap();
        let mut cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        assert_eq!(cfg.errors.len(), 4, "{:?}", cfg.errors);
        assert_eq!(
            cfg.capture.hide_layer,
            [("hyperion".to_string(), "eclipse-eye".to_string())]
        );
    }

    #[test]
    fn shipped_policy_hides_the_taskbar_eye() {
        let text = include_str!("../../../../packaging/etc/policy.kdl");
        let doc: KdlDocument = text.parse().unwrap();
        let mut cfg = Config {
            cur: Some((policy_src("/etc/eclipse/policy.kdl"), text.to_owned())),
            ..Config::default()
        };
        cfg.apply(&doc, &mut Vec::new());
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert_eq!(
            cfg.capture.hide_layer,
            [("ec-hyperion-bar".to_string(), "eclipse-eye".to_string())]
        );
    }

    #[test]
    fn repeated_hide_layer_is_rejected() {
        let doc: KdlDocument = r#"
            capture { hide-layer "a:b"; hide-layer "c:d" }
        "#
        .parse()
        .unwrap();
        let mut cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
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
        assert_eq!(cfg.general.layout, LayoutKind::Radiant);
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

    #[test]
    fn annotation_chords_parse_and_are_not_bound_by_default() {
        use crate::input::Action;
        for (name, want) in [
            ("annotation-select", Action::AnnotationSelect),
            ("annotation-dismiss", Action::AnnotationDismiss),
            ("annotation-expand", Action::AnnotationExpand),
            ("annotation-auto-toggle", Action::AnnotationAutoToggle),
        ] {
            let doc: KdlDocument = format!("bind \"SUPER CTRL\" \"o\" {{ {name}; }}")
                .parse()
                .unwrap();
            let mut cfg = Config::default();
            let mut binds = Vec::new();
            cfg.apply(&doc, &mut binds);
            assert_eq!(binds[0].action, want, "{name}");
        }
        // The addon is optional, so its chords cost the default config nothing.
        assert!(!default_binds().iter().any(|b| matches!(
            b.action,
            Action::AnnotationSelect
                | Action::AnnotationDismiss
                | Action::AnnotationExpand
                | Action::AnnotationAutoToggle
        )));
    }

    fn gestures(text: &str) -> Config {
        let doc: KdlDocument = text.parse().unwrap();
        let mut cfg = Config {
            cur: Some((abyss_src("a.kdl"), text.to_owned())),
            ..Config::default()
        };
        cfg.apply(&doc, &mut Vec::new());
        cfg
    }

    #[test]
    fn gesture_node_parses() {
        use crate::input::Action;
        let cfg = gestures("gesture \"swipe\" 4 \"up\" { spawn \"foot\"; }\n");
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert_eq!(
            cfg.gesture_for(4, Direction::Up),
            Some(&Action::Spawn("foot".into()))
        );
        assert!(cfg.gesture_bound(4));
        // Additive: the defaults are still there.
        assert_eq!(cfg.gesture_binds.len(), default_gesture_binds().len() + 1);
        assert_eq!(cfg.gesture_for(3, Direction::Left), Some(&Action::WorkspaceNext));
        assert_eq!(cfg.gesture_for(3, Direction::Right), Some(&Action::WorkspacePrev));
        assert!(!cfg.gesture_bound(5));
    }

    #[test]
    fn gesture_rejects_bad_fingers_kind_and_direction() {
        for bad in [
            "gesture \"swipe\" 2 \"left\" { workspace-next; }",
            "gesture \"swipe\" 5 \"left\" { workspace-next; }",
            "gesture \"swipe\" \"3\" \"left\" { workspace-next; }",
            "gesture \"swipe\" 3 \"sideways\" { workspace-next; }",
            "gesture \"pinch\" 3 \"left\" { workspace-next; }",
            "gesture \"swipe\" 3 { workspace-next; }",
            "gesture \"swipe\" 3 \"left\"",
            "gesture \"swipe\" 3 \"left\" { no-such-action; }",
        ] {
            let cfg = gestures(bad);
            assert_eq!(cfg.errors.len(), 1, "{bad}: {:?}", cfg.errors);
            assert_eq!(cfg.gesture_binds, default_gesture_binds(), "{bad}");
        }
    }

    #[test]
    fn config_gesture_overrides_the_default() {
        use crate::input::Action;
        let cfg = gestures(
            "gesture \"swipe\" 3 \"left\" { workspace-prev; }\ngesture \"swipe\" 3 \"left\" { toggle-layout; }\n",
        );
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert_eq!(
            cfg.gesture_binds.len(),
            default_gesture_binds().len(),
            "override, not addition"
        );
        // The later entry wins, the same as a later file over an earlier one.
        assert_eq!(cfg.gesture_for(3, Direction::Left), Some(&Action::ToggleLayout));
        assert_eq!(cfg.gesture_for(3, Direction::Right), Some(&Action::WorkspacePrev));
    }

    #[test]
    fn drag_gesture_defaults_to_super_two_fingers() {
        let cfg = Config::default();
        assert_eq!(cfg.drag_gestures, default_drag_gestures());
        let sup = ModifiersState {
            logo: true,
            ..Default::default()
        };
        assert!(cfg.drag_gesture_bound(2, &sup));
        assert!(!cfg.drag_gesture_bound(2, &ModifiersState::default()));
        assert!(!cfg.drag_gesture_bound(3, &sup));
        // A drag is not a swipe: two fingers stay unbound for swipes.
        assert!(!cfg.gesture_bound(2));
    }

    #[test]
    fn drag_gesture_replaces_the_default_for_the_same_fingers() {
        let cfg = gestures("gesture \"drag\" 2 \"Alt\" { move-window; }\ngesture \"drag\" 4 \"Super Shift\" { move-window; }\n");
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert_eq!(cfg.drag_gestures.len(), 2, "2 replaced, 4 added");
        let alt = ModifiersState {
            alt: true,
            ..Default::default()
        };
        let sup = ModifiersState {
            logo: true,
            ..Default::default()
        };
        assert!(cfg.drag_gesture_bound(2, &alt));
        assert!(!cfg.drag_gesture_bound(2, &sup));
        assert!(cfg.drag_gesture_bound(4, &ModifiersState { shift: true, ..sup }));
    }

    #[test]
    fn drag_gesture_none_disables_it() {
        for off in [
            "gesture \"drag\" 2 { none; }",
            "gesture \"drag\" 2 \"Super\" { none; }",
        ] {
            let cfg = gestures(off);
            assert!(cfg.errors.is_empty(), "{off}: {:?}", cfg.errors);
            assert!(cfg.drag_gestures.is_empty(), "{off}");
        }
        // Disabling a finger count that has no drag is harmless, even one a
        // swipe is bound to.
        let cfg = gestures("gesture \"drag\" 3 { none; }");
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert_eq!(cfg.drag_gestures, default_drag_gestures());
    }

    #[test]
    fn drag_gesture_may_not_share_fingers_with_a_bound_swipe() {
        // The default 3-finger swipes are bound, so a 3-finger drag is refused.
        let cfg = gestures("gesture \"drag\" 3 \"Super\" { move-window; }");
        assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
        assert!(cfg.errors[0]
            .message
            .contains("collides with a bound 3-finger swipe"));
        assert_eq!(cfg.drag_gestures, default_drag_gestures());
        // And the other way round: a swipe on a dragged finger count.
        let cfg = gestures(
            "gesture \"drag\" 4 \"Super\" { move-window; }\ngesture \"swipe\" 4 \"up\" { toggle-layout; }",
        );
        assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
        assert!(cfg.errors[0]
            .message
            .contains("collides with the 4-finger drag gesture"));
        assert!(!cfg.gesture_bound(4));
    }

    #[test]
    fn drag_gesture_rejects_bad_nodes_and_keeps_the_default() {
        for bad in [
            "gesture \"drag\" 2 { move-window; }",
            "gesture \"drag\" 2 \"\" { move-window; }",
            "gesture \"drag\" 2 \"none\" { move-window; }",
            "gesture \"drag\" 1 \"Super\" { move-window; }",
            "gesture \"drag\" 5 \"Super\" { move-window; }",
            "gesture \"drag\" 2 \"Hyper\" { move-window; }",
            "gesture \"drag\" 2 \"Super\" { resize-window; }",
            "gesture \"drag\" 2 \"Super\"",
            "gesture \"drag\" 2 \"Super\" \"left\" { move-window; }",
        ] {
            let cfg = gestures(bad);
            assert_eq!(cfg.errors.len(), 1, "{bad}: {:?}", cfg.errors);
            assert_eq!(cfg.drag_gestures, default_drag_gestures(), "{bad}");
        }
        let cfg = gestures("gesture \"drag\" 2 { move-window; }");
        assert!(cfg.errors[0].message.contains("at least one modifier"));
    }

    #[test]
    fn mousebind_defaults_are_alt_drag() {
        use crate::input::MouseAction;
        let cfg = Config::default();
        let alt = Mods {
            alt: true,
            ..Mods::default()
        };
        let mut state = ModifiersState {
            alt: true,
            ..Default::default()
        };
        assert_eq!(cfg.mouse_bind_for(&state, 0x110), Some(MouseAction::MoveWindow));
        assert_eq!(cfg.mouse_bind_for(&state, 0x111), Some(MouseAction::ResizeWindow));
        assert_eq!(cfg.mouse_bind_for(&state, 0x112), None);
        assert_eq!(cfg.mouse_binds, default_mouse_binds());
        assert!(cfg.mouse_binds.iter().all(|b| b.mods == alt));
        // Exact match: Alt+Shift, and no modifier at all, are not Alt.
        state.shift = true;
        assert_eq!(cfg.mouse_bind_for(&state, 0x110), None);
        assert_eq!(cfg.mouse_bind_for(&ModifiersState::default(), 0x110), None);
    }

    #[test]
    fn mousebind_node_parses_and_extends_the_defaults() {
        use crate::input::{MouseAction, MouseButton};
        let cfg = gestures("mousebind \"SUPER SHIFT\" \"middle\" { resize-window; }\n");
        assert_eq!(cfg.mouse_binds.len(), default_mouse_binds().len() + 1);
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        let added = cfg.mouse_binds.last().unwrap();
        assert_eq!(added.button, MouseButton::Middle);
        assert_eq!(added.action, MouseAction::ResizeWindow);
        assert_eq!(
            added.mods,
            Mods {
                logo: true,
                shift: true,
                ..Mods::default()
            }
        );
    }

    #[test]
    fn mousebind_replaces_the_default_for_the_same_mods_and_button() {
        use crate::input::MouseAction;
        let cfg = gestures(
            "mousebind \"ALT\" \"left\" { resize-window; }\nmousebind \"MOD1\" \"left\" { move-window; }\n",
        );
        // Replaced once by the first node, again by the second: still one entry.
        assert_eq!(cfg.mouse_binds.len(), default_mouse_binds().len());
        let state = ModifiersState {
            alt: true,
            ..Default::default()
        };
        assert_eq!(cfg.mouse_bind_for(&state, 0x110), Some(MouseAction::MoveWindow));
        assert_eq!(cfg.mouse_bind_for(&state, 0x111), Some(MouseAction::ResizeWindow));
        let cfg = gestures("mousebind \"ALT\" \"left\" { resize-window; }\n");
        assert_eq!(cfg.mouse_bind_for(&state, 0x110), Some(MouseAction::ResizeWindow));
    }

    #[test]
    fn mousebind_rejects_bad_nodes_and_keeps_the_defaults() {
        for bad in [
            "mousebind \"Alt\" \"side\" { move-window; }",
            "mousebind \"Alt\" \"left\" { spawn \"foot\"; }",
            "mousebind \"Alt\" \"left\" { }",
            "mousebind \"Alt\" \"left\"",
            "mousebind \"Alt\" { move-window; }",
            "mousebind \"Hyper\" \"left\" { move-window; }",
            "mousebind \"left\" { move-window; }",
            "mousebind \"\" \"left\" { move-window; }",
            "mousebind \"none\" \"left\" { move-window; }",
        ] {
            let cfg = gestures(bad);
            assert!(!cfg.errors.is_empty(), "{bad}");
            assert_eq!(cfg.mouse_binds, default_mouse_binds(), "{bad}");
        }
    }

    #[test]
    fn workspace_step_actions_parse() {
        use crate::input::Action;
        let doc: KdlDocument =
            "bind \"SUPER\" \"n\" { workspace-next; }\nbind \"SUPER\" \"p\" { workspace-prev; }"
                .parse()
                .unwrap();
        let mut binds = Vec::new();
        Config::default().apply(&doc, &mut binds);
        assert_eq!(binds[0].action, Action::WorkspaceNext);
        assert_eq!(binds[1].action, Action::WorkspacePrev);
    }

    #[test]
    fn swipe_direction_takes_the_dominant_axis_past_the_threshold() {
        use crate::input::{swipe_direction, SWIPE_THRESHOLD};
        let t = SWIPE_THRESHOLD;
        assert_eq!(swipe_direction(-t, 0.0, t), Some(Direction::Left));
        assert_eq!(swipe_direction(t * 2.0, t, t), Some(Direction::Right));
        assert_eq!(swipe_direction(10.0, -t * 1.5, t), Some(Direction::Up));
        assert_eq!(swipe_direction(-t, t * 1.1, t), Some(Direction::Down));
        // Short of the threshold on both axes: nothing, however diagonal.
        assert_eq!(swipe_direction(t - 1.0, -(t - 1.0), t), None);
        assert_eq!(swipe_direction(0.0, 0.0, t), None);
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

/// One pointer key of `input` or a `device` block, validated. `Err(None)` is
/// a name this does not know; `Err(Some)` a known name with a bad value.
fn pointer_key(n: &KdlNode) -> Result<PointerKey, Option<String>> {
    let s = arg(n).and_then(KdlValue::as_string);
    match n.name().value() {
        "accel-profile" => match s {
            Some(v @ ("flat" | "adaptive")) => Ok(PointerKey::AccelProfile(v.to_owned())),
            other => Err(Some(format!("unknown accel-profile {other:?}"))),
        },
        "accel-speed" => match arg(n).and_then(as_f64) {
            Some(v) if (-1.0..=1.0).contains(&v) => Ok(PointerKey::AccelSpeed(v)),
            _ => Err(Some("accel-speed must be a number -1.0..=1.0".to_owned())),
        },
        "scroll-method" => match s {
            Some(v @ ("default" | "none" | "on-button-down")) => Ok(PointerKey::ScrollMethod(v.to_owned())),
            other => Err(Some(format!(
                "unknown scroll-method {other:?}; expected default, none or on-button-down"
            ))),
        },
        "calibration" => {
            let vals = args(n);
            let nums: Vec<f32> = vals
                .iter()
                .filter_map(|v| as_f64(v))
                .filter(|v| v.is_finite())
                .map(|v| v as f32)
                .collect();
            match <[f32; 6]>::try_from(nums) {
                Ok(m) if vals.len() == 6 => Ok(PointerKey::Calibration(m)),
                _ => Err(Some(
                    "calibration needs exactly six numbers: a b c d e f".to_owned(),
                )),
            }
        }
        _ => Err(None),
    }
}

/// One `touchpad` key, validated; errors as in [`pointer_key`].
fn touchpad_key(n: &KdlNode) -> Result<TouchpadKey, Option<String>> {
    let name = n.name().value();
    let s = arg(n).and_then(KdlValue::as_string);
    match name {
        "click-method" => {
            return match s {
                Some(v @ ("clickfinger" | "button-areas")) => Ok(TouchpadKey::ClickMethod(v.to_owned())),
                other => Err(Some(format!("unknown click-method {other:?}"))),
            }
        }
        "scroll-method" => {
            return match s {
                Some(v @ ("two-finger" | "edge" | "none")) => Ok(TouchpadKey::ScrollMethod(v.to_owned())),
                other => Err(Some(format!(
                    "unknown touchpad scroll-method {other:?}; expected two-finger, edge or none"
                ))),
            }
        }
        _ => {}
    }
    let Some(b) = arg(n).and_then(KdlValue::as_bool) else {
        return Err(Some(format!("touchpad key needs a boolean: {name:?}")));
    };
    match name {
        "natural-scroll" => Ok(TouchpadKey::NaturalScroll(b)),
        "tap-to-click" => Ok(TouchpadKey::TapToClick(b)),
        "tap-and-drag" => Ok(TouchpadKey::TapAndDrag(b)),
        "dwt" => Ok(TouchpadKey::Dwt(b)),
        _ => Err(None),
    }
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

#[cfg(test)]
mod rounding_tests {
    use super::*;

    #[test]
    fn rounding_parses_and_enables_the_effect_path() {
        let doc: KdlDocument = "decoration {\n    rounding 20\n}\n".parse().expect("kdl parses");
        let mut cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        assert_eq!(cfg.decoration.rounding, 20);
        assert!(cfg.decoration.any_window_effect());
    }
}

/// ADR 0064: an invalid `abyss.kdl` at startup drops the bad nodes and starts;
/// a short fail-closed list still refuses.
#[cfg(test)]
mod startup_tests {
    use super::*;

    fn parse(owner: schema::Owner, text: &str) -> Config {
        let doc: KdlDocument = text.parse().expect("kdl parses");
        let mut cfg = Config {
            cur: Some((
                Source {
                    path: PathBuf::from(match owner {
                        schema::Owner::Abyss => "abyss.kdl",
                        schema::Owner::Policy => "policy.kdl",
                    }),
                    owner,
                },
                text.to_owned(),
            )),
            ..Config::default()
        };
        let mut binds = Vec::new();
        cfg.apply(&doc, &mut binds);
        cfg.cur = None;
        cfg.binds = merge_binds(default_binds(), binds);
        cfg
    }

    fn abyss(text: &str) -> Config {
        parse(schema::Owner::Abyss, text)
    }

    #[test]
    fn an_unknown_key_is_dropped_and_its_neighbours_apply() {
        let mut cfg = abyss("general {\n    gaps-in 7\n    gaps-sideways 3\n}\n");
        assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
        assert!(cfg.errors[0].message.contains("gaps-sideways"));
        assert_eq!(cfg.general.gaps_in, 7, "the valid key applies");
        let d = General::default();
        assert_eq!(cfg.general.gaps_out, d.gaps_out, "untouched keys keep defaults");
        assert_eq!(
            cfg.general.gaps_in_vertical, d.gaps_in_vertical,
            "the untouched vertical key keeps its default"
        );
        assert_eq!(cfg.startup(), Startup::Start { ignored: 1 });
    }

    #[test]
    fn parses_the_new_pointer_keys() {
        let cfg = abyss(
            "input {\n    accel-speed -0.5\n    scroll-method \"on-button-down\"\n    touchpad {\n        \
             tap-and-drag #false\n        scroll-method \"edge\"\n    }\n}\n",
        );
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert_eq!(cfg.input.accel_speed, -0.5);
        assert_eq!(cfg.input.scroll_method, "on-button-down");
        assert!(!cfg.input.touchpad.tap_and_drag);
        assert_eq!(cfg.input.touchpad.scroll_method, "edge");
        // An integer is a number too.
        assert_eq!(abyss("input { accel-speed 1; }").input.accel_speed, 1.0);
    }

    #[test]
    fn bad_pointer_values_are_refused_and_keep_the_default() {
        for (kdl, needle) in [
            ("input { accel-speed 1.5; }", "accel-speed"),
            ("input { accel-speed \"fast\"; }", "accel-speed"),
            ("input { scroll-method \"two-finger\"; }", "scroll-method"),
            (
                "input { touchpad { scroll-method \"on-button-down\"; }; }",
                "scroll-method",
            ),
            ("input { touchpad { tap-and-drag \"yes\"; }; }", "tap-and-drag"),
        ] {
            let cfg = abyss(kdl);
            assert_eq!(cfg.errors.len(), 1, "{kdl}: {:?}", cfg.errors);
            assert!(cfg.errors[0].message.contains(needle), "{kdl}: {:?}", cfg.errors);
            let d = Input::default();
            assert_eq!(cfg.input.accel_speed, d.accel_speed);
            assert_eq!(cfg.input.scroll_method, d.scroll_method);
            assert_eq!(cfg.input.touchpad.scroll_method, d.touchpad.scroll_method);
            assert!(cfg.input.touchpad.tap_and_drag);
        }
    }

    #[test]
    fn parses_input_device_blocks() {
        let cfg = abyss(
            r#"input {
    device "Pad" {
        accel-profile "flat"
        accel-speed 0.25
        touchpad { natural-scroll #true; click-method "button-areas"; scroll-method "none"; }
    }
    device "Screen" { calibration 0 -1 1 1 0.5 0; }
    device "Pad" { accel-speed -1; scroll-method "none"; }
}
"#,
        );
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert_eq!(cfg.input.devices.len(), 2, "one entry per name");
        let pad = &cfg.input.devices[0];
        assert_eq!(pad.name, "Pad");
        assert_eq!(pad.accel_profile.as_deref(), Some("flat"));
        assert_eq!(
            pad.accel_speed,
            Some(-1.0),
            "the later block overrides key by key"
        );
        assert_eq!(pad.scroll_method.as_deref(), Some("none"));
        assert_eq!(pad.touchpad.natural_scroll, Some(true));
        assert_eq!(pad.touchpad.click_method.as_deref(), Some("button-areas"));
        assert_eq!(pad.touchpad.scroll_method.as_deref(), Some("none"));
        assert_eq!(pad.touchpad.tap_to_click, None);
        assert_eq!(pad.calibration, None);
        assert_eq!(
            cfg.input.devices[1].calibration,
            Some([0.0, -1.0, 1.0, 1.0, 0.5, 0.0])
        );
        // The globals are untouched by a device block.
        assert_eq!(cfg.input.accel_profile, "adaptive");
        assert!(!cfg.input.touchpad.natural_scroll);
    }

    #[test]
    fn bad_device_blocks_are_refused() {
        for (kdl, needle) in [
            ("input { calibration 1 0 0 0 1 0; }", "per device"),
            ("input { touchpad { calibration 1 0 0 0 1 0; }; }", "calibration"),
            ("input { device { accel-speed 0; }; }", "device name"),
            ("input { device \"\" { accel-speed 0; }; }", "device name"),
            (
                "input { device \"M\" { calibration 1 0 0 0 1; }; }",
                "six numbers",
            ),
            (
                "input { device \"M\" { calibration 1 0 0 0 1 0 0; }; }",
                "six numbers",
            ),
            (
                "input { device \"M\" { calibration 1 0 \"x\" 0 1 0; }; }",
                "six numbers",
            ),
            ("input { device \"M\" { accel-speed -2; }; }", "accel-speed"),
            (
                "input { device \"M\" { accel-profile \"slow\"; }; }",
                "accel-profile",
            ),
            (
                "input { device \"M\" { kb-layout \"de\"; }; }",
                "unknown input.device key",
            ),
            (
                "input { device \"M\" { touchpad { tap #true; }; }; }",
                "unknown input.device.touchpad key",
            ),
            (
                "input { device \"M\" { touchpad { scroll-method \"sideways\"; }; }; }",
                "scroll-method",
            ),
        ] {
            let cfg = abyss(kdl);
            assert_eq!(cfg.errors.len(), 1, "{kdl}: {:?}", cfg.errors);
            assert!(cfg.errors[0].message.contains(needle), "{kdl}: {:?}", cfg.errors);
            assert!(
                cfg.input
                    .devices
                    .iter()
                    .all(|d| d.calibration.is_none() && d.accel_speed.is_none() && d.accel_profile.is_none()),
                "{kdl}: nothing invalid applied"
            );
        }
    }

    /// The owner's login loop: a touchpad key this build does not know.
    #[test]
    fn an_unknown_touchpad_key_leaves_tap_to_click_alone() {
        let mut cfg = abyss(
            "input {\n    touchpad {\n        tap-to-click #true\n        drag-lock \"sticky\"\n    }\n}\n",
        );
        assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
        assert_eq!(cfg.errors[0].line, 4);
        assert!(cfg.input.touchpad.tap_to_click);
        assert!(!cfg.input.touchpad.natural_scroll && !cfg.input.touchpad.dwt);
        assert_eq!(cfg.startup(), Startup::Start { ignored: 1 });

        let ev = error_event(&cfg.errors, true);
        assert_eq!(
            ev["summary"],
            "abyss.kdl: 1 problem ignored \u{2014} line 4: touchpad key needs a boolean: \"drag-lock\""
        );
        assert_eq!(ev["errors"].as_array().map(Vec::len), Some(1));
        assert_eq!(ev["line"], 4);
    }

    #[test]
    fn a_config_with_no_errors_starts_clean() {
        assert_eq!(
            abyss("general { gaps-in 4; }\n").startup(),
            Startup::Start { ignored: 0 }
        );
    }

    /// Fail-closed case 1: nothing in `policy.kdl` is dropped to a default.
    #[test]
    fn any_policy_kdl_error_refuses() {
        let mut cfg = parse(
            schema::Owner::Policy,
            "clipboard {\n    data-control-alow \"x\"\n}\n",
        );
        assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
        assert_eq!(cfg.startup(), Startup::Refuse { fatal: 1 });

        // Unparseable policy.kdl refuses; unparseable abyss.kdl is dropped whole.
        let bad = "capture { allow \"unterminated\n";
        let mut policy = Config {
            errors: Config::check_text(Path::new("policy.kdl"), schema::Owner::Policy, bad),
            ..Config::default()
        };
        assert_eq!(policy.startup(), Startup::Refuse { fatal: 1 });
        let mut ours = Config {
            errors: Config::check_text(Path::new("abyss.kdl"), schema::Owner::Abyss, bad),
            ..Config::default()
        };
        assert_eq!(ours.startup(), Startup::Start { ignored: 1 });
    }

    /// Fail-closed case 2: a policy-owned key misplaced in `abyss.kdl`.
    #[test]
    fn a_misplaced_policy_key_refuses() {
        for text in [
            "misc {\n    scripted-input #true\n}\n",
            "capture {\n    allow \"grim\"\n}\n",
            "clipboard {\n    data-control-allow \"wl-paste\"\n}\n",
            "windowrule \"no-agent\" {\n    app-id \"keepassxc\"\n}\n",
            "windowrule \"sensitivity secret\" {\n    app-id \"keepassxc\"\n}\n",
            "windowrule \"app-trust trusted\" {\n    app-id \"x\"\n}\n",
            "windowrule \"seat-compat lock\" {\n    app-id \"x\"\n}\n",
            "windowrule \"irreversible-capable false\" {\n    app-id \"x\"\n}\n",
        ] {
            let mut cfg = abyss(text);
            assert_eq!(
                cfg.startup(),
                Startup::Refuse { fatal: 1 },
                "{text}: {:?}",
                cfg.errors
            );
        }
    }

    /// Fail-closed case 3, parse half; the resolve half is in `backend::drm`.
    #[test]
    fn a_rejected_render_device_refuses() {
        let mut cfg = abyss("misc {\n    render-device 1\n}\n");
        assert_eq!(cfg.startup(), Startup::Refuse { fatal: 1 }, "{:?}", cfg.errors);
        // A sibling that is merely misspelt does not.
        let mut cfg = abyss("misc {\n    render-devcie \"/dev/dri/card1\"\n}\n");
        assert_eq!(cfg.startup(), Startup::Start { ignored: 1 }, "{:?}", cfg.errors);
    }

    /// ADR 0064 with ADR 0065: an invalid widget block is dropped and so is
    /// its id in `order`/`important`; the valid block beside it stays.
    #[test]
    fn a_rejected_widget_block_starts_without_it() {
        let text = "bar {\n    widget \"ok\" {\n        exec \"date\"\n    }\n    widget \"bad\" {\n        exec 3\n    }\n    widgets {\n        order \"clock\" \"custom:ok\" \"custom:bad\"\n        important \"custom:bad\"\n    }\n}\n";
        let mut cfg = abyss(text);
        assert_eq!(cfg.startup(), Startup::Start { ignored: 2 }, "{:?}", cfg.errors);
        let names: Vec<&str> = cfg.bar.custom_widgets.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["ok"]);
        assert_eq!(cfg.bar.widgets.order, ["clock", "custom:ok"]);
        assert!(cfg.bar.widgets.important.is_empty());
    }

    fn startup_summary(cfg: &Config) -> String {
        error_event(&cfg.errors, true)["summary"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    fn lock_bind_intact(cfg: &Config) -> bool {
        let sup_shift = m(true, true, false, false);
        cfg.binds.iter().any(|b| {
            b.mods == sup_shift
                && b.key == Keysym::L
                && matches!(&b.action, Action::Spawn(c) if c == "loginctl lock-session")
        })
    }

    /// Owner decision 2026-09-25: a rejected lock setting starts, auto-lock
    /// stays off, and the summary leads with that — ahead of an earlier,
    /// unrelated error, so "(and N more)" cannot hide it.
    #[test]
    fn a_rejected_idle_lock_setting_starts_with_auto_lock_off() {
        for (text, line) in [
            ("general {\n    nope 1\n}\nidle {\n    lock-timeout-seconds -5\n    lock-command \"swaylock\"\n}\n", 5),
            ("general {\n    nope 1\n}\nidle {\n    lock-timeout-seconds 600\n    lock-command 3\n}\n", 6),
        ] {
            let mut cfg = abyss(text);
            assert_eq!(cfg.startup(), Startup::Start { ignored: 2 }, "{text}: {:?}", cfg.errors);
            assert!(
                cfg.idle.lock_command.is_none() || cfg.idle.lock_timeout.is_none(),
                "auto-lock must be off: {:?}",
                cfg.idle
            );
            let s = startup_summary(&cfg);
            assert!(
                s.starts_with(&format!("abyss.kdl: auto-lock is OFF \u{2014} line {line}: ")),
                "{s}"
            );
            assert!(s.ends_with("(and 1 more; `ec-ctl config validate` lists them)"), "{s}");
            assert!(lock_bind_intact(&cfg), "Super+Shift+L must stay bound");
        }
        // DPMS is not a protection; its default (never) is safe and says so
        // in the ordinary way.
        let mut cfg = abyss("idle {\n    dpms-timeout-seconds -5\n}\n");
        assert_eq!(cfg.startup(), Startup::Start { ignored: 1 });
        assert!(startup_summary(&cfg).starts_with("abyss.kdl: 1 problem ignored"));
    }

    /// A misspelt lock key leaves the session never locking just as surely.
    #[test]
    fn a_misspelt_idle_key_warns_auto_lock_off() {
        let mut cfg = abyss("idle {\n    lock-timout-seconds 600\n    lock-command \"swaylock\"\n}\n");
        assert_eq!(cfg.startup(), Startup::Start { ignored: 1 });
        assert!(cfg.idle.lock_timeout.is_none());
        let s = startup_summary(&cfg);
        assert!(
            s.starts_with("abyss.kdl: auto-lock is OFF \u{2014} line 2: "),
            "{s}"
        );
        assert!(lock_bind_intact(&cfg));
    }

    /// So does a misspelt `idle` node (within edit distance 2).
    #[test]
    fn a_misspelt_idle_node_warns_auto_lock_off() {
        let mut cfg = abyss("idel {\n    lock-timeout-seconds 600\n    lock-command \"swaylock\"\n}\n");
        assert_eq!(cfg.startup(), Startup::Start { ignored: 1 });
        assert!(cfg.idle.lock_command.is_none());
        let s = startup_summary(&cfg);
        assert!(
            s.starts_with("abyss.kdl: auto-lock is OFF \u{2014} line 1: "),
            "{s}"
        );
        assert!(lock_bind_intact(&cfg));
        // A node nowhere near `idle` or `xwayland` is only an ordinary error.
        let mut cfg = abyss("generl {\n    gaps-in 1\n}\n");
        cfg.startup();
        assert!(startup_summary(&cfg).starts_with("abyss.kdl: 1 problem ignored"));
    }

    /// The notice only says what is true: if both lock keys still ended up
    /// set (here the misspelt key is an unrelated extra), auto-lock is on.
    #[test]
    fn auto_lock_notice_is_dropped_when_auto_lock_is_on() {
        let mut cfg = abyss(
            "idle {\n    lock-timeout-seconds 600\n    lock-command \"swaylock\"\n    lock-grace 5\n}\n",
        );
        assert_eq!(cfg.startup(), Startup::Start { ignored: 1 });
        assert!(startup_summary(&cfg).starts_with("abyss.kdl: 1 problem ignored"));
    }

    /// ADR 0026 / ADR 0064: any refusal touching `xwayland` starts with it
    /// OFF, the isolating value, never the `enable #true` default.
    #[test]
    fn a_rejected_xwayland_setting_starts_with_xwayland_off() {
        for (text, line) in [
            // misspelt child
            ("xwayland {\n    enabled #false\n}\n", 2),
            // misspelt node
            ("xwyland {\n    enable #false\n}\n", 1),
            // non-bool value
            ("xwayland {\n    enable \"false\"\n}\n", 2),
        ] {
            let mut cfg = abyss(text);
            assert_eq!(cfg.errors.len(), 1, "{text}: {:?}", cfg.errors);
            assert_eq!(cfg.startup(), Startup::Start { ignored: 1 });
            assert!(!cfg.xwayland.enable, "{text}: Xwayland must be off");
            let s = startup_summary(&cfg);
            assert!(
                s.starts_with(&format!("abyss.kdl: Xwayland is OFF \u{2014} line {line}: ")),
                "{s}"
            );
        }
        // A clean `enable #true` still starts it.
        let mut cfg = abyss("xwayland {\n    enable #true\n}\n");
        assert_eq!(cfg.startup(), Startup::Start { ignored: 0 });
        assert!(cfg.xwayland.enable);
    }

    #[test]
    fn both_fail_safes_lead_the_summary() {
        let mut cfg = abyss(
            "general {\n    nope 1\n}\nxwayland {\n    enabled #false\n}\nidle {\n    lock-command 3\n}\n",
        );
        assert_eq!(cfg.startup(), Startup::Start { ignored: 3 });
        let s = startup_summary(&cfg);
        assert!(
            s.starts_with("abyss.kdl: auto-lock is OFF \u{2014} line 8: "),
            "{s}"
        );
        assert!(s.contains("; Xwayland is OFF \u{2014} line 5: "), "{s}");
        assert!(
            s.ends_with("(and 1 more; `ec-ctl config validate` lists them)"),
            "{s}"
        );
        // A failed hot reload changes nothing live, so it claims nothing.
        let r = error_event(&cfg.errors, false)["summary"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(r.starts_with("abyss.kdl: 3 problems, change not applied"), "{r}");
    }

    /// C-00 §4.5 / COMP-04 §6: a dropped `bind` or `idle` node leaves the
    /// human override, agent-attention and the lock bind in force.
    #[test]
    fn dropped_nodes_leave_the_reserved_and_lock_binds_bound() {
        let mut cfg = abyss(concat!(
            "bind \"SUPER\" \"Escape\" { quit; }\n",
            "bind \"SUPER\" \"space\" { quit; }\n",
            "bind \"SUPER+SHIFT\" \"L\" { no-such-action; }\n",
            "idle {\n    dpms-timeout-seconds \"soon\"\n}\n",
        ));
        assert_eq!(cfg.errors.len(), 4, "{:?}", cfg.errors);
        assert_eq!(cfg.startup(), Startup::Start { ignored: 4 });
        let sup = m(true, false, false, false);
        let sup_shift = m(true, true, false, false);
        let has = |mods: Mods, key: Keysym, f: &dyn Fn(&Action) -> bool| {
            cfg.binds
                .iter()
                .any(|b| b.mods == mods && b.key == key && f(&b.action))
        };
        assert!(has(sup, Keysym::Escape, &|a| matches!(a, Action::AgentOverride)));
        assert!(has(sup, Keysym::space, &|a| matches!(a, Action::AgentAttention)));
        assert!(has(sup_shift, Keysym::L, &|a| {
            matches!(a, Action::Spawn(c) if c == "loginctl lock-session")
        }));
    }

    /// A rejected node is dropped whole, never applied as well.
    #[test]
    fn rejected_nodes_do_not_half_apply() {
        let cfg = abyss("bar {\n    tray {\n        pinned \"a\"\n        pinned \"b\"\n    }\n}\n");
        assert_eq!(cfg.errors.len(), 1);
        assert_eq!(cfg.bar.tray.pinned, Some(vec!["a".to_string()]));

        let cfg = abyss("animations {\n    animation \"windows\" duration=\"80ms\" speed=2\n}\n");
        assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
        assert!(cfg.animations.curves.is_empty(), "{:?}", cfg.animations.curves);
    }

    #[test]
    fn the_summary_counts_and_points_at_the_list() {
        let cfg = abyss("general {\n    nope 1\n    nada 2\n}\n");
        let s = error_event(&cfg.errors, true)["summary"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(
            s.starts_with("abyss.kdl: 2 problems ignored \u{2014} line 2: "),
            "{s}"
        );
        assert!(s.contains("and 1 more; `ec-ctl config validate`"), "{s}");
        let r = error_event(&cfg.errors, false)["summary"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(r.contains("change not applied"), "{r}");
    }
}
