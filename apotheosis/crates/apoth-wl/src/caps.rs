// SPDX-License-Identifier: AGPL-3.0-only
//! The protocol table of T-09 §3 as data: which globals are fatal, which are optional, what
//! each absence falls back to, and what is bound. Exposed by the `wayland-info` Action and
//! IPC (AUTOMATION-38). Also the blur ladder of T-09 §5.

use std::fmt::Write as _;

/// How much the client needs a global.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Requirement {
    /// Absence is a fatal connection error.
    Fatal,
    /// Always bound; no specified absence behaviour (reported, not fatal).
    Core,
    /// Bound opportunistically; absence falls back.
    Optional,
    /// Detected only. Never bound on our surfaces (fifo, commit-timing).
    InformationalOnly,
    /// Detected only; a later release uses it.
    Later(&'static str),
    /// Never used.
    NotUsed,
}

/// One row of the protocol table.
#[derive(Clone, Copy, Debug)]
pub struct CapSpec {
    /// Interface (global) name.
    pub interface: &'static str,
    /// Requirement class.
    pub requirement: Requirement,
    /// What happens when absent.
    pub absent: &'static str,
}

macro_rules! row {
    ($i:expr, $r:expr, $a:expr) => {
        CapSpec {
            interface: $i,
            requirement: $r,
            absent: $a,
        }
    };
}

/// T-09 §3.
pub const TABLE: &[CapSpec] = &[
    row!("wl_compositor", Requirement::Fatal, "fatal"),
    row!("wl_subcompositor", Requirement::Fatal, "fatal"),
    row!("wl_shm", Requirement::Fatal, "fatal"),
    row!("wl_seat", Requirement::Fatal, "fatal"),
    row!("xdg_wm_base", Requirement::Fatal, "fatal (v6+ required)"),
    row!(
        "wl_output",
        Requirement::Core,
        "integer scale 1, no output info"
    ),
    row!("wl_data_device_manager", Requirement::Core, "no clipboard"),
    row!(
        "zxdg_decoration_manager_v1",
        Requirement::Optional,
        "client-side decorations"
    ),
    row!(
        "wp_fractional_scale_manager_v1",
        Requirement::Optional,
        "integer wl_output scale"
    ),
    row!(
        "wp_viewporter",
        Requirement::Optional,
        "integer wl_output scale"
    ),
    row!("zwp_text_input_manager_v3", Requirement::Optional, "no IME"),
    row!(
        "zwp_primary_selection_device_manager_v1",
        Requirement::Optional,
        "primary selection disabled"
    ),
    row!(
        "wp_cursor_shape_manager_v1",
        Requirement::Optional,
        "themed cursor via shm"
    ),
    row!("xdg_activation_v1", Requirement::Optional, "no urgency"),
    row!(
        "zwp_input_timestamps_manager_v1",
        Requirement::Optional,
        "client-side stamps only"
    ),
    row!(
        "wp_presentation",
        Requirement::Optional,
        "frame callbacks only"
    ),
    row!(
        "wp_fifo_manager_v1",
        Requirement::InformationalOnly,
        "frame callbacks"
    ),
    row!(
        "wp_commit_timing_manager_v1",
        Requirement::InformationalOnly,
        "frame callbacks"
    ),
    row!(
        "ext_background_effect_manager_v1",
        Requirement::Optional,
        "kwin blur, then tint"
    ),
    row!("org_kde_kwin_blur_manager", Requirement::Optional, "tint"),
    row!(
        "xdg_system_bell_v1",
        Requirement::Optional,
        "no audible bell"
    ),
    row!(
        "xdg_toplevel_icon_manager_v1",
        Requirement::Optional,
        "app-id icon"
    ),
    row!(
        "xdg_toplevel_tag_manager_v1",
        Requirement::Optional,
        "no tag"
    ),
    row!("wp_content_type_manager_v1", Requirement::Optional, "none"),
    row!("zwp_idle_inhibit_manager_v1", Requirement::Optional, "none"),
    row!(
        "zwlr_layer_shell_v1",
        Requirement::Later("R2"),
        "ordinary toplevel"
    ),
    row!(
        "zwp_keyboard_shortcuts_inhibit_manager_v1",
        Requirement::Later("R2"),
        "none"
    ),
    row!(
        "xdg_toplevel_drag_manager_v1",
        Requirement::Later("R3"),
        "none"
    ),
    row!("wp_tearing_control_manager_v1", Requirement::NotUsed, "-"),
    row!("xdg_wm_dialog_v1", Requirement::NotUsed, "-"),
];

