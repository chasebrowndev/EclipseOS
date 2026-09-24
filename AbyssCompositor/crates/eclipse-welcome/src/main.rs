// SPDX-License-Identifier: AGPL-3.0-only
//! `eclipse-welcome`: the welcome screen, standalone and full-screen.
//!
//! ```text
//! eclipse-welcome [--reduced-motion] [--version-label X.Y.Z]
//!                 [--size WxH] [--at SECONDS] [--fade 0..1] [--shot FILE.png]
//! ```
//!
//! `--size` opens a window instead of going full-screen; `--at` freezes the
//! animation at that many animation-seconds (`--fade` pins the exit fade);
//! `--shot` writes the rendered window to a PNG and exits. Those three exist so
//! the screen can be compared frame by frame against `reference/welcome.html`.
//! Exit status is 0 once the fade completes.

use eclipse_welcome::{reduced_motion_from_env, Message, Welcome};
use iced::{window, Element, Size, Subscription, Task, Theme};
use std::path::PathBuf;

/// How long `--shot` waits for the first frames to land before capturing.
const SHOT_DELAY_MS: u64 = 1500;

#[derive(Debug, Clone, Default)]
struct Args {
    reduced: bool,
    version: Option<String>,
    size: Option<(f32, f32)>,
    at: Option<f32>,
    fade: f32,
    shot: Option<PathBuf>,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut out = Args {
        reduced: reduced_motion_from_env(),
        ..Args::default()
    };
    let mut it = args;
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().ok_or(format!("{name} needs a value"));
        match a.as_str() {
            "--reduced-motion" => out.reduced = true,
            "--version-label" => out.version = Some(value("--version-label")?),
            "--at" => {
                out.at = Some(value("--at")?.parse().map_err(|_| "--at: not a number")?);
            }
            "--fade" => out.fade = value("--fade")?.parse().map_err(|_| "--fade: not a number")?,
            "--shot" => out.shot = Some(value("--shot")?.into()),
            "--size" => {
                let v = value("--size")?;
                let (w, h) = v.split_once('x').ok_or("--size: expected WxH")?;
                out.size = Some((
                    w.parse().map_err(|_| "--size: bad width")?,
                    h.parse().map_err(|_| "--size: bad height")?,
                ));
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(out)
}

struct App {
    welcome: Welcome,
    shot: Option<PathBuf>,
}

#[derive(Debug, Clone)]
enum Msg {
    Welcome(Message),
    Capture,
    Captured(window::Screenshot),
}

fn boot(args: &Args) -> App {
    let mut welcome = Welcome::new(args.version.as_deref().unwrap_or(Welcome::default_version()))
        .reduced_motion(args.reduced);
    if let Some(t) = args.at {
        welcome = welcome.frozen(t, args.fade);
    }
    App {
        welcome,
        shot: args.shot.clone(),
    }
}

fn update(app: &mut App, msg: Msg) -> Task<Msg> {
    match msg {
        // The hand-off. Standalone, that is the end: exit 0.
        Msg::Welcome(Message::Begin) => iced::exit(),
        Msg::Welcome(m) => app.welcome.update(m).map(Msg::Welcome),
        Msg::Capture => window::oldest().and_then(window::screenshot).map(Msg::Captured),
        Msg::Captured(shot) => {
            if let Some(path) = &app.shot {
                if let Err(e) = image::save_buffer(
                    path,
                    &shot.rgba,
                    shot.size.width,
                    shot.size.height,
                    image::ColorType::Rgba8,
                ) {
                    eprintln!("eclipse-welcome: --shot: {e}");
                }
            }
            iced::exit()
        }
    }
}

fn view(app: &App) -> Element<'_, Msg> {
    app.welcome.view().map(Msg::Welcome)
}

fn subscription(app: &App) -> Subscription<Msg> {
    let mut subs = vec![app.welcome.subscription().map(Msg::Welcome)];
    if app.shot.is_some() {
        subs.push(Subscription::run(|| {
            iced::stream::channel(1, async move |mut sender| {
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(SHOT_DELAY_MS));
                    let _ = sender.try_send(Msg::Capture);
                });
            })
        }));
    }
    Subscription::batch(subs)
}

fn main() -> iced::Result {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("eclipse-welcome: {e}");
            std::process::exit(2);
        }
    };
    let settings = match args.size {
        Some((w, h)) => window::Settings {
            size: Size::new(w, h),
            resizable: false,
            decorations: false,
            ..window::Settings::default()
        },
        None => window::Settings {
            fullscreen: true,
            decorations: false,
            ..window::Settings::default()
        },
    };
    iced::application(move || boot(&args), update, view)
        .title("Welcome to EclipseOS")
        .theme(|_: &App| Theme::Dark)
        .subscription(subscription)
        .window(settings)
        .antialiasing(true)
        .run()
}
