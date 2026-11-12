// SPDX-License-Identifier: AGPL-3.0-only
//! How each schema key is presented: its label, the Settings page and group it
//! sits under, the caption beneath it, and extra search keywords (ADR 0107).
//!
//! Paired one-to-one with [`crate::schema::TABLE`], in the same order, so a key and
//! its presentation are reviewed together. Before this table, a key's label
//! lived in `ec-settings`' `schema.rs`, its page in `pane.rs` and its caption
//! in `app.rs`, and each could be missed independently. Now a new key in
//! [`crate::schema::TABLE`] fails `tests::every_schema_key_has_metadata` until it
//! has a row here.
//!
//! Plain `&'static str` data: no iced, no smithay. `page` is the Settings
//! page's command-line path (`ec-settings windows/gaps`), so this crate does
//! not need the app's `Page` enum; `ec-settings` resolves it and tests that
//! every one does. Whether a key is policy-owned is [`crate::schema::Key::owner`],
//! not repeated here.

/// One key's presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    /// The schema path this row describes.
    pub path: &'static str,
    /// The row label in Settings and in its search results.
    pub label: &'static str,
    /// The Settings page, as `section/page` (or `section` for a section that
    /// is one page).
    pub page: &'static str,
    /// The group heading on that page.
    pub group: &'static str,
    /// The fixed line shown beneath the row, if any. A caption that depends on
    /// the key's current value (the blur mode's, the interaction mode's) is
    /// not data and stays with the app.
    pub caption: Option<&'static str>,
    /// Words people search for this key by, beyond its label, path and doc.
    /// Lowercase words joined by single spaces.
    pub keywords: &'static [&'static str],
    /// The label on the page's own pane when that pane words it differently
    /// from search and the generic list (`label`). `None`: same as `label`.
    pub pane_label: Option<&'static str>,
}

const fn m(path: &'static str, label: &'static str, page: &'static str, group: &'static str) -> Meta {
    Meta {
        path,
        label,
        page,
        group,
        caption: None,
        keywords: &[],
        pane_label: None,
    }
}

impl Meta {
    /// This row with its own-pane wording.
    const fn pane(mut self, label: &'static str) -> Self {
        self.pane_label = Some(label);
        self
    }

    /// The label the page's own pane shows.
    pub fn pane_label(&self) -> &'static str {
        self.pane_label.unwrap_or(self.label)
    }
}

const ADAPTIVE_CAPTION: &str =
    "Darkens or lightens glass so text stays readable over bright or dark backgrounds.";
const FRECENCY_CAPTION: &str =
    "History stays on this device and stores no search text, only what was opened and when.";
const SECONDARY_CAPTION: &str =
    "Advanced. Off: the main GPU draws every screen and copies to the other GPU's. On: that GPU draws its own.";

/// Schema keys that may lack a row here. Each entry needs a reason on the line
/// above it. Empty: every key in [`crate::schema::TABLE`] has one.
pub const EXEMPT: &[&str] = &[];