/// Look up a row.
pub fn spec(interface: &str) -> Option<&'static CapSpec> {
    TABLE.iter().find(|s| s.interface == interface)
}

/// What the registry and our binds say about one global.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindState {
    /// Not advertised.
    Absent,
    /// Advertised, not bound by us.
    Present {
        /// Advertised version.
        version: u32,
    },
    /// Advertised and bound.
    Bound {
        /// Bound version.
        version: u32,
    },
}

/// The runtime capability table.
#[derive(Clone, Debug)]
pub struct CapabilityTable {
    states: Vec<BindState>,
}

impl Default for CapabilityTable {
    fn default() -> Self {
        CapabilityTable::new()
    }
}

impl CapabilityTable {
    /// All absent.
    pub fn new() -> CapabilityTable {
        CapabilityTable {
            states: vec![BindState::Absent; TABLE.len()],
        }
    }

    fn index(interface: &str) -> Option<usize> {
        TABLE.iter().position(|s| s.interface == interface)
    }

    /// The registry advertised `interface` (does not downgrade a bound entry).
    pub fn set_present(&mut self, interface: &str, version: u32) {
        if let Some(i) = Self::index(interface)
            && !matches!(self.states[i], BindState::Bound { .. })
        {
            self.states[i] = BindState::Present { version };
        }
    }

    /// We bound `interface`.
    pub fn set_bound(&mut self, interface: &str, version: u32) {
        if let Some(i) = Self::index(interface) {
            self.states[i] = BindState::Bound { version };
        }
    }

    /// The registry removed `interface`.
    pub fn set_removed(&mut self, interface: &str) {
        if let Some(i) = Self::index(interface) {
            self.states[i] = BindState::Absent;
        }
    }

    /// State of one global (unknown interfaces read as absent).
    pub fn state(&self, interface: &str) -> BindState {
        Self::index(interface).map_or(BindState::Absent, |i| self.states[i])
    }

    /// Advertised by the compositor (bound or not).
    pub fn is_present(&self, interface: &str) -> bool {
        !matches!(self.state(interface), BindState::Absent)
    }

    /// Bound by us.
    pub fn is_bound(&self, interface: &str) -> bool {
        matches!(self.state(interface), BindState::Bound { .. })
    }

    /// The first fatal-class global that is not bound, if any.
    pub fn missing_fatal(&self) -> Option<&'static str> {
        TABLE
            .iter()
            .zip(&self.states)
            .find(|(s, st)| {
                s.requirement == Requirement::Fatal && !matches!(st, BindState::Bound { .. })
            })
            .map(|(s, _)| s.interface)
    }

    /// Rows for display and IPC: `(spec, state)`.
    pub fn rows(&self) -> impl Iterator<Item = (&'static CapSpec, BindState)> + '_ {
        TABLE.iter().zip(self.states.iter().copied())
    }

    /// Plain-text table for the `wayland-info` Action.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (spec, st) in self.rows() {
            let state = match st {
                BindState::Absent => "absent".to_string(),
                BindState::Present { version } => format!("present v{version}"),
                BindState::Bound { version } => format!("bound v{version}"),
            };
            let _ = writeln!(
                out,
                "{:<44} {:<12} absent: {}",
                spec.interface, state, spec.absent
            );
        }
        out
    }
}

/// How blur is achieved for a window (T-09 §5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlurPath {
    /// `ext_background_effect_v1`.
    Ext,
    /// `org_kde_kwin_blur`.
    Kwin,
    /// No compositor blur: the renderer lays a tint or opaque fill.
    Tint,
}

