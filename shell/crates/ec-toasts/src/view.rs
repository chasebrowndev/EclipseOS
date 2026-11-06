// SPDX-License-Identifier: AGPL-3.0-only
//! One card per notification, stacked down the corner.
//!
//! Every colour and size comes from `ec_ui`; a literal anywhere in here is
//! a bug, with the exception of the layout metrics named at the top of the file,
//! which the surface height is derived from and which therefore cannot live in
//! a styling token.
//!
//! The stack is a column of units. A unit is one card, or the cards of one
//! app: folded they are the newest card with the rest peeking out beneath it
//! as thin glass strips, opened (pointer over the column, or a click) they are
//! a plain run of cards, newest first.

use iced::widget::{
    button, container, image, mouse_area, rich_text, span, stack, svg, text, Column, Row, Space,
};
use iced::{Alignment, Color, ContentFit, Element, Length, Padding, Theme};

use ec_services::notifications::Notification;
use ec_ui::theme::CellTone;
use ec_ui::tokens::{bar, color, font, radius, size, space};

use crate::app::{App, Message, Plan, Style, Toast, Unit};
use crate::art::Art;
use crate::pane::pane;
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
/// Lines the body shows, and how many characters of body text a line holds at
/// the card's width (a little under what fits, so the ellipsis lands inside the
/// slot rather than past it).
const BODY_LINES: usize = 2;
const BODY_CHARS: usize = 42;
/// Air between the body and the app-name line under it.
const BODY_GAP: f32 = 4.0;
/// Height of the app-name line.
const SOURCE_H: f32 = 14.0;
/// Height of the action-button row, when there is one.
const ACTIONS_H: f32 = 36.0;
/// The button itself, set to the bottom of its row so there is air between the
/// app line above and the button.
const ACTION_BTN_H: f32 = 26.0;
/// The picture's square, and the air between it and the text.
const AVATAR: f32 = 32.0;
const AVATAR_GAP: f32 = 12.0;
/// A progress card's extra row: the bar and the air above it.
const PROGRESS_ROW_H: f32 = bar::GAP * 2.0 + ec_ui::tokens::console::BAR_H;
/// A folded group's strips: how tall each is, and how much narrower than the
/// one above on each side.
const PEEK_H: f32 = 6.0;
const PEEK_INSET: f32 = 14.0;
/// Most strips a folded group shows, however many it holds.
const PEEKS: usize = 2;
/// The row that says how many notifications wait past the visible cards.
const MORE_H: f32 = 24.0;
/// The close button's square.
const CLOSE: f32 = 20.0;
/// How much of its ink and ground a low-urgency card keeps.
const LOW: f32 = 0.7;

/// How tall one card is, including its own padding.
fn card_height(notification: &Notification) -> f32 {
    let mut h = space::CARD * 2.0 + SUMMARY_H + SOURCE_H;
    if !notification.body.is_empty() {
        h += BODY_H + BODY_GAP;
    }
    if notification.progress.is_some() {
        h += PROGRESS_ROW_H;
    }
    if !notification.actions.is_empty() {
        h += ACTIONS_H;
    }
    h
}

fn peeks(unit: &Unit) -> usize {
    if unit.folded() {
        (unit.all.len() - 1).min(PEEKS)
    } else {
        0
    }
}

fn unit_height(toasts: &[Toast], unit: &Unit) -> f32 {
    let cards: f32 = unit
        .cards
        .iter()
        .map(|&i| card_height(&toasts[i].notification))
        .sum();
    let gaps = GAP * unit.cards.len().saturating_sub(1) as f32;
    cards + gaps + PEEK_H * peeks(unit) as f32
}

/// The surface height for a given stack, in whole pixels.
///
/// An empty stack is zero, meaning no surface at all: a zero-sized layer
/// surface is a protocol error, so `app::update` closes it instead.
pub fn height(toasts: &[Toast], plan: &Plan) -> u32 {
    if plan.units.is_empty() {
        return 0;
    }
    let units: f32 = plan.units.iter().map(|u| unit_height(toasts, u)).sum();
    let mut h = units + GAP * (plan.units.len() - 1) as f32;
    if plan.hidden > 0 {
        h += GAP + MORE_H;
    }
    h.ceil() as u32
}

