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
//! `PREVIEW_TILES="clock 0 0 12 6/battery 12 0 6 4"` sets an exact layout (tiles
//! `kind x y w h` split by `/`; keep Users and Password in it), `PREVIEW_BAT`
//! (`none|one|several`) and `PREVIEW_LINK` (`none|strong|weak`) the Glance data,
//! `PREVIEW_CLOCK` (`24h|12h|24h-date|12h-date`) the clock preset, and
//! `PREVIEW_LIGHT=1` the light palette. These are for comparing against the
//! layout builder's preview.
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
    if let Ok(spec) = std::env::var("PREVIEW_TILES") {
        let tiles: Vec<Tile> = spec
            .split('/')
            .filter_map(|t| {
                let mut it = t.split_whitespace();
                let kind = Kind::from_name(it.next()?)?;
                let mut n = || it.next()?.parse::<i32>().ok();
                Some(Tile {
                    kind,
                    rect: Rect::new(n()?, n()?, n()?, n()?),
                })
            })
            .collect();
        settings.layout = Some(Layout { tiles });
    }
    let dev = |name: &str, percent, charge| Battery {
        internal: false,
        name: name.to_owned(),
        percent,
        charge,
    };
    let internal = Battery {
        internal: true,
        name: "BAT0".to_owned(),
        percent: 72,
        charge: Charge::Discharging,
    };
    let batteries = match std::env::var("PREVIEW_BAT").as_deref() {
        Ok("none") => Some(Vec::new()),
        Ok("one") => Some(vec![internal]),
        Ok("several") => Some(vec![
            internal,
            dev("Logitech MX Master 3S", 40, Charge::Discharging),
            dev("Keychron K3", 100, Charge::Charging),
        ]),
        _ => None,
    };
    let wifi = match std::env::var("PREVIEW_LINK").as_deref() {
        Ok("none") => Some(None),
        Ok("strong") => Some(Some(Wifi { percent: 92 })),
        Ok("weak") => Some(Some(Wifi { percent: 24 })),
        _ => None,
    };
    if batteries.is_some() || wifi.is_some() {
        let base = options.glance.take().unwrap_or_default();
        options.glance = Some(Snapshot {
            batteries: batteries.unwrap_or(base.batteries),
            wifi: wifi.unwrap_or(base.wifi),
        });
    }
    if let Ok(c) = std::env::var("PREVIEW_CLOCK") {
        settings.clock = match c.as_str() {
            "24h" => Some(ec_greeter::Clock::Time24),
            "12h" => Some(ec_greeter::Clock::Time12),
            "24h-date" => Some(ec_greeter::Clock::Date24),
            "12h-date" => Some(ec_greeter::Clock::Date12),
            _ => None,
        };
    }
    if std::env::var_os("PREVIEW_LIGHT").is_some() {
        settings.theme = Some(ec_greeter::Theme {
            base: 0xfaf9f6,
            ink: 0x1a1610,
            accent: 0xf2c33c,
            accent_text: 0x7a5c00,
        });
    }
    let users = ["robin", "sam", "ada"];
    if std::env::var_os("PREVIEW_PROMPT").is_some() {
        let mut auth = FakeAuth::with_answer(&users, Duration::ZERO);
        auth.begin("robin");
        return run_with(settings, Box::new(auth), options);
    }
    run_with(settings, Box::new(FakeAuth::new(&users)), options)
}
