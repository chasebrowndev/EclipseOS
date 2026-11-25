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
use iced::{Alignment, Color, ContentFit, Element, Length, Padding, Size, Theme, Vector};

use ec_services::notifications::{Class, Notification, Silence};
use ec_ui::theme::CellTone;
use ec_ui::tokens::{bar, color, drawer, font, radius, size, space};
use ec_ui::widget::art::{Glyph, Hue};

use crate::app::{App, Message, Plan, Style, Toast, Unit};
use crate::art::Art;
use crate::bell::bell_off;
use crate::pane::{frame, pane, pane_linked, reveal, shifted, Link};
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
/// Height of a system card's source line, and of an app card's, which also
/// carries Open and Silence.
const SOURCE_H: f32 = 24.0;
const FOOT_H: f32 = 30.0;
/// Open and Silence: quiet cells, shorter than a declared action.
const FOOT_BTN_H: f32 = 22.0;
/// The silence drawer: as wide as its three options need, each option a row,
/// and the fillet where it joins the card.
const DRAWER_W: f32 = 104.0;
const OPT_H: f32 = 26.0;
const ERR_H: f32 = 20.0;
const JOIN_R: f32 = 8.0;
/// Right edge of the silence button, in from the card's edge.
const BUTTON_INSET: f32 = space::CARD;
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
/// Text column width of a full-width card, which `BODY_CHARS` was set for.
const TEXT_W_FULL: f32 = WIDTH as f32 - 2.0 * space::CARD - AVATAR - AVATAR_GAP;

/// How tall one card is, including its own padding.
fn card_height(notification: &Notification) -> f32 {
    let mut h = space::CARD * 2.0 + SUMMARY_H;
    if !notification.body.is_empty() {
        h += BODY_H + BODY_GAP;
    }
    if notification.progress.is_some() {
        h += PROGRESS_ROW_H;
    }
    if !notification.actions.is_empty() {
        h += ACTIONS_H;
    }
    h + foot_h(notification)
}

/// The source line: an app's carries Open and Silence, a system card's is a
/// label alone, because a system source is not the human's to silence.
fn foot_h(n: &Notification) -> f32 {
    if n.class == Class::App {
        FOOT_H
    } else {
        SOURCE_H
    }
}

/// The silence drawer's full height: three options and, after a failed save,
/// a line saying so.
pub fn drawer_height(error: bool) -> f32 {
    2.0 * drawer::PAD + 3.0 * OPT_H + if error { ERR_H } else { 0.0 }
}

fn peeks(unit: &Unit) -> usize {
    if unit.folded() {
        (unit.all.len() - 1).min(PEEKS)
    } else {
        0
    }
}

/// Where the stack's cards sit, and how much room the open silence drawer
/// needs past the stack's own ends.
///
/// The drawer is drawn over the stack, not in it, so opening it moves nothing.
/// It hangs below its card in a top corner and stands above it in a bottom
/// one, so the surface is given the room it needs: `lead` above the stack in a
/// bottom corner, `tail` below it in a top one.
pub struct Geometry {
    pub lead: f32,
    pub tail: f32,
    pub stack: f32,
    /// The drawer's card: its top within the stack, and its height.
    pub card: Option<(f32, f32)>,
    /// Whether the drawer would sit over another card of the stack.
    pub overlaps: bool,
}

