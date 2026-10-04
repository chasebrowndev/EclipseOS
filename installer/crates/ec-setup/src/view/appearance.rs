// SPDX-License-Identifier: AGPL-3.0-only
//! Step 11: appearance (`decoration.rounding`, `decoration.blur.mode`,
//! `animations.preset`, `bar.position`).
//!
//! Hero: a small desktop that redraws as you choose, a bar strip and two
//! tiled windows, so rounding and the bar's edge are seen rather than read.
//! Under it one inset list of the four choices, each a row of white chips —
//! a pair, except blur, whose four modes are still one decision.
//! Scale, theme and pointer options are not here: `docs/CONFIG.md` has no
//! global scale or theme key (scale is per output), and the input keys are not
//! on the seed allowlist (D-07 §5), so offering them would be a choice that is
//! dropped at Apply.
//! The accented value: the bar's strip in the preview.

use super::Body;
use crate::choices::{BarPosition, Blur};
use crate::model::{Message, Model};
use crate::parts::{self, El};
use ec_ui::tokens::space;
use ec_ui::widget::{inset_list, list_row};
use iced::widget::Row;

/// Two chips, the first for `true`.
fn pair<'a>(on: bool, yes: &str, no: &str, set: fn(bool) -> Message) -> El<'a, Message> {
    Row::new()
        .spacing(space::PILL_GAP)
        .push(parts::tab(yes, on, set(true)))
        .push(parts::tab(no, !on, set(false)))
        .into()
}

pub fn body(m: &Model) -> Body<'_> {
    let c = &m.choices;
    let bottom = c.bar_position == BarPosition::Bottom;
    let position: El<'_, Message> = Row::new()
        .spacing(space::PILL_GAP)
        .push(parts::tab(
            "Top",
            !bottom,
            Message::SetBarPosition(BarPosition::Top),
        ))
        .push(parts::tab(
            "Bottom",
            bottom,
            Message::SetBarPosition(BarPosition::Bottom),
        ))
        .into();
    let blur: El<'_, Message> = Blur::ALL
        .iter()
        .fold(Row::new().spacing(space::PILL_GAP), |r, &b| {
            r.push(parts::tab(b.label(), c.blur == b, Message::SetBlur(b)))
        })
        .into();
    let list = inset_list(
        "Look",
        vec![
            list_row(
                "Rounded corners",
                pair(c.rounded, "On", "Off", Message::SetRounded),
            ),
            list_row("Translucency", blur),
            list_row(
                "Window animations",
                pair(c.animations, "On", "Off", Message::SetAnimations),
            ),
            list_row("Bar position", position),
        ],
    );
    Body {
        title: "How should it look?".to_owned(),
        lead: "The picture shows your choices as you make them.".to_owned(),
        blocks: vec![parts::desk(c.rounded, bottom), list],
        scroll: true,
    }
}
