// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-desktop` — the desktop's icons, one surface per output.
//!
//! Starts with no surface of its own and opens one per output the control
//! socket lists (`app::reconcile`, the wallpaper's pattern). Run by
//! `ec-desktop.service`, which abyss starts only under `mode "de"`.

use ec_desktop::{app, view};
use iced_layershell::settings::{LayerShellSettings, StartMode};

fn namespace() -> String {
    "ec-desktop".to_owned()
}

fn main() -> iced_layershell::Result {
    // Start on the compositor's theme (ADR 0084); the subscription keeps it there.
    ec_ui::ipc::sync_theme_now();
    // Nothing here reads or writes a selection.
    iced_layershell::disable_clipboard();

    let mut builder = iced_layershell::build_pattern::daemon(app::boot, namespace, app::update, view::view)
        .layer_settings(LayerShellSettings {
            // Every surface is opened by `app::open_surface` on its output.
            start_mode: StartMode::Background,
            ..Default::default()
        })
        .style(view::style)
        .theme(|_: &app::App, _: iced::window::Id| ec_ui::theme::theme())
        .subscription(app::subscription);
    // Every face, registered once, before the first surface exists.
    for face in ec_ui::FONTS {
        builder = builder.font(*face);
    }
    builder.run()
}
