// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-lock-trigger`: starts `ec-lock` when logind asks for a lock (ADR 0095).
//!
//! Two signals start it: `Session.Lock` on the user's display session (what
//! `loginctl lock-session` sends, so the `Super+Shift+L` bind, the bar and the
//! center all land here) and `Manager.PrepareForSleep(true)`. For the second it
//! holds a logind `delay` inhibitor so the machine does not suspend before the
//! locker is up, and lets go when the locker exits or after
//! [`session::SLEEP_GRACE`], whichever is first. It never holds a suspend
//! longer than that.
//!
//! It only ever *starts* the locker. Nothing here unlocks: that is `ec-lock`'s
//! PAM grant and nothing else. A duplicate start is harmless, because a second
//! `ec-lock` finds the first's claim and exits 0.
//!
//! Each locker runs in its own `app-*.scope` (`app_scope::wrap`), not in this
//! unit's cgroup: stopping or restarting this service must never kill a
//! running locker, which would leave the session behind the compositor's
//! no-input fallback (ADR 0024).

use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::Instant;

use ec_services::app_scope;
use ec_services::session::{self, Response, Session};

enum Event {
    Signal(zbus::Message),
    /// A signal stream ended: the system bus went away.
    BusGone,
    /// The locker started as `seq` exited.
    LockerExited(u64),
}

/// A suspend we are holding: since when, for which locker.
struct Holding {
    since: Instant,
    locker: u64,
    _delay: OwnedFd,
}

fn main() -> ExitCode {
    let session = match Session::connect() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ec-lock-trigger: no system bus: {e}");
            return ExitCode::FAILURE;
        }
    };
    let ours = match session.display_session() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("ec-lock-trigger: cannot find this user's display session: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Subscribe before arming, so nothing between the two is missed.
    let streams = match session.lock_signals(&ours) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ec-lock-trigger: cannot subscribe to logind: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (tx, rx) = mpsc::channel();
    for stream in streams {
        let tx = tx.clone();
        let spawned = std::thread::Builder::new()
            .name("ec-lock-trigger-signals".into())
            .spawn(move || {
                for message in stream {
                    let Ok(message) = message else { break };
                    if tx.send(Event::Signal(message)).is_err() {
                        return;
                    }
                }
                let _ = tx.send(Event::BusGone);
            });
        if let Err(e) = spawned {
            eprintln!("ec-lock-trigger: cannot start a signal thread: {e}");
            return ExitCode::FAILURE;
        }
    }

    let mut armed = arm(&session);
    let mut holding: Option<Holding> = None;
    let mut seq = 0u64;

    loop {
        let got = match &holding {
            Some(h) => rx.recv_timeout(session::SLEEP_GRACE.saturating_sub(h.since.elapsed())),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        let exited = match got {
            Ok(Event::Signal(message)) => {
                let Some(event) = event_of(&message, ours.as_str()) else {
                    continue;
                };
                match session::respond(event) {
                    Response::StartLocker => {
                        seq += 1;
                        start_locker(seq, &tx);
                    }
                    Response::StartLockerBeforeSleep => {
                        seq += 1;
                        start_locker(seq, &tx);
                        match armed.take() {
                            Some(delay) => {
                                holding = Some(Holding {
                                    since: Instant::now(),
                                    locker: seq,
                                    _delay: delay,
                                })
                            }
                            None => eprintln!(
                                "ec-lock-trigger: no sleep delay held; suspend may not wait for the lock"
                            ),
                        }
                    }
                    Response::Rearm => {
                        holding = None;
                        if armed.is_none() {
                            armed = arm(&session);
                        }
                    }
                }
                false
            }
            Ok(Event::LockerExited(n)) => holding.as_ref().is_some_and(|h| h.locker == n),
            Ok(Event::BusGone) | Err(RecvTimeoutError::Disconnected) => {
                // systemd restarts us (Restart=on-failure).
                eprintln!("ec-lock-trigger: lost the system bus");
                return ExitCode::FAILURE;
            }
            Err(RecvTimeoutError::Timeout) => false,
        };
        if holding
            .as_ref()
            .is_some_and(|h| session::release_sleep_delay(h.since.elapsed(), exited))
        {
            // Dropping the fd lets logind go ahead with the suspend.
            holding = None;
        }
    }
}

/// Take the sleep delay for the next suspend. Without one the locker still
/// starts on `PrepareForSleep`; logind just will not wait for it.
fn arm(session: &Session) -> Option<OwnedFd> {
    match session.delay_sleep("Lock the screen before sleep") {
        Ok(fd) => Some(fd),
        Err(e) => {
            eprintln!("ec-lock-trigger: cannot take a sleep delay: {e}");
            None
        }
    }
}

fn event_of(message: &zbus::Message, ours: &str) -> Option<session::LoginEvent> {
    let header = message.header();
    let interface = header.interface()?.as_str();
    let member = header.member()?.as_str();
    let path = header.path()?.as_str();
    let sleeping = if member == "PrepareForSleep" {
        message.body().deserialize::<bool>().ok()
    } else {
        None
    };
    session::classify(interface, member, path, ours, sleeping)
}

/// Start `ec-lock` in its own scope and report its exit as `seq`.
fn start_locker(seq: u64, tx: &Sender<Event>) {
    let argv = vec!["ec-lock".to_owned()];
    let spawn = |argv: &[String]| {
        // stderr is inherited: ec-lock's refusals ("not locking") belong in
        // this unit's journal. It never writes the password anywhere.
        Command::new(&argv[0])
            .args(&argv[1..])
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
    };
    // As in `apps::launch_with`: a wrapped spawn that fails before exec runs
    // the plain argv, since nothing started.
    let scoped = app_scope::wrap("ec-lock", argv.clone());
    let child = match spawn(&scoped) {
        Err(_) if scoped.len() != argv.len() => spawn(&argv),
        other => other,
    };
    match child {
        Ok(mut child) => {
            let tx = tx.clone();
            std::thread::spawn(move || {
                match child.wait() {
                    Ok(status) if !status.success() => {
                        eprintln!("ec-lock-trigger: ec-lock exited with {status}")
                    }
                    Ok(_) => {}
                    Err(e) => eprintln!("ec-lock-trigger: lost track of ec-lock: {e}"),
                }
                let _ = tx.send(Event::LockerExited(seq));
            });
        }
        Err(e) => {
            eprintln!("ec-lock-trigger: cannot start ec-lock: {e}");
            let _ = tx.send(Event::LockerExited(seq));
        }
    }
}
