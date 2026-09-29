// SPDX-License-Identifier: AGPL-3.0-only
//! What a wallpaper surface draws: its colour edge to edge, and the picture on
//! it when there is one. No chrome, no text, no effect.
//!
//! Accent ledger: none. A wallpaper is not a pane and carries no state.

use iced::widget::{container, image, Space};
use iced::window::Id;
use iced::{Color, Element, Length, Size, Theme};

use crate::app::{App, Message, Picture};
use crate::config::Mode;

pub fn view(app: &App, id: Id) -> Element<'_, Message, Theme> {
    let Some(surface) = app.surfaces.get(&id) else {
        return Space::new().into();
    };
    let spec = app.config.for_output(&surface.output);
    let content: Element<'_, Message, Theme> = match spec.path.as_deref().and_then(|p| app.picture(p)) {
        Some(picture) => picture_of(picture, spec.mode, surface.size),
        None => Space::new().into(),
    };
    let color = spec.color;
    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_: &Theme| container::Style {
            background: Some(color.into()),
            ..container::Style::default()
        })
        .into()
}

/// The picture laid over the whole surface.
///
/// `center` wants one image pixel per logical pixel, centred. iced's
/// `ContentFit::None` pins the image to the top-left of its bounds instead,
/// so `center` is `Contain` — which does centre — scaled back by the factor
/// `Contain` applied. Until the surface's size is known that factor is 1.
fn picture_of(picture: &Picture, mode: Mode, surface: Option<Size>) -> Element<'_, Message, Theme> {
    let unscale = match (mode, surface) {
        (Mode::Center, Some(s)) if picture.size.width > 0.0 && picture.size.height > 0.0 => {
            let contain = (s.width / picture.size.width).min(s.height / picture.size.height);
            if contain > 0.0 {
                1.0 / contain
            } else {
                1.0
            }
        }
        _ => 1.0,
    };
    image(picture.handle.clone())
        .width(Length::Fill)
        .height(Length::Fill)
        .content_fit(mode.content_fit())
        .scale(unscale)
        .into()
}

/// The runtime's own clear under every surface. Transparent: the container
/// paints the configured colour, which differs per output.
pub fn style(_app: &App, theme: &Theme) -> iced::theme::Style {
    iced::theme::Style {
        background_color: Color::TRANSPARENT,
        text_color: theme.palette().text,
    }
}
