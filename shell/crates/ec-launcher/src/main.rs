// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-launcher` — type, pick, run.
//!
//! A centred overlay layer surface, spawned by a keybind, that filters the
//! installed applications and starts one. Like the bar, the toast stack and
//! the control center it is an ordinary Wayland client with no capability of
//! its own (ADR 0038): it reads `.desktop` files the session can already read
//! and spawns a child of itself, and when that is refused the refusal is shown
//! rather than worked around.

use ec_launcher::{app, conn, view};
use ec_ui::tokens::space;
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::settings::LayerShellSettings;

fn namespace() -> String {
    "ec-launcher".to_owned()
}

fn main() -> iced_layershell::Result {
    // `launcher.style "menu"`: the bar's start menu is the launcher, and
    // this keybind only asks for it. A bar that took the request is the whole
    // answer; with no bar listening, draw the centred launcher as always.
    // `--centered` is the bar starting us because it cannot show its menu
    // (folded, hidden): asking it again would bounce straight back.
    let centered = std::env::args().skip(1).any(|a| a == "--centered");
    let cfg = ec_launcher::conn::fetch_launcher_config();
    if !centered && cfg.menu_style && ec_launcher::conn::open_bar_menu() {
        return Ok(());
    }
    // The cursor belongs in the filter field before the human has typed: the
    // first keystroke after the keybind is part of the query.
    let boot = move || (app::App::with(cfg), iced::widget::operation::focus(app::INPUT_ID));
    // `launcher.centered.anchor`: nothing anchored is centred by the
    // compositor; top and bottom sit a pane's padding off that edge.
    let edge = space::PANE_Y as i32;
    let (anchor, margin) = match cfg.anchor {
        conn::Anchor::Center => (Anchor::empty(), (0, 0, 0, 0)),
        conn::Anchor::Top => (Anchor::Top, (edge, 0, 0, 0)),
        conn::Anchor::Bottom => (Anchor::Bottom, (0, 0, edge, 0)),
    };
    // Deliberately no `disable_clipboard()`, unlike the bar's menus: the
    // filter field is a place a human will paste into.
    let mut builder = iced_layershell::build_pattern::application(boot, namespace, app::update, view::view)
        .layer_settings(LayerShellSettings {
            anchor,
            layer: Layer::Overlay,
            // Fixed for the launcher's whole life — see
            // `view::surface_height`.
            size: Some((
                cfg.width,
                view::surface_height(ec_launcher::conn::show_key_hints(), cfg.rows),
            )),
            exclusive_zone: 0,
            margin,
            // A launcher is nothing but keyboard, and abyss hands an
            // on-demand layer the keyboard when it maps. On-demand rather
            // than exclusive so a click anywhere else takes it back, and the
            // focus loss closes us (`app::subscription`).
            keyboard_interactivity: KeyboardInteractivity::OnDemand,
            ..Default::default()
        })
        .style(view::style)
        .theme(|_: &app::App| ec_ui::theme::theme())
        .subscription(app::subscription)
        .antialiasing(true);
    for face in ec_ui::FONTS {
        builder = builder.font(*face);
    }
    builder.run()
}
