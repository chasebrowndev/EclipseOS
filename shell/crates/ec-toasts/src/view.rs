// SPDX-License-Identifier: AGPL-3.0-only
//! One card per notification, stacked newest-last down the corner.
//!
//! Every colour and size comes from `ec_ui`; a literal anywhere in here is
//! a bug, with the exception of the layout metrics named at the top of the file,
//! which the surface height is derived from and which therefore cannot live in
//! a styling token.

use iced::widget::{container, mouse_area, text, Column, Row, Space};
use iced::{Alignment, Color, Element, Length, Theme};

use ec_services::notifications::{Notification, Urgency};
use ec_ui::tokens::{color, font, size, space};

use crate::app::{Message, Style, Toast};
use crate::WIDTH;

/// Space between cards. There is none around the stack: the first card's
/// top and right edges are the surface's, and the air to the screen edge is
/// layer-shell margin (`app::TOP_MARGIN`), because the compositor blurs the
/// whole surface and a clear band inside it would show as a blurred rim.
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
/// An empty stack is zero, meaning no surface at all: a zero-sized layer
/// surface is a protocol error, so `app::update` closes it instead.
pub fn height(drawn: &[Toast]) -> u32 {
    if drawn.is_empty() {
        return 0;
    }
    let cards: f32 = drawn.iter().map(|t| card_height(&t.notification)).sum();
    let gaps = GAP * (drawn.len() - 1) as f32;
    (cards + gaps).ceil() as u32
}

pub fn view(app: &crate::app::App, _id: iced::window::Id) -> Element<'_, Message, Theme> {
    let mut stack = Column::new().spacing(GAP);
    for toast in app.drawn() {
        let pose = toast.pose(app.now, &app.anim);
        let presence = pose.presence.clamp(0.0, 1.0);
        // Slide enters from the anchored (right) edge, so the card's
        // distance from home is its width at nothing and zero at rest. The
        // slot is clipped to the surface: a card out of its home is cut by
        // the surface edge, not drawn past it. A bounce curve's overshoot
        // past home is clamped away — the surface has no room beyond it.
        let (away, alpha) = match pose.style {
            Style::Slide => ((1.0 - presence) * WIDTH as f32, 1.0),
            Style::Fade => (0.0, presence),
            Style::None => (0.0, 1.0),
        };
        let face = card(&toast.notification, app.glass_radius, app.blur, alpha);
        stack = stack.push(if away > 0.5 {
            Element::from(
                container(
                    Row::new()
                        .push(Space::new().width(Length::Fixed(away.round())))
                        .push(face),
                )
                .width(Length::Fixed(WIDTH as f32))
                .clip(true),
            )
        } else {
            face
        });
    }
    container(stack).width(Length::Fixed(WIDTH as f32)).into()
}

/// `style` with every colour at `alpha` of its own: iced has no layer
/// opacity, so a fading card scales its ground, rim and shadow itself.
fn faded(mut style: container::Style, alpha: f32) -> container::Style {
    style.background = style.background.map(|b| match b {
        iced::Background::Color(c) => iced::Background::Color(c.scale_alpha(alpha)),
        other => other,
    });
    style.border.color = style.border.color.scale_alpha(alpha);
    style.shadow.color = style.shadow.color.scale_alpha(alpha);
    style
}

fn card(notification: &Notification, radius: f32, blur: bool, alpha: f32) -> Element<'_, Message, Theme> {
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
                .color(summary_tint.scale_alpha(alpha)),
        )
        .height(Length::Fixed(SUMMARY_H))
        .align_y(Alignment::Center),
    );

    if !notification.body.is_empty() {
        body = body.push(
            container(
                text(notification.body.clone())
                    .size(size::BODY_SMALL)
                    .color(color::TEXT_SECONDARY.scale_alpha(alpha)),
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
                .color(color::TEXT_TERTIARY.scale_alpha(alpha)),
        )
        .height(Length::Fixed(SOURCE_H))
        .align_y(Alignment::Center),
    );

    if !notification.actions.is_empty() {
        let mut buttons = Row::new().spacing(GAP).align_y(Alignment::Center);
        for action in &notification.actions {
            buttons = buttons.push(button(notification.id, &action.key, &action.label, alpha));
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
            .width(Length::Fixed(WIDTH as f32))
            .height(Length::Fixed(card_height(notification)))
            .padding(space::CARD)
            .style(move |t: &Theme| faded(ec_ui::theme::surface(radius, blur)(t), alpha)),
    )
    .on_press(Message::Dismiss(notification.id))
    .into()
}

fn button<'a>(id: u32, key: &str, label: &str, alpha: f32) -> Element<'a, Message, Theme> {
    let cell = container(
        text(label.to_owned())
            .size(size::BODY_SMALL)
            .font(font::UI_MEDIUM)
            .color(color::TEXT.scale_alpha(alpha)),
    )
    .padding([0, space::CARD as u16])
    .height(Length::Fill)
    .align_y(Alignment::Center)
    .style(move |t: &Theme| faded(ec_ui::theme::inset(t), alpha));
    mouse_area(cell)
        .on_press(Message::Invoke(id, key.to_owned()))
        .into()
}

/// The stack draws itself; the surface behind it is nothing at all, so the
/// cards float over whatever is on screen rather than sitting on a sheet.
pub fn style(_app: &crate::app::App, theme: &Theme) -> iced::theme::Style {
    iced::theme::Style {
        background_color: Color::TRANSPARENT,
        text_color: theme.palette().text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_services::notifications::Action;

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

    /// An empty stack asks for no surface, not a pixel of one (BLUR-02).
    #[test]
    fn an_empty_stack_asks_for_no_surface() {
        assert_eq!(height(&[]), 0);
    }

    /// The card grows for what it actually holds — a notification with no body
    /// and no buttons must not leave two empty bands of glass on screen.
    #[test]
    fn a_bare_notification_is_shorter_than_a_full_one() {
        assert!(card_height(&notification("", 0)) < card_height(&notification("Body", 2)));
    }
}
