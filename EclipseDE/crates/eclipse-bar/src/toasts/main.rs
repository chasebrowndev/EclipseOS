// SPDX-License-Identifier: AGPL-3.0-only
//! `eclipse-toasts` — the notification stack, top-right.
//!
//! A layer surface of its own rather than a popup of the bar: its height
//! changes with what it holds, and it must be able to outlive a bar restart.
//! Nothing here is trusted UI — a notification is client-supplied text, and
//! anything the compositor has to vouch for it draws itself.

use eclipse_bar::toasts::{app, view, WIDTH};
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::settings::LayerShellSettings;

fn namespace() -> String {
    "eclipse-toasts".to_owned()
}

/// Gap between the bar's bottom edge and the first card.
const TOP_MARGIN: i32 = eclipse_bar::HEIGHT as i32 + 4;
/// Gap between the cards and the right edge of the output.
const RIGHT_MARGIN: i32 = 4;

fn main() -> iced_layershell::Result {
    // The stack renders client-supplied text and nothing else. It has no
    // reason to hold a selection, and not holding one is the cheapest way to
    // keep the no-logging-human-input rule true here.
    iced_layershell::disable_clipboard();

    let mut builder =
        iced_layershell::build_pattern::application(app::App::new, namespace, app::update, view::view)
            .layer_settings(LayerShellSettings {
                anchor: Anchor::Top | Anchor::Right,
                // Above the bar and above fullscreen windows: a notification the human
                // cannot see is a notification that did not happen.
                layer: Layer::Overlay,
                // Starts one pixel tall and is resized by `update` as cards arrive.
                size: Some((WIDTH, 1)),
                // Toasts never push windows around. The stack comes and goes several
                // times a minute, and a reflow each time would be unusable.
                exclusive_zone: 0,
                margin: (TOP_MARGIN, RIGHT_MARGIN, 0, 0),
                keyboard_interactivity: KeyboardInteractivity::None,
                ..Default::default()
            })
            .style(view::style)
            .theme(|_: &app::App| eclipse_ui::theme::theme())
            .subscription(app::subscription)
            .antialiasing(true);
    for face in eclipse_ui::FONTS {
        builder = builder.font(*face);
    }
    builder.run()
}