pub fn geometry(toasts: &[Toast], plan: &Plan, bottom: bool, drawer: Option<(u32, bool)>) -> Geometry {
    // Top-down: the height of each piece and the gap between it and the next.
    let (mut hs, mut ids, mut gs): (Vec<f32>, Vec<Option<u32>>, Vec<f32>) = (vec![], vec![], vec![]);
    for (ui, unit) in plan.units.iter().enumerate() {
        if ui > 0 {
            gs.push(GAP);
        }
        for (ci, &i) in unit.cards.iter().enumerate() {
            if ci > 0 {
                gs.push(GAP);
            }
            hs.push(card_height(&toasts[i].notification));
            ids.push(Some(toasts[i].notification.id));
        }
        for _ in 0..peeks(unit) {
            gs.push(0.0);
            hs.push(PEEK_H);
            ids.push(None);
        }
    }
    if plan.hidden > 0 {
        gs.push(GAP);
        hs.push(MORE_H);
        ids.push(None);
    }
    if bottom {
        hs.reverse();
        ids.reverse();
        gs.reverse();
    }
    let stack = hs.iter().sum::<f32>() + gs.iter().sum::<f32>();
    let mut y = 0.0;
    let mut card = None;
    for (k, h) in hs.iter().enumerate() {
        if drawer.is_some_and(|d| ids[k] == Some(d.0)) {
            card = Some((y, *h));
        }
        y += h + gs.get(k).copied().unwrap_or(0.0);
    }
    let need = drawer.map_or(0.0, |d| drawer_height(d.1));
    let (lead, tail) = match card {
        Some((top, h)) if bottom => ((need - top).max(0.0), 0.0 * h),
        Some((top, h)) => (0.0, (top + h + need - stack).max(0.0)),
        None => (0.0, 0.0),
    };
    // Over another card when anything of the stack lies on the drawer's side.
    let overlaps = card.is_some_and(|(top, h)| if bottom { top > 0.5 } else { top + h < stack - 0.5 });
    Geometry {
        lead,
        tail,
        stack,
        card,
        overlaps,
    }
}

/// The surface height for a given stack, in whole pixels.
///
/// An empty stack is zero, meaning no surface at all: a zero-sized layer
/// surface is a protocol error, so `app::update` closes it instead.
pub fn height(toasts: &[Toast], plan: &Plan, bottom: bool, drawer: Option<(u32, bool)>) -> u32 {
    if plan.units.is_empty() {
        return 0;
    }
    let g = geometry(toasts, plan, bottom, drawer);
    (g.lead + g.stack + g.tail).ceil() as u32
}

pub fn view(app: &App, id: iced::window::Id) -> Element<'_, Message, Theme> {
    let plan = app.plan();
    // Built top-down, then turned over for a bottom corner: the stack grows
    // away from the edge it is anchored to, so the first card is nearest it.
    let mut parts: Vec<Element<'_, Message, Theme>> = plan.units.iter().map(|u| unit_view(app, u)).collect();
    if plan.hidden > 0 {
        parts.push(more(app, plan.hidden));
    }
    if app.corner.bottom() {
        parts.reverse();
    }
    let g = geometry(&app.toasts, &plan, app.corner.bottom(), app.drawer_extent());
    let w = app.width as f32;
    let col = container(Column::with_children(parts).spacing(GAP))
        .width(Length::Fixed(w))
        .padding(Padding {
            top: g.lead,
            ..Padding::ZERO
        });
    let inner: Element<'_, Message, Theme> = match silence_drawer(app, &g) {
        Some(drawer) => stack![col.height(Length::Fixed(g.lead + g.stack + g.tail)), drawer]
            .width(Length::Fixed(w))
            .into(),
        None => col.into(),
    };
    ec_ui::glass::root(id, frame(inner)).into()
}

