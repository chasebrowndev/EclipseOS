// SPDX-License-Identifier: AGPL-3.0-only
//! Audio output (T-16 §3, ADR 0118; MEDIA-09, -26..-29). [`AudioSink`] is the boundary
//! (MEDIA-28); [`PulseSink`] speaks the PulseAudio native protocol (the pure-Rust
//! `pulseaudio` crate, no linked audio library) to `pipewire-pulse`. No socket ⇒
//! [`PulseSink::open`] fails and the placement plays muted with `audio-unavailable`
//! (MEDIA-29). PCM is written by the terminal process, never by the helper (T-16 §4.5).
//!
//! The clock (MEDIA-10, -27): the stream's played position from `GetPlaybackLatency` —
//! the server's read offset minus the sink latency — extrapolated from the reply's time
//! while the stream plays; it stops on underrun, which holds video.

use std::collections::VecDeque;
use std::ffi::CString;
use std::io::{BufReader, Cursor, Read};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use pulseaudio::protocol::{
    self, ChannelMap, ChannelPosition, ChannelVolume, Command, CorkStreamParams, LatencyParams,
    PlaybackLatency, Prop, Props, SampleFormat, SampleSpec, TagStructReader, Volume,
};

/// The output boundary. Bytes are interleaved s16le in the format the sink was opened with.
pub trait AudioSink: Send {
    /// Queue PCM for playback.
    fn write(&mut self, pcm: &[u8]);
    /// Bytes accepted but not yet played (client queue plus the server's buffer, estimated).
    fn queued(&self) -> usize;
    /// Media time played since the last [`AudioSink::flush`] (µs); `None` before playback
    /// starts.
    fn played_us(&self) -> Option<u64>;
    /// Pause (`true`) or resume.
    fn cork(&mut self, paused: bool);
    /// Drop everything queued (seek); the played clock restarts at 0.
    fn flush(&mut self);
    /// Volume 0..=1 and mute.
    fn set_volume(&mut self, volume: f32, mute: bool);
}

/// Why no sink could be opened (the placement reports `audio-unavailable`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unavailable;

#[derive(Default)]
struct Shared {
    buf: VecDeque<u8>,
    /// Bytes the server asked for and has not been sent yet.
    owed: u64,
    /// Bytes sent since the last flush.
    sent: u64,
    /// Latest timing: (played bytes since flush, at, playing).
    timing: Option<(i64, Instant, bool)>,
    /// Write offset at the last flush (server bytes), to rebase read offsets.
    base: Option<i64>,
    /// Flushes so far: a timing reply asked for before the latest flush is stale.
    epoch: u32,
    cork: Option<bool>,
    flush: bool,
    volume: Option<ChannelVolume>,
    paused: bool,
}

/// A playback stream on a PulseAudio-protocol server.
pub struct PulseSink {
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    byte_rate: u64,
    channels: u8,
}

impl std::fmt::Debug for PulseSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PulseSink")
            .field("byte_rate", &self.byte_rate)
            .finish()
    }
}

/// Seq numbers of our own requests.
const SEQ_TIMING: u32 = 0x7000_0001;
const SEQ_OTHER: u32 = 0x7000_0002;
/// How often the clock is refreshed while playing.
const TIMING_EVERY: Duration = Duration::from_millis(40);

fn channel_map(ch: u8) -> ChannelMap {
    use ChannelPosition::*;
    match ch {
        1 => ChannelMap::mono(),
        2 => ChannelMap::stereo(),
        n => {
            let order = [
                FrontLeft,
                FrontRight,
                FrontCenter,
                Lfe,
                RearLeft,
                RearRight,
                SideLeft,
                SideRight,
            ];
            ChannelMap::new(order.into_iter().take(usize::from(n)))
        }
    }
}

fn volume(ch: u8, v: f32, mute: bool) -> ChannelVolume {
    if mute || v <= 0.0 {
        return ChannelVolume::muted(ch);
    }
    let mut cv = ChannelVolume::empty();
    for _ in 0..ch {
        cv.push(Volume::from_linear(v.clamp(0.0, 1.0)));
    }
    cv
}

