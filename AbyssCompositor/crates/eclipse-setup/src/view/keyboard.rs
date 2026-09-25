// SPDX-License-Identifier: AGPL-3.0-only
//! Step 1, second screen: keyboard.
//!
//! Hero: the *prompt band*, a live line you type into, because the keyboard is
//! the one thing here you can test rather than read about. Under it the layouts,
//! already set from the language. Variants and the console keymap are one
//! click away under "More options", which replaces the list while it is open.
//! The accented value: the selected layout row.

use super::{list_box, Body};
use crate::model::{ids, KbLive, Key, Message, Model};
use crate::parts::{self, El};
use eclipse_ui::theme;
use eclipse_ui::tokens::{color, font, size, space};
use eclipse_ui::widget::{inset, prompt_band};
use iced::widget::{column, row, text, text_input, Column, Row, Space};
use iced::{Alignment, Length};

pub fn body(m: &Model) -> Body<'_> {
    let layout = m.current_layout();
    let layout_name = layout.map_or(m.layout.as_str(), |l| l.name.as_str());

    let field = text_input("Type here to try the keyboard", &m.kb_test)
        .id(ids::KB_TEST)
        .on_input(Message::KbTest)
        .on_submit(Message::Key(Key::Enter))
        .font(font::DATA)
        .size(size::PROMPT)
        .padding(0)
        .style(theme::prompt_input);
    let refused: Option<El<'_, Message>> =
        (m.kb_live == KbLive::Refused).then(|| parts::mono("refused", color::DANGER));
    let hero = prompt_band("kbd", field, refused);

    let mut blocks: Vec<El<'_, Message>> = vec![hero];

    if m.more {
        blocks.push(options(m, layout_name));
        blocks.push(parts::link("Fewer options", Message::More(false)));
    } else {
        let filter = parts::input(
            ids::KB_FILTER,
            "Search layouts",
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
                true,
                Some(Message::Layout(l.code.clone())),
            ));
        }
        blocks.push(list_box(Some(filter), layouts));
        blocks.push(parts::link("More options", Message::More(true)));
    }

    Body {
        title: "Check your keyboard".to_owned(),
        lead: format!("{layout_name}. Type above to try it."),
        blocks,
        scroll: false,
    }
}

/// Variants of the chosen layout, and the console keymap the installed system
/// boots with.
fn options<'a>(m: &'a Model, layout_name: &str) -> El<'a, Message> {
    let mut col = Column::new().spacing(space::BLOCK);

    if let Some(l) = m.current_layout().filter(|l| !l.variants.is_empty()) {
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
        col = col.push(column![parts::body(format!("{layout_name} variant")), tabs].spacing(space::CHIP_GAP));
    }

    let choices = m.keymap_choices();
    if choices.len() > 1 {
        let tabs = Row::with_children(
            choices
                .into_iter()
                .take(10)
                .map(|k| El::from(parts::tab(k, *k == m.keymap, Message::Keymap(k.clone())))),
        )
        .spacing(space::PILL_GAP)
        .wrap()
        .vertical_spacing(space::PILL_GAP);
        col = col.push(column![parts::body("Console keymap"), tabs].spacing(space::CHIP_GAP));
    } else {
        col = col.push(parts::mono(
            format!("console keymap {}", m.keymap),
            color::TEXT_TERTIARY,
        ));
    }
    inset(col).padding(space::CARD).width(Length::Fill).into()
}
