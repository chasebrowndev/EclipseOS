// SPDX-License-Identifier: AGPL-3.0-only
//! `eclipse-settings` — a plain xdg_toplevel. Not a layer surface: only the
//! bar and the OSD are.

use eclipse_settings::app::{self, App};

fn main() -> iced::Result {
    let mut builder = iced::application(App::new, app::update, app::view)
        .title("Eclipse Settings")
        .theme(|_: &App| eclipse_ui::theme::theme())
        .subscription(app::subscription)
        .window_size((1100.0, 760.0))
        .antialiasing(true);
    // Every face, registered once, before the window exists.
    for face in eclipse_ui::FONTS {
        builder = builder.font(*face);
    }
    builder.run()
}
