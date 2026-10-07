// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-console` — a plain xdg_toplevel with app id `eclipse-console`, which the
//! shipped policy hides from every agent (A-08 §10).

use ec_console::app::{self, App};
use ec_console::APP_ID;
use ec_ui::tokens::console;

fn window_size() -> (f32, f32) {
    #[cfg(debug_assertions)]
    if let Some(s) = ec_console::fixture::size() {
        return s;
    }
    (console::WINDOW_W, console::WINDOW_H)
}

fn main() -> iced::Result {
    let (w, h) = window_size();
    let mut builder = iced::application(app::boot, app::update, app::view)
        .title("Eclipse Console")
        .theme(|_: &App| ec_ui::theme::theme())
        // Transparent, so the compositor's blur shows through; the panes tint
        // it, opaque only when blur is off (`view::view`).
        .transparent(true)
        .style(ec_ui::theme::clear_window)
        .subscription(app::subscription)
        .window(iced::window::Settings {
            size: iced::Size::new(w, h),
            platform_specific: iced::window::settings::PlatformSpecific {
                application_id: APP_ID.to_owned(),
                ..Default::default()
            },
            // The protected surface is released on the close request, before
            // the window goes (`shield::Shield::detach`).
            exit_on_close_request: false,
            ..Default::default()
        })
        .antialiasing(true);
    // Every face, registered once, before the window exists.
    for face in ec_ui::FONTS {
        builder = builder.font(*face);
    }
    builder.run()
}
