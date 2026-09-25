// SPDX-License-Identifier: AGPL-3.0-only
//! Step 13: review.
//!
//! Hero: the danger band. The whole step is one fact, that a disk is about to be
//! erased, and the band says it before anything else. Then the plan as two
//! bars, what is on the disk now (in the status red, because it is going) over
//! what will be there after, each with its partitions listed exactly. Then every
//! other choice in a grid, and last the field that arms the button.
//! The accented value: the armed Apply button in the footer. The band is the
//! status red, the "matches" reading is the OK green. Apply stays disabled
//! until the disk's own name has been typed again (D-07 §4.2).

use super::{ok_ink, Body};
use crate::data::{format_size, LANGUAGES};
use crate::model::{ids, Key, Message, Model};
use crate::parts::{self, El, Ink, Tone};
use eclipse_ui::theme;
use eclipse_ui::tokens::{color, font, radius, size, space};
use eclipse_ui::widget::{hairline, list_row, micro_label, panel, prompt_band, value};
use iced::widget::{column, container, row, text_input, Column};
use iced::Length;

/// The EFI system partition D-07 §4.2 lays down.
const ESP_BYTES: u64 = 1 << 30;

pub fn body(m: &Model) -> Body<'_> {
    let Some(disk) = m.selected_disk() else {
        return Body {
            subtitle: "No disk is chosen. Go back and choose one.".to_owned(),
            chip: ("no disk".to_owned(), "nothing to apply".to_owned()),
            blocks: vec![parts::banner(
                Tone::Danger,
                "review",
                "No disk chosen",
                "There is nothing to install onto.",
                vec![],
            )],
            scroll: false,
        };
    };

    let banner = parts::banner(
        Tone::Danger,
        "erase",
        &format!("Everything on {} will be erased", disk.model),
        "Every partition and every file on this disk is destroyed and cannot be recovered. Nothing else is touched.",
        vec![
            ("whole disk".to_owned(), format_size(disk.size_bytes)),
            (format!("{} now", disk.partitions.len()), "partitions".to_owned()),
        ],
    );

    // --- the plan: before and after
    let now_sizes: Vec<u64> = disk.partitions.iter().map(|p| p.size_bytes).collect();
    let root_bytes = disk.size_bytes.saturating_sub(ESP_BYTES);
    let after_sizes = [ESP_BYTES, root_bytes];

    let mut now = Column::new()
        .spacing(space::CHIP_GAP)
        .push(micro_label("On the disk now, erased"));
    now = now.push(parts::disk_bar(&now_sizes, disk.size_bytes, Ink::Erase));
    let mut now_rows = Column::new();
    if disk.partitions.is_empty() {
        now_rows = now_rows.push(
            container(parts::body("Nothing. The disk has no partitions.")).padding([space::ROW_Y, 0.0]),
        );
    }
    for p in &disk.partitions {
        now_rows = now_rows
            .push(hairline())
            .push(list_row(&partition_name(p), value(&format_size(p.size_bytes))));
    }
    let now = now.push(now_rows);

    let after = column![
        micro_label("On the disk after"),
        parts::disk_bar(&after_sizes, disk.size_bytes, Ink::Neutral),
        column![
            hairline(),
            list_row("vfat  EFI system", value(&format_size(ESP_BYTES))),
            hairline(),
            list_row("ext4  root", value(&format_size(root_bytes))),
        ],
    ]
    .spacing(space::CHIP_GAP);

    let plan = panel(
        radius::CARD,
        row![
            now.width(Length::FillPortion(1)),
            after.width(Length::FillPortion(1))
        ]
        .spacing(space::BLOCK),
    );

    // --- everything else
    let lang = &LANGUAGES[m.language.min(LANGUAGES.len() - 1)];
    let keyboard = match &m.variant {
        Some(v) => format!("{} ({v})", m.layout),
        None => m.layout.clone(),
    };
    let cells = vec![
        ("Language", format!("{}, {}", lang.native, lang.english)),
        ("Keyboard", format!("{keyboard}, console {}", m.keymap)),
        ("Time zone", m.zone.clone()),
        (
            "Network",
            if m.online() {
                "connected".to_owned()
            } else {
                "not connected".to_owned()
            },
        ),
        ("Host name", m.hostname.clone()),
        ("User", m.username.clone()),
        (
            "Password",
            if m.password.is_empty() {
                "not set".to_owned()
            } else {
                "set".to_owned()
            },
        ),
        ("Profile", "Standard".to_owned()),
    ];
    let grid = parts::stat_grid(cells, 4);

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
            micro_label("Type the disk's name to arm the button"),
            iced::widget::Space::new().width(Length::Fill),
            parts::mono(disk.by_id.clone(), color::TEXT_SECONDARY),
        ],
        prompt_band("type", field, Some(reading)),
    ]
    .spacing(space::CHIP_GAP);

    let chip = if matches {
        ("armed".to_owned(), format!("erase {}", disk.model))
    } else {
        ("not confirmed".to_owned(), "type the disk name".to_owned())
    };
    let mut blocks: Vec<El<'_, Message>> = vec![banner, plan.into(), grid];
    if m.wifi_to_carry() {
        let mark = if m.carry_network { "[x]" } else { "[ ]" };
        let content = row![
            parts::mono(mark, color::TEXT_SECONDARY),
            parts::strong("Carry Wi-Fi connections to the installed system"),
            iced::widget::Space::new().width(Length::Fill),
            parts::mono("up / down", color::TEXT_TERTIARY),
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
        subtitle: "Read it through. Nothing is written until you press the button.".to_owned(),
        chip,
        blocks,
        scroll: true,
    }
}

fn partition_name(p: &eclipse_setup_plan::Partition) -> String {
    match (p.fs.is_empty(), p.label.is_empty()) {
        (true, true) => "unknown".to_owned(),
        (false, true) => p.fs.clone(),
        (true, false) => p.label.clone(),
        (false, false) => format!("{}  {}", p.fs, p.label),
    }
}
