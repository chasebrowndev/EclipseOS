// SPDX-License-Identifier: AGPL-3.0-only
//! The switch (T-08 §2): a pill track with a round knob. On = the accent with the on-accent
//! knob; off = the control-off track with the off knob. Hover steps the off track up one
//! step; a keyboard-origin focus draws the crisp focus rim; disabled loses the accent.
//! Drawn into a rect the caller lays out (a row's trailing edge); hit-testing is the row's.

use crate::draw::{DrawList, Rim};
use crate::geom::Rect;
use crate::state::ControlState;
use crate::tokens::{Palette, Rgba, metrics, radius};

/// Width and height of the switch.
pub fn size() -> (f32, f32) {
    (metrics::TOGGLE_W, metrics::TOGGLE_H)
}

/// The switch's track and knob rects inside `rect` (its trailing, vertically centred cell).
pub fn parts(rect: Rect, on: bool) -> (Rect, Rect) {
    let (w, h) = size();
    let track = Rect::new(rect.right() - w, rect.y + (rect.h - h) / 2.0, w, h);
    let d = h - 2.0 * metrics::TOGGLE_KNOB_INSET;
    let x = if on {
        track.right() - metrics::TOGGLE_KNOB_INSET - d
    } else {
        track.x + metrics::TOGGLE_KNOB_INSET
    };
    let knob = Rect::new(x, track.y + metrics::TOGGLE_KNOB_INSET, d, d);
    (track, knob)
}

pub fn draw(rect: Rect, on: bool, st: ControlState, pal: &Palette, out: &mut DrawList) {
    let (track, knob) = parts(rect, on);
    let live = !st.disabled;
    let (fill, knob_fill): (Rgba, Rgba) = if on && live {
        (pal.accent, pal.on_accent)
    } else if live && (st.hover || st.pressed) {
        (pal.control_off_hover, pal.knob_off)
    } else {
        (pal.control_off, pal.knob_off)
    };
    let rim = if st.focus_kb && live {
        Rim {
            width: metrics::FOCUS_RIM_W,
            colour: pal.accent,
        }
    } else {
        Rim {
            width: metrics::RIM_W,
            colour: if on && live {
                pal.accent_border
            } else {
                pal.cell_rim
            },
        }
    };
    let fade = if live { 1.0 } else { 0.5 };
    out.rect(track, radius::PILL, fill.scaled(fade), Some(rim));
    out.rect(knob, radius::PILL, knob_fill.scaled(fade), None);
}
