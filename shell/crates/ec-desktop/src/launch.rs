// SPDX-License-Identifier: AGPL-3.0-only
//! Activating an icon: a `.desktop` launcher runs the way the launcher runs
//! it (`ec_services::apps::launch`); anything else opens with `xdg-open`
//! (or `gio open`), detached.
//!
//! Errors are kinds, never names or paths.

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use ec_services::apps;

use crate::scan::{Entry, Kind};

/// Why an activation did nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// A `.desktop` file without the executable bit: the human has not
    /// allowed it to launch (`ops::allow_launching`).
    NotAllowed,
    /// A `.desktop` file that is not a launchable application.
    NotLaunchable,
    /// A symlink whose target is gone.
    Broken,
    /// Neither `xdg-open` nor `gio` is installed.
    NoOpener,
    /// The process would not start (or the entry wanted a terminal and none
    /// is configured).
    Spawn,
}

impl Error {
    /// The log word for this error.
    pub fn kind(self) -> &'static str {
        match self {
            Error::NotAllowed => "not-allowed",
            Error::NotLaunchable => "not-launchable",
            Error::Broken => "broken-link",
            Error::NoOpener => "no-opener",
            Error::Spawn => "spawn",
        }
    }
}

/// The argv that opens `path`: `xdg-open` when it is installed, else
/// `gio open`, else `None`. The two availabilities are parameters so the
/// choice is testable.
pub fn open_argv(path: &Path, has_xdg_open: bool, has_gio: bool) -> Option<Vec<OsString>> {
    if has_xdg_open {
        Some(vec!["xdg-open".into(), path.into()])
    } else if has_gio {
        Some(vec!["gio".into(), "open".into(), path.into()])
    } else {
        None
    }
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

/// Run `argv` as an ordinary child of the session: own process group, no
/// terminal, reaped off-thread.
fn spawn_detached(argv: &[OsString]) -> Result<(), Error> {
    let (program, args) = argv.split_first().ok_or(Error::Spawn)?;
    let mut cmd = Command::new(program);
    cmd.args(args)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().map_err(|_| Error::Spawn)?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Open `path` with the desktop's default handler. A folder opens in the file
/// manager the same way.
pub fn open(path: &Path) -> Result<(), Error> {
    let argv = open_argv(path, on_path("xdg-open"), on_path("gio")).ok_or(Error::NoOpener)?;
    spawn_detached(&argv)
}

/// A terminal in `dir` ("Open Terminal Here"): `term` is
/// `misc.terminal-command`, started with `dir` as its working directory.
/// Without a configured terminal it is refused rather than guessed.
pub fn terminal_here(term: Option<&str>, dir: &Path) -> Result<(), Error> {
    let term = term.filter(|t| !t.is_empty()).ok_or(Error::Spawn)?;
    let mut cmd = Command::new(term);
    cmd.current_dir(dir)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().map_err(|_| Error::Spawn)?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Settings, on `pane` (the argument `ec-settings` takes, as the bar's links
/// pass it).
pub fn settings(pane: &str) -> Result<(), Error> {
    spawn_detached(&["ec-settings".into(), pane.into()])
}

/// What activating `entry` does, decided without doing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    Launch,
    Open,
    Refuse(Error),
}

pub fn plan(entry: &Entry) -> Plan {
    match entry.kind {
        Kind::Launcher => match &entry.launcher {
            Some(l) if !l.trusted => Plan::Refuse(Error::NotAllowed),
            Some(l) if l.app.is_none() => Plan::Refuse(Error::NotLaunchable),
            Some(_) => Plan::Launch,
            None => Plan::Refuse(Error::NotLaunchable),
        },
        Kind::Symlink if entry.broken => Plan::Refuse(Error::Broken),
        _ => Plan::Open,
    }
}

/// Double-click: launch a trusted launcher, open anything else. `term` is
/// `misc.terminal-command`.
pub fn activate(entry: &Entry, term: Option<&str>) -> Result<(), Error> {
    match plan(entry) {
        Plan::Refuse(e) => Err(e),
        Plan::Open => open(&entry.path),
        Plan::Launch => {
            let app = entry.launcher.as_ref().and_then(|l| l.app.as_ref());
            let app = app.ok_or(Error::NotLaunchable)?;
            apps::launch(app, term).map_err(|_| Error::Spawn)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::parse_launcher;
    use std::path::PathBuf;

    fn entry(kind: Kind, launcher: Option<crate::scan::Launcher>, broken: bool) -> Entry {
        Entry {
            name: "x".into(),
            path: PathBuf::from("/d/x"),
            kind,
            mime: None,
            modified: None,
            size: 0,
            to_dir: false,
            broken,
            launcher,
        }
    }

    #[test]
    fn xdg_open_first_then_gio_then_nothing() {
        let p = Path::new("/d/a b.txt");
        assert_eq!(open_argv(p, true, true).unwrap()[0], "xdg-open");
        let g = open_argv(p, false, true).unwrap();
        assert_eq!((g[0].to_str(), g[1].to_str()), (Some("gio"), Some("open")));
        assert_eq!(g[2], p.as_os_str());
        assert!(open_argv(p, false, false).is_none());
    }

    #[test]
    fn only_an_allowed_launchable_launcher_launches() {
        let text = "[Desktop Entry]\nName=T\nExec=true\n";
        let ok = entry(
            Kind::Launcher,
            Some(parse_launcher("t.desktop", text, 0o755, None)),
            false,
        );
        assert_eq!(plan(&ok), Plan::Launch);
        let locked = entry(
            Kind::Launcher,
            Some(parse_launcher("t.desktop", text, 0o644, None)),
            false,
        );
        assert_eq!(plan(&locked), Plan::Refuse(Error::NotAllowed));
        let junk = entry(
            Kind::Launcher,
            Some(parse_launcher("t.desktop", "nope", 0o755, None)),
            false,
        );
        assert_eq!(plan(&junk), Plan::Refuse(Error::NotLaunchable));
    }

    #[test]
    fn files_open_and_dead_links_do_not() {
        assert_eq!(plan(&entry(Kind::File, None, false)), Plan::Open);
        assert_eq!(plan(&entry(Kind::Dir, None, false)), Plan::Open);
        assert_eq!(
            plan(&entry(Kind::Symlink, None, true)),
            Plan::Refuse(Error::Broken)
        );
    }
}
