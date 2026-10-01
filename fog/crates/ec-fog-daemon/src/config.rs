// SPDX-License-Identifier: AGPL-3.0-only

//! `fog.kdl` in ec-fogd (FOG §Configuration): loaded at start, applied, and
//! hot-reloaded from an inotify watch on the config directory. The watch is
//! its own, separate from the listing watcher. A rejected file is logged and
//! broadcast as [`Reply::ConfigError`]; the last valid config stays active.

use std::ffi::OsString;
use std::io;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use ec_fog_config::{Config, Error, Live};
use ec_fog_proto::{Reply, Sort, SortKey};
use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use rustix::io::Errno;

use crate::Daemon;

/// Load `path` (defaults if it is missing or rejected), apply it to `daemon`
/// and start watching it. Watch failures are logged: Fog runs on without
/// hot reload rather than not at all.
pub fn start(daemon: &Arc<Daemon>, path: PathBuf) -> Live {
    let live = Live::new(match ec_fog_config::load(&path) {
        Ok(c) => c,
        Err(e) => {
            rejected(daemon, &e);
            ec_fog_config::defaults()
        }
    });
    apply(daemon, live.current());
    if let Err(e) = watch(daemon.clone(), path, live.clone()) {
        tracing::warn!(error = %e, "fog.kdl hot reload unavailable");
    }
    live
}

/// Push the config's daemon-side settings into `daemon`.
pub fn apply(daemon: &Daemon, c: &Config) {
    let p = &c.performance;
    daemon.cache().set_limits(p.cache_dirs, p.cache_bytes());
    daemon.set_default_sort(default_sort(&c.view));
}

/// `view { sort "<key>" dirs-first=… }` as the wire sort. Names always
/// compare naturally in `ec-fogd`.
pub fn default_sort(v: &ec_fog_config::View) -> Sort {
    Sort {
        key: match v.sort {
            ec_fog_config::SortKey::Name => SortKey::Name,
            ec_fog_config::SortKey::Size => SortKey::Size,
            ec_fog_config::SortKey::Modified => SortKey::Modified,
            ec_fog_config::SortKey::Type => SortKey::Type,
        },
        reverse: false,
        dirs_first: v.dirs_first,
    }
}

/// Re-read `path` into `live`; on success apply it and broadcast the text
/// as [`Reply::ConfigReloaded`], so running UIs restyle and rebind; on error
/// log and broadcast, leaving `live` as it was.
pub fn reload(daemon: &Daemon, path: &Path, live: &mut Live) {
    let res = ec_fog_config::read(path).and_then(|text| {
        let changed = live.apply(ec_fog_config::parse(&text))?;
        Ok(changed.then_some(text))
    });
    match res {
        Ok(Some(text)) => {
            apply(daemon, live.current());
            tracing::info!(path = %path.display(), "fog.kdl reloaded");
            daemon.broadcast(Reply::ConfigReloaded { text });
        }
        Ok(None) => {}
        Err(e) => rejected(daemon, &e),
    }
}

fn rejected(daemon: &Daemon, e: &Error) {
    tracing::warn!(error = %e, "fog.kdl rejected; previous config stays active");
    daemon.broadcast(Reply::ConfigError {
        line: e.line,
        col: e.col,
        msg: e.msg.clone(),
    });
}

/// Watch `path`'s directory (created if missing) on a dedicated thread and
/// reload whenever `path` is written, renamed into place or removed.
pub fn watch(
    daemon: Arc<Daemon>,
    path: PathBuf,
    mut live: Live,
) -> io::Result<thread::JoinHandle<()>> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(Errno::INVAL.into());
    };
    let name: OsString = name.to_owned();
    std::fs::create_dir_all(dir)?;
    let fd = inotify::init(CreateFlags::CLOEXEC)?;
    inotify::add_watch(
        &fd,
        dir,
        WatchFlags::CLOSE_WRITE
            | WatchFlags::MOVED_TO
            | WatchFlags::MOVED_FROM
            | WatchFlags::DELETE
            | WatchFlags::ONLYDIR,
    )?;
    thread::Builder::new()
        .name("ec-fog-config".into())
        .spawn(move || {
            let mut buf = [MaybeUninit::uninit(); 4096];
            let mut rd = inotify::Reader::new(&fd, &mut buf);
            loop {
                let hit = match rd.next() {
                    Ok(ev) => {
                        ev.events().contains(ReadFlags::QUEUE_OVERFLOW)
                            || ev
                                .file_name()
                                .is_some_and(|n| n.to_bytes() == name.as_bytes())
                    }
                    Err(Errno::INTR) => continue,
                    Err(e) => {
                        tracing::warn!(error = %e, "fog.kdl watch ended");
                        return;
                    }
                };
                if hit {
                    reload(&daemon, &path, &mut live);
                }
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn hot_reload_applies_and_rejects() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("eclipse").join("fog.kdl");
        // Not `local()`: tests never open the real journal.
        let daemon = Arc::new(Daemon::new(
            Box::new(crate::LocalBackend),
            crate::Cache::default(),
        ));
        let mut events = daemon.subscribe();

        let live = start(&daemon, path.clone());
        assert_eq!(live.current(), &ec_fog_config::defaults());

        let text = "performance { cache-dirs 3 }\nappearance { reduce-motion #true; }\n";
        std::fs::write(&path, text).unwrap();
        let t0 = std::time::Instant::now();
        while daemon.cache().max_dirs() != 3 {
            assert!(t0.elapsed() < Duration::from_secs(5), "reload not applied");
            thread::sleep(Duration::from_millis(10));
        }
        // Running UIs are told, with the text they lay over the defaults.
        let t0 = std::time::Instant::now();
        let pushed = loop {
            match events.try_recv() {
                Ok(Reply::ConfigReloaded { text }) => break text,
                Ok(r) => panic!("unexpected {r:?}"),
                Err(_) => {
                    assert!(t0.elapsed() < Duration::from_secs(5), "no ConfigReloaded");
                    thread::sleep(Duration::from_millis(10));
                }
            }
        };
        assert_eq!(pushed, text);
        let over = ec_fog_config::parse(&pushed).unwrap();
        assert!(over.appearance.reduce_motion && !over.appearance.reduce_transparency);
        assert_eq!(over.performance.cache_dirs, 3);

        std::fs::write(&path, "performance { cache-dirs 3 }\nbogus\n").unwrap();
        let t0 = std::time::Instant::now();
        let reply = loop {
            match events.try_recv() {
                Ok(r) => break r,
                Err(_) => {
                    assert!(t0.elapsed() < Duration::from_secs(5), "no ConfigError");
                    thread::sleep(Duration::from_millis(10));
                }
            }
        };
        match reply {
            Reply::ConfigError { line, col, .. } => assert_eq!((line, col), (2, 1)),
            r => panic!("unexpected {r:?}"),
        }
        assert_eq!(daemon.cache().max_dirs(), 3, "old config stays active");
    }
}
