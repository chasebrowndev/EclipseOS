// SPDX-License-Identifier: AGPL-3.0-only
//! The structural pieces of a pane.
//!
//! These are constructors over iced's stock containers, not new widgets: the
//! spec's structure rules (one level of inset, a 214px sidebar, a header row
//! with its subtitle) are layout, and layout is what `container` and `column`
//! already do. Keeping them here rather than in each app is what stops the
//! third settings pane from inventing its own padding.

use iced::widget::{button, column, container, row, rule, text, Column, Row, Space};
use iced::{Alignment, Color, Element, Font, Length, Theme};

use crate::theme;
use crate::tokens::{color, font, radius, size, space};

/// A glass panel. The one container every block on a pane sits in.
pub fn panel<'a, Message: 'a>(
    content: impl Into<Element<'a, Message, Theme>>,
) -> container::Container<'a, Message, Theme> {
    container(content)
        .padding(space::CARD)
        .style(theme::panel)
        .width(Length::Fill)
}

/// One level of inset inside a panel — and the only level. There is no
/// `inset(inset(..))` guard in code, so this is the honour system the style
/// spec asks for.
pub fn inset<'a, Message: 'a>(
    content: impl Into<Element<'a, Message, Theme>>,
) -> container::Container<'a, Message, Theme> {
    container(content).style(theme::inset).width(Length::Fill)
}

/// A small uppercase mono section label.
pub fn micro_label<'a, Message: 'a>(label: &str) -> Element<'a, Message, Theme> {
    // Tracking is not a `text` property in iced, so the spacing the spec asks
    // for is put into the string itself. Ugly, honest, and it renders right.
    let spaced: String = label
        .to_uppercase()
        .chars()
        .flat_map(|c| [c, '\u{2009}'])
        .collect();
    text(spaced)
        .font(font::DATA_MEDIUM)
        .size(size::MICRO)
        .style(theme::text_tertiary)
        .into()
}

/// A pane title with its one-line subtitle, and up to three controls at the
/// right. The subtitle is where a pane is allowed to say one thing in yellow.
pub fn header<'a, Message: 'a>(
    title: &'a str,
    subtitle: impl Into<Element<'a, Message, Theme>>,
    controls: Vec<Element<'a, Message, Theme>>,
) -> Element<'a, Message, Theme> {
    let left = column![
        text(title)
            .font(font::UI_SEMIBOLD)
            .size(size::PANE_TITLE)
            .style(theme::text_primary),
        subtitle.into(),
    ]
    .spacing(6);

    let mut right = Row::new().spacing(8).align_y(Alignment::Center);
    for control in controls {
        right = right.push(control);
    }

    row![left, Space::new().width(Length::Fill), right]
        .align_y(Alignment::Center)
        .into()
}

/// A subtitle line whose middle clause carries the accent.
pub fn subtitle<'a, Message: 'a>(before: &str, accented: &str, after: &str) -> Element<'a, Message, Theme> {
    row![
        text(before.to_string())
            .font(font::UI)
            .size(size::BODY)
            .style(theme::text_secondary),
        text(accented.to_string())
            .font(font::UI)
            .size(size::BODY)
            .style(theme::text_accent),
        text(after.to_string())
            .font(font::UI)
            .size(size::BODY)
            .style(theme::text_secondary),
    ]
    .into()
}

/// A hairline. One pixel, drawn between list rows and nowhere else.
pub fn hairline<'a, Message: 'a>() -> Element<'a, Message, Theme> {
    rule::horizontal(1).style(theme::hairline).into()
}

/// A list row: label at the left, mono value at the right.
pub fn list_row<'a, Message: 'a>(
    label: &str,
    value: impl Into<Element<'a, Message, Theme>>,
) -> Element<'a, Message, Theme> {
    container(
        row![
            text(label.to_string())
                .font(font::UI)
                .size(size::BODY)
                .style(theme::text_secondary),
            Space::new().width(Length::Fill),
            value.into(),
        ]
        .align_y(Alignment::Center),
    )
    .padding([space::ROW_Y, space::CARD])
    .width(Length::Fill)
    .into()
}

/// The mono right-hand side of a list row — a path, a rate, a device id.
pub fn value<'a, Message: 'a>(v: &str) -> Element<'a, Message, Theme> {
    text(v.to_string())
        .font(font::DATA)
        .size(size::MONO)
        .style(theme::text_primary)
        .into()
}

/// An inset list panel: a mono header label, then hairline-separated rows.
pub fn inset_list<'a, Message: 'a>(
    heading: &str,
    rows: Vec<Element<'a, Message, Theme>>,
) -> Element<'a, Message, Theme> {
    let mut col = Column::new().push(container(micro_label(heading)).padding([space::ROW_Y, space::CARD]));
    // A hairline before every row, the heading included — the heading is a
    // row of the list, not a caption floating above it.
    for r in rows {
        col = col.push(hairline());
        col = col.push(r);
    }
    inset(col).into()
}

