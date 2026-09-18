// SPDX-License-Identifier: AGPL-3.0-only

//! Which screen the human is actually looking at (ADR 0042).
//!
//! Automatic mode used to walk every output round-robin, so two thirds of its
//! answers were about a monitor nobody was facing — and all of them shared one
//! display slot, so the one you *were* facing had to wait its turn. Asking the
//! compositor which output has focus turns that into one screen per tick.
//!
//! This reads `get_outputs`, a COMP-13 query, and uses exactly two things from
//! it: the `focused` flag and enough geometry to rebuild a logical rect. It is
//! read-only, and the compositor does not behave differently because we call
//! it — the "never load-bearing" invariant is untouched. It is still a widening
//! of the control-socket surface beyond `annotation_*`, which is why there is
//! an ADR.

use serde_json::{json, Value};

use crate::frame::Region;
use crate::hud::Control;

/// The logical rect of the focused output, or `None` when the compositor
/// reports no focus or an output too incompletely to place. A caller with
/// `None` should fall back to its old behaviour rather than guess: guessing
/// annotates the wrong screen, which is the bug this exists to fix.
pub fn focused_output(c: &mut impl Control) -> Option<Region> {
    let reply = c.call("get_outputs", json!({})).ok()?;
    let rows = reply.as_array()?;
    rows.iter().filter(|o| focused(o)).find_map(rect_of)
}

fn focused(o: &Value) -> bool {
    o.get("focused").and_then(Value::as_bool).unwrap_or(false)
}

/// Rebuild the logical rectangle from the pieces `get_outputs` reports: a
/// position already in logical space, and a mode in physical pixels that has
/// to be divided by the scale. A rotated output swaps the two, the same way
/// the compositor's own logical geometry does.
fn rect_of(o: &Value) -> Option<Region> {
    let pos = o.get("position")?;
    let x = pos.get("x")?.as_i64()? as i32;
    let y = pos.get("y")?.as_i64()? as i32;
    let mode = o.get("mode")?;
    let mw = mode.get("width")?.as_i64()? as f64;
    let mh = mode.get("height")?.as_i64()? as f64;
    let scale = o
        .get("scale")
        .and_then(Value::as_f64)
        .filter(|s| *s > 0.0)
        .unwrap_or(1.0);
    let (mw, mh) = if turned(o) { (mh, mw) } else { (mw, mh) };
    let w = (mw / scale).round() as i32;
    let h = (mh / scale).round() as i32;
    if w <= 0 || h <= 0 {
        return None;
    }
    Some(Region { x, y, w, h })
}

/// A quarter-turn, in either direction and flipped or not, is the case where
/// the mode's width is the logical height.
fn turned(o: &Value) -> bool {
    let t = o
        .get("transform")
        .and_then(Value::as_str)
        .unwrap_or("normal");
    t.contains("90") || t.contains("270")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(Value);

    impl Control for Fake {
        fn call(&mut self, method: &str, _params: Value) -> Result<Value, String> {
            assert_eq!(method, "get_outputs");
            Ok(self.0.clone())
        }
    }

    fn out(x: i64, w: i64, h: i64, scale: f64, focused: bool) -> Value {
        json!({
            "focused": focused,
            "scale": scale,
            "transform": "normal",
            "position": {"x": x, "y": 0},
            "mode": {"width": w, "height": h, "refresh": 60000},
        })
    }

    #[test]
    fn the_focused_output_is_the_one_returned() {
        let mut c = Fake(json!([
            out(-1920, 1920, 1080, 1.0, false),
            out(0, 2560, 1440, 1.0, true),
        ]));
        assert_eq!(
            focused_output(&mut c),
            Some(Region {
                x: 0,
                y: 0,
                w: 2560,
                h: 1440
            })
        );
    }

    #[test]
    fn a_scaled_output_reports_its_logical_size_not_its_mode() {
        let mut c = Fake(json!([out(0, 3840, 2160, 2.0, true)]));
        let r = focused_output(&mut c).expect("focused");
        assert_eq!((r.w, r.h), (1920, 1080));
    }

    #[test]
    fn a_rotated_output_swaps_width_and_height() {
        let mut c = Fake(json!([{
            "focused": true, "scale": 1.0, "transform": "90",
            "position": {"x": 0, "y": 0},
            "mode": {"width": 1920, "height": 1080, "refresh": 60000},
        }]));
        let r = focused_output(&mut c).expect("focused");
        assert_eq!((r.w, r.h), (1080, 1920));
    }

    #[test]
    fn nothing_focused_is_none_rather_than_the_first_screen() {
        let mut c = Fake(json!([out(0, 1920, 1080, 1.0, false)]));
        assert_eq!(focused_output(&mut c), None);
    }

    #[test]
    fn an_output_with_no_mode_yet_is_skipped_not_placed_at_zero() {
        let mut c = Fake(json!([{
            "focused": true, "scale": 1.0, "transform": "normal",
            "position": {"x": 0, "y": 0}, "mode": null,
        }]));
        assert_eq!(focused_output(&mut c), None);
    }
}
