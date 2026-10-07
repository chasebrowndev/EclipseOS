// SPDX-License-Identifier: AGPL-3.0-only
//! General settings and the small top-level blocks: layout, allowlist,
//! clipboard, capture, xwayland, idle, wallpaper, annotation colours, Oracle
//! Eyes and setup records (COMP-13 §1.1).

use super::*;

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
pub(crate) const fn rgba8(r: u8, g: u8, b: u8, a: u8) -> [f32; 4] {
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
    pub(crate) fn slot_mut(&mut self, key: &str) -> Option<&mut [f32; 4]> {
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
pub(crate) enum WallpaperKey {
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
