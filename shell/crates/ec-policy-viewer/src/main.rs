// SPDX-License-Identifier: AGPL-3.0-only
//! `eclipse-policy-viewer` — a plain xdg_toplevel. Not a layer surface: only
//! the bar and the OSD are.

use eclipse_policy_viewer::app::{self, App};

fn main() -> iced::Result {
    let mut builder = iced::application(App::new, app::update, app::view)
        .title("Eclipse Policy")
        .theme(|_: &App| eclipse_ui::theme::theme())
        .window_size((820.0, 720.0))
        .antialiasing(true);
    // Every face, registered once, before the window exists.
    for face in eclipse_ui::FONTS {
        builder = builder.font(*face);
    }
    builder.run()
}
