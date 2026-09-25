// SPDX-License-Identifier: AGPL-3.0-only
//! Step 4: disk.
//!
//! Hero: the disks themselves, as cards each carrying a proportional bar of what
//! is on it. That is the spatial half of the answer; the list of partitions
//! under the chosen disk is the exact half, and the "not yet" strip is the
//! honest edge of what this version can do (D-07 §4.2: the whole disk, and only
//! the whole disk).
//! The accented value: the chosen disk's card. Nothing is preselected: erasing
//! a disk is a choice.

use super::Body;
use crate::data::format_size;
use crate::model::{Load, Message, Model};
use crate::parts::{self, El, Ink, Tone};
use eclipse_setup_plan::Disk;
use eclipse_ui::theme;
use eclipse_ui::tokens::{color, font, size, space};
use eclipse_ui::widget::{hairline, inset, list_row, micro_label, value};
use iced::widget::{column, container, row, text, Column, Row, Space};
use iced::{Alignment, Length};

/// Partitions listed for the chosen disk before "and N more".
const SHOWN: usize = 6;

pub fn body(m: &Model) -> Body<'_> {
    let subtitle =
        "The whole disk is used and everything on it is erased. You confirm on the last step.".to_owned();

    let disks = match &m.disks {
        Load::Idle | Load::Loading => {
            return Body {
                subtitle,
                chip: ("reading".to_owned(), "install helper".to_owned()),
                blocks: vec![parts::banner(
                    Tone::Quiet,
                    "disks",
                    "Reading the disks",
                    "Asking the install helper what is attached.",
                    vec![],
                )],
                scroll: false,
            };
        }
        Load::Failed(e) => {
            return Body {
                subtitle,
                chip: ("no list".to_owned(), "install helper".to_owned()),
                blocks: vec![
                    parts::banner(Tone::Danger, "disks", "Could not list the disks", e, vec![]),
                    row![parts::verb("try again", Message::ReloadDisks)].into(),
                ],
                scroll: false,
            };
        }
        Load::Ready(v) => v,
    };

    if disks.is_empty() {
        return Body {
            subtitle,
            chip: ("no disks".to_owned(), "nothing to install onto".to_owned()),
            blocks: vec![
                parts::banner(
                    Tone::Danger,
                    "disks",
                    "No disks found",
                    "Nothing here can be installed onto. Attach a disk and look again.",
                    vec![],
                ),
                row![parts::verb("look again", Message::ReloadDisks)].into(),
            ],
            scroll: false,
        };
    }

    let mut blocks: Vec<El<'_, Message>> = Vec::new();
    let mut cards = Column::new().spacing(space::CARD);
    for d in disks {
        cards = cards.push(disk_card(d, m.disk.as_deref() == Some(d.by_id.as_str())));
    }
    blocks.push(cards.into());

    if let Some(d) = m.selected_disk() {
        blocks.push(partitions(d));
    }

    let not_yet = Row::with_children(
        [
            "alongside another system",
            "keep existing partitions",
            "custom layout",
            "encryption",
            "swap",
        ]
        .into_iter()
        .map(|what| El::from(parts::tag(what))),
    )
    .spacing(space::PILL_GAP)
    .align_y(Alignment::Center)
    .width(Length::Fill)
    .wrap()
    .vertical_spacing(space::PILL_GAP);
    blocks.push(
        inset(
            row![micro_label("Not yet"), not_yet]
                .spacing(space::CARD)
                .align_y(Alignment::Center),
        )
        .padding(space::CARD)
        .into(),
    );

    let chip = match m.selected_disk() {
        Some(d) => (d.model.clone(), format_size(d.size_bytes)),
        None => ("no disk chosen".to_owned(), format!("{} available", disks.len())),
    };
    Body {
        subtitle,
        chip,
        blocks,
        scroll: true,
    }
}

fn disk_card(d: &Disk, selected: bool) -> El<'_, Message> {
    let sizes: Vec<u64> = d.partitions.iter().map(|p| p.size_bytes).collect();
    let summary = if d.partitions.is_empty() {
        "empty".to_owned()
    } else {
        let mut fs: Vec<&str> = d
            .partitions
            .iter()
            .map(|p| p.fs.as_str())
            .filter(|f| !f.is_empty())
            .collect();
        fs.dedup();
        format!(
            "{} partition{}  {}",
            d.partitions.len(),
            if d.partitions.len() == 1 { "" } else { "s" },
            fs.join("  ")
        )
    };
    let content = column![
        row![
            text(d.model.clone())
                .font(font::UI_SEMIBOLD)
                .size(size::PROMPT)
                .style(theme::text_primary),
            Space::new().width(Length::Fill),
            text(format_size(d.size_bytes))
                .font(font::DATA_MEDIUM)
                .size(size::PROMPT)
                .style(theme::text_primary),
        ]
        .align_y(Alignment::Center),
        parts::mono(d.by_id.clone(), color::TEXT_TERTIARY),
        parts::disk_bar(&sizes, d.size_bytes, Ink::Neutral),
        parts::mono(summary, color::TEXT_SECONDARY),
    ]
    .spacing(space::CHIP_GAP + 2.0);
    parts::card(
        content,
        Length::Shrink,
        selected,
        Some(Message::SelectDisk(d.by_id.clone())),
    )
}

fn partitions(d: &Disk) -> El<'_, Message> {
    let mut rows = Column::new()
        .push(container(micro_label(&format!("On {} now", d.model))).padding([space::ROW_Y, space::CARD]));
    if d.partitions.is_empty() {
        rows = rows.push(hairline()).push(
            container(parts::body("Nothing. The disk has no partitions."))
                .padding([space::ROW_Y, space::CARD]),
        );
    }
    for p in d.partitions.iter().take(SHOWN) {
        let name = match (p.fs.is_empty(), p.label.is_empty()) {
            (true, true) => "unknown".to_owned(),
            (false, true) => p.fs.clone(),
            (true, false) => p.label.clone(),
            (false, false) => format!("{}  {}", p.fs, p.label),
        };
        rows = rows
            .push(hairline())
            .push(container(list_row(&name, value(&format_size(p.size_bytes)))).padding([0.0, space::CARD]));
    }
    if d.partitions.len() > SHOWN {
        rows = rows.push(hairline()).push(
            container(parts::mono(
                format!("and {} more", d.partitions.len() - SHOWN),
                color::TEXT_TERTIARY,
            ))
            .padding([space::ROW_Y, space::CARD]),
        );
    }
    inset(rows).into()
}
