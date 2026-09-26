// SPDX-License-Identifier: AGPL-3.0-only
//! Battery: the drawn gauge and its percentage. Important by default, so it
//! never compresses; present only on a machine that has one.
//!
//! A status widget: it reads `app.battery`, so it has no `State` of its own.

use iced::widget::{text, Row};
use iced::Alignment;

use eclipse_services::status::{Battery, Charge};
use eclipse_ui::tokens::{bar, color, font, size, space};
use eclipse_ui::widget::{self as parts, ShellFrame};

use super::{Parts, Spans};
use crate::app::App;

/// Percentage at or below which a discharging battery is drawn as a warning.
pub(crate) const LOW: u8 = 15;

/// Gauge, gap, then the text at its own length: a fixed cell clipped "76%"
/// against its edge, and "+100%" would not have fit at all.
pub fn spans(app: &App) -> Spans {
    let chars = app.battery.map_or(0, |b| battery_text(b).chars().count());
    Spans {
        core: space::MARK_CELL_W + bar::GAP + chars as f32 * bar::CHAR_W,
        revealed: 0.0,
        present: app.battery.is_some(),
    }
}

pub fn view(app: &App, frame: ShellFrame) -> Parts<'_> {
    let ink = frame.core_alpha();
    let Some(battery) = app.battery else {
        return Parts::empty();
    };
    // Low and not charging is the one status the human has to act on, so it
    // is the one status allowed to leave the neutral palette.
    let tint = if battery.percent <= LOW && battery.state == Charge::Discharging {
        color::DANGER
    } else {
        color::TEXT_SECONDARY
    };
    let face = Row::new()
        .spacing(bar::GAP)
        .align_y(Alignment::Center)
        .push(parts::battery_gauge_faded(battery.percent, tint, ink))
        .push(
            text(battery_text(battery))
                .size(size::MONO)
                .font(font::DATA)
                .color(tint.scale_alpha(ink)),
        );
    Parts {
        core: face.into(),
        revealed: None,
    }
}

/// Charging is a leading `+`, discharging bare, full the word. Time remaining
/// is deliberately absent: it is the least trustworthy number UPower reports.
pub(crate) fn battery_text(battery: Battery) -> String {
    match battery.state {
        Charge::Charging => format!("+{}%", battery.percent),
        Charge::Full => "full".to_owned(),
        Charge::Discharging | Charge::Unknown => format!("{}%", battery.percent),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_text_marks_charging_and_full() {
        let b = |percent, state| Battery {
            percent,
            state,
            remaining: None,
        };
        assert_eq!(battery_text(b(40, Charge::Charging)), "+40%");
        assert_eq!(battery_text(b(100, Charge::Full)), "full");
        assert_eq!(battery_text(b(7, Charge::Discharging)), "7%");
    }
}