/// A pill button. `selected` is the accent state; a group of these is a
/// segmented choice, and only one of them may be selected.
pub fn pill<'a, Message: Clone + 'a>(
    label: &str,
    selected: bool,
    on_press: Message,
) -> Element<'a, Message, Theme> {
    button(
        text(label.to_string())
            .font(font::UI_MEDIUM)
            .size(size::BODY_SMALL),
    )
    .padding([6, 14])
    .on_press(on_press)
    .style(theme::pill(selected))
    .into()
}

/// A segmented choice: pills in a row, one of them accented.
pub fn segmented<'a, T, Message>(
    options: &'a [(T, &'a str)],
    current: &T,
    on_select: impl Fn(&T) -> Message + 'a,
) -> Element<'a, Message, Theme>
where
    T: PartialEq,
    Message: Clone + 'a,
{
    let mut r = Row::new().spacing(6);
    for (v, label) in options {
        r = r.push(pill(label, v == current, on_select(v)));
    }
    r.into()
}

/// A sidebar nav item. The 3px accent bar is a sibling quad, because a
/// container border cannot be applied to one edge only.
pub fn nav_item<'a, Message: Clone + 'a>(
    label: &'a str,
    active: bool,
    on_press: Message,
) -> Element<'a, Message, Theme> {
    let bar = container(Space::new())
        .width(3)
        .height(18)
        .style(move |_t: &Theme| container::Style {
            background: Some(iced::Background::Color(if active {
                color::ACCENT
            } else {
                Color::TRANSPARENT
            })),
            border: iced::border::rounded(radius::BAR),
            ..container::Style::default()
        });

    // The placeholder icon square the spec calls for: a small rounded rect,
    // not an icon. Icons are a later problem and a worse one.
    let glyph = container(Space::new())
        .width(15)
        .height(15)
        .style(move |_t: &Theme| container::Style {
            background: Some(iced::Background::Color(if active {
                color::ACCENT_FILL
            } else {
                Color {
                    a: 0.07,
                    ..Color::WHITE
                }
            })),
            border: iced::Border {
                color: if active {
                    color::ACCENT_BORDER
                } else {
                    color::BORDER
                },
                width: 1.0,
                radius: 4.5.into(),
            },
            ..container::Style::default()
        });

    row![
        bar,
        button(
            row![
                glyph,
                text(label)
                    .font(if active { font::UI_MEDIUM } else { font::UI })
                    .size(size::BODY),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        )
        .padding([7, 10])
        .width(Length::Fill)
        .on_press(on_press)
        .style(theme::nav(active)),
    ]
    .spacing(5)
    .align_y(Alignment::Center)
    .into()
}

/// The 214px left column: nav items above, a mono status block at the foot.
pub fn sidebar<'a, Message: 'a>(
    items: Vec<Element<'a, Message, Theme>>,
    footer: Vec<(&'a str, String)>,
) -> Element<'a, Message, Theme> {
    let mut nav = Column::new().spacing(2);
    for item in items {
        nav = nav.push(item);
    }

    let mut foot = Column::new().spacing(4);
    for (k, v) in footer {
        foot = foot.push(
            row![
                text(k)
                    .font(font::DATA)
                    .size(size::MICRO)
                    .style(theme::text_tertiary),
                Space::new().width(Length::Fill),
                text(v)
                    .font(font::DATA)
                    .size(size::MICRO)
                    .style(theme::text_secondary),
            ]
            .align_y(Alignment::Center),
        );
    }

    container(
        column![nav, Space::new().height(Length::Fill), foot]
            .spacing(space::BLOCK)
            .width(Length::Fill),
    )
    .width(space::SIDEBAR_W)
    .height(Length::Fill)
    .padding([space::PANE_Y, 12.0])
    .style(theme::sidebar)
    .into()
}

/// The content column to the right of the sidebar: 26/30 padding, 18 between
/// blocks.
pub fn content<'a, Message: 'a>(blocks: Vec<Element<'a, Message, Theme>>) -> Element<'a, Message, Theme> {
    let mut col = Column::new().spacing(space::BLOCK);
    for b in blocks {
        col = col.push(b);
    }
    container(col)
        .padding([space::PANE_Y, space::PANE_X])
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// A big number with a mono unit beside it — the one live value on a pane.
pub fn big_value<'a, Message: 'a>(number: &str, unit: &str, accented: bool) -> Element<'a, Message, Theme> {
    row![
        text(number.to_string())
            .font(Font {
                weight: iced::font::Weight::Medium,
                ..font::DATA
            })
            .size(size::BIG_NUMBER)
            .style(if accented {
                theme::text_accent
            } else {
                theme::text_primary
            }),
        text(unit.to_string())
            .font(font::DATA)
            .size(size::BODY_SMALL)
            .style(theme::text_tertiary),
    ]
    .spacing(5)
    .align_y(Alignment::End)
    .into()
}
