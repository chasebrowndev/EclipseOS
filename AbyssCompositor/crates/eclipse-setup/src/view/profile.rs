// SPDX-License-Identifier: AGPL-3.0-only
//! Step 6: profile.
//!
//! Hero: a status grid, four cards in two rows, one per profile. Only Standard
//! exists in this version, so it is the one that answers the pointer; the other
//! three stay visible and flat with the word for why (`coming`, `preview`).
//! Under it a plain list of exactly what Standard sets.
//! The accented value: the Standard card.

use super::Body;
use crate::data::{self, CARDS, STANDARD_SEEDS};
use crate::metrics;
use crate::model::{Message, Model};
use crate::parts::{self, El};
use eclipse_ui::tokens::{color, font, size, space};
use eclipse_ui::widget::{inset_list, list_row, value};
use iced::widget::{column, container, row, text, Space};
use iced::{Alignment, Length};

pub fn body(m: &Model) -> Body<'_> {
    let cells: Vec<El<'_, Message>> = CARDS
        .iter()
        .map(|c| {
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
            let content = column![
                top,
                text(c.blurb).font(font::UI).size(size::BODY).color(body_ink),
                Space::new().height(Length::Fill),
                parts::mono(c.meta, color::TEXT_TERTIARY),
            ]
            .spacing(space::CHIP_GAP + 2.0);
            parts::card(
                content,
                Length::Fixed(metrics::CARD_H),
                live && m.profile == c.profile,
                live.then_some(Message::Profile(c.profile)),
            )
        })
        .collect();

    let mut it = cells.into_iter();
    let mut grid = iced::widget::Column::new().spacing(space::CARD);
    while let Some(a) = it.next() {
        let b: El<'_, Message> = it
            .next()
            .unwrap_or_else(|| Space::new().width(Length::Fill).into());
        grid = grid.push(
            row![container(a).width(Length::Fill), container(b).width(Length::Fill)].spacing(space::CARD),
        );
    }

    let seeds = inset_list(
        "Standard sets",
        STANDARD_SEEDS
            .iter()
            .map(|(k, v)| list_row(k, value(v)))
            .collect(),
    );

    Body {
        subtitle: "What the desktop starts as. You can change every part of it afterwards.".to_owned(),
        chip: (
            "standard".to_owned(),
            format!("{} settings seeded", STANDARD_SEEDS.len()),
        ),
        blocks: vec![grid.into(), seeds],
        scroll: true,
    }
}