pub fn view(app: &App, id: iced::window::Id) -> Element<'_, Message, Theme> {
    let plan = app.plan();
    let mut stack = Column::new().spacing(GAP);
    for unit in &plan.units {
        stack = stack.push(unit_view(app, unit));
    }
    if plan.hidden > 0 {
        stack = stack.push(more(plan.hidden));
    }
    ec_ui::glass::root(id, container(stack).width(Length::Fixed(WIDTH as f32))).into()
}

/// One unit: its cards, and for a group the pointer area that opens it.
fn unit_view<'a>(app: &'a App, unit: &Unit) -> Element<'a, Message, Theme> {
    let mut cards = Column::new().spacing(GAP);
    for &i in &unit.cards {
        let count = if unit.folded() { unit.all.len() } else { 1 };
        cards = cards.push(slot(app, &app.toasts[i], count));
    }
    let mut col = Column::new().push(cards);
    for level in 1..=peeks(unit) {
        col = col.push(peek(level, app.blur, &unit.key));
    }
    if unit.all.len() < 2 {
        return col.into();
    }
    let key = unit.key.clone();
    mouse_area(col)
        .on_enter(Message::HoverGroup(key.clone(), true))
        .on_exit(Message::HoverGroup(key, false))
        .into()
}

/// A strip of the glass behind a folded group's face.
fn peek<'a>(level: usize, blur: bool, key: &str) -> Element<'a, Message, Theme> {
    // Each strip is fainter than the one above it, so the stack reads as
    // receding rather than as three equal bars.
    let alpha = LOW.powi(level as i32);
    let strip = container(Space::new())
        .width(Length::Fixed(WIDTH as f32 - 2.0 * PEEK_INSET * level as f32))
        .height(Length::Fixed(PEEK_H))
        .style(move |t: &Theme| {
            // Without a pane the strip is the card's own ground, fainter.
            ec_ui::theme::bare_container_if_glass(faded(ec_ui::theme::surface(radius::CHIP, blur)(t), alpha))
        });
    // A click on the strips pins the group open; dismissal stays on the
    // cross and the swipe.
    mouse_area(
        container(pane(strip, CellTone::Plain, radius::CHIP, alpha))
            .width(Length::Fixed(WIDTH as f32))
            .align_x(Alignment::Center),
    )
    .on_press(Message::Pin(key.to_owned()))
    .interaction(iced::mouse::Interaction::Pointer)
    .into()
}

/// The count of what is waiting. Not a card: it has no body to read and no
/// clock of its own.
fn more<'a>(hidden: usize) -> Element<'a, Message, Theme> {
    let label = container(
        text(format!("+{hidden} more"))
            .size(size::MICRO)
            .font(font::DATA)
            .color(color::text_secondary()),
    )
    .padding([space::BADGE_Y * 2.0, space::CHIP_X])
    .style(|t: &Theme| ec_ui::theme::bare_container_if_glass(ec_ui::theme::inset(t)));
    container(pane(label, CellTone::Plain, radius::PILL, 1.0))
        .width(Length::Fixed(WIDTH as f32))
        .height(Length::Fixed(MORE_H))
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .into()
}

