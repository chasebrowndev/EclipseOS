// SPDX-License-Identifier: AGPL-3.0-only
//! Step 13: review.
//!
//! Hero: the danger band. The whole step is one fact, that a disk is about to be
//! erased, and the band says it before anything else. Under it four lines of
//! what you chose, the Wi-Fi carry choice when there is Wi-Fi to carry, and last
//! the field that arms the button.
//! The accented value: the armed Apply button in the footer. The band is the
//! status red, the "matches" reading is the OK green. Apply stays disabled
//! until the disk's own name has been typed again (D-07 §4.2).

use super::{ok_ink, Body};
use crate::choices::NONE;
use crate::data::{format_size, CARDS, LANGUAGES};
use crate::model::{ids, Key, Message, Model};
use crate::parts::{self, El, Tone};
use eclipse_ui::theme;
use eclipse_ui::tokens::{color, font, size, space};
use eclipse_ui::widget::{micro_label, prompt_band};
use iced::widget::{column, row, text_input};
use iced::Length;

/// The chosen profile's name as its card says it.
fn profile_name(m: &Model) -> &'static str {
    CARDS
        .iter()
        .find(|c| c.profile == m.profile)
        .map_or("Custom", |c| c.name)
}

pub fn body(m: &Model) -> Body<'_> {
    let title = "Ready to install".to_owned();
    let Some(disk) = m.selected_disk() else {
        return Body {
            title,
            lead: "No disk is chosen. Go back and choose one.".to_owned(),
            blocks: vec![],
            scroll: false,
        };
    };

    let banner = parts::banner(
        Tone::Danger,
        "erase",
        &format!("Everything on {} will be erased", disk.model),
        &format!(
            "The whole {} disk is wiped and cannot be recovered. Nothing else is touched.",
            format_size(disk.size_bytes)
        ),
        vec![],
    );

    let lang = &LANGUAGES[m.language.min(LANGUAGES.len() - 1)];
    let keyboard = match &m.variant {
        Some(v) => format!("{} ({v})", m.layout),
        None => m.layout.clone(),
    };
    let parts_on = m.choices.slots.iter().filter(|s| **s != NONE).count();
    let apps = match m.choices.app_count() {
        0 => "no apps".to_owned(),
        1 => "1 app".to_owned(),
        n => format!("{n} apps"),
    };
    let grid = parts::stat_grid(
        vec![
            ("Language", format!("{} ({keyboard})", lang.english)),
            ("User", m.username.clone()),
            (
                "Desktop",
                format!("{} / {}", profile_name(m), m.choices.mode.id()),
            ),
            ("Parts", format!("{parts_on} parts, {apps}")),
        ],
        4,
    );

    // --- the arming field
    let matches = m.confirmed();
    let field = text_input(&disk.by_id, &m.confirm)
        .id(ids::CONFIRM)
        .on_input(Message::Confirm)
        .on_submit(Message::Key(Key::Enter))
        .font(font::DATA)
        .size(size::BODY)
        .padding(0)
        .style(theme::prompt_input);
    let reading = if matches {
        parts::mono("matches", ok_ink())
    } else {
        parts::mono("type the name", color::TEXT_TERTIARY)
    };
    let confirm = column![
        row![
            micro_label("Type the disk name"),
            iced::widget::Space::new().width(Length::Fill),
            parts::mono(disk.by_id.clone(), color::TEXT_SECONDARY),
        ],
        prompt_band("type", field, Some(reading)),
    ]
    .spacing(space::CHIP_GAP);

    let mut blocks: Vec<El<'_, Message>> = vec![banner, grid];
    if m.wifi_to_carry() {
        let mark = if m.carry_network { "[x]" } else { "[ ]" };
        let content = row![
            parts::mono(mark, color::TEXT_SECONDARY),
            parts::strong("Carry Wi-Fi connections to the installed system"),
        ]
        .spacing(space::CHIP_GAP)
        .align_y(iced::Alignment::Center);
        blocks.push(parts::pick(
            content,
            false,
            false,
            Some(Message::CarryNetwork(!m.carry_network)),
        ));
    }
    blocks.push(confirm.into());
    Body {
        title,
        lead: String::new(),
        blocks,
        scroll: true,
    }
}
