// SPDX-License-Identifier: AGPL-3.0-only
//! The login layout editor on its own, for comparing its tiles with the real
//! greeter's (`ec-greeter-ui`'s `preview` example takes the same variables).
//!
//! `PREVIEW_TILES="clock 0 0 12 6/battery 12 0 6 4"` sets the layout (tiles
//! `kind x y w h` split by `/`), `PREVIEW_BAT` (`none|one|several`),
//! `PREVIEW_LINK` (`none|strong|weak`), `PREVIEW_CLOCK`
//! (`24h|12h|24h-date|12h-date`), `PREVIEW_LIGHT=1` the light theme and
//! `BUILDER_W` the window width, px, and `LOCK=1` the lock screen's editor
//! (compare with `ec-lock`'s `preview` example, which takes a layout file).
//!
//! Run it under a nested abyss, never on the host session.

use ec_layout::{Kind, Layout, Rect, Tile};
use iced::widget::container;
use iced::{Element, Length, Theme};

struct State {
    layout: Layout,
}

fn view(state: &State) -> Element<'_, Layout, Theme> {
    let light = std::env::var_os("PREVIEW_LIGHT").map(|_| ec_greeter::Theme {
        base: 0xfaf9f6,
        ink: 0x1a1610,
        accent: 0xf2c33c,
        accent_text: 0x7a5c00,
    });
    let clock = match std::env::var("PREVIEW_CLOCK").as_deref() {
        Ok("24h") => Some(ec_greeter::Clock::Time24),
        Ok("12h") => Some(ec_greeter::Clock::Time12),
        Ok("24h-date") => Some(ec_greeter::Clock::Date24),
        Ok("12h-date") => Some(ec_greeter::Clock::Date12),
        _ => None,
    };
    let bat = std::env::var("PREVIEW_BAT").unwrap_or_default();
    let link = std::env::var("PREVIEW_LINK").unwrap_or_default();
    if std::env::var_os("LOCK").is_some() {
        return container(ec_settings::login_screen::lock::preview_editor(
            &state.layout,
            clock,
            &bat,
            &link,
            |l| l,
        ))
        .width(Length::Fill)
        .height(Length::Fill)
        .into();
    }
    container(ec_settings::login_screen::preview_editor(
        &state.layout,
        light,
        clock,
        &bat,
        &link,
        |l| l,
    ))
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn update(state: &mut State, layout: Layout) {
    state.layout = layout;
}

fn main() -> iced::Result {
    if std::env::var_os("LOCK").is_some() && std::env::var_os("PREVIEW_LIGHT").is_some() {
        // The lock wears the desktop's palette.
        ec_ui::tokens::color::set_seeds(&ec_theme::LIGHT_SEEDS);
    }
    let layout = match std::env::var("PREVIEW_TILES") {
        Ok(spec) => Layout {
            tiles: spec
                .split('/')
                .filter_map(|t| {
                    let mut it = t.split_whitespace();
                    let kind = Kind::from_name(it.next()?)?;
                    let mut n = || it.next()?.parse::<i32>().ok();
                    Some(Tile {
                        kind,
                        rect: Rect::new(n()?, n()?, n()?, n()?),
                    })
                })
                .collect(),
        },
        Err(_) => Layout::default(),
    };
    let w = std::env::var("BUILDER_W")
        .ok()
        .and_then(|w| w.parse::<f32>().ok())
        .unwrap_or(1100.0);
    let mut app = iced::application(
        move || State {
            layout: layout.clone(),
        },
        update,
        view,
    )
    .title("login layout editor")
    .theme(|_: &State| ec_ui::theme::theme())
    .window_size((w, w * 0.56 + 140.0))
    .antialiasing(true);
    for face in ec_ui::FONTS {
        app = app.font(*face);
    }
    app.run()
}
