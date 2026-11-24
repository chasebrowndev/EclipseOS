// SPDX-License-Identifier: AGPL-3.0-only
//! Where a config key appears: a [`Section`] in the sidebar, one of its
//! [`Page`]s, and a group heading on that page.
//!
//! The assignment is not made here: every schema key names its page and group
//! in `ec_abyss_config::meta`, beside its label (ADR 0107). This module owns
//! the sidebar's shape and resolves those names. A key with no row there
//! fails `ec_abyss_config`'s own tests, and `tests/coverage.rs` fails one that
//! lands nowhere, rather than let it quietly become unreachable from the GUI
//! (F-01 §4).
//!
//! Each topic lives in one place. A key is placed by what it changes, not by
//! the config node it happens to be spelled under: `components.launcher` and
//! `misc.terminal-command` are the launcher's.

use ec_ui::widget::art::{Glyph, Hue};

/// A top-level entry in the sidebar. Sections with more than one page expand
/// in place to list them; the rest are a page of their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    /// The landing page: a summary of this machine and the settings reached
    /// for most. Not a settings page; no config key lands on it.
    Overview,
    Windows,
    Effects,
    Desktop,
    Taskbar,
    Launcher,
    Display,
    Network,
    Input,
    Session,
    System,
    Privacy,
    Accounts,
    Addons,
    OracleEyes,
    Terminal,
}

/// One short page of settings: what the content column shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Page {
    /// The landing page.
    Overview,
    // Windows
    Layout,
    Gaps,
    Borders,
    Focus,
    // Effects
    Blur,
    Shadow,
    Rounding,
    Animations,
    Graphics,
    // Desktop
    Mode,
    Components,
    Wallpaper,
    LoginScreen,
    LockScreen,
    // Taskbar
    BarAppearance,
    BarWidgets,
    BarFolding,
    BarTray,
    BarMotion,
    // Launcher
    LauncherStyle,
    LauncherCentered,
    LauncherMenu,
    LauncherSearch,
    LauncherKeybinds,
    // Single pages
    Display,
    Network,
    // Input
    Keyboard,
    Pointer,
    Touchpad,
    Session,
    // System
    Rendering,
    Xwayland,
    Setup,
    General,
    Privacy,
    Accounts,
    Addons,
    // Oracle Eyes
    OeGeneral,
    OeColours,
    // Terminal (Apotheosis)
    TermFont,
    TermColors,
    TermWindow,
    TermScrollback,
    TermInput,
    TermShell,
    TermBinds,
    TermProfiles,
    TermRules,
    TermSecurity,
    TermMedia,
}

/// A heading in the sidebar: a few families of sections, named the way an
/// operating system's own settings name them. Presentation only. A key's page
/// path (`effects/blur-glass`) never includes it, so no path, CLI argument or
/// config key changes with how the sidebar is grouped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Group {
    Look,
    Devices,
    System,
    Privacy,
    Apps,
}

impl Group {
    /// Sidebar order.
    pub const ALL: &'static [Group] = &[
        Group::Look,
        Group::Devices,
        Group::System,
        Group::Privacy,
        Group::Apps,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Group::Look => "Look & feel",
            Group::Devices => "Devices",
            Group::System => "System",
            Group::Privacy => "Privacy & accounts",
            Group::Apps => "Apps & add-ons",
        }
    }

    /// The heading in a narrow sidebar, where a long one would wrap mid-word.
    pub fn short_title(self) -> &'static str {
        match self {
            Group::Privacy => "Privacy",
            Group::Apps => "Apps",
            other => other.title(),
        }
    }

    /// The sections under this heading, in sidebar order.
    pub fn sections(self) -> impl Iterator<Item = Section> {
        Section::ALL
            .iter()
            .copied()
            .filter(move |s| s.group() == Some(self))
    }

    pub fn hue(self) -> Hue {
        match self {
            Group::Look => Hue::Look,
            Group::Devices => Hue::Devices,
            Group::System => Hue::System,
            Group::Privacy => Hue::Privacy,
            Group::Apps => Hue::Apps,
        }
    }
}

