// SPDX-License-Identifier: AGPL-3.0-only
//! Step 14: install.
//!
//! Hero: a live meter, the percentage set large over a bar that fills across the
//! column. Under it the stage ladder, every stage the helper will run with a
//! mark for done, current and waiting, which is what turns "47%" into something
//! you can believe. A failure swaps the meter for the danger band and says
//! whether the disk had already been changed (D-07 §8).
//! The accented value: the percentage while it runs, and the Restart button once
//! it is done. Never both at once.

use super::Body;
use crate::metrics;
use crate::model::{stage_rank, touched_disk, Message, Model, Phase, STAGE_ORDER};
use crate::parts::{self, El, Tone};
use eclipse_setup_plan::Stage;
use eclipse_ui::theme;
use eclipse_ui::tokens::{color, font, radius, size, space};
use eclipse_ui::widget::{big_value, inset, micro_label, panel, quad};
use iced::widget::{column, container, row, text, Column, Space};
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

    let (subtitle, chip) = match ins.phase {
        Phase::Failed => (
            "The install did not finish.".to_owned(),
            ("failed".to_owned(), at.clone()),
        ),
        Phase::Done => (
            "Remove the installation medium, then restart.".to_owned(),
            (
                "installed".to_owned(),
                if m.fake {
                    "dry run".to_owned()
                } else {
                    "ready to restart".to_owned()
                },
            ),
        ),
        _ => (
            "Leave the machine on and plugged in.".to_owned(),
            (format!("{}%", ins.pct), at.clone()),
        ),
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
            "Installed",
            if m.fake {
                "Dry run: the whole flow ran, and nothing was written to any disk."
            } else {
                "EclipseOS is on the disk. Take out the installation medium so the machine starts from it."
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
                        column![
                            micro_label("Installing"),
                            big_value(&ins.pct.to_string(), "%", true)
                        ]
                        .spacing(space::CHIP_GAP),
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

    // --- the ladder
    let here = stage.map_or(0, stage_rank);
    let mut ladder = Column::new();
    for s in STAGE_ORDER {
        let rank = stage_rank(s);
        let failed_here = ins.phase == Phase::Failed && ins.failed_at == Some(s);
        let (mark, ink) = if failed_here {
            (color::DANGER, color::DANGER)
        } else if ins.phase == Phase::Done || rank < here {
            (color::TEXT_SECONDARY, color::TEXT_SECONDARY)
        } else if rank == here && ins.phase == Phase::Running {
            (color::TEXT, color::TEXT)
        } else {
            (color::TRACK, color::TEXT_TERTIARY)
        };
        let word = if failed_here {
            "failed"
        } else if ins.phase == Phase::Done || rank < here {
            "done"
        } else if rank == here && ins.phase == Phase::Running {
            "now"
        } else {
            "waiting"
        };
        ladder = ladder.push(
            container(
                row![
                    quad(
                        Length::Fixed(metrics::STAGE_MARK),
                        Length::Fixed(metrics::STAGE_MARK),
                        mark,
                        radius::BAR
                    ),
                    text(stage_label(s)).font(font::UI).size(size::BODY).color(ink),
                    Space::new().width(Length::Fill),
                    parts::mono(word, ink),
                ]
                .spacing(space::CARD)
                .align_y(Alignment::Center),
            )
            .height(Length::Fixed(metrics::STAGE_H))
            .align_y(Alignment::Center)
            .padding([0.0, space::CARD]),
        );
    }

    let mut blocks: Vec<El<'_, Message>> = vec![hero, inset(ladder).padding([space::CHIP_GAP, 0.0]).into()];
    if ins.phase == Phase::Running && m.fake {
        blocks.push(parts::mono("dry run: nothing is written", color::TEXT_TERTIARY));
    }
    if let Some(e) = &ins.restart_error {
        blocks.push(parts::problem(e));
    }

    Body {
        subtitle,
        chip,
        blocks,
        scroll: true,
    }
}
