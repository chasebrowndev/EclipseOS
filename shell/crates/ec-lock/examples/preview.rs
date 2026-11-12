// SPDX-License-Identifier: AGPL-3.0-only
//! The lock screen's view in an ordinary window, for screenshots.
//!
//! Not a locker: it takes no lock, runs no PAM, reads no keys and has no
//! password. It draws `ec_lock::view::scene` for one fixed state:
//!
//! `cargo run -p ec-lock --example preview -- <state>`
//!
//! States: `ready`, `typed`, `checking`, `wrong`, `wrong-retry`,
//! `unavailable`, `unlocking`, and `secondary` (the scene every other output
//! shows, at a smaller output's size).

use ec_lock::app::{Message, Status};
use ec_lock::clock::Reading;
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
}

fn fixture(state: &str) -> Option<(Preview, (f32, f32))> {
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
    })
}

fn main() -> iced::Result {
    let state = std::env::args().nth(1).unwrap_or_else(|| "ready".to_owned());
    let Some((preview, size)) = fixture(&state) else {
        eprintln!("usage: preview <{STATES}>");
        std::process::exit(2);
    };
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