/// The silence drawer, drawn over the stack beside its card: a small panel of
/// three options that grows out of the silence button, joined to the card as
/// one piece of glass when the compositor joins panes. It is laid out at its
/// full size and shown through a window that opens, so its rows are never
/// squeezed on the way.
fn silence_drawer<'a>(app: &'a App, g: &Geometry) -> Option<Element<'a, Message, Theme>> {
    let id = app.drawer_card?;
    let (top, card_h) = g.card?;
    app.toasts.iter().find(|t| t.notification.id == id)?;
    let t = app.drawer_t().clamp(0.0, 1.0);
    let error = app.drawer_error.is_some();
    let full = Size::new(DRAWER_W, drawer_height(error));
    let outer = Size::new((full.width * t).round(), (full.height * t).round());
    if outer.width < 1.0 || outer.height < 1.0 {
        return None;
    }
    let bottom = app.corner.bottom();
    let card_top = g.lead + top;
    let x = app.width as f32 - BUTTON_INSET - outer.width;
    let y = if bottom {
        card_top - outer.height
    } else {
        card_top + card_h
    };
    // The window opens from the button's side: the panel's right edge stays
    // under the button, and its top (bottom corners: bottom) edge stays on the
    // card.
    let offset = Vector::new(
        outer.width - full.width,
        if bottom { outer.height - full.height } else { 0.0 },
    );
    let (blur, r) = (app.blur, app.card_radius());
    let mut options = Column::new()
        .push(option("1 hr", Message::Silence(id, Silence::Hour)))
        .push(option("24 hrs", Message::Silence(id, Silence::Day)))
        .push(option("Forever", Message::Silence(id, Silence::Forever)));
    if error {
        options = options.push(
            container(
                text("Not saved")
                    .size(size::MICRO)
                    .font(font::DATA)
                    .color(color::danger()),
            )
            .height(Length::Fixed(ERR_H))
            .align_y(Alignment::Center)
            .padding([0.0, space::HEADER_GAP]),
        );
    }
    let overlaps = g.overlaps;
    let ground = container(options)
        .width(Length::Fixed(full.width))
        .height(Length::Fixed(full.height))
        .padding(drawer::PAD)
        .style(move |th: &Theme| {
            if overlaps {
                // Over a neighbour the compositor's glass would sit under that
                // card's text, so the panel is an opaque menu surface instead.
                ec_ui::theme::menu_surface(radius::CARD)(th)
            } else {
                ec_ui::theme::bare_container_if_glass(ec_ui::theme::surface(r, blur)(th))
            }
        });
    let window = reveal(ground, full, outer, offset);
    // The panel fades in over the first third of its opening, so its first
    // sliver is not a hard-edged shard of glass. Over a neighbour it is a plain
    // popover; clear of one it is a pane, joined to its card.
    let panel: Element<'a, Message, Theme> = if overlaps {
        window
    } else {
        pane_linked(
            window,
            CellTone::Plain,
            r.min(radius::CARD),
            (t * 3.0).min(1.0),
            link(id),
        )
    };
    Some(
        container(panel)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(Padding {
                top: y.max(0.0),
                left: x.max(0.0),
                ..Padding::ZERO
            })
            .into(),
    )
}

/// One option of the drawer: a quiet row that lifts under the pointer.
fn option<'a>(label: &str, on_press: Message) -> Element<'a, Message, Theme> {
    let face = container(
        text(label.to_owned())
            .size(size::BODY_SMALL)
            .font(font::UI_MEDIUM)
            .color(color::text()),
    )
    .center_y(Length::Fill)
    .padding([0.0, drawer::ROW_X]);
    button(face)
        .width(Length::Fill)
        .height(Length::Fixed(OPT_H))
        .padding(space::ZERO)
        .on_press(on_press)
        .style(|t: &Theme, s| ec_ui::theme::bar_cell(CellTone::Latent)(t, s))
        .into()
}

/// The ground and link of a joined piece of glass: `id` keys the chain.
fn link(id: u32) -> Option<Link> {
    Some(Link {
        key: u64::from(id),
        fillet: JOIN_R,
    })
}

/// One unit: its cards, and for a group the pointer area that opens it.
fn unit_view<'a>(app: &'a App, unit: &Unit) -> Element<'a, Message, Theme> {
    let mut cards: Vec<Element<'a, Message, Theme>> = unit
        .cards
        .iter()
        .map(|&i| {
            let count = if unit.folded() { unit.all.len() } else { 1 };
            block(app, &app.toasts[i], count)
        })
        .collect();
    if app.corner.bottom() {
        cards.reverse();
    }
    let mut parts: Vec<Element<'a, Message, Theme>> = vec![Column::with_children(cards).spacing(GAP).into()];
    for level in 1..=peeks(unit) {
        parts.push(peek(app, level, &unit.key));
    }
    if app.corner.bottom() {
        parts.reverse();
    }
    let col = Column::with_children(parts);
    if unit.all.len() < 2 {
        return col.into();
    }
    let key = unit.key.clone();
    mouse_area(col)
        .on_enter(Message::HoverGroup(key.clone(), true))
        .on_exit(Message::HoverGroup(key, false))
        .into()
}

