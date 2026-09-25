// SPDX-License-Identifier: AGPL-3.0-only
//! Step 1: language.
//!
//! Hero: a list, and nothing else. The keyboard is the next screen, chosen for
//! you from the language.
//! The accented value: the selected language row.

use super::{list_box, Body};
use crate::data::LANGUAGES;
use crate::model::{Message, Model};
use crate::parts;
use eclipse_ui::theme;
use eclipse_ui::tokens::{font, size};
use iced::widget::{row, text, Column, Space};
use iced::{Alignment, Length};

pub fn body(m: &Model) -> Body<'_> {
    let mut langs = Column::new();
    for (i, l) in LANGUAGES.iter().enumerate() {
        let content = row![
            text(l.native)
                .font(font::UI_MEDIUM)
                .size(size::PROMPT)
                .style(theme::text_primary),
            Space::new().width(Length::Fill),
            text(l.english)
                .font(font::UI)
                .size(size::BODY)
                .style(theme::text_tertiary),
        ]
        .align_y(Alignment::Center);
        langs = langs.push(parts::pick(
            content,
            i == m.language,
            true,
            Some(Message::Language(i)),
        ));
    }
    Body {
        title: "Choose your language".to_owned(),
        lead: String::new(),
        blocks: vec![list_box(None, langs)],
        scroll: false,
    }
}
