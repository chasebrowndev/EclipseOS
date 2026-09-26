// SPDX-License-Identifier: AGPL-3.0-only
//! Audio: the default sink's volume and mute, per-app mute, and the
//! visualizer's monitor tap (ADR 0065).
//!
//! Replaces the taskbar's `pactl` shell-outs. Spoken over the PulseAudio
//! native protocol (the `pulseaudio` crate's synchronous `protocol` module),
//! which pipewire-pulse serves: no libpulse, no bindgen. Same shape as
//! [`crate::status`]: an `mpsc` feed the GUI drains with [`Handle::try_recv`]
//! and a separate [`Actions`] handle for changes.
//!
//! The service thread owns the state and a request connection; a second
//! connection carries the server's change events (`Subscribe`) to it, so
//! nothing polls. The default sink is re-read whenever a sink or the server
//! changes, which also follows a change of default sink. If the server goes
//! away the feed says [`Update::Sink`]`(None)` and the thread reconnects with
//! backoff.
//!
//! **The monitor tap** (ADR 0065, "The monitor tap") records the default
//! sink's monitor only while [`Handle::set_tap`] is on; while it is off no
//! stream exists and nothing is recorded. Turning it off closes the stream.
//! Samples live only in fixed, reused buffers (the frame being read and the
//! analyzer's ring), are reduced there to [`Bands`], and are overwritten by
//! the next packet: never stored, logged, or sent anywhere.

mod pids;
mod spectrum;
mod tap;
mod wire;

use std::ffi::CString;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use pulseaudio::protocol::{
    ChannelVolume, Command as Pa, GetSinkInfo, Prop, Props, ProtocolError, ServerInfo, SetDeviceMuteParams,
    SetDeviceVolumeParams, SetStreamMuteParams, SinkInfo, SinkInputInfoList, SubscriptionEventFacility,
    SubscriptionMask, Volume,
};

use tap::Tap;
use wire::Conn;

/// Band count of [`Bands`].
const BANDS: usize = 16;

/// The loudest [`Actions::set_volume`] will go: 150%.
const MAX_VOLUME: f32 = 1.5;

/// Reconnect delays after the server goes away.
const BACKOFF_MIN: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(5);

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
pub struct Bands(pub [f32; BANDS]);

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

    /// Mute or unmute every stream owned by `pid` or any of its descendants
    /// (a browser plays from a content child, not the window's process).
    pub fn set_app_muted(&self, pid: u32, m: bool) {
        let _ = self.commands.send(Command::SetAppMuted { pid, muted: m });
    }
}

/// Start the service. The thread lives until the [`Handle`] and every
/// [`Actions`] cloned from it are dropped.
pub fn spawn() -> Handle {
    spawn_at(None)
}

/// [`spawn`], talking to the server at `socket` instead of the one the
/// environment names. Tests point it at nothing.
fn spawn_at(socket: Option<PathBuf>) -> Handle {
    let (updates_tx, updates) = mpsc::channel();
    let (commands, commands_rx) = mpsc::channel();
    let (internal, internal_rx) = mpsc::channel();
    // The service waits on one channel for both the GUI's commands and the
    // event connection's news. This forwarder turns the GUI hanging up into
    // an explicit `Quit`, which the shared channel could never report.
    let forward = internal.clone();
    let _ = std::thread::Builder::new()
        .name("eclipse-audio-cmd".into())
        .spawn(move || {
            for command in commands_rx {
                if forward.send(Internal::Command(command)).is_err() {
                    return;
                }
            }
            let _ = forward.send(Internal::Quit);
        });
    let _ = std::thread::Builder::new()
        .name("eclipse-audio".into())
        .spawn(move || Service::new(socket, updates_tx, internal).run(&internal_rx));
    Handle { updates, commands }
}

/// What the service thread waits on.
enum Internal {
    Command(Command),
    /// A change on the server, from event connection `generation`.
    Event {
        generation: u64,
        facility: SubscriptionEventFacility,
    },
    /// Event connection `generation` has closed.
    Gone(u64),
    /// Tap `id`'s pump thread has ended, after running for `ran`.
    TapEnded {
        id: u64,
        ran: Duration,
    },
    /// The GUI hung up.
    Quit,
}