fn read_payload(r: &mut BufReader<UnixStream>) -> std::io::Result<(protocol::Descriptor, Vec<u8>)> {
    let desc = protocol::read_descriptor(r).map_err(std::io::Error::other)?;
    if desc.length > 1 << 20 {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    let mut v = vec![0; desc.length as usize];
    r.read_exact(&mut v)?;
    Ok((desc, v))
}

fn reply<T: protocol::CommandReply>(
    r: &mut BufReader<UnixStream>,
    version: u16,
) -> Result<T, Unavailable> {
    protocol::read_reply_message::<T>(r, version)
        .map(|(_, t)| t)
        .map_err(|_| Unavailable)
}

impl PulseSink {
    /// The server socket from the environment (`PULSE_SERVER`, `$XDG_RUNTIME_DIR/pulse`).
    pub fn socket() -> Option<PathBuf> {
        pulseaudio::socket_path_from_env()
    }

    /// Connect to `path` and create a stream (`rate` Hz, `channels`, s16le), started
    /// corked. `volume` 0..=1.
    pub fn open(
        path: &std::path::Path,
        rate: u32,
        channels: u8,
        vol: f32,
        mute: bool,
    ) -> Result<PulseSink, Unavailable> {
        if !(1..=192_000).contains(&rate) || !(1..=8).contains(&channels) {
            return Err(Unavailable);
        }
        let sock = UnixStream::connect(path).map_err(|_| Unavailable)?;
        sock.set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|_| Unavailable)?;
        let mut r = BufReader::new(sock.try_clone().map_err(|_| Unavailable)?);
        let mut w = sock;
        let cookie = pulseaudio::cookie_path_from_env()
            .and_then(|p| std::fs::read(p).ok())
            .unwrap_or_default();
        let auth = protocol::AuthParams {
            version: protocol::MAX_VERSION,
            supports_shm: false,
            supports_memfd: false,
            cookie,
        };
        protocol::write_command_message(&mut w, 0, &Command::Auth(auth), protocol::MAX_VERSION)
            .map_err(|_| Unavailable)?;
        let a: protocol::AuthReply = reply(&mut r, protocol::MAX_VERSION)?;
        let version = protocol::MAX_VERSION.min(a.version);
        let mut props = Props::new();
        props.set(Prop::ApplicationName, c"Apotheosis");
        protocol::write_command_message(&mut w, 1, &Command::SetClientName(props), version)
            .map_err(|_| Unavailable)?;
        let _: protocol::SetClientNameReply = reply(&mut r, version)?;
        let mut sprops = Props::new();
        sprops.set(Prop::MediaRole, c"video");
        let params = protocol::PlaybackStreamParams {
            sample_spec: SampleSpec {
                format: SampleFormat::S16Le,
                channels,
                sample_rate: rate,
            },
            channel_map: channel_map(channels),
            cvolume: Some(volume(channels, vol, mute)),
            sink_name: Some(CString::from(c"@DEFAULT_SINK@")),
            props: sprops,
            flags: protocol::stream::StreamFlags {
                start_corked: true,
                ..Default::default()
            },
            ..Default::default()
        };
        protocol::write_command_message(&mut w, 2, &Command::CreatePlaybackStream(params), version)
            .map_err(|_| Unavailable)?;
        let s: protocol::CreatePlaybackStreamReply = reply(&mut r, version)?;
        let _ = r.get_ref().set_read_timeout(None);
        let shared = Arc::new(Mutex::new(Shared {
            owed: u64::from(s.requested_bytes),
            paused: true,
            ..Shared::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let byte_rate = u64::from(rate) * u64::from(channels) * 2;
        let run = Runner {
            r,
            w,
            version,
            channel: s.channel,
            index: s.stream_index,
            shared: Arc::clone(&shared),
            stop: Arc::clone(&stop),
            byte_rate,
            asked: None,
        };
        let thread = std::thread::Builder::new()
            .name("apoth-audio".into())
            .spawn(move || run.run())
            .map_err(|_| Unavailable)?;
        Ok(PulseSink {
            shared,
            stop,
            thread: Some(thread),
            byte_rate,
            channels,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl AudioSink for PulseSink {
    fn write(&mut self, pcm: &[u8]) {
        self.lock().buf.extend(pcm);
    }

    fn queued(&self) -> usize {
        let s = self.lock();
        let played = s.timing.map_or(0, |(b, ..)| b.max(0) as u64);
        s.buf.len() + s.sent.saturating_sub(played) as usize
    }

    fn played_us(&self) -> Option<u64> {
        let s = self.lock();
        let (bytes, at, playing) = s.timing?;
        let mut us = (bytes.max(0) as u64).saturating_mul(1_000_000) / self.byte_rate.max(1);
        if playing && !s.paused {
            us += at.elapsed().as_micros() as u64;
        }
        Some(us)
    }

    fn cork(&mut self, paused: bool) {
        let mut s = self.lock();
        s.paused = paused;
        s.cork = Some(paused);
        if let Some((b, at, playing)) = s.timing {
            // Freeze the extrapolation at the pause.
            let extra = if playing && paused {
                (at.elapsed().as_micros() as u64 * self.byte_rate / 1_000_000) as i64
            } else {
                0
            };
            s.timing = Some((b + extra, Instant::now(), playing && !paused));
        }
    }

    fn flush(&mut self) {
        let mut s = self.lock();
        s.buf.clear();
        s.flush = true;
        s.sent = 0;
        s.timing = None;
        s.base = None;
        s.epoch = s.epoch.wrapping_add(1);
    }

    fn set_volume(&mut self, v: f32, mute: bool) {
        self.lock().volume = Some(volume(self.channels, v, mute));
    }
}

impl Drop for PulseSink {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Runner {
    r: BufReader<UnixStream>,
    w: UnixStream,
    version: u16,
    channel: u32,
    index: u32,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    byte_rate: u64,
    /// (bytes sent, epoch) when the outstanding timing request went out.
    asked: Option<(u64, u32)>,
}

impl Runner {
    fn readable(&self, ms: i32) -> bool {
        use rustix::event::{PollFd, PollFlags, Timespec, poll};
        if !self.r.buffer().is_empty() {
            return true;
        }
        let ts = Timespec {
            tv_sec: 0,
            tv_nsec: i64::from(ms) * 1_000_000,
        };
        let mut fds = [PollFd::new(self.r.get_ref(), PollFlags::IN)];
        matches!(poll(&mut fds, Some(&ts)), Ok(n) if n > 0)
    }

    fn cmd(&mut self, seq: u32, c: &Command) -> bool {
        protocol::write_command_message(&mut self.w, seq, c, self.version).is_ok()
    }

    fn run(mut self) {
        let mut last_timing = Instant::now() - TIMING_EVERY;
        let mut chunk = Vec::new();
        while !self.stop.load(Ordering::Acquire) {
            if self.readable(10) {
                let Ok((desc, payload)) = read_payload(&mut self.r) else {
                    break;
                };
                if desc.channel != u32::MAX {
                    continue;
                }
                let mut cur = Cursor::new(&payload[..]);
                let Ok((seq, msg)) = Command::read_tag_prefixed(&mut cur, self.version) else {
                    continue;
                };
                match msg {
                    Command::Request(rq) if rq.channel == self.channel => {
                        self.lock().owed += u64::from(rq.length);
                    }
                    Command::Reply if seq == SEQ_TIMING => {
                        let mut ts = TagStructReader::new(&mut cur, self.version);
                        if let (Ok(t), Some(asked)) = (ts.read::<PlaybackLatency>(), self.asked) {
                            self.timing(&t, asked);
                        }
                    }
                    _ => {}
                }
            }
            // Control first, then data.
            let (cork, flush, vol) = {
                let mut s = self.lock();
                (s.cork.take(), std::mem::take(&mut s.flush), s.volume.take())
            };
            if flush && !self.cmd(SEQ_OTHER, &Command::FlushPlaybackStream(self.channel)) {
                break;
            }
            if let Some(v) = vol {
                let p = protocol::SetStreamVolumeParams {
                    index: self.index,
                    volume: v,
                };
                if !self.cmd(SEQ_OTHER, &Command::SetSinkInputVolume(p)) {
                    break;
                }
            }
            if let Some(c) = cork {
                let p = CorkStreamParams {
                    channel: self.channel,
                    cork: c,
                };
                if !self.cmd(SEQ_OTHER, &Command::CorkPlaybackStream(p)) {
                    break;
                }
            }
            chunk.clear();
            {
                let mut s = self.lock();
                let n = (s.owed.min(s.buf.len() as u64) as usize).min(64 << 10);
                chunk.extend(s.buf.drain(..n));
                s.owed -= n as u64;
                s.sent += n as u64;
            }
            if !chunk.is_empty()
                && protocol::write_memblock(&mut self.w, self.channel, &chunk, 0).is_err()
            {
                break;
            }
            if last_timing.elapsed() >= TIMING_EVERY {
                last_timing = Instant::now();
                self.asked = {
                    let s = self.lock();
                    Some((s.sent, s.epoch))
                };
                let p = LatencyParams {
                    channel: self.channel,
                    now: std::time::SystemTime::now(),
                };
                if !self.cmd(SEQ_TIMING, &Command::GetPlaybackLatency(p)) {
                    break;
                }
            }
        }
        let _ = self.cmd(SEQ_OTHER, &Command::DeletePlaybackStream(self.channel));
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn timing(&self, t: &PlaybackLatency, (sent, epoch): (u64, u32)) {
        let mut s = self.lock();
        if epoch != s.epoch {
            return;
        }
        // The write offset includes what we had sent when we asked: rebase on it.
        let base = *s.base.get_or_insert(t.write_offset - sent as i64);
        let sink_bytes = (t.sink_usec as i128 * self.byte_rate as i128 / 1_000_000) as i64;
        let played = t.read_offset - base - sink_bytes;
        s.timing = Some((played, Instant::now(), t.playing));
    }
}

/// A sink with a clock the test drives (A/V sync tests, the no-server case in CI). With
/// [`FakeSink::realtime`] its clock runs at the wall clock, capped by what was written.
#[derive(Debug, Clone, Default)]
pub struct FakeSink {
    pub state: Arc<Mutex<FakeState>>,
}

#[derive(Debug, Default)]
pub struct FakeState {
    pub written: u64,
    pub played_us: Option<u64>,
    pub paused: bool,
    pub flushes: u32,
    pub volume: (f32, bool),
    /// Realtime mode: bytes per second (0: the test sets `played_us`).
    pub byte_rate: u64,
    run_from: Option<Instant>,
    run_us: u64,
}

impl FakeState {
    fn realtime_us(&self) -> Option<u64> {
        if self.written == 0 {
            return None;
        }
        let ran = self.run_us + self.run_from.map_or(0, |t| t.elapsed().as_micros() as u64);
        Some(ran.min(self.written * 1_000_000 / self.byte_rate))
    }
}

impl FakeSink {
    /// A sink that plays in real time at `byte_rate`, starting corked.
    pub fn realtime(byte_rate: u64) -> FakeSink {
        let s = FakeSink::default();
        if let Ok(mut st) = s.state.lock() {
            st.byte_rate = byte_rate.max(1);
            st.paused = true;
        }
        s
    }
}

impl AudioSink for FakeSink {
    fn write(&mut self, pcm: &[u8]) {
        if let Ok(mut s) = self.state.lock() {
            if s.byte_rate > 0 && s.written == 0 && !s.paused {
                s.run_from = Some(Instant::now());
            }
            s.written += pcm.len() as u64;
        }
    }
    fn queued(&self) -> usize {
        let Ok(s) = self.state.lock() else {
            return 0;
        };
        if s.byte_rate == 0 {
            return 0;
        }
        let ran = s.realtime_us().unwrap_or(0);
        // Played to the end counts every byte: the µs round trip must not leave a byte queued
        // for ever (the player waits for an empty sink before it reports the end).
        if ran >= s.written * 1_000_000 / s.byte_rate {
            return 0;
        }
        let played = ran * s.byte_rate / 1_000_000;
        s.written.saturating_sub(played) as usize
    }
    fn played_us(&self) -> Option<u64> {
        let s = self.state.lock().ok()?;
        if s.byte_rate > 0 {
            return s.realtime_us();
        }
        s.played_us
    }
    fn cork(&mut self, paused: bool) {
        if let Ok(mut s) = self.state.lock() {
            if s.byte_rate > 0 && paused != s.paused {
                if paused {
                    s.run_us = s.realtime_us().unwrap_or(0);
                    s.run_from = None;
                } else if s.written > 0 {
                    s.run_from = Some(Instant::now());
                }
            }
            s.paused = paused;
        }
    }
    fn flush(&mut self) {
        if let Ok(mut s) = self.state.lock() {
            s.flushes += 1;
            s.played_us = None;
            s.written = 0;
            s.run_us = 0;
            s.run_from = None;
        }
    }
    fn set_volume(&mut self, v: f32, mute: bool) {
        if let Ok(mut s) = self.state.lock() {
            s.volume = (v, mute);
        }
    }
}