/// A strip of the glass behind a folded group's face. Not joined to it: a 6px
/// strip melted into the card's shape reads as a notch in the card, and loses
/// the card's lens and shadow, so the group keeps its separate strips.
fn peek<'a>(app: &App, level: usize, key: &str) -> Element<'a, Message, Theme> {
    let blur = app.blur;
    // Each strip is fainter than the one above it, so the stack reads as
    // receding rather than as three equal bars.
    let alpha = LOW.powi(level as i32);
    let strip = container(Space::new())
        .width(Length::Fixed(app.width as f32 - 2.0 * PEEK_INSET * level as f32))
        .height(Length::Fixed(PEEK_H))
        .style(move |t: &Theme| {
            // Without a pane the strip is the card's own ground, fainter.
            ec_ui::theme::bare_container_if_glass(faded(ec_ui::theme::surface(radius::CHIP, blur)(t), alpha))
        });
    // A click on the strips pins the group open; dismissal stays on the
    // cross and the swipe.
    mouse_area(
        container(pane(strip, CellTone::Plain, radius::CHIP, alpha))
            .width(Length::Fixed(app.width as f32))
            .align_x(Alignment::Center),
    )
    .on_press(Message::Pin(key.to_owned()))
    .interaction(iced::mouse::Interaction::Pointer)
    .into()
}

/// The count of what is waiting. Not a card: it has no body to read and no
/// clock of its own.
fn more<'a>(app: &App, hidden: usize) -> Element<'a, Message, Theme> {
    let label = container(
        text(format!("+{hidden} more"))
            .size(size::MICRO)
            .font(font::DATA)
            .color(color::text_secondary()),
    )
    .padding([space::BADGE_Y * 2.0, space::CHIP_X])
    .style(|t: &Theme| ec_ui::theme::bare_container_if_glass(ec_ui::theme::inset(t)));
    container(pane(label, CellTone::Plain, radius::PILL, 1.0))
        .width(Length::Fixed(app.width as f32))
        .height(Length::Fixed(MORE_H))
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .into()
}

/// A card in a pointer area that holds its clock while the pointer is on it.
fn block<'a>(app: &'a App, toast: &'a Toast, folded: usize) -> Element<'a, Message, Theme> {
    let id = toast.notification.id;
    mouse_area(slot(app, toast, folded))
        .on_enter(Message::HoverCard(id, true))
        .on_exit(Message::HoverCard(id, false))
        .into()
}

