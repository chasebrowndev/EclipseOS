// SPDX-License-Identifier: AGPL-3.0-only
//! Which windows are making noise.
//!
//! A Wayland window and an audio stream have nothing in common except the
//! process behind them, so that is the join: the compositor reports a window's
//! client pid, the audio service reports each playback stream's owning pid,
//! and a stream belongs to a window when its pid is that pid or a descendant of
//! it. The descendant part is not pedantry — a browser plays audio from a
//! content child, never from the process that owns the surface.
//!
//! The streams themselves come from `eclipse_services::audio` (ADR 0065), kept
//! on the app as the service last reported them; this module only answers
//! "which of these are this window's". Muting goes back through the service's
//! `set_app_muted`, one call per owning pid.

use std::collections::HashSet;

use eclipse_services::audio::Stream;

/// The streams in `streams` that belong to `pid` or any of its descendants.
pub fn streams_for(streams: &[Stream], pid: i32) -> Vec<Stream> {
    streams
        .iter()
        .filter(|s| i32::try_from(s.pid).is_ok_and(|spid| descends_from(spid, pid)))
        .copied()
        .collect()
}

/// Is `pid` `ancestor`, or a child of it? Walks `/proc/<pid>/stat`'s ppid
/// upward. The `seen` set is not paranoia about cycles so much as about a pid
/// being recycled underneath us mid-walk.
fn descends_from(mut pid: i32, ancestor: i32) -> bool {
    let mut seen = HashSet::new();
    while pid > 1 && seen.insert(pid) {
        if pid == ancestor {
            return true;
        }
        let Some(parent) = ppid_of(pid) else { return false };
        pid = parent;
    }
    false
}

/// The parent of `pid`. `/proc/<pid>/stat` puts the command name in
/// parentheses in field 2, and the name may itself contain spaces and
/// parentheses, so the fields are read from after the *last* `)`.
fn ppid_of(pid: i32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let tail = &stat[stat.rfind(')')? + 1..];
    tail.split_whitespace().nth(1)?.parse().ok()
}

/// `Some(muted)` when this window's process has playback streams, `None` when
/// it has none — which is the menu's cue to leave the entry out entirely.
pub fn state_of(streams: &[Stream], pid: Option<i32>) -> Option<bool> {
    let mine = streams_for(streams, pid?);
    (!mine.is_empty()).then(|| mine.iter().all(|s| s.muted))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> i32 {
        std::process::id() as i32
    }

    #[test]
    fn a_process_descends_from_itself() {
        assert!(descends_from(me(), me()));
    }

    #[test]
    fn a_process_does_not_descend_from_its_own_child() {
        // pid 1 is nobody's descendant but its own.
        assert!(!descends_from(1, me()));
    }

    #[test]
    fn the_parent_of_this_process_is_readable() {
        assert!(ppid_of(me()).is_some_and(|p| p > 0));
    }

    #[test]
    fn a_window_with_no_pid_has_no_audio() {
        assert_eq!(state_of(&[], None), None);
    }

    /// A stream owned by this very process is this process's; all muted reads
    /// as muted, one live stream as not.
    #[test]
    fn a_windows_streams_are_joined_by_pid() {
        let pid = std::process::id();
        let mine = |muted| Stream { pid, muted };
        let theirs = Stream { pid: 1, muted: false };
        assert_eq!(state_of(&[theirs], Some(me())), None);
        assert_eq!(state_of(&[mine(true), theirs], Some(me())), Some(true));
        assert_eq!(state_of(&[mine(true), mine(false)], Some(me())), Some(false));
        assert_eq!(streams_for(&[mine(true), theirs], me()).len(), 1);
    }
}
