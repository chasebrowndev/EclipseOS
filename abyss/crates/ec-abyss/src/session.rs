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
//! Children are spawned and never waited on. `SIGCHLD` carries
//! `SA_NOCLDWAIT` (see `main.rs`), so the kernel reaps them; nothing here may
//! block the calloop.

use std::process::{Command, Stdio};

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

/// Publish the session environment and start the user target.
///
/// Must be called *after* the Wayland socket exists and `WAYLAND_DISPLAY` is
/// set in our own environment — the helpers read it from us, which is the
/// whole reason the handoff lives here instead of in the wrapper script.
pub fn import() {
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
}

/// Stop the user target on the way out. Services that are
/// `PartOf=graphical-session.target` stop with it.
pub fn teardown() {
    spawn("systemctl", &["--user", "stop", TARGET]);
    tracing::info!(target = TARGET, "session teardown requested");
}
