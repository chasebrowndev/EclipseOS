// SPDX-License-Identifier: AGPL-3.0-only
//! The selectable tile: a thumbnail well over a two-line caption.
//!
//! A widget and not a styled container for the reason `ec_ui`'s `chip` is: it
//! is an object you pick, with three looks that must not drift between the
//! source chooser and the screenshot-mode cards (rest, keyboard cursor,
//! selected), and its well takes an optional image handle so the daemon's
//! frames can replace the placeholder without any view changing. It lives in
//! this crate only because the portal is its sole user; it is built entirely
//! from `ec_ui` tokens and styles, and moves to `ec_ui::widget` if a second
//! crate wants it.
//!
//! Glassy, not glowy: selected is a brighter fill and a crisp light rim, and
//! the pane's one yellow lands on the caption. No halo, no gradient.

use iced::widget::image::Handle;
use iced::widget::{button, column, container, image, row, text, Space};
use iced::{Alignment, Background, Border, Element, Length, Theme};

use ec_ui::theme;
use ec_ui::tokens::{color, font, radius, size, space};

use crate::metrics;

/// What the placeholder in an empty well draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    Screen,
    Window,
    /// A region within a screen.
    Area,
}

pub struct TileSpec {
    pub glyph: Glyph,
    pub title: String,
    /// Second caption line, mono.
    pub caption: String,
    /// Mono line under the placeholder glyph (the resolution). Ignored once
    /// a thumbnail is present.
    pub well_note: Option<String>,
    pub thumb: Option<Handle>,
    pub well_h: f32,
    pub selected: bool,
    /// The keyboard is on this tile.
    pub cursor: bool,
}

/// A rounded outline of `w` x `h` with `inner` laid inside it.
fn outline<'a, M: 'a>(
    w: f32,
    h: f32,
    ink: iced::Color,
    inner: Element<'a, M, Theme>,
) -> Element<'a, M, Theme> {
    container(inner)
        .width(Length::Fixed(w))
        .height(Length::Fixed(h))
        .style(move |_t: &Theme| container::Style {
            border: Border {
                color: ink,
                width: metrics::GLYPH_STROKE,
                radius: radius::BADGE.into(),
            },
            ..container::Style::default()
        })
        .into()
}

fn bar<'a, M: 'a>(ink: iced::Color) -> Element<'a, M, Theme> {
    container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(metrics::GLYPH_BAR_H))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(ink)),
            ..container::Style::default()
        })
        .into()
}

/// The placeholder glyph for a kind of source: no hand-drawn art, only
/// rounded rects, as the style spec asks.
pub fn glyph<'a, M: 'a>(kind: Glyph, ink: iced::Color) -> Element<'a, M, Theme> {
    match kind {
        Glyph::Screen => outline(
            metrics::GLYPH_SCREEN_W,
            metrics::GLYPH_SCREEN_H,
            ink,
            Space::new().into(),
        ),
        Glyph::Window => outline(
            metrics::GLYPH_WINDOW_W,
            metrics::GLYPH_WINDOW_H,
            ink,
            bar(ink),
        ),
        Glyph::Area => outline(
            metrics::GLYPH_SCREEN_W,
            metrics::GLYPH_SCREEN_H,
            ink,
            container(outline(
                metrics::GLYPH_SCREEN_W * metrics::GLYPH_REGION,
                metrics::GLYPH_SCREEN_H * metrics::GLYPH_REGION,
                ink,
                Space::new().into(),
            ))
            .padding(metrics::GLYPH_INSET)
            .into(),
        ),
    }
}

/// One tile. Pressing it sends `on_press`; hover lifts the fill and rim.
pub fn source_tile<'a, M: Clone + 'a>(spec: TileSpec, on_press: M) -> Element<'a, M, Theme> {
    let TileSpec {
        glyph: kind,
        title,
        caption,
        well_note,
        thumb,
        well_h,
        selected,
        cursor,
    } = spec;

    let ink = if selected {
        color::TEXT_SECONDARY
    } else {
        color::TEXT_TERTIARY
    };

    let inside: Element<'a, M, Theme> = match thumb {
        Some(handle) => image(handle)
            .width(Length::Fill)
            .height(Length::Fill)
            .content_fit(iced::ContentFit::Cover)
            .into(),
        None => {
            let mut col = column![glyph(kind, ink)]
                .spacing(space::KEY_GAP)
                .align_x(Alignment::Center);
            if let Some(note) = well_note {
                col = col.push(
                    text(note)
                        .font(font::DATA)
                        .size(size::MICRO)
                        .style(theme::text_tertiary),
                );
            }
            container(col)
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .into()
        }
    };

    let well = container(inside)
        .width(Length::Fill)
        .height(Length::Fixed(well_h))
        .clip(true)
        .style(|_t: &Theme| container::Style {
            background: Some(Background::Color(color::LIFT_SOFT)),
            border: Border {
                color: color::HAIRLINE,
                width: space::HAIRLINE,
                radius: radius::CHIP.into(),
            },
            ..container::Style::default()
        });

    // The dot says "selected" besides the colour; unselected tiles reserve
    // its room so a pick never moves the title.
    let dot: Element<'a, M, Theme> = if selected {
        container(Space::new())
            .width(Length::Fixed(metrics::DOT))
            .height(Length::Fixed(metrics::DOT))
            .style(|_t: &Theme| container::Style {
                background: Some(Background::Color(color::TEXT)),
                border: iced::border::rounded(radius::PILL),
                ..container::Style::default()
            })
            .into()
    } else {
        Space::new()
            .width(Length::Fixed(metrics::DOT))
            .height(Length::Fixed(metrics::DOT))
            .into()
    };

    let heading = row![
        text(title)
            .font(font::UI_MEDIUM)
            .size(size::BODY_SMALL)
            .color(if selected {
                color::ACCENT_TEXT
            } else {
                color::TEXT
            })
            .wrapping(text::Wrapping::None),
        Space::new().width(Length::Fill),
        dot,
    ]
    .align_y(Alignment::Center);

    let body = column![
        well,
        column![
            heading,
            text(caption)
                .font(font::DATA)
                .size(size::MICRO)
                .style(theme::text_tertiary)
                .wrapping(text::Wrapping::None),
        ]
        .spacing(space::LINE_GAP),
    ]
    .spacing(metrics::TILE_GAP);

    button(body)
        .padding(metrics::TILE_PAD)
        .width(Length::Fill)
        .on_press(on_press)
        .style(move |_t: &Theme, status: button::Status| {
            let lit = matches!(status, button::Status::Hovered | button::Status::Pressed);
            let (fill, rim, width) = match (selected, cursor, lit) {
                (true, _, _) => (color::HIGHLIGHT, color::HIGHLIGHT_STRONG, metrics::RING),
                (false, true, true) => (color::LIFT_STRONG, color::TEXT_SECONDARY, space::HAIRLINE),
                (false, true, false) => (color::LIFT, color::TEXT_SECONDARY, space::HAIRLINE),
                (false, false, true) => (color::LIFT, color::BORDER_STRONG, space::HAIRLINE),
                (false, false, false) => (color::LIFT_SOFT, color::BORDER, space::HAIRLINE),
            };
            button::Style {
                background: Some(Background::Color(fill)),
                text_color: color::TEXT,
                border: Border {
                    color: rim,
                    width,
                    radius: radius::INSET.into(),
                },
                ..button::Style::default()
            }
        })
        .into()
}
