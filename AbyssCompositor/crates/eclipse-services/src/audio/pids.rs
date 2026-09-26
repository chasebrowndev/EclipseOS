// SPDX-License-Identifier: AGPL-3.0-only
//! Which process a stream belongs to.
//!
//! A window and an audio stream share nothing but the process behind them:
//! the compositor reports a window's client pid, the audio server reports each
//! stream's `application.process.id`, and a stream belongs to a window when
//! its pid is that pid or a descendant of it. The descendant part is not
//! pedantry: a browser plays audio from a content child, never from the
//! process that owns the surface.

use std::collections::HashSet;

/// Is `pid` `ancestor`, or a descendant of it? Walks `/proc/<pid>/stat`'s
/// ppid upward. The `seen` set guards less against cycles than against a pid
/// being recycled underneath us mid-walk.
pub(super) fn descends_from(pid: u32, ancestor: u32) -> bool {
    let mut pid = pid;
    let mut seen = HashSet::new();
    while pid > 1 && seen.insert(pid) {
        if pid == ancestor {
            return true;
        }
        let Some(parent) = ppid_of(pid) else {
            return false;
        };
        pid = parent;
    }
    false
}

/// The parent of `pid`. `/proc/<pid>/stat` puts the command name in
/// parentheses in field 2, and the name may itself contain spaces and
/// parentheses, so the fields are read from after the *last* `)`.
fn ppid_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_ppid(&stat)
}

fn parse_ppid(stat: &str) -> Option<u32> {
    let tail = &stat[stat.rfind(')')? + 1..];
    tail.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_process_descends_from_itself() {
        assert!(descends_from(std::process::id(), std::process::id()));
    }

    #[test]
    fn a_process_descends_from_its_parent() {
        let me = std::process::id();
        let parent = ppid_of(me).unwrap();
        if parent > 1 {
            assert!(descends_from(me, parent));
        }
    }

    #[test]
    fn a_process_does_not_descend_from_its_own_child() {
        // pid 1 is nobody's descendant but its own.
        assert!(!descends_from(1, std::process::id()));
    }

    #[test]
    fn the_parent_of_this_process_is_readable() {
        assert!(ppid_of(std::process::id()).is_some_and(|p| p > 0));
    }

    #[test]
    fn a_command_name_with_parentheses_does_not_confuse_the_parse() {
        assert_eq!(parse_ppid("42 (a) b (c)) S 7 42 42 0"), Some(7));
        assert_eq!(parse_ppid("garbage"), None);
    }
}
