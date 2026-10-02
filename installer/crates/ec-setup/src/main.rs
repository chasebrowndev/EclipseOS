// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-setup`: the graphical installer (D-07).
//!
//! ```text
//! ec-setup --install [--fake-helper[=fail-at-<stage>]]
//!               [--reduced-motion] [--size WxH] [--scale F]
//! ec-setup --reconfigure                    (not yet)
//! ec-setup --shot NAME FILE.png [--size WxH] [--scale F]
//! ```
//!
//! `--fake-helper` runs the whole flow against simulations of the install
//! helper, the network, the reboot and the compositor's config, so it needs no
//! root and changes nothing on the machine. It is never the default. Without
//! `--size` the window is full screen: on a desktop somebody is using, give it
//! a size, or use `--shot`, which renders one screen offscreen and opens no
//! window at all.

use ec_setup::app::{self, Options};
use ec_setup::{helper, shot};
use std::path::PathBuf;

enum Mode {
    Install,
    Reconfigure,
    Shot(String, PathBuf),
}

struct Args {
    /// Only what was asked for: `--shot` defaults to 1.0, a window to the app's own.
    scale: Option<f32>,
    mode: Mode,
    options: Options,
}

const USAGE: &str = "usage: ec-setup --install [--fake-helper[=fail-at-<stage>]] [--reduced-motion] [--size WxH] [--scale F]\n       ec-setup --reconfigure\n       ec-setup --shot NAME FILE.png [--size WxH] [--scale F]";

fn parse(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut mode = None;
    let mut options = Options::default();
    let mut scale = None;
    let mut it = args;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--install" => mode = Some(Mode::Install),
            "--reconfigure" => mode = Some(Mode::Reconfigure),
            "--reduced-motion" => options.reduced_motion = true,
            "--shot" => {
                let name = it.next().ok_or("--shot needs a screen name")?;
                let file = it.next().ok_or("--shot needs a file")?;
                mode = Some(Mode::Shot(name, file.into()));
            }
            "--size" => {
                let v = it.next().ok_or("--size needs WxH")?;
                let (w, h) = v.split_once('x').ok_or("--size: expected WxH")?;
                options.size = Some((
                    w.parse().map_err(|_| "--size: bad width")?,
                    h.parse().map_err(|_| "--size: bad height")?,
                ));
            }
            "--scale" => {
                scale = Some(
                    it.next()
                        .ok_or("--scale needs a number")?
                        .parse()
                        .map_err(|_| "--scale: not a number")?,
                );
                if !(0.25..=4.0).contains(&scale.unwrap_or(1.0)) {
                    return Err("--scale: out of range".into());
                }
            }
            a if a.starts_with("--fake-helper") => options.fake = Some(helper::parse_fake_arg(a)?),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if let Some(scale) = scale {
        options.scale = scale;
    }
    Ok(Args {
        scale,
        mode: mode.ok_or("say what to do: --install")?,
        options,
    })
}

fn main() -> iced::Result {
    let args = match parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("ec-setup: {e}\n{USAGE}");
            std::process::exit(2);
        }
    };
    match args.mode {
        Mode::Reconfigure => {
            eprintln!("ec-setup: --reconfigure is not built yet");
            std::process::exit(2);
        }
        Mode::Shot(name, file) => {
            let size = args.options.size.unwrap_or((1920.0, 1080.0));
            if let Err(e) = shot::render(&name, size, args.scale.unwrap_or(1.0), &file) {
                eprintln!("ec-setup: --shot: {e}");
                std::process::exit(1);
            }
            Ok(())
        }
        Mode::Install => app::run(args.options),
    }
}
