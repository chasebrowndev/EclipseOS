// SPDX-License-Identifier: AGPL-3.0-only
//! Which pane a config key appears in.
//!
//! The assignment is by path, not by a per-pane list of keys — a new key in a
//! node a pane already claims needs no change here. A new *node* does, and
//! `tests/coverage.rs` is what says so: an unassigned key fails the build
//! rather than quietly becoming unreachable from the GUI (F-01 §4).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Appearance,
    Display,
    Input,
    Session,
    System,
    Privacy,
}

impl Pane {
    /// Sidebar order.
    pub const ALL: &'static [Pane] = &[
        Pane::Appearance,
        Pane::Display,
        Pane::Input,
        Pane::Session,
        Pane::System,
        Pane::Privacy,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Pane::Appearance => "Appearance",
            Pane::Display => "Display",
            Pane::Input => "Input",
            Pane::Session => "Session",
            Pane::System => "System",
            Pane::Privacy => "Privacy",
        }
    }

    pub fn subtitle(self) -> &'static str {
        match self {
            Pane::Appearance => "Layout, borders, decoration and animation.",
            Pane::Display => "Outputs, modes and overscan calibration.",
            Pane::Input => "Keyboard, pointer and touchpad.",
            Pane::Session => "Idle, lock and power.",
            Pane::System => "Xwayland and the render device.",
            Pane::Privacy => "Capture, clipboard and input scripting.",
        }
    }

    /// Panes whose content is not a list of schema keys.
    pub fn is_bespoke(self) -> bool {
        matches!(self, Pane::Display)
    }
}

/// The pane a dotted config path belongs to, or `None` if nothing claims it.
///
/// Matching is on the leading node except where one node splits across panes:
/// `misc` holds both a hardware setting and a policy-owned one, and they do not
/// belong together.
pub fn pane_for(path: &str) -> Option<Pane> {
    match path {
        "misc.render-device" => return Some(Pane::System),
        "misc.scripted-input" => return Some(Pane::Privacy),
        _ => {}
    }
    let node = path.split('.').next().unwrap_or(path);
    match node {
        "general" | "decoration" | "animations" | "render" => Some(Pane::Appearance),
        "input" => Some(Pane::Input),
        "idle" => Some(Pane::Session),
        "xwayland" => Some(Pane::System),
        "clipboard" | "capture" => Some(Pane::Privacy),
        _ => None,
    }
}

/// Heading a key is grouped under inside its pane: the node prefix, so
/// `decoration.blur.size` sits under "blur" and `general.gaps-in` under
/// "general".
pub fn group_for(path: &str) -> &str {
    let mut parts = path.rsplitn(3, '.');
    let _leaf = parts.next();
    parts.next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn misc_splits_by_key_not_by_node() {
        assert_eq!(pane_for("misc.render-device"), Some(Pane::System));
        assert_eq!(pane_for("misc.scripted-input"), Some(Pane::Privacy));
        assert_eq!(pane_for("misc.something-new"), None);
    }

    #[test]
    fn groups_come_from_the_node_prefix() {
        assert_eq!(group_for("decoration.blur.size"), "blur");
        assert_eq!(group_for("general.gaps-in"), "general");
    }
}