/// One row per [`crate::schema::TABLE`] key, in table order.
pub const META: &[Meta] = &[
    // mode
    m("mode", "Interaction mode", "desktop/mode", "interaction"),
    // components
    m("components.bar", "Bar", "taskbar/appearance", "bar").pane("Show the bar"),
    m("components.launcher", "Launcher", "launcher/style", "launcher"),
    m(
        "components.notifications",
        "Notifications",
        "desktop/components",
        "components",
    ),
    m(
        "components.control-center",
        "Control center",
        "desktop/components",
        "components",
    ),
    // wallpaper
    m("wallpaper.path", "Path", "desktop/wallpaper", "wallpaper"),
    m("wallpaper.mode", "Mode", "desktop/wallpaper", "wallpaper"),
    m("wallpaper.color", "Color", "desktop/wallpaper", "wallpaper"),
    // annotations
    m("annotations.accent", "Accent", "oracle-eyes/colours", "colours"),
    m(
        "annotations.accent-text",
        "Pick letter",
        "oracle-eyes/colours",
        "colours",
    ),
    m("annotations.text", "Text", "oracle-eyes/colours", "colours"),
    m(
        "annotations.panel-tint",
        "Panel tint",
        "oracle-eyes/colours",
        "colours",
    ),
    m(
        "annotations.panel-rim",
        "Panel rim",
        "oracle-eyes/colours",
        "colours",
    ),
    m(
        "annotations.hairline",
        "Hairline",
        "oracle-eyes/colours",
        "colours",
    ),
    m(
        "annotations.region-outline",
        "Region outline",
        "oracle-eyes/colours",
        "colours",
    ),
    m("annotations.leader", "Leader", "oracle-eyes/colours", "colours"),
    m(
        "annotations.selection-dim",
        "Selection dim",
        "oracle-eyes/colours",
        "colours",
    ),
    m("annotations.danger", "Danger", "oracle-eyes/colours", "colours"),
    // oracle-eyes
    m(
        "oracle-eyes.model-command",
        "Model command",
        "oracle-eyes/model-capture",
        "model",
    ),
    m(
        "oracle-eyes.timeout-ms",
        "Answer timeout",
        "oracle-eyes/model-capture",
        "timing",
    ),
    m(
        "oracle-eyes.auto-interval-ms",
        "Automatic interval",
        "oracle-eyes/model-capture",
        "timing",
    ),
    m(
        "oracle-eyes.hold-ms",
        "Answer hold time",
        "oracle-eyes/model-capture",
        "timing",
    ),
    Meta {
        pane_label: None,
        caption: Some("While on, the taskbar eye turns red and shows in screen captures."),
        ..m(
            "oracle-eyes.debug",
            "Debug mode",
            "oracle-eyes/model-capture",
            "debug",
        )
    },
    m(
        "oracle-eyes.models",
        "Models",
        "oracle-eyes/model-capture",
        "timing",
    ),
    m(
        "oracle-eyes.image-mode",
        "Image mode (send screenshot)",
        "oracle-eyes/model-capture",
        "timing",
    ),
    m(
        "oracle-eyes.image-auto-escalate",
        "Escalate weak answers",
        "oracle-eyes/model-capture",
        "timing",
    ),
    Meta {
        pane_label: None,
        caption: Some("A chord like Super+A. \"none\" leaves an action unbound."),
        ..m(
            "oracle-eyes.bind.select",
            "Select a region",
            "oracle-eyes/model-capture",
            "keybinds",
        )
    },
    m(
        "oracle-eyes.bind.dismiss",
        "Dismiss answers",
        "oracle-eyes/model-capture",
        "keybinds",
    ),
    m(
        "oracle-eyes.bind.expand",
        "Expand an answer",
        "oracle-eyes/model-capture",
        "keybinds",
    ),
    m(
        "oracle-eyes.bind.auto-toggle",
        "Automatic mode",
        "oracle-eyes/model-capture",
        "keybinds",
    ),
    m(
        "oracle-eyes.bind.model-switch",
        "Switch model",
        "oracle-eyes/model-capture",
        "keybinds",
    ),
    m(
        "oracle-eyes.bind.image-toggle",
        "Image mode",
        "oracle-eyes/model-capture",
        "keybinds",
    ),
    // general
    m("general.gaps-in", "Between windows", "windows/gaps", "gaps"),
    m("general.gaps-out", "Screen edges", "windows/gaps", "gaps"),
    m("general.gaps-in-vertical", "Between rows", "windows/gaps", "gaps"),
    m(
        "general.gaps-out-vertical",
        "Top and bottom",
        "windows/gaps",
        "gaps",
    ),
    m("general.border-size", "Border size", "windows/borders", "borders"),
    m("general.layout", "Layout", "windows/layout", "layout"),
    m(
        "general.floating-placement",
        "Floating placement",
        "windows/layout",
        "layout",
    ),
    m(
        "general.focus-follows-mouse",
        "Focus follows mouse",
        "windows/focus-guides",
        "focus",
    ),
    m(
        "general.maximize-first-window",
        "Maximize first window on a workspace",
        "windows/focus-guides",
        "new windows",
    ),
    m(
        "general.maximize-first-window-exempt",
        "Exemptions",
        "windows/focus-guides",
        "new windows",
    ),
    m(
        "general.focus-follows-mouse-across-outputs",
        "Focus follows mouse across outputs",
        "windows/focus-guides",
        "focus",
    ),
    m(
        "general.cursor-follows-moved-window",
        "Cursor follows moved window",
        "windows/focus-guides",
        "focus",
    ),
    m(
        "general.follow-window-to-workspace",
        "Follow window to workspace",
        "windows/focus-guides",
        "focus",
    ),
    m(
        "general.unfocus-on-empty-workspace",
        "Unfocus on empty workspace",
        "windows/focus-guides",
        "focus",
    ),
    m(
        "general.refocus-on-scene-change",
        "Refocus on scene change",
        "windows/focus-guides",
        "focus",
    ),
    m(
        "general.col-active-border",
        "Active border colour",
        "windows/borders",
        "borders",
    ),
    m(
        "general.col-inactive-border",
        "Inactive border colour",
        "windows/borders",
        "borders",
    ),
    m(
        "general.drop-guides",
        "Drop guides",
        "windows/focus-guides",
        "drag guides",
    ),
    m(
        "general.drop-guide-color",
        "Drop guide colour",
        "windows/focus-guides",
        "drag guides",
    ),
    m(
        "general.drop-edge-band",
        "Drop edge band",
        "windows/focus-guides",
        "drag guides",
    ),
    m("general.snap", "Snap", "windows/layout", "layout"),
    m(
        "general.snap-distance",
        "Snap distance",
        "windows/layout",
        "layout",
    ),
    m("general.edge-tile", "Edge tile", "windows/layout", "layout"),
    m(
        "general.edge-tile-distance",
        "Edge tile distance",
        "windows/layout",
        "layout",
    ),
    m("general.desktop-rows", "Desktop rows", "windows/layout", "layout"),
    m("general.desktop-wrap", "Desktop wrap", "windows/layout", "layout"),
    m(
        "general.hot-corners.top-left",
        "Top left",
        "windows/layout",
        "layout",
    ),
    m(
        "general.hot-corners.top-right",
        "Top right",
        "windows/layout",
        "layout",
    ),
    m(
        "general.hot-corners.bottom-left",
        "Bottom left",
        "windows/layout",
        "layout",
    ),
    m(
        "general.hot-corners.bottom-right",
        "Bottom right",
        "windows/layout",
        "layout",
    ),
    m(
        "general.hot-corner-dwell",
        "Hot corner dwell",
        "windows/layout",
        "layout",
    ),
    m(
        "general.hot-corner-cooldown",
        "Hot corner cooldown",
        "windows/layout",
        "layout",
    ),
    m(
        "general.color-scheme",
        "Colour scheme",
        "effects/blur-glass",
        "colour scheme",
    ),
    m("general.accent", "Accent", "effects/blur-glass", "colour scheme"),
    m(
        "general.theme-dark",
        "Theme while dark",
        "effects/blur-glass",
        "colour scheme",
    ),
    m(
        "general.theme-light",
        "Theme while light",
        "effects/blur-glass",
        "colour scheme",
    ),
    m(
        "general.light-from",
        "Light from",
        "effects/blur-glass",
        "colour scheme",
    ),
    m(
        "general.dark-from",
        "Dark from",
        "effects/blur-glass",
        "colour scheme",
    ),
    // render
    m(
        "render.direct-scanout",
        "Direct scanout",
        "system/rendering",
        "rendering",
    ),
    // bar
    m(
        "bar.fold-when-inactive",
        "Fold when inactive",
        "taskbar/folding",
        "folding",
    )
    .pane("Fold when inactive"),
    m("bar.fold-height", "Fold height", "taskbar/folding", "folding").pane("Folded height"),
    m(
        "bar.fold-when-idle",
        "Fold when idle",
        "taskbar/folding",
        "folding",
    )
    .pane("Fold when idle"),
    m("bar.idle-seconds", "Idle seconds", "taskbar/folding", "folding").pane("Idle after, s"),
    m(
        "bar.fold-duration-ms",
        "Fold duration ms",
        "taskbar/folding",
        "folding",
    )
    .pane("Fold duration, ms"),
    m("bar.fold-curve", "Fold curve", "taskbar/folding", "folding").pane("Fold curve"),
    m("bar.position", "Position", "taskbar/appearance", "appearance").pane("Position"),
    m("bar.tray.pinned", "Pinned", "taskbar/tray", "tray"),
    m(
        "bar.pinned-apps",
        "Pinned apps",
        "taskbar/appearance",
        "appearance",
    ),
    m("bar.tray.hidden", "Hidden", "taskbar/tray", "tray"),
    m("bar.rounding", "Rounding", "taskbar/appearance", "appearance").pane("Corner radius"),
    m("bar.clock.hour-12", "Hour 12", "taskbar/widgets", "widgets").pane("12-hour time"),
    m("bar.clock.date-mdy", "Date mdy", "taskbar/widgets", "widgets").pane("Month / day / year"),
    m("bar.clock.week-start", "Week start", "taskbar/widgets", "widgets").pane("Week starts on"),
    m(
        "bar.clock.week-numbers",
        "Week numbers",
        "taskbar/widgets",
        "widgets",
    )
    .pane("Week numbers"),
    m(
        "bar.popup-anchor",
        "Popup anchor",
        "taskbar/appearance",
        "appearance",
    )
    .pane("Popups open"),
    m("bar.eye", "Eye indicator", "taskbar/appearance", "appearance"),
    m("bar.widgets.order", "Order", "taskbar/widgets", "widgets"),
    m("bar.widgets.important", "Important", "taskbar/widgets", "widgets"),
    m("bar.widgets.rounding", "Rounding", "taskbar/widgets", "widgets").pane("Corner radius"),
    m("bar.widgets.now-playing.art", "Art", "taskbar/widgets", "widgets").pane("Album art"),
    m(
        "bar.widgets.now-playing.visualizer",
        "Visualizer",
        "taskbar/widgets",
        "widgets",
    )
    .pane("Visualizer"),
    m(
        "bar.widgets.now-playing.remote-art",
        "Remote art",
        "taskbar/widgets",
        "widgets",
    )
    .pane("Fetch remote art"),
    m(
        "bar.widgets.system-usage.interval-ms",
        "Interval ms",
        "taskbar/widgets",
        "widgets",
    )
    .pane("Refresh, ms"),
    m(
        "bar.widgets.system-usage.gpu",
        "Gpu",
        "taskbar/widgets",
        "widgets",
    )
    .pane("GPU meter"),
    m(
        "bar.widgets.system-usage.disk",
        "Disk",
        "taskbar/widgets",
        "widgets",
    )
    .pane("Disk meter"),
    m(
        "bar.widgets.system-usage.disk-path",
        "Disk path",
        "taskbar/widgets",
        "widgets",
    )
    .pane("Disk to measure"),
    m("bar.widgets.volume.step", "Step", "taskbar/widgets", "widgets").pane("Step, %"),
    m(
        "bar.widgets.volume.scroll",
        "Scroll",
        "taskbar/widgets",
        "widgets",
    )
    .pane("Scroll to adjust"),
    m(
        "bar.widgets.volume.max-percent",
        "Max percent",
        "taskbar/widgets",
        "widgets",
    )
    .pane("Loudest, %"),
    m("bar.motion.enabled", "Enabled", "taskbar/motion", "motion").pane("Animate"),
    m(
        "bar.motion.duration-ms",
        "Duration ms",
        "taskbar/motion",
        "motion",
    )
    .pane("Duration, ms"),
    m("bar.motion.curve", "Curve", "taskbar/motion", "motion").pane("Curve"),
    // decoration
    m(
        "decoration.rounding",
        "Rounding",
        "effects/rounding-opacity",
        "windows",
    ),
    m(
        "decoration.titlebar-height",
        "Titlebar height",
        "effects/rounding-opacity",
        "windows",
    ),
    m(
        "decoration.active-opacity",
        "Active window opacity",
        "effects/rounding-opacity",
        "windows",
    ),
    m(
        "decoration.inactive-opacity",
        "Inactive window opacity",
        "effects/rounding-opacity",
        "windows",
    ),
    m(
        "decoration.dim-inactive",
        "Dim inactive windows",
        "effects/rounding-opacity",
        "windows",
    ),
    m("decoration.blur.mode", "Mode", "effects/blur-glass", "blur"),
    m("decoration.blur.size", "Size", "effects/blur-glass", "blur"),
    m("decoration.blur.passes", "Passes", "effects/blur-glass", "blur"),
    m(
        "decoration.blur.glass.refraction",
        "Glass refraction",
        "effects/blur-glass",
        "blur",
    ),
    m(
        "decoration.blur.glass.bevel",
        "Glass bevel",
        "effects/blur-glass",
        "blur",
    ),
    m(
        "decoration.blur.glass.dispersion",
        "Glass dispersion",
        "effects/blur-glass",
        "blur",
    ),
    m(
        "decoration.blur.glass.rim",
        "Glass rim light",
        "effects/blur-glass",
        "blur",
    ),
    Meta {
        pane_label: None,
        caption: Some(ADAPTIVE_CAPTION),
        ..m(
            "decoration.blur.adaptive.enabled",
            "Adaptive glass",
            "effects/blur-glass",
            "blur",
        )
    },
    m(
        "decoration.blur.adaptive.strength",
        "Adaptive strength",
        "effects/blur-glass",
        "blur",
    ),
    m(
        "decoration.blur.frost.tint",
        "Frost tint",
        "effects/blur-glass",
        "blur",
    ),
    m(
        "decoration.surface.tint",
        "Window tint",
        "effects/blur-glass",
        "surface",
    ),
    Meta {
        pane_label: None,
        caption: Some("Hairline cards, with blur off or blur. Glass and frost keep their glass panel."),
        ..m(
            "decoration.surface.card-fill",
            "Card fill",
            "effects/blur-glass",
            "surface",
        )
    },
    m(
        "decoration.surface.inset-fill",
        "Inset fill",
        "effects/blur-glass",
        "surface",
    ),
    m(
        "decoration.shadow.enabled",
        "Enabled",
        "effects/shadow-glow",
        "shadow",
    ),
    m(
        "decoration.shadow.range",
        "Range",
        "effects/shadow-glow",
        "shadow",
    ),
    m(
        "decoration.glow.enabled",
        "Enabled",
        "effects/shadow-glow",
        "glow",
    ),
    m(
        "decoration.glow.active",
        "Glow on the active window",
        "effects/shadow-glow",
        "glow",
    ),
    m(
        "decoration.glow.inactive",
        "Glow on inactive windows",
        "effects/shadow-glow",
        "glow",
    ),
    m(
        "decoration.glow.strength",
        "Strength",
        "effects/shadow-glow",
        "glow",
    ),
    // animations
    m("animations.preset", "Preset", "effects/animations", "animations"),
    m("animations.speed", "Speed", "effects/animations", "animations"),
    m(
        "animations.reduce-motion",
        "Reduce motion",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.window-open.style",
        "Style",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.window-open.duration-ms",
        "Duration ms",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.window-open.curve",
        "Curve",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.window-close.style",
        "Style",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.window-close.duration-ms",
        "Duration ms",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.window-close.curve",
        "Curve",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.window-move.style",
        "Style",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.window-move.duration-ms",
        "Duration ms",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.window-move.curve",
        "Curve",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.workspace-switch.style",
        "Style",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.workspace-switch.duration-ms",
        "Duration ms",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.workspace-switch.curve",
        "Curve",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.window-to-workspace.style",
        "Style",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.window-to-workspace.duration-ms",
        "Duration ms",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.window-to-workspace.curve",
        "Curve",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.minimize.style",
        "Style",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.minimize.duration-ms",
        "Duration ms",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.minimize.curve",
        "Curve",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.unminimize.style",
        "Style",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.unminimize.duration-ms",
        "Duration ms",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.unminimize.curve",
        "Curve",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.fullscreen.style",
        "Style",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.fullscreen.duration-ms",
        "Duration ms",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.fullscreen.curve",
        "Curve",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.layer-open.style",
        "Style",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.layer-open.duration-ms",
        "Duration ms",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.layer-open.curve",
        "Curve",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.layer-close.style",
        "Style",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.layer-close.duration-ms",
        "Duration ms",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.layer-close.curve",
        "Curve",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.focus.style",
        "Style",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.focus.duration-ms",
        "Duration ms",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.focus.curve",
        "Curve",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.chip-add.style",
        "Style",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.chip-add.duration-ms",
        "Duration ms",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.chip-add.curve",
        "Curve",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.chip-remove.style",
        "Style",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.chip-remove.duration-ms",
        "Duration ms",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.chip-remove.curve",
        "Curve",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.bar-layout.style",
        "Style",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.bar-layout.duration-ms",
        "Duration ms",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.bar-layout.curve",
        "Curve",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.toast.style",
        "Style",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.toast.duration-ms",
        "Duration ms",
        "effects/animations",
        "animations",
    ),
    m(
        "animations.toast.curve",
        "Curve",
        "effects/animations",
        "animations",
    ),
    // xwayland
    m(
        "xwayland.enable",
        "Xwayland enabled",
        "system/xwayland",
        "xwayland",
    ),
    m("xwayland.scaling", "Scaling", "system/xwayland", "xwayland"),
    // idle
    m(
        "idle.dpms-timeout-seconds",
        "Screen off after (seconds)",
        "session",
        "idle",
    ),
    m(
        "idle.lock-timeout-seconds",
        "Lock after (seconds)",
        "session",
        "idle",
    ),
    m("idle.lock-command", "Lock command", "session", "idle"),
    m(
        "idle.suspend-timeout-seconds",
        "Suspend after (seconds)",
        "session",
        "idle",
    ),
    m(
        "idle.lock-before-suspend",
        "Lock before suspending",
        "session",
        "idle",
    ),
    // input
    m("input.kb-layout", "Keyboard layout", "input/keyboard", "keyboard"),
    m(
        "input.kb-variant",
        "Keyboard variant",
        "input/keyboard",
        "keyboard",
    ),
    m(
        "input.kb-options",
        "Keyboard options",
        "input/keyboard",
        "keyboard",
    ),
    m(
        "input.repeat-rate",
        "Key repeat rate",
        "input/keyboard",
        "keyboard",
    ),
    m(
        "input.repeat-delay",
        "Key repeat delay",
        "input/keyboard",
        "keyboard",
    ),
    m(
        "input.accel-profile",
        "Pointer acceleration",
        "input/pointer",
        "pointer",
    ),
    m("input.accel-speed", "Accel speed", "input/pointer", "pointer"),
    m("input.scroll-method", "Scroll method", "input/pointer", "pointer"),
    m(
        "input.touchpad.natural-scroll",
        "Natural scroll",
        "input/touchpad",
        "touchpad",
    ),
    m(
        "input.touchpad.tap-to-click",
        "Tap to click",
        "input/touchpad",
        "touchpad",
    ),
    m(
        "input.touchpad.dwt",
        "Disable while typing",
        "input/touchpad",
        "touchpad",
    ),
    m(
        "input.touchpad.click-method",
        "Click method",
        "input/touchpad",
        "touchpad",
    ),
    m(
        "input.touchpad.tap-and-drag",
        "Tap and drag",
        "input/touchpad",
        "touchpad",
    ),
    m(
        "input.touchpad.scroll-method",
        "Scroll method",
        "input/touchpad",
        "touchpad",
    ),
    // launcher
    m("launcher.style", "Launcher style", "launcher/style", "launcher"),
    m(
        "launcher.centered.width",
        "Width",
        "launcher/centered",
        "centered",
    ),
    m(
        "launcher.centered.max-rows",
        "Max rows",
        "launcher/centered",
        "centered",
    ),
    m(
        "launcher.centered.anchor",
        "Anchor",
        "launcher/centered",
        "centered",
    ),
    m(
        "launcher.menu.max-rows",
        "Max rows",
        "launcher/in-bar-menu",
        "in-bar menu",
    ),
    m(
        "launcher.search.path-binaries",
        "Programs on PATH",
        "launcher/search",
        "search",
    ),
    Meta {
        pane_label: None,
        caption: Some("Opened in the terminal command above."),
        ..m(
            "launcher.search.terminal-apps",
            "Terminal apps",
            "launcher/search",
            "search",
        )
    },
    m(
        "launcher.search.match-descriptions",
        "Match descriptions",
        "launcher/search",
        "search",
    ),
    Meta {
        pane_label: None,
        caption: Some(FRECENCY_CAPTION),
        ..m(
            "launcher.search.frecency",
            "Rank by recent use",
            "launcher/search",
            "search",
        )
    },
    m(
        "launcher.bind.open",
        "Open launcher",
        "launcher/keybinds",
        "keybinds",
    ),
    m(
        "launcher.bind.run",
        "Run command",
        "launcher/keybinds",
        "keybinds",
    ),
    // ui
    Meta {
        pane_label: None,
        caption: Some("Keyboard hints in the launcher and start menu"),
        ..m("ui.show-key-hints", "Show controls", "system/general", "general")
    },
    // settings
    Meta {
        pane_label: None,
        caption: Some(FRECENCY_CAPTION),
        ..m(
            "settings.search.frecency",
            "Rank by recent use",
            "system/general",
            "search",
        )
    },
    // misc
    m(
        "misc.render-device",
        "Render device",
        "system/rendering",
        "rendering",
    ),
    m("misc.gpu-mode", "Graphics mode", "system/rendering", "graphics"),
    Meta {
        pane_label: None,
        caption: Some(SECONDARY_CAPTION),
        ..m(
            "misc.secondary-compositing",
            "Render second GPU's screens natively",
            "system/rendering",
            "graphics",
        )
    },
    m(
        "misc.discrete-gpu-apps",
        "Discrete gpu apps",
        "system/rendering",
        "graphics",
    ),
    m(
        "misc.terminal-command",
        "Terminal command",
        "launcher/search",
        "search",
    ),
    // setup
    m("setup.profile", "Profile", "system/setup", "setup"),
    m("setup.complete", "Setup complete", "system/setup", "setup"),
    m("setup.pending-preset", "Pending preset", "system/setup", "setup"),
    // misc
    m(
        "misc.scripted-input",
        "Scripted input",
        "privacy",
        "input scripting",
    ),
    // clipboard
    m(
        "clipboard.data-control-allow",
        "Data control allow",
        "privacy",
        "clipboard",
    ),
    // capture
    m("capture.allow", "Allow", "privacy", "capture"),
    m("capture.redact-app-id", "Redact app IDs", "privacy", "capture"),
    m("capture.hide-layer", "Hide layer", "privacy", "capture"),
    // input-method / virtual-pointer (ADR 0094)
    m("input-method.allow", "Allow", "privacy", "input method"),
    m("virtual-pointer.allow", "Allow", "privacy", "virtual pointer"),
];