impl Section {
    /// Sidebar order.
    pub const ALL: &'static [Section] = &[
        Section::Overview,
        // Look & feel
        Section::Windows,
        Section::Effects,
        Section::Desktop,
        Section::Taskbar,
        Section::Launcher,
        // Devices
        Section::Display,
        Section::Input,
        Section::Network,
        // System
        Section::Session,
        Section::System,
        // Privacy & accounts
        Section::Privacy,
        Section::Accounts,
        // Apps & add-ons
        Section::Terminal,
        Section::Addons,
        Section::OracleEyes,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Section::Overview => "Overview",
            Section::Windows => "Windows",
            Section::Effects => "Effects",
            Section::Desktop => "Desktop",
            Section::Taskbar => "Taskbar",
            Section::Launcher => "Launcher",
            Section::Display => "Display",
            Section::Network => "Network",
            Section::Input => "Input",
            Section::Session => "Session",
            Section::System => "System",
            Section::Privacy => "Privacy",
            Section::Accounts => "Accounts",
            Section::Addons => "Add-ons",
            Section::OracleEyes => "Oracle Eyes",
            Section::Terminal => "Terminal",
        }
    }

    /// Its pages, in sidebar order. Never empty; the first is what a click
    /// on the section opens.
    pub fn pages(self) -> &'static [Page] {
        match self {
            Section::Overview => &[Page::Overview],
            Section::Windows => &[Page::Layout, Page::Gaps, Page::Borders, Page::Focus],
            Section::Effects => &[
                Page::Blur,
                Page::Shadow,
                Page::Rounding,
                Page::Animations,
                Page::Graphics,
            ],
            Section::Desktop => &[
                Page::Mode,
                Page::Components,
                Page::Wallpaper,
                Page::LoginScreen,
                Page::LockScreen,
            ],
            Section::Taskbar => &[
                Page::BarAppearance,
                Page::BarWidgets,
                Page::BarFolding,
                Page::BarTray,
                Page::BarMotion,
            ],
            Section::Launcher => &[
                Page::LauncherStyle,
                Page::LauncherCentered,
                Page::LauncherMenu,
                Page::LauncherSearch,
                Page::LauncherKeybinds,
            ],
            Section::Display => &[Page::Display],
            Section::Network => &[Page::Network],
            Section::Input => &[Page::Keyboard, Page::Pointer, Page::Touchpad],
            Section::Session => &[Page::Session],
            Section::System => &[Page::Rendering, Page::Xwayland, Page::Setup, Page::General],
            Section::Privacy => &[Page::Privacy],
            Section::Accounts => &[Page::Accounts],
            Section::Addons => &[Page::Addons],
            Section::OracleEyes => &[Page::OeGeneral, Page::OeColours],
            Section::Terminal => Page::TERMINAL,
        }
    }

    /// The heading this section sits under; the overview sits above them all.
    pub fn group(self) -> Option<Group> {
        match self {
            Section::Overview => None,
            Section::Windows | Section::Effects | Section::Desktop | Section::Taskbar | Section::Launcher => {
                Some(Group::Look)
            }
            Section::Display | Section::Input | Section::Network => Some(Group::Devices),
            Section::Session | Section::System => Some(Group::System),
            Section::Privacy | Section::Accounts => Some(Group::Privacy),
            Section::Terminal | Section::Addons | Section::OracleEyes => Some(Group::Apps),
        }
    }

    pub fn hue(self) -> Hue {
        self.group().map_or(Hue::Home, Group::hue)
    }

    /// The picture on the section's icon tile.
    pub fn glyph(self) -> Glyph {
        match self {
            Section::Overview => Glyph::Overview,
            Section::Windows => Glyph::Tiles,
            Section::Effects => Glyph::Panes,
            Section::Desktop => Glyph::Landscape,
            Section::Taskbar => Glyph::Bar,
            Section::Launcher => Glyph::Search,
            Section::Display => Glyph::Monitor,
            Section::Network => Glyph::Wifi,
            Section::Input => Glyph::Keycap,
            Section::Session => Glyph::Power,
            Section::System => Glyph::Chip,
            Section::Privacy => Glyph::Lock,
            Section::Accounts => Glyph::Person,
            Section::Addons => Glyph::Cube,
            Section::OracleEyes => Glyph::Eye,
            Section::Terminal => Glyph::Prompt,
        }
    }

    /// A short line under the section's name on the overview.
    pub fn blurb(self) -> &'static str {
        match self {
            Section::Overview => "This computer at a glance",
            Section::Windows => "Tiling, gaps, borders and focus",
            Section::Effects => "Glass, shadow, corners and motion",
            Section::Desktop => "Mode, wallpaper and login screen",
            Section::Taskbar => "The bar, its widgets and tray",
            Section::Launcher => "How apps are found and opened",
            Section::Display => "Monitors, modes and scale",
            Section::Input => "Keyboard, pointer and touchpad",
            Section::Network => "Wifi and paired devices",
            Section::Session => "Idle, lock and power",
            Section::System => "Rendering, X11 apps and defaults",
            Section::Privacy => "What apps may capture or inject",
            Section::Accounts => "Accounts a task can sign in as",
            Section::Terminal => "Font, colours, keys and profiles",
            Section::Addons => "Optional packages and their hooks",
            Section::OracleEyes => "What the add-on sees and asks",
        }
    }

    /// The page a click on the section opens.
    pub fn first(self) -> Page {
        self.pages()[0]
    }

    /// Whether the section expands to list its pages, or is one page itself.
    pub fn expands(self) -> bool {
        self.pages().len() > 1
    }

    /// The section named on the command line — its title, any case, hyphens
    /// optional (`addons` finds "Add-ons").
    pub fn from_arg(arg: &str) -> Option<Section> {
        Section::ALL
            .iter()
            .copied()
            .find(|s| bare(s.title()) == bare(arg))
    }
}

