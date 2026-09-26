// SPDX-License-Identifier: AGPL-3.0-only
//! Audio: the default sink's volume and mute, per-app mute, and the
//! visualizer's monitor tap (ADR 0065).
//!
//! Replaces the taskbar's `pactl` shell-outs. Spoken over the PulseAudio
//! native protocol (the `pulseaudio` crate), which pipewire-pulse serves, on
//! one blocking thread: no libpulse, no bindgen. Same shape as
//! [`crate::status`]: an `mpsc` feed the GUI drains with [`Handle::try_recv`]
//! and a separate [`Actions`] handle for changes.
//!
//! The monitor tap reads the default sink's monitor only while
//! [`Handle::set_tap`] is on. Samples are reduced to [`Bands`] in memory and
//! dropped: never stored, logged, or sent anywhere.
//!
//! Skeleton: the thread only keeps the channels alive. The protocol body
//! lands with the audio node of the taskbar-widgets work.

use std::sync::mpsc::{self, Receiver, Sender};

/// One change to the audio picture.
#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    /// `None`: no audio server, or no default sink.
    Sink(Option<Sink>),
    /// Every playing stream that belongs to a known process.
    Streams(Vec<Stream>),
    /// Monitor band levels, about 60 Hz while the tap is on.
    Levels(Bands),
}

/// The default sink.
#[derive(Debug, Clone, PartialEq)]
pub struct Sink {
    /// Linear, `1.0` = 100%. May exceed `1.0` when the sink is boosted.
    pub volume: f32,
    pub muted: bool,
    pub description: String,
}

/// A playback stream, keyed by the process that owns it, so a window's mute
/// can find it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stream {
    pub pid: u32,
    pub muted: bool,
}

/// Sixteen band levels, each `0.0..=1.0`, lowest frequency first.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bands(pub [f32; 16]);

// Read by the service body, which lands with its own node; `expect` fails
// the build the moment it does, so this cannot outlive the skeleton.
#[expect(dead_code, reason = "skeleton: the service body reads these")]
#[derive(Debug)]
enum Command {
    SetVolume(f32),
    SetMuted(bool),
    SetAppMuted { pid: u32, muted: bool },
    Tap(bool),
}

/// The GUI's end of the service.
pub struct Handle {
    updates: Receiver<Update>,
    commands: Sender<Command>,
}

impl Handle {
    /// The next change, if one is waiting. Never blocks.
    pub fn try_recv(&self) -> Option<Update> {
        self.updates.try_recv().ok()
    }

    /// A handle for volume and mute changes, cheap to clone into view closures.
    pub fn actions(&self) -> Actions {
        Actions {
            commands: self.commands.clone(),
        }
    }

    /// Open (`true`) or close (`false`) the monitor tap. The GUI turns it on
    /// only while media plays, the Now Playing widget is on screen and
    /// `bar.widgets.now-playing.visualizer` is set.
    pub fn set_tap(&self, on: bool) {
        let _ = self.commands.send(Command::Tap(on));
    }
}

/// Volume and mute changes. Every call returns at once; the service thread
/// does the round trip.
#[derive(Debug, Clone)]
pub struct Actions {
    commands: Sender<Command>,
}

impl Actions {
    /// Default-sink volume, linear, `1.0` = 100%.
    pub fn set_volume(&self, v: f32) {
        let _ = self.commands.send(Command::SetVolume(v));
    }

    pub fn set_muted(&self, m: bool) {
        let _ = self.commands.send(Command::SetMuted(m));
    }

    /// Mute or unmute every stream owned by `pid`.
    pub fn set_app_muted(&self, pid: u32, m: bool) {
        let _ = self.commands.send(Command::SetAppMuted { pid, muted: m });
    }
}

/// Start the service. The thread lives until the [`Handle`] and every
/// [`Actions`] cloned from it are dropped.
pub fn spawn() -> Handle {
    let (updates_tx, updates) = mpsc::channel();
    let (commands, commands_rx) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("eclipse-audio".into())
        .spawn(move || run(updates_tx, commands_rx));
    Handle { updates, commands }
}

fn run(_updates: Sender<Update>, commands: Receiver<Command>) {
    for _command in commands {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_starts_quiet() {
        let h = spawn();
        assert_eq!(h.try_recv(), None);
        let a = h.actions();
        a.clone().set_volume(0.5);
        a.set_muted(true);
        a.set_app_muted(1, true);
        h.set_tap(true);
        h.set_tap(false);
        assert_eq!(h.try_recv(), None);
    }
}
