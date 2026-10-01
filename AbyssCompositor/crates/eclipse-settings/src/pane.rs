// SPDX-License-Identifier: AGPL-3.0-only
//! Which pane a config key appears in.
//!
//! The assignment is by path, not by a per-pane list of keys — a new key in a
//! node a pane already claims needs no change here. A new *node* does, and
//! `tests/coverage.rs` is what says so: an unassigned key fails the build
//! rather than quietly becoming unreachable from the GUI (F-01 §4).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Windows,
    Effects,
    Desktop,
    Taskbar,
    Display,
    Network,
    Input,
    Session,
    System,
    Privacy,
    Addons,
}

impl Pane {
    /// Sidebar order.
    pub const ALL: &'static [Pane] = &[
        Pane::Windows,
        Pane::Effects,
        Pane::Desktop,
        Pane::Taskbar,
        Pane::Display,
        Pane::Network,
        Pane::Input,
        Pane::Session,
        Pane::System,
        Pane::Privacy,
        Pane::Addons,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Pane::Windows => "Windows",
            Pane::Effects => "Effects",
            Pane::Desktop => "Desktop",
            Pane::Taskbar => "Taskbar",
            Pane::Display => "Display",
            Pane::Network => "Network",
            Pane::Input => "Input",
            Pane::Session => "Session",
            Pane::System => "System",
            Pane::Privacy => "Privacy",
            Pane::Addons => "Add-ons",
        }
    }

    pub fn subtitle(self) -> &'static str {
        match self {
            Pane::Windows => "Layout, gaps, borders and focus.",
            Pane::Effects => "Blur, glass, shadow, glow and motion.",
            Pane::Desktop => "How much desktop there is, and which apps run it.",
            Pane::Taskbar => "Widgets, their order, and how the bar gives way.",
            Pane::Display => "Outputs, modes and overscan calibration.",
            Pane::Network => "The wifi link, saved networks and paired devices.",
            Pane::Input => "Keyboard, pointer and touchpad.",
            Pane::Session => "Idle, lock and power.",
            Pane::System => "Xwayland, the render device and scanout.",
            Pane::Privacy => "Capture, clipboard and input scripting.",
            Pane::Addons => "Optional packages, and the hooks they switch on.",
        }
    }

    /// Panes whose content is not a list of schema keys.
    pub fn is_bespoke(self) -> bool {
        matches!(self, Pane::Display | Pane::Network | Pane::Addons)
    }

    /// The pane named on the command line — its title, any case, hyphens
    /// optional (`addons` finds "Add-ons"). The taskbar opens
    /// `eclipse-settings network` from its drawers. `appearance` names the
    /// pane that was split into Windows and Effects, and opens the first of
    /// them, so a launcher entry or a habit written against it still lands.
    pub fn from_arg(arg: &str) -> Option<Pane> {
        let bare = |s: &str| s.replace('-', "").to_ascii_lowercase();
        if bare(arg) == "appearance" {
            return Some(Pane::Windows);
        }
        Pane::ALL.iter().copied().find(|p| bare(p.title()) == bare(arg))
    }
}

/// `bar.launcher-style`: the centred launcher or the bar's start menu. It is
/// a `bar` key but reads as part of `components.launcher`, so the Desktop
/// pane shows it in the components group, directly under that row.
pub const LAUNCHER_STYLE: &str = "bar.launcher-style";

/// The pane a dotted config path belongs to, or `None` if nothing claims it.
///
/// Matching is on the leading node except where one node splits across panes:
/// `misc` holds both a hardware setting and a policy-owned one, and they do not
/// belong together.
pub fn pane_for(path: &str) -> Option<Pane> {
    match path {
        "misc.render-device" => return Some(Pane::System),
        "misc.terminal-command" => return Some(Pane::System),
        "misc.scripted-input" => return Some(Pane::Privacy),
        // How Super+R opens the launcher: a sub-choice of the Launcher slot,
        // so it sits beside it rather than with the bar's looks.
        LAUNCHER_STYLE => return Some(Pane::Desktop),
        _ => {}
    }
    let node = path.split('.').next().unwrap_or(path);
    match node {
        "mode" | "components" | "wallpaper" => Some(Pane::Desktop),
        "general" => Some(Pane::Windows),
        "decoration" | "animations" => Some(Pane::Effects),
        "render" => Some(Pane::System),
        "bar" => Some(Pane::Taskbar),
        "input" => Some(Pane::Input),
        "idle" => Some(Pane::Session),
        "xwayland" => Some(Pane::System),
        "setup" => Some(Pane::System),
        "clipboard" | "capture" => Some(Pane::Privacy),
        _ => None,
    }
}

