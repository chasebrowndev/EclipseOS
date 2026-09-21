// SPDX-License-Identifier: AGPL-3.0-only
//! Which windows are making noise, and muting them.
//!
//! A Wayland window and an audio stream have nothing in common except the
//! process behind them, so that is the join: the compositor reports a window's
//! client pid, PipeWire reports each playback stream's `application.process.id`,
//! and a stream belongs to a window when its pid is that pid or a descendant of
//! it. The descendant part is not pedantry — a browser plays audio from a
//! content child, never from the process that owns the surface.
//!
//! This shells out to `pactl`, which is how the rest of the session already
//! talks to PipeWire (the volume keybinds use `wpctl`). No daemon, no state:
//! every question is asked at the moment the menu opens, because a stream that
//! existed a second ago may not exist now. If `pactl` is missing or answers
//! something we do not understand, the answer is "no audio" and the menu
//! simply does not offer a Mute entry.

use std::collections::HashSet;
use std::process::Command;

use serde_json::Value;

/// The playback streams belonging to `pid` or any of its descendants, as
/// `(sink input index, muted)`.
fn streams_for(pid: i32) -> Vec<(u64, bool)> {
    let Ok(out) = Command::new("pactl")
        .args(["-f", "json", "list", "sink-inputs"])
        .output()
    else {
        return Vec::new();
    };
    let Ok(list): Result<Vec<Value>, _> = serde_json::from_slice(&out.stdout) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|s| {
            let index = s.get("index").and_then(Value::as_u64)?;
            let spid = s
                .get("properties")?
                .get("application.process.id")?
                .as_str()?
                .parse::<i32>()
                .ok()?;
            descends_from(spid, pid)
                .then_some((index, s.get("mute").and_then(Value::as_bool).unwrap_or(false)))
        })
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
pub fn state_of(pid: Option<i32>) -> Option<bool> {
    let streams = streams_for(pid?);
    (!streams.is_empty()).then(|| streams.iter().all(|&(_, muted)| muted))
}

/// Mute or unmute every stream this window's process owns.
pub fn set_mute(pid: Option<i32>, mute: bool) {
    let Some(pid) = pid else { return };
    for (index, _) in streams_for(pid) {
        let _ = Command::new("pactl")
            .args([
                "set-sink-input-mute",
                &index.to_string(),
                if mute { "1" } else { "0" },
            ])
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_process_descends_from_itself() {
        assert!(descends_from(
            std::process::id() as i32,
            std::process::id() as i32
        ));
    }

    #[test]
    fn a_process_does_not_descend_from_its_own_child() {
        // pid 1 is nobody's descendant but its own.
        assert!(!descends_from(1, std::process::id() as i32));
    }

    #[test]
    fn the_parent_of_this_process_is_readable() {
        assert!(ppid_of(std::process::id() as i32).is_some_and(|p| p > 0));
    }

    #[test]
    fn a_window_with_no_pid_has_no_audio() {
        assert_eq!(state_of(None), None);
    }
}