/// A card in its fixed slot: the slot catches the pointer, so a card carried
/// off by a drag is still measured against the place it started from.
fn slot<'a>(app: &'a App, toast: &'a Toast, folded: usize) -> Element<'a, Message, Theme> {
    let n = &toast.notification;
    let pose = toast.pose(app.now, &app.anim);
    let presence = pose.presence.clamp(0.0, 1.0);
    let drag = app.drag.filter(|d| d.id == n.id);
    // How far the card is from home, toward the nearest side edge. Carried by
    // a drag, or left by a swipe and still on its way out, else by the
    // entrance and exit: the distance is the card's width at nothing and zero
    // at rest. The slot is clipped to the surface.
    let carried = drag.map_or(toast.fling, |d| d.dx);
    let w = app.width as f32;
    let (away, alpha) = match pose.style {
        Style::Slide => (carried + (w - carried).max(0.0) * (1.0 - presence), 1.0),
        Style::Fade => (carried, presence),
        Style::None => (carried, 1.0),
    };
    let hovered = app.hover == Some(n.id);
    let face = card(app, toast, folded, alpha, hovered);
    let moved = shifted(face, (away * app.corner.side()).round());
    let slot = container(moved)
        .width(Length::Fixed(w))
        .height(Length::Fixed(card_height(n)))
        .clip(true);
    let mut area = mouse_area(slot)
        .on_press(Message::Press(n.id))
        .on_release(Message::Release(n.id))
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
    // Class decides the look. A system card is the shell speaking: its own
    // mark, the brighter focused glass and a crisp rim. An app card can never
    // wear any of that, whatever name, icon or urgency it sends.
    let system = notification.class == Class::System;
    let critical = notification.urgency == Urgency::Critical;
    let low = notification.urgency == Urgency::Low;
    // A low card is the quiet one: its ink and its ground both recede.
    let alpha = if low { alpha * LOW } else { alpha };
    let text_w = app.width as f32 - 2.0 * space::CARD - AVATAR - AVATAR_GAP;

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
        Row::new()
            .align_y(Alignment::Center)
            .push(
                container(
                    text(notification.summary.clone())
                        .size(size::CARD_TITLE)
                        .font(if low { font::UI_MEDIUM } else { font::UI_SEMIBOLD })
                        .wrapping(text::Wrapping::None)
                        .color(summary_ink.scale_alpha(alpha)),
                )
                .width(Length::Fill)
                .height(Length::Fixed(SUMMARY_H))
                .align_y(Alignment::Center)
                .clip(true),
            )
            .push(close_button(notification.id, alpha)),
    );

    if !notification.body.is_empty() {
        let ink = if low {
            color::text_tertiary()
        } else {
            color::text_secondary()
        }
        .scale_alpha(alpha);
        let chars = ((BODY_CHARS as f32 * text_w / TEXT_W_FULL) as usize).max(12);
        let spans: Vec<_> = ec_ui::markup::clamp(&toast.runs, BODY_LINES, chars)
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

    if !notification.actions.is_empty() {
        let question = toast.is_question();
        let mut buttons = Row::new().spacing(GAP).align_y(Alignment::Center);
        // The order is the sender's, left to right, in every corner. Only a
        // system question marks its yes as the default.
        for action in &notification.actions {
            let primary = question && action.key == ec_services::notifications::ANSWER_YES;
            buttons = buttons.push(action_button(
                notification.id,
                &action.key,
                &action.label,
                alpha,
                primary,
            ));
        }
        text_col = text_col.push(
            container(buttons)
                .height(Length::Fixed(ACTIONS_H))
                .align_y(Alignment::End),
        );
    }

    text_col = text_col.push(foot(app, notification, folded, alpha));

    let main = Row::new()
        .spacing(AVATAR_GAP)
        .push(avatar(toast, critical, alpha))
        .push(text_col);

    // With glass the compositor draws the card's ground as a pane, at the
    // card's own opacity, so a fade takes the glass with it (GLASS-05).
    // Without it the card paints its ground itself.
    let radius = app.card_radius();
    let blur = app.blur;
    let ground = container(main)
        .width(Length::Fixed(app.width as f32))
        .height(Length::Fixed(card_height(notification)))
        .padding(space::CARD)
        .style(move |t: &Theme| {
            let mut s = faded(ec_ui::theme::surface(radius, blur)(t), alpha);
            // Without a pane the lift on hover and the crisp rim of a
            // system or critical card are the card's own border.
            if hovered || critical || system {
                s.border.color = color::border_strong().scale_alpha(alpha);
            }
            ec_ui::theme::bare_container_if_glass(s)
        });
    let mut layers = stack![ground]
        .width(Length::Fixed(app.width as f32))
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
    let tone = if system {
        CellTone::Focused
    } else {
        CellTone::Plain
    };
    // A card with its drawer open is one shape with it; otherwise on its own.
    let joined = app.drawer_card == Some(notification.id);
    pane_linked(
        layers,
        tone,
        radius,
        alpha,
        joined.then(|| link(notification.id)).flatten(),
    )
}

