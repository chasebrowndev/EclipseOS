// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-cheatsheet` — the keybind cheat sheet.
//!
//! A centred layer surface, spawned by the bind that opens it. It answers one
//! question, what does the keyboard do right now, and goes away on Escape or
//! on a second press of the bind (see `instance`). An ordinary Wayland client
//! with no capability of its own (ADR 0038): it reads the config and draws it.

use ec_cheatsheet::{app, instance, view, HEIGHT, WIDTH};
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::settings::{LayerShellSettings, Settings};

fn namespace() -> String {
    "ec-cheatsheet".to_owned()
}

fn main() -> iced_layershell::Result {
    // A second press of the bind closes the open sheet and draws nothing.
    if !instance::claim() {
        return Ok(());
    }
    // Start on the compositor's theme (ADR 0084); the subscription keeps it there.
    ec_ui::ipc::sync_theme_now();
    // A reference sheet has no business holding the selection.
    iced_layershell::disable_clipboard();
    // eclipse_glass_v1: fail-soft, nothing changes under a non-abyss host.
    let connection = ec_ui::glass::setup(iced_layershell::set_surface_hook);

    let mut builder =
        iced_layershell::build_pattern::application(app::App::new, namespace, app::update, view::view)
            .settings(Settings {
                with_connection: connection.map(Into::into),
                ..Default::default()
            })
            .layer_settings(LayerShellSettings {
                // No anchor: the sheet is centred on the output.
                anchor: Anchor::empty(),
                layer: Layer::Overlay,
                size: Some((WIDTH, HEIGHT)),
                exclusive_zone: 0,
                margin: (0, 0, 0, 0),
                // Escape has to reach the sheet, so it takes the keyboard for
                // its short life. The compositor's own binds still run first.
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                ..Default::default()
            })
            .style(view::style)
            .theme(|_: &app::App| ec_ui::theme::theme())
            .subscription(app::subscription)
            .antialiasing(true);
    for face in ec_ui::FONTS {
        builder = builder.font(*face);
    }
    let result = builder.run();
    instance::release();
    result
}
