// SPDX-License-Identifier: AGPL-3.0-only
//! `eclipse-welcome`: the welcome screen, standalone and full-screen.
//!
//! ```text
//! eclipse-welcome [--reduced-motion] [--version-label X.Y.Z]
//!                 [--size WxH] [--scale F] [--at SECONDS] [--fade 0..1]
//!                 [--shot FILE.png]
//! ```
//!
//! `--size` opens a window instead of going full-screen; `--at` freezes the
//! animation at that many animation-seconds (`--fade` pins the exit fade);
//! `--scale` multiplies the output's scale factor.
//!
//! `--shot` renders that one frame **offscreen** and writes it to a PNG: it
//! opens no window and never touches the session it is run from, so it is safe
//! to run on a desktop someone is using. `--size` is then the logical size and
//! `--scale` the scale factor, so `--size 1280x720 --scale 1.5` writes a
//! 1920x1080 image, laid out and resampled exactly as on a 1.5x output. It
//! needs a GPU adapter (any wgpu backend, `WGPU_BACKEND=gl` or a software
//! Vulkan will do) but no display. Together these exist so the screen can be
//! compared frame by frame against `reference/welcome.html`.
//! Exit status is 0 once the fade completes.

use eclipse_welcome::{reduced_motion_from_env, Message, Welcome};
use iced::advanced::renderer::Headless;
use iced::{window, Element, Font, Pixels, Size, Subscription, Task, Theme};
use std::path::PathBuf;

#[derive(Debug, Clone, Default)]
struct Args {
    reduced: bool,
    version: Option<String>,
    size: Option<(f32, f32)>,
    at: Option<f32>,
    fade: f32,
    /// Multiplies the output's scale factor: `0.5` on a 2x output renders at 1x.
    scale: Option<f32>,
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
            "--scale" => {
                out.scale = Some(value("--scale")?.parse().map_err(|_| "--scale: not a number")?);
            }
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
}

#[derive(Debug, Clone)]
enum Msg {
    Welcome(Message),
}

fn boot(args: &Args) -> (App, Task<Msg>) {
    let mut welcome = Welcome::new(args.version.as_deref().unwrap_or(Welcome::default_version()))
        .reduced_motion(args.reduced)
        .ui_scale(args.scale.unwrap_or(1.0));
    if let Some(t) = args.at {
        welcome = welcome.frozen(t, args.fade);
    }
    let app = App { welcome };
    (app, Welcome::probe().map(Msg::Welcome))
}

fn update(app: &mut App, msg: Msg) -> Task<Msg> {
    match msg {
        // The hand-off. Standalone, that is the end: exit 0.
        Msg::Welcome(Message::Begin) => iced::exit(),
        Msg::Welcome(m) => app.welcome.update(m).map(Msg::Welcome),
    }
}

fn view(app: &App) -> Element<'_, Msg> {
    app.welcome.view().map(Msg::Welcome)
}

fn subscription(app: &App) -> Subscription<Msg> {
    app.welcome.subscription().map(Msg::Welcome)
}

/// `--shot`: one frozen frame, rendered offscreen. No window is opened.
fn shoot(args: &Args, path: &PathBuf) -> Result<(), String> {
    let (w, h) = args.size.unwrap_or((1920.0, 1080.0));
    let scale = args.scale.unwrap_or(1.0);
    let mut welcome = Welcome::new(args.version.as_deref().unwrap_or(Welcome::default_version()))
        .reduced_motion(args.reduced)
        .frozen(args.at.unwrap_or(0.0), args.fade);
    let mut renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
        Font::DEFAULT,
        Pixels(16.0),
        None,
    ))
    .ok_or("no GPU adapter to render with")?;
    let size = Size::new(w, h);
    welcome.paint(&mut renderer, size, scale);
    let physical = Size::new((w * scale).round() as u32, (h * scale).round() as u32);
    let rgba = Headless::screenshot(&mut renderer, physical, scale, iced::Color::BLACK);
    image::save_buffer(
        path,
        &rgba,
        physical.width,
        physical.height,
        image::ColorType::Rgba8,
    )
    .map_err(|e| e.to_string())
}

fn main() -> iced::Result {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("eclipse-welcome: {e}");
            std::process::exit(2);
        }
    };
    if let Some(path) = &args.shot {
        if let Err(e) = shoot(&args, path) {
            eprintln!("eclipse-welcome: --shot: {e}");
            std::process::exit(1);
        }
        return Ok(());
    }
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
    let scale = args.scale.unwrap_or(1.0);
    iced::application(move || boot(&args), update, view)
        .title("Welcome to EclipseOS")
        .theme(|_: &App| Theme::Dark)
        .subscription(subscription)
        .window(settings)
        .scale_factor(move |_| scale)
        .antialiasing(true)
        .run()
}
