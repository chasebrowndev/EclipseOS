// SPDX-License-Identifier: AGPL-3.0-only

//! Small parts every region of the window draws the same way: the icon
//! stand-ins, the hover wash on anything clickable, the drop target's
//! ground and the drag ghost. Kept here, not inlined at each call site in
//! `view.rs`, because each is a contract across regions: a crumb that
//! answered the pointer differently from a column label, or a place that
//! took a drop differently from a folder row, would not read as the same
//! kind of thing.

use iced::widget::{button, container, row, Space};
use iced::{alignment, Background, Border, Color, Element, Length, Padding};

use crate::app::Message;
use crate::theme::{color, look, size};

/// What an icon stand-in stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    /// A folder row: a landscape rect in the warm neutral.
    Folder,
    /// Any other row: a portrait outline.
    File,
    /// A sidebar place: a small square.
    Place,
}

/// A small rounded rect where an icon would be (STYLE.md: "small rounded
/// rects stand in for icons", "no hand-drawn SVG icons"). A part because
/// the list, the sidebar and the ghost must agree on it: an icon theme
/// (FOG §Visual design, "Icons") replaces it in one place.
pub fn glyph<'a>(g: Glyph) -> Element<'a, Message> {
    let (w, h, fill, edge) = match g {
        Glyph::Folder => (
            size::GLYPH_LONG,
            size::GLYPH_SHORT,
            color::GLYPH_FOLDER,
            Color::TRANSPARENT,
        ),
        Glyph::File => (
            size::GLYPH_SHORT,
            size::GLYPH_LONG,
            color::GLYPH_FILE,
            color::GLYPH_EDGE,
        ),
        Glyph::Place => (
            size::PLACE_GLYPH,
            size::PLACE_GLYPH,
            color::GLYPH_FILE,
            color::GLYPH_EDGE,
        ),
    };
    container(Space::new().width(w).height(h))
        .style(move |_| container::Style {
            background: Some(Background::Color(fill)),
            border: Border {
                color: edge,
                width: if edge.a > 0.0 { size::HAIRLINE } else { 0.0 },
                radius: size::GLYPH_R.into(),
            },
            ..Default::default()
        })
        .into()
}

/// Something clickable that is not a row: a crumb, a column label, a tray
/// or trash-bar action, a sheet's button. At rest it is its content; under
/// the pointer it takes the hover wash (tokens `LIFT_SOFT`), pressed a
/// step more. Never gold: the pointer is not state.
///
/// A part because iced's `button` brings its own filled look, and the
/// wash, its radius and its padding have to match the rows' own.
pub fn hover<'a>(
    content: impl Into<Element<'a, Message>>,
    on_press: Message,
    pad: impl Into<Padding>,
) -> button::Button<'a, Message> {
    let r = look().chip_radius;
    button(content)
        .padding(pad)
        .on_press(on_press)
        .style(move |_, status| {
            let fill = match status {
                button::Status::Hovered => color::HOVER,
                button::Status::Pressed => color::MARK_FILL,
                _ => Color::TRANSPARENT,
            };
            button::Style {
                background: Some(Background::Color(fill)),
                border: Border {
                    radius: r.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        })
}

/// A drop target with the drag over it: a lifted fill with a secondary
/// white hairline (`eclipse-ui`'s `drop_well` when hot, Settings' taskbar
/// pane). Targets at rest draw as they are. Never yellow: a target is a
/// place, not the pane's live value.
pub fn drop_well() -> container::Style {
    container::Style {
        background: Some(Background::Color(color::MARK_FILL)),
        border: Border {
            color: color::TEXT_SECONDARY,
            width: size::HAIRLINE,
            radius: look().chip_radius.into(),
        },
        ..Default::default()
    }
}

/// The thing in hand: its glyph and what it is, on the nearly opaque menu
/// ground with a strong edge (`eclipse-ui`'s `drag_ghost`), so it stays
/// legible over rows, places or the desktop showing through.
pub fn ghost<'a>(g: Glyph, what: String) -> Element<'a, Message> {
    let r = look().chip_radius;
    container(
        row![
            glyph(g),
            iced::widget::text(what)
                .size(size::TEXT)
                .color(color::TEXT)
                .wrapping(iced::widget::text::Wrapping::None),
        ]
        .spacing(size::GLYPH_GAP)
        .align_y(alignment::Vertical::Center),
    )
    .padding([0.0, size::PAD_X])
    .height(size::GHOST_H)
    .max_width(size::GHOST_MAX_W)
    .clip(true)
    .align_y(alignment::Vertical::Center)
    .style(move |_| container::Style {
        background: Some(Background::Color(color::MENU_GROUND)),
        border: Border {
            color: color::BORDER_STRONG,
            width: size::HAIRLINE,
            radius: r.into(),
        },
        ..Default::default()
    })
    .width(Length::Shrink)
    .into()
}
