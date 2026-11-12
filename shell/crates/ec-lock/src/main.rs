// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-lock` — lock the session until the user's password passes PAM.
//!
//! Order matters. Everything that could leave the locker unable to unlock is
//! settled *before* the lock is requested: the arguments, the user, the
//! PAM service file, the one-locker claim. After `iced_sessionlock` takes the lock, every exit but
//! a PAM grant leaves the session locked: an error, a panic, a signal, a
//! `kill -9`. The compositor then draws its own fallback until another locker
//! binds (ADR 0024).

use std::process::ExitCode;

use ec_lock::{app, args, instance, service, user, view};

fn main() -> ExitCode {
    match args::parse(std::env::args_os().skip(1)) {
        Ok(args::Cmd::Lock) => {}
        Ok(args::Cmd::Help) => {
            print!("{}", args::USAGE);
            return ExitCode::SUCCESS;
        }
        Ok(args::Cmd::Version) => {
            println!("ec-lock {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Err(args::Refused) => {
            eprint!("{}", args::USAGE);
            return ExitCode::from(2);
        }
    }

    // Never the payload: a panic message is ours, but nothing about a lock
    // screen's state belongs in a log. Unwinding still runs, so the password
    // buffer is wiped on the way out; the lock stays.
    std::panic::set_hook(Box::new(|info| {
        let at = info.location().map(|l| format!(" at {}:{}", l.file(), l.line()));
        eprintln!(
            "ec-lock: panicked{}; the session stays locked",
            at.unwrap_or_default()
        );
    }));

    let user = match user::current() {
        Ok(u) => u,
        Err(e) => {
            eprintln!("ec-lock: cannot tell which user to authenticate ({e:?}); not locking");
            return ExitCode::FAILURE;
        }
    };

    // No service file means libpam's `other` (pam_deny): a lock no password
    // opens. Refuse rather than strand the human.
    if service::find(&service::DIRS, ec_lock::PAM_SERVICE).is_none() {
        eprintln!(
            "ec-lock: no PAM service file ({0}/{2} or {1}/{2}); not locking",
            service::DIRS[0],
            service::DIRS[1],
            ec_lock::PAM_SERVICE
        );
        return ExitCode::FAILURE;
    }

    let _claim = match instance::claim() {
        instance::Outcome::Claimed(c) => c,
        instance::Outcome::Running => {
            eprintln!("ec-lock: another ec-lock holds the session");
            return ExitCode::SUCCESS;
        }
    };

    // Start on the compositor's theme (ADR 0084); the subscription keeps it.
    ec_ui::ipc::sync_theme_now();
    // No text widget, no paste: nothing here needs the clipboard worker.
    iced_sessionlock::disable_clipboard();

    let mut builder =
        iced_sessionlock::application(move || app::App::new(user.clone()), app::update, view::view)
            .style(view::style)
            .theme(|_: &app::App| ec_ui::theme::theme())
            .subscription(app::subscription)
            .antialiasing(true);
    for face in ec_ui::FONTS {
        builder = builder.font(*face);
    }
    match builder.run() {
        // Only reached after `unlock_and_destroy`.
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // If the lock was taken, it stays taken: the compositor's
            // fallback covers every output until another locker binds.
            eprintln!("ec-lock: {e}; if the session locked, it stays locked");
            ExitCode::FAILURE
        }
    }
}
