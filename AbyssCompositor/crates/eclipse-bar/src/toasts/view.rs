// SPDX-License-Identifier: AGPL-3.0-only
//! One card per notification, stacked newest-last down the corner.
//!
//! Every colour and size comes from `eclipse_ui`; a literal anywhere in here is
//! a bug, with the exception of the layout metrics named at the top of the file,
//! which the surface height is derived from and which therefore cannot live in
//! a styling token.

use iced::widget::{container, mouse_area, text, Column, Row};
use iced::{Alignment, Color, Element, Length, Theme};

use eclipse_services::notifications::{Notification, Urgency};
use eclipse_ui::tokens::{color, font, size, space};

use crate::toasts::app::{Message, Toast};
use crate::toasts::WIDTH;

/// Space between cards, and around the stack.
const GAP: f32 = 10.0;
/// Height of the summary line's row.
const SUMMARY_H: f32 = 20.0;
/// Height of the two body lines. A longer body is clipped rather than allowed
/// to grow the card: the surface height is computed, not measured, and a card
/// that outgrew its slot would be drawn over its neighbour.
const BODY_H: f32 = 32.0;
/// Height of the app-name line.
const SOURCE_H: f32 = 14.0;
/// Height of the action-button row, when there is one.
const ACTIONS_H: f32 = 28.0;

/// How tall one card is, including its own padding.
fn card_height(notification: &Notification) -> f32 {
    let mut h = space::CARD * 2.0 + SUMMARY_H + SOURCE_H;
    if !notification.body.is_empty() {
        h += BODY_H;
    }
    if !notification.actions.is_empty() {
        h += ACTIONS_H;
    }
    h
}

/// The surface height for a given stack, in whole pixels.
///
/// An empty stack asks for one pixel rather than zero: a zero-sized layer
/// surface is a protocol error, and `events_transparent` is not an option here
/// because the action buttons need the pointer.
pub fn height(drawn: &[Toast]) -> u32 {
    if drawn.is_empty() {
        return 1;
    }
    let cards: f32 = drawn.iter().map(|t| card_height(&t.notification)).sum();
    let gaps = GAP * (drawn.len() + 1) as f32;
    (cards + gaps).ceil() as u32
}

pub fn view(app: &crate::toasts::app::App) -> Element<'_, Message, Theme> {
    let mut stack = Column::new().spacing(GAP).padding(GAP);
    for toast in app.drawn() {
        stack = stack.push(card(&toast.notification));
    }
    container(stack).width(Length::Fixed(WIDTH as f32)).into()
}

fn card(notification: &Notification) -> Element<'_, Message, Theme> {
    let mut body = Column::new();

    // Critical is the one thing allowed off the neutral palette here, for the
    // same reason a dying battery is: it is a state the human must act on.
    let summary_tint = if notification.urgency == Urgency::Critical {
        color::DANGER
    } else {
        color::TEXT
    };
    body = body.push(
        container(
            text(notification.summary.clone())
                .size(size::CARD_TITLE)
                .font(font::UI_SEMIBOLD)
                .color(summary_tint),
        )
        .height(Length::Fixed(SUMMARY_H))
        .align_y(Alignment::Center),
    );

    if !notification.body.is_empty() {
        body = body.push(
            container(
                text(notification.body.clone())
                    .size(size::BODY_SMALL)
                    .color(color::TEXT_SECONDARY),
            )
            .height(Length::Fixed(BODY_H))
            // The card's height is computed, so anything longer than its slot
            // is cut off rather than pushing the stack past its surface.
            .clip(true),
        );
    }

    body = body.push(
        container(
            text(notification.app_name.clone())
                .size(size::MICRO)
                .font(font::DATA)
                .color(color::TEXT_TERTIARY),
        )
        .height(Length::Fixed(SOURCE_H))
        .align_y(Alignment::Center),
    );

    if !notification.actions.is_empty() {
        let mut buttons = Row::new().spacing(GAP).align_y(Alignment::Center);
        for action in &notification.actions {
            buttons = buttons.push(button(notification.id, &action.key, &action.label));
        }
        body = body.push(
            container(buttons)
                .height(Length::Fixed(ACTIONS_H))
                .align_y(Alignment::Center),
        );
    }

    // Clicking anywhere that is not a button dismisses it — the same gesture
    // the rest of the desktop uses, and the only one available without a
    // close glyph to click.
    mouse_area(
        container(body)
            .width(Length::Fill)
            .height(Length::Fixed(card_height(notification)))
            .padding(space::CARD)
            .style(eclipse_ui::theme::panel),
    )
    .on_press(Message::Dismiss(notification.id))
    .into()
}

fn button<'a>(id: u32, key: &str, label: &str) -> Element<'a, Message, Theme> {
    let cell = container(
        text(label.to_owned())
            .size(size::BODY_SMALL)
            .font(font::UI_MEDIUM)
            .color(color::TEXT),
    )
    .padding([0, space::CARD as u16])
    .height(Length::Fill)
    .align_y(Alignment::Center)
    .style(eclipse_ui::theme::inset);
    mouse_area(cell)
        .on_press(Message::Invoke(id, key.to_owned()))
        .into()
}

/// The stack draws itself; the surface behind it is nothing at all, so the
/// cards float over whatever is on screen rather than sitting on a sheet.
pub fn style(_app: &crate::toasts::app::App, theme: &Theme) -> iced::theme::Style {
    iced::theme::Style {
        background_color: Color::TRANSPARENT,
        text_color: theme.palette().text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eclipse_services::notifications::Action;

    fn notification(body: &str, actions: usize) -> Notification {
        Notification {
            id: 1,
            app_name: "mail".into(),
            summary: "Summary".into(),
            body: body.into(),
            icon: String::new(),
            urgency: Urgency::Normal,
            actions: (0..actions)
                .map(|i| Action {
                    key: format!("k{i}"),
                    label: format!("L{i}"),
                })
                .collect(),
            expires_in: None,
            transient: false,
        }
    }

    /// A zero-height layer surface is a protocol error, so an empty stack still
    /// asks for a pixel.
    #[test]
    fn an_empty_stack_still_asks_for_a_pixel() {
        assert_eq!(height(&[]), 1);
    }

    /// The card grows for what it actually holds — a notification with no body
    /// and no buttons must not leave two empty bands of glass on screen.
    #[test]
    fn a_bare_notification_is_shorter_than_a_full_one() {
        assert!(card_height(&notification("", 0)) < card_height(&notification("Body", 2)));
    }
}
