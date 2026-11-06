// SPDX-License-Identifier: AGPL-3.0-only
//! The cheat sheet's one panel.
//!
//! Three blocks: a header whose yellow phrase is the number of chords the
//! human changed from the stock table, a status grid of the six groups (the
//! hero: how the binds are distributed, and how many are the human's own),
//! and one scrolling inset list of every chord under its group heading.
//!
//! The accent ledger: the single live yellow is the changed-chord count in the
//! subtitle. Nothing else on the sheet is accented.

use iced::widget::{container, scrollable, Column};
use iced::{Element, Length, Theme};

use ec_ui::theme;
use ec_ui::tokens::space;
use ec_ui::widget as parts;

use crate::app::{App, Message};
use crate::model::Group;
use crate::{HEIGHT, WIDTH};

pub fn view(app: &App) -> Element<'_, Message, Theme> {
    let body = Column::new()
        .spacing(space::BLOCK)
        .height(Length::Fill)
        .push(header(app))
        .push(hero(app))
        .push(list(app));

    // The sheet is the whole surface; its air is layer-shell margin, so the
    // compositor's blur mask and this corner coincide.
    ec_ui::glass::root_sole(
        container(parts::surface(app.glass_radius, app.blur, body))
            .width(Length::Fixed(WIDTH as f32))
            .height(Length::Fixed(HEIGHT as f32)),
    )
    .into()
}

/// The surface paints no ground of its own: the compositor's glass is behind
/// the sheet, and the sheet's fill sits on it.
pub fn style(_app: &App, theme: &Theme) -> iced::theme::Style {
    iced::theme::Style {
        background_color: iced::Color::TRANSPARENT,
        text_color: theme.palette().text,
    }
}

/// Title, the one accented phrase, and the status chip: the chord count in
/// the config's mode, and how to close.
fn header(app: &App) -> Element<'_, Message, Theme> {
    let subtitle = parts::subtitle(
        "Live from your config, ",
        &format!("{} changed", app.changed),
        " from the stock table. Escape closes this sheet.",
    );
    let chip = parts::status_chip(
        &format!("{} chords live", app.entries.len()),
        &format!("mode {}", app.mode),
    );
    parts::header("Keybinds", subtitle, vec![chip])
}

/// One cell per group: the group, how many chords it holds, and how many of
/// those are the human's own.
fn hero(app: &App) -> Element<'_, Message, Theme> {
    let cells = Group::ALL
        .iter()
        .map(|&group| {
            let of_group = app.entries.iter().filter(|e| e.group == group);
            let total = of_group.clone().count();
            let mine = of_group.filter(|e| e.custom).count();
            let measure = if mine == 0 {
                "stock".to_owned()
            } else {
                format!("{mine} yours")
            };
            parts::status_cell(group.label(), &format!("{total} chords"), &measure, false)
        })
        .collect();
    parts::status_grid(cells, 3)
}

/// Every chord, under its group heading, in one scrolling inset.
fn list(app: &App) -> Element<'_, Message, Theme> {
    let mut rows = Column::new();
    for group in Group::ALL {
        let mut section = app.entries.iter().filter(|e| e.group == group).peekable();
        if section.peek().is_none() {
            continue;
        }
        rows = rows.push(parts::hairline());
        rows = rows.push(container(parts::micro_label(group.label())).padding([space::ROW_Y, space::CARD]));
        for entry in section {
            rows = rows.push(parts::hairline());
            rows = rows.push(parts::chord_row(
                &entry.label,
                &entry.keys,
                if entry.custom { "yours" } else { "" },
                Message::Ignore,
            ));
        }
    }
    let scroll = scrollable(rows)
        .style(theme::eclipse_scrollable)
        .height(Length::Fill);
    container(parts::glass_inset(scroll)).height(Length::Fill).into()
}
