// SPDX-License-Identifier: AGPL-3.0-only
//! The control center's one panel.
//!
//! Two blocks: what the session is doing, and how to end it. Every colour and
//! size comes from `eclipse_ui`; a literal anywhere in here is a bug, with the
//! exception of the layout metrics named at the top of the file, which the
//! surface height is derived from and which therefore cannot live in a styling
//! token.

use iced::widget::{container, mouse_area, row, text, Column, Space};
use iced::{Alignment, Color, Element, Length, Theme};

use eclipse_services::session::{Action, Availability};
use eclipse_ui::tokens::{color, font, size, space};
use eclipse_ui::widget as parts;

use crate::center::app::{App, Message, ACTIONS};
use crate::center::WIDTH;

/// Padding between the panel and the edge of its surface.
const OUTER: f32 = 10.0;
/// Height of one row, status or action.
const ROW_H: f32 = 36.0;
/// Height of a block's heading row.
const HEAD_H: f32 = 34.0;
/// The hairline drawn above every row, the heading included.
const HAIR: f32 = 1.0;
/// The last line of the panel, where logind's refusal goes. It is reserved
/// whether or not there is anything to say: a panel that grows a line when an
/// action fails moves every row under the pointer that just clicked.
const PROBLEM_H: f32 = 34.0;

/// Rows in the status block: network, bluetooth, battery. Always three, even
/// on a machine with no battery — see [`battery_text`].
const STATUS_ROWS: usize = 3;

/// One block: a heading, then a hairline above each of its rows.
fn block_h(rows: usize) -> f32 {
    HEAD_H + (HAIR + ROW_H) * rows as f32
}

/// The panel's surface height, in whole pixels.
///
/// Constant for the life of the panel: every row it can draw is drawn, and a
/// refused action is greyed out rather than removed. A layer surface fixes its
/// size before the boot fn runs, so a height that depended on logind's answers
/// would need a `SizeChange` round trip on every open.
pub fn surface_height() -> u32 {
    (OUTER * 2.0
        + space::CARD * 2.0
        + block_h(STATUS_ROWS)
        + space::BLOCK
        + block_h(ACTIONS.len())
        + space::BLOCK
        + PROBLEM_H)
        .ceil() as u32
}

pub fn view(app: &App) -> Element<'_, Message, Theme> {
    let body = Column::new()
        .spacing(space::BLOCK)
        .push(status(app))
        .push(session(app))
        .push(problem(app));

    container(parts::panel(body))
        .width(Length::Fixed(WIDTH as f32))
        .padding(OUTER)
        .into()
}

/// The same three readings the bar draws, in words rather than in a cell.
fn status(app: &App) -> Element<'_, Message, Theme> {
    let battery_low = app.battery.is_some_and(|b| {
        b.percent <= crate::view::LOW && b.state == eclipse_services::status::Charge::Discharging
    });
    let rows = vec![
        reading("Network", crate::view::network_text(&app.network), false),
        reading(
            "Bluetooth",
            // The bar hides a powered-down adapter because a bar cell is
            // scarce. Here there is room for the answer, and "off" is an
            // answer the human opened this panel to get.
            crate::view::bluetooth_text(app.bluetooth).unwrap_or_else(|| "off".to_owned()),
            false,
        ),
        reading("Battery", battery_text(app), battery_low),
    ];
    block("system", rows)
}

/// `None` is a machine with no battery, not a flat one, and the panel says so
/// rather than drawing a percentage it does not have.
fn battery_text(app: &App) -> String {
    app.battery
        .map_or_else(|| "none".to_owned(), crate::view::battery_text)
}

fn reading(label: &str, value: String, warn: bool) -> Element<'static, Message, Theme> {
    // Low and discharging is the one reading allowed off the neutral palette,
    // for the same reason it is on the bar: it is a state the human must act
    // on before the machine acts for them.
    let tint = if warn {
        color::DANGER
    } else {
        color::TEXT_SECONDARY
    };
    cell(
        text(label.to_owned())
            .size(size::BODY)
            .font(font::UI)
            .color(color::TEXT),
        text(value).size(size::MONO).font(font::DATA).color(tint),
    )
}

