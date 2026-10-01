// SPDX-License-Identifier: AGPL-3.0-only
//! Step 14: install.
//!
//! Hero: a live meter, the percentage set large over a bar that fills across the
//! column, and the name of what the installer is doing now. A failure swaps the
//! meter for the danger band and says whether the disk had already been changed
//! (D-07 §8).
//! The accented value: the percentage while it runs, and the Restart button once
//! it is done. Never both at once.

use super::Body;
use crate::model::{touched_disk, Message, Model, Phase};
use crate::parts::{self, El, Tone};
use ec_setup_plan::Stage;
use ec_ui::theme;
use ec_ui::tokens::{color, font, radius, size, space};
use ec_ui::widget::{big_value, panel};
use iced::widget::{column, row, text, Space};
use iced::{Alignment, Length};

pub fn stage_label(s: Stage) -> &'static str {
    match s {
        Stage::Validate => "Checking the request",
        Stage::Preflight => "Checking the machine",
        Stage::Confirm => "Confirming on the trusted screen",
        Stage::Partition => "Partitioning the disk",
        Stage::Pacstrap => "Installing the system",
        Stage::Configure => "Configuring the system",
        Stage::Bootloader => "Installing the boot loader",
        Stage::User => "Creating your account",
        Stage::Seed => "Seeding the desktop",
        Stage::Units => "Enabling services",
        Stage::Done => "Finished",
    }
}

pub fn body(m: &Model) -> Body<'_> {
    let ins = &m.install;
    let stage = ins.stage;
    let at = stage.map_or("starting", stage_label).to_owned();

    let title = match ins.phase {
        Phase::Failed => "The install stopped",
        Phase::Done => "Installed",
        _ => "Installing",
    }
    .to_owned();
    let lead = match ins.phase {
        Phase::Failed => String::new(),
        Phase::Done => "Remove the installation medium, then restart.".to_owned(),
        _ => "Leave the machine on and plugged in.".to_owned(),
    };

    let hero: El<'_, Message> = match ins.phase {
        Phase::Failed => {
            let failed = ins.failed_at.or(stage).unwrap_or(Stage::Validate);
            let sentence = if touched_disk(failed) {
                "The disk had already been changed, so it may not start. Try again, or restart and reinstall."
            } else {
                "Nothing on the disk was changed."
            };
            let note = if ins.note.is_empty() {
                at.clone()
            } else {
                ins.note.clone()
            };
            parts::banner(
                Tone::Danger,
                "stopped",
                &format!("Stopped: {}", stage_label(failed).to_lowercase()),
                sentence,
                vec![("failed".to_owned(), note)],
            )
        }
        Phase::Done => parts::banner(
            Tone::Quiet,
            "done",
            "EclipseOS is installed",
            if m.fake {
                "Dry run: the whole flow ran, and nothing was written to any disk."
            } else {
                "Take out the installation medium so the machine starts from the disk."
            },
            vec![],
        ),
        _ => {
            let pct = f32::from(ins.pct);
            let caption = if stage == Some(Stage::Confirm) {
                "Confirm on the trusted screen. Nothing has been written to the disk yet.".to_owned()
            } else if ins.note.is_empty() {
                at.clone()
            } else {
                ins.note.clone()
            };
            panel(
                radius::CARD,
                column![
                    row![
                        big_value(&ins.pct.to_string(), "%", true),
                        Space::new().width(Length::Fill),
                        column![
                            text(at.clone())
                                .font(font::UI_SEMIBOLD)
                                .size(size::PROMPT)
                                .style(theme::text_primary),
                            text(caption)
                                .font(font::UI)
                                .size(size::BODY)
                                .style(theme::text_secondary),
                        ]
                        .spacing(space::CHIP_GAP)
                        .align_x(Alignment::End),
                    ]
                    .align_y(Alignment::End),
                    parts::meter_fill(pct / 100.0, color::ACCENT),
                ]
                .spacing(space::BLOCK),
            )
            .into()
        }
    };

    let mut blocks: Vec<El<'_, Message>> = vec![hero];
    if ins.phase == Phase::Running && m.fake {
        blocks.push(parts::mono("dry run: nothing is written", color::TEXT_TERTIARY));
    }
    if let Some(e) = &ins.restart_error {
        blocks.push(parts::problem(e));
    }

    Body {
        title,
        lead,
        blocks,
        scroll: true,
    }
}
