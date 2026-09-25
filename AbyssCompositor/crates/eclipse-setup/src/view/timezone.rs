// SPDX-License-Identifier: AGPL-3.0-only
//! Step 2: time zone.
//!
//! Hero: a magnitude with a place on a scale. The big local time at the zone
//! you have chosen, over a ruler of the world's hours with a marker where that
//! zone sits. Under it a strip of region tabs, then the list of places.
//! The accented value: the local time. The ruler marker is white and the zone
//! list selection is neutral.

use super::{list_box, Body};
use crate::metrics;
use crate::model::{Message, Model};
use crate::parts::{self, El};
use crate::sys;
use eclipse_ui::theme;
use eclipse_ui::tokens::{color, font, radius, size, space};
use eclipse_ui::widget::{big_value, inset, micro_label, panel, quad};
use iced::widget::{column, container, row, text, Column, Row, Space};
use iced::{Alignment, Length};

pub fn body(m: &Model) -> Body<'_> {
    let (time, unit, hours) = match &m.zone_clock {
        Some(c) => (
            c.time.clone(),
            sys::offset_label(&c.offset),
            sys::offset_hours(&c.offset),
        ),
        None => ("--:--".to_owned(), "UTC".to_owned(), None),
    };

    let place = zone_name(&m.zone);
    let head = row![
        column![
            micro_label("Local time"),
            big_value(&time, &unit, m.zone_clock.is_some())
        ]
        .spacing(space::CHIP_GAP),
        Space::new().width(Length::Fill),
        column![
            text(place.clone())
                .font(font::UI_SEMIBOLD)
                .size(size::PROMPT)
                .style(theme::text_primary),
            parts::mono(m.zone.clone(), color::TEXT_TERTIARY),
        ]
        .spacing(space::CHIP_GAP)
        .align_x(Alignment::End),
    ]
    .align_y(Alignment::End);

    let hero = panel(radius::CARD, column![head, ruler(hours)].spacing(space::BLOCK));

    // --- region tabs
    let tabs = Row::with_children(m.regions().into_iter().map(|r| {
        let on = r == m.region;
        El::from(parts::tab(&r, on, Message::Region(r.clone())))
    }))
    .spacing(space::PILL_GAP)
    .wrap()
    .vertical_spacing(space::PILL_GAP);
    let tabs = inset(tabs).padding(space::CARD);

    // --- places
    let mut list = Column::new();
    for z in m.zones_in_region() {
        let name = zone_name(z);
        let content = row![
            text(name)
                .font(font::UI)
                .size(size::BODY)
                .style(theme::text_primary),
            Space::new().width(Length::Fill),
            parts::mono(z.clone(), color::TEXT_TERTIARY),
        ]
        .align_y(Alignment::Center);
        list = list.push(parts::pick(
            content,
            *z == m.zone,
            false,
            Some(Message::Zone(z.clone())),
        ));
    }
    let places = list_box(&m.region, vec![], None, list);

    Body {
        subtitle: "Where the machine lives. The clock and the calendar follow it.".to_owned(),
        chip: (m.zone.clone(), format!("{unit}  {time}")),
        blocks: vec![hero.into(), tabs.into(), places],
        scroll: false,
    }
}

/// `America/Argentina/Buenos_Aires` -> `Argentina / Buenos Aires`.
fn zone_name(zone: &str) -> String {
    zone.split_once('/')
        .map_or(zone, |(_, rest)| rest)
        .replace('_', " ")
        .replace('/', " / ")
}

/// One column per hour from UTC-12 to UTC+14, a tick each, the marker where
/// the zone sits (rounded to the hour; the exact offset is in the label).
fn ruler<'a>(hours: Option<f32>) -> El<'a, Message> {
    let here = hours.map(|h| h.round() as i32);
    let mut r = Row::new().width(Length::Fill);
    for h in metrics::RULER_FIRST..=metrics::RULER_LAST {
        let on = here == Some(h);
        let mark: El<'a, Message> = if on {
            quad(
                Length::Fixed(metrics::RULER_MARKER_W),
                Length::Fixed(metrics::RULER_H - metrics::RULER_TICK_H),
                color::TEXT,
                radius::BAR,
            )
        } else {
            quad(
                Length::Fixed(space::HAIRLINE),
                Length::Fixed(metrics::RULER_TICK_H),
                color::BORDER_STRONG,
                0.0,
            )
        };
        let label = match h {
            0 => "0".to_owned(),
            h if h > 0 => format!("+{h}"),
            h => format!("{h}"),
        };
        r = r.push(
            container(
                column![
                    container(mark)
                        .height(Length::Fixed(metrics::RULER_H - metrics::RULER_TICK_H))
                        .align_y(Alignment::End),
                    parts::mono(label, if on { color::TEXT } else { color::TEXT_TERTIARY }),
                ]
                .spacing(space::CHIP_GAP / 2.0)
                .align_x(Alignment::Center),
            )
            .width(Length::Fill)
            .center_x(Length::Fill),
        );
    }
    r.into()
}