/// Evaluate the ladder. On Abyss only the ext effect is used (T-08 §3). Elsewhere:
/// ext, then kwin blur, then tint. `ext_blur` means the manager is bound and its
/// capabilities carry the blur bit; capabilities can change at runtime so this is
/// re-evaluated on every change.
pub fn choose_blur(on_abyss: bool, ext_blur: bool, kwin_bound: bool) -> BlurPath {
    if ext_blur {
        BlurPath::Ext
    } else if on_abyss {
        BlurPath::Tint
    } else if kwin_bound {
        BlurPath::Kwin
    } else {
        BlurPath::Tint
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fatal_set_matches_spec() {
        let fatals: Vec<_> = TABLE
            .iter()
            .filter(|s| s.requirement == Requirement::Fatal)
            .map(|s| s.interface)
            .collect();
        assert_eq!(
            fatals,
            [
                "wl_compositor",
                "wl_subcompositor",
                "wl_shm",
                "wl_seat",
                "xdg_wm_base"
            ]
        );
    }

    #[test]
    fn table_has_no_duplicates() {
        for (i, a) in TABLE.iter().enumerate() {
            assert!(
                TABLE[i + 1..].iter().all(|b| b.interface != a.interface),
                "{}",
                a.interface
            );
        }
    }

    #[test]
    fn informational_globals_are_never_bound_by_table_class() {
        for i in ["wp_fifo_manager_v1", "wp_commit_timing_manager_v1"] {
            assert_eq!(spec(i).unwrap().requirement, Requirement::InformationalOnly);
        }
        assert_eq!(
            spec("wp_tearing_control_manager_v1").unwrap().requirement,
            Requirement::NotUsed
        );
    }

    #[test]
    fn missing_fatal_reports_first_unbound() {
        let mut t = CapabilityTable::new();
        assert_eq!(t.missing_fatal(), Some("wl_compositor"));
        for i in ["wl_compositor", "wl_subcompositor", "wl_shm", "wl_seat"] {
            t.set_bound(i, 1);
        }
        assert_eq!(t.missing_fatal(), Some("xdg_wm_base"));
        t.set_bound("xdg_wm_base", 6);
        assert_eq!(t.missing_fatal(), None);
    }

    #[test]
    fn present_does_not_downgrade_bound_and_removal_clears() {
        let mut t = CapabilityTable::new();
        t.set_bound("wp_viewporter", 1);
        t.set_present("wp_viewporter", 1);
        assert!(t.is_bound("wp_viewporter"));
        t.set_removed("wp_viewporter");
        assert!(!t.is_present("wp_viewporter"));
        t.set_present("wp_presentation", 2);
        assert_eq!(
            t.state("wp_presentation"),
            BindState::Present { version: 2 }
        );
        assert_eq!(t.state("not_a_global"), BindState::Absent);
    }

    #[test]
    fn render_lists_every_row() {
        let t = CapabilityTable::new();
        assert_eq!(t.render().lines().count(), TABLE.len());
    }

    #[test]
    fn protocol_absence_matrix() {
        // Every non-fatal row absent: connection stays viable and each row has a fallback.
        let mut t = CapabilityTable::new();
        for s in TABLE.iter().filter(|s| s.requirement == Requirement::Fatal) {
            t.set_bound(s.interface, 6);
        }
        assert_eq!(t.missing_fatal(), None);
        for s in TABLE {
            assert!(
                !s.absent.is_empty(),
                "{} has no absence behaviour",
                s.interface
            );
        }
        // Each fatal row on its own missing is reported by name.
        for fatal in TABLE.iter().filter(|s| s.requirement == Requirement::Fatal) {
            let mut t = CapabilityTable::new();
            for s in TABLE.iter().filter(|s| s.requirement == Requirement::Fatal) {
                if s.interface != fatal.interface {
                    t.set_bound(s.interface, 6);
                }
            }
            assert_eq!(t.missing_fatal(), Some(fatal.interface));
        }
    }

    #[test]
    fn blur_capability_removed_at_runtime_falls_to_tint() {
        assert_eq!(choose_blur(true, true, false), BlurPath::Ext);
        assert_eq!(choose_blur(true, false, false), BlurPath::Tint);
    }

    #[test]
    fn blur_ladder() {
        assert_eq!(choose_blur(true, true, true), BlurPath::Ext);
        assert_eq!(choose_blur(true, false, true), BlurPath::Tint); // Abyss never uses kwin blur
        assert_eq!(choose_blur(false, true, true), BlurPath::Ext);
        assert_eq!(choose_blur(false, false, true), BlurPath::Kwin);
        assert_eq!(choose_blur(false, false, false), BlurPath::Tint);
    }
}
