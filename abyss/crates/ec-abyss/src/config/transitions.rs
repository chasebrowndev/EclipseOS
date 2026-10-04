// SPDX-License-Identifier: AGPL-3.0-only
//! The add-on transition-shader catalog (ADR 0073; hook `transition-shaders`).
//! Reading lives in `ec_abyss_config::transitions`; this half decides when it
//! is read and watches the directory with inotify on the compositor loop,
//! debounced, like the widget catalog (`catalog.rs`).
//!
//! The catalog is read only while the hook is on. Off, the render registry is
//! empty, so every `pack:style` in the config falls back to the built-in
//! style and no shader source is ever read.

use std::{
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::{Path, PathBuf},
    time::Duration,
};

use smithay::reexports::calloop::{
    generic::Generic,
    timer::{TimeoutAction, Timer},
    Interest, LoopHandle, Mode, PostAction,
};

use ec_abyss_config::transitions::{self, TRANSITIONS_DIR};

use crate::addons::Hook;
use crate::state::AbyssState;

const DEBOUNCE: Duration = Duration::from_millis(100);

#[cfg(test)]
thread_local! {
    static TEST_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub fn set_test_root(root: Option<PathBuf>) {
    TEST_ROOT.with(|r| *r.borrow_mut() = root);
}

fn root() -> PathBuf {
    #[cfg(test)]
    if let Some(r) = TEST_ROOT.with(|r| r.borrow().clone()) {
        return r;
    }
    PathBuf::from(TRANSITIONS_DIR)
}

/// The styles to install: the catalog while the hook is on, nothing (and no
/// file read) while it is off.
fn styles_for(hook_on: bool, root: &Path) -> Vec<transitions::TransitionStyle> {
    if !hook_on {
        return Vec::new();
    }
    let catalog = transitions::load_from(root);
    tracing::info!(
        styles = catalog.styles.len(),
        rejected = catalog.rejected.len(),
        "transition shader catalog read"
    );
    catalog.styles
}

/// Bring the render registry in line with the hook.
pub fn sync(state: &mut AbyssState) {
    let on = state.addons.hooks.is_on(Hook::TransitionShaders);
    state.borders.anim.shaders.set_catalog(styles_for(on, &root()));
    // A running animation may now draw differently.
    crate::backend::damage_all(state);
}

/// Watch the catalog root (and its parent, so the first install is seen).
pub fn start(handle: &LoopHandle<'static, AbyssState>) {
    // SAFETY: `inotify_init1` takes only flags and returns a new fd or -1.
    let raw = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
    if raw < 0 {
        tracing::warn!(error = %std::io::Error::last_os_error(), "inotify unavailable; transition catalog changes need a restart");
        return;
    }
    // SAFETY: `raw` is a fresh, owned, valid descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let dir = root();
    let parent = dir.parent().unwrap_or(Path::new("/")).to_path_buf();
    let raw = fd.as_raw_fd();
    let watched = [add_watch(raw, &parent), add_watch(raw, &dir)];
    if !watched.contains(&true) {
        tracing::debug!(dir = TRANSITIONS_DIR, "no transition catalog directory to watch");
        return;
    }
    let inserted = handle.insert_source(Generic::new(fd, Interest::READ, Mode::Level), |_, fd, state| {
        drain(fd.as_raw_fd());
        // Re-arm: a pack directory created since is a new watch target.
        let root = root();
        add_watch(fd.as_raw_fd(), &root);
        if let Ok(rd) = std::fs::read_dir(&root) {
            for pack in rd.flatten() {
                add_watch(fd.as_raw_fd(), &pack.path());
            }
        }
        schedule(state);
        Ok(PostAction::Continue)
    });
    if let Err(e) = inserted {
        tracing::warn!(%e, "inserting the transition catalog watcher");
        return;
    }
    // Packs already installed.
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for pack in rd.flatten() {
            add_watch(raw, &pack.path());
        }
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
    if state.transitions_pending {
        return;
    }
    let armed = state
        .loop_handle
        .insert_source(Timer::from_duration(DEBOUNCE), |_, _, state| {
            state.transitions_pending = false;
            if state.addons.hooks.is_on(Hook::TransitionShaders) {
                tracing::info!("transition catalog changed; re-reading it");
                sync(state);
            }
            TimeoutAction::Drop
        });
    match armed {
        Ok(_) => state.transitions_pending = true,
        Err(e) => tracing::warn!(%e, "arming the transition catalog debounce"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("abyss-transitions-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("fx");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("ripple.kdl"),
            "label \"Ripple\"\nevents \"window-close\"\nshader \"ripple.frag\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ripple.frag"),
            "void main() { gl_FragColor = vec4(0.0); }\n",
        )
        .unwrap();
        root
    }

    #[test]
    fn hook_off_ignores_the_catalog() {
        let root = pack("off");
        assert!(styles_for(false, &root).is_empty());
        let on = styles_for(true, &root);
        assert_eq!(on.len(), 1);
        assert_eq!(on[0].id, "fx:ripple");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_test_root_overrides_the_install_dir() {
        let root = pack("root");
        set_test_root(Some(root.clone()));
        assert_eq!(super::root(), root);
        set_test_root(None);
        assert_eq!(super::root(), PathBuf::from(TRANSITIONS_DIR));
        let _ = std::fs::remove_dir_all(&root);
    }
}
