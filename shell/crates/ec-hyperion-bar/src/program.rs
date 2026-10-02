// SPDX-License-Identifier: AGPL-3.0-only
//! A terminal window → the program in its foreground.
//!
//! A terminal's `app_id` is `foot` whatever runs inside it, so a condensed
//! chip for a terminal running Claude Code would read `foot`. This module
//! looks one level down: the terminal's child is its shell, and the shell's
//! controlling terminal has a foreground process group (`tpgid`). When that
//! group is not the shell itself, its leader is the program the human is
//! looking at.
//!
//! The `/proc` read happens on every snapshot refresh — two tiny reads per
//! child — because the foreground program changes without the compositor
//! saying anything. The `comm` → display-name lookup touches desktop files
//! and is cached per `comm`, hits and misses alike.
//!
//! A `secret` window is never read at all, exactly as in `icons::warm`.

use std::collections::HashMap;

use crate::model::{Trust, Window};

/// The `comm` → display-name cache.
#[derive(Debug, Default)]
pub struct Programs {
    names: HashMap<String, String>,
    /// Basenames from `/etc/shells`, read once on first use.
    shells: Option<Vec<String>>,
}

impl Programs {
    pub fn new() -> Self {
        Programs::default()
    }

    /// Set [`Window::program`] on every window from `/proc`.
    pub fn fill(&mut self, windows: &mut [Window]) {
        for w in windows {
            w.program = None;
            if w.trust == Trust::Secret {
                continue;
            }
            let Some(pid) = w.pid.filter(|p| *p > 0) else {
                continue;
            };
            let shells = self.shells.get_or_insert_with(read_shells);
            if let Some(comm) = foreground(pid, shells) {
                w.program = Some(self.name(&comm));
            }
        }
    }

    fn name(&mut self, comm: &str) -> String {
        self.names
            .entry(comm.to_owned())
            .or_insert_with(|| desktop_name(comm).unwrap_or_else(|| comm.to_owned()))
            .clone()
    }
}

/// The `comm` of the foreground program under client `pid`.
///
/// For each child of the client (normally its shell): when the terminal's
/// foreground group is some other group, its leader is the program. When the
/// child is its own foreground group it is either an idle interactive shell
/// (`None`) or a program the terminal ran directly — `foot -e htop`, or
/// `sh -c 'sleep 600'`, which execs `sleep` in place — and that is named.
/// No controlling terminal (`tpgid` -1) is `None`.
fn foreground(pid: i32, shells: &[String]) -> Option<String> {
    let children = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).ok()?;
    children
        .split_whitespace()
        .filter_map(|c| c.parse::<i32>().ok())
        .find_map(|child| {
            let stat = std::fs::read_to_string(format!("/proc/{child}/stat")).ok()?;
            let tpgid = tpgid(&stat)?;
            if tpgid <= 0 {
                return None;
            }
            let comm = std::fs::read_to_string(format!("/proc/{tpgid}/comm")).ok()?;
            let comm = comm.trim();
            if comm.is_empty() || (tpgid == child && is_shell(comm, shells)) {
                return None;
            }
            Some(comm.to_owned())
        })
}

/// The basenames of the login shells in `/etc/shells` (`zsh`, `bash`, …).
fn read_shells() -> Vec<String> {
    std::fs::read_to_string("/etc/shells")
        .map(|body| shells_of(&body))
        .unwrap_or_default()
}

fn shells_of(body: &str) -> Vec<String> {
    body.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.rsplit('/').next())
        .map(str::to_owned)
        .collect()
}

/// `comm` is truncated to 15 bytes by the kernel, so compare on that much.
fn is_shell(comm: &str, shells: &[String]) -> bool {
    shells
        .iter()
        .any(|s| s == comm || (comm.len() == 15 && s.starts_with(comm)))
}

/// Field 8 of `/proc/<pid>/stat`. `comm` (field 2) is parenthesised and may
/// itself contain spaces and `)`, so the fields are counted from after the
/// *last* `)`: state, ppid, pgrp, session, tty_nr, tpgid.
pub(crate) fn tpgid(stat: &str) -> Option<i32> {
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(5)?.parse().ok()
}

/// The `Name=` key of `<comm>.desktop`, searched like `icons::desktop_icon_key`.
fn desktop_name(comm: &str) -> Option<String> {
    let file = format!("{comm}.desktop");
    ec_services::apps::search_path()
        .into_iter()
        .filter_map(|dir| std::fs::read_to_string(dir.join(&file)).ok())
        .find_map(|body| name_key(&body))
}

/// Pull `Name=` out of the `[Desktop Entry]` group of a desktop file. A
/// `[Desktop Action ...]` group has its own `Name=` and it is not the
/// application's. Localised `Name[xx]=` keys do not match the prefix.
pub(crate) fn name_key(body: &str) -> Option<String> {
    let mut in_entry = false;
    for line in body.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        if let Some(value) = line.strip_prefix("Name=") {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tpgid_is_field_eight() {
        let stat = "3975 (zsh) S 3974 3975 3975 34817 10687 4194304 1 2 3";
        assert_eq!(tpgid(stat), Some(10687));
    }

    #[test]
    fn tpgid_survives_a_comm_with_spaces_and_parens() {
        let stat = "42 (a) b (c)) S 1 42 42 34817 99 0 0";
        assert_eq!(tpgid(stat), Some(99));
    }

    #[test]
    fn no_controlling_terminal_is_minus_one() {
        assert_eq!(tpgid("7 (daemon) S 1 7 7 0 -1 0"), Some(-1));
    }

    #[test]
    fn a_truncated_stat_is_none() {
        assert_eq!(tpgid("7 (x) S 1"), None);
        assert_eq!(tpgid("garbage"), None);
    }

    #[test]
    fn an_idle_shell_is_not_a_program() {
        let shells = shells_of("# /etc/shells\n/bin/sh\n/usr/bin/zsh\n\n/bin/bash\n");
        assert_eq!(shells, ["sh", "zsh", "bash"]);
        assert!(is_shell("zsh", &shells));
        assert!(!is_shell("sleep", &shells));
        assert!(!is_shell("claude", &shells));
    }

    #[test]
    fn name_key_ignores_action_groups_and_locales() {
        let body =
            "[Desktop Action new]\nName=New Window\n\n[Desktop Entry]\nName[de]=Kralle\nName=Claude Code\n";
        assert_eq!(name_key(body).as_deref(), Some("Claude Code"));
        assert_eq!(name_key("[Desktop Action x]\nName=Nope\n"), None);
    }

    /// The policy invariant, mirrored from `icons`: a secret window is not
    /// read, so even a live pid leaves `program` empty.
    #[test]
    fn a_secret_window_is_never_read() {
        let mut programs = Programs::new();
        let mut w = Window {
            handle: 1,
            app_id: "foot".into(),
            title: "t".into(),
            workspace: None,
            output: None,
            focused: false,
            minimized: false,
            pid: Some(std::process::id() as i32),
            program: Some("stale".into()),
            trust: Trust::Secret,
        };
        programs.fill(std::slice::from_mut(&mut w));
        assert_eq!(w.program, None);
        assert!(programs.names.is_empty());
    }
}