/// A card in its fixed slot: the slot catches the pointer, so a card carried
/// right by a drag is still measured against the place it started from.
fn slot<'a>(app: &'a App, toast: &'a Toast, folded: usize) -> Element<'a, Message, Theme> {
    let n = &toast.notification;
    let pose = toast.pose(app.now, &app.anim);
    let presence = pose.presence.clamp(0.0, 1.0);
    let drag = app.drag.filter(|d| d.id == n.id);
    // How far the card is from home. Carried by a drag, or left by a swipe
    // and still on its way out, else by the entrance and exit: the slide
    // comes from the anchored (right) edge, so the distance is the card's
    // width at nothing and zero at rest. The slot is clipped to the surface.
    let carried = drag.map_or(toast.fling, |d| d.dx);
    let w = WIDTH as f32;
    let (away, alpha) = match pose.style {
        Style::Slide => (carried + (w - carried).max(0.0) * (1.0 - presence), 1.0),
        Style::Fade => (carried, presence),
        Style::None => (carried, 1.0),
    };
    let hovered = app.hover == Some(n.id);
    let face = card(app, toast, folded, alpha, hovered);
    let moved = Row::new()
        .push(Space::new().width(Length::Fixed(away.round())))
        .push(face);
    let slot = container(moved)
        .width(Length::Fixed(w))
        .height(Length::Fixed(card_height(n)))
        .clip(true);
    let mut area = mouse_area(slot)
        .on_press(Message::Press(n.id))
        .on_release(Message::Release(n.id))
        .on_enter(Message::HoverCard(n.id, true))
        .on_exit(Message::HoverCard(n.id, false))
        .interaction(iced::mouse::Interaction::Pointer);
    // Only a card being pressed needs to hear the pointer move.
    if drag.is_some() {
        let id = n.id;
        area = area.on_move(move |p| Message::Pointer(id, p.x));
    }
    area.into()
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

fn card<'a>(
    app: &'a App,
    toast: &'a Toast,
    folded: usize,
    alpha: f32,
    hovered: bool,
) -> Element<'a, Message, Theme> {
    let notification = &toast.notification;
    use ec_services::notifications::Urgency;
    let critical = notification.urgency == Urgency::Critical;
    let low = notification.urgency == Urgency::Low;
    // A low card is the quiet one: its ink and its ground both recede.
    let alpha = if low { alpha * LOW } else { alpha };

    // Critical is the one thing allowed off the neutral palette here, for the
    // same reason a dying battery is: it is a state the human must act on.
    let summary_ink = if critical {
        color::danger()
    } else if low {
        color::text_secondary()
    } else {
        color::text()
    };
    let mut text_col = Column::new().width(Length::Fill);
    text_col = text_col.push(
        container(
            text(notification.summary.clone())
                .size(size::CARD_TITLE)
                .font(if low { font::UI_MEDIUM } else { font::UI_SEMIBOLD })
                .wrapping(text::Wrapping::None)
                .color(summary_ink.scale_alpha(alpha)),
        )
        .height(Length::Fixed(SUMMARY_H))
        .align_y(Alignment::Center),
    );

    if !notification.body.is_empty() {
        let ink = if low {
            color::text_tertiary()
        } else {
            color::text_secondary()
        }
        .scale_alpha(alpha);
        let spans: Vec<_> = ec_ui::markup::clamp(&toast.runs, BODY_LINES, BODY_CHARS)
            .into_iter()
            .map(|r| {
                let mut f = if r.bold { font::UI_SEMIBOLD } else { font::UI };
                if r.italic {
                    f.style = iced::font::Style::Italic;
                }
                span::<(), _>(r.text).font(f).color(ink).underline(r.underline)
            })
            .collect();
        text_col = text_col.push(
            container(rich_text(spans).size(size::BODY_SMALL).width(Length::Fill))
                .height(Length::Fixed(BODY_H))
                // The card's height is computed, so anything longer than its
                // slot is cut off rather than pushing the stack past its surface.
                .clip(true),
        );
        text_col = text_col.push(Space::new().height(Length::Fixed(BODY_GAP)));
    }

    if let Some(percent) = notification.progress {
        let fill = if critical {
            color::danger()
        } else {
            color::accent()
        };
        text_col = text_col.push(
            container(ec_ui::widget::progress_bar(
                f32::from(percent) / 100.0,
                fill.scale_alpha(alpha),
            ))
            .height(Length::Fixed(PROGRESS_ROW_H))
            .align_y(Alignment::Center),
        );
    }

    let source = if folded > 1 {
        format!("{}  ·  {folded}", notification.app_name)
    } else {
        notification.app_name.clone()
    };
    text_col = text_col.push(
        container(
            text(source)
                .size(size::MICRO)
                .font(font::DATA)
                .wrapping(text::Wrapping::None)
                .color(color::text_tertiary().scale_alpha(alpha)),
        )
        .height(Length::Fixed(SOURCE_H))
        .align_y(Alignment::Center),
    );

    if !notification.actions.is_empty() {
        let mut buttons = Row::new().spacing(GAP).align_y(Alignment::Center);
        for action in &notification.actions {
            buttons = buttons.push(action_button(notification.id, &action.key, &action.label, alpha));
        }
        text_col = text_col.push(
            container(buttons)
                .height(Length::Fixed(ACTIONS_H))
                .align_y(Alignment::End),
        );
    }

    let main = Row::new()
        .spacing(AVATAR_GAP)
        .push(avatar(toast, critical, alpha))
        .push(text_col);

    // With glass the compositor draws the card's ground as a pane, at the
    // card's own opacity, so a fade takes the glass with it (GLASS-05).
    // Without it the card paints its ground itself.
    let radius = app.glass_radius;
    let blur = app.blur;
    let ground = container(main)
        .width(Length::Fixed(WIDTH as f32))
        .height(Length::Fixed(card_height(notification)))
        .padding(space::CARD)
        .style(move |t: &Theme| {
            let mut s = faded(ec_ui::theme::surface(radius, blur)(t), alpha);
            // Without a pane the lift on hover and the crisp rim of a
            // critical card are the card's own border.
            if hovered || critical {
                s.border.color = color::border_strong().scale_alpha(alpha);
            }
            ec_ui::theme::bare_container_if_glass(s)
        });
    let mut layers = stack![ground]
        .width(Length::Fixed(WIDTH as f32))
        .height(Length::Fixed(card_height(notification)));
    if critical {
        // The spine: the one place a card spends its danger ink besides its
        // title, parting a critical card from a normal one at a glance.
        layers = layers.push(
            container(ec_ui::widget::quad(
                Length::Fixed(space::BAR_W),
                Length::Fill,
                color::danger().scale_alpha(alpha),
                radius::BAR,
            ))
            .padding(Padding {
                top: space::CARD,
                bottom: space::CARD,
                left: space::BAR_W * 2.0,
                right: 0.0,
            })
            .width(Length::Fill)
            .height(Length::Fill),
        );
    }
    if hovered {
        layers = layers.push(
            container(close_button(notification.id, alpha))
                .padding(space::HEADER_GAP)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Alignment::End)
                .align_y(Alignment::Start),
        );
    }
    let tone = if critical {
        CellTone::Focused
    } else {
        CellTone::Plain
    };
    pane(layers, tone, radius, alpha)
}

