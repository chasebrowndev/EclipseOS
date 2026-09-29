// SPDX-License-Identifier: AGPL-3.0-only
//! `--shot NAME FILE.png`: one screen rendered offscreen.
//!
//! It opens no window and never touches the session it runs from, so it is safe
//! on a desktop somebody is using; it needs a GPU adapter but no display. Every
//! screen is built from fixture data (the fake disks, the fake network, a made
//! up identity), never from the machine, so the same command draws the same
//! frame anywhere. The fixtures are driven through [`Model::update`], the same
//! door the real app uses, so a screenshot cannot show a state the wizard cannot
//! reach.

use crate::choices::{BarPosition, Blur, Choices};
use crate::helper::{fake_disks, HelperEvent};
use crate::model::{Inputs, Message, Model, Secret, Step, ZoneClock};
use crate::net::fake_snapshot;
use eclipse_setup_plan::{Profile, Progress, Stage};
use eclipse_ui::theme;
use eclipse_ui::tokens::color;
use eclipse_welcome::{timeline, Welcome};
use iced::advanced::clipboard::Null;
use iced::advanced::renderer::{Headless, Style};
use iced::{mouse, window, Font, Pixels, Size};
use iced_runtime::user_interface::{Cache, UserInterface};
use std::borrow::Cow;
use std::path::Path;

/// Every name `--shot` takes.
pub const NAMES: &[&str] = &[
    "welcome",
    "language",
    "keyboard",
    "keyboard-more",
    "timezone",
    "timezone-search",
    "timezone-none",
    "network",
    "network-more",
    "network-offline",
    "network-join",
    "disk",
    "disk-empty",
    "disk-none",
    "disk-more",
    "identity",
    "identity-invalid",
    "profile",
    "profile-more",
    "mode",
    "layout",
    "components",
    "components-more",
    "appearance",
    "appearance-changed",
    "apps",
    "apps-full",
    "review",
    "review-armed",
    "install",
    "install-confirm",
    "install-done",
    "install-failed",
];

fn walk(m: &mut Model, to: Step, snapshot_online: bool) {
    m.update(Message::Begin);
    loop {
        match m.step {
            Step::Keyboard => {
                m.update(Message::KbTest("Grüße, wörld".into()));
                m.update(Message::ConfigResult(crate::config::KB_LAYOUT, Ok(())));
            }
            Step::Timezone => {
                m.update(Message::ZoneClock(
                    m.zone.clone(),
                    Some(ZoneClock {
                        time: "14:32".into(),
                        offset: "+0200".into(),
                    }),
                ));
            }
            Step::Network => {
                m.update(Message::NetScanned(fake_snapshot(snapshot_online)));
            }
            Step::Disk => {
                m.update(Message::DisksLoaded(Ok(fake_disks())));
                m.update(Message::SelectDisk(fake_disks()[0].by_id.clone()));
            }
            Step::Identity => {
                m.update(Message::Username("chase".into()));
                m.update(Message::Password(Secret::new("correct horse".into())));
                m.update(Message::Password2(Secret::new("correct horse".into())));
            }
            _ => {}
        }
        if m.step == to {
            break;
        }
        m.update(Message::Next);
    }
}

fn progress(m: &mut Model, stage: Stage, pct: u8, msg: &str, failed: bool) {
    m.update(Message::Helper(HelperEvent::Progress(Progress {
        stage,
        pct,
        msg: msg.into(),
        failed,
    })));
}

/// The model for a named screen, or `None` for a name that is not one.
pub fn fixture(name: &str) -> Option<Model> {
    let mut m = Model::new(Inputs::builtin(true));
    match name {
        "welcome" => {}
        "language" => walk(&mut m, Step::Language, true),
        "keyboard" => walk(&mut m, Step::Keyboard, true),
        "keyboard-more" => {
            walk(&mut m, Step::Keyboard, true);
            m.update(Message::More(true));
        }
        "timezone" => walk(&mut m, Step::Timezone, true),
        "timezone-search" => {
            walk(&mut m, Step::Timezone, true);
            m.update(Message::ZoneQuery("us eastern".into()));
            m.update(Message::ZoneClock(
                m.zone.clone(),
                Some(ZoneClock {
                    time: "08:32".into(),
                    offset: "-0400".into(),
                }),
            ));
        }
        "timezone-none" => {
            walk(&mut m, Step::Timezone, true);
            m.update(Message::ZoneQuery("atlantis".into()));
        }
        "network" => walk(&mut m, Step::Network, true),
        "network-more" => {
            walk(&mut m, Step::Network, true);
            m.update(Message::More(true));
        }
        "network-offline" => walk(&mut m, Step::Network, false),
        "network-join" => {
            walk(&mut m, Step::Network, false);
            m.update(Message::SelectAp("Lighthouse".into()));
            m.update(Message::Passphrase(Secret::new("sunlight-and-salt".into())));
        }
        "disk" => walk(&mut m, Step::Disk, true),
        "disk-empty" => {
            walk(&mut m, Step::Disk, true);
            m.update(Message::SelectDisk(fake_disks()[1].by_id.clone()));
        }
        "disk-none" => {
            walk(&mut m, Step::Disk, true);
            m.update(Message::DisksLoaded(Ok(vec![])));
        }
        "disk-more" => {
            walk(&mut m, Step::Disk, true);
            m.update(Message::More(true));
        }
        "identity" => {
            walk(&mut m, Step::Identity, true);
            m.update(Message::Hostname("lighthouse".into()));
        }
        "identity-invalid" => {
            walk(&mut m, Step::Identity, true);
            m.update(Message::Username("chase".into()));
            m.update(Message::Password(Secret::new("correct horse".into())));
            m.update(Message::Password2(Secret::new("correct hors".into())));
        }
        "profile" => walk(&mut m, Step::Profile, true),
        "profile-more" => {
            walk(&mut m, Step::Profile, true);
            m.update(Message::More(true));
        }
        "mode" => walk(&mut m, Step::Mode, true),
        "layout" => walk(&mut m, Step::Layout, true),
        "components" => walk(&mut m, Step::Components, true),
        "components-more" => {
            walk(&mut m, Step::Components, true);
            m.update(Message::More(true));
            m.update(Message::SetSlot(0, "waybar"));
        }
        "appearance" => walk(&mut m, Step::Appearance, true),
        "appearance-changed" => {
            walk(&mut m, Step::Appearance, true);
            m.update(Message::SetRounded(false));
            m.update(Message::SetBlur(Blur::Glass));
            m.update(Message::SetBarPosition(BarPosition::Bottom));
        }
        "apps" => walk(&mut m, Step::Apps, true),
        "apps-full" => {
            walk(&mut m, Step::Apps, true);
            // Full is not selectable yet; its defaults are what the step
            // would open on once it is.
            m.choices = Choices::for_profile(Profile::Full);
        }
        "review" => walk(&mut m, Step::Review, true),
        "review-armed" | "install" | "install-confirm" | "install-done" | "install-failed" => {
            walk(&mut m, Step::Review, true);
            let id = m.disk.clone().unwrap_or_default();
            m.update(Message::Confirm(id));
            if name != "review-armed" {
                // The effects are dropped, and with them the request and its password.
                drop(m.update(Message::Apply));
            }
            match name {
                "install" => progress(&mut m, Stage::Pacstrap, 47, "installing packages", false),
                "install-confirm" => progress(
                    &mut m,
                    Stage::Confirm,
                    12,
                    "waiting for confirmation on the trusted screen",
                    false,
                ),
                "install-done" => progress(&mut m, Stage::Done, 100, "installed", false),
                "install-failed" => progress(&mut m, Stage::Pacstrap, 44, "package download failed", true),
                _ => {}
            }
        }
        _ => return None,
    }
    Some(m)
}

