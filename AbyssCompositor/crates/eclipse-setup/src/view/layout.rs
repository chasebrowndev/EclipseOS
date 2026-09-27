// SPDX-License-Identifier: AGPL-3.0-only
//! Step 9: window layout (`general.layout`).
//!
//! Hero: three cards side by side, each a small picture of how the layout
//! divides a screen with its name and one sentence beneath. Side by side, not
//! stacked like the mode step, because the pictures are what is being compared.
//! The accented value: the selected card.

use super::Body;
use crate::choices::Tiling;
use crate::model::{Message, Model};
use crate::parts::{self, El};
use eclipse_ui::tokens::{color, font, size, space};
use iced::widget::{column, text, Row};
use iced::Length;

pub fn body(m: &Model) -> Body<'_> {
    let mut cards = Row::new().spacing(space::CARD);
    for t in Tiling::ALL {
        let content = column![
            parts::tiles(t),
            text(capitalised(t.id()))
                .font(font::UI_SEMIBOLD)
                .size(size::PROMPT)
                .color(color::TEXT),
            text(t.blurb())
                .font(font::UI)
                .size(size::BODY)
                .color(color::TEXT_SECONDARY),
        ]
        .spacing(space::CARD);
        cards = cards.push(parts::card(
            content,
            Length::Shrink,
            m.choices.tiling == t,
            Some(Message::SetTiling(t)),
        ));
    }
    let blocks: Vec<El<'_, Message>> = vec![cards.into()];
    Body {
        title: "How should windows fit?".to_owned(),
        lead: "New windows are placed for you. You can move them by hand either way.".to_owned(),
        blocks,
        scroll: true,
    }
}

fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}
