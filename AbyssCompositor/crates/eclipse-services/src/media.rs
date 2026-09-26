// SPDX-License-Identifier: AGPL-3.0-only
//! Now Playing: the active MPRIS player on the session bus (ADR 0065).
//!
//! Feeds the taskbar's `now-playing` widget. Same shape as [`crate::status`]:
//! one watcher thread, an `mpsc` feed the GUI drains with [`Handle::try_recv`],
//! and a separate [`Actions`] handle for the transport buttons, so a click is
//! never answered on a draw path.
//!
//! Skeleton: the thread only keeps the channels alive. The MPRIS body lands
//! with the media node of the taskbar-widgets work.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

/// One change to what is playing.
#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    /// `None`: nothing playing, or a player sitting idle. The widget
    /// compresses to zero width rather than drawing an empty card.
    Player(Option<NowPlaying>),
}

/// The player the widget shows.
#[derive(Debug, Clone, PartialEq)]
pub struct NowPlaying {
    /// MPRIS bus-name suffix, e.g. `spotify` for `org.mpris.MediaPlayer2.spotify`.
    pub player: String,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub art: Option<Art>,
    pub status: Playback,
    pub can_prev: bool,
    pub can_next: bool,
    pub can_pause: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Playback {
    Playing,
    Paused,
    Stopped,
}

/// Cover art, read from a `file://` `mpris:artUrl`. An `http(s)` URL is not
/// fetched: the service makes no network requests on a player's say-so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Art {
    /// Stable identity for the image (the resolved URL), so the GUI can cache
    /// the decoded handle and skip re-decoding unchanged art.
    pub key: String,
    pub bytes: Arc<[u8]>,
}

#[derive(Debug)]
enum Command {
    Previous,
    PlayPause,
    Next,
}

/// The GUI's end of the watcher.
pub struct Handle {
    updates: Receiver<Update>,
    commands: Sender<Command>,
}

impl Handle {
    /// The next change, if one is waiting. Never blocks.
    pub fn try_recv(&self) -> Option<Update> {
        self.updates.try_recv().ok()
    }

    /// A handle for the transport buttons, cheap to clone into view closures.
    pub fn actions(&self) -> Actions {
        Actions {
            commands: self.commands.clone(),
        }
    }
}

/// Transport controls for the active player. Every call returns at once; the
/// watcher thread does the bus round trip.
#[derive(Debug, Clone)]
pub struct Actions {
    commands: Sender<Command>,
}

impl Actions {
    pub fn previous(&self) {
        let _ = self.commands.send(Command::Previous);
    }

    pub fn play_pause(&self) {
        let _ = self.commands.send(Command::PlayPause);
    }

    pub fn next(&self) {
        let _ = self.commands.send(Command::Next);
    }
}

/// Start watching. The thread lives until the [`Handle`] and every
/// [`Actions`] cloned from it are dropped.
pub fn spawn() -> Handle {
    let (updates_tx, updates) = mpsc::channel();
    let (commands, commands_rx) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("eclipse-media".into())
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
        a.clone().play_pause();
        a.previous();
        a.next();
        assert_eq!(h.try_recv(), None);
    }
}
