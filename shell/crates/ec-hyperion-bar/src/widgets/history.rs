// SPDX-License-Identifier: AGPL-3.0-only
//! The notification widget (`mode "de"`, COMP-17 §2): the bar's bell, its
//! quick view and the history drawer inside it.
//!
//! The bell carries the mode's mark (a bell, a moon, a bell struck through)
//! and, when something is unread, the count in the bar's mono: `(3)`. It opens
//! a small popover with the unread notifications, newest first (the app, the
//! summary, the body cut to a few lines and how long ago, a dismiss per entry
//! and a live entry's own action buttons), under a three-way mode switch.
//! Opening marks everything read; the rows stay on screen for as long as the
//! popover is open, then live on in the history.
//!
//! The **history drawer** is a fold row under the quick view, shut by
//! default. Unfolded it lists the read entries three at a time with a pager
//! and "Clear all".
//!
//! A status widget like progress: it reads `app.history` and owns no state.
//! The popover is a popup (the tray overflow's machinery), sized from what it
//! lists and reopened when that changes.
//!
//! ## Shape
//!
//! Not a menu and not a list of menu rows: a head, the segmented mode lozenge,
//! then blocks of different heights (a short entry is two lines, a long one
//! has its body and a row of pill buttons) parted by hairlines, a critical
//! entry carrying a short danger rail, and the fold row at the foot. Empty,
//! the quick view is one centred note.
//!
//! ## The accent ledger
//!
//! **None.** The count is white: it says "unread", not "live". The selected
//! mode is the focused-chip glass, a brighter fill and a crisp rim, never the
//! accent. The one yellow on the bar is the focused chip's.
//!
//! Bodies are human content and are never logged.

use iced::widget::{button, row, text, Column, Row, Space};
use iced::{Alignment, Element, Length, Theme};

use ec_services::notifications::{HistoryEntry, Mode, Urgency};
use ec_ui::theme;
use ec_ui::tokens::{bar, color, drawer, font, history as tok, size};
use ec_ui::widget::{self as parts, ShellFrame};

use super::{Parts, Spans};
use crate::app::{App, Drawer, Message};
use crate::history::{ago, badge, clamp_body, entry_height, now};
use crate::view::{elide, Sheet};

/// The mark for each mode: the freedesktop bell, a moon for Quiet (stored, no
/// popup) and the struck-through bell for Do Not Disturb (ignored).
const fn icon(mode: Mode) -> &'static str {
    match mode {
        Mode::Normal => "preferences-system-notifications",
        Mode::Quiet => "weather-clear-night",
        Mode::Dnd => "notifications-disabled",
    }
}

/// The bell's width: the mark, and the count after it when there is one.
fn core_width(count: u32) -> f32 {
    let chars = badge(count).chars().count();
    if chars == 0 {
        bar::MARK
    } else {
        bar::MARK + bar::GAP + chars as f32 * bar::CHAR_W
    }
}

pub fn spans(app: &App) -> Spans {
    Spans {
        core: core_width(app.history.unread()),
        revealed: 0.0,
        lead: 0.0,
        present: true,
    }
}

pub fn view(app: &App, frame: ShellFrame) -> Parts<'_> {
    let ink = frame.core_alpha();
    let mode = app.history.mode();
    let tint = if mode == Mode::Normal {
        color::text_secondary()
    } else {
        color::text_tertiary()
    };
    let mark = parts::mark(
        crate::icons::symbolic(icon(mode)),
        bar::MARK,
        tint.scale_alpha(ink),
    );
    let count = app.history.unread();
    let mut face = Row::new().spacing(bar::GAP).align_y(Alignment::Center).push(mark);
    if count > 0 {
        face = face.push(
            text(badge(count))
                .size(size::MONO)
                .font(font::DATA)
                .wrapping(text::Wrapping::None)
                .color(color::text_secondary().scale_alpha(ink)),
        );
    }
    Parts {
        core: super::press(face, core_width(count), Message::Open(Drawer::History)),
        revealed: None,
    }
}

/// The popover. See the module doc for its anatomy and ledger.
pub(crate) fn sheet(app: &App) -> Sheet {
    let panel = &app.history;
    let quick = panel.quick();
    let mut s = Sheet::new(head(panel.unread() as usize), drawer::HEAD_H);

    s.block(mode_switch(panel.mode()), tok::SWITCH_H);
    s.rule();

    let at = now();
    if quick.is_empty() {
        s.block(
            empty("All caught up", "New notifications land here"),
            tok::EMPTY_H,
        );
    }
    for (i, e) in quick.iter().enumerate() {
        if i > 0 {
            s.rule();
        }
        s.block(entry(e, at, false), entry_height(e));
    }
    if panel.quick_hidden() > 0 && !panel.expanded() {
        // Opens the drawer, where the rest are.
        s.block(
            parts::drawer_more(
                &format!("+{} more in history", panel.quick_hidden()),
                Message::HistoryFold,
            ),
            drawer::ROW_H,
        );
    }

    s.rule();
    let total = panel.read_total();
    s.block(
        parts::drawer_fold(
            "History",
            &total.to_string(),
            panel.expanded(),
            Message::HistoryFold,
        ),
        drawer::ROW_H,
    );
    if panel.expanded() {
        let rows = panel.drawer_rows();
        if total == 0 {
            s.block(
                empty("Nothing in history", "Read notifications are kept here"),
                tok::EMPTY_H,
            );
        }
        for (i, e) in rows.into_iter().enumerate() {
            if i > 0 {
                s.rule();
            }
            s.block(entry(e, at, true), entry_height(e));
        }
        if total > 0 {
            s.rule();
            s.block(pager(panel), tok::PAGER_H);
        }
    }
    s
}

