// SPDX-License-Identifier: AGPL-3.0-only
//! Outputs as `get_outputs` reports them (COMP-03 §1).

use ec_ui::widget::arrangement_geometry::Rect;
use serde_json::Value;

/// A video mode as the compositor reports it: pixels, and refresh in mHz
/// (the unit Smithay and `set_output {mode}` both use).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    pub w: i64,
    pub h: i64,
    pub mhz: i64,
}

impl Mode {
    fn parse(v: &Value) -> Option<Self> {
        Some(Mode {
            w: v.get("width")?.as_i64()?,
            h: v.get("height")?.as_i64()?,
            mhz: v.get("refresh")?.as_f64()?.round() as i64,
        })
    }

    /// `1920×1080`.
    pub fn resolution(self) -> String {
        format!("{}\u{d7}{}", self.w, self.h)
    }

    /// `144 Hz`, or `59.94 Hz` when the rate is not a whole number.
    pub fn refresh(self) -> String {
        let hz = self.mhz as f64 / 1000.0;
        if (hz - hz.round()).abs() < 0.05 {
            format!("{} Hz", hz.round() as i64)
        } else {
            format!("{hz:.2} Hz")
        }
    }

    /// What `set_output {mode}` expects: exact, in mHz.
    pub fn wire(self) -> String {
        format!("{}x{}@{}", self.w, self.h, self.mhz)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    pub id: u64,
    pub name: String,
    pub identity: String,
    pub enabled: bool,
    pub focused: bool,
    pub scale: f64,
    pub transform: String,
    /// The current mode, or `None` when the output is off.
    pub mode: Option<Mode>,
    /// Every mode the panel offers. Empty when the compositor does not report
    /// them (`get_outputs` has no `modes` field yet): the picker then shows
    /// only the current mode.
    pub modes: Vec<Mode>,
    /// Logical position (the compositor's layout space).
    pub position: Option<(i64, i64)>,
    /// The overscan the compositor is applying right now, in physical pixels.
    /// Absent (an older compositor, or none set) reads as all zeros.
    pub overscan: Inset,
}

impl Output {
    pub fn parse(v: &Value) -> Option<Self> {
        Some(Output {
            id: v.get("id")?.as_u64()?,
            name: v.get("name")?.as_str()?.to_owned(),
            identity: v
                .get("identity")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            enabled: v.get("enabled").and_then(Value::as_bool).unwrap_or(false),
            focused: v.get("focused").and_then(Value::as_bool).unwrap_or(false),
            scale: v.get("scale").and_then(Value::as_f64).unwrap_or(1.0),
            transform: v
                .get("transform")
                .and_then(Value::as_str)
                .unwrap_or("normal")
                .to_owned(),
            mode: v.get("mode").and_then(Mode::parse),
            modes: v
                .get("modes")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Mode::parse).collect())
                .unwrap_or_default(),
            position: v
                .get("position")
                .and_then(|p| Some((p.get("x")?.as_i64()?, p.get("y")?.as_i64()?))),
            overscan: v.get("overscan").map(Inset::parse).unwrap_or_default(),
        })
    }

    /// `3440×1440 @ 144 Hz`, or a dash when the output is off.
    pub fn mode_display(&self) -> String {
        match self.mode {
            Some(m) => format!("{} @ {}", m.resolution(), m.refresh()),
            None => "\u{2014}".into(),
        }
    }

    pub fn position_display(&self) -> String {
        match self.position {
            Some((x, y)) => format!("{x}, {y}"),
            None => "\u{2014}".into(),
        }
    }

    /// The size this output occupies in the layout: its mode over its scale,
    /// swapped when the transform stands it on its side.
    pub fn logical_size(&self) -> Option<(i64, i64)> {
        let m = self.mode?;
        let s = if self.scale > 0.0 { self.scale } else { 1.0 };
        let (w, h) = ((m.w as f64 / s).round() as i64, (m.h as f64 / s).round() as i64);
        Some(if self.on_its_side() { (h, w) } else { (w, h) })
    }

    fn on_its_side(&self) -> bool {
        matches!(
            self.transform.as_str(),
            "90" | "270" | "flipped-90" | "flipped-270"
        )
    }

    /// Where it sits on the arrangement canvas. Only an output that is on and
    /// placed has a place; the rest are listed beside the canvas.
    pub fn rect(&self) -> Option<Rect> {
        if !self.enabled {
            return None;
        }
        let (x, y) = self.position?;
        let (w, h) = self.logical_size()?;
        Some(Rect::new(x, y, w, h))
    }

    /// The distinct resolutions on offer, largest first. The current mode
    /// stands in when the compositor lists none.
    pub fn resolutions(&self) -> Vec<(i64, i64)> {
        let mut v: Vec<(i64, i64)> = self.modes.iter().map(|m| (m.w, m.h)).collect();
        if v.is_empty() {
            v.extend(self.mode.map(|m| (m.w, m.h)));
        }
        v.sort_by_key(|&(w, h)| std::cmp::Reverse(w * h));
        v.dedup();
        v
    }

    /// The refresh rates available at one resolution, fastest first.
    pub fn refreshes(&self, w: i64, h: i64) -> Vec<Mode> {
        let mut v: Vec<Mode> = self
            .modes
            .iter()
            .copied()
            .filter(|m| m.w == w && m.h == h)
            .collect();
        if v.is_empty() {
            v.extend(self.mode.filter(|m| m.w == w && m.h == h));
        }
        v.sort_by_key(|m| std::cmp::Reverse(m.mhz));
        v.dedup();
        v
    }
}

