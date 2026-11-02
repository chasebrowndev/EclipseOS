// SPDX-License-Identifier: AGPL-3.0-only
//! Visual probe: `probe <source|permission|shot|color|account|email>` shows one
//! pane with a canned `PaneRequest`, with no daemon. The reply is dropped.

use std::os::unix::net::UnixStream;
use std::sync::Mutex;

use ecl_portal_proto::{IdentitySource, PaneRequest, Requester, Source};
use ecl_portal_ui::app::{self, App};
use ecl_portal_ui::model::Pane;
use ecl_portal_ui::{view, APP_ID};
use iced::window;

fn requester() -> Requester {
    Requester {
        app_id: "org.example.Recorder".into(),
        display_name: Some("Example Recorder".into()),
        icon: None,
        source: IdentitySource::DesktopFile,
    }
}

fn request(kind: &str) -> PaneRequest {
    let requester = requester();
    match kind {
        "source" | "source-win" => {
            let mut sources = vec![
                Source::Output {
                    name: "DP-1".into(),
                    width: 2560,
                    height: 1440,
                },
                Source::Output {
                    name: "HDMI-A-1".into(),
                    width: 1920,
                    height: 1080,
                },
                Source::Output {
                    name: "DP-2".into(),
                    width: 3840,
                    height: 2160,
                },
            ];
            for (i, (t, a)) in [
                ("Terminal - foot", "foot"),
                ("Docs - Firefox", "firefox"),
                ("main.rs - Zed", "dev.zed.Zed"),
                ("Music", "org.example.Music"),
                (
                    "A very long window title that will need to be cut off somewhere sensible",
                    "org.example.Long",
                ),
            ]
            .iter()
            .enumerate()
            {
                sources.push(Source::Window {
                    id: i as u64 + 1,
                    title: (*t).into(),
                    app_id: (*a).into(),
                });
            }
            PaneRequest::PickSource {
                requester,
                sources,
                allow_multiple: true,
                allow_audio: true,
                allow_window: true,
                allow_monitor: true,
                remembered: None,
            }
        }
        "permission" => PaneRequest::Permission {
            requester,
            interface: "org.freedesktop.impl.portal.Location".into(),
            title: "Allow access to your location?".into(),
            detail: "Example Recorder wants to know where you are while it is open.".into(),
            allow_remember: true,
        },
        "shot" => PaneRequest::ScreenshotMode { requester },
        "color" => PaneRequest::PickColor { requester },
        "account" => PaneRequest::Account {
            requester,
            reason: "To label your recordings with your name.".into(),
        },
        _ => PaneRequest::Email {
            requester,
            addresses: vec!["ada@example.org".into(), "grace@example.org".into()],
        },
    }
}

/// cage maximises the toplevel; lay the pane out at its real size.
fn framed(a: &App) -> iced::Element<'_, ecl_portal_ui::app::Message> {
    let (w, h, _) = a.pane.window();
    iced::widget::container(view::view(a))
        .width(iced::Length::Fixed(w))
        .height(iced::Length::Fixed(h))
        .into()
}

fn main() -> iced::Result {
    let kind = std::env::args().nth(1).unwrap_or_else(|| "source".into());
    let (mine, theirs) = UnixStream::pair().expect("pair");
    std::mem::forget(mine);
    let mut opened = Pane::open(request(&kind), app::system_identity(), app::start_color());
    if kind == "source-win" {
        if let Pane::Source(c) = &mut opened.pane {
            c.set_tab(ecl_portal_ui::model::Tab::Window);
            c.toggle(4);
        }
    } else if kind == "source" {
        if let Pane::Source(c) = &mut opened.pane {
            c.toggle(0);
        }
    }
    let (w, h, _) = opened.pane.window();
    let size = iced::Size::new(w, h);
    let boot = Mutex::new(Some((opened, theirs)));
    let mut b = iced::application(
        move || {
            let (o, s) = boot.lock().unwrap().take().unwrap();
            App::new(o, s)
        },
        app::update,
        framed,
    )
    .title(|a: &App| a.title().to_owned())
    .theme(|_: &App| ec_ui::theme::theme())
    .style(ec_ui::theme::clear_window)
    .subscription(app::subscription)
    .window(window::Settings {
        size,
        min_size: Some(size),
        max_size: Some(size),
        resizable: false,
        decorations: false,
        transparent: true,
        exit_on_close_request: false,
        platform_specific: window::settings::PlatformSpecific {
            application_id: APP_ID.to_owned(),
            ..Default::default()
        },
        ..window::Settings::default()
    })
    .antialiasing(true);
    for face in ec_ui::FONTS {
        b = b.font(*face);
    }
    b.run()
}
