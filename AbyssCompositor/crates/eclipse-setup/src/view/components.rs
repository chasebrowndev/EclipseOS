// SPDX-License-Identifier: AGPL-3.0-only
//! Step 8: desktop components (D-07 §4.1).
//!
//! Hero: a 2x2 status grid, one cell per slot (bar, launcher, notifications,
//! control center) reading what is chosen. On the happy path that is all there
//! is: the profile already picked all four. Swapping a slot for another
//! candidate is under "More options", one row of choices per slot.
//! The accented value: none. Every reading is settled, and the choosers under
//! More options mark their selection in white.

use super::Body;
use crate::choices::{BAR, NONE, SLOTS};
use crate::model::{Message, Model};
use crate::parts::{self, El};
use eclipse_ui::tokens::{color, space};
use eclipse_ui::widget::inset;
use iced::widget::{column, Row};
use iced::Length;

/// The chosen candidate as a person reads it.
fn shown(id: &str) -> String {
    if id == NONE {
        "None".to_owned()
    } else {
        id.to_owned()
    }
}

pub fn body(m: &Model) -> Body<'_> {
    let cells = SLOTS
        .iter()
        .enumerate()
        .map(|(i, s)| (s.label, shown(m.choices.slots[i])))
        .collect();
    let mut blocks: Vec<El<'_, Message>> = vec![parts::stat_grid(cells, 2)];

    // A non-native bar is a real choice with a real cost (D-07 §4.1).
    if matches!(m.choices.slots[BAR], "waybar" | "quickshell") {
        blocks.push(parts::mono(
            "Hyperion's drawers, tray lanes and Settings links do not exist in this bar.",
            color::TEXT_SECONDARY,
        ));
    }

    if m.more {
        blocks.push(choosers(m));
        blocks.push(parts::link("Fewer options", Message::More(false)));
    } else {
        blocks.push(parts::link("More options", Message::More(true)));
    }

    Body {
        title: "Your desktop parts".to_owned(),
        lead: "These come with your profile. Change one only if you have a reason to.".to_owned(),
        blocks,
        scroll: true,
    }
}

/// One row of candidates per slot.
fn choosers(m: &Model) -> El<'_, Message> {
    let mut col = iced::widget::Column::new().spacing(space::BLOCK);
    for (i, slot) in SLOTS.iter().enumerate() {
        let tabs = Row::with_children(slot.choices.iter().map(|id| {
            El::from(parts::tab(
                &shown(id),
                m.choices.slots[i] == *id,
                Message::SetSlot(i, id),
            ))
        }))
        .spacing(space::PILL_GAP)
        .wrap()
        .vertical_spacing(space::PILL_GAP);
        col = col.push(column![parts::body(slot.label), tabs].spacing(space::CHIP_GAP));
    }
    inset(col).padding(space::CARD).width(Length::Fill).into()
}
