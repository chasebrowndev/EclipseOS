// SPDX-License-Identifier: AGPL-3.0-only
//! The premade command-widget catalog watcher (ADR 0067 "Premade catalog";
//! COMP-10 §3.11). Reading the catalog lives in `ec_abyss_config::catalog`;
//! this half is the directory watcher: inotify on the compositor loop,
//! debounced, nothing on another thread, like the add-on directory
//! (`addons.rs`).

use std::{
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::Path,
    time::Duration,
};

use smithay::reexports::calloop::{
    generic::Generic,
    timer::{TimeoutAction, Timer},
    Interest, LoopHandle, Mode, PostAction,
};

#[cfg(test)]
pub use ec_abyss_config::catalog::set_test_dir;
pub use ec_abyss_config::catalog::{load, CATALOG_DIR};

use crate::state::AbyssState;

const DEBOUNCE: Duration = Duration::from_millis(100);

/// Watch [`CATALOG_DIR`] (and its parent, so the first install is seen).
/// A change re-reads the whole config while `taskbar-widgets` is on.
pub fn start(handle: &LoopHandle<'static, AbyssState>) {
    // SAFETY: `inotify_init1` takes only flags and returns a new fd or -1.
    let raw = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
    if raw < 0 {
        tracing::warn!(error = %std::io::Error::last_os_error(), "inotify unavailable; catalog changes need a restart");
        return;
    }
    // SAFETY: `raw` is a fresh, owned, valid descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let dir = Path::new(CATALOG_DIR);
    let parent = dir.parent().unwrap_or(Path::new("/"));
    let raw = fd.as_raw_fd();
    let watched = [add_watch(raw, parent), add_watch(raw, dir)];
    if !watched.contains(&true) {
        tracing::debug!(dir = CATALOG_DIR, "no widget catalog directory to watch");
        return;
    }
    let inserted = handle.insert_source(Generic::new(fd, Interest::READ, Mode::Level), |_, fd, state| {
        drain(fd.as_raw_fd());
        add_watch(fd.as_raw_fd(), Path::new(CATALOG_DIR));
        schedule(state);
        Ok(PostAction::Continue)
    });
    if let Err(e) = inserted {
        tracing::warn!(%e, "inserting the widget catalog watcher");
    }
}

fn add_watch(fd: std::os::fd::RawFd, dir: &Path) -> bool {
    let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()) else {
        return false;
    };
    let mask = libc::IN_CLOSE_WRITE
        | libc::IN_MOVED_TO
        | libc::IN_MOVED_FROM
        | libc::IN_CREATE
        | libc::IN_DELETE
        | libc::IN_ONLYDIR;
    // SAFETY: `fd` is a live inotify descriptor and `c` a NUL-terminated path
    // that outlives the call.
    unsafe { libc::inotify_add_watch(fd, c.as_ptr(), mask) >= 0 }
}

fn drain(fd: std::os::fd::RawFd) {
    let mut buf = [0u8; (std::mem::size_of::<libc::inotify_event>() + libc::NAME_MAX as usize + 1) * 16];
    loop {
        // SAFETY: reading into a local buffer from a valid non-blocking fd.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            return;
        }
    }
}

fn schedule(state: &mut AbyssState) {
    if state.catalog_pending {
        return;
    }
    let armed = state
        .loop_handle
        .insert_source(Timer::from_duration(DEBOUNCE), |_, _, state| {
            state.catalog_pending = false;
            if state.addons.hooks.is_on(crate::addons::Hook::TaskbarWidgets) {
                tracing::info!("widget catalog changed; re-reading the config");
                // The config files did not change, so the own-write check
                // would call this reload redundant; it is not.
                state.config_written.clear();
                super::watch::reload_now(state);
            }
            TimeoutAction::Drop
        });
    match armed {
        Ok(_) => state.catalog_pending = true,
        Err(e) => tracing::warn!(%e, "arming the widget catalog debounce"),
    }
}
