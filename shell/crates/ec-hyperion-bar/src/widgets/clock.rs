// SPDX-License-Identifier: AGPL-3.0-only
//! The clock: time over date, both mono and centred — the only two-line cell
//! on the row, which is what makes the right end read as an end. Important
//! by default; always present.
//!
//! It is also the one place the bar admits the compositor is gone: a
//! disconnected bar dims its clock rather than freezing it.

use iced::widget::{column, container, mouse_area, row, scrollable, text, Column, Row, Space};
use iced::{Alignment, Length};

use ec_ui::theme;
use ec_ui::tokens::{bar, color, drawer, font, size, space};
use ec_ui::widget::{self as parts, DayCell, ShellFrame};

use super::{Parts, Spans};
use crate::app::{App, Drawer, Message};
use crate::calendar::{self, Day};
use crate::view::Sheet;

pub fn spans(_app: &App) -> Spans {
    // A fixed width, so a minute rolling over never makes the chips twitch.
    Spans {
        core: bar::CLOCK_W - 2.0 * bar::WIDGET_X,
        revealed: 0.0,
        lead: 0.0,
        present: true,
    }
}

pub fn view(app: &App, frame: ShellFrame) -> Parts<'_> {
    let ink = frame.core_alpha();
    let tint = if app.snapshot.connected {
        color::TEXT
    } else {
        color::TEXT_TERTIARY
    };
    let stack = column![
        text(crate::clock::time(app.bar.hour_12))
            .size(size::BODY_SMALL)
            .font(font::DATA_MEDIUM)
            .color(tint.scale_alpha(ink)),
        text(crate::clock::date(app.bar.date_mdy))
            .size(size::MICRO)
            .font(font::DATA)
            .color(color::TEXT_TERTIARY.scale_alpha(ink)),
    ]
    .spacing(0)
    .align_x(Alignment::Center);
    // The whole cell is the calendar's button.
    Parts {
        core: super::press(
            stack,
            bar::CLOCK_W - 2.0 * bar::WIDGET_X,
            Message::Open(Drawer::Calendar),
        ),
        revealed: None,
    }
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
/// Indexed by `calendar::weekday_of`, Sunday first.
const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const INITIALS: [&str; 7] = ["S", "M", "T", "W", "T", "F", "S"];

/// The month calendar: the grid at the left and the selected day's detail at
/// the right, one two-column block on the drawer's glass.
///
/// - **Hero:** the month grid, a spatial canvas of days; its neighbour is a
///   different silhouette (a scrolling list of events beside a rule), not a
///   second panel.
/// - **Accent ledger:** the one yellow is today's numeral. The selected day is
///   the focused-chip glass and a crisp rim; event colours are the provider's
///   data and never the accent.
/// - **No provider:** the grid stays whole with no dots, and the detail says
///   so quietly and offers nothing to press.
pub(crate) fn sheet(app: &App) -> Sheet {
    let cal = &app.calendar;
    let grid = calendar::grid(cal.view_y, cal.view_m, app.bar.week_start);
    let week_numbers = app.bar.week_numbers;
    let on_this_month = grid.iter().flatten().any(|d| d.today && d.in_month);

    // Left: header, initials, six weeks.
    let initials: [&str; 7] = std::array::from_fn(|i| {
        let d = &grid[0][i];
        INITIALS[calendar::weekday_of(d.y, d.m, d.d) as usize]
    });
    let mut left = Column::new()
        .push(parts::cal_header(
            MONTHS[(cal.view_m - 1) as usize],
            &cal.view_y.to_string(),
            (!on_this_month).then_some(Message::CalToday),
            Message::CalPrev,
            Message::CalNext,
        ))
        .push(parts::cal_weekdays(initials, week_numbers));
    for week in &grid {
        let mut r = Row::new();
        if week_numbers {
            let mid = &week[3];
            r = r.push(parts::cal_week_no(
                &calendar::iso_week(mid.y, mid.m, mid.d).to_string(),
            ));
        }
        for d in week {
            let dots: Vec<_> = cal
                .day_events(d.y, d.m, d.d)
                .map(|e| event_colour(e.colour))
                .take(3)
                .collect();
            r = r.push(parts::cal_day(
                d.d,
                DayCell {
                    in_month: d.in_month,
                    today: d.today,
                    selected: (d.y, d.m, d.d) == cal.selected,
                },
                &dots,
                Message::CalSelect(d.y, d.m, d.d),
            ));
        }
        left = left.push(r);
    }
    let left_w = 7.0 * drawer::CAL_CELL_W + if week_numbers { drawer::CAL_WEEK_W } else { 0.0 };
    let left = mouse_area(container(left).width(Length::Fixed(left_w))).on_scroll(Message::CalScroll);

    let block = row![
        left,
        Space::new().width(Length::Fixed(drawer::CAL_GAP)),
        parts::cal_divider(),
        Space::new().width(Length::Fixed(drawer::CAL_GAP)),
        detail(app, &grid),
    ];
    let height = drawer::CAL_HEAD_H + drawer::CAL_WEEKDAY_H + 6.0 * drawer::CAL_CELL_H;
    let mut s = Sheet::new(Space::new().into(), 0.0);
    s.block(
        container(block)
            .width(Length::Fill)
            .height(Length::Fixed(height))
            .into(),
        height,
    );
    s
}