impl Page {
    /// Every page, in sidebar order.
    pub fn all() -> impl Iterator<Item = Page> {
        Section::ALL.iter().flat_map(|s| s.pages().iter().copied())
    }

    pub fn section(self) -> Section {
        match self {
            Page::Overview => Section::Overview,
            Page::Layout | Page::Gaps | Page::Borders | Page::Focus => Section::Windows,
            Page::Blur | Page::Shadow | Page::Rounding | Page::Animations | Page::Graphics => {
                Section::Effects
            }
            Page::Mode | Page::Components | Page::Wallpaper | Page::LoginScreen | Page::LockScreen => {
                Section::Desktop
            }
            Page::BarAppearance | Page::BarWidgets | Page::BarFolding | Page::BarTray | Page::BarMotion => {
                Section::Taskbar
            }
            Page::LauncherStyle
            | Page::LauncherCentered
            | Page::LauncherMenu
            | Page::LauncherSearch
            | Page::LauncherKeybinds => Section::Launcher,
            Page::Display => Section::Display,
            Page::Network => Section::Network,
            Page::Keyboard | Page::Pointer | Page::Touchpad => Section::Input,
            Page::Session => Section::Session,
            Page::Rendering | Page::Xwayland | Page::Setup | Page::General => Section::System,
            Page::Privacy => Section::Privacy,
            Page::Accounts => Section::Accounts,
            Page::Addons => Section::Addons,
            Page::OeGeneral | Page::OeColours => Section::OracleEyes,
            Page::TermFont
            | Page::TermColors
            | Page::TermWindow
            | Page::TermScrollback
            | Page::TermInput
            | Page::TermShell
            | Page::TermBinds
            | Page::TermProfiles
            | Page::TermRules
            | Page::TermSecurity
            | Page::TermMedia => Section::Terminal,
        }
    }

