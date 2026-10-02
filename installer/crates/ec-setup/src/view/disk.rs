// SPDX-License-Identifier: AGPL-3.0-only
//! Step 4: disk.
//!
//! Hero: the disks themselves, as cards each carrying a proportional bar of what
//! is on it. What is on the chosen disk in exact terms, and what this version
//! cannot do (D-07 §4.2: the whole disk, and only the whole disk), are under
//! "More options". With no disk to list the step says so in plain words, says
//! what to check and offers Rescan: it is never a dead end without a reason.
//! The accented value: the chosen disk's card. Nothing is preselected: erasing
//! a disk is a choice.

use super::Body;
use crate::data::format_size;
use crate::model::{Load, Message, Model};
use crate::parts::{self, El, Ink};
use ec_setup_plan::Disk;
use ec_ui::theme;
use ec_ui::tokens::{color, font, size, space};
use ec_ui::widget::{hairline, inset, list_row, micro_label, value};
use iced::widget::{column, container, row, text, Column, Row, Space};
use iced::{Alignment, Length};

/// Partitions listed for the chosen disk before "and N more".
const SHOWN: usize = 6;

/// The message when nothing installable is listed.
pub const NO_DISK: &str = "No installable disk found.";
/// What to check, in the words the step uses.
pub const NO_DISK_HELP: &str = "Check that a disk is attached and powered. In a virtual machine, give the disk a serial number: a disk without one has no stable name and cannot be listed. Then scan again.";

pub fn body(m: &Model) -> Body<'_> {
    let title = "Choose a disk".to_owned();
    let lead = "Everything on it is erased. You confirm on the last step.".to_owned();

    let disks = match &m.disks {
        Load::Idle | Load::Loading => {
            return Body {
                title,
                lead: "Looking for disks.".to_owned(),
                blocks: vec![],
                scroll: false,
            };
        }
        Load::Failed(e) => {
            return Body {
                title,
                lead: String::new(),
                blocks: vec![
                    parts::strong("Could not list the disks."),
                    parts::body(e.clone()),
                    row![parts::primary("Try again", Some(Message::ReloadDisks))].into(),
                ],
                scroll: false,
            };
        }
        Load::Ready(v) => v,
    };

    if disks.is_empty() {
        return Body {
            title,
            lead: String::new(),
            blocks: vec![
                parts::lead(NO_DISK),
                parts::body(NO_DISK_HELP),
                row![parts::primary("Rescan", Some(Message::ReloadDisks))].into(),
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

    if m.more {
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
        blocks.push(parts::link("Fewer options", Message::More(false)));
    } else {
        blocks.push(parts::link("More options", Message::More(true)));
    }

    Body {
        title,
        lead,
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