/// The selected day: its date, its place in the year, then what is on it.
fn detail(app: &App, _grid: &[[Day; 7]; 6]) -> iced::Element<'static, Message, iced::Theme> {
    let cal = &app.calendar;
    let (y, m, d) = cal.selected;
    let title = format!(
        "{}, {} {}",
        WEEKDAYS[calendar::weekday_of(y, m, d) as usize],
        MONTHS[(m - 1) as usize],
        d
    );
    let place = format!(
        "week {} \u{b7} day {}",
        calendar::iso_week(y, m, d),
        calendar::day_of_year(y, m, d)
    );
    let head = column![
        text(title)
            .font(font::UI_SEMIBOLD)
            .size(size::BODY)
            .color(color::TEXT),
        parts::micro_label(&place),
    ]
    .spacing(drawer::LINE_GAP + 2.0);

    // All-day and multi-day events are tags on the day; the rest are its
    // schedule, in order.
    let day_start = calendar::local_midnight(y, m, d);
    let day_end = calendar::local_midnight(y, m, d + 1);
    let mut tags = Vec::new();
    let mut timed = Vec::new();
    for e in cal.day_events(y, m, d) {
        if e.all_day || (e.start < day_start && e.end > day_end) {
            tags.push(e);
        } else {
            timed.push(e);
        }
    }
    timed.sort_by_key(|e| e.start);

    let body: iced::Element<'static, Message, iced::Theme> = if tags.is_empty() && timed.is_empty() {
        parts::cal_note(if cal.provider {
            "Nothing scheduled"
        } else {
            "No calendar connected \u{2014} Ethereal will sync here"
        })
    } else {
        let mut list = Column::new().spacing(drawer::ROW_INSET * 2.0);
        for e in tags {
            list = list.push(parts::cal_event_chip(
                event_colour(e.colour),
                &e.title,
                Message::CalOpenEvent(e.id.clone()),
            ));
        }
        for e in timed {
            list = list.push(parts::cal_event_row(
                event_colour(e.colour),
                (
                    &crate::clock::hm(e.start, app.bar.hour_12),
                    &crate::clock::hm(e.end, app.bar.hour_12),
                ),
                &e.title,
                e.location.as_deref(),
                Message::CalOpenEvent(e.id.clone()),
            ));
        }
        scrollable(list)
            .style(theme::eclipse_scrollable)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    };

    let mut col = Column::new()
        .push(container(head).padding([space::ROW_Y / 2.0, 0.0]))
        .push(Space::new().height(Length::Fixed(space::ROW_Y - 3.0)))
        .push(parts::drawer_rule())
        .push(container(body).width(Length::Fill).height(Length::Fill));
    if cal.provider {
        col = col.push(parts::drawer_link("Open in calendar", Message::CalOpenDay));
    }
    col.width(Length::Fill).height(Length::Fill).into()
}

/// An event's dot or bar: its own colour, else the neutral swatch.
fn event_colour(c: Option<u32>) -> iced::Color {
    c.map_or(color::NEUTRAL, color::data)
}