/// Where a key that is not in [`META`] lands: one the compositor grew after
/// this build of Settings, keyed by its top-level node. A key of this build is
/// never placed by this table (`tests::every_schema_key_has_metadata`); a
/// node absent here leaves such a key unplaced.
pub const NODE_FALLBACK: &[(&str, &str, &str)] = &[
    ("components", "desktop/components", "components"),
    ("wallpaper", "desktop/wallpaper", "wallpaper"),
    ("general", "windows/layout", "layout"),
    ("decoration", "effects/rounding-opacity", "windows"),
    ("animations", "effects/animations", "animations"),
    ("bar", "taskbar/appearance", "appearance"),
    ("launcher", "launcher/style", "launcher"),
    ("input", "input/pointer", "pointer"),
    ("idle", "session", "idle"),
    ("render", "system/rendering", "rendering"),
    ("xwayland", "system/xwayland", "xwayland"),
    ("setup", "system/setup", "setup"),
    ("ui", "system/general", "general"),
    ("settings", "system/general", "search"),
    ("clipboard", "privacy", "clipboard"),
    ("capture", "privacy", "capture"),
    ("oracle-eyes", "oracle-eyes/model-capture", "timing"),
    ("annotations", "oracle-eyes/colours", "colours"),
];

/// The row for `path`, if this build has one.
pub fn get(path: &str) -> Option<&'static Meta> {
    META.iter().find(|m| m.path == path)
}

