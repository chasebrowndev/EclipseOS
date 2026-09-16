// SPDX-License-Identifier: AGPL-3.0-only

//! Oracle-Eyes — the EclipseOS screen annotation daemon (spec.md, COMP-18).
//!
//! Phase 1 is the wiring and nothing else: subscribe to the compositor's
//! `keybind` stream (COMP-18 §4) and put a fixed string on screen when the
//! select chord fires. Capture, OCR, redaction and the model call arrive in
//! Phase 2 — this binary deliberately has no capture capability yet.

mod hud;

use std::process::ExitCode;

use eclipse_ipc::{Client, EventKind};

use hud::{Anchor, Hud};

/// Stand-in for a real answer until Phase 2. Deliberately says what it is:
/// "fail visibly" (CLAUDE.md) cuts both ways, and a placeholder that looks
/// like an answer is worse than one that admits it isn't.
const PLACEHOLDER: &str = "Oracle-Eyes is connected. No OCR or model call yet — this is the annotation pass proving it can draw.";

/// Where the placeholder goes until the region selector exists (Phase 2).
/// The compositor clamps this onto an output, so an anchor it dislikes is a
/// misplaced panel, never a crash.
const FIXED_ANCHOR: Anchor = Anchor {
    x: 64,
    y: 64,
    w: 480,
    h: 120,
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("oracle-eyes: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut client = Client::connect().map_err(|e| format!("control socket: {e}"))?;
    client
        .subscribe(&[EventKind::Keybind])
        .map_err(|e| format!("subscribe to keybind: {e}"))?;
    eprintln!("oracle-eyes: connected, listening for annotation chords");

    let mut hud = Hud::new();
    loop {
        wait_readable(client.as_raw_fd())?;
        loop {
            let event = match client.poll_event() {
                Ok(Some(e)) => e,
                Ok(None) => break,
                Err(e) => return Err(format!("control socket: {e}")),
            };
            if event.kind != EventKind::Keybind {
                continue;
            }
            let action = event.data.get("action").and_then(|v| v.as_str());
            // An unknown action is the compositor being newer than this
            // binary, which is ordinary. Ignore it; do not exit.
            let result = match action {
                Some("annotation-select") => {
                    hud.show(&mut client, FIXED_ANCHOR, PLACEHOLDER).map(|_| ())
                }
                Some("annotation-dismiss") => hud.dismiss(&mut client),
                Some("annotation-expand") => {
                    hud.show(&mut client, FIXED_ANCHOR, PLACEHOLDER).map(|_| ())
                }
                _ => continue,
            };
            // A refused method is a policy answer, not a crash (COMP-13
            // §2.2). Say so and keep running: the owner may be editing the
            // gate while we are up.
            if let Err(e) = result {
                eprintln!("oracle-eyes: {e}");
            }
        }
    }
}

/// Block until the socket has something, so the daemon costs nothing while
/// idle. `poll(2)` directly rather than a runtime: one fd does not justify
/// a dependency.
fn wait_readable(fd: std::os::fd::RawFd) -> Result<(), String> {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        // SAFETY: one initialised pollfd, count 1, no timeout.
        let n = unsafe { libc::poll(&mut pfd, 1, -1) };
        if n >= 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::Interrupted {
            return Err(format!("poll: {err}"));
        }
    }
}
