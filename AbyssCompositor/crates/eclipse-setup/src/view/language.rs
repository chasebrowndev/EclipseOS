// SPDX-License-Identifier: AGPL-3.0-only
//! Step 1: language and keyboard.
//!
//! Hero: the *prompt band*, a live line you type into, because the keyboard is
//! the one thing on this step you can test rather than read about. Under it two
//! lists side by side, which is what the choice really is.
//! The accented value: the selected language row. The layout list, the variant
//! tabs and the band's readings are all neutral.

use super::{list_box, Body};
use crate::data::LANGUAGES;
use crate::model::{ids, KbLive, Key, Message, Model};
use crate::parts::{self, El};
use eclipse_ui::theme;
use eclipse_ui::tokens::{color, font, size, space};
use eclipse_ui::widget::{inset, prompt_band};
use iced::widget::{column, container, row, text, text_input, Column, Row, Space};
use iced::{Alignment, Length};

pub fn body(m: &Model) -> Body<'_> {
    let layout = m.current_layout();
    let layout_name = layout.map_or(m.layout.as_str(), |l| l.name.as_str());
    let reading = match &m.variant {
        Some(v) => format!("{} ({v})", m.layout),
        None => m.layout.clone(),
    };
    let live = match m.kb_live {
        KbLive::Idle => "applying",
        KbLive::Live => "live now",
        KbLive::Offline => "applies to the install",
        KbLive::Refused => "refused",
    };

    // --- hero: the test band
    let field = text_input("Type here to try the keyboard", &m.kb_test)
        .id(ids::KB_TEST)
        .on_input(Message::KbTest)
        .on_submit(Message::Key(Key::Enter))
        .font(font::DATA)
        .size(size::PROMPT)
        .padding(0)
        .style(theme::prompt_input);
    let readings = column![
        parts::mono(reading.clone(), color::TEXT_SECONDARY),
        parts::mono(format!("console {}", m.keymap), color::TEXT_TERTIARY),
        parts::mono(
            live,
            if m.kb_live == KbLive::Refused {
                color::DANGER
            } else {
                color::TEXT_TERTIARY
            }
        ),
    ]
    .align_x(Alignment::End);
    let hero = prompt_band("kbd", field, Some(readings.into()));

    // --- left: languages
    let mut langs = Column::new();
    for (i, l) in LANGUAGES.iter().enumerate() {
        let content = row![
            text(l.native)
                .font(font::UI_MEDIUM)
                .size(size::BODY)
                .style(theme::text_primary),
            Space::new().width(Length::Fill),
            text(l.english)
                .font(font::UI)
                .size(size::BODY_SMALL)
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
    let left = list_box("Language", vec![], None, langs);

    // --- right: layouts
    let filter = parts::input(
        ids::KB_FILTER,
        "Filter layouts",
        &m.kb_filter,
        false,
        false,
        Message::KbFilter,
        Message::Key(Key::Tab),
    );
    let mut layouts = Column::new();
    for l in m.visible_layouts() {
        let content = row![
            text(l.name.clone())
                .font(font::UI)
                .size(size::BODY)
                .style(theme::text_primary),
            Space::new().width(Length::Fill),
            parts::mono(l.code.clone(), color::TEXT_TERTIARY),
        ]
        .align_y(Alignment::Center);
        layouts = layouts.push(parts::pick(
            content,
            l.code == m.layout,
            false,
            Some(Message::Layout(l.code.clone())),
        ));
    }
    let right_list = list_box("Keyboard layout", vec![], Some(filter), layouts);

    let keymaps: Vec<El<'_, Message>> = {
        let choices = m.keymap_choices();
        if choices.len() > 1 {
            choices
                .into_iter()
                .take(10)
                .map(|k| El::from(parts::tab(k, *k == m.keymap, Message::Keymap(k.clone()))))
                .collect()
        } else {
            Vec::new()
        }
    };
    let keymap_row: El<'_, Message> = if keymaps.is_empty() {
        Space::new().height(Length::Shrink).into()
    } else {
        let tabs = Row::with_children(keymaps)
            .spacing(space::PILL_GAP)
            .wrap()
            .vertical_spacing(space::PILL_GAP);
        inset(column![parts::body("Console keymap"), tabs].spacing(space::CHIP_GAP))
            .padding(space::CARD)
            .into()
    };
    let variants: El<'_, Message> = match layout.filter(|l| !l.variants.is_empty()) {
        Some(l) => {
            let first = El::from(parts::tab("default", m.variant.is_none(), Message::Variant(None)));
            let rest = l.variants.iter().take(10).map(|v| {
                El::from(parts::tab(
                    &v.code,
                    m.variant.as_deref() == Some(v.code.as_str()),
                    Message::Variant(Some(v.code.clone())),
                ))
            });
            let tabs = Row::with_children(std::iter::once(first).chain(rest))
                .spacing(space::PILL_GAP)
                .wrap()
                .vertical_spacing(space::PILL_GAP);
            inset(column![parts::body(format!("{layout_name} variants")), tabs].spacing(space::CHIP_GAP))
                .padding(space::CARD)
                .into()
        }
        None => Space::new().height(Length::Shrink).into(),
    };
    let right =
        column![container(right_list).height(Length::Fill), variants, keymap_row].spacing(space::BLOCK);

    let columns = row![
        container(left).width(Length::FillPortion(2)).height(Length::Fill),
        container(right)
            .width(Length::FillPortion(3))
            .height(Length::Fill),
    ]
    .spacing(space::BLOCK)
    .height(Length::Fill);

    Body {
        subtitle: "Choose the language of the installed system, and check the keyboard.".to_owned(),
        chip: (
            LANGUAGES[m.language.min(LANGUAGES.len() - 1)].locale.to_owned(),
            format!("kb {reading}"),
        ),
        blocks: vec![hero, columns.into()],
        scroll: false,
    }
}
