// SPDX-License-Identifier: AGPL-3.0-only
//! The launcher's one panel: a filter field, a list, and a line for refusals.
//!
//! Every colour and size comes from `eclipse_ui`; a literal anywhere in here
//! is a bug, with the exception of the layout metrics named at the top of the
//! file, which the surface height is derived from and which therefore cannot
//! live in a styling token.

use iced::widget::{container, mouse_area, row, text, text_input, Column, Space};
use iced::{Alignment, Color, Element, Length, Theme};

use eclipse_services::apps::Entry;
use eclipse_ui::tokens::{color, font, size, space};
use eclipse_ui::widget as parts;

use crate::launcher::app::{App, Message, INPUT_ID};
use crate::launcher::{MAX_ROWS, WIDTH};

/// Padding between the panel and the edge of its surface.
const OUTER: f32 = 10.0;
/// Height of the filter field, its own padding included.
const INPUT_H: f32 = 44.0;
/// Height of one result row.
const ROW_H: f32 = 38.0;
/// The hairline drawn above every row but the first.
const HAIR: f32 = 1.0;
/// The last line of the panel, where a refusal and the match count go. It is
/// reserved whether or not there is anything to say: a panel that grows a line
/// when a launch fails moves every row under the pointer that just clicked.
const PROBLEM_H: f32 = 30.0;

/// The launcher's surface height, in whole pixels.
///
/// Constant for the life of the launcher. A layer surface fixes its size
/// before the boot fn runs, so a height that depended on how many entries
/// matched would need a `SizeChange` round trip on every keystroke.
pub fn surface_height() -> u32 {
    (OUTER * 2.0
        + space::CARD * 2.0
        + INPUT_H
        + space::BLOCK
        + ROW_H * MAX_ROWS as f32
        + HAIR * (MAX_ROWS - 1) as f32
        + space::BLOCK
        + PROBLEM_H)
        .ceil() as u32
}

pub fn view(app: &App) -> Element<'_, Message, Theme> {
    let body = Column::new()
        .spacing(space::BLOCK)
        .push(field(app))
        .push(results(app))
        .push(footer(app));

    container(parts::panel(body))
        .width(Length::Fixed(WIDTH as f32))
        .padding(OUTER)
        .into()
}

fn field(app: &App) -> Element<'_, Message, Theme> {
    container(
        text_input("Type to search applications", &app.query)
            .id(INPUT_ID)
            .on_input(Message::Query)
            // iced's `text_input` captures Enter itself, so the launcher's
            // Enter is wired here rather than in the key subscription.
            .on_submit(Message::Activate)
            .size(size::BODY)
            .font(font::UI)
            .padding(space::CARD)
            .style(eclipse_ui::theme::eclipse_input),
    )
    .height(Length::Fixed(INPUT_H))
    .into()
}

/// The visible slice of the match list, always `MAX_ROWS` tall.
fn results(app: &App) -> Element<'_, Message, Theme> {
    let mut col = Column::new();
    for slot in 0..MAX_ROWS {
        if slot > 0 {
            col = col.push(parts::hairline());
        }
        col = match app.matched.get(slot) {
            Some(&index) => col.push(entry_row(&app.entries[index], slot, slot == app.selected)),
            // An empty slot rather than a shorter list: the panel is a fixed
            // height, and rows that slide up as you type are rows you misclick.
            None => col.push(
                container(Space::new())
                    .height(Length::Fixed(ROW_H))
                    .width(Length::Fill),
            ),
        };
    }
    parts::inset(col).into()
}

fn entry_row(entry: &Entry, slot: usize, selected: bool) -> Element<'static, Message, Theme> {
    // A terminal-only entry is listed and greyed: `apps::launch` will refuse
    // it, and a row that is simply missing teaches the human nothing.
    let name_tint = if selected {
        color::ACCENT_TEXT
    } else if entry.terminal {
        color::TEXT_TERTIARY
    } else {
        color::TEXT
    };
    let name = text(entry.name.clone())
        .size(size::BODY)
        .font(font::UI)
        .color(name_tint);
    let note = if entry.terminal {
        "needs a terminal".to_owned()
    } else {
        entry.comment.clone().unwrap_or_default()
    };
    let note = text(note).size(size::MONO).font(font::DATA).color(if selected {
        color::ACCENT_TEXT
    } else {
        color::TEXT_SECONDARY
    });

    let body = container(row![name, Space::new().width(Length::Fill), note].align_y(Alignment::Center))
        .height(Length::Fixed(ROW_H))
        .width(Length::Fill)
        .padding([0, space::CARD as u16])
        .align_y(Alignment::Center);

    let body = if selected {
        body.style(|_: &Theme| container::Style {
            background: Some(color::ACCENT_FILL.into()),
            ..container::Style::default()
        })
    } else {
        body
    };

    // The terminal row keeps its mouse area on purpose: clicking it must put
    // the refusal on the footer rather than being silently inert.
    mouse_area(body)
        .on_enter(Message::Select(slot))
        .on_press(Message::Activate)
        .into()
}

/// The refusal, or how much of the list is off the bottom.
fn footer(app: &App) -> Element<'_, Message, Theme> {
    let body: Element<'_, Message, Theme> = match app.problem.as_ref() {
        // Verbatim, and in the warning colour: the human asked for something
        // and did not get it.
        Some(problem) => text(problem.clone())
            .size(size::BODY_SMALL)
            .font(font::DATA)
            .color(color::DANGER)
            .into(),
        None if app.matched.len() > MAX_ROWS => {
            text(format!("{} more — keep typing", app.matched.len() - MAX_ROWS))
                .size(size::BODY_SMALL)
                .font(font::DATA)
                .color(color::TEXT_TERTIARY)
                .into()
        }
        None => Space::new().into(),
    };
    container(body)
        .height(Length::Fixed(PROBLEM_H))
        .padding([0, space::CARD as u16])
        .align_y(Alignment::Center)
        .into()
}

/// The panel draws itself; the surface behind it is nothing at all, so it
/// floats over whatever is on screen rather than sitting on a sheet.
pub fn style(_app: &App, theme: &Theme) -> iced::theme::Style {
    iced::theme::Style {
        background_color: Color::TRANSPARENT,
        text_color: theme.palette().text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The surface has to be tall enough for every row it draws, or the last
    /// one is clipped off the bottom of the screen.
    #[test]
    fn the_surface_holds_every_row() {
        assert!(surface_height() as f32 >= INPUT_H + ROW_H * MAX_ROWS as f32 + PROBLEM_H);
    }
}