/// Why [`Service::serve`] returned.
enum Exit {
    Lost,
    Quit,
}

/// The default sink, as far as actions need it.
struct Current {
    index: u32,
    channels: usize,
    monitor: Option<CString>,
}

/// A connected server.
struct Live {
    path: PathBuf,
    conn: Conn,
    /// A clone of the event connection's socket, shut to end its reader.
    events: UnixStream,
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = self.events.shutdown(Shutdown::Both);
    }
}

struct Service {
    socket: Option<PathBuf>,
    updates: Sender<Update>,
    internal: Sender<Internal>,
    generation: u64,
    live: Option<Live>,
    sink: Option<Current>,
    sent_sink: Option<Option<Sink>>,
    sent_streams: Option<Vec<Stream>>,
    tap_wanted: bool,
    tap: Option<Tap>,
    /// Id for the next [`Tap`], so a stale exit signal is recognised.
    next_tap: u64,
    /// When to try the tap again after it failed to open or was dropped by
    /// the server.
    tap_retry: Backoff,
}

/// A doubling retry delay, `BACKOFF_MIN` up to `BACKOFF_MAX`.
#[derive(Debug, PartialEq, Eq)]
struct Backoff {
    next: Duration,
    due: Option<Instant>,
}

impl Backoff {
    const fn new() -> Self {
        Self {
            next: BACKOFF_MIN,
            due: None,
        }
    }

    /// Schedule a try after the current delay, and double the delay.
    fn schedule(&mut self, now: Instant) {
        self.due = Some(now + self.next);
        self.next = (self.next * 2).min(BACKOFF_MAX);
    }
}

/// A tap that ran this long before ending was healthy: its retry starts
/// again from `BACKOFF_MIN`.
const TAP_HEALTHY: Duration = Duration::from_secs(10);

impl Service {
    fn new(socket: Option<PathBuf>, updates: Sender<Update>, internal: Sender<Internal>) -> Self {
        Self {
            socket,
            updates,
            internal,
            generation: 0,
            live: None,
            sink: None,
            sent_sink: None,
            sent_streams: None,
            tap_wanted: false,
            tap: None,
            next_tap: 0,
            tap_retry: Backoff::new(),
        }
    }

