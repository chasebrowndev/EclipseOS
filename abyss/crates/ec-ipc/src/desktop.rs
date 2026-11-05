// SPDX-License-Identifier: AGPL-3.0-only
//! Desktop grid, window menu and scripted drag (COMP-05 §7.1, COMP-13 §2.1,
//! §2.2): typed calls for `get_desktop_grid`, `switch_desktop_direction`,
//! `open_window_menu` and `drag_at`.
//!
//! The grid, the switch and the menu are `mode "de"` features; the compositor
//! answers a `denied` error elsewhere ([`crate::Error::is_denied`]) except for
//! `get_desktop_grid`, a read that works in every mode.

use serde_json::{json, Value};

use crate::{Client, Error, Result};

/// A way to step over the desktop grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Left => "left",
            Direction::Right => "right",
            Direction::Up => "up",
            Direction::Down => "down",
        }
    }
}

/// One desktop's occupancy. Counts only: never a title or an app id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Desktop {
    /// 1-based, the workspace number.
    pub index: usize,
    pub occupied: bool,
    pub windows: usize,
}

/// An output's active desktop (1-based) and its own per-desktop counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputDesktops {
    pub output: u64,
    pub name: String,
    pub active: usize,
    pub desktops: Vec<Desktop>,
}

/// The `get_desktop_grid` reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopGrid {
    pub rows: usize,
    pub columns: usize,
    /// Whether stepping off an edge wraps (`general.desktop-wrap`).
    pub wrap: bool,
    /// Whether `switch_desktop_direction` acts (`mode "de"` is running).
    pub directional: bool,
    pub outputs: Vec<OutputDesktops>,
    /// Each desktop's total over every output.
    pub desktops: Vec<Desktop>,
}

fn uint(v: &Value, k: &str) -> Option<usize> {
    v.get(k)?.as_u64().map(|n| n as usize)
}

fn desktops(v: Option<&Value>) -> Option<Vec<Desktop>> {
    v?.as_array()?
        .iter()
        .map(|d| {
            Some(Desktop {
                index: uint(d, "index")?,
                occupied: d.get("occupied")?.as_bool()?,
                windows: uint(d, "windows")?,
            })
        })
        .collect()
}

