// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-toasts` — the notification stack, top-right.
//!
//! A layer surface of its own rather than a popup of the bar: its height
//! changes with what it holds, and it must be able to outlive a bar restart.
//! Nothing here is trusted UI — a notification is client-supplied text, and
//! anything the compositor has to vouch for it draws itself.

use ec_toasts::{app, view};
use iced_layershell::settings::{LayerShellSettings, StartMode};

fn namespace() -> String {
    "ec-toasts".to_owned()
}

fn main() -> iced_layershell::Result {
    // The stack renders client-supplied text and nothing else. It has no
    // reason to hold a selection, and not holding one is the cheapest way to
    // keep the no-logging-human-input rule true here.
    iced_layershell::disable_clipboard();

    let mut builder =
        iced_layershell::build_pattern::daemon(app::App::new, namespace, app::update, view::view)
            .layer_settings(LayerShellSettings {
                // No surface until there is something to show: `app::update`
                // opens the stack with the first card and closes it with the
                // last. Even a transparent pixel of it would be blurred by the
                // compositor into a line across the corner (BLUR-02).
                start_mode: StartMode::Background,
                ..Default::default()
            })
            .style(view::style)
            .theme(|_: &app::App, _: iced::window::Id| ec_ui::theme::theme())
            .subscription(app::subscription)
            .antialiasing(true);
    for face in ec_ui::FONTS {
        builder = builder.font(*face);
    }
    builder.run()
}
