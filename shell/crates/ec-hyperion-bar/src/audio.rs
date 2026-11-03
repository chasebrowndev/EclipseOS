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
//! The streams themselves come from `ec_services::audio` (ADR 0065), kept
//! on the app as the service last reported them; this module only answers
//! "which of these are this window's". Muting goes back through the service's
//! `set_app_muted`, one call per owning pid.

use std::collections::HashSet;

use ec_services::audio::{Device, Stream};

/// The audio picker's lists, as the service last reported them. Output
/// level and mute live on the volume widget's sink; the input's are the
/// default input device's own.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Devices {
    pub outputs: Vec<Device>,
    pub inputs: Vec<Device>,
    pub default_output: Option<String>,
    pub default_input: Option<String>,
}

impl Devices {
    /// Make `name` the default output now; the next report corrects it.
    pub fn pick_output(&mut self, name: &str) {
        if self.outputs.iter().any(|d| d.name == name) {
            self.default_output = Some(name.to_owned());
        }
    }

    pub fn pick_input(&mut self, name: &str) {
        if self.inputs.iter().any(|d| d.name == name) {
            self.default_input = Some(name.to_owned());
        }
    }

    /// The default input, if there is one.
    fn input_mut(&mut self) -> Option<&mut Device> {
        let name = self.default_input.as_ref()?;
        self.inputs.iter_mut().find(|d| &d.name == name)
    }

    pub fn set_input_volume(&mut self, v: f32) {
        let v = if v.is_finite() { v.max(0.0) } else { 0.0 };
        if let Some(d) = self.input_mut() {
            d.volume = v;
        }
    }

    pub fn set_input_muted(&mut self, m: bool) {
        if let Some(d) = self.input_mut() {
            d.muted = m;
        }
    }
}

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

    fn dev(name: &str) -> Device {
        Device {
            name: name.into(),
            description: name.into(),
            volume: 0.5,
            muted: false,
            kind: ec_services::audio::DeviceKind::Other,
        }
    }

    #[test]
    fn picking_a_device_is_optimistic_and_ignores_strangers() {
        let mut d = Devices {
            outputs: vec![dev("a"), dev("b")],
            inputs: vec![dev("m")],
            default_output: Some("a".into()),
            default_input: Some("m".into()),
        };
        d.pick_output("b");
        assert_eq!(d.default_output.as_deref(), Some("b"));
        d.pick_output("nope");
        assert_eq!(d.default_output.as_deref(), Some("b"));
        d.pick_input("nope");
        assert_eq!(d.default_input.as_deref(), Some("m"));
        d.set_input_volume(0.8);
        d.set_input_muted(true);
        assert_eq!((d.inputs[0].volume, d.inputs[0].muted), (0.8, true));
        d.set_input_volume(f32::NAN);
        assert_eq!(d.inputs[0].volume, 0.0);
    }

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
