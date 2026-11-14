// SPDX-License-Identifier: AGPL-3.0-only
//! Retained per-widget state keyed by stable ids, and the state -> colour mapping.

use crate::draw::Rim;
use crate::tokens::{Palette, Rgba, metrics};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WidgetId(pub u64);

impl WidgetId {
    /// A stable child id: same parent and index always give the same id.
    pub fn child(self, n: u64) -> WidgetId {
        WidgetId(self.0.wrapping_mul(0x0000_0100_0000_01b3).rotate_left(7) ^ n.wrapping_add(1))
    }
}

/// Pointer and keyboard interaction state for the whole interface.
#[derive(Clone, Debug, Default)]
pub struct UiState {
    pub hover: Option<WidgetId>,
    pub pressed: Option<WidgetId>,
    pub focus: Option<WidgetId>,
    /// Focus arrived by keyboard: only then is a focus rim drawn (T-08 §5).
    pub focus_from_keyboard: bool,
}

impl UiState {
    pub fn set_hover(&mut self, id: Option<WidgetId>) {
        self.hover = id;
    }

    pub fn press(&mut self, id: WidgetId) {
        self.pressed = Some(id);
        self.focus = Some(id);
        self.focus_from_keyboard = false;
    }

    pub fn release(&mut self) -> Option<WidgetId> {
        self.pressed.take()
    }

    pub fn focus_by_keyboard(&mut self, id: WidgetId) {
        self.focus = Some(id);
        self.focus_from_keyboard = true;
    }

    /// The five visual states of one control.
    pub fn control(&self, id: WidgetId, disabled: bool, selected: bool) -> ControlState {
        ControlState {
            hover: self.hover == Some(id),
            pressed: self.pressed == Some(id),
            focus_kb: self.focus == Some(id) && self.focus_from_keyboard,
            disabled,
            selected,
        }
    }
}

/// Rest / hover / pressed / keyboard-focused / disabled, plus `selected` (active tab,
/// primary button), which uses the brighter "focus" fill family.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ControlState {
    pub hover: bool,
    pub pressed: bool,
    pub focus_kb: bool,
    pub disabled: bool,
    pub selected: bool,
}

/// Fill and rim for a control: focus and selection are a brighter fill plus a crisp rim.
pub fn surface(p: &Palette, s: ControlState) -> (Rgba, Rim) {
    let bright = s.selected || s.focus_kb;
    let active = !s.disabled;
    let fill = match (bright, active && s.pressed, active && s.hover) {
        (true, true, _) => p.cell_focus_press,
        (true, _, true) => p.cell_focus_hover,
        (true, _, _) => p.cell_focus,
        (false, true, _) => p.cell_press,
        (false, _, true) => p.cell_hover,
        _ => p.cell,
    };
    let rim = if s.focus_kb && active {
        Rim {
            width: metrics::FOCUS_RIM_W,
            colour: p.accent,
        }
    } else {
        let colour = match (bright, active && s.hover) {
            (true, true) => p.cell_focus_rim_hover,
            (true, false) => p.cell_focus_rim,
            (false, true) => p.cell_rim_hover,
            (false, false) => p.cell_rim,
        };
        Rim {
            width: metrics::RIM_W,
            colour,
        }
    };
    if s.disabled {
        (
            fill,
            Rim {
                width: rim.width,
                colour: p.hairline,
            },
        )
    } else {
        (fill, rim)
    }
}

/// Label colour for a control state.
pub fn label_colour(p: &Palette, s: ControlState) -> Rgba {
    if s.disabled {
        p.text_tertiary
    } else if s.selected || s.hover || s.focus_kb {
        p.text
    } else {
        p.text_secondary
    }
}
