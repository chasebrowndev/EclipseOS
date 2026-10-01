// SPDX-License-Identifier: AGPL-3.0-only
//! `eclipse-screensaver`: owns `org.freedesktop.ScreenSaver` for the session so
//! a playing video keeps the display on (ADR 0051).

use std::process::ExitCode;

fn main() -> ExitCode {
    let _service = match eclipse_services::screensaver::spawn() {
        Ok(service) => service,
        Err(err) => {
            // Journald picks stderr up. Not fatal to the session: the display
            // simply times out as it would with no service at all.
            eprintln!("eclipse-screensaver: cannot own org.freedesktop.ScreenSaver: {err}");
            return ExitCode::FAILURE;
        }
    };
    // The bus connection and the bridge run on their own threads.
    loop {
        std::thread::park();
    }
}
