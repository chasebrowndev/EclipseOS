// SPDX-License-Identifier: AGPL-3.0-only
//! Where an ssh domain's panes live (T-22 §4, §5; owner decision 2026-11-14).
//!
//! Nothing is ever installed on the host. `remote.persist` (or the domain's `persist`)
//! picks the home of the panes:
//! - `off`: panes are local `apoth ssh --hold DEST` processes; with `sessions.server on`
//!   they are ordinary panes of the *local* session daemon (session `ssh-<domain>`), so
//!   they outlive the window; the remote shell ends with the link.
//! - `auto` (default): ask the host over the shared master whether `apoth` is on its PATH
//!   (`command -v apoth`, 5 s, `BatchMode`: no prompt). Present: the host's session daemon
//!   through `apoth proxy` (the remote shell survives a dropped link). Absent or no
//!   answer: silently `off`.
//! - `always`: the host's daemon; a host that says it has no `apoth` is a clear error. A
//!   host that cannot answer without a password is tried anyway (the askpass sheet asks;
//!   a missing `apoth` then ends the window with the same message).

use std::ffi::OsStr;
use std::time::Duration;

use apoth_pty::ssh::{self as sshx, HasApoth};

/// `remote.persist`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Persist {
    Off,
    Auto,
    Always,
}

impl Persist {
    /// The domain's own `persist` wins over the global key; unknown text is `auto` (the
    /// config validated both).
    pub(crate) fn resolve(domain: Option<&str>, global: Option<&str>) -> Persist {
        match domain.or(global) {
            Some("off") => Persist::Off,
            Some("always") => Persist::Always,
            _ => Persist::Auto,
        }
    }
}

/// The probe limit (owner decision: 5 s).
pub(crate) const PROBE_WITHIN: Duration = Duration::from_secs(5);

/// Use the host's daemon? `Err` is the message for `always` on a host without `apoth`.
pub(crate) fn use_remote(
    mode: Persist,
    probe: impl FnOnce() -> HasApoth,
    dest: &str,
) -> Result<bool, String> {
    match mode {
        Persist::Off => Ok(false),
        Persist::Auto => Ok(probe() == HasApoth::Yes),
        Persist::Always => match probe() {
            HasApoth::No => Err(format!(
                "{dest} has no `apoth` on its PATH, and this domain needs the host's session \
                 daemon (persist \"always\"). Install Apotheosis there with the package \
                 manager, or set persist \"auto\" or \"off\"."
            )),
            HasApoth::Yes | HasApoth::Unknown => Ok(true),
        },
    }
}

/// Probe `dest` with the system ssh over the private master.
pub(crate) fn probe(dest: &str) -> HasApoth {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from);
    let dir = runtime
        .as_deref()
        .and_then(|rt| sshx::prepare_control_dir(rt, rustix::process::getuid().as_raw()).ok());
    let master = sshx::master_args(dir.as_deref());
    let r = sshx::probe_apoth(OsStr::new("ssh"), &master, dest, PROBE_WITHIN);
    tracing::info!(result = ?r, "remote apoth probe");
    r
}

/// The local daemon session an ssh domain's panes live in.
pub(crate) fn local_session(domain: &str) -> String {
    let mut s: String = domain
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(60)
        .collect();
    if s.is_empty() {
        s.push('_');
    }
    format!("ssh-{s}")
}

/// The `apoth` next to this binary, else by `PATH`.
pub(crate) fn apoth_exe() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|e| Some(e.parent()?.join("apoth")))
        .filter(|p| p.is_file())
        .map_or_else(|| "apoth".to_owned(), |p| p.to_string_lossy().into_owned())
}

/// An ssh domain pane's command: `apoth ssh --hold [--cwd DIR] DEST`.
pub(crate) fn pane_argv(dest: &str, cwd: Option<String>) -> Vec<String> {
    let mut v = vec![apoth_exe(), "ssh".to_owned(), "--hold".to_owned()];
    if let Some(d) = cwd {
        v.extend(["--cwd".to_owned(), d]);
    }
    v.push(dest.to_owned());
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_domain_overrides_the_key() {
        assert_eq!(Persist::resolve(None, None), Persist::Auto);
        assert_eq!(Persist::resolve(None, Some("off")), Persist::Off);
        assert_eq!(
            Persist::resolve(Some("always"), Some("off")),
            Persist::Always
        );
        assert_eq!(Persist::resolve(Some("off"), Some("always")), Persist::Off);
    }

    #[test]
    fn modes() {
        let never = || -> HasApoth { panic!("off never probes") };
        assert_eq!(use_remote(Persist::Off, never, "h"), Ok(false));
        assert_eq!(use_remote(Persist::Auto, || HasApoth::Yes, "h"), Ok(true));
        assert_eq!(use_remote(Persist::Auto, || HasApoth::No, "h"), Ok(false));
        assert_eq!(
            use_remote(Persist::Auto, || HasApoth::Unknown, "h"),
            Ok(false)
        );
        assert_eq!(use_remote(Persist::Always, || HasApoth::Yes, "h"), Ok(true));
        assert_eq!(
            use_remote(Persist::Always, || HasApoth::Unknown, "h"),
            Ok(true)
        );
        let e = use_remote(Persist::Always, || HasApoth::No, "me@h").unwrap_err();
        assert!(e.contains("me@h") && e.contains("apoth"), "{e}");
    }

    #[test]
    fn sessions_and_argv() {
        assert_eq!(local_session("box"), "ssh-box");
        assert_eq!(local_session("my box/1"), "ssh-my_box_1");
        assert_eq!(local_session(""), "ssh-_");
        let a = pane_argv("me@h", Some("/srv".into()));
        assert_eq!(&a[1..], ["ssh", "--hold", "--cwd", "/srv", "me@h"]);
        assert_eq!(&pane_argv("h", None)[1..], ["ssh", "--hold", "h"]);
    }
}
