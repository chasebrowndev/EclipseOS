// SPDX-License-Identifier: AGPL-3.0-only
//! `hyperion` — the taskbar, one layer surface per output.
//!
//! One of the DE's handful of layer-shell clients (the toast stack and the OSD
//! are the others).
//! Trusted UI is compositor-drawn and never comes through here.
//!
//! One process draws every bar. It starts with no surface of its own and opens
//! a bar per output the control socket lists, closing and opening bars as
//! monitors come and go (see `app::reconcile`). The shared services, the
//! control-socket connection and the BlueZ pairing agent each exist once,
//! whatever the number of screens.
//!
//! `--output <NAME>` pins the process to one bar on that connector and never
//! reconciles: a debug path, not how the session runs it.

use hyperion::{app, view};
use iced_layershell::settings::{LayerShellSettings, StartMode};

fn namespace() -> String {
    "hyperion".to_owned()
}

fn main() -> iced_layershell::Result {
    let mut args = std::env::args().skip(1);
    let pin = match args.next().as_deref() {
        Some("--output") => match args.next() {
            Some(name) => Some(name),
            None => {
                eprintln!("hyperion: --output needs a connector name");
                std::process::exit(2);
            }
        },
        None => None,
        Some(other) => {
            eprintln!("hyperion: unknown argument {other:?} (expected --output <NAME>)");
            std::process::exit(2);
        }
    };

    // The bar never reads or writes a selection. Human input is never logged
    // by content, and the simplest way to keep that true is not to hold a
    // clipboard at all.
    iced_layershell::disable_clipboard();
    hyperion::pairing::spawn();

    let mut builder = iced_layershell::build_pattern::daemon(
        move || app::boot(pin.clone()),
        namespace,
        app::update,
        view::view,
    )
    .layer_settings(LayerShellSettings {
        // No surface of its own: every bar is opened by `app::open_bar` on
        // the output it belongs to, and the process outlives a moment with
        // no monitors at all.
        start_mode: StartMode::Background,
        ..Default::default()
    })
    .style(view::style)
    .theme(|_: &app::App, _: iced::window::Id| eclipse_ui::theme::theme())
    .subscription(app::subscription)
    .antialiasing(true);
    // Every face, registered once, before the first surface exists.
    for face in eclipse_ui::FONTS {
        builder = builder.font(*face);
    }
    builder.run()
}
