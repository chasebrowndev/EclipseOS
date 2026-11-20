// SPDX-License-Identifier: AGPL-3.0-only
//! A preview of the login screen with a fake backend, for screenshots. Grants
//! the secret `demo`; opens no session.
//!
//! `PREVIEW_PROMPT=1` starts the form already prompting for `robin`, so the
//! focused field and its caret can be seen without typing.
//! `PREVIEW_LAYOUT=portrait` draws a tall, narrow clock tile.
//! `PREVIEW_LAYOUT=glances` draws a second layout with Battery and Wifi tiles
//! over fixed fake Glance data (an internal battery and two connected devices).
//!
//! `PREVIEW_DEVICES=n` (default 3) is how many batteries the fake Glance data
//! holds: 1, 3 or 6 show a lone gauge, one row, and a wrapped second row.
//!
//! Run it under a nested abyss, never on the host session:
//! `cargo run -p ec-greeter-ui --example preview`.

use std::path::PathBuf;
use std::time::Duration;

use ec_glance::{Battery, Charge, Wifi};
use ec_greeter::Settings;
use ec_greeter_ui::glance::Snapshot;
use ec_greeter_ui::{run_with, Auth, FakeAuth, Options};
use ec_layout::{Kind, Layout, Rect, Tile};

/// The shipped wallpaper, read from the repository's copy.
fn wallpaper() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../packaging/greetd/wallpaper.png")
}

/// Clock and form on the left; Glances to the right.
fn glances_layout() -> Layout {
    let t = |kind, x, y, w, h| Tile {
        kind,
        rect: Rect::new(x, y, w, h),
    };
    Layout {
        tiles: vec![
            t(Kind::Clock, 2, 1, 10, 3),
            t(Kind::Greeting, 2, 4, 10, 1),
            t(Kind::Users, 2, 5, 10, 4),
            t(Kind::Password, 2, 9, 10, 2),
            t(Kind::Power, 5, 12, 7, 1),
            t(Kind::Battery, 14, 1, 2, 2),
            t(Kind::Wifi, 14, 7, 5, 3),
        ],
    }
}

/// A tall, narrow clock beside the form: the portrait tile.
fn portrait_layout() -> Layout {
    let t = |kind, x, y, w, h| Tile {
        kind,
        rect: Rect::new(x, y, w, h),
    };
    Layout {
        tiles: vec![
            t(Kind::Clock, 2, 1, 5, 12),
            t(Kind::Users, 9, 1, 9, 6),
            t(Kind::Password, 9, 8, 9, 2),
        ],
    }
}

fn glances() -> Snapshot {
    let device = |name: &str, percent, charge| Battery {
        internal: false,
        name: name.to_owned(),
        percent,
        charge,
    };
    let all = [
        Battery {
            internal: true,
            name: "BAT0".to_owned(),
            percent: 72,
            charge: Charge::Discharging,
        },
        device("Mouse", 40, Charge::Discharging),
        device("Keyboard", 85, Charge::Charging),
        device("Headphones", 18, Charge::Discharging),
        device("Phone", 100, Charge::Full),
        device("Pad", 56, Charge::Discharging),
    ];
    let n = std::env::var("PREVIEW_DEVICES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(3)
        .min(all.len());
    Snapshot {
        batteries: all.into_iter().take(n).collect(),
        wifi: Some(Wifi { percent: 68 }),
    }
}

fn main() -> iced::Result {
    let mut settings = Settings::parse("").unwrap_or_default();
    let mut options = Options {
        shipped_wallpaper: wallpaper(),
        glance: None,
    };
    if std::env::var("PREVIEW_LAYOUT").as_deref() == Ok("glances") {
        settings.layout = Some(glances_layout());
        options.glance = Some(glances());
    }
    if std::env::var("PREVIEW_LAYOUT").as_deref() == Ok("portrait") {
        settings.layout = Some(portrait_layout());
    }
    let users = ["robin", "sam", "ada"];
    if std::env::var_os("PREVIEW_PROMPT").is_some() {
        let mut auth = FakeAuth::with_answer(&users, Duration::ZERO);
        auth.begin("robin");
        return run_with(settings, Box::new(auth), options);
    }
    run_with(settings, Box::new(FakeAuth::new(&users)), options)
}
