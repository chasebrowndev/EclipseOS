// SPDX-License-Identifier: AGPL-3.0-only
//! The widgets' services (ADR 0065): media, audio, usage and the custom
//! widget runner, plus the calendar drawer's provider client (ADR 0079), one of each per process.
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

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread;
use std::time::Duration;

use ec_services::custom::Kind;
use ec_services::power_profiles::{self, PowerActions, PowerEvents, Profile};
use ec_services::progress::{self, Update as ProgressUpdate};
use ec_services::session::{Action as SessionAction, Availability, Session};
use ec_services::updates::{self, After};
use ec_services::{audio, calendar, custom, media, usage};

use crate::app::Message;
use crate::widgets::{self, now_playing, system_usage, volume, Action, Feed, WidgetId};

/// What logind answered, or refused, for the power drawer.
#[derive(Debug, Clone)]
pub enum SessionFeed {
    /// Every session action's answer, from one probe.
    Answer(Vec<(SessionAction, Availability)>),
    /// logind or polkit refused an action the human asked for.
    Refused(String),
}

#[derive(Default)]
struct Handles {
    media: Option<media::Handle>,
    audio: Option<audio::Handle>,
    usage: Option<usage::Handle>,
    calendar: Option<calendar::Handle>,
    custom: Option<custom::Handle>,
    /// The power-profiles daemon's actions and reading (ADR 0065 amendment).
    power: Option<(PowerActions, PowerEvents)>,
    /// The progress feed and its `/proc` scan (ADR 0065 amendment). Started
    /// once, scanning for the life of the bar.
    progress: Option<progress::Handle>,
    /// The pending Eclipse update and the apply it runs (ADR 0085). Started on
    /// the first configure, never in a preview.
    updates: Option<updates::Handle>,
    /// logind's answers and refusals for the power drawer, on one channel.
    session: Option<(Sender<SessionFeed>, Receiver<SessionFeed>)>,
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

/// A call test builds count, since they start nothing.
#[derive(Clone, Copy)]
enum Call {
    Configure,
    Act,
}

#[cfg(test)]
thread_local! {
    /// `configure` and `act` calls on this thread, for the tests that prove
    /// one process with many bars still does each once.
    static CALLS: std::cell::Cell<(u32, u32)> = const { std::cell::Cell::new((0, 0)) };
}

/// `configure` and `act` calls on this test thread so far.
#[cfg(test)]
pub(crate) fn calls() -> (u32, u32) {
    CALLS.with(std::cell::Cell::get)
}

fn counted(call: Call) {
    #[cfg(test)]
    CALLS.with(|c| {
        let (configure, act) = c.get();
        c.set(match call {
            Call::Configure => (configure + 1, act),
            Call::Act => (configure, act + 1),
        });
    });
    #[cfg(not(test))]
    let _ = call;
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
        counted(Call::Configure);
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
    if h.calendar.is_none() {
        h.calendar = Some(calendar::spawn());
    }
    // The progress feed is served from here, so `ec-ctl progress` reaches
    // this bar and the cell shows what the scan finds. It scans for as long as
    // the bar runs; the scanner itself slow-polls while nothing is running.
    if h.progress.is_none() {
        h.progress = Some(progress::spawn());
    }
    // No system bus means no power reading, and the battery drawer's selector
    // stays hidden; a later configure tries again.
    if h.power.is_none() {
        h.power = power_profiles::spawn().ok();
    }
    // The update watcher runs `checkupdates` (a read-only sync of a temporary
    // database) on start and every 30 minutes. A preview never starts it.
    if h.updates.is_none() && !previewing() {
        h.updates = Some(updates::spawn());
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
                audio::Update::Outputs { devices, default } => Message::Outputs(devices, default),
                audio::Update::Inputs { devices, default } => Message::Inputs(devices, default),
            });
        }
    }
    if let Some(c) = h.calendar.as_ref() {
        while let Some(u) = c.try_recv() {
            out.push(Message::CalendarFeed(match u {
                calendar::Update::Provider(on) => crate::calendar::Feed::Provider(on),
                calendar::Update::Events { start, end, events } => {
                    crate::calendar::Feed::Events { start, end, events }
                }
            }));
        }
    }
    if let Some((_, events)) = h.power.as_ref() {
        while let Some(event) = events.try_recv() {
            out.push(Message::Power(event));
        }
    }
    if let Some(p) = h.progress.as_ref() {
        while let Some(ProgressUpdate::Progress(jobs)) = p.try_recv() {
            out.push(Message::Progress(jobs));
        }
    }
    if let Some(u) = h.updates.as_ref() {
        while let Some(update) = u.try_recv() {
            out.push(Message::Updates(update));
        }
    }
    if let Some((_, rx)) = h.session.as_ref() {
        while let Ok(feed) = rx.try_recv() {
            out.push(match feed {
                SessionFeed::Answer(answers) => Message::PowerAnswer(answers),
                SessionFeed::Refused(reason) => Message::PowerRefused(reason),
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
    counted(Call::Act);
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

/// The audio service's actions, if it is running.
pub fn audio_actions() -> Option<audio::Actions> {
    counted(Call::Act);
    handles().audio.as_ref().map(audio::Handle::actions)
}

/// The calendar service's actions, if it is running.
pub fn calendar_actions() -> Option<calendar::Actions> {
    handles().calendar.as_ref().map(calendar::Handle::actions)
}

/// A debug preview's fixture stands in for the services. Nothing that reaches
/// logind, pkexec or pacman starts or acts while one is on screen.
fn previewing() -> bool {
    cfg!(debug_assertions) && std::env::var_os("HYPERION_PREVIEW").is_some()
}

/// The power drawer's session channel's sender, created on first use.
fn session_sender() -> Sender<SessionFeed> {
    handles().session.get_or_insert_with(mpsc::channel).0.clone()
}

/// Ask logind what the power drawer may offer, on a thread: a `Can*` query can
/// wait on polkit. The answers come back through [`drain`]. With no system bus
/// every action is refused, which is the fail-closed reading.
pub fn probe_session() {
    if previewing() {
        return;
    }
    let tx = session_sender();
    let spawned = thread::Builder::new()
        .name("eclipse-session-probe".into())
        .spawn(move || {
            let actions = [
                SessionAction::Lock,
                SessionAction::LogOut,
                SessionAction::Suspend,
                SessionAction::Reboot,
                SessionAction::PowerOff,
            ];
            let answers = match Session::connect() {
                Ok(session) => actions.map(|a| (a, session.availability(a))).to_vec(),
                Err(_) => actions.map(|a| (a, Availability::No)).to_vec(),
            };
            let _ = tx.send(SessionFeed::Answer(answers));
        });
    if let Err(e) = spawned {
        eprintln!("eclipse-session-probe: not started: {e}");
    }
}

/// Ask logind to do a session action the human confirmed. Runs on a thread, since
/// a polkit prompt can hold the call open; a refusal comes back through
/// [`drain`] so the drawer can say so.
pub fn session_perform(action: SessionAction) {
    if previewing() {
        return;
    }
    let tx = session_sender();
    let spawned = thread::Builder::new()
        .name("eclipse-session".into())
        .spawn(move || {
            let outcome = Session::connect()
                .map_err(|e| e.to_string())
                .and_then(|session| session.perform(action, true).map_err(|e| e.to_string()));
            if let Err(reason) = outcome {
                let _ = tx.send(SessionFeed::Refused(reason));
            }
        });
    if let Err(e) = spawned {
        eprintln!("eclipse-session: not started: {e}");
    }
}

/// Start the full Eclipse update the Confirm step asked for. The answer, whether
/// a refusal or the upgrade's own outcome, comes back through [`drain`]. Only the
/// backend runs pacman, and it powers the machine off only on success (ADR 0085).
pub fn apply_update(then: After) {
    if previewing() {
        return;
    }
    if let Some(u) = handles().updates.as_ref() {
        u.apply(then);
    }
}

/// Ask the power-profiles daemon for a profile. Returns at once; the answer
/// (or the refusal) comes back through [`drain`].
pub fn set_profile(profile: Profile) {
    if let Some((actions, _)) = handles().power.as_ref() {
        actions.set_profile(profile);
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
