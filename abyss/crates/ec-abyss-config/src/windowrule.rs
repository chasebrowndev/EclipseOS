// SPDX-License-Identifier: AGPL-3.0-only
//! `windowrule` blocks: matchers, actions and pattern matching (COMP-05 §4).

use super::*;

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
    pub(crate) fn is_empty(&self) -> bool {
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
pub(crate) fn parse_pair(source: &str, sep: char) -> Option<(i32, i32)> {
    let (a, b) = source.split_once(sep)?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}
