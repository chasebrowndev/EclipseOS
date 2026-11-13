// SPDX-License-Identifier: AGPL-3.0-only
//! Hot reload plumbing (T-10 §4): the inotify watch set, the debounce constants and the pure
//! The diff itself and its subsystem mapping are `apoth_config::reload` (`Reloader`,
//! `Applied`); the loop wiring is in `app.rs`. Nothing here reads or logs configuration values, only paths and key names.

use std::collections::{BTreeSet, HashSet};
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rustix::fs::inotify::{self, CreateFlags, WatchFlags};
use rustix::io::Errno;

/// Events within this window collapse into one reload.
pub(crate) const DEBOUNCE: Duration = Duration::from_millis(100);
/// Poll period where inotify cannot be set up.
pub(crate) const POLL_PERIOD: Duration = Duration::from_secs(2);

/// How the config files are being watched (`apoth doctor` reports it; T-23 `IDLE` excludes
/// the poll timer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WatchMode {
    Inotify,
    Poll,
}

/// The directories to watch for `files`: the parent of each file and of its canonical target
/// (a symlink swap changes the link's directory or the target's), each replaced by its nearest
/// existing ancestor when it does not exist yet. Watching directories, not files, is what
/// survives an editor's rename-replace.
pub(crate) fn watch_dirs(files: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = BTreeSet::new();
    for f in files {
        let mut cands = vec![f.clone()];
        if let Ok(c) = f.canonicalize() {
            cands.push(c);
        }
        for c in cands {
            if let Some(d) = nearest_dir(c.parent()) {
                out.insert(d);
            }
        }
    }
    out.into_iter().collect()
}

fn nearest_dir(mut cur: Option<&Path>) -> Option<PathBuf> {
    while let Some(p) = cur {
        if !p.as_os_str().is_empty() && p.is_dir() {
            return Some(p.to_path_buf());
        }
        cur = p.parent();
    }
    None
}

/// One inotify instance and the directories it watches.
#[derive(Debug)]
pub(crate) struct Watcher {
    fd: OwnedFd,
    watched: HashSet<PathBuf>,
}

