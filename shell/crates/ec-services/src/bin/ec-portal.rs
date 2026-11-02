// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-portal`: the xdg-desktop-portal Settings backend for Abyss, serving
//! `org.freedesktop.appearance` from the compositor's theme (ADR 0084).

use std::process::ExitCode;

fn main() -> ExitCode {
    let _service = match ec_services::portal::spawn() {
        Ok(service) => service,
        Err(err) => {
            eprintln!("ec-portal: cannot own org.freedesktop.impl.portal.desktop.abyss: {err}");
            return ExitCode::FAILURE;
        }
    };
    loop {
        std::thread::park();
    }
}
