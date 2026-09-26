// SPDX-License-Identifier: AGPL-3.0-only
//! The clock: time over date, both mono and centred — the only two-line cell
//! on the row, which is what makes the right end read as an end. Important
//! by default; always present.
//!
//! It is also the one place the bar admits the compositor is gone: a
//! disconnected bar dims its clock rather than freezing it.

use iced::widget::{column, text};
use iced::Alignment;

use eclipse_ui::tokens::{bar, color, font, size};
use eclipse_ui::widget::ShellFrame;

use super::{Parts, Spans};
use crate::app::App;

pub fn spans(_app: &App) -> Spans {
    // A fixed width, so a minute rolling over never makes the chips twitch.
    Spans {
        core: bar::CLOCK_W - 2.0 * bar::WIDGET_X,
        revealed: 0.0,
        present: true,
    }
}

pub fn view(app: &App, frame: ShellFrame) -> Parts<'_> {
    let ink = frame.core_alpha();
    let tint = if app.snapshot.connected {
        color::TEXT
    } else {
        color::TEXT_TERTIARY
    };
    // TODO: clicking the clock should open a calendar drawer on the same
    // popup machinery as the tray drawer. Not wired yet: it needs a calendar
    // data path, and inventing one here would be a fake backend.
    let stack = column![
        text(crate::clock::time(app.bar.hour_12))
            .size(size::BODY_SMALL)
            .font(font::DATA_MEDIUM)
            .color(tint.scale_alpha(ink)),
        text(crate::clock::date(app.bar.date_mdy))
            .size(size::MICRO)
            .font(font::DATA)
            .color(color::TEXT_TERTIARY.scale_alpha(ink)),
    ]
    .spacing(0)
    .align_x(Alignment::Center);
    Parts {
        core: stack.into(),
        revealed: None,
    }
}