impl Watcher {
    pub(crate) fn new() -> io::Result<Watcher> {
        let fd = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK)?;
        Ok(Watcher {
            fd,
            watched: HashSet::new(),
        })
    }

    /// A second handle on the same instance, for the event loop to poll.
    pub(crate) fn poll_fd(&self) -> io::Result<OwnedFd> {
        self.fd.try_clone()
    }

    /// Watch the directories of `files` that are not watched yet. Returns how many could not
    /// be added.
    pub(crate) fn sync(&mut self, files: &[PathBuf]) -> usize {
        let flags = WatchFlags::CREATE
            | WatchFlags::DELETE
            | WatchFlags::MODIFY
            | WatchFlags::CLOSE_WRITE
            | WatchFlags::MOVED_TO
            | WatchFlags::MOVED_FROM
            | WatchFlags::ATTRIB
            | WatchFlags::DELETE_SELF
            | WatchFlags::MOVE_SELF
            | WatchFlags::ONLYDIR;
        let mut failed = 0;
        // Re-add every directory on each call: `add_watch` on an inode that is already
        // watched returns the existing watch, while a directory that was removed or renamed
        // and recreated at the same path is a new inode and needs a new watch. The set is
        // rebuilt so it only holds paths that were watchable just now.
        let mut now = HashSet::new();
        for d in watch_dirs(files) {
            match inotify::add_watch(&self.fd, &d, flags) {
                Ok(_) => {
                    now.insert(d);
                }
                Err(e) => {
                    failed += 1;
                    tracing::warn!(errno = e.raw_os_error(), "cannot watch a config directory");
                }
            }
        }
        self.watched = now;
        failed
    }

    pub(crate) fn watched(&self) -> usize {
        self.watched.len()
    }

    /// Discard every queued event. True when there was at least one. Names are irrelevant: a
    /// reload that finds no difference costs one parse on a worker.
    pub(crate) fn drain(&self) -> bool {
        let mut buf = [0u8; 4096];
        let mut any = false;
        loop {
            match rustix::io::read(self.fd.as_fd(), &mut buf) {
                Ok(0) => return any,
                Ok(_) => any = true,
                Err(Errno::INTR) => {}
                Err(_) => return any,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("apoth-reload-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn missing_directories_fall_back_to_an_existing_ancestor() {
        let d = scratch("anc");
        let f = d.join("nope/deeper/apotheosis.kdl");
        assert_eq!(watch_dirs(&[f]), vec![d]);
    }

    #[test]
    fn symlink_target_directory_is_watched_too() {
        let d = scratch("sym");
        std::fs::create_dir_all(d.join("real")).unwrap();
        std::fs::create_dir_all(d.join("conf")).unwrap();
        std::fs::write(d.join("real/a.kdl"), "x").unwrap();
        std::os::unix::fs::symlink(d.join("real/a.kdl"), d.join("conf/a.kdl")).unwrap();
        let dirs = watch_dirs(&[d.join("conf/a.kdl")]);
        assert!(dirs.contains(&d.join("conf")));
        assert!(dirs.iter().any(|p| p.ends_with("real")));
    }

    fn readable(fd: &OwnedFd, ms: i32) -> bool {
        use rustix::event::{PollFd, PollFlags, Timespec, poll};
        let mut pfd = [PollFd::new(fd, PollFlags::IN)];
        let ts = Timespec {
            tv_sec: i64::from(ms / 1000),
            tv_nsec: i64::from(ms % 1000) * 1_000_000,
        };
        poll(&mut pfd, Some(&ts)).is_ok_and(|n| n > 0)
    }

    #[test]
    fn rename_replace_wakes_the_watcher() {
        let d = scratch("rr");
        let file = d.join("apotheosis.kdl");
        std::fs::write(&file, "a").unwrap();
        let mut w = Watcher::new().unwrap();
        assert_eq!(w.sync(std::slice::from_ref(&file)), 0);
        assert_eq!(w.watched(), 1);
        let fd = w.poll_fd().unwrap();
        assert!(!readable(&fd, 0));
        // An editor writes a temp file next to it and renames it over the original.
        let tmp = d.join(".apotheosis.kdl.swp");
        std::fs::write(&tmp, "b").unwrap();
        std::fs::rename(&tmp, &file).unwrap();
        assert!(readable(&fd, 2000));
        assert!(w.drain());
        assert!(!w.drain());
        // The watch survives the replace: a second one is seen too.
        std::fs::write(&tmp, "c").unwrap();
        std::fs::rename(&tmp, &file).unwrap();
        assert!(readable(&fd, 2000));
    }

    #[test]
    fn a_directory_recreated_at_the_same_path_is_watched_again() {
        let d = scratch("recreate");
        let sub = d.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let file = sub.join("apotheosis.kdl");
        std::fs::write(&file, "a").unwrap();
        let mut w = Watcher::new().unwrap();
        w.sync(std::slice::from_ref(&file));
        let fd = w.poll_fd().unwrap();
        std::fs::rename(&sub, d.join("sub.old")).unwrap();
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(&file, "b").unwrap();
        assert!(w.drain());
        w.sync(std::slice::from_ref(&file));
        std::fs::write(&file, "c").unwrap();
        assert!(readable(&fd, 2000));
        assert!(w.drain());
    }

    #[test]
    fn a_directory_created_later_is_picked_up_by_sync() {
        let d = scratch("late");
        let file = d.join("sub/apotheosis.kdl");
        let mut w = Watcher::new().unwrap();
        w.sync(std::slice::from_ref(&file));
        assert_eq!(w.watched(), 1); // the nearest existing ancestor
        std::fs::create_dir_all(d.join("sub")).unwrap();
        w.sync(std::slice::from_ref(&file));
        // The set follows the files' real parent now; the ancestor's kernel watch is harmless.
        assert_eq!(w.watched(), 1);
        assert!(w.watched.contains(&d.join("sub")));
    }
}
