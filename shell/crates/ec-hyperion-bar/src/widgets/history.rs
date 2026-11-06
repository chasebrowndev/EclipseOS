// SPDX-License-Identifier: AGPL-3.0-only
//! The notification bell and its history panel (`mode "de"`, COMP-17 §2).
//!
//! Under `de` the widget area carries a bell after the tray. It opens a panel
//! of what the notification service remembers, newest first: the app, the
//! summary, the body cut to a few lines and how long ago, with a dismiss per
//! entry, "Clear all" in the head, and a live entry's own action buttons. A dot
//! on the bell says something listed has not been looked at yet.
//!
//! A status widget like progress: it reads `app.history` and owns no state.
//! The panel is a popup (the tray overflow's machinery), sized from the entries
//! it lists and reopened when they change.
//!
//! ## Shape
//!
//! Not a menu and not a list of menu rows: a head with its one control, then
//! blocks of different heights — a short entry is two lines, a long one has
//! its body and a row of pill buttons — parted by hairlines, and a critical
//! entry carries a short danger rail. Empty, it is one centred note.
//!
//! ## The accent ledger
//!
//! **None.** The bell's dot is white: it says "unread", not "live". The
//! one yellow on the bar is the focused chip's, and a notification list has
//! no state of its own that deserves a second.
//!
//! Bodies are human content and are never logged.

use iced::widget::{button, row, text, Column, Row, Space};
use iced::{Alignment, Element, Length, Theme};

use ec_services::notifications::{HistoryEntry, Urgency};
use ec_ui::theme;
use ec_ui::tokens::{bar, color, drawer, font, history as tok, size};
use ec_ui::widget::{self as parts, ShellFrame};

use super::{Parts, Spans};
use crate::app::{App, Drawer, Message};
use crate::history::{ago, clamp_body, entry_height, now};
use crate::view::{elide, Sheet};

/// The bell: the freedesktop notifications mark.
const CELL_ICON: &str = "preferences-system-notifications";

pub fn spans(_app: &App) -> Spans {
    Spans {
        core: bar::MARK,
        revealed: 0.0,
        lead: 0.0,
        present: true,
    }
}

pub fn view(app: &App, frame: ShellFrame) -> Parts<'_> {
    let ink = frame.core_alpha();
    let mark = parts::mark(
        crate::icons::symbolic(CELL_ICON),
        bar::MARK,
        color::text_secondary().scale_alpha(ink),
    );
    // The dot sits on the bell's upper right corner.
    let face: Element<'_, Message, Theme> = if app.history.unseen() {
        let dot = parts::quad(
            Length::Fixed(bar::BELL_DOT),
            Length::Fixed(bar::BELL_DOT),
            color::text().scale_alpha(ink),
            bar::BELL_DOT / 2.0,
        );
        iced::widget::stack![
            mark,
            iced::widget::container(dot)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Alignment::End)
                .align_y(Alignment::Start),
        ]
        .width(Length::Fixed(bar::MARK))
        .height(Length::Fixed(bar::MARK))
        .into()
    } else {
        mark
    };
    Parts {
        core: super::press(face, bar::MARK, Message::Open(Drawer::History)),
        revealed: None,
    }
}

/// The panel. See the module doc for its anatomy and ledger.
pub(crate) fn sheet(app: &App) -> Sheet {
    let panel = &app.history;
    let mut s = Sheet::new(head(!panel.is_empty()), drawer::HEAD_H);
    if panel.is_empty() {
        s.block(empty(), tok::EMPTY_H);
        return s;
    }
    let at = now();
    let shown = panel.shown();
    for (i, e) in shown.iter().enumerate() {
        if i > 0 {
            s.rule();
        }
        s.block(entry(e, at), entry_height(e));
    }
    if panel.hidden() > 0 {
        s.row(parts::drawer_note(&format!("+{} older", panel.hidden())));
    }
    s
}

/// The head: the micro label, and "Clear all" at the right when there is
/// anything to clear.
fn head(clearable: bool) -> Element<'static, Message, Theme> {
    let mut r = Row::new()
        .align_y(Alignment::Center)
        .push(parts::micro_label("notifications"))
        .push(Space::new().width(Length::Fill));
    if clearable {
        r = r.push(
            button(text("Clear all").font(font::UI_MEDIUM).size(size::BODY_SMALL))
                .padding(0)
                .style(theme::link)
                .on_press(Message::HistoryClear),
        );
    }
    iced::widget::container(r)
        .width(Length::Fill)
        .height(Length::Fixed(drawer::HEAD_H))
        .align_y(Alignment::Center)
        .padding([0.0, drawer::ROW_X])
        .into()
}

