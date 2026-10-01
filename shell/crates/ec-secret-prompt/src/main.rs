// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-secret-prompt wifi <ssid>` | `bt <addr> pin|passkey|authorize`
//! | `bt <addr> confirm <passkey>` | `bt <addr> show <code>`.
//!
//! A plain xdg_toplevel, not a layer surface: windowrules — and with them the
//! `secret` sensitivity class — only match toplevels (ADR 0053). Fixed size,
//! so abyss floats it without a rule.

use ec_secret_prompt::app::{self, App};
use ec_secret_prompt::{Target, APP_ID};
use ec_ui::tokens::secret;
use iced::window;

fn main() -> iced::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let target = match Target::parse(&args) {
        Ok(t) => t,
        Err(usage) => {
            // The arguments name a network or a device, never the secret, so
            // echoing the usage line leaks nothing.
            eprintln!("{usage}");
            std::process::exit(2);
        }
    };

    let size = iced::Size::new(secret::W, secret::H);
    let mut builder = iced::application(move || App::new(target.clone()), app::update, app::view)
        .title(|app: &App| app.title().to_owned())
        .theme(|_: &App| ec_ui::theme::theme())
        .subscription(app::subscription)
        .window(window::Settings {
            size,
            min_size: Some(size),
            max_size: Some(size),
            resizable: false,
            decorations: false,
            platform_specific: window::settings::PlatformSpecific {
                application_id: APP_ID.to_owned(),
                ..Default::default()
            },
            ..window::Settings::default()
        })
        .antialiasing(true);
    for face in ec_ui::FONTS {
        builder = builder.font(*face);
    }
    builder.run()
}
