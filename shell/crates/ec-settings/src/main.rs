// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-settings` — a plain xdg_toplevel. Not a layer surface: only the
//! bar and the OSD are.

use ec_settings::app::{self, App};
use ec_settings::pane::Page;

fn main() -> iced::Result {
    // `ec-settings [section[/page]]`: the taskbar's drawers open
    // `network`, and `desktop/wallpaper` opens that page. An unknown name
    // opens the default page rather than refusing to start.
    let page = std::env::args()
        .nth(1)
        .and_then(|a| Page::from_arg(&a))
        .unwrap_or(Page::Layout);
    let mut builder = iced::application(move || app::boot(page), app::update, app::view)
        .title("Eclipse Settings")
        .theme(|_: &App| ec_ui::theme::theme())
        // Transparent, so the compositor's blur shows through; the panes tint
        // it, opaque only when blur is off (`app::view`).
        .transparent(true)
        .style(ec_ui::theme::clear_window)
        .subscription(app::subscription)
        .window_size((1100.0, 760.0))
        .antialiasing(true);
    // Every face, registered once, before the window exists.
    for face in ec_ui::FONTS {
        builder = builder.font(*face);
    }
    builder.run()
}
