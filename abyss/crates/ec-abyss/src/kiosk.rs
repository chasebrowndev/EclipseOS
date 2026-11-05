// SPDX-License-Identifier: AGPL-3.0-only
//! `--kiosk -- <client> [args]`: abyss as the greeter's host (replaces `cage`
//! under greetd). One client is spawned once the socket is up, and abyss exits
//! when it does. Runs before anyone has logged in, as an unprivileged user, so
//! everything that could hand out a process or a second way in is off:
//!
//! - add-ons are not loaded (`addons::start`) and Xwayland is not started;
//! - neither the control socket nor the trusted socket is bound;
//! - only the reserved chords act (see [`permits`]).
//!
//! The exit status of the client is not available: `main.rs` sets
//! `SA_NOCLDWAIT` on `SIGCHLD`, so the kernel reaps it. Exit is detected
//! through a pidfd instead, and abyss exits 0 when the client does.

use std::os::fd::{FromRawFd, OwnedFd};
use std::process::Command;
use std::sync::OnceLock;

use anyhow::{anyhow, Context, Result};
use smithay::reexports::calloop::{generic::Generic, Interest, LoopHandle, Mode, PostAction};

use crate::input::Action;
use crate::state::AbyssState;

/// The client argv, set once by `main` before a backend starts.
static ARGV: OnceLock<Vec<String>> = OnceLock::new();

/// Turn kiosk mode on with this client. Called once, from `main`.
pub fn enable(argv: Vec<String>) {
    let _ = ARGV.set(argv);
}

/// Mark the state as kiosk, if enabled. First thing after `AbyssState::new`,
/// before any socket, add-on or Xwayland startup looks at the flag.
pub fn init(state: &mut AbyssState) {
    state.kiosk = ARGV.get().is_some();
}

/// Whether a bound action may run in kiosk mode. An allowlist with no
/// catch-all: a newly added `Action` is dropped until it is listed here.
///
/// - `AgentOverride` / `AgentTerminate`: the reserved chords (COMP-04 §6).
/// - `SwitchVt`: the keyboard filter turns VT keysyms into this action, and it
///   must keep working so a stuck greeter can be left.
/// - `Prompt`: only produced while a trusted prompt (the override panel) owns
///   the seat; dropping it would trap the keys.
///
/// Everything else is dropped: `Spawn` and `Quit` (a shell pre-auth),
/// `AgentAttention` (Super+space opens phrase entry, a trusted prompt with no
/// user session), window management, add-on chords, calibration, selection.
pub fn permits(action: &Action) -> bool {
    match action {
        Action::AgentOverride | Action::AgentTerminate | Action::SwitchVt(_) | Action::Prompt(_) => true,
        Action::Quit
        | Action::Spawn(_)
        | Action::Close
        | Action::ToggleFloating
        | Action::ToggleFullscreen
        | Action::ToggleMaximize
        | Action::Minimize
        | Action::Unminimize
        | Action::ToggleLayout
        | Action::Focus(_)
        | Action::Move(_)
        | Action::Priority(_)
        | Action::SwitchWorkspace(_)
        | Action::WorkspaceNext
        | Action::WorkspacePrev
        | Action::MoveToWorkspace(_)
        | Action::MoveToOutputWorkspace(_)
        | Action::AgentAttention
        | Action::Calibrate(_)
        | Action::RegionSelect(_)
        | Action::AnnotationSelect
        | Action::AnnotationDismiss
        | Action::AnnotationExpand
        | Action::AnnotationAutoToggle
        | Action::AnnotationModelSwitch
        | Action::AnnotationImageToggle => false,
    }
}

/// Spawn the client and stop the loop when it exits. An error here (nothing
/// to run, spawn failed, no pidfd) ends abyss non-zero rather than leaving a
/// black screen. Call once the Wayland socket exists and `WAYLAND_DISPLAY` is
/// set.
pub fn launch(state: &mut AbyssState, handle: &LoopHandle<'static, AbyssState>) -> Result<()> {
    let Some(argv) = ARGV.get().filter(|_| state.kiosk) else {
        return Ok(());
    };
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| anyhow!("kiosk: no client command"))?;
    let child = Command::new(program)
        .args(args)
        .env("WAYLAND_DISPLAY", &state.socket_name)
        .env("XDG_SESSION_TYPE", "wayland")
        .env_remove("DISPLAY")
        .spawn()
        .with_context(|| format!("kiosk: spawning {program}"))?;
    let pid = child.id();
    // Never waited on: SIGCHLD is SA_NOCLDWAIT, the kernel reaps it.
    drop(child);
    // SAFETY: `pidfd_open` takes a pid and flags and returns a new fd or -1.
    // A client that exits before this line is already reaped, so it fails with
    // ESRCH: abyss exits non-zero, the same as a client that could not start.
    // Once the fd is open, an exit makes it readable, whenever it happens.
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
    if raw < 0 {
        return Err(anyhow!(
            "kiosk: pidfd_open({pid}) failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: `raw` is a fresh, owned, valid descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
    handle
        .insert_source(Generic::new(fd, Interest::READ, Mode::Level), |_, _, state| {
            tracing::info!("kiosk: client exited; shutting down");
            state.loop_signal.stop();
            Ok(PostAction::Remove)
        })
        .map_err(|e| anyhow!("kiosk: inserting the exit watcher: {e}"))?;
    tracing::info!(pid, client = %program, "kiosk: client started");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Direction;

    #[test]
    fn spawn_and_quit_are_dropped() {
        assert!(!permits(&Action::Spawn("foot".into())));
        assert!(!permits(&Action::Quit));
        assert!(!permits(&Action::AgentAttention));
        assert!(!permits(&Action::Focus(Direction::Left)));
    }

    #[test]
    fn reserved_chords_vt_switch_and_prompt_keys_are_kept() {
        assert!(permits(&Action::AgentOverride));
        assert!(permits(&Action::AgentTerminate));
        assert!(permits(&Action::SwitchVt(2)));
    }
}
