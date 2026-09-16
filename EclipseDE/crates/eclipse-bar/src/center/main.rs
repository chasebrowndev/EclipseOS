// SPDX-License-Identifier: AGPL-3.0-only
//! `eclipse-center` — the control center panel.
//!
//! A layer surface under the bar's right end: spawned by a keybind, it answers
//! one question and goes away. Like the bar and the toast stack it is an
//! ordinary Wayland client with no capability of its own (ADR 0038) — every
//! action it offers is one logind re-authorises through polkit, and a refusal
//! is shown rather than worked around.

use eclipse_bar::center::{app, view, WIDTH};
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::settings::LayerShellSettings;

fn namespace() -> String {
    "eclipse-center".to_owned()
}

/// Gap between the bar's bottom edge and the panel.
const TOP_MARGIN: i32 = eclipse_bar::HEIGHT as i32 + 4;
/// Gap between the panel and the right edge of the output.
const RIGHT_MARGIN: i32 = 4;

fn main() -> iced_layershell::Result {
    // A menu has no business holding the selection.
    iced_layershell::disable_clipboard();

    let mut builder =
        iced_layershell::build_pattern::application(app::App::new, namespace, app::update, view::view)
            .layer_settings(LayerShellSettings {
                anchor: Anchor::Top | Anchor::Right,
                layer: Layer::Overlay,
                // Fixed for the panel's whole life — see `view::surface_height`.
                size: Some((WIDTH, view::surface_height())),
                // The panel floats over what is on screen. Reflowing every
                // window for the few seconds a menu is open would be worse
                // than the menu.
                exclusive_zone: 0,
                margin: (TOP_MARGIN, RIGHT_MARGIN, 0, 0),
                // The panel does not read the keyboard, and must not take it
                // away from the window the human was using.
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
