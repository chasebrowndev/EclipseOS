// SPDX-License-Identifier: AGPL-3.0-only
//! Step 6: profile.
//!
//! Hero: one card, Standard, which is the only profile that exists in this
//! version. The other three profiles and the list of what Standard sets are
//! under "More options".
//! The accented value: the Standard card.

use super::Body;
use crate::data::{self, CARDS, STANDARD_SEEDS};
use crate::model::{Message, Model};
use crate::parts::{self, El};
use ec_ui::tokens::{color, font, size, space};
use ec_ui::widget::{inset_list, list_row, value};
use iced::widget::{column, row, text, Column, Space};
use iced::{Alignment, Length};

pub fn body(m: &Model) -> Body<'_> {
    let mut cards = Column::new().spacing(space::CARD);
    for c in CARDS.iter().filter(|c| m.more || data::selectable(c.profile)) {
        let live = data::selectable(c.profile);
        let (name_ink, body_ink) = if live {
            (color::TEXT, color::TEXT_SECONDARY)
        } else {
            (color::TEXT_TERTIARY, color::TEXT_TERTIARY)
        };
        let mut top = row![
            text(c.name)
                .font(font::UI_SEMIBOLD)
                .size(size::PROMPT)
                .color(name_ink),
            Space::new().width(Length::Fill)
        ]
        .align_y(Alignment::Center);
        if let Some(word) = data::status_word(c.profile) {
            top = top.push(parts::tag(word));
        }
        let content = column![top, text(c.blurb).font(font::UI).size(size::BODY).color(body_ink),]
            .spacing(space::CHIP_GAP + 2.0);
        cards = cards.push(parts::card(
            content,
            Length::Shrink,
            live && m.profile == c.profile,
            live.then_some(Message::Profile(c.profile)),
        ));
    }

    let mut blocks: Vec<El<'_, Message>> = vec![cards.into()];
    if m.more {
        blocks.push(inset_list(
            "Standard sets",
            STANDARD_SEEDS
                .iter()
                .map(|(k, v)| list_row(k, value(v)))
                .collect(),
        ));
        blocks.push(parts::link("Fewer options", Message::More(false)));
    } else {
        blocks.push(parts::link("More options", Message::More(true)));
    }

    Body {
        title: "Your desktop".to_owned(),
        lead: "You can change every part of it afterwards.".to_owned(),
        blocks,
        scroll: true,
    }
}
