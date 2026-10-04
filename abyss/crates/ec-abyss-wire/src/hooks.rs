// SPDX-License-Identifier: AGPL-3.0-only
//! The abyss add-on hook set (ADR 0066). Manifest loading stays in
//! `ec-abyss::addons`; only the plain types the gate needs live here.

/// The abyss hooks at v1 (ADR 0066 table). A closed set: adding one is a host
/// change with its own review, never something a manifest can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hook {
    /// `annotation_*` methods; annotation binds forward on `keybind`.
    Annotations,
    /// The `annotation-select` bind action (the region selector).
    RegionSelect,
    /// The `widget` collection and its socket writes.
    TaskbarWidgets,
    /// The agent stack (ADR 0069): the privileged socket, agent and semantic
    /// globals, the `policyd` link and the agent lifecycle methods. Off is
    /// the normal state, not degraded mode (COMP-01 §6).
    Agents,
    /// Add-on transition shaders (ADR 0073): abyss reads the package-owned
    /// transition catalog and compiles its fragment shaders. Not a gated
    /// method; the gate table is unaffected.
    TransitionShaders,
}

impl Hook {
    pub const ALL: [Hook; 5] = [
        Hook::Annotations,
        Hook::RegionSelect,
        Hook::TaskbarWidgets,
        Hook::Agents,
        Hook::TransitionShaders,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Hook::Annotations => "annotations",
            Hook::RegionSelect => "region-select",
            Hook::TaskbarWidgets => "taskbar-widgets",
            Hook::Agents => "agents",
            Hook::TransitionShaders => "transition-shaders",
        }
    }

    /// The refusal a gated method answers with. Static so the gate's
    /// `Decision::Deny(&'static str)` carries it without allocating.
    pub fn off_reason(self) -> &'static str {
        match self {
            Hook::Annotations => "add-on hook `annotations` is off",
            Hook::RegionSelect => "add-on hook `region-select` is off",
            Hook::TaskbarWidgets => "add-on hook `taskbar-widgets` is off",
            Hook::Agents => "add-on hook `agents` is off",
            Hook::TransitionShaders => "add-on hook `transition-shaders` is off",
        }
    }

    pub fn from_name(s: &str) -> Option<Hook> {
        Hook::ALL.into_iter().find(|h| h.name() == s)
    }

    fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// Which hooks are on. A bitset so the input path's check is one bool read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HookSet(u8);

impl HookSet {
    pub fn is_on(self, hook: Hook) -> bool {
        self.0 & hook.bit() != 0
    }

    pub fn insert(&mut self, hook: Hook) {
        self.0 |= hook.bit();
    }

    pub fn names(self) -> Vec<&'static str> {
        Hook::ALL
            .into_iter()
            .filter(|h| self.is_on(*h))
            .map(Hook::name)
            .collect()
    }
}