    fn run(mut self, rx: &Receiver<Internal>) {
        let mut backoff = BACKOFF_MIN;
        loop {
            if self.open().is_ok() {
                backoff = BACKOFF_MIN;
                if let Exit::Quit = self.serve(rx) {
                    return;
                }
            }
            self.lost();
            // Wait out the backoff. Nothing can be done without a server, but
            // the tap's wanted state is remembered for when it returns.
            let deadline = Instant::now() + backoff;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                match rx.recv_timeout(left) {
                    Ok(Internal::Command(Command::Tap(on))) => self.tap_wanted = on,
                    Ok(Internal::Quit) | Err(RecvTimeoutError::Disconnected) => return,
                    Ok(_) => {}
                    Err(RecvTimeoutError::Timeout) => break,
                }
            }
            backoff = (backoff * 2).min(BACKOFF_MAX);
        }
    }

    /// Connect, subscribe, and read the initial picture.
    fn open(&mut self) -> wire::Result<()> {
        let path = self
            .socket
            .clone()
            .or_else(pulseaudio::socket_path_from_env)
            .ok_or_else(|| ProtocolError::Io(std::io::ErrorKind::NotFound.into()))?;
        let conn = wire::connect(&path, c"Eclipse")?;
        // Subscribe before the first read, so no change slips between them.
        let mut events = wire::connect(&path, c"Eclipse events")?;
        events.ack(&Pa::Subscribe(
            SubscriptionMask::SINK | SubscriptionMask::SINK_INPUT | SubscriptionMask::SERVER,
        ))?;
        events.stream().set_read_timeout(None)?;
        let shut = events.stream().try_clone()?;
        self.generation += 1;
        let generation = self.generation;
        let tx = self.internal.clone();
        std::thread::Builder::new()
            .name("eclipse-audio-events".into())
            .spawn(move || loop {
                match events.read_command() {
                    Ok(Pa::SubscribeEvent(e)) => {
                        let facility = e.event_facility;
                        if tx.send(Internal::Event { generation, facility }).is_err() {
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => {
                        let _ = tx.send(Internal::Gone(generation));
                        return;
                    }
                }
            })?;
        self.live = Some(Live {
            path,
            conn,
            events: shut,
        });
        self.refresh_sink()?;
        self.refresh_streams()
    }

    /// Handle messages until the server is lost or the GUI hangs up. Bursts
    /// of events are coalesced: one re-read per burst.
    fn serve(&mut self, rx: &Receiver<Internal>) -> Exit {
        loop {
            // Wait for news, or until a tap retry is due.
            let first = match self.tap_retry.due {
                Some(due) => match rx.recv_timeout(due.saturating_duration_since(Instant::now())) {
                    Ok(message) => Some(message),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return Exit::Quit,
                },
                None => match rx.recv() {
                    Ok(message) => Some(message),
                    Err(_) => return Exit::Quit,
                },
            };
            let (mut sink, mut streams) = (false, false);
            let mut next = first;
            while let Some(message) = next {
                match message {
                    Internal::Quit => return Exit::Quit,
                    Internal::Gone(g) if g == self.generation => return Exit::Lost,
                    Internal::Event { generation, facility } if generation == self.generation => {
                        match facility {
                            SubscriptionEventFacility::Sink | SubscriptionEventFacility::Server => {
                                sink = true;
                            }
                            SubscriptionEventFacility::SinkInput => streams = true,
                            _ => {}
                        }
                    }
                    Internal::Command(command) => {
                        if self.command(command).is_err() {
                            return Exit::Lost;
                        }
                    }
                    Internal::TapEnded { id, ran } => self.tap_ended(id, ran, Instant::now()),
                    Internal::Gone(_) | Internal::Event { .. } => {}
                }
                next = rx.try_recv().ok();
            }
            if sink && self.refresh_sink().is_err() {
                return Exit::Lost;
            }
            if streams && self.refresh_streams().is_err() {
                return Exit::Lost;
            }
            let now = Instant::now();
            if self.tap_retry.due.is_some_and(|due| due <= now) {
                self.retry_tap(now);
            }
        }
    }

    /// Run one GUI command. `Err` only when the connection is broken; a
    /// server refusing (the sink vanished mid-change) is not.
    fn command(&mut self, command: Command) -> wire::Result<()> {
        if let Command::Tap(on) = command {
            self.tap_wanted = on;
            self.sync_tap(Instant::now());
            return Ok(());
        }
        let Some(live) = self.live.as_mut() else {
            return Ok(());
        };
        let result = match command {
            Command::SetVolume(v) => {
                let Some(sink) = &self.sink else {
                    return Ok(());
                };
                let Some(volume) = channel_volume(v, sink.channels) else {
                    return Ok(());
                };
                live.conn.ack(&Pa::SetSinkVolume(SetDeviceVolumeParams {
                    device_index: Some(sink.index),
                    device_name: None,
                    volume,
                }))
            }
            Command::SetMuted(mute) => {
                let Some(sink) = &self.sink else {
                    return Ok(());
                };
                live.conn.ack(&Pa::SetSinkMute(SetDeviceMuteParams {
                    device_index: Some(sink.index),
                    device_name: None,
                    mute,
                }))
            }
            Command::SetAppMuted { pid, muted } => set_app_muted(&mut live.conn, pid, muted),
            Command::Tap(_) => Ok(()),
        };
        match result {
            Err(ProtocolError::ServerError(_)) => Ok(()),
            other => other,
        }
    }

    /// Re-read the default sink and publish it.
    fn refresh_sink(&mut self) -> wire::Result<()> {
        let Some(live) = self.live.as_mut() else {
            return Ok(());
        };
        let server: ServerInfo = live.conn.request(&Pa::GetServerInfo)?;
        let info = match server.default_sink_name {
            Some(name) => match live.conn.request::<SinkInfo>(&Pa::GetSinkInfo(GetSinkInfo {
                index: None,
                name: Some(name),
            })) {
                Ok(info) => Some(info),
                Err(ProtocolError::ServerError(_)) => None,
                Err(e) => return Err(e),
            },
            None => None,
        };
        self.sink = info.as_ref().map(|i| Current {
            index: i.index,
            channels: i.cvolume.channels().len(),
            monitor: i.monitor_source_name.clone(),
        });
        let sink = info.map(|i| Sink {
            volume: volume_of(&i.cvolume),
            muted: i.muted,
            description: i.description.unwrap_or(i.name).to_string_lossy().into_owned(),
        });
        self.emit_sink(sink);
        self.sync_tap(Instant::now());
        Ok(())
    }

    fn refresh_streams(&mut self) -> wire::Result<()> {
        let Some(live) = self.live.as_mut() else {
            return Ok(());
        };
        let list: SinkInputInfoList = live.conn.request(&Pa::GetSinkInputInfoList)?;
        let streams = list
            .iter()
            .filter_map(|s| {
                Some(Stream {
                    pid: pid_of(&s.props)?,
                    muted: s.muted,
                })
            })
            .collect();
        self.emit_streams(streams);
        Ok(())
    }

    /// Open, move or close the tap so it matches what is wanted: on, and on
    /// the current default sink's monitor. While a retry is pending the tap
    /// stays closed until it is due; not wanting the tap cancels the retry.
    fn sync_tap(&mut self, now: Instant) {
        let want = self
            .sink
            .as_ref()
            .and_then(|s| s.monitor.clone())
            .filter(|_| self.tap_wanted);
        let Some(source) = want else {
            self.tap = None; // closes the stream, if any
            self.tap_retry = Backoff::new();
            return;
        };
        if self.tap.as_ref().map(Tap::source) == Some(source.as_c_str()) {
            return;
        }
        if self.tap.is_none() && self.tap_retry.due.is_some_and(|due| now < due) {
            return;
        }
        self.tap = None;
        self.tap_retry.due = None;
        let id = self.next_tap;
        self.next_tap += 1;
        let started = self.live.as_ref().and_then(|live| {
            Tap::start(
                &live.path,
                &source,
                id,
                self.updates.clone(),
                self.internal.clone(),
            )
            .ok()
        });
        match started {
            Some(tap) => self.tap = Some(tap),
            None => self.tap_retry.schedule(now),
        }
    }

    /// A tap's pump thread ended. If it was the current tap and it is still
    /// wanted, the server dropped it: reopen it after the backoff.
    fn tap_ended(&mut self, id: u64, ran: Duration, now: Instant) {
        if self.tap.as_ref().map(Tap::id) != Some(id) {
            return; // a tap we closed ourselves
        }
        self.tap = None;
        if ran >= TAP_HEALTHY {
            self.tap_retry = Backoff::new();
        }
        if self.tap_wanted {
            self.tap_retry.schedule(now);
        }
    }

    /// The retry delay has passed: try the tap again.
    fn retry_tap(&mut self, now: Instant) {
        self.tap_retry.due = None;
        self.sync_tap(now);
    }

    /// The server is gone: drop everything that referred to it. The tap
    /// reopens with the connection, if still wanted.
    fn lost(&mut self) {
        self.tap = None;
        self.tap_retry = Backoff::new();
        self.live = None;
        self.sink = None;
        self.emit_sink(None);
        self.emit_streams(Vec::new());
    }

    fn emit_sink(&mut self, sink: Option<Sink>) {
        if self.sent_sink.as_ref() != Some(&sink) {
            let _ = self.updates.send(Update::Sink(sink.clone()));
            self.sent_sink = Some(sink);
        }
    }

    fn emit_streams(&mut self, streams: Vec<Stream>) {
        if self.sent_streams.as_ref() != Some(&streams) {
            let _ = self.updates.send(Update::Streams(streams.clone()));
            self.sent_streams = Some(streams);
        }
    }
}

/// Mute every sink input owned by `pid` or a descendant of it.
fn set_app_muted(conn: &mut Conn, pid: u32, mute: bool) -> wire::Result<()> {
    let list: SinkInputInfoList = conn.request(&Pa::GetSinkInputInfoList)?;
    for input in list {
        if pid_of(&input.props).is_some_and(|p| pids::descends_from(p, pid)) {
            conn.ack(&Pa::SetSinkInputMute(SetStreamMuteParams {
                index: input.index,
                mute,
            }))?;
        }
    }
    Ok(())
}

/// A stream's `application.process.id`.
fn pid_of(props: &Props) -> Option<u32> {
    let raw = props.get(Prop::ApplicationProcessId)?;
    let raw = raw.strip_suffix(&[0]).unwrap_or(raw);
    std::str::from_utf8(raw).ok()?.trim().parse().ok()
}

/// The channel average as pactl shows it: the ratio to `PA_VOLUME_NORM`, so
/// `1.0` is 100%. Not the cubic "linear" of `Volume::to_linear`.
fn volume_of(volume: &ChannelVolume) -> f32 {
    let channels = volume.channels();
    if channels.is_empty() {
        return 0.0;
    }
    let sum: u64 = channels.iter().map(|v| u64::from(v.as_u32())).sum();
    (sum as f64 / channels.len() as f64 / f64::from(Volume::NORM.as_u32())) as f32
}

/// [`volume_of`]'s inverse, clamped to `0.0..=MAX_VOLUME` and set on every
/// channel. `None` for a value that is no volume at all.
fn channel_volume(v: f32, channels: usize) -> Option<ChannelVolume> {
    if !v.is_finite() || channels == 0 {
        return None;
    }
    let raw = (v.clamp(0.0, MAX_VOLUME) * Volume::NORM.as_u32() as f32).round() as u32;
    let mut volume = ChannelVolume::empty();
    for _ in 0..channels {
        volume.push(Volume::from_u32_clamped(raw));
    }
    Some(volume)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cv(raw: &[u32]) -> ChannelVolume {
        let mut v = ChannelVolume::empty();
        for &r in raw {
            v.push(Volume::from_u32_clamped(r));
        }
        v
    }

    #[test]
    fn volume_is_the_norm_ratio_averaged_over_channels() {
        assert_eq!(volume_of(&cv(&[0x10000, 0x10000])), 1.0);
        assert_eq!(volume_of(&cv(&[0x8000, 0x8000])), 0.5);
        assert_eq!(volume_of(&cv(&[0x10000, 0])), 0.5);
        assert_eq!(volume_of(&cv(&[0x18000])), 1.5);
        assert_eq!(volume_of(&cv(&[])), 0.0);
    }

    #[test]
    fn setting_a_volume_round_trips_and_clamps() {
        let set =
            |v, n| channel_volume(v, n).map(|c| c.channels().iter().map(Volume::as_u32).collect::<Vec<_>>());
        assert_eq!(set(0.5, 2), Some(vec![0x8000, 0x8000]));
        assert_eq!(set(1.0, 1), Some(vec![0x10000]));
        assert_eq!(set(2.0, 2), Some(vec![0x18000, 0x18000]), "clamped to 150%");
        assert_eq!(set(-1.0, 1), Some(vec![0]));
        assert_eq!(set(f32::NAN, 2), None);
        assert_eq!(set(0.5, 0), None);
        // What the feed reports, set back, is what was there.
        let there = cv(&[40_000, 40_000]);
        assert_eq!(set(volume_of(&there), 2), Some(vec![40_000, 40_000]));
    }

    #[test]
    fn a_stream_pid_comes_from_its_props() {
        let mut props = Props::new();
        assert_eq!(pid_of(&props), None);
        props.set(Prop::ApplicationProcessId, c"4242");
        assert_eq!(pid_of(&props), Some(4242));
        props.set(Prop::ApplicationProcessId, c"not a pid");
        assert_eq!(pid_of(&props), None);
    }

    /// A service with a wanted tap on sink monitor `mon`, and no server.
    fn tapping() -> Service {
        let (updates, _) = mpsc::channel();
        let (internal, _) = mpsc::channel();
        let mut s = Service::new(None, updates, internal);
        s.sink = Some(Current {
            index: 0,
            channels: 2,
            monitor: Some(c"mon".to_owned()),
        });
        s.tap_wanted = true;
        s.next_tap = 8;
        s.tap = Some(Tap::fake(7, c"mon"));
        s
    }

    #[test]
    fn a_tap_the_server_drops_is_retried_with_backoff() {
        let mut s = tapping();
        let t0 = Instant::now();
        let ms = Duration::from_millis;

        // An exit signal from a tap we already replaced changes nothing.
        s.tap_ended(6, ms(0), t0);
        assert!(s.tap.is_some());
        assert_eq!(s.tap_retry.due, None);

        // The current tap ends: closed, with a retry 250 ms out.
        s.tap_ended(7, ms(100), t0);
        assert!(s.tap.is_none());
        assert_eq!(s.tap_retry.due, Some(t0 + ms(250)));

        // Nothing reopens it early, even a sink refresh.
        s.sync_tap(t0 + ms(100));
        assert_eq!(s.tap_retry.due, Some(t0 + ms(250)));

        // Each failed retry (no server here) doubles the delay, up to 5 s.
        let mut now = t0 + ms(250);
        for expect in [500, 1000, 2000, 4000, 5000, 5000] {
            s.retry_tap(now);
            assert!(s.tap.is_none());
            assert_eq!(s.tap_retry.due, Some(now + ms(expect)));
            now += ms(expect);
        }

        // Only turning the tap off gives up.
        s.command(Command::Tap(false)).unwrap();
        assert_eq!(s.tap_retry, Backoff::new());
        assert!(s.tap.is_none());
    }

    #[test]
    fn a_healthy_tap_that_ends_retries_from_the_start() {
        let mut s = tapping();
        s.tap_retry.next = BACKOFF_MAX;
        let t0 = Instant::now();
        s.tap_ended(7, TAP_HEALTHY, t0);
        assert_eq!(s.tap_retry.due, Some(t0 + BACKOFF_MIN));
    }

    #[test]
    fn a_tap_that_ends_when_not_wanted_is_not_retried() {
        let mut s = tapping();
        s.tap_wanted = false;
        s.tap_ended(7, Duration::ZERO, Instant::now());
        assert!(s.tap.is_none());
        assert_eq!(s.tap_retry.due, None);
    }

    /// Drain the feed for up to two seconds.
    fn collect(h: &Handle, want: usize) -> Vec<Update> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut got = Vec::new();
        while got.len() < want && Instant::now() < deadline {
            match h.try_recv() {
                Some(u) => got.push(u),
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        got
    }

    #[test]
    fn no_server_reads_as_no_sink() {
        // Never the human's server: a socket path that does not exist.
        let h = spawn_at(Some(PathBuf::from("/nonexistent/eclipse-test/pulse/native")));
        assert_eq!(
            collect(&h, 2),
            vec![Update::Sink(None), Update::Streams(Vec::new())]
        );
        // Actions without a server are dropped, not queued or fatal.
        let a = h.actions();
        a.clone().set_volume(0.5);
        a.set_muted(true);
        a.set_app_muted(1, true);
        h.set_tap(true);
        h.set_tap(false);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(h.try_recv(), None, "nothing new while nothing changed");
    }

    /// Read-only readout of the live server: `cargo test -p eclipse-services
    /// live_readout -- --ignored --nocapture`. Changes nothing; opens the tap
    /// for two seconds and prints band levels, never samples.
    #[test]
    #[ignore = "talks to the human's audio server"]
    fn live_readout() {
        let h = spawn();
        for u in collect(&h, 2) {
            println!("{u:?}");
        }
        h.set_tap(true);
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut frames = 0;
        while Instant::now() < deadline {
            match h.try_recv() {
                Some(Update::Levels(b)) => {
                    frames += 1;
                    if frames % 30 == 0 {
                        println!("levels {:?}", b.0.map(|l| (l * 9.0).round() as u8));
                    }
                }
                Some(u) => println!("{u:?}"),
                None => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        h.set_tap(false);
        println!("{frames} level frames in 2 s");
    }
}
