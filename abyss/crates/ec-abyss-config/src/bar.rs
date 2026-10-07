// SPDX-License-Identifier: AGPL-3.0-only
//! `bar`, `launcher`, `ui` and `settings` blocks: the taskbar, its widgets and
//! the launcher (COMP-13 §1.1).

use super::*;

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
    /// Easing for that slide, one of `EASING_CURVES`.
    pub fold_curve: String,
    /// Which edge of every output the bar is anchored to. The layer surface's
    /// anchor is chosen once, at surface creation (COMP-13 §1.1's `restart`
    /// reload class), so a running bar keeps its old edge until relaunched.
    pub position: BarPosition,
    /// `tray { ... }`: which applets and StatusNotifierItems the bar shows.
    pub tray: BarTray,
    /// `pinned-apps`: desktop-entry ids (the `.desktop` basename, no suffix)
    /// pinned to the taskbar, in bar order (ADR 0074). Empty pins nothing.
    pub pinned_apps: Vec<String>,
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

    pub(crate) fn parse(s: &str) -> Option<Self> {
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
    pub(crate) fn new(text: &str) -> Self {
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
pub(crate) struct PendingCustom {
    pub(crate) name: String,
    pub(crate) offset: usize,
    pub(crate) len: usize,
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
            pinned_apps: Vec::new(),
            clock: BarClock::default(),
            popup_anchor: BarPopupAnchor::Cell,
            eye: true,
            widgets: BarWidgets::default(),
            motion: BarMotion::default(),
            custom_widgets: Vec::new(),
        }
    }
}