impl DesktopGrid {
    /// Parse a `get_desktop_grid` reply. `None` if it is not that shape.
    pub fn from_json(v: &Value) -> Option<DesktopGrid> {
        let outputs = v
            .get("outputs")?
            .as_array()?
            .iter()
            .map(|o| {
                Some(OutputDesktops {
                    output: o.get("output")?.as_u64()?,
                    name: o.get("name")?.as_str()?.to_owned(),
                    active: uint(o, "active")?,
                    desktops: desktops(o.get("desktops"))?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(DesktopGrid {
            rows: uint(v, "rows")?,
            columns: uint(v, "columns")?,
            wrap: v.get("wrap")?.as_bool()?,
            directional: v.get("directional")?.as_bool()?,
            outputs,
            desktops: desktops(v.get("desktops"))?,
        })
    }
}

/// A scripted drag (`drag_at`).
#[derive(Debug, Clone, PartialEq)]
pub struct Drag {
    pub from: (f64, f64),
    pub to: (f64, f64),
    /// `left`, `right` or `middle`; `None` is left.
    pub button: Option<&'static str>,
    pub steps: Option<u32>,
    /// `ctrl`, `alt`, `shift`, `logo`, held for the whole drag.
    pub modifiers: Vec<String>,
    /// Keep the button down this long after the last move, so a screenshot can
    /// be taken mid-drag. The compositor releases it from a timer.
    pub hold_ms: Option<u64>,
}

impl Drag {
    pub fn new(from: (f64, f64), to: (f64, f64)) -> Drag {
        Drag {
            from,
            to,
            button: None,
            steps: None,
            modifiers: Vec::new(),
            hold_ms: None,
        }
    }

    /// The `drag_at` params.
    pub fn params(&self) -> Value {
        let mut p = json!({
            "x": self.from.0, "y": self.from.1,
            "to_x": self.to.0, "to_y": self.to.1,
        });
        if let Some(b) = self.button {
            p["button"] = json!(b);
        }
        if let Some(n) = self.steps {
            p["steps"] = json!(n);
        }
        if !self.modifiers.is_empty() {
            p["modifiers"] = json!(self.modifiers);
        }
        if let Some(ms) = self.hold_ms {
            p["hold_ms"] = json!(ms);
        }
        p
    }
}

impl Client {
    /// The desktop grid (COMP-05 §7.1). Works in every mode.
    pub fn desktop_grid(&mut self) -> Result<DesktopGrid> {
        let v = self.call("get_desktop_grid", Value::Null)?;
        DesktopGrid::from_json(&v).ok_or_else(|| Error::Protocol("malformed get_desktop_grid reply".into()))
    }

    /// Step to the neighbouring desktop. `Ok(false)` at an unwrapped edge;
    /// a `denied` error outside `mode "de"`.
    pub fn switch_desktop_direction(&mut self, dir: Direction) -> Result<bool> {
        let v = self.call("switch_desktop_direction", json!({"direction": dir.as_str()}))?;
        Ok(v.get("changed").and_then(Value::as_bool).unwrap_or(false))
    }

    /// Open window `id`'s menu with its corner at `(x, y)`, global logical px.
    /// Allowed for a minimized window (a taskbar chip opens it). Returns the
    /// anchor after the compositor clamped it onto the window's output.
    pub fn open_window_menu(&mut self, id: u64, x: f64, y: f64) -> Result<(i64, i64)> {
        let v = self.call("open_window_menu", json!({"id": id, "x": x, "y": y}))?;
        let at = |k: &str| v.get(k).and_then(Value::as_i64);
        match (at("x"), at("y")) {
            (Some(x), Some(y)) => Ok((x, y)),
            _ => Err(Error::Protocol("malformed open_window_menu reply".into())),
        }
    }

    /// Scripted drag (needs `misc { scripted-input }`). `Ok(true)` when the
    /// button is being held for `hold_ms` rather than already released.
    pub fn drag_at(&mut self, drag: &Drag) -> Result<bool> {
        let v = self.call("drag_at", drag.params())?;
        Ok(v.get("holding").and_then(Value::as_bool).unwrap_or(false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grid_reply_parses() {
        let d = |i: usize, n: usize| json!({"index": i, "occupied": n > 0, "windows": n});
        let v = json!({
            "rows": 2, "columns": 5, "count": 10, "wrap": true, "directional": false,
            "outputs": [{"output": 1, "name": "HEADLESS-1", "active": 3, "desktops": [d(1, 0), d(2, 4)]}],
            "desktops": [d(1, 0), d(2, 4)],
        });
        let g = DesktopGrid::from_json(&v).expect("grid");
        assert_eq!((g.rows, g.columns, g.wrap, g.directional), (2, 5, true, false));
        assert_eq!(g.outputs[0].active, 3);
        assert_eq!(
            g.desktops[1],
            Desktop {
                index: 2,
                occupied: true,
                windows: 4
            }
        );
        assert_eq!(DesktopGrid::from_json(&json!({"rows": 2})), None);
    }

    #[test]
    fn drag_params_carry_only_what_was_asked() {
        let bare = Drag::new((1.0, 2.0), (30.0, 40.0)).params();
        assert_eq!(bare, json!({"x": 1.0, "y": 2.0, "to_x": 30.0, "to_y": 40.0}));
        let full = Drag {
            button: Some("right"),
            steps: Some(4),
            modifiers: vec!["shift".into()],
            hold_ms: Some(500),
            ..Drag::new((1.0, 2.0), (30.0, 40.0))
        }
        .params();
        assert_eq!(full["button"], "right");
        assert_eq!(full["steps"], 4);
        assert_eq!(full["modifiers"], json!(["shift"]));
        assert_eq!(full["hold_ms"], 500);
    }

    #[test]
    fn directions_use_the_wire_names() {
        let names: Vec<_> = [Direction::Left, Direction::Right, Direction::Up, Direction::Down]
            .map(Direction::as_str)
            .into();
        assert_eq!(names, ["left", "right", "up", "down"]);
    }
}
