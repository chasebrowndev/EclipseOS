// SPDX-License-Identifier: AGPL-3.0-only
//! Debug-build fixtures for screenshotting the widget bar (ADR 0065) on a
//! machine with nothing behind it and nothing that can click.
//!
//! - `HYPERION_PREVIEW=widgets` — a strip of fixture windows and every
//!   widget, media playing; `widgets-idle` is the same with no player.
//! - `HYPERION_PREVIEW_CHIPS=3|8|20` — how many windows (default 8).
//! - `HYPERION_PREVIEW_SCRIPT=grow|drag|np-out` — a timed sequence, for frame
//!   captures: windows arriving one by one, a grip dragged open, the player
//!   stopping.
//! - `HYPERION_PREVIEW_SLOW=<n>` — every movement `n` times slower, so a
//!   screenshot loop catches the frames in between.
//!
//! A fixture bar neither refetches nor starts services; see `App::fixture`.

use std::time::{Duration, Instant};

use iced::widget::image;
use iced::{Color, Subscription};

use eclipse_services::custom::{Kind, Output, WidgetSpec};
use eclipse_services::media::{NowPlaying, Playback};
use eclipse_services::status::{Battery, Charge};
use eclipse_services::usage::Sample;
use eclipse_ui::tokens::{bar, color};

use crate::app::{App, Message};
use crate::model::{Snapshot, Trust, Window, Workspace};
use crate::widgets::{self, now_playing, system_usage, volume, GripEv, WidgetId};

/// Fixture windows: common apps, titles of every length.
const WINDOWS: [(&str, &str); 20] = [
    ("firefox", "Eclipse OS — design notes"),
    ("foot", "~/src/abyss"),
    ("org.gnome.Nautilus", "Downloads"),
    ("code", "layout.rs — hyperion"),
    ("spotify", "Spotify"),
    ("thunderbird", "Inbox"),
    ("org.telegram.desktop", "Telegram"),
    ("gimp", "cover.xcf"),
    ("blender", "scene.blend"),
    ("obs", "OBS 31"),
    ("steam", "Steam"),
    ("discord", "#general"),
    ("vlc", "VLC media player"),
    ("libreoffice-writer", "Quarterly report.odt"),
    ("org.kde.dolphin", "Home"),
    ("mpv", "talk.mkv"),
    ("krita", "sketch.kra"),
    ("inkscape", "logo.svg"),
    ("chromium", "Docs"),
    ("kitty", "htop"),
];

