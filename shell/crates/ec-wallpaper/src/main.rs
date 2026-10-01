// SPDX-License-Identifier: AGPL-3.0-only
//! `eclipse-wallpaper` — the desktop background, one surface per output.
//!
//! Starts with no surface of its own and opens one per output the control
//! socket lists, opening and closing them as monitors come and go
//! (`app::reconcile`, the taskbar's pattern).

use eclipse_wallpaper::{app, view};
use iced_layershell::settings::{LayerShellSettings, StartMode};

fn namespace() -> String {
    "eclipse-wallpaper".to_owned()
}

fn main() -> iced_layershell::Result {
    // Nothing here reads or writes a selection.
    iced_layershell::disable_clipboard();

    iced_layershell::build_pattern::daemon(app::boot, namespace, app::update, view::view)
        .layer_settings(LayerShellSettings {
            // Every surface is opened by `app::open_surface` on its output.
            start_mode: StartMode::Background,
            ..Default::default()
        })
        .style(view::style)
        .theme(|_: &app::App, _: iced::window::Id| eclipse_ui::theme::theme())
        .subscription(app::subscription)
        .run()
}
