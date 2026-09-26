// SPDX-License-Identifier: AGPL-3.0-only
//! The widgets' services (ADR 0065): media, audio, usage and the custom
//! widget runner, one of each per process.
//!
//! Same shape as `radio`: the handles live in a process-wide cell, because
//! iced's `update` and the compositor thread that polls them are different
//! threads and the handles are `Send` but not `Sync`. The compositor loop
//! [`drain`]s everything waiting into messages; `update` asks for things
//! with [`act`], [`set_tap`] and [`configure`]. Nothing here blocks: every
//! handle call is a channel send.
//!
//! Nothing starts under `cfg(test)`, and nothing starts in a preview, whose
//! fixture stands in for all of it.

use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use eclipse_services::custom::Kind;
use eclipse_services::{audio, custom, media, usage};

use crate::app::Message;
use crate::widgets::{self, now_playing, system_usage, volume, Action, Feed, WidgetId};

#[derive(Default)]
struct Handles {
    media: Option<media::Handle>,
    audio: Option<audio::Handle>,
    usage: Option<usage::Handle>,
    custom: Option<custom::Handle>,
    /// What the monitor tap was last asked to be.
    tap: bool,
}

fn handles() -> MutexGuard<'static, Handles> {
    static HANDLES: OnceLock<Mutex<Handles>> = OnceLock::new();
    HANDLES
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

fn usage_cfg(cfg: &widgets::Config) -> usage::UsageConfig {
    usage::UsageConfig {
        interval: Duration::from_millis(cfg.usage.interval_ms),
        disk_path: cfg.usage.disk_path.clone(),
    }
}

/// Whether anything on the bar reads the usage sampler: the widget itself,
/// or a custom block whose source is one of its readings.
fn wants_usage(cfg: &widgets::Config) -> bool {
    cfg.order.contains(&WidgetId::SystemUsage)
        || cfg
            .custom
            .iter()
            .any(|s| matches!(&s.kind, Kind::Source { source, .. } if source.starts_with("usage.")))
}

/// Start what `cfg` needs and bring the rest up to date. Called at start and
/// after every reload. Media and audio always run: the volume widget and
/// the per-window mute read audio, and both are cheap when idle.
pub fn configure(cfg: &widgets::Config) {
    if cfg!(test) {
        return;
    }
    let mut h = handles();
    let media = h
        .media
        .get_or_insert_with(|| media::spawn(cfg.now_playing.remote_art));
    // Every configure, not only the first: a reload that turns remote art
    // off must drop the covers already fetched, and the service makes a
    // repeat of the current setting a no-op.
    media.set_remote_art(cfg.now_playing.remote_art);
    if h.audio.is_none() {
        h.audio = Some(audio::spawn());
    }
    if wants_usage(cfg) {
        match h.usage.as_ref() {
            Some(u) => u.reconfigure(usage_cfg(cfg)),
            None => h.usage = Some(usage::spawn(usage_cfg(cfg))),
        }
    } else {
        h.usage = None;
    }
    let commands: Vec<_> = cfg
        .custom
        .iter()
        .filter(|s| !matches!(s.kind, Kind::Source { .. }))
        .cloned()
        .collect();
    if commands.is_empty() {
        // Dropping the handle kills every widget command.
        h.custom = None;
    } else {
        match h.custom.as_ref() {
            Some(c) => c.reconfigure(commands),
            None => h.custom = Some(custom::spawn(commands)),
        }
    }
}

/// Everything waiting, as messages. Never blocks.
pub fn drain() -> Vec<Message> {
    let h = handles();
    let mut out = Vec::new();
    if let Some(m) = h.media.as_ref() {
        while let Some(media::Update::Player(p)) = m.try_recv() {
            out.push(Message::Widget(Feed::NowPlaying(now_playing::Feed::Player(p))));
        }
    }
    if let Some(a) = h.audio.as_ref() {
        while let Some(u) = a.try_recv() {
            out.push(match u {
                audio::Update::Sink(s) => Message::Widget(Feed::Volume(volume::Feed::Sink(s))),
                audio::Update::Streams(s) => Message::Streams(s),
                audio::Update::Levels(b) => Message::Widget(Feed::NowPlaying(now_playing::Feed::Levels(b.0))),
            });
        }
    }
    if let Some(u) = h.usage.as_ref() {
        while let Some(usage::Update::Sample(s)) = u.try_recv() {
            out.push(Message::Widget(Feed::Usage(system_usage::Feed::Sample(s))));
        }
    }
    if let Some(c) = h.custom.as_ref() {
        while let Some(u) = c.try_recv() {
            out.push(match u {
                custom::Update::Output { name, out } => {
                    Message::Widget(Feed::Custom(name, widgets::custom::Feed::Output(out)))
                }
                // The reason is ours, but still not worth a line per poll.
                custom::Update::Failed { name, .. } => {
                    Message::Widget(Feed::Custom(name, widgets::custom::Feed::Failed))
                }
            });
        }
    }
    out
}

/// Whether the monitor tap is on, so the poll loop knows to spin fast.
pub fn tapping() -> bool {
    handles().tap
}

/// Ask a service to do something on a widget's behalf.
pub fn act(action: Action) {
    let h = handles();
    match action {
        Action::Previous | Action::PlayPause | Action::Next => {
            if let Some(m) = h.media.as_ref() {
                let a = m.actions();
                match action {
                    Action::Previous => a.previous(),
                    Action::Next => a.next(),
                    _ => a.play_pause(),
                }
            }
        }
        Action::SetVolume(v) => {
            if let Some(a) = h.audio.as_ref() {
                a.actions().set_volume(v);
            }
        }
        Action::SetMuted(m) => {
            if let Some(a) = h.audio.as_ref() {
                a.actions().set_muted(m);
            }
        }
        Action::Run(argv) => {
            if let Some(c) = h.custom.as_ref() {
                c.run(argv);
            }
        }
    }
}

/// Mute or unmute every stream `pid` owns.
pub fn set_app_muted(pid: u32, muted: bool) {
    if let Some(a) = handles().audio.as_ref() {
        a.actions().set_app_muted(pid, muted);
    }
}

/// Open or close the monitor tap; a no-op when it already is.
pub fn set_tap(on: bool) {
    let mut h = handles();
    if h.tap == on {
        return;
    }
    h.tap = on;
    if let Some(a) = h.audio.as_ref() {
        a.set_tap(on);
    }
}