    /// The page's name in the sidebar. A single-page section's page is
    /// named for the section.
    pub fn title(self) -> &'static str {
        match self {
            Page::Layout => "Layout",
            Page::Gaps => "Gaps",
            Page::Borders => "Borders",
            Page::Focus => "Focus & guides",
            Page::Blur => "Blur & glass",
            Page::Shadow => "Shadow & glow",
            Page::Rounding => "Rounding & opacity",
            Page::Animations => "Animations",
            Page::Graphics => "Graphics",
            Page::Mode => "Mode",
            Page::Components => "Components",
            Page::Wallpaper => "Wallpaper",
            Page::LoginScreen => "Login screen",
            Page::LockScreen => "Lock screen",
            Page::BarAppearance => "Appearance",
            Page::BarWidgets => "Widgets",
            Page::BarFolding => "Folding",
            Page::BarTray => "Tray",
            Page::BarMotion => "Motion",
            Page::LauncherStyle => "Style",
            Page::LauncherCentered => "Centered",
            Page::LauncherMenu => "In-bar menu",
            Page::LauncherSearch => "Search",
            Page::LauncherKeybinds => "Keybinds",
            Page::Keyboard => "Keyboard",
            Page::Pointer => "Pointer",
            Page::Touchpad => "Touchpad",
            Page::Rendering => "Rendering",
            Page::Xwayland => "XWayland",
            Page::Setup => "Setup",
            Page::General => "General",
            Page::OeGeneral => "Model & capture",
            Page::OeColours => "Colours",
            Page::TermFont => "Font & text",
            Page::TermColors => "Colors & cursor",
            Page::TermWindow => "Window & tabs",
            Page::TermScrollback => "Scrollback & selection",
            Page::TermInput => "Input & alerts",
            Page::TermShell => "Shell & automation",
            Page::TermBinds => "Key binds",
            Page::TermProfiles => "Profiles",
            Page::TermRules => "Rules",
            Page::TermSecurity => "Paste & security",
            Page::TermMedia => "Media & render",
            single => single.section().title(),
        }
    }

    /// The one line under the page title.
    pub fn subtitle(self) -> &'static str {
        match self {
            Page::Overview => "This computer at a glance, and the settings you reach for most.",
            Page::Layout => "How windows tile, and where a floating one opens.",
            Page::Gaps => "The air between windows and around the screen.",
            Page::Borders => "The line around each window, active and not.",
            Page::Focus => "Which window has focus, and the guides a drag shows.",
            Page::Blur => "What is drawn behind every translucent surface.",
            Page::Shadow => "Depth under windows, and the glow on the active one.",
            Page::Rounding => "Window corners, and how see-through windows are.",
            Page::Animations => "Whether windows move or snap into place.",
            Page::Graphics => "How much the desktop draws, matched to this computer.",
            Page::Mode => "How much desktop there is around your windows.",
            Page::Components => "The programs that draw the desktop's notifications and panels.",
            Page::Wallpaper => "The picture behind everything.",
            Page::LoginScreen => "The picture, greeting and clock on this machine's login screen.",
            Page::LockScreen => {
                "Where the clock, the account line and the password field sit on your lock screen."
            }
            Page::BarAppearance => "Where the bar sits and how it looks.",
            Page::BarWidgets => "Widgets, their order, and how the bar gives way.",
            Page::BarFolding => "When the bar folds away to a strip.",
            Page::BarTray => "Apps' status icons: pinned, in the drawer, or hidden.",
            Page::BarMotion => "How the bar's cells move.",
            Page::LauncherStyle => "Which launcher runs, and how it opens.",
            Page::LauncherCentered => "The launcher that opens in the middle of the screen.",
            Page::LauncherMenu => "The launcher that drops from the bar.",
            Page::LauncherSearch => "What the launcher finds as you type.",
            Page::LauncherKeybinds => "The keys that open the launcher. \"none\" leaves one unbound.",
            Page::Display => "Outputs, modes and overscan calibration.",
            Page::Network => "The wifi link, saved networks and paired devices.",
            Page::Keyboard => "Layout, variant and key repeat.",
            Page::Pointer => "How the pointer accelerates.",
            Page::Touchpad => "Tapping, scrolling and clicking on a touchpad.",
            Page::Session => "Idle, lock and power.",
            Page::Rendering => "The render device and direct scanout.",
            Page::Xwayland => "Running X11 apps.",
            Page::Setup => "The first-run profile.",
            Page::General => "Settings that apply across the desktop.",
            Page::Privacy => "Capture, clipboard, input methods and injected input.",
            Page::Accounts => {
                "The Claude accounts a task can sign in as. Names only; values stay in the store."
            }
            Page::TermFont => "Typeface, size, ligatures, cell metrics and Unicode widths.",
            Page::TermColors => "Theme, the sixteen ANSI colors and the cursor.",
            Page::TermWindow => "Opacity, padding, tabs, split panes and the quick terminal.",
            Page::TermScrollback => "History kept, copy on select and word boundaries.",
            Page::TermInput => "Mouse, keyboard protocols, bell and notifications.",
            Page::TermShell => {
                "The program that starts, sessions, and defaults for binds, rules and profiles."
            }
            Page::TermBinds => "Keys that run actions, in key tables and modes, and the defaults you unbind.",
            Page::TermProfiles => {
                "Settings that follow a process, host or folder, picked by how well it matches."
            }
            Page::TermRules => {
                "When something happens in a pane, run actions, if the match expression holds."
            }
            Page::TermSecurity => "Clipboard, paste guard, links, remote hosts and the control socket.",
            Page::TermMedia => "Inline images and video, frame pacing and accessibility.",
            Page::Addons => "Optional packages, and the hooks they switch on.",
            Page::OeGeneral => "What the add-on may see, the model it asks, and how long answers stay.",
            Page::OeColours => "The colours the compositor draws answers and the region selector in.",
        }
    }

    /// The picture on the page's header vignette: its section's, unless the
    /// page is about something more particular.
    pub fn glyph(self) -> Glyph {
        match self {
            Page::Layout => Glyph::Tiles,
            Page::Gaps => Glyph::Gaps,
            Page::Borders => Glyph::Frame,
            Page::Focus => Glyph::Focus,
            Page::Blur => Glyph::Panes,
            Page::Shadow => Glyph::Shadow,
            Page::Rounding => Glyph::Corner,
            Page::Graphics => Glyph::Chip,
            Page::Animations | Page::BarMotion => Glyph::Motion,
            Page::Mode | Page::General | Page::Setup => Glyph::Sliders,
            Page::Components => Glyph::Dots,
            Page::LoginScreen => Glyph::Person,
            Page::LockScreen => Glyph::Lock,
            Page::Pointer => Glyph::Mouse,
            Page::Touchpad => Glyph::Touchpad,
            Page::Rendering => Glyph::Chip,
            Page::Xwayland => Glyph::Tiles,
            Page::OeColours => Glyph::Panes,
            other => other.section().glyph(),
        }
    }

    /// The Terminal section's pages, in sidebar order.
    pub const TERMINAL: &'static [Page] = &[
        Page::TermFont,
        Page::TermColors,
        Page::TermWindow,
        Page::TermScrollback,
        Page::TermInput,
        Page::TermShell,
        Page::TermBinds,
        Page::TermProfiles,
        Page::TermRules,
        Page::TermSecurity,
        Page::TermMedia,
    ];

    pub fn terminal_pages() -> &'static [Page] {
        Page::TERMINAL
    }

    /// A page of the Terminal section: keys of `apoth-config`, not of abyss.
    pub fn is_terminal(self) -> bool {
        Page::TERMINAL.contains(&self)
    }

    /// Pages whose content is not a list of schema keys.
    pub fn is_bespoke(self) -> bool {
        matches!(
            self,
            Page::Overview
                | Page::Display
                | Page::Network
                | Page::Addons
                | Page::BarWidgets
                | Page::BarTray
                | Page::BarMotion
                | Page::BarAppearance
                | Page::BarFolding
                | Page::Animations
                | Page::Accounts
                | Page::LoginScreen
                | Page::LockScreen
        ) || self.is_terminal()
    }

    /// The page named on the command line: `section`, `section/page` or a
    /// page's own name, each any case with hyphens, spaces and `&` optional
    /// (`desktop/wallpaper`, `effects/blur-glass`, `addons`). A bare section
    /// opens its first page. Names from before the split still land:
    /// `appearance` opens Windows, and `taskbar/launcher` the launcher.
    pub fn from_arg(arg: &str) -> Option<Page> {
        let (head, tail) = match arg.split_once('/') {
            Some((h, t)) => (h, Some(t)),
            None => (arg, None),
        };
        if let Some(page) = legacy(&bare(arg)) {
            return Some(page);
        }
        let section = match bare(head).as_str() {
            "appearance" => Some(Section::Windows),
            _ => Section::from_arg(head),
        };
        match (section, tail) {
            (Some(s), None) => Some(s.first()),
            (Some(s), Some(t)) => s
                .pages()
                .iter()
                .copied()
                .find(|p| p.matches(t))
                .or_else(|| Some(s.first())),
            // Not a section: a page's own name, if only one page has it.
            (None, None) => {
                let mut hits = Page::all().filter(|p| p.matches(head));
                match (hits.next(), hits.next()) {
                    (Some(p), None) => Some(p),
                    _ => None,
                }
            }
            (None, Some(_)) => None,
        }
    }

    /// Whether `name` names this page: its title, or the leading word of it
    /// (`blur` for "Blur & glass", `focus` for "Focus & guides").
    fn matches(self, name: &str) -> bool {
        let want = bare(name);
        let title = bare(self.title());
        let lead = bare(self.title().split([' ', '&']).next().unwrap_or_default());
        !want.is_empty() && (want == title || want == lead)
    }

    /// The page whose [`Page::path`] is exactly `path`, without the
    /// leniency of [`Page::from_arg`]. Allocation-free: it runs per row.
    pub fn from_path(path: &str) -> Option<Page> {
        let (head, tail) = match path.split_once('/') {
            Some((h, t)) => (h, Some(t)),
            None => (path, None),
        };
        let section = Section::ALL.iter().copied().find(|s| slug_is(s.title(), head))?;
        match (section.expands(), tail) {
            (true, Some(t)) => section.pages().iter().copied().find(|p| slug_is(p.title(), t)),
            (false, None) => Some(section.first()),
            _ => None,
        }
    }

    /// `section/page` — the inverse of [`Page::from_arg`].
    pub fn path(self) -> String {
        let s = self.section();
        if s.expands() {
            format!("{}/{}", slug(s.title()), slug(self.title()))
        } else {
            slug(s.title())
        }
    }
}

