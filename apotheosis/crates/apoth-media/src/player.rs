// SPDX-License-Identifier: AGPL-3.0-only
//! One media placement's playback, terminal side (T-16 §3, §4). A player thread owns the
//! helper's command channel, the audio sink and the clock; a reader thread owns the
//! helper's output and validates every message before anything else sees it (T-16 §4.5):
//! frame geometry must match what `Info` negotiated, a slot must belong to the helper, PCM
//! must stay within the granted credit and 1.1x the stream's byte rate. A violation kills
//! the helper and the placement ends with `helper-protocol`.
//!
//! Frames leave as `Arc<Image>` through the event callback; the pane turns them into
//! draws. The player never touches a terminal lock.

use std::collections::VecDeque;
use std::os::fd::{AsFd, OwnedFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use apoth_image::Image;

use crate::chan::Chan;
use crate::client::{Helper, Mode, Proc, StartError};
use crate::sink::{AudioSink, FakeSink, PulseSink};
use crate::sync::{Clock, pick, wait_us};
use crate::wire::{self, Fail, FromHelper, ToHelper};

/// Ring slots per placement.
pub const SLOTS: u8 = 4;
/// Audio kept queued ahead of the clock.
const AUDIO_AHEAD_MS: u64 = 150;
/// The PCM rate a helper may sustain, as a multiple of the negotiated byte rate (x10).
const PCM_RATE_X10: u64 = 11;
/// PCM a helper may send at once beyond the sustained rate (start, after a seek).
const PCM_BURST_MS: u64 = 1_000;
/// No frame, PCM or end for this long while playing: the helper is stuck (CPU watchdog).
const STALL: Duration = Duration::from_secs(10);
/// How long the helper may take to report the stream.
const INFO_MS: i32 = 10_000;
/// Largest message the terminal accepts from a media helper: a PCM chunk, framing aside.
const MAX_IN: usize = wire::MAX_PCM + 64;

/// Where audio goes.
#[derive(Debug, Clone)]
pub enum SinkKind {
    /// The PulseAudio-protocol server from the environment.
    Pulse,
    /// No audio output (policy, or the placement asked for none).
    Off,
    /// Tests.
    Fake(FakeSink),
}

#[derive(Debug, Clone)]
pub struct Opts {
    /// The box the video is fitted into (pixels).
    pub box_w: u32,
    pub box_h: u32,
    /// Largest source the helper may decode.
    pub max_w: u32,
    pub max_h: u32,
    pub sink: SinkKind,
    pub volume: f32,
    pub muted: bool,
    pub looping: bool,
    /// Start playing at once (otherwise paused on the first frame).
    pub play: bool,
    /// The helper binary (tests); `None` finds it next to the executable.
    pub helper: Option<PathBuf>,
    /// Sandbox test knobs passed to the helper (tighten only).
    pub test_args: Vec<String>,
}

impl Default for Opts {
    fn default() -> Opts {
        Opts {
            box_w: 640,
            box_h: 360,
            max_w: 3840,
            max_h: 2160,
            sink: SinkKind::Off,
            volume: 1.0,
            muted: false,
            looping: false,
            play: true,
            helper: None,
            test_args: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Control {
    Play,
    Pause,
    Seek(u64),
    Volume(f32),
    Mute(bool),
    Loop(bool),
    Stop,
}

/// What a player reports. After `Error` nothing more comes.
#[derive(Debug, Clone)]
pub enum Event {
    /// The stream is open: output size (0x0 for audio only), duration, whether audio plays.
    Ready {
        width: u32,
        height: u32,
        duration_ms: u64,
        audio: bool,
    },
    /// Audio was asked for but no sound server is reachable: playback continues muted.
    AudioUnavailable,
    /// The frame to show now.
    Frame(Arc<Image>),
    /// Whole seconds played (once per change).
    Position(u64),
    /// Played to the end (not looping). A `Seek` restarts.
    Ended,
    /// Playback stopped for good: the T-16 error code (`helper-protocol`, `decode`, ...).
    Error(&'static str),
}

pub type OnEvent = Box<dyn FnMut(Event) + Send>;

/// A playing placement. Dropping it stops playback and kills the helper.
pub struct Player {
    tx: Sender<In>,
    thread: Option<JoinHandle<()>>,
    /// Shared with the player thread, for the CPU watchdog and tests.
    pid: Arc<AtomicU32>,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Player").finish_non_exhaustive()
    }
}

impl Player {
    /// Start playing `input` (an open file, never a path: the helper cannot open one).
    pub fn spawn(input: OwnedFd, opts: Opts, on_event: OnEvent) -> Player {
        let (tx, rx) = mpsc::channel();
        let pid = Arc::new(AtomicU32::new(0));
        let tx2 = tx.clone();
        let pid2 = Arc::clone(&pid);
        let thread = std::thread::Builder::new()
            .name("apoth-media".into())
            .spawn(move || {
                let mut on_event = on_event;
                if let Err(code) = run(input, opts, rx, tx2, &pid2, &mut on_event) {
                    on_event(Event::Error(code));
                }
            })
            .ok();
        Player { tx, thread, pid }
    }

    pub fn control(&self, c: Control) {
        let _ = self.tx.send(In::Ctl(c));
    }

    /// The helper's pid once it runs (0 before).
    pub fn helper_pid(&self) -> u32 {
        self.pid.load(Ordering::Relaxed)
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.tx.send(In::Ctl(Control::Stop));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Into the player thread: controls, and the reader's validated helper output.
enum In {
    Ctl(Control),
    Frame {
        slot: u8,
        image: Arc<Image>,
        pts: i64,
    },
    Pcm {
        pts: i64,
        data: Vec<u8>,
    },
    Seeked,
    End,
    Failed(Fail),
    /// The helper broke protocol (and was killed).
    Violation,
    Gone,
}

/// What the reader holds the helper to.
#[derive(Debug, Clone, Copy)]
struct Terms {
    width: u32,
    height: u32,
    /// Bytes per PCM frame (0: no audio expected).
    pcm_frame: u32,
    byte_rate: u64,
}

/// State the two threads share.
#[derive(Default)]
struct Shared {
    /// Slots the player holds (bit n = slot n); the helper may only fill free ones.
    held: AtomicU32,
    /// PCM bytes granted and not yet received.
    credit: AtomicI64,
}

/// Validate one helper message (pure, so tests can forge them). `bucket` is the PCM
/// received since `since` (reset at `Seeked`).
fn check(msg: &FromHelper, t: &Terms, sh: &Shared, bucket: &mut (u64, Instant)) -> Result<(), ()> {
    match msg {
        FromHelper::Frame {
            slot,
            width,
            height,
            stride,
            len,
            ..
        } => {
            let bytes = u64::from(t.width) * u64::from(t.height) * 4;
            if t.width == 0
                || *slot >= SLOTS
                || (*width, *height) != (t.width, t.height)
                || u64::from(*stride) != u64::from(t.width) * 4
                || u64::from(*len) != bytes
            {
                return Err(());
            }
            let bit = 1u32 << slot;
            if sh.held.fetch_or(bit, Ordering::AcqRel) & bit != 0 {
                return Err(());
            }
            Ok(())
        }
        FromHelper::Pcm { data, .. } => {
            let n = data.len() as u64;
            if t.pcm_frame == 0 || n == 0 || !n.is_multiple_of(u64::from(t.pcm_frame)) {
                return Err(());
            }
            if sh.credit.fetch_sub(n as i64, Ordering::AcqRel) < n as i64 {
                return Err(());
            }
            bucket.0 += n;
            let ms = bucket.1.elapsed().as_millis() as u64 + PCM_BURST_MS;
            let allowed = t.byte_rate * ms * PCM_RATE_X10 / 10 / 1000;
            if bucket.0 > allowed {
                return Err(());
            }
            Ok(())
        }
        FromHelper::Seeked => {
            *bucket = (0, Instant::now());
            Ok(())
        }
        FromHelper::End | FromHelper::Failed(_) => Ok(()),
        _ => Err(()),
    }
}

fn reader(
    mut rx: Chan,
    ring: Option<OwnedFd>,
    terms: Terms,
    sh: Arc<Shared>,
    proc: Arc<Mutex<Option<Proc>>>,
    out: Sender<In>,
) {
    let mut bucket = (0u64, Instant::now());
    let slot_bytes = terms.width as usize * terms.height as usize * 4;
    loop {
        let msg = match rx.recv() {
            Ok(Some((tag, body))) => FromHelper::decode(tag, &body).map_err(|_| ()),
            Ok(None) | Err(_) => {
                let _ = out.send(In::Gone);
                return;
            }
        };
        let ok = msg.and_then(|m| check(&m, &terms, &sh, &mut bucket).map(|()| m));
        let m = match ok {
            Ok(m) => m,
            Err(()) => {
                if let Ok(mut p) = proc.lock()
                    && let Some(p) = p.as_mut()
                {
                    p.kill();
                }
                let _ = out.send(In::Violation);
                return;
            }
        };
        let send = match m {
            FromHelper::Frame { slot, pts_us, .. } => {
                let Some(ring) = ring.as_ref() else {
                    let _ = out.send(In::Violation);
                    return;
                };
                let mut rgba = vec![0u8; slot_bytes];
                let off = u64::from(slot) * slot_bytes as u64;
                let mut done = 0;
                while done < rgba.len() {
                    match rustix::io::pread(ring, &mut rgba[done..], off + done as u64) {
                        Ok(0) | Err(_) => {
                            let _ = out.send(In::Gone);
                            return;
                        }
                        Ok(n) => done += n,
                    }
                }
                In::Frame {
                    slot,
                    image: Arc::new(Image {
                        width: terms.width,
                        height: terms.height,
                        rgba,
                    }),
                    pts: pts_us,
                }
            }
            FromHelper::Pcm { pts_us, data } => In::Pcm { pts: pts_us, data },
            FromHelper::Seeked => In::Seeked,
            FromHelper::End => In::End,
            FromHelper::Failed(f) => In::Failed(f),
            _ => In::Violation,
        };
        if out.send(send).is_err() {
            return;
        }
    }
}

fn start_error(e: StartError) -> &'static str {
    match e {
        StartError::Missing => "helper-missing",
        StartError::Failed(f) => f.code(),
    }
}

struct Queued {
    slot: u8,
    image: Arc<Image>,
    pts: i64,
}

fn run(
    input: OwnedFd,
    opts: Opts,
    rx: Receiver<In>,
    loopback: Sender<In>,
    pid: &AtomicU32,
    on_event: &mut OnEvent,
) -> Result<(), &'static str> {
    let args: Vec<&str> = opts.test_args.iter().map(String::as_str).collect();
    let h = match &opts.helper {
        Some(p) => Helper::start_at(p, Mode::Media, &args, MAX_IN),
        None => Helper::start(Mode::Media, &args, MAX_IN),
    }
    .map_err(start_error)?;
    let Helper {
        proc,
        rx: mut hrx,
        tx: htx,
    } = h;
    pid.store(proc.pid(), Ordering::Relaxed);
    let proc = Arc::new(Mutex::new(Some(proc)));
    let want_audio = !matches!(opts.sink, SinkKind::Off);
    let open = ToHelper::Open {
        audio: want_audio,
        box_w: opts.box_w,
        box_h: opts.box_h,
        max_w: opts.max_w,
        max_h: opts.max_h,
    };
    htx.send(&open.encode(), Some(input.as_fd()))
        .map_err(|_| "helper-protocol")?;
    drop(input);
    if !hrx.readable(INFO_MS) {
        return Err("helper-protocol");
    }
    let info = match hrx.recv() {
        Ok(Some((tag, body))) => FromHelper::decode(tag, &body).map_err(|_| "helper-protocol")?,
        _ => return Err("helper-protocol"),
    };
    let (width, height, rate, channels, duration_ms) = match info {
        FromHelper::Info {
            width,
            height,
            rate,
            channels,
            duration_ms,
        } => (width, height, rate, channels, duration_ms),
        FromHelper::Failed(f) => return Err(f.code()),
        _ => return Err("helper-protocol"),
    };
    // The helper may only shrink into the box, and send audio only when asked.
    let bad_video = width > opts.box_w || height > opts.box_h || (width == 0) != (height == 0);
    let bad_audio = if rate == 0 {
        channels != 0
    } else {
        !want_audio || !(1..=192_000).contains(&rate) || !(1..=8).contains(&channels)
    };
    if bad_video || bad_audio || (width == 0 && rate == 0) {
        return Err("helper-protocol");
    }
    let byte_rate = u64::from(rate) * u64::from(channels) * 2;
    let mut ring = None;
    if width > 0 {
        let slot_bytes = width as usize * height as usize * 4;
        let fd = rustix::fs::memfd_create("apoth-media-ring", rustix::fs::MemfdFlags::CLOEXEC)
            .map_err(|_| "resources")?;
        rustix::fs::ftruncate(&fd, (slot_bytes * usize::from(SLOTS)) as u64)
            .map_err(|_| "resources")?;
        let msg = ToHelper::Ring {
            slots: SLOTS,
            slot_bytes: slot_bytes as u32,
        };
        htx.send(&msg.encode(), Some(fd.as_fd()))
            .map_err(|_| "helper-protocol")?;
        ring = Some(fd);
    }
    // The sink.
    let mut sink: Option<Box<dyn AudioSink>> = None;
    if rate > 0 {
        sink = match &opts.sink {
            SinkKind::Pulse => PulseSink::socket()
                .and_then(|p| PulseSink::open(&p, rate, channels, opts.volume, opts.muted).ok())
                .map(|s| Box::new(s) as Box<dyn AudioSink>),
            SinkKind::Fake(f) => Some(Box::new(f.clone())),
            SinkKind::Off => None,
        };
    }
    let sh = Arc::new(Shared::default());
    let terms = Terms {
        width,
        height,
        pcm_frame: u32::from(channels) * 2,
        byte_rate,
    };
    {
        let sh = Arc::clone(&sh);
        let proc = Arc::clone(&proc);
        let out = loopback.clone();
        std::thread::Builder::new()
            .name("apoth-media-rd".into())
            .spawn(move || reader(hrx, ring, terms, sh, proc, out))
            .map_err(|_| "resources")?;
    }
    on_event(Event::Ready {
        width,
        height,
        duration_ms,
        audio: sink.is_some(),
    });
    if rate > 0 && sink.is_none() {
        on_event(Event::AudioUnavailable);
    }
    let mut p = Play {
        htx,
        proc,
        sh,
        sink,
        clock: Clock::new(false),
        queue: VecDeque::new(),
        playing: opts.play,
        looping: opts.looping,
        seeking: false,
        helper_ended: false,
        ended_sent: false,
        started: false,
        granted_target: byte_rate * AUDIO_AHEAD_MS / 1000,
        last_progress: Instant::now(),
        last_second: None,
        volume: (opts.volume, opts.muted),
        rate: rate > 0,
        has_video: width > 0,
        byte_rate,
        pcm_frame: u32::from(channels) * 2,
        pcm_rx: 0,
        pcm_base_us: 0,
    };
    p.clock = Clock::new(p.sink.is_some());
    if let Some(s) = p.sink.as_mut() {
        s.cork(!p.playing);
    }
    if p.clock.audio_master() || width == 0 {
        // Audio clock, or audio only without a sink: no frame anchors the clock.
        p.clock.restart(0, p.playing);
        p.started = true;
    }
    p.grant();
    let r = p.run(&rx, on_event);
    let _ = p.htx.send(&ToHelper::Quit.encode(), None);
    if let Ok(mut g) = p.proc.lock()
        && let Some(pr) = g.take()
    {
        let _ = pr.finish(500);
    }
    r
}

struct Play {
    htx: Chan,
    proc: Arc<Mutex<Option<Proc>>>,
    sh: Arc<Shared>,
    sink: Option<Box<dyn AudioSink>>,
    clock: Clock,
    queue: VecDeque<Queued>,
    playing: bool,
    looping: bool,
    /// A seek is in flight: helper output until `Seeked` predates it.
    seeking: bool,
    helper_ended: bool,
    ended_sent: bool,
    /// The frame clock has been anchored at the first frame.
    started: bool,
    granted_target: u64,
    last_progress: Instant,
    last_second: Option<u64>,
    volume: (f32, bool),
    /// The stream has audio (whether or not it plays).
    rate: bool,
    has_video: bool,
    byte_rate: u64,
    pcm_frame: u32,
    /// PCM bytes received since the last seek, and the media time it started at (for
    /// crediting without a sink).
    pcm_rx: u64,
    pcm_base_us: i64,
}

impl Play {
    fn send(&self, m: &ToHelper) -> Result<(), &'static str> {
        self.htx
            .send(&m.encode(), None)
            .map_err(|_| "helper-protocol")
    }

    /// Keep the helper's PCM credit topped up to the target ahead of the clock. Without
    /// a sink PCM is discarded on arrival, so credit follows the frame clock instead.
    fn grant(&mut self) {
        if !self.rate || self.granted_target == 0 {
            return;
        }
        let queued = match self.sink.as_ref() {
            Some(s) => s.queued() as u64,
            // No sink: PCM is dropped, as if played at the frame clock.
            None => {
                let ran = (self.now() - self.pcm_base_us).max(0) as u64;
                self.pcm_rx
                    .saturating_sub(ran.saturating_mul(self.byte_rate) / 1_000_000)
            }
        };
        let outstanding = self.sh.credit.load(Ordering::Acquire).max(0) as u64;
        let have = queued + outstanding;
        if have < self.granted_target {
            let frame = u64::from(self.pcm_frame).max(1);
            let more = (self.granted_target - have).min(wire::MAX_PCM as u64);
            let more = (more - more % frame) as u32;
            if more == 0 {
                return;
            }
            self.sh.credit.fetch_add(i64::from(more), Ordering::AcqRel);
            let _ = self.send(&ToHelper::Want { bytes: more });
        }
    }

    fn release(&self, slot: u8) {
        self.sh.held.fetch_and(!(1u32 << slot), Ordering::AcqRel);
        let _ = self.send(&ToHelper::Release { slot });
    }

    fn seek(&mut self, ms: u64) -> Result<(), &'static str> {
        self.send(&ToHelper::Seek { ms })?;
        self.seeking = true;
        self.helper_ended = false;
        self.ended_sent = false;
        while let Some(q) = self.queue.pop_front() {
            self.release(q.slot);
        }
        if let Some(s) = self.sink.as_mut() {
            s.flush();
        }
        let at = (ms as i64).saturating_mul(1000);
        self.clock.restart(at, self.playing);
        self.started = self.clock.audio_master() || !self.has_video();
        self.pcm_rx = 0;
        self.pcm_base_us = at;
        Ok(())
    }

    fn has_video(&self) -> bool {
        self.has_video
    }

    fn now(&self) -> i64 {
        self.clock.now_us(self.sink.as_deref())
    }

    fn run(&mut self, rx: &Receiver<In>, on_event: &mut OnEvent) -> Result<(), &'static str> {
        loop {
            let wait = if self.playing {
                let next = self.queue.front().map(|q| q.pts);
                wait_us(next, self.now()).map_or(40_000, |w| w.clamp(1_000, 40_000))
            } else {
                200_000
            };
            let msg = match rx.recv_timeout(Duration::from_micros(wait as u64)) {
                Ok(m) => Some(m),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            };
            if let Some(m) = msg {
                match m {
                    In::Ctl(Control::Stop) => return Ok(()),
                    In::Ctl(c) => self.control(c)?,
                    In::Frame { slot, image, pts } => {
                        self.last_progress = Instant::now();
                        if self.seeking {
                            self.release(slot);
                        } else {
                            if !self.started {
                                // Frame clock: time starts at the first frame.
                                self.clock.restart(pts, self.playing);
                                self.started = true;
                            }
                            self.queue.push_back(Queued { slot, image, pts });
                        }
                    }
                    In::Pcm { pts, data } => {
                        self.last_progress = Instant::now();
                        if !self.seeking {
                            if self.pcm_rx == 0 && self.sink.is_none() {
                                self.pcm_base_us = self.now();
                            }
                            self.pcm_rx += data.len() as u64;
                            if let Some(s) = self.sink.as_mut() {
                                self.clock.audio_started_at(pts);
                                s.write(&data);
                            }
                        }
                    }
                    In::Seeked => self.seeking = false,
                    In::End => {
                        self.last_progress = Instant::now();
                        if !self.seeking {
                            self.helper_ended = true;
                        }
                    }
                    In::Failed(f) => return Err(f.code()),
                    In::Violation => return Err("helper-protocol"),
                    In::Gone => return Err("helper-crashed"),
                }
            }
            self.tick(on_event)?;
        }
    }

    fn control(&mut self, c: Control) -> Result<(), &'static str> {
        match c {
            Control::Play if !self.playing => {
                self.playing = true;
                self.clock.play();
                if let Some(s) = self.sink.as_mut() {
                    s.cork(false);
                }
                self.last_progress = Instant::now();
                if self.ended_sent {
                    self.seek(0)?;
                }
            }
            Control::Pause if self.playing => {
                self.clock.pause(self.sink.as_deref());
                self.playing = false;
                if let Some(s) = self.sink.as_mut() {
                    s.cork(true);
                }
            }
            Control::Seek(ms) => self.seek(ms)?,
            Control::Volume(v) => {
                self.volume.0 = v.clamp(0.0, 1.0);
                let (v, m) = self.volume;
                if let Some(s) = self.sink.as_mut() {
                    s.set_volume(v, m);
                }
            }
            Control::Mute(m) => {
                self.volume.1 = m;
                let (v, m) = self.volume;
                if let Some(s) = self.sink.as_mut() {
                    s.set_volume(v, m);
                }
            }
            Control::Loop(l) => self.looping = l,
            _ => {}
        }
        Ok(())
    }

    fn tick(&mut self, on_event: &mut OnEvent) -> Result<(), &'static str> {
        if !self.playing {
            // Paused before anything showed: show the first frame.
            if let Some(q) = self.queue.front()
                && !self.ended_sent
                && self.last_second.is_none()
            {
                on_event(Event::Frame(Arc::clone(&q.image)));
                self.last_second = Some(0);
            }
            return Ok(());
        }
        let now = self.now();
        let pts: Vec<i64> = self.queue.iter().map(|q| q.pts).collect();
        let (show, mut used) = pick(&pts, now);
        let mut shown = show.map(|i| Arc::clone(&self.queue[i].image));
        // Audio master underrun with every slot full: video must not wedge the helper.
        let underrun = self.sink.as_ref().is_some_and(|s| s.queued() == 0);
        if shown.is_none() && underrun && self.queue.len() == usize::from(SLOTS) {
            shown = Some(Arc::clone(&self.queue[0].image));
            used = 1;
            self.clock.restart(self.queue[0].pts, true);
        }
        for _ in 0..used {
            if let Some(q) = self.queue.pop_front() {
                self.release(q.slot);
            }
        }
        if let Some(img) = shown {
            on_event(Event::Frame(img));
        }
        let sec = (now.max(0) / 1_000_000) as u64;
        if self.last_second != Some(sec) {
            self.last_second = Some(sec);
            on_event(Event::Position(sec));
        }
        self.grant();
        let drained = self.sink.as_ref().is_none_or(|s| s.queued() == 0);
        if self.helper_ended && self.queue.is_empty() && drained && !self.ended_sent {
            if self.looping {
                self.seek(0)?;
            } else {
                self.ended_sent = true;
                self.playing = false;
                on_event(Event::Ended);
            }
        }
        if !self.helper_ended
            && !self.seeking
            && self.last_progress.elapsed() > STALL
            && self.queue.len() < usize::from(SLOTS)
        {
            if let Ok(mut g) = self.proc.lock()
                && let Some(p) = g.as_mut()
            {
                p.kill();
            }
            return Err("helper-stalled");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms() -> Terms {
        Terms {
            width: 4,
            height: 2,
            pcm_frame: 4,
            byte_rate: 48_000 * 4,
        }
    }

    fn frame(slot: u8, w: u32, h: u32) -> FromHelper {
        FromHelper::Frame {
            slot,
            width: w,
            height: h,
            stride: w * 4,
            len: w * h * 4,
            pts_us: 0,
        }
    }

    #[test]
    fn forged_frame_metadata_is_a_violation() {
        let sh = Shared::default();
        let mut b = (0, Instant::now());
        let t = terms();
        assert!(check(&frame(0, 4, 2), &t, &sh, &mut b).is_ok());
        // The same slot again before the player released it.
        assert!(check(&frame(0, 4, 2), &t, &sh, &mut b).is_err());
        // Bigger than negotiated, a slot past the ring, a lying stride or length.
        assert!(check(&frame(1, 5, 2), &t, &sh, &mut b).is_err());
        assert!(check(&frame(SLOTS, 4, 2), &t, &sh, &mut b).is_err());
        let mut f = frame(2, 4, 2);
        if let FromHelper::Frame { stride, .. } = &mut f {
            *stride = 64;
        }
        assert!(check(&f, &t, &sh, &mut b).is_err());
        let mut f = frame(2, 4, 2);
        if let FromHelper::Frame { len, .. } = &mut f {
            *len = 1 << 30;
        }
        assert!(check(&f, &t, &sh, &mut b).is_err());
        // Anything an image helper says, or a second Info.
        assert!(check(&FromHelper::Ready, &t, &sh, &mut b).is_err());
        assert!(check(&FromHelper::ImageErr(0), &t, &sh, &mut b).is_err());
    }

    #[test]
    fn pcm_beyond_credit_or_rate_is_a_violation() {
        let sh = Shared::default();
        let mut b = (0, Instant::now());
        let t = terms();
        let pcm = |n: usize| FromHelper::Pcm {
            pts_us: 0,
            data: vec![0; n],
        };
        // No credit granted.
        assert!(check(&pcm(4), &t, &sh, &mut b).is_err());
        sh.credit.store(1 << 30, Ordering::Release);
        // Not whole PCM frames.
        assert!(check(&pcm(6), &t, &sh, &mut b).is_err());
        // Within the burst (1 s at 1.1x): fine; far over: killed.
        assert!(check(&pcm(48_000 * 4), &t, &sh, &mut b).is_ok());
        assert!(check(&pcm(wire::MAX_PCM - wire::MAX_PCM % 4), &t, &sh, &mut b).is_err());
        // Audio when none was negotiated.
        let none = Terms {
            pcm_frame: 0,
            ..terms()
        };
        assert!(check(&pcm(4), &none, &sh, &mut b).is_err());
    }
}
