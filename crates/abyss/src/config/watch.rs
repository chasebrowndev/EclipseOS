// SPDX-License-Identifier: AGPL-3.0-only
//! Config hot reload (COMP-13 §1.2): inotify, debounced 100 ms.
//!
//! inotify rather than a poll loop, because COMP-14 §3 forbids waking the
//! compositor on a timer to ask whether anything happened. The watches are on
//! the *directories* holding the config, not the files: editors write a
//! temporary file and rename over the target, which destroys an inode watch
//! on the first save.
//!
//! Both the descriptor and the debounce timer are calloop sources on the
//! compositor loop; nothing here runs on another thread.

use std::{
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::PathBuf,
    time::Duration,
};

use smithay::reexports::calloop::{
    generic::Generic,
    timer::{TimeoutAction, Timer},
    Interest, LoopHandle, Mode, PostAction,
};

use crate::state::AbyssState;

/// COMP-13 §1.2. Long enough to absorb a write-and-rename or a `abyss.d`
/// directory rewritten file by file, short enough to feel immediate.
const DEBOUNCE: Duration = Duration::from_millis(100);

const EVENT_SIZE: usize = std::mem::size_of::<libc::inotify_event>();

/// Directories worth watching: the parent of every existing source, plus the
/// user config directory itself so a config created after startup is noticed.
fn watch_dirs(state: &AbyssState) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut push = |p: PathBuf| {
        if p.is_dir() && !dirs.contains(&p) {
            dirs.push(p);
        }
    };
    for src in &state.config.sources {
        if let Some(parent) = src.parent() {
            push(parent.to_path_buf());
        }
    }
    if let Some(base) = dirs_config() {
        push(base.join("eclipse"));
        push(base.join("eclipse/abyss.d"));
    }
    push(PathBuf::from("/etc/eclipse"));
    dirs
}

fn dirs_config() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
        if !x.is_empty() {
            return Some(PathBuf::from(x));
        }
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config"))
}

/// Start watching. A failure is logged and the compositor runs on without hot
/// reload; `reload_config` over the control socket still works.
pub fn start(state: &mut AbyssState, handle: &LoopHandle<'static, AbyssState>) {
    // SAFETY: `inotify_init1` takes only flags and returns a new fd or -1.
    let raw = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
    if raw < 0 {
        tracing::warn!(error = %std::io::Error::last_os_error(), "inotify unavailable; no config hot reload");
        return;
    }
    // SAFETY: `raw` is a fresh, owned, valid descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };

    let dirs = watch_dirs(state);
    let mask = libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO | libc::IN_CREATE | libc::IN_DELETE;
    let mut watched = 0usize;
    for dir in &dirs {
        let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()) else {
            continue;
        };
        // SAFETY: `fd` is a live inotify descriptor and `c` a NUL-terminated
        // path that outlives the call.
        let wd = unsafe { libc::inotify_add_watch(fd.as_raw_fd(), c.as_ptr(), mask) };
        if wd < 0 {
            tracing::debug!(path = %dir.display(), "not watching");
        } else {
            watched += 1;
        }
    }
    if watched == 0 {
        tracing::info!("no config directories to watch");
        return;
    }

    let inserted = handle.insert_source(Generic::new(fd, Interest::READ, Mode::Level), |_, fd, state| {
        drain(fd.as_raw_fd());
        schedule(state);
        Ok(PostAction::Continue)
    });
    if let Err(e) = inserted {
        tracing::warn!(%e, "inserting the config watcher");
        return;
    }
    tracing::info!(dirs = watched, "watching for config changes");
}

/// Empty the inotify queue. The events themselves are not inspected: any
/// activity in a config directory means re-read everything, which is both
/// simpler and correct for `abyss.d` (COMP-13 §1.2 "never half-apply").
fn drain(fd: std::os::fd::RawFd) {
    let mut buf = [0u8; (EVENT_SIZE + libc::NAME_MAX as usize + 1) * 16];
    loop {
        // SAFETY: reading into a local buffer from a valid non-blocking fd.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            return;
        }
    }
}

/// Arm the debounce. Repeated events inside the window collapse into one
/// reload because the pending timer is left alone and only the deadline it
/// checks moves forward.
fn schedule(state: &mut AbyssState) {
    state.config_dirty = Some(std::time::Instant::now() + DEBOUNCE);
    if state.config_timer.is_some() {
        return;
    }
    let token = state
        .loop_handle
        .insert_source(Timer::from_duration(DEBOUNCE), |_, _, state| {
            match state.config_dirty {
                Some(deadline) if deadline > std::time::Instant::now() => TimeoutAction::ToInstant(deadline),
                _ => {
                    state.config_dirty = None;
                    state.config_timer = None;
                    reload_now(state);
                    TimeoutAction::Drop
                }
            }
        });
    match token {
        Ok(t) => state.config_timer = Some(t),
        Err(e) => {
            tracing::warn!(%e, "arming the config debounce");
            state.config_dirty = None;
        }
    }
}

/// Re-read the config and apply it. Also the body of `reload_config` over the
/// control socket.
///
/// Validation is total (COMP-13 §1.2): an invalid file keeps the last good
/// config, surfaces the refusals as a `config-error` IPC event and in journald,
/// and applies nothing. Never half-apply.
pub fn reload_now(state: &mut AbyssState) {
    let next = state.config.reload();
    if !next.errors.is_empty() {
        let errors: Vec<serde_json::Value> = next
            .errors
            .iter()
            .map(|e| {
                serde_json::json!({
                    "file": e.file.display().to_string(),
                    "line": e.line,
                    "col": e.col,
                    "message": e.message,
                })
            })
            .collect();
        for e in &next.errors {
            tracing::error!("{e}");
        }
        tracing::warn!(
            count = next.errors.len(),
            "config invalid, keeping the last good one"
        );
        crate::ipc::emit(state, "config-error", serde_json::json!({ "errors": errors }));
        return;
    }
    let sources: Vec<String> = next.sources.iter().map(|p| p.display().to_string()).collect();
    state.config = next;
    // Retune the two global bind filters. They hold Allowlist handles rather
    // than snapshots precisely so this line is possible (ADR 0022 amendment).
    state.capture_allow.set(state.config.capture.allow.clone());
    state
        .clipboard_allow
        .set(state.config.clipboard.data_control_allow.clone());
    crate::input::apply_config(state);
    crate::outputs::relayout(state);
    crate::shell::arrange(state);
    crate::backend::damage_all(state);
    tracing::info!(?sources, "config reloaded");
}