/// The windows a fixture starts with.
fn chips() -> usize {
    std::env::var("HYPERION_PREVIEW_CHIPS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(8)
        .min(WINDOWS.len())
}

fn window(i: usize, output: u64) -> Window {
    let (app_id, title) = WINDOWS[i];
    Window {
        handle: i as u64 + 1,
        app_id: app_id.to_owned(),
        title: title.to_owned(),
        workspace: Some(1),
        output: Some(output),
        focused: i == 0,
        // A few put away, so the ledger's "up" reads as a difference.
        minimized: i % 5 == 3,
        pid: None,
        trust: Trust::Private,
    }
}

fn track() -> NowPlaying {
    NowPlaying {
        player: "spotify".to_owned(),
        title: "Midnight City".to_owned(),
        artist: Some("M83".to_owned()),
        album: None,
        art: None,
        status: Playback::Playing,
        can_prev: true,
        can_next: true,
        can_pause: true,
    }
}

/// A cover drawn from the palette: the accent falling to the ground.
fn cover() -> image::Handle {
    let side = (bar::ART * 2.0) as u32;
    let mix = |a: Color, b: Color, t: f32| {
        [
            a.r + (b.r - a.r) * t,
            a.g + (b.g - a.g) * t,
            a.b + (b.b - a.b) * t,
        ]
    };
    let mut rgba = Vec::with_capacity((side * side * 4) as usize);
    for y in 0..side {
        for x in 0..side {
            let t = (x + y) as f32 / (2 * side) as f32;
            for c in mix(color::ACCENT, color::SURFACE_0, t) {
                rgba.push((c * 255.0).round() as u8);
            }
            rgba.push(u8::MAX);
        }
    }
    image::Handle::from_rgba(side, side, rgba)
}

/// Fill `app` with the widget fixture.
pub fn widgets(app: &mut App, idle: bool) {
    let now = Instant::now();
    app.fixture = Some(now);
    let output = app.output_id;
    let windows: Vec<Window> = (0..chips()).map(|i| window(i, output)).collect();
    app.snapshot = Snapshot {
        connected: true,
        workspaces: (1..=3)
            .map(|index| Workspace {
                index,
                output,
                output_name: String::new(),
                active: index == 1,
                windows: if index == 1 {
                    windows.len()
                } else {
                    usize::from(index == 2)
                },
            })
            .collect(),
        focused: windows.first().map(|w| w.handle),
        windows,
    };
    app.icons.warm(&app.snapshot.windows);
    app.battery = Some(Battery {
        percent: 76,
        state: Charge::Discharging,
        remaining: None,
    });

    let order = [
        "now-playing",
        "system-usage",
        "volume",
        "network",
        "bluetooth",
        "battery",
        "tray",
        "clock",
        "custom:weather",
    ];
    let cfg = &mut app.widget_cfg;
    cfg.order = order.iter().filter_map(|n| WidgetId::parse(n)).collect();
    cfg.custom = vec![WidgetSpec {
        name: "weather".to_owned(),
        kind: Kind::Exec {
            argv: vec!["true".to_owned()],
            interval: Duration::from_secs(600),
        },
        icon: Some("weather-few-clouds-symbolic".to_owned()),
        on_click: None,
        on_scroll_up: None,
        on_scroll_down: None,
    }];
    let slow: u32 = std::env::var("HYPERION_PREVIEW_SLOW")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(1);
    cfg.motion.duration *= slow.max(1);
    let motion = cfg.motion;
    app.motion.set_motion(motion);

    let state = &mut app.widgets;
    if !idle {
        widgets::update(
            state,
            widgets::Feed::NowPlaying(now_playing::Feed::Player(Some(track()))),
        );
        state.now_playing.set_art("fixture", cover());
    }
    widgets::update(
        state,
        widgets::Feed::Usage(system_usage::Feed::Sample(Sample {
            cpu: 0.23,
            mem: Some(0.58),
            gpu: Some(0.12),
            disk: Some(0.71),
        })),
    );
    widgets::update(
        state,
        widgets::Feed::Volume(volume::Feed::Sink(Some(eclipse_services::audio::Sink {
            volume: 0.64,
            muted: false,
            description: "Speakers".to_owned(),
        }))),
    );
    state.custom.entry("weather".to_owned()).or_default().out = Some(Output {
        text: "18°".to_owned(),
        detail: vec!["Clear".to_owned()],
        tooltip: None,
        state: None,
    });
}

/// One frame of a fixture: the visualizer, from the clock.
pub fn frame(app: &mut App, now: Instant) {
    let Some(t0) = app.fixture else {
        return;
    };
    if !app.widgets.now_playing.playing() {
        return;
    }
    let t = now.duration_since(t0).as_secs_f32();
    let mut levels = [0.0; bar::VIZ_BANDS];
    for (i, level) in levels.iter_mut().enumerate() {
        let f = i as f32;
        let fall = 1.0 - f / bar::VIZ_BANDS as f32 * 0.6;
        *level = ((0.5 + 0.5 * (t * (2.0 + f * 0.37) + f * 1.3).sin()) * fall).clamp(0.05, 1.0);
    }
    app.widgets.now_playing.levels = levels;
}

fn script_name() -> String {
    std::env::var("HYPERION_PREVIEW_SCRIPT").unwrap_or_default()
}

/// Presses on the drag script, and the travel per step.
const DRAG_STEPS: u32 = 40;
const DRAG_STEP_PX: f32 = 5.0;

/// The script's schedule: `(step, wait before it)`.
fn plan(name: &str) -> Vec<(u32, Duration)> {
    let ms = Duration::from_millis;
    match name {
        "grow" => (1..=(WINDOWS.len() - chips()) as u32)
            .map(|n| (n, ms(300)))
            .collect(),
        // Press, travel a frame-ish apart, release.
        "drag" => (1..=DRAG_STEPS + 2).map(|n| (n, ms(40))).collect(),
        "np-out" => vec![(1, ms(0))],
        _ => Vec::new(),
    }
}

/// The script's clock. Waits `HYPERION_PREVIEW_DELAY_MS` (default 4s) for
/// the surface to map, then walks the plan.
pub fn script() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(8, async move |mut sender| {
            std::thread::spawn(move || {
                let plan = plan(&script_name());
                if plan.is_empty() {
                    return;
                }
                let delay = std::env::var("HYPERION_PREVIEW_DELAY_MS")
                    .ok()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(4000);
                std::thread::sleep(Duration::from_millis(delay));
                for (n, wait) in plan {
                    std::thread::sleep(wait);
                    if !crate::app::send(&mut sender, Message::Script(n)) {
                        return;
                    }
                }
            });
        })
    })
}

/// One step of the script.
pub fn step(app: &mut App, n: u32) {
    let now = Instant::now();
    match script_name().as_str() {
        "grow" => {
            let next = app.snapshot.windows.len();
            if next < WINDOWS.len() {
                app.snapshot.windows.push(window(next, app.output_id));
                if let Some(ws) = app.snapshot.workspaces.first_mut() {
                    ws.windows = app.snapshot.windows.len();
                }
                app.icons.warm(&app.snapshot.windows);
            }
        }
        "drag" => {
            let key = WidgetId::NowPlaying.key();
            let ev = match n {
                1 => GripEv::Press,
                n if n <= DRAG_STEPS + 1 => GripEv::Drag(-((n - 1) as f32) * DRAG_STEP_PX),
                _ => GripEv::Release,
            };
            crate::app::grip(app, key, ev, now);
        }
        "np-out" => {
            widgets::update(
                &mut app.widgets,
                widgets::Feed::NowPlaying(now_playing::Feed::Player(None)),
            );
        }
        _ => {}
    }
}