fn empty() -> Element<'static, Message, Theme> {
    iced::widget::container(
        Column::new()
            .align_x(Alignment::Center)
            .spacing(tok::GAP)
            .push(
                text("No notifications")
                    .font(font::UI_MEDIUM)
                    .size(size::BODY_SMALL)
                    .color(color::text_secondary()),
            )
            .push(
                text("Anything that arrives stays here")
                    .font(font::DATA)
                    .size(size::MICRO)
                    .color(color::text_tertiary()),
            ),
    )
    .width(Length::Fill)
    .height(Length::Fixed(tok::EMPTY_H))
    .align_x(Alignment::Center)
    .align_y(Alignment::Center)
    .into()
}

/// One entry's block: app and age with the dismiss cross, the summary, up to a
/// few lines of body, and the buttons of a notification that is still up.
fn entry(e: &HistoryEntry, at: u64) -> Element<'static, Message, Theme> {
    let critical = e.urgency == Urgency::Critical;
    let id = e.id;
    let head = row![
        text(elide(&e.app_name.to_uppercase(), tok::APP_CHARS))
            .font(font::DATA)
            .size(size::MICRO)
            .color(color::text_tertiary()),
        text(ago(at, e.timestamp))
            .font(font::DATA)
            .size(size::MICRO)
            .color(color::text_tertiary()),
        Space::new().width(Length::Fill),
        button(
            iced::widget::container(
                text("\u{d7}")
                    .font(font::DATA)
                    .size(size::BODY_SMALL)
                    .color(color::text_secondary()),
            )
            .center(Length::Fill),
        )
        .width(Length::Fixed(tok::DISMISS))
        .height(Length::Fixed(tok::DISMISS))
        .padding(0)
        .style(dismiss_style)
        .on_press(Message::HistoryRemove(id)),
    ]
    .spacing(drawer::MARK_GAP)
    .align_y(Alignment::Center)
    .height(Length::Fixed(tok::HEAD_H));

    let mut col = Column::new()
        .spacing(tok::GAP)
        .width(Length::Fill)
        .push(head)
        .push(
            iced::widget::container(
                text(elide(&e.summary, tok::SUMMARY_CHARS))
                    .font(font::UI_MEDIUM)
                    .size(size::BODY_SMALL)
                    .color(color::text()),
            )
            .height(Length::Fixed(tok::SUMMARY_H))
            .align_y(Alignment::Center),
        );
    for line in clamp_body(&e.body) {
        col = col.push(
            iced::widget::container(
                text(line)
                    .font(font::UI)
                    .size(size::BODY_SMALL)
                    .color(color::text_secondary()),
            )
            .height(Length::Fixed(tok::LINE_H))
            .align_y(Alignment::Center),
        );
    }
    if e.live && !e.actions.is_empty() {
        let mut actions = Row::new().spacing(drawer::MARK_GAP);
        for (key, label) in &e.actions {
            actions = actions.push(
                button(
                    iced::widget::container(
                        text(elide(label, tok::APP_CHARS))
                            .font(font::UI_MEDIUM)
                            .size(size::MICRO),
                    )
                    .height(Length::Fill)
                    .align_y(Alignment::Center),
                )
                .padding([0.0, drawer::ROW_X])
                .height(Length::Fixed(tok::ACTION_H - tok::GAP))
                .style(theme::pill(false))
                .on_press(Message::HistoryAction(id, key.clone())),
            );
        }
        col = col.push(actions);
    }
    let height = entry_height(e);
    // The rail sits in the left pad, so a critical entry's text stays on the
    // same gridline as its neighbours'.
    let left = if critical {
        drawer::ROW_X - tok::RAIL_W
    } else {
        drawer::ROW_X
    };
    let body = iced::widget::container(col)
        .width(Length::Fill)
        .padding(iced::Padding {
            top: tok::PAD_Y,
            right: drawer::ROW_X,
            bottom: tok::PAD_Y,
            left,
        });
    let block: Element<'static, Message, Theme> = if critical {
        row![
            iced::widget::container(parts::quad(
                Length::Fixed(tok::RAIL_W),
                Length::Fill,
                color::danger(),
                tok::RAIL_W / 2.0
            ))
            .height(Length::Fill)
            .padding([tok::PAD_Y, 0.0]),
            body
        ]
        .into()
    } else {
        body.into()
    };
    iced::widget::container(block)
        .width(Length::Fill)
        .height(Length::Fixed(height))
        .into()
}

/// The dismiss cross: bare until the pointer is on it.
fn dismiss_style(_t: &Theme, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Hovered => Some(iced::Background::Color(color::lift())),
        button::Status::Pressed => Some(iced::Background::Color(color::lift_strong())),
        _ => None,
    };
    button::Style {
        background,
        text_color: color::text_secondary(),
        border: iced::border::rounded(drawer::RADIUS_ROW),
        ..button::Style::default()
    }
}
