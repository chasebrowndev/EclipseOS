// SPDX-License-Identifier: AGPL-3.0-only
//! Step 12: optional applications.
//!
//! Hero: a grid of nine tick cells in three columns, plain names, one per
//! application, ticked where the profile ticked them. Nothing is required, so
//! Continue is always the answer. There is no "More options".
//! The accented value: none. A tick is a white bar on a lifted ground, so nine
//! ticks are not nine yellows; the count in the lead is the reading.

use super::Body;
use crate::choices::APPS;
use crate::model::{Message, Model};
use crate::parts::{self, El};
use ec_ui::tokens::{color, space};
use iced::widget::{row, Column, Row};
use iced::Length;

const COLUMNS: usize = 3;

pub fn body(m: &Model) -> Body<'_> {
    let mut grid = Column::new().spacing(space::CHIP_GAP);
    for chunk in APPS.chunks(COLUMNS).enumerate() {
        let (r, apps) = chunk;
        let mut line = Row::new().spacing(space::CHIP_GAP);
        for (j, app) in apps.iter().enumerate() {
            let i = r * COLUMNS + j;
            let on = m.choices.apps[i];
            let mark = if on { "[x]" } else { "[ ]" };
            let content = row![
                parts::mono(mark, if on { color::TEXT } else { color::TEXT_TERTIARY }),
                parts::strong(app.name),
            ]
            .spacing(space::CHIP_GAP)
            .align_y(iced::Alignment::Center);
            line = line.push(parts::pick(content, on, false, Some(Message::ToggleApp(i))));
        }
        grid = grid.push(line.width(Length::Fill));
    }
    let n = m.choices.app_count();
    let lead = match n {
        0 => "None chosen. You can add any of them later.".to_owned(),
        1 => "1 chosen. You can add more later.".to_owned(),
        n => format!("{n} chosen. You can add more later."),
    };
    let blocks: Vec<El<'_, Message>> = vec![grid.into()];
    Body {
        title: "Add some apps?".to_owned(),
        lead,
        blocks,
        scroll: true,
    }
}