/// Render `name` to `path`. `size` is the logical size, `scale` the output's
/// scale factor: `1280x720` at `1.5` writes 1920x1080.
pub fn render(name: &str, size: (f32, f32), scale: f32, path: &Path) -> Result<(), String> {
    let model =
        fixture(name).ok_or_else(|| format!("no such screen: {name} (try one of: {})", NAMES.join(", ")))?;
    let (w, h) = size;
    let logical = Size::new(w, h);
    let physical = Size::new((w * scale).round() as u32, (h * scale).round() as u32);

    let mut renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
        Font::DEFAULT,
        Pixels(16.0),
        None,
    ))
    .ok_or("no GPU adapter to render with")?;
    {
        // The font database is process-global; a window's compositor fills it,
        // an offscreen renderer has to be told.
        let mut fonts = iced::advanced::graphics::text::font_system()
            .write()
            .expect("font system");
        for face in eclipse_ui::FONTS {
            fonts.load_font(Cow::Borrowed(*face));
        }
    }

    let rgba = if model.step == Step::Welcome {
        let mut welcome = Welcome::new(&model.version)
            .reduced_motion(false)
            .frozen(timeline::READY + 1.0, 0.0);
        welcome.paint(&mut renderer, logical, scale);
        Headless::screenshot(&mut renderer, physical, scale, color::BASE)
    } else {
        let mut ui = UserInterface::build(crate::view::view(&model), logical, Cache::new(), &mut renderer);
        let style = Style {
            text_color: color::TEXT,
        };
        // Widgets learn their status (active, disabled) from the redraw event;
        // without one every button is drawn as if it could not be pressed.
        let mut ignored = Vec::new();
        let redraw = iced::Event::Window(window::Event::RedrawRequested(std::time::Instant::now()));
        let _ = ui.update(
            &[redraw],
            mouse::Cursor::Unavailable,
            &mut renderer,
            &mut Null,
            &mut ignored,
        );
        ui.draw(&mut renderer, &theme::theme(), &style, mouse::Cursor::Unavailable);
        // Text is drawn from paragraphs the interface owns, and the renderer
        // holds them weakly: the interface has to outlive the screenshot.
        let rgba = Headless::screenshot(&mut renderer, physical, scale, color::BASE);
        drop(ui);
        rgba
    };
    image::save_buffer(
        path,
        &rgba,
        physical.width,
        physical.height,
        image::ColorType::Rgba8,
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_named_screen_has_a_fixture_at_the_right_step() {
        for name in NAMES {
            let m = fixture(name).unwrap_or_else(|| panic!("{name}"));
            let want = match *name {
                "welcome" => Step::Welcome,
                n if n.starts_with("language") => Step::Language,
                n if n.starts_with("keyboard") => Step::Keyboard,
                n if n.starts_with("timezone") => Step::Timezone,
                n if n.starts_with("network") => Step::Network,
                n if n.starts_with("disk") => Step::Disk,
                n if n.starts_with("identity") => Step::Identity,
                n if n.starts_with("profile") => Step::Profile,
                "mode" => Step::Mode,
                "layout" => Step::Layout,
                n if n.starts_with("components") => Step::Components,
                n if n.starts_with("appearance") => Step::Appearance,
                n if n.starts_with("apps") => Step::Apps,
                "review" | "review-armed" => Step::Review,
                _ => Step::Install,
            };
            assert_eq!(m.step, want, "{name}");
        }
        assert!(fixture("nope").is_none());
    }

    #[test]
    fn a_screenshot_fixture_never_keeps_a_password() {
        let m = fixture("install").unwrap();
        assert!(m.password.is_empty() && m.password2.is_empty());
    }
}
