// SPDX-License-Identifier: AGPL-3.0-only
//! systemd/D-Bus user-session handoff, used only when `--session` was passed.
//!
//! COMP-01 §5 stops at step 9 ("export $WAYLAND_DISPLAY"); it says nothing
//! about the user session manager, so this module is additive to the spec, not
//! an implementation of it (ADR 0032).
//!
//! Everything here is best effort. A missing `systemctl`, a missing D-Bus, or
//! a unit that fails to start is logged and ignored: a login session that came
//! up without its user services is still a usable compositor, and a compositor
//! that exits because `systemctl` is absent is not.
//!
//! Children are spawned and never waited on (the desktop unit's start and
//! stop are the exception: they go through one worker thread, in order). `SIGCHLD` carries
//! `SA_NOCLDWAIT` (see `main.rs`), so the kernel reaps them; nothing here may
//! block the calloop.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::config::Mode;

/// Set once `import()` has run, so late variables know to follow it.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// The variables a user service needs to talk to us.
const VARS: [&str; 3] = ["WAYLAND_DISPLAY", "XDG_CURRENT_DESKTOP", "XDG_SESSION_TYPE"];

/// The target ordinary `PartOf=graphical-session.target` units hang off.
const TARGET: &str = "abyss-session.target";

/// Spawn one command, logging rather than propagating any failure.
fn spawn(program: &str, args: &[&str]) {
    match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => tracing::debug!(program, pid = child.id(), "session helper spawned"),
        Err(err) => tracing::warn!(?err, program, "session helper not run; continuing"),
    }
}

/// Run commands one at a time, in the order given, on a worker thread.
///
/// For commands whose order matters (`start` then `stop` of one unit): separate
/// fire-and-forget children race in the user manager. The calloop only sends on
/// a channel and never waits. `SA_NOCLDWAIT` makes `wait` fail with `ECHILD`
/// rather than return a status, but only once the child has exited, which is
/// all the worker needs.
fn spawn_ordered(program: &'static str, args: Vec<String>) {
    use std::sync::mpsc::{channel, Sender};
    use std::sync::OnceLock;
    type Job = (&'static str, Vec<String>);
    static QUEUE: OnceLock<Sender<Job>> = OnceLock::new();
    let tx = QUEUE.get_or_init(|| {
        let (tx, rx) = channel::<Job>();
        let started = std::thread::Builder::new()
            .name("session-ordered".into())
            .spawn(move || {
                for (program, args) in rx {
                    match Command::new(program)
                        .args(&args)
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                    {
                        Ok(mut child) => {
                            let _ = child.wait();
                        }
                        Err(err) => {
                            tracing::warn!(?err, program, "session helper not run; continuing");
                        }
                    }
                }
            });
        if let Err(err) = started {
            tracing::warn!(?err, "session worker not started; continuing");
        }
        tx
    });
    let _ = tx.send((program, args));
}

/// Publish the session environment and start the user target.
///
/// Must be called *after* the Wayland socket exists and `WAYLAND_DISPLAY` is
/// set in our own environment — the helpers read it from us, which is the
/// whole reason the handoff lives here instead of in the wrapper script.
pub fn import(mode: Mode) {
    ACTIVE.store(true, Ordering::Relaxed);
    spawn(
        "dbus-update-activation-environment",
        &["--systemd", VARS[0], VARS[1], VARS[2]],
    );
    spawn(
        "systemctl",
        &["--user", "import-environment", VARS[0], VARS[1], VARS[2]],
    );
    spawn("systemctl", &["--user", "start", TARGET]);
    tracing::info!(target = TARGET, "session handoff requested");
    // The target does not pull the desktop in (its unit has no [Install]); the
    // mode decides, and a session that starts in "de" starts it here.
    desktop_sync(None, mode);
}

/// The desktop-icons unit: started and stopped by the compositor, never by the
/// target, because it exists only under `mode "de"` (C-24).
const DESKTOP_UNIT: &str = "ec-desktop.service";

/// What the desktop-icons unit should do when the mode goes `old` -> `new`
/// (`None`: session start).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitAction {
    Start,
    Stop,
    Keep,
}

/// Pure decision for [`desktop_sync`]: start on entering `De` (or starting in
/// it), stop on leaving it, nothing otherwise. A start-up in another mode does
/// nothing: the unit is not enabled, so there is nothing to stop.
pub fn desktop_action(old: Option<Mode>, new: Mode) -> UnitAction {
    let was = old == Some(Mode::De);
    match (was, new == Mode::De) {
        (false, true) => UnitAction::Start,
        (true, false) => UnitAction::Stop,
        _ => UnitAction::Keep,
    }
}

/// Start or stop `ec-desktop.service` for a mode change. No-op outside
/// `--session` (before `import()` ran), like every helper here.
pub fn desktop_sync(old: Option<Mode>, new: Mode) {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let verb = match desktop_action(old, new) {
        UnitAction::Start => "start",
        UnitAction::Stop => "stop",
        UnitAction::Keep => return,
    };
    spawn_ordered(
        "systemctl",
        vec!["--user".into(), verb.into(), DESKTOP_UNIT.into()],
    );
    tracing::info!(unit = DESKTOP_UNIT, verb, "desktop icons unit requested");
}

/// Publish `DISPLAY` once XWayland is up, which is after `import()` ran.
/// No-op outside `--session`. Launchers started as user services need it:
/// X11-only clients (Qt `xcb` AppImages) die without it.
pub fn import_display() {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    spawn("dbus-update-activation-environment", &["--systemd", "DISPLAY"]);
    spawn("systemctl", &["--user", "import-environment", "DISPLAY"]);
}

/// Stop the user target on the way out. Services that are
/// `PartOf=graphical-session.target` stop with it.
pub fn teardown() {
    spawn("systemctl", &["--user", "stop", TARGET]);
    tracing::info!(target = TARGET, "session teardown requested");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_commands_run_in_order_and_one_at_a_time() {
        let out = std::env::temp_dir().join(format!("ec-session-ordered-{}", std::process::id()));
        let _ = std::fs::remove_file(&out);
        let o = out.display().to_string();
        // The first is slower: unordered, "b" would land first.
        spawn_ordered("sh", vec!["-c".into(), format!("sleep 0.3; echo a >> {o}")]);
        spawn_ordered("sh", vec!["-c".into(), format!("echo b >> {o}")]);
        for _ in 0..100 {
            if std::fs::read_to_string(&out).is_ok_and(|t| t.lines().count() == 2) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "a\nb\n");
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn the_desktop_unit_follows_the_mode() {
        use UnitAction::*;
        assert_eq!(desktop_action(None, Mode::De), Start);
        assert_eq!(desktop_action(None, Mode::Wm), Keep);
        assert_eq!(desktop_action(None, Mode::Hybrid), Keep);
        assert_eq!(desktop_action(Some(Mode::Wm), Mode::De), Start);
        assert_eq!(desktop_action(Some(Mode::Hybrid), Mode::De), Start);
        assert_eq!(desktop_action(Some(Mode::De), Mode::Wm), Stop);
        assert_eq!(desktop_action(Some(Mode::De), Mode::Hybrid), Stop);
        assert_eq!(desktop_action(Some(Mode::De), Mode::De), Keep);
        assert_eq!(desktop_action(Some(Mode::Wm), Mode::Hybrid), Keep);
    }
}