/// The card's last line. An app's names its source once and carries Open and
/// the silence button. A system card's names the shell under a SYSTEM badge,
/// the one thing that parts it from an app's at a glance, and carries nothing:
/// there is nothing of the shell's to open or silence.
fn foot<'a>(app: &App, n: &Notification, folded: usize, alpha: f32) -> Element<'a, Message, Theme> {
    let system = n.class == Class::System;
    let source = if folded > 1 {
        format!("{}  ·  {folded}", n.app_name)
    } else {
        n.app_name.clone()
    };
    let mut line = Row::new().spacing(space::HEADER_GAP).align_y(Alignment::Center);
    if system {
        line = line.push(badge(alpha));
    }
    line = line.push(
        container(
            text(source)
                .size(size::MICRO)
                .font(font::DATA)
                .wrapping(text::Wrapping::None)
                .color(color::text_tertiary().scale_alpha(alpha)),
        )
        .width(Length::Fill)
        .clip(true),
    );
    if !system {
        let open = app.drawer_card == Some(n.id);
        line = line
            .push(foot_button("Open", Message::Open(n.id), alpha))
            .push(silence_button(n.id, open, alpha));
    }
    container(line)
        .width(Length::Fill)
        .height(Length::Fixed(foot_h(n)))
        .align_y(Alignment::Center)
        .into()
}

/// SYSTEM, on the shell's own cards. A badge and not a chip: it classifies the
/// card and cannot be pressed.
fn badge<'a>(alpha: f32) -> Element<'a, Message, Theme> {
    container(
        text(ec_ui::widget::tracked("SYSTEM"))
            .size(size::MICRO)
            .font(font::DATA_MEDIUM)
            .color(color::text().scale_alpha(alpha)),
    )
    .padding([space::BADGE_Y, space::BADGE_X])
    .style(move |_t: &Theme| container::Style {
        background: Some(iced::Background::Color(color::cell_focus().scale_alpha(alpha))),
        border: iced::Border {
            color: color::cell_focus_rim().scale_alpha(alpha),
            width: space::HAIRLINE,
            radius: radius::BADGE.into(),
        },
        ..container::Style::default()
    })
    .into()
}

/// Open: a quiet cell that lifts on the pointer.
fn foot_button<'a>(label: &str, on_press: Message, alpha: f32) -> Element<'a, Message, Theme> {
    let label = container(
        text(label.to_owned())
            .size(size::BODY_SMALL)
            .font(font::UI_MEDIUM)
            .wrapping(text::Wrapping::None)
            .color(color::text_secondary().scale_alpha(alpha)),
    )
    .center_y(Length::Fill);
    let cell = button(label)
        .padding([0.0, space::HEADER_GAP])
        .height(Length::Fixed(FOOT_BTN_H))
        .on_press(on_press)
        .style(move |t: &Theme, s| {
            ec_ui::theme::bare_if_glass(ec_ui::theme::bar_cell(CellTone::Latent)(t, s))
        });
    pane(cell, CellTone::Latent, radius::INSET, alpha)
}

/// The silence control: a bell with a slash, square, that opens the drawer.
/// While the drawer is open it is the brighter focused glass, not gold.
fn silence_button<'a>(id: u32, open: bool, alpha: f32) -> Element<'a, Message, Theme> {
    let tone = if open { CellTone::Focused } else { CellTone::Latent };
    let ink = if open {
        color::text()
    } else {
        color::text_secondary()
    };
    let cell = button(container(bell_off(ink.scale_alpha(alpha))).center(Length::Fixed(FOOT_BTN_H)))
        .padding(space::ZERO)
        .width(Length::Fixed(FOOT_BTN_H))
        .height(Length::Fixed(FOOT_BTN_H))
        .on_press(Message::Drawer(id))
        .style(move |t: &Theme, s| ec_ui::theme::bare_if_glass(ec_ui::theme::bar_cell(tone)(t, s)));
    pane(cell, tone, radius::INSET, alpha)
}