/// The card's picture: what it sent, else the app's initial on a glass tile.
fn avatar<'a>(toast: &'a Toast, critical: bool, alpha: f32) -> Element<'a, Message, Theme> {
    let side = Length::Fixed(AVATAR);
    let art: Element<'a, Message, Theme> = match &toast.art {
        Art::Raster(handle) => image(handle.clone())
            .width(side)
            .height(side)
            .content_fit(ContentFit::Cover)
            .border_radius(bar::RADIUS_TILE)
            .opacity(alpha)
            .into(),
        Art::Svg(handle) => svg(handle.clone()).width(side).height(side).opacity(alpha).into(),
        Art::None => {
            let initial = toast
                .notification
                .app_name
                .chars()
                .find(|c| c.is_alphanumeric())
                .map_or_else(|| "·".to_owned(), |c| c.to_uppercase().collect());
            let ink = if critical {
                color::danger()
            } else {
                color::text_secondary()
            };
            let tile = container(
                text(initial)
                    .size(size::BODY)
                    .font(font::DATA_MEDIUM)
                    .color(ink.scale_alpha(alpha)),
            )
            .center(side)
            .style(move |t: &Theme| {
                ec_ui::theme::bare_container_if_glass(faded(ec_ui::theme::inset(t), alpha))
            });
            pane(tile, CellTone::Plain, bar::RADIUS_TILE, alpha)
        }
    };
    // Pinned to the top: a tall card keeps its picture by the summary.
    container(art)
        .height(Length::Shrink)
        .align_y(Alignment::Start)
        .into()
}