/// `get_outputs` answers with a bare array, not an object.
pub fn parse_all(reply: &Value) -> Vec<Output> {
    reply
        .as_array()
        .map(|a| a.iter().filter_map(Output::parse).collect())
        .unwrap_or_default()
}

/// The four overscan insets, in the order the compositor names them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Inset {
    pub top: i64,
    pub right: i64,
    pub bottom: i64,
    pub left: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Edge {
    Top,
    Right,
    Bottom,
    Left,
}

impl Edge {
    pub const ALL: &'static [Edge] = &[Edge::Top, Edge::Right, Edge::Bottom, Edge::Left];

    pub fn label(self) -> &'static str {
        match self {
            Edge::Top => "top",
            Edge::Right => "right",
            Edge::Bottom => "bottom",
            Edge::Left => "left",
        }
    }
}

impl Inset {
    /// `{"top": N, "right": N, "bottom": N, "left": N}`, defensively: a
    /// missing or malformed edge is zero, not a dropped output.
    pub fn parse(v: &Value) -> Self {
        let edge = |k: &str| v.get(k).and_then(Value::as_i64).unwrap_or(0).max(0);
        Inset {
            top: edge("top"),
            right: edge("right"),
            bottom: edge("bottom"),
            left: edge("left"),
        }
    }

    pub fn get(self, edge: Edge) -> i64 {
        match edge {
            Edge::Top => self.top,
            Edge::Right => self.right,
            Edge::Bottom => self.bottom,
            Edge::Left => self.left,
        }
    }

    pub fn set(&mut self, edge: Edge, v: i64) {
        let v = v.clamp(0, 10_000);
        match edge {
            Edge::Top => self.top = v,
            Edge::Right => self.right = v,
            Edge::Bottom => self.bottom = v,
            Edge::Left => self.left = v,
        }
    }

