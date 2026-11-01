// SPDX-License-Identifier: AGPL-3.0-only
//! `hyperion` — the taskbar, one layer surface per output.
//!
//! One of the DE's handful of layer-shell clients (the toast stack and the OSD
//! are the others).
//! Trusted UI is compositor-drawn and never comes through here.
//!
//! One process draws every bar. It starts with no surface of its own and opens
//! a bar per output the control socket lists, closing and opening bars as
//! monitors come and go (see `app::reconcile`). The shared services and the
//! control-socket connection each exist once, whatever the number of screens.
//! The BlueZ pairing agent is not here: it is `ec-pairing` (ADR 0066).
//!
//! `--output <NAME>` pins the process to one bar on that connector and never
//! reconciles: a debug path, not how the session runs it.

use ec_hyperion_bar::{app, view};
use iced_layershell::settings::{LayerShellSettings, Settings, StartMode};

fn namespace() -> String {
    "hyperion".to_owned()
}

fn main() -> iced_layershell::Result {
    let mut args = std::env::args().skip(1);
    let pin = match args.next().as_deref() {
        Some("--output") => match args.next() {
            Some(name) => Some(name),
            None => {
                eprintln!("ec-hyperion-bar: --output needs a connector name");
                std::process::exit(2);
            }
        },
        None => None,
        Some(other) => {
            eprintln!("ec-hyperion-bar: unknown argument {other:?} (expected --output <NAME>)");
            std::process::exit(2);
        }
    };

    // The bar never reads or writes a selection. Human input is never logged
    // by content, and the simplest way to keep that true is not to hold a
    // clipboard at all.
    iced_layershell::disable_clipboard();

    // eclipse_glass_v1: bind the glass manager on a connection the layer-shell
    // runtime then shares, and learn each surface as it is created. Fail-soft:
    // under any other host, or when the compositor does not offer the global
    // to this client, nothing changes and the bar draws its own ground.
    let connection = ec_ui::glass::bootstrap();
    iced_layershell::set_surface_hook(ec_ui::glass::surface_created);

    let mut builder = iced_layershell::build_pattern::daemon(
        move || app::boot(pin.clone()),
        namespace,
        app::update,
        view::view,
    )
    .settings(Settings {
        with_connection: connection.map(Into::into),
        ..Default::default()
    })
    .layer_settings(LayerShellSettings {
        // No surface of its own: every bar is opened by `app::open_bar` on
        // the output it belongs to, and the process outlives a moment with
        // no monitors at all.
        start_mode: StartMode::Background,
        ..Default::default()
    })
    .style(view::style)
    .theme(|_: &app::App, _: iced::window::Id| ec_ui::theme::theme())
    .subscription(app::subscription)
    .antialiasing(true);
    // Every face, registered once, before the first surface exists.
    for face in ec_ui::FONTS {
        builder = builder.font(*face);
    }
    builder.run()
}