/// Dismiss one card. Drawn only while the pointer is on the card.
fn close_button<'a>(id: u32, alpha: f32) -> Element<'a, Message, Theme> {
    let glyph = container(
        text("×")
            .size(size::BODY)
            .font(font::UI_MEDIUM)
            .color(color::text_secondary().scale_alpha(alpha)),
    )
    .center(Length::Fixed(CLOSE));
    let cell = button(glyph)
        .padding(space::ZERO)
        .on_press(Message::Dismiss(id))
        .style(move |t: &Theme, s| {
            ec_ui::theme::bare_if_glass(ec_ui::theme::bar_cell(CellTone::Plain)(t, s))
        });
    pane(cell, CellTone::Plain, radius::PILL, alpha)
}

fn action_button<'a>(id: u32, key: &str, label: &str, alpha: f32) -> Element<'a, Message, Theme> {
    let text = text(label.to_owned())
        .size(size::BODY_SMALL)
        .font(font::UI_MEDIUM)
        .color(color::text().scale_alpha(alpha));
    let label = container(text).center_y(Length::Fill);
    let cell = button(label)
        .padding([0.0, space::CARD])
        .height(Length::Fixed(ACTION_BTN_H))
        .on_press(Message::Invoke(id, key.to_owned()))
        .style(move |t: &Theme, s| {
            ec_ui::theme::bare_if_glass(ec_ui::theme::bar_cell(CellTone::Plain)(t, s))
        });
    pane(cell, CellTone::Plain, radius::INSET, alpha)
}

/// The stack draws itself; the surface behind it is nothing at all, so the
/// cards float over whatever is on screen rather than sitting on a sheet.
pub fn style(_app: &App, theme: &Theme) -> iced::theme::Style {
    iced::theme::Style {
        background_color: Color::TRANSPARENT,
        text_color: theme.palette().text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_services::notifications::{Action, Urgency};

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
            ..Notification::default()
        }
    }

    /// An empty stack asks for no surface, not a pixel of one (BLUR-02).
    #[test]
    fn an_empty_stack_asks_for_no_surface() {
        assert_eq!(height(&[], &Plan::default()), 0);
    }

    /// The card grows for what it actually holds — a notification with no body
    /// and no buttons must not leave two empty bands of glass on screen.
    #[test]
    fn a_bare_notification_is_shorter_than_a_full_one() {
        assert!(card_height(&notification("", 0)) < card_height(&notification("Body", 2)));
    }

    #[test]
    fn a_progress_bar_adds_its_row() {
        let mut n = notification("", 0);
        let plain = card_height(&n);
        n.progress = Some(40);
        assert_eq!(card_height(&n), plain + PROGRESS_ROW_H);
    }

    fn unit(all: Vec<usize>, cards: Vec<usize>, expanded: bool) -> Unit {
        Unit {
            key: "mail".into(),
            all,
            cards,
            expanded,
        }
    }

    /// A folded group is its newest card plus its strips; opened it is a run
    /// of cards; waiting notifications add one row of their own.
    #[test]
    fn the_surface_height_counts_strips_cards_and_the_overflow_row() {
        let toasts: Vec<Toast> = (0..3)
            .map(|i| {
                let mut n = notification("", 0);
                n.id = i;
                Toast::new(n)
            })
            .collect();
        let one = card_height(&toasts[0].notification);
        let folded = Plan {
            units: vec![unit(vec![0, 1, 2], vec![2], false)],
            hidden: 0,
        };
        assert_eq!(height(&toasts, &folded), (one + 2.0 * PEEK_H) as u32);
        let open = Plan {
            units: vec![unit(vec![0, 1, 2], vec![2, 1, 0], true)],
            hidden: 0,
        };
        assert_eq!(height(&toasts, &open), (3.0 * one + 2.0 * GAP) as u32);
        let waiting = Plan { hidden: 2, ..open };
        assert_eq!(height(&toasts, &waiting), (3.0 * one + 3.0 * GAP + MORE_H) as u32);
    }
}
