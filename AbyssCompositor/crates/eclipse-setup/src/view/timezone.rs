// SPDX-License-Identifier: AGPL-3.0-only
//! Step 2: time zone.
//!
//! Hero: the *prompt band*, a search box. Type a city, a country or a phrase
//! like "US Eastern"; the places that match are listed under it and the best
//! one is already chosen. Until something is typed the current guess is shown
//! as one line, so the common path is Continue. The local time at the chosen
//! place sits small at the band's right.
//! The accented value: the chosen place.

use super::{list_box, Body};
use crate::model::{ids, Key, Message, Model};
use crate::parts::{self, El};
use crate::sys;
use eclipse_ui::theme;
use eclipse_ui::tokens::{color, font, size, space};
use eclipse_ui::widget::prompt_band;
use iced::widget::{column, row, text, text_input, Column, Space};
use iced::{Alignment, Length};

pub fn body(m: &Model) -> Body<'_> {
    let clock = match &m.zone_clock {
        Some(c) => format!("{} {}", c.time, sys::offset_label(&c.offset)),
        None => "--:--".to_owned(),
    };

    let field = text_input("City or country", &m.zone_query)
        .id(ids::ZONE_SEARCH)
        .on_input(Message::ZoneQuery)
        .on_submit(Message::Key(Key::Enter))
        .font(font::DATA)
        .size(size::PROMPT)
        .padding(0)
        .style(theme::prompt_input);
    let hero = prompt_band("find", field, Some(parts::mono(clock, color::TEXT_TERTIARY)));

    let mut blocks: Vec<El<'_, Message>> = vec![hero];

    let results = m.zone_results();
    if m.zone_query.trim().is_empty() {
        let place = m.zone_entry().map_or_else(
            || crate::data::place_name(&m.zone),
            |e| {
                if e.country.is_empty() {
                    e.place.clone()
                } else {
                    format!("{}, {}", e.place, e.country)
                }
            },
        );
        blocks.push(parts::card(
            column![
                text(place)
                    .font(font::UI_SEMIBOLD)
                    .size(size::PROMPT)
                    .style(theme::text_accent),
                parts::mono(m.zone.clone(), color::TEXT_TERTIARY),
            ]
            .spacing(space::CHIP_GAP),
            Length::Shrink,
            true,
            None,
        ));
    } else if results.is_empty() {
        blocks.push(parts::body(
            "No place matches that. Try a nearby city, or the country.",
        ));
    } else {
        let mut list = Column::new();
        for e in results {
            let content = row![
                text(e.place.clone())
                    .font(font::UI_MEDIUM)
                    .size(size::BODY)
                    .style(theme::text_primary),
                Space::new().width(Length::Fill),
                parts::mono(
                    if e.country.is_empty() {
                        e.zone.clone()
                    } else {
                        e.country.clone()
                    },
                    color::TEXT_TERTIARY
                ),
            ]
            .spacing(space::CARD)
            .align_y(Alignment::Center);
            list = list.push(parts::pick(
                content,
                e.zone == m.zone,
                true,
                Some(Message::Zone(e.zone.clone())),
            ));
        }
        blocks.push(list_box(None, list));
    }

    Body {
        title: "Where are you?".to_owned(),
        lead: "The clock and the calendar follow it.".to_owned(),
        blocks,
        scroll: false,
    }
}