/// Names a page had before it became one, by their `bare` spelling.
fn legacy(name: &str) -> Option<Page> {
    match name {
        "taskbar/launcher" => Some(Page::LauncherStyle),
        "system/terminal" => Some(Page::LauncherSearch),
        _ => None,
    }
}

/// Lower case with every character but letters, digits and `/` dropped, so
/// `Add-ons`, `add ons` and `addons` compare equal.
fn bare(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '/')
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// A title as a path segment: `Blur & glass` is `blur-glass`.
fn slug(s: &str) -> String {
    s.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join("-")
}

/// Whether `seg` is `slug(title)`, compared word by word without building it.
fn slug_is(title: &str, seg: &str) -> bool {
    let mut words = title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty());
    let mut segs = seg.split('-');
    loop {
        match (words.next(), segs.next()) {
            (None, None) => return true,
            (Some(w), Some(s))
                if w.len() == s.len() && w.bytes().map(|b| b.to_ascii_lowercase()).eq(s.bytes()) => {}
            _ => return false,
        }
    }
}

/// Where a key appears: the section, the page within it, and the group
/// heading on that page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Place {
    pub section: Section,
    pub page: Page,
    pub group: &'static str,
}

/// Where a dotted config path appears, or `None` if nothing claims it.
///
/// Read from the compositor's presentation table (`ec_abyss_config::meta`):
/// each schema key names its page and group there, beside its label. A key
/// that table does not know (one a newer compositor grew) lands where its
/// top-level node's fallback puts it, or nowhere.
pub fn place_for(path: &str) -> Option<Place> {
    let (page, group) = ec_abyss_config::meta::place(path)?;
    let page = Page::from_path(page)?;
    Some(Place {
        section: page.section(),
        page,
        group,
    })
}