    pub fn to_json(self) -> Value {
        serde_json::json!({
            "top": self.top, "right": self.right,
            "bottom": self.bottom, "left": self.left,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_disabled_output_parses_without_a_mode() {
        let o = Output::parse(&json!({
            "id": 3, "name": "DP-2", "identity": "Dell", "enabled": false,
            "focused": false, "scale": 1.0, "transform": "normal",
            "mode": null, "position": null
        }))
        .unwrap();
        assert_eq!(o.mode_display(), "\u{2014}");
        assert_eq!(o.position_display(), "\u{2014}");
        assert_eq!(o.rect(), None);
        assert_eq!(o.overscan, Inset::default());
    }

    #[test]
    fn overscan_comes_from_the_compositor() {
        let o = Output::parse(&json!({
            "id": 1, "name": "DP-1", "enabled": true, "scale": 1.0,
            "overscan": { "top": 12, "right": 8, "bottom": 12, "left": -4 }
        }))
        .unwrap();
        assert_eq!(o.overscan.top, 12);
        assert_eq!(o.overscan.right, 8);
        // A negative edge is not an inset; it reads as none.
        assert_eq!(o.overscan.left, 0);
    }

    #[test]
    fn insets_clamp_to_the_compositors_range() {
        let mut i = Inset::default();
        i.set(Edge::Top, -5);
        i.set(Edge::Left, 99_999);
        assert_eq!(i.top, 0);
        assert_eq!(i.left, 10_000);
    }

    fn on(mode: (i64, i64, i64), scale: f64, transform: &str, at: (i64, i64)) -> Output {
        Output::parse(&json!({
            "id": 1, "name": "DP-1", "enabled": true, "scale": scale, "transform": transform,
            "mode": { "width": mode.0, "height": mode.1, "refresh": mode.2 },
            "position": { "x": at.0, "y": at.1 },
        }))
        .unwrap()
    }

    #[test]
    fn the_layout_size_is_the_mode_over_the_scale() {
        let o = on((3840, 2160, 60_000), 2.0, "normal", (10, 20));
        assert_eq!(o.rect(), Some(Rect::new(10, 20, 1920, 1080)));
    }

    #[test]
    fn a_rotated_output_stands_on_its_side() {
        let o = on((1920, 1080, 60_000), 1.0, "90", (0, 0));
        assert_eq!(o.logical_size(), Some((1080, 1920)));
        let o = on((1920, 1080, 60_000), 1.0, "180", (0, 0));
        assert_eq!(o.logical_size(), Some((1920, 1080)));
    }

    #[test]
    fn a_fractional_scale_rounds_the_layout_size() {
        let o = on((2560, 1440, 144_000), 1.5, "normal", (0, 0));
        assert_eq!(o.logical_size(), Some((1707, 960)));
    }

    #[test]
    fn modes_read_in_hertz_and_go_back_in_millihertz() {
        let m = Mode {
            w: 2560,
            h: 1440,
            mhz: 143_998,
        };
        assert_eq!(m.refresh(), "144 Hz");
        assert_eq!(
            Mode {
                w: 1,
                h: 1,
                mhz: 59_940
            }
            .refresh(),
            "59.94 Hz"
        );
        assert_eq!(m.wire(), "2560x1440@143998");
        assert_eq!(m.resolution(), "2560\u{d7}1440");
    }

    #[test]
    fn the_mode_list_groups_by_resolution() {
        let o = Output::parse(&json!({
            "id": 1, "name": "DP-1", "enabled": true,
            "mode": { "width": 2560, "height": 1440, "refresh": 144000 },
            "modes": [
                { "width": 1920, "height": 1080, "refresh": 60000 },
                { "width": 2560, "height": 1440, "refresh": 60000 },
                { "width": 2560, "height": 1440, "refresh": 144000 },
            ],
        }))
        .unwrap();
        assert_eq!(o.resolutions(), vec![(2560, 1440), (1920, 1080)]);
        let hz: Vec<i64> = o.refreshes(2560, 1440).iter().map(|m| m.mhz).collect();
        assert_eq!(hz, vec![144_000, 60_000]);
    }

    #[test]
    fn without_a_mode_list_the_current_mode_stands_alone() {
        let o = on((1920, 1080, 60_000), 1.0, "normal", (0, 0));
        assert!(o.modes.is_empty());
        assert_eq!(o.resolutions(), vec![(1920, 1080)]);
        assert_eq!(o.refreshes(1920, 1080).len(), 1);
    }
}
