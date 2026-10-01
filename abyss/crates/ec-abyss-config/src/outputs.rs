// SPDX-License-Identifier: AGPL-3.0-only
//! Output-rule data the config parser needs (COMP-03): overscan insets, the
//! `mode` string grammar and the transform names. Geometry that needs
//! smithay's `Size`/`Rectangle` lives in `ec-abyss`'s `outputs::overscan`.

/// Per-edge inset in physical pixels. All zero means "no overscan
/// compensation", which is the only state in which the render path is
/// untouched and direct scanout stays available.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Overscan {
    pub top: i32,
    pub bottom: i32,
    pub left: i32,
    pub right: i32,
}

impl Overscan {
    /// The same inset on all four edges.
    pub fn uniform(px: i32) -> Self {
        Self {
            top: px,
            bottom: px,
            left: px,
            right: px,
        }
    }

    pub fn is_zero(&self) -> bool {
        *self == Self::default()
    }

    /// Step one edge. `outward` grows the desktop back towards the panel edge.
    pub fn nudge(&mut self, edge: Edge, step: i32, outward: bool) {
        let d = if outward { -step } else { step };
        let slot = match edge {
            Edge::Top => &mut self.top,
            Edge::Bottom => &mut self.bottom,
            Edge::Left => &mut self.left,
            Edge::Right => &mut self.right,
        };
        *slot = (*slot + d).max(0);
    }

    /// Step all four edges at once.
    pub fn nudge_all(&mut self, step: i32, outward: bool) {
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            self.nudge(edge, step, outward);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

/// Resolved meaning of one keypress while calibrating. Computed in the input
/// filter, which only sees `&AbyssState`, and applied by `outputs::calibrate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Move one edge by `px`; positive is inward.
    Edge(Edge, i32),
    /// Move all four edges by `px`; positive is inward.
    All(i32),
    Next,
    Commit,
    Cancel,
    Reset,
    /// A key with no meaning here. Swallowed anyway — see the module docs.
    Ignored,
}

/// Transform names accepted by `output { transform }` (the compositor maps
/// them to smithay's `Transform`).
pub fn is_transform_name(s: &str) -> bool {
    matches!(
        s,
        "normal" | "0" | "90" | "180" | "270" | "flipped" | "flipped-90" | "flipped-180" | "flipped-270"
    )
}

/// `1920x1080` or `1920x1080@60000` (mHz) or `1920x1080@60` (Hz).
pub fn parse_mode(s: &str) -> Option<(i32, i32, i32)> {
    let (dims, refresh) = match s.split_once('@') {
        Some((d, r)) => (d, Some(r)),
        None => (s, None),
    };
    let (w, h) = dims.trim().split_once('x')?;
    let w: i32 = w.trim().parse().ok()?;
    let h: i32 = h.trim().parse().ok()?;
    let r = match refresh {
        None => 0,
        Some(r) => {
            let r = r.trim();
            let hz: f64 = r.parse().ok()?;
            // Anything under 1000 is plainly Hz, not mHz.
            if hz < 1000.0 {
                (hz * 1000.0).round() as i32
            } else {
                hz.round() as i32
            }
        }
    };
    Some((w, h, r))
}

/// Read `top=`/`bottom=`/`left=`/`right=` properties off a node. Shared with
/// the state-file reader so the two spellings cannot drift. An `overscan` node: bare `overscan 30` or `overscan top=20 left=40`.
pub fn overscan_of(node: &kdl::KdlNode) -> Overscan {
    let prop = |name: &str| -> Option<i32> { node.get(name).and_then(|v| v.as_integer()).map(|i| i as i32) };
    // A bare `overscan 30` means all four edges, which is what a human writing
    // this by hand almost always wants.
    let all = node
        .entries()
        .iter()
        .find(|e| e.name().is_none() && e.value().as_integer().is_some())
        .and_then(|e| e.value().as_integer())
        .map(|i| i as i32)
        .unwrap_or(0);
    Overscan {
        top: prop("top").unwrap_or(all),
        bottom: prop("bottom").unwrap_or(all),
        left: prop("left").unwrap_or(all),
        right: prop("right").unwrap_or(all),
    }
}