/// `(page, group)` for `path`: its own row, else its node's fallback.
pub fn place(path: &str) -> Option<(&'static str, &'static str)> {
    if let Some(m) = get(path) {
        return Some((m.page, m.group));
    }
    let node = path.split('.').next().unwrap_or(path);
    NODE_FALLBACK
        .iter()
        .find(|(n, _, _)| *n == node)
        .map(|(_, page, group)| (*page, *group))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema;

    /// The taskbar pane's labels, as they read before they moved onto `META`.
    #[test]
    fn pane_labels_match_the_old_taskbar_table() {
        let old: &[(&str, &str)] = &[
            ("components.bar", "Show the bar"),
            ("bar.position", "Position"),
            ("bar.rounding", "Corner radius"),
            ("bar.widgets.rounding", "Corner radius"),
            ("bar.popup-anchor", "Popups open"),
            ("bar.fold-when-inactive", "Fold when inactive"),
            ("bar.fold-when-idle", "Fold when idle"),
            ("bar.idle-seconds", "Idle after, s"),
            ("bar.fold-height", "Folded height"),
            ("bar.fold-duration-ms", "Fold duration, ms"),
            ("bar.fold-curve", "Fold curve"),
            ("bar.widgets.now-playing.art", "Album art"),
            ("bar.widgets.now-playing.visualizer", "Visualizer"),
            ("bar.widgets.now-playing.remote-art", "Fetch remote art"),
            ("bar.widgets.system-usage.interval-ms", "Refresh, ms"),
            ("bar.widgets.system-usage.gpu", "GPU meter"),
            ("bar.widgets.system-usage.disk", "Disk meter"),
            ("bar.widgets.system-usage.disk-path", "Disk to measure"),
            ("bar.widgets.volume.step", "Step, %"),
            ("bar.widgets.volume.scroll", "Scroll to adjust"),
            ("bar.widgets.volume.max-percent", "Loudest, %"),
            ("bar.clock.hour-12", "12-hour time"),
            ("bar.clock.date-mdy", "Month / day / year"),
            ("bar.clock.week-start", "Week starts on"),
            ("bar.clock.week-numbers", "Week numbers"),
            ("bar.motion.enabled", "Animate"),
            ("bar.motion.duration-ms", "Duration, ms"),
            ("bar.motion.curve", "Curve"),
        ];
        for (path, label) in old {
            assert_eq!(get(path).map(Meta::pane_label), Some(*label), "{path}");
        }
        // A key with no own-pane wording reads as its generic label.
        let plain = get("bar.position").unwrap();
        assert_eq!(plain.pane_label(), plain.label);
    }

    #[test]
    fn every_schema_key_has_metadata() {
        for key in schema::TABLE {
            assert!(
                get(key.path).is_some() || EXEMPT.contains(&key.path),
                "{} has no row in meta::META: give it a label, page and group",
                key.path
            );
        }
    }

    #[test]
    fn every_row_is_a_schema_key_in_table_order() {
        let rows: Vec<&str> = META.iter().map(|m| m.path).collect();
        let keys: Vec<&str> = schema::TABLE
            .iter()
            .map(|k| k.path)
            .filter(|p| !EXEMPT.contains(p))
            .collect();
        assert_eq!(
            rows, keys,
            "meta::META must list schema::TABLE's keys, once each, in its order"
        );
        for e in EXEMPT {
            assert!(
                schema::get_key(e).is_some(),
                "{e} is exempted but not in the schema"
            );
            assert!(
                get(e).is_none(),
                "{e} is exempted and has a row: drop the exemption"
            );
        }
    }

    #[test]
    fn every_row_is_filled_in() {
        for m in META {
            assert!(
                m.label.starts_with(char::is_uppercase),
                "{} label {:?}",
                m.path,
                m.label
            );
            assert!(
                !m.page.is_empty() && !m.group.is_empty(),
                "{} has no place",
                m.path
            );
            assert!(
                m.caption.is_none_or(|c| !c.is_empty()),
                "{} has an empty caption",
                m.path
            );
            for w in m.keywords {
                let normal: Vec<String> = w
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|s| !s.is_empty())
                    .map(str::to_lowercase)
                    .collect();
                assert_eq!(normal.join(" "), *w, "{} keyword {w:?} is not normalised", m.path);
            }
        }
    }

    #[test]
    fn a_fallback_node_is_a_schema_node() {
        for (node, _, _) in NODE_FALLBACK {
            assert!(
                schema::TABLE
                    .iter()
                    .any(|k| k.path.split('.').next() == Some(node)),
                "{node} falls back for a node the schema does not have"
            );
        }
    }
}
