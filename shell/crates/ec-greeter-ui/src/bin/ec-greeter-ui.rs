// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-greeter-ui`: the login screen under greetd (ADR 0126). The executable name
//! is what the compositor's glass allow-list matches. Started by `ec-greeter-run`.

use std::process::ExitCode;

use ec_greeter::{Settings, STATE_DIR};
use ec_greeter_ui::greetd::GreetdAuth;

fn main() -> ExitCode {
    let Some(auth) = GreetdAuth::from_env() else {
        let _ = std::io::Write::write_all(&mut std::io::stderr(), b"ec-greeter-ui: no GREETD_SOCK\n");
        return ExitCode::FAILURE;
    };
    // A missing or bad settings file is the built-in look, never a failed login.
    let settings = std::fs::read_to_string(std::path::Path::new(STATE_DIR).join(ec_greeter::SETTINGS_FILE))
        .ok()
        .and_then(|t| Settings::parse(&t).ok())
        .unwrap_or_default();
    match ec_greeter_ui::run(settings, Box::new(auth)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}