/// Heading a key is grouped under inside its pane: the node prefix, so
/// `decoration.blur.size` sits under "blur" and `general.gaps-in` under
/// "general". The drag-drop keys share `general` with the layout they serve
/// but are one feature, so they get their own heading beneath it. Blur's
/// per-mode sub-nodes (`decoration.blur.glass.*`, `.frost.*`) stay under
/// "blur" beside the mode they tune, rather than as panels of their own —
/// captioned and dimmed there by mode (`schema::moded`).
pub fn group_for(path: &str) -> &str {
    if path.starts_with("decoration.blur.") {
        return "blur";
    }
    if path == "mode" {
        return "interaction";
    }
    if path == LAUNCHER_STYLE {
        return "components";
    }
    if path.starts_with("general.drop-") {
        return "drag guides";
    }
    let mut parts = path.rsplitn(3, '.');
    let _leaf = parts.next();
    parts.next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pane_is_named_with_or_without_its_hyphen() {
        assert_eq!(Pane::from_arg("addons"), Some(Pane::Addons));
        assert_eq!(Pane::from_arg("Add-ons"), Some(Pane::Addons));
        assert_eq!(Pane::from_arg("NETWORK"), Some(Pane::Network));
        assert_eq!(Pane::from_arg("nope"), None);
    }

    #[test]
    fn appearance_still_opens_what_it_became() {
        assert_eq!(Pane::from_arg("appearance"), Some(Pane::Windows));
        assert_eq!(Pane::from_arg("Appearance"), Some(Pane::Windows));
        assert_eq!(Pane::from_arg("effects"), Some(Pane::Effects));
    }

    #[test]
    fn appearance_splits_into_windows_effects_and_system() {
        assert_eq!(pane_for("general.gaps-in"), Some(Pane::Windows));
        assert_eq!(pane_for("general.drop-guides"), Some(Pane::Windows));
        assert_eq!(pane_for("decoration.blur.mode"), Some(Pane::Effects));
        assert_eq!(pane_for("animations.enabled"), Some(Pane::Effects));
        assert_eq!(pane_for("render.direct-scanout"), Some(Pane::System));
        assert_eq!(&Pane::ALL[..2], &[Pane::Windows, Pane::Effects]);
    }

    #[test]
    fn misc_splits_by_key_not_by_node() {
        assert_eq!(pane_for("misc.render-device"), Some(Pane::System));
        assert_eq!(pane_for("misc.scripted-input"), Some(Pane::Privacy));
        assert_eq!(pane_for("misc.something-new"), None);
    }

    #[test]
    fn launcher_style_sits_with_the_launcher_slot() {
        assert_eq!(pane_for(LAUNCHER_STYLE), Some(Pane::Desktop));
        assert_eq!(group_for(LAUNCHER_STYLE), "components");
        assert_eq!(pane_for("bar.position"), Some(Pane::Taskbar));
    }

    #[test]
    fn groups_come_from_the_node_prefix() {
        assert_eq!(group_for("decoration.blur.size"), "blur");
        assert_eq!(group_for("decoration.blur.glass.refraction"), "blur");
        assert_eq!(group_for("decoration.blur.frost.tint"), "blur");
        assert_eq!(group_for("general.gaps-in"), "general");
        assert_eq!(group_for("general.drop-edge-band"), "drag guides");
        assert_eq!(group_for("mode"), "interaction");
        assert_eq!(group_for("components.bar"), "components");
    }
}
