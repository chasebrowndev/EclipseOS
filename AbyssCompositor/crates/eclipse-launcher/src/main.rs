// SPDX-License-Identifier: AGPL-3.0-only
//! `eclipse-launcher` — type, pick, run.
//!
//! A centred overlay layer surface, spawned by a keybind, that filters the
//! installed applications and starts one. Like the bar, the toast stack and
//! the control center it is an ordinary Wayland client with no capability of
//! its own (ADR 0038): it reads `.desktop` files the session can already read
//! and spawns a child of itself, and when that is refused the refusal is shown
//! rather than worked around.

use eclipse_launcher::{app, view, WIDTH};
use iced::Task;
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::settings::LayerShellSettings;

fn namespace() -> String {
    "eclipse-launcher".to_owned()
}

/// The cursor belongs in the filter field before the human has typed: the
/// first keystroke after the keybind is part of the query.
fn boot() -> (app::App, Task<app::Message>) {
    (app::App::new(), iced::widget::operation::focus(app::INPUT_ID))
}

fn main() -> iced_layershell::Result {
    // `bar.launcher-style "menu"`: the bar's start menu is the launcher, and
    // this keybind only asks for it. A bar that took the request is the whole
    // answer; with no bar listening, draw the centred launcher as always.
    // `--centered` is the bar starting us because it cannot show its menu
    // (folded, hidden): asking it again would bounce straight back.
    let centered = std::env::args().skip(1).any(|a| a == "--centered");
    if !centered && eclipse_launcher::conn::menu_style() && eclipse_launcher::conn::open_bar_menu() {
        return Ok(());
    }
    // Deliberately no `disable_clipboard()`, unlike the bar's menus: the
    // filter field is a place a human will paste into.
    let mut builder = iced_layershell::build_pattern::application(boot, namespace, app::update, view::view)
        .layer_settings(LayerShellSettings {
            // Anchored to nothing, which the compositor centres.
            anchor: Anchor::empty(),
            layer: Layer::Overlay,
            // Fixed for the launcher's whole life — see
            // `view::surface_height`.
            size: Some((WIDTH, view::surface_height())),
            exclusive_zone: 0,
            margin: (0, 0, 0, 0),
            // Unlike the other surfaces, a launcher is nothing but
            // keyboard: it must take it, and give it back on exit.
            keyboard_interactivity: KeyboardInteractivity::Exclusive,
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
