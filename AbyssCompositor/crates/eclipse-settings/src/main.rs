// SPDX-License-Identifier: AGPL-3.0-only
//! `eclipse-settings` — a plain xdg_toplevel. Not a layer surface: only the
//! bar and the OSD are.

use eclipse_settings::app::{self, App};
use eclipse_settings::pane::Pane;

fn main() -> iced::Result {
    // `eclipse-settings [pane]`: the taskbar's drawers open `network`. An
    // unknown name opens the default pane rather than refusing to start.
    let pane = std::env::args()
        .nth(1)
        .and_then(|a| Pane::from_arg(&a))
        .unwrap_or(Pane::Appearance);
    let mut builder = iced::application(move || app::boot(pane), app::update, app::view)
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
