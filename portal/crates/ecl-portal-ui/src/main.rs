// SPDX-License-Identifier: AGPL-3.0-only
//! `ecl-portal-ui --fd N`: one dialog, one reply. Not a layer surface: window
//! rules only match toplevels (same reasoning as `ec-secret-prompt`).

use std::os::fd::FromRawFd;
use std::os::unix::net::UnixStream;
use std::sync::Mutex;

use ecl_portal_ui::app::{self, App};
use ecl_portal_ui::model::Pane;
use ecl_portal_ui::{view, wire, APP_ID};
use iced::window;

fn main() -> iced::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(fd) = wire::parse_fd(&args) else {
        eprintln!("usage: ecl-portal-ui --fd N");
        std::process::exit(2);
    };
    // SAFETY: the portal daemon passes this descriptor to us, open and
    // unshared, and names it on the command line; nothing else in this
    // process owns it.
    let mut stream = unsafe { UnixStream::from_raw_fd(fd) };
    let request = match wire::read_request(&mut stream) {
        Ok(r) => r,
        Err(e) => {
            // The frame's contents are never printed, only why it failed.
            eprintln!("ecl-portal-ui: no usable request: {e}");
            std::process::exit(1);
        }
    };

    let opened = Pane::open(request, app::system_identity(), app::start_color());
    let (w, h, _) = opened.pane.window();
    let size = iced::Size::new(w, h);
    let boot = Mutex::new(Some((opened, stream)));

    let mut builder = iced::application(
        move || {
            let (opened, stream) = boot.lock().expect("boot lock").take().expect("booted once");
            App::new(opened, stream)
        },
        app::update,
        view::view,
    )
    .title(|a: &App| a.title().to_owned())
    .theme(|_: &App| ec_ui::theme::theme())
    // Transparent, so the compositor's blur shows through; the view lays a
    // tint, or an opaque ground when blur is off.
    .style(ec_ui::theme::clear_window)
    .subscription(app::subscription)
    .window(window::Settings {
        size,
        min_size: Some(size),
        max_size: Some(size),
        resizable: false,
        decorations: false,
        transparent: true,
        // A close request is a refusal: it is answered, not obeyed blindly.
        exit_on_close_request: false,
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