/// The card's picture. An app's is what it sent, else its initial on a glass
/// tile; a system card's is the shell's own mark and nothing an app could
/// send, so the two cannot be mistaken for each other.
fn avatar<'a>(toast: &'a Toast, critical: bool, alpha: f32) -> Element<'a, Message, Theme> {
    let side = Length::Fixed(AVATAR);
    let art: Element<'a, Message, Theme> = if toast.notification.class == Class::System {
        ec_ui::widget::art::tile(Glyph::Chip, Hue::System, AVATAR, true)
    } else {
        match &toast.art {
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
        }
    };
    // Pinned to the top: a tall card keeps its picture by the summary.
    container(art)
        .height(Length::Shrink)
        .align_y(Alignment::Start)
        .into()
}

/// Dismiss one card. Always there: a quiet cell that lifts on the pointer.
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
            ec_ui::theme::bare_if_glass(ec_ui::theme::bar_cell(CellTone::Latent)(t, s))
        });
    pane(cell, CellTone::Latent, radius::PILL, alpha)
}

/// One of the card's declared buttons. A system question's yes is the default
/// action: the focused cell, whose label is gold, the card's one yellow.
fn action_button<'a>(
    id: u32,
    key: &str,
    label: &str,
    alpha: f32,
    primary: bool,
) -> Element<'a, Message, Theme> {
    let tone = CellTone::from(primary);
    let mut text = text(label.to_owned())
        .size(size::BODY_SMALL)
        .font(font::UI_MEDIUM);
    if !primary {
        text = text.color(color::text().scale_alpha(alpha));
    }
    let label = container(text).center_y(Length::Fill);
    let cell = button(label)
        .padding([0.0, space::CARD])
        .height(Length::Fixed(ACTION_BTN_H))
        .on_press(Message::Invoke(id, key.to_owned()))
        .style(move |t: &Theme, s| ec_ui::theme::bare_if_glass(ec_ui::theme::bar_cell(tone)(t, s)));
    pane(cell, tone, radius::INSET, alpha)
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
        assert_eq!(height(&[], &Plan::default(), false, None), 0);
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
        assert_eq!(height(&toasts, &folded, false, None), (one + 2.0 * PEEK_H) as u32);
        let open = Plan {
            units: vec![unit(vec![0, 1, 2], vec![2, 1, 0], true)],
            hidden: 0,
        };
        assert_eq!(
            height(&toasts, &open, false, None),
            (3.0 * one + 2.0 * GAP) as u32
        );
        let waiting = Plan { hidden: 2, ..open };
        assert_eq!(
            height(&toasts, &waiting, false, None),
            (3.0 * one + 3.0 * GAP + MORE_H) as u32
        );
    }

    /// The drawer is laid over the stack: opening it moves no card, and the
    /// surface only grows where the drawer would hang past the stack's end.
    #[test]
    fn the_drawer_moves_nothing_and_reserves_room_past_the_ends() {
        let toasts: Vec<Toast> = (0..2)
            .map(|i| {
                let mut n = notification("", 0);
                n.id = i;
                n.group = format!("g{i}");
                Toast::new(n)
            })
            .collect();
        let plan = Plan {
            units: vec![unit(vec![0], vec![0], false), unit(vec![1], vec![1], false)],
            hidden: 0,
        };
        let plain = geometry(&toasts, &plan, false, None);
        let on_first = geometry(&toasts, &plan, false, Some((0, false)));
        assert_eq!(plain.stack, on_first.stack);
        assert_eq!(on_first.lead, 0.0);
        assert!(
            on_first.tail < drawer_height(false),
            "the next card already gives it most of its room"
        );
        assert!(on_first.overlaps);
        let on_last = geometry(&toasts, &plan, false, Some((1, false)));
        assert_eq!(on_last.tail, drawer_height(false));
        assert!(!on_last.overlaps, "nothing under it: it can join the card");
        // Mirrored for a bottom corner: the newest card is the farthest from the
        // edge, so it is the one with nothing above it.
        let low = geometry(&toasts, &plan, true, Some((1, false)));
        assert_eq!(low.lead, drawer_height(false));
        assert!(!low.overlaps);
        assert_eq!(
            height(&toasts, &plan, false, Some((1, false))),
            (plain.stack + drawer_height(false)).ceil() as u32
        );
    }
}
