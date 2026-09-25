// SPDX-License-Identifier: AGPL-3.0-only
//! Step 7: interaction mode (ADR 0062).
//!
//! Hero: three cards, one per mode, each a name and one sentence; the choice is
//! the whole step. There is no "More options": nothing here is advanced.
//! The accented value: the selected card.

use super::Body;
use crate::choices::Mode;
use crate::model::{Message, Model};
use crate::parts::{self, El};
use eclipse_ui::tokens::{color, font, size, space};
use iced::widget::{column, text, Column};
use iced::Length;

pub fn body(m: &Model) -> Body<'_> {
    let mut cards = Column::new().spacing(space::CARD);
    for mode in Mode::ALL {
        let content = column![
            text(mode.name())
                .font(font::UI_SEMIBOLD)
                .size(size::PROMPT)
                .color(color::TEXT),
            text(mode.blurb())
                .font(font::UI)
                .size(size::BODY)
                .color(color::TEXT_SECONDARY),
        ]
        .spacing(space::CHIP_GAP);
        cards = cards.push(parts::card(
            content,
            Length::Shrink,
            m.choices.mode == mode,
            Some(Message::SetMode(mode)),
        ));
    }
    let blocks: Vec<El<'_, Message>> = vec![cards.into()];
    Body {
        title: "How should it work?".to_owned(),
        lead: "You can change this any time in Settings.".to_owned(),
        blocks,
        scroll: true,
    }
}