/// The page a key is on, or `None` if nothing claims it.
pub fn page_of(path: &str) -> Option<Page> {
    place_for(path).map(|p| p.page)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_section_is_named_with_or_without_its_hyphen() {
        assert_eq!(Page::from_arg("addons"), Some(Page::Addons));
        assert_eq!(Page::from_arg("Add-ons"), Some(Page::Addons));
        assert_eq!(Page::from_arg("NETWORK"), Some(Page::Network));
        // The agent console opens `ec-settings accounts`.
        assert_eq!(Page::from_arg("accounts"), Some(Page::Accounts));
        assert_eq!(Page::from_arg("login-screen"), Some(Page::LoginScreen));
        assert_eq!(Page::from_arg("desktop/login-screen"), Some(Page::LoginScreen));
        assert_eq!(Page::from_arg("nope"), None);
    }

    #[test]
    fn a_section_opens_its_first_page_and_a_path_opens_that_page() {
        assert_eq!(Page::from_arg("desktop"), Some(Page::Mode));
        assert_eq!(Page::from_arg("desktop/wallpaper"), Some(Page::Wallpaper));
        assert_eq!(Page::from_arg("Effects/Blur-Glass"), Some(Page::Blur));
        assert_eq!(Page::from_arg("effects/blur"), Some(Page::Blur));
        assert_eq!(Page::from_arg("windows/focus"), Some(Page::Focus));
        assert_eq!(Page::from_arg("launcher/in-bar-menu"), Some(Page::LauncherMenu));
        assert_eq!(Page::from_arg("input/touchpad"), Some(Page::Touchpad));
        // An unknown page in a known section still opens the section.
        assert_eq!(Page::from_arg("desktop/nope"), Some(Page::Mode));
        assert_eq!(Page::from_arg("nope/wallpaper"), None);
    }

    #[test]
    fn a_unique_page_name_works_alone() {
        assert_eq!(Page::from_arg("wallpaper"), Some(Page::Wallpaper));
        assert_eq!(Page::from_arg("touchpad"), Some(Page::Touchpad));
        // "Appearance" is a Taskbar page, but the old pane name wins.
        assert_eq!(Page::from_arg("appearance"), Some(Page::Layout));
    }

    #[test]
    fn old_pane_names_still_open_what_they_became() {
        assert_eq!(Page::from_arg("appearance"), Some(Page::Layout));
        assert_eq!(Page::from_arg("Appearance"), Some(Page::Layout));
        assert_eq!(Page::from_arg("effects"), Some(Page::Blur));
        assert_eq!(Page::from_arg("taskbar"), Some(Page::BarAppearance));
        assert_eq!(Page::from_arg("system"), Some(Page::Rendering));
    }

    #[test]
    fn a_path_round_trips() {
        for page in Page::all() {
            assert_eq!(Page::from_arg(&page.path()), Some(page), "{}", page.path());
        }
        assert_eq!(Page::Wallpaper.path(), "desktop/wallpaper");
        assert_eq!(Page::Blur.path(), "effects/blur-glass");
        assert_eq!(Page::Addons.path(), "add-ons");
    }

    #[test]
    fn a_page_is_found_by_its_exact_path() {
        for page in Page::all() {
            assert_eq!(Page::from_path(&page.path()), Some(page), "{}", page.path());
        }
        assert_eq!(Page::from_path("windows"), None);
        assert_eq!(Page::from_path("display/display"), None);
        assert_eq!(Page::from_path("effects/blur"), None);
    }

    /// Every row the compositor's presentation table describes is a real
    /// schema key and lands on a real page with the group it names.
    #[test]
    fn every_meta_row_is_a_schema_key_on_a_real_page() {
        for m in ec_abyss_config::meta::META {
            assert!(
                ec_abyss_config::schema::get_key(m.path).is_some(),
                "{} is placed but not in the schema",
                m.path
            );
            let page = Page::from_path(m.page).unwrap_or_else(|| panic!("{}: no page {:?}", m.path, m.page));
            assert_eq!(
                place_for(m.path),
                Some(Place {
                    section: page.section(),
                    page,
                    group: m.group
                })
            );
        }
        for (node, page, _) in ec_abyss_config::meta::NODE_FALLBACK {
            assert!(
                Page::from_path(page).is_some(),
                "{node} falls back to no page {page:?}"
            );
        }
    }

    #[test]
    fn every_page_is_in_exactly_one_section() {
        for page in Page::all() {
            let owners = Section::ALL.iter().filter(|s| s.pages().contains(&page)).count();
            assert_eq!(owners, 1, "{page:?}");
            assert!(page.section().pages().contains(&page));
        }
        assert_eq!(Page::all().count(), 51);
    }

    #[test]
    fn the_sitemap_has_no_duplicate_ids() {
        let mut paths: Vec<String> = Page::all().map(Page::path).collect();
        let n = paths.len();
        paths.sort();
        paths.dedup();
        assert_eq!(paths.len(), n, "two pages share a path");
        // Titles are unique inside a section, or a sidebar row is ambiguous.
        for s in Section::ALL {
            let mut titles: Vec<&str> = s.pages().iter().map(|p| p.title()).collect();
            let n = titles.len();
            titles.sort_unstable();
            titles.dedup();
            assert_eq!(titles.len(), n, "{:?} repeats a page title", s);
        }
        // So are the sections' own.
        let mut names: Vec<&str> = Section::ALL.iter().map(|s| s.title()).collect();
        names.sort_unstable();
        let n = names.len();
        names.dedup();
        assert_eq!(names.len(), n);
    }

    /// Every section sits under exactly one heading (the overview, above
    /// them all, under none), every heading has sections, and every section
    /// and page has a picture and a hue.
    #[test]
    fn every_section_has_a_group_an_icon_and_a_hue() {
        for s in Section::ALL {
            match (s.group(), *s == Section::Overview) {
                (None, true) | (Some(_), false) => {}
                other => panic!("{s:?}: {other:?}"),
            }
            assert!(!s.blurb().is_empty());
            let _ = (s.glyph(), s.hue());
        }
        let mut seen = 0;
        for g in Group::ALL {
            let n = g.sections().count();
            assert!(n > 0, "{g:?} is empty");
            seen += n;
        }
        assert_eq!(seen, Section::ALL.len() - 1);
        let mut hues: Vec<_> = Group::ALL.iter().map(|g| g.hue()).collect();
        hues.dedup();
        assert_eq!(hues.len(), Group::ALL.len(), "two groups share a hue");
        for p in Page::all() {
            let _ = p.glyph();
        }
        // Sections draw their own glyph, so two sections never look alike.
        let mut glyphs: Vec<_> = Section::ALL.iter().map(|s| s.glyph()).collect();
        let n = glyphs.len();
        glyphs.sort_by_key(|g| *g as u8);
        glyphs.dedup();
        assert_eq!(glyphs.len(), n, "two sections share an icon");
    }

    #[test]
    fn a_groups_sections_are_contiguous_in_sidebar_order() {
        // The sidebar draws a heading before each group's first section, so a
        // group split across the list would heading twice.
        let mut last = None;
        let mut closed: Vec<Group> = Vec::new();
        for s in Section::ALL {
            let g = s.group();
            if g != last {
                if let Some(done) = last {
                    closed.push(done);
                }
                if let Some(g) = g {
                    assert!(!closed.contains(&g), "{g:?} is split");
                }
                last = g;
            }
        }
    }

    #[test]
    fn the_overview_is_the_default_and_nothing_lands_on_it() {
        assert_eq!(Page::from_arg("overview"), Some(Page::Overview));
        assert_eq!(Page::Overview.path(), "overview");
        assert_eq!(Section::Overview.first(), Page::Overview);
        for m in ec_abyss_config::meta::META {
            assert_ne!(Page::from_path(m.page), Some(Page::Overview), "{}", m.path);
        }
    }

    #[test]
    fn windows_splits_by_topic() {
        assert_eq!(page_of("general.layout"), Some(Page::Layout));
        assert_eq!(page_of("general.floating-placement"), Some(Page::Layout));
        assert_eq!(page_of("general.gaps-in"), Some(Page::Gaps));
        assert_eq!(page_of("general.gaps-out-vertical"), Some(Page::Gaps));
        assert_eq!(page_of("general.border-size"), Some(Page::Borders));
        assert_eq!(page_of("general.col-active-border"), Some(Page::Borders));
        assert_eq!(page_of("general.focus-follows-mouse"), Some(Page::Focus));
        assert_eq!(page_of("general.refocus-on-scene-change"), Some(Page::Focus));
        assert_eq!(page_of("general.drop-guides"), Some(Page::Focus));
        assert_eq!(page_of("general.maximize-first-window"), Some(Page::Focus));
        assert_eq!(page_of("general.maximize-first-window-exempt"), Some(Page::Focus));
        assert_eq!(page_of("general.color-scheme"), Some(Page::Blur));
        assert_eq!(page_of("general.light-from"), Some(Page::Blur));
        assert_eq!(page_of("general.theme-dark"), Some(Page::Blur));
        assert_eq!(page_of("general.accent"), Some(Page::Blur));
        assert_eq!(place_for("general.drop-edge-band").unwrap().group, "drag guides");
    }

    #[test]
    fn effects_splits_by_topic() {
        assert_eq!(page_of("decoration.blur.mode"), Some(Page::Blur));
        assert_eq!(page_of("decoration.blur.glass.rim"), Some(Page::Blur));
        assert_eq!(page_of("decoration.profile"), Some(Page::Graphics));
        assert_eq!(page_of("decoration.profile-governor"), Some(Page::Graphics));
        assert_eq!(Page::from_arg("effects/graphics"), Some(Page::Graphics));
        assert_eq!(Page::Graphics.path(), "effects/graphics");
        assert_eq!(page_of("decoration.shadow.range"), Some(Page::Shadow));
        assert_eq!(page_of("decoration.glow.strength"), Some(Page::Shadow));
        assert_eq!(page_of("decoration.rounding"), Some(Page::Rounding));
        assert_eq!(page_of("decoration.dim-inactive"), Some(Page::Rounding));
        assert_eq!(page_of("animations.enabled"), Some(Page::Animations));
    }

    #[test]
    fn the_launcher_is_one_topic() {
        assert_eq!(page_of("components.launcher"), Some(Page::LauncherStyle));
        assert_eq!(page_of("launcher.style"), Some(Page::LauncherStyle));
        assert_eq!(page_of("launcher.centered.width"), Some(Page::LauncherCentered));
        assert_eq!(page_of("launcher.centered.anchor"), Some(Page::LauncherCentered));
        assert_eq!(page_of("launcher.menu.max-rows"), Some(Page::LauncherMenu));
        assert_eq!(
            page_of("launcher.search.terminal-apps"),
            Some(Page::LauncherSearch)
        );
        // The terminal gates terminal apps: the two sit together.
        assert_eq!(page_of("misc.terminal-command"), Some(Page::LauncherSearch));
        assert_eq!(page_of("launcher.bind.open"), Some(Page::LauncherKeybinds));
        assert_eq!(page_of("launcher.bind.run"), Some(Page::LauncherKeybinds));
        assert_eq!(page_of("launcher.something-new"), Some(Page::LauncherStyle));
    }

    #[test]
    fn the_taskbar_splits_by_topic() {
        assert_eq!(page_of("components.bar"), Some(Page::BarAppearance));
        assert_eq!(page_of("bar.position"), Some(Page::BarAppearance));
        assert_eq!(page_of("bar.eye"), Some(Page::BarAppearance));
        assert_eq!(page_of("bar.fold-height"), Some(Page::BarFolding));
        assert_eq!(page_of("bar.idle-seconds"), Some(Page::BarFolding));
        assert_eq!(page_of("bar.tray.pinned"), Some(Page::BarTray));
        assert_eq!(page_of("bar.motion.curve"), Some(Page::BarMotion));
        assert_eq!(page_of("bar.widgets.order"), Some(Page::BarWidgets));
        assert_eq!(page_of("bar.clock.hour-12"), Some(Page::BarWidgets));
    }

    #[test]
    fn misc_splits_by_key_not_by_node() {
        assert_eq!(page_of("misc.render-device"), Some(Page::Rendering));
        assert_eq!(page_of("misc.gpu-mode"), Some(Page::Rendering));
        assert_eq!(page_of("misc.discrete-gpu-apps"), Some(Page::Rendering));
        assert_eq!(page_of("misc.scripted-input"), Some(Page::Privacy));
        assert_eq!(
            place_for("input-method.allow").map(|p| p.group),
            Some("input method")
        );
        assert_eq!(
            place_for("virtual-pointer.allow").map(|p| p.group),
            Some("virtual pointer")
        );
        assert_eq!(page_of("oracle-eyes.model-command"), Some(Page::OeGeneral));
        assert_eq!(page_of("oracle-eyes.debug"), Some(Page::OeGeneral));
        assert_eq!(
            place_for("oracle-eyes.bind.select").map(|p| p.group),
            Some("keybinds")
        );
        assert_eq!(page_of("annotations.accent"), Some(Page::OeColours));
        assert_eq!(Page::from_arg("oracle-eyes/colours"), Some(Page::OeColours));
        assert_eq!(Page::from_arg("oracle-eyes"), Some(Page::OeGeneral));
        assert_eq!(page_of("misc.something-new"), None);
    }

    #[test]
    fn input_and_system_split_by_topic() {
        assert_eq!(page_of("input.kb-layout"), Some(Page::Keyboard));
        assert_eq!(page_of("input.repeat-rate"), Some(Page::Keyboard));
        assert_eq!(page_of("input.accel-profile"), Some(Page::Pointer));
        assert_eq!(page_of("input.touchpad.dwt"), Some(Page::Touchpad));
        assert_eq!(page_of("render.direct-scanout"), Some(Page::Rendering));
        assert_eq!(page_of("xwayland.scaling"), Some(Page::Xwayland));
        assert_eq!(page_of("setup.profile"), Some(Page::Setup));
        assert_eq!(page_of("ui.show-key-hints"), Some(Page::General));
    }

    #[test]
    fn desktop_keeps_its_mode_and_wallpaper() {
        assert_eq!(page_of("mode"), Some(Page::Mode));
        assert_eq!(place_for("mode").unwrap().group, "interaction");
        assert_eq!(page_of("components.notifications"), Some(Page::Components));
        assert_eq!(page_of("wallpaper.path"), Some(Page::Wallpaper));
    }
}
