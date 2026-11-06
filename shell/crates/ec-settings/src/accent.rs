// SPDX-License-Identifier: AGPL-3.0-only
//! The accent control's strip: preset swatches, the current name, and a reset.
//!
//! `general.accent` is `""` (follow the theme) or an opaque `#rrggbb`. The
//! hex field and picker are the generic colour row the schema already renders
//! for the key; this strip sits under it as a row of round chips, a different
//! silhouette from the rows around it. Every control is a write of the key
//! through [`Message::Chose`]; the compositor serves the theme back and every
//! pane recolours on its `theme` event.

use iced::widget::{column, container, row, text, Space};
use iced::{Alignment, Color, Element, Length, Theme};

use ec_ui::theme;
use ec_ui::tokens::accent_presets::ALL;
use ec_ui::tokens::{font, size, space};
use ec_ui::widget::{pill, swatch_button};

use crate::app::Message;
use crate::schema::rgba;

/// The key every preset writes.
pub const KEY: &str = "general.accent";

/// The preset a stored value names, case-insensitively.
fn preset_of(value: &str) -> Option<&'static str> {
    ALL.iter()
        .find(|(_, hex)| hex.eq_ignore_ascii_case(value))
        .map(|(name, _)| *name)
}

/// What the strip says about the stored value.
fn caption(value: &str) -> String {
    if value.is_empty() {
        "Following the theme. Gold in the built-in themes.".to_owned()
    } else if let Some(name) = preset_of(value) {
        format!("{name}. Text in this colour is lifted automatically so it stays readable.")
    } else {
        format!("Custom {value}. Text in this colour is lifted automatically so it stays readable.")
    }
}

/// The strip under the accent row. `value` is the stored string.
pub fn strip<'a>(value: &str) -> Element<'a, Message, Theme> {
    let chips = ALL.iter().map(|&(_, hex)| {
        let [r, g, b, _] = rgba(hex).unwrap_or([0, 0, 0, 255]);
        swatch_button(
            Color::from_rgb8(r, g, b),
            hex.eq_ignore_ascii_case(value),
            Message::Chose(KEY.to_owned(), hex.to_owned()),
        )
    });
    container(
        column![
            row(chips)
                .push(Space::new().width(Length::Fill))
                .push(pill(
                    "Theme default",
                    false,
                    Message::Chose(KEY.to_owned(), String::new()),
                ))
                .spacing(space::CONTROL_GAP)
                .align_y(Alignment::Center),
            text(caption(value))
                .font(font::UI)
                .size(size::BODY_SMALL)
                .style(theme::text_tertiary),
        ]
        .spacing(space::ROW_Y),
    )
    .padding([0.0, space::CARD])
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captions_name_the_state() {
        assert!(caption("").starts_with("Following"));
        assert!(caption("#F2C33C").starts_with("Gold"));
        assert!(caption("#123456").starts_with("Custom #123456"));
    }
}
