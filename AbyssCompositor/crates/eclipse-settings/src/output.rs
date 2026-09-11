// SPDX-License-Identifier: AGPL-3.0-only
//! Outputs as `get_outputs` reports them (COMP-03 §1).

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    pub id: u64,
    pub name: String,
    pub identity: String,
    pub enabled: bool,
    pub focused: bool,
    pub scale: f64,
    pub transform: String,
    /// `WxH@R`, or `None` when the output is off.
    pub mode: Option<String>,
    pub position: Option<(i64, i64)>,
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
            mode: v.get("mode").and_then(mode_string),
            position: v
                .get("position")
                .and_then(|p| Some((p.get("x")?.as_i64()?, p.get("y")?.as_i64()?))),
        })
    }

    /// What `set_output {mode}` expects back.
    pub fn mode_display(&self) -> String {
        self.mode.clone().unwrap_or_else(|| "—".into())
    }

    pub fn position_display(&self) -> String {
        match self.position {
            Some((x, y)) => format!("{x}, {y}"),
            None => "—".into(),
        }
    }
}

fn mode_string(v: &Value) -> Option<String> {
    let w = v.get("width")?.as_i64()?;
    let h = v.get("height")?.as_i64()?;
    let r = v.get("refresh")?.as_f64()?;
    Some(format!("{w}x{h}@{r:.0}"))
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
        assert_eq!(o.mode_display(), "—");
        assert_eq!(o.position_display(), "—");
    }

    #[test]
    fn insets_clamp_to_the_compositors_range() {
        let mut i = Inset::default();
        i.set(Edge::Top, -5);
        i.set(Edge::Left, 99_999);
        assert_eq!(i.top, 0);
        assert_eq!(i.left, 10_000);
    }
}
