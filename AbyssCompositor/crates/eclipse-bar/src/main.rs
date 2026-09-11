// SPDX-License-Identifier: AGPL-3.0-only
//! `eclipse-bar` — a layer surface anchored to the top edge.
//!
//! One of exactly two layer-shell clients in the DE (the other is the OSD).
//! Trusted UI is compositor-drawn and never comes through here.

use eclipse_bar::{app, view, HEIGHT};
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::settings::LayerShellSettings;

fn namespace() -> String {
    "eclipse-bar".to_owned()
}

fn main() -> iced_layershell::Result {
    // The bar never reads or writes a selection. Human input is never logged
    // by content, and the simplest way to keep that true is not to hold a
    // clipboard at all.
    iced_layershell::disable_clipboard();

    let mut builder =
        iced_layershell::build_pattern::application(app::App::new, namespace, app::update, view::view)
            .layer_settings(LayerShellSettings {
                anchor: Anchor::Top | Anchor::Left | Anchor::Right,
                layer: Layer::Top,
                // Width 0 means "as wide as the output"; the height is also
                // the exclusive zone, so a window opened afterwards starts
                // below the bar rather than under it.
                size: Some((0, HEIGHT)),
                exclusive_zone: HEIGHT as i32,
                // The bar is pointer-driven. It must never take the keyboard
                // away from the window the human is typing into.
                keyboard_interactivity: KeyboardInteractivity::None,
                ..Default::default()
            })
            .style(view::style)
            .theme(|_: &app::App| eclipse_ui::theme::theme())
            .subscription(app::subscription)
            .antialiasing(true);
    // Every face, registered once, before the surface exists.
    for face in eclipse_ui::FONTS {
        builder = builder.font(*face);
    }
    builder.run()
}