/// Every action logind knows about, whether or not it will do it.
fn session(app: &App) -> Element<'_, Message, Theme> {
    let mut rows = Vec::new();
    for action in ACTIONS {
        let availability = app
            .offered
            .iter()
            .find(|(a, _)| *a == action)
            // No entry means we never got an answer — no system bus at all.
            // That is not an offer.
            .map_or(Availability::No, |(_, availability)| *availability);
        rows.push(action_row(action, availability));
    }
    block("session", rows)
}

fn action_row(action: Action, availability: Availability) -> Element<'static, Message, Theme> {
    let offered = availability.offered();
    let label = text(label(action))
        .size(size::BODY)
        .font(font::UI)
        .color(if offered {
            color::TEXT
        } else {
            color::TEXT_TERTIARY
        });
    let note = text(note(availability))
        .size(size::MONO)
        .font(font::DATA)
        .color(color::TEXT_TERTIARY);
    let row = cell(label, note);
    if offered {
        mouse_area(row).on_press(Message::Perform(action)).into()
    } else {
        // No `mouse_area` at all rather than a click that does nothing: the
        // row is inert to the pointer as well as to the eye.
        row
    }
}

fn label(action: Action) -> &'static str {
    match action {
        Action::Lock => "Lock",
        Action::LogOut => "Log out",
        Action::Suspend => "Suspend",
        Action::Hibernate => "Hibernate",
        Action::Reboot => "Restart",
        Action::PowerOff => "Power off",
    }
}

/// A `Challenge` says so on the row. A polkit prompt that appears out of
/// nowhere reads as a machine that has been compromised, not as a machine
/// doing its job.
fn note(availability: Availability) -> &'static str {
    match availability {
        Availability::Yes => "",
        Availability::Challenge => "asks first",
        Availability::No => "unavailable",
    }
}

/// One row: a label at the left, a mono value at the right, a fixed height the
/// surface size is computed from.
fn cell<'a>(
    label: impl Into<Element<'a, Message, Theme>>,
    value: impl Into<Element<'a, Message, Theme>>,
) -> Element<'a, Message, Theme> {
    container(row![label.into(), Space::new().width(Length::Fill), value.into()].align_y(Alignment::Center))
        .height(Length::Fixed(ROW_H))
        .padding([0, space::CARD as u16])
        .align_y(Alignment::Center)
        .into()
}

fn block<'a>(heading: &str, rows: Vec<Element<'a, Message, Theme>>) -> Element<'a, Message, Theme> {
    let mut col = Column::new().push(
        container(parts::micro_label(heading))
            .height(Length::Fixed(HEAD_H))
            .padding([0, space::CARD as u16])
            .align_y(Alignment::Center),
    );
    for r in rows {
        col = col.push(parts::hairline());
        col = col.push(r);
    }
    parts::inset(col).into()
}

/// logind's refusal, and the only way out that does nothing.
fn problem(app: &App) -> Element<'_, Message, Theme> {
    let body: Element<'_, Message, Theme> = match app.problem.as_ref() {
        // Shown verbatim, and in the warning colour for the same reason a
        // dying battery is: the human asked for something and did not get it.
        Some(problem) => text(problem.clone())
            .size(size::BODY_SMALL)
            .font(font::DATA)
            .color(color::DANGER)
            .into(),
        None => Space::new().into(),
    };
    container(
        row![
            body,
            Space::new().width(Length::Fill),
            mouse_area(
                container(
                    text("Close")
                        .size(size::BODY_SMALL)
                        .font(font::UI_MEDIUM)
                        .color(color::TEXT_SECONDARY)
                )
                .padding([0, space::CARD as u16])
                .height(Length::Fill)
                .align_y(Alignment::Center)
            )
            .on_press(Message::Close),
        ]
        .align_y(Alignment::Center),
    )
    .height(Length::Fixed(PROBLEM_H))
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
        let rows = (STATUS_ROWS + ACTIONS.len()) as f32;
        assert!(surface_height() as f32 >= rows * ROW_H);
    }

    /// A refusal must read as one. An empty note on an unavailable action is
    /// how a dead button gets shipped.
    #[test]
    fn an_unavailable_action_says_so() {
        assert_eq!(note(Availability::No), "unavailable");
        assert_eq!(note(Availability::Challenge), "asks first");
        assert_eq!(note(Availability::Yes), "");
    }
}