/// The head: the micro label, and how many were new at the right.
fn head(unread: usize) -> Element<'static, Message, Theme> {
    let mut r = Row::new()
        .align_y(Alignment::Center)
        .push(parts::micro_label("notifications"))
        .push(Space::new().width(Length::Fill));
    if unread > 0 {
        r = r.push(
            text(format!("{unread} new"))
                .font(font::DATA)
                .size(size::MICRO)
                .color(color::text_tertiary()),
        );
    }
    iced::widget::container(r)
        .width(Length::Fill)
        .height(Length::Fixed(drawer::HEAD_H))
        .align_y(Alignment::Center)
        .padding([0.0, drawer::ROW_X])
        .into()
}

/// Normal, Quiet, Do not disturb: one lozenge, one in force.
fn mode_switch(mode: Mode) -> Element<'static, Message, Theme> {
    let option = |label, m: Mode| (label, mode == m, Some(Message::HistoryMode(m)));
    iced::widget::container(parts::segment_control(vec![
        option("Normal", Mode::Normal),
        option("Quiet", Mode::Quiet),
        option("Do not disturb", Mode::Dnd),
    ]))
    .width(Length::Fill)
    .height(Length::Fixed(tok::SWITCH_H))
    .align_y(Alignment::Center)
    .padding([0.0, drawer::ROW_X])
    .into()
}

/// The drawer's foot: older/newer, where the page is, and "Clear all".
fn pager(panel: &crate::history::Panel) -> Element<'static, Message, Theme> {
    let (first, last) = panel.span();
    let step = |glyph: &'static str, to: Option<u32>| {
        let b = button(
            iced::widget::container(text(glyph).font(font::DATA).size(size::BODY_SMALL).color(
                if to.is_some() {
                    color::text_secondary()
                } else {
                    color::text_tertiary()
                },
            ))
            .center(Length::Fill),
        )
        .width(Length::Fixed(tok::DISMISS))
        .height(Length::Fixed(tok::DISMISS))
        .padding(0)
        .style(lift_style);
        match to {
            Some(offset) => Element::from(b.on_press(Message::HistoryPage(offset))),
            // No page that way: the slot stays, empty, so the count does not shift.
            None => Space::new()
                .width(Length::Fixed(tok::DISMISS))
                .height(Length::Fixed(tok::DISMISS))
                .into(),
        }
    };
    let r = row![
        step("\u{2190}", panel.newer()),
        text(format!("{first}\u{2013}{last} of {}", panel.read_total()))
            .font(font::DATA)
            .size(size::MICRO)
            .color(color::text_tertiary()),
        step("\u{2192}", panel.older()),
        Space::new().width(Length::Fill),
        button(text("Clear all").font(font::UI_MEDIUM).size(size::BODY_SMALL))
            .padding(0)
            .style(theme::link)
            .on_press(Message::HistoryClear),
    ]
    .spacing(drawer::MARK_GAP)
    .align_y(Alignment::Center);
    iced::widget::container(r)
        .width(Length::Fill)
        .height(Length::Fixed(tok::PAGER_H))
        .align_y(Alignment::Center)
        .padding([0.0, drawer::ROW_X])
        .into()
}

fn empty(title: &str, hint: &str) -> Element<'static, Message, Theme> {
    iced::widget::container(
        Column::new()
            .align_x(Alignment::Center)
            .spacing(tok::GAP)
            .push(
                text(title.to_owned())
                    .font(font::UI_MEDIUM)
                    .size(size::BODY_SMALL)
                    .color(color::text_secondary()),
            )
            .push(
                text(hint.to_owned())
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

/// One entry's block (a read one in the secondary ink): app and age with the dismiss cross, the summary, up to a
/// few lines of body, and the buttons of a notification that is still up.
fn entry(e: &HistoryEntry, at: u64, read: bool) -> Element<'static, Message, Theme> {
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
        .style(lift_style)
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
                    .color(if read {
                        color::text_secondary()
                    } else {
                        color::text()
                    }),
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

/// A small square control (the dismiss cross, a pager arrow): bare until the
/// pointer is on it.
fn lift_style(_t: &Theme, status: button::Status) -> button::Style {
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
