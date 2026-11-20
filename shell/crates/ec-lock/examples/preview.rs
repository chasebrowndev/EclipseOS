// SPDX-License-Identifier: AGPL-3.0-only
//! The lock screen's view in an ordinary window, for screenshots.
//!
//! Not a locker: it takes no lock, runs no PAM, reads no keys and has no
//! password. It draws `ec_lock::view::scene` for one fixed state:
//!
//! `cargo run -p ec-lock --example preview -- <state> [layout-file|-] [glance] [WxH]`
//!
//! `layout-file` goes through the lock's own loader (`face::read_capped`,
//! `face::layout_from_text`), so a bad file shows the fallback; `-` or none is
//! the built-in layout. `glance` fakes a battery with two devices and a link.
//! `WxH` sets the window size.
//!
//! States: `ready`, `typed`, `checking`, `wrong`, `wrong-retry`,
//! `unavailable`, `unlocking`, and `secondary` (the scene every other output
//! shows, at a smaller output's size).

use ec_glance::{Battery, Charge, Wifi};
use ec_lock::app::{Message, Status};
use ec_lock::clock::Reading;
use ec_lock::face::{self, Face, Snapshot};
use ec_lock::machine::Reason;
use ec_lock::view::{self, Scene};
use iced::{Element, Theme};

const STATES: &str = "ready | typed | checking | wrong | wrong-retry | unavailable | unlocking | secondary";

#[derive(Clone)]
struct Preview {
    status: Status,
    typed: usize,
    can_submit: bool,
    user: String,
    clock: Reading,
    face: Face,
}

fn fixture(state: &str, face: Face) -> Option<(Preview, (f32, f32))> {
    let failed = |reason, failures, retry_in_secs| Status::Failed {
        reason,
        failures,
        retry_in_secs,
    };
    let (status, typed, can_submit) = match state {
        "ready" => (Status::Ready, 0, false),
        "typed" | "secondary" => (Status::Ready, 9, true),
        "checking" => (Status::Checking, 0, false),
        "wrong" => (failed(Reason::Wrong, 1, None), 0, false),
        "wrong-retry" => (failed(Reason::Wrong, 3, Some(4)), 5, false),
        "unavailable" => (failed(Reason::Unavailable, 1, None), 0, false),
        "unlocking" => (Status::Unlocking, 0, false),
        _ => return None,
    };
    let size = if state == "secondary" {
        (1280.0, 800.0)
    } else {
        (1600.0, 1000.0)
    };
    let p = Preview {
        status,
        typed,
        can_submit,
        user: "robin".to_owned(),
        clock: Reading {
            time: "9:41 AM".to_owned(),
            date: "Wednesday, November 11".to_owned(),
        },
        face,
    };
    Some((p, size))
}

fn update(_p: &mut Preview, _m: Message) {}

fn draw(p: &Preview) -> Element<'_, Message, Theme> {
    view::scene(Scene {
        status: p.status,
        typed: p.typed,
        user: &p.user,
        clock: &p.clock,
        can_submit: p.can_submit,
        face: &p.face,
    })
}

fn main() -> iced::Result {
    let state = std::env::args().nth(1).unwrap_or_else(|| "ready".to_owned());
    let mut layout = face::builtin();
    let (mut glance, mut dims) = (Snapshot::default(), None);
    for arg in std::env::args().skip(2) {
        if arg == "glance" {
            let dev = |internal, name: &str, percent, charge| Battery {
                internal,
                name: name.to_owned(),
                percent,
                charge,
            };
            glance = Snapshot {
                batteries: vec![
                    dev(true, "Internal", 72, Charge::Discharging),
                    dev(false, "Mouse", 40, Charge::Discharging),
                ],
                wifi: Some(Wifi { percent: 68 }),
            };
        } else if let Some((w, h)) = arg.split_once('x') {
            dims = w.parse::<f32>().ok().zip(h.parse::<f32>().ok());
        } else if arg != "-" {
            layout = face::read_capped(std::path::Path::new(&arg), ec_layout::MAX_LINES_BYTES)
                .map_or_else(face::builtin, |t| face::layout_from_text(&t));
        }
    }
    let Some((preview, mut size)) = fixture(&state, Face::settled(layout, glance)) else {
        eprintln!("usage: preview <{STATES}> [layout-file|-] [glance] [WxH]");
        std::process::exit(2);
    };
    if let Some(d) = dims {
        size = d;
    }
    let mut app = iced::application(move || preview.clone(), update, draw)
        .title("ec-lock preview")
        .theme(|_: &Preview| ec_ui::theme::theme())
        .style(|_: &Preview, t: &Theme| view::ground(t))
        .window_size(size)
        .antialiasing(true);
    for face in ec_ui::FONTS {
        app = app.font(*face);
    }
    app.run()
}
