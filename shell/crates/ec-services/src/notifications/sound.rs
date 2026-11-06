// SPDX-License-Identifier: AGPL-3.0-only
//! Notification sounds, played in-house over the PulseAudio native protocol
//! (pipewire-pulse serves it) with the same pure-Rust client the audio widget
//! uses. No new dependency.
//!
//! Sources: a `sound-file` (a WAV, parsed here, size and length capped) or a
//! `sound-name` (a short tone synthesised in code). Playback runs on one
//! worker thread behind a one-slot queue: the notification path only ever
//! does a `try_send`, so it never waits on audio, and a sound that arrives
//! while another is still playing is dropped. Any failure (no audio server,
//! bad file) drops the sound; it is reported only when `EC_NOTIFY_DEBUG` is
//! set. The decision whether to sound at all is [`plan`]; mode, DND and the
//! `mute` rule are already folded into the server's verdict, and the player is
//! only asked on [`Verdict::Show`](super::history::Verdict).

use std::f32::consts::TAU;
use std::ffi::CString;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, SyncSender};
use std::time::{Duration, Instant};

use pulseaudio::protocol::{
    self, ChannelMap, Command, CreatePlaybackStreamReply, PlaybackStreamParams, Prop, Props, SampleFormat,
    SampleSpec,
};

use pulseaudio::protocol::stream::BufferAttr;

use super::compositor::CONFIG_ID;
use super::{Notification, Urgency};
use crate::audio::wire::{self, Conn};

/// At most one sound per this long.
pub(super) const MIN_GAP: Duration = Duration::from_millis(250);
/// Largest WAV file read.
const MAX_FILE: usize = 2 * 1024 * 1024;
/// Longest sound played.
const MAX_SECONDS: u64 = 10;
/// Synthesised tones: mono S16 at this rate.
const RATE: u32 = 48_000;
/// Peak of a synthesised tone, as a fraction of full scale. Soft.
const PEAK: f32 = 0.3;
/// Bytes per stream write. Divisible by every frame size we send.
const CHUNK: usize = 8192;

fn debug(msg: &str) {
    if std::env::var_os("EC_NOTIFY_DEBUG").is_some() {
        eprintln!("ec-notifications sound: {msg}");
    }
}

// ---------------------------------------------------------------- samples

/// Decoded audio in a format the server takes as is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Pcm {
    format: SampleFormat,
    channels: u8,
    rate: u32,
    data: Vec<u8>,
}

impl Pcm {
    fn frame_bytes(&self) -> usize {
        let width = match self.format {
            SampleFormat::U8 => 1,
            SampleFormat::S16Le => 2,
            _ => 4,
        };
        width * usize::from(self.channels)
    }

    fn duration(&self) -> Duration {
        let frames = (self.data.len() / self.frame_bytes().max(1)) as u64;
        Duration::from_micros(frames * 1_000_000 / u64::from(self.rate.max(1)))
    }
}

/// Parse a RIFF/WAVE file: PCM 8 or 16-bit, or 32-bit float, mono or stereo.
/// Anything else is refused rather than guessed at.
pub(super) fn parse_wav(b: &[u8]) -> Result<Pcm, &'static str> {
    if b.len() > MAX_FILE {
        return Err("file too large");
    }
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file");
    }
    let mut fmt: Option<(SampleFormat, u8, u32, usize)> = None;
    let mut data: Option<&[u8]> = None;
    let mut p = 12usize;
    while p + 8 <= b.len() {
        let id = &b[p..p + 4];
        let size = u32::from_le_bytes([b[p + 4], b[p + 5], b[p + 6], b[p + 7]]) as usize;
        let start = p + 8;
        // A truncated or streamed file declares more than it has.
        let body = &b[start..start.saturating_add(size).min(b.len())];
        match id {
            b"fmt " => {
                if body.len() < 16 {
                    return Err("short fmt chunk");
                }
                let u16at = |i: usize| u16::from_le_bytes([body[i], body[i + 1]]);
                let mut tag = u16at(0);
                if tag == 0xFFFE && body.len() >= 26 {
                    tag = u16at(24); // WAVE_FORMAT_EXTENSIBLE: sub-format code
                }
                let channels = u16at(2);
                let rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
                let align = usize::from(u16at(12));
                let bits = u16at(14);
                let format = match (tag, bits) {
                    (1, 8) => SampleFormat::U8,
                    (1, 16) => SampleFormat::S16Le,
                    (3, 32) => SampleFormat::Float32Le,
                    _ => return Err("unsupported sample format"),
                };
                if !(1..=2).contains(&channels) {
                    return Err("only mono and stereo");
                }
                if !(8_000..=192_000).contains(&rate) {
                    return Err("unsupported sample rate");
                }
                if align != usize::from(channels) * usize::from(bits / 8) {
                    return Err("inconsistent block size");
                }
                fmt = Some((format, channels as u8, rate, align));
            }
            b"data" => {
                data = Some(body);
                break;
            }
            _ => {}
        }
        p = start.saturating_add(size).saturating_add(size & 1);
    }
    let ((format, channels, rate, align), data) = fmt.zip(data).ok_or("missing fmt or data chunk")?;
    let data = &data[..data.len() - data.len() % align];
    if data.is_empty() {
        return Err("no audio");
    }
    if (data.len() / align) as u64 > MAX_SECONDS * u64::from(rate) {
        return Err("too long");
    }
    let mut data = data.to_vec();
    if format == SampleFormat::Float32Le {
        // A hostile or broken file must not hand the server NaN or +-inf.
        for s in data.chunks_exact_mut(4) {
            let v = f32::from_le_bytes([s[0], s[1], s[2], s[3]]);
            let v = if v.is_finite() { v.clamp(-1.0, 1.0) } else { 0.0 };
            s.copy_from_slice(&v.to_le_bytes());
        }
    }
    Ok(Pcm {
        format,
        channels,
        rate,
        data,
    })
}

/// The tones `sound-name` maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tone {
    /// A soft single blip.
    Blip,
    /// Two notes, rising: a warning.
    Warning,
    /// Two lower notes, falling: an error or a critical notice.
    Critical,
}

impl Tone {
    /// `(hertz, milliseconds)` per note.
    fn notes(self) -> &'static [(f32, u32)] {
        match self {
            Self::Blip => &[(880.0, 140)],
            Self::Warning => &[(587.0, 110), (784.0, 150)],
            Self::Critical => &[(523.0, 160), (392.0, 220)],
        }
    }
}

/// A soft sine per note: 5 ms attack, quadratic decay to silence.
pub(super) fn synth(tone: Tone) -> Pcm {
    let attack = (RATE / 200) as f32;
    let mut data = Vec::new();
    for &(hz, ms) in tone.notes() {
        let n = (RATE as u64 * u64::from(ms) / 1000) as usize;
        for i in 0..n {
            let t = i as f32 / RATE as f32;
            let decay = 1.0 - i as f32 / n as f32;
            let env = (i as f32 / attack).min(1.0) * decay * decay;
            let s = (TAU * hz * t).sin() * PEAK * env;
            data.extend_from_slice(&((s * 32_767.0) as i16).to_le_bytes());
        }
    }
    Pcm {
        format: SampleFormat::S16Le,
        channels: 1,
        rate: RATE,
        data,
    }
}

// ------------------------------------------------------------------ gating

/// What to play for one notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Source {
    Tone(Tone),
    /// A WAV file, read on the worker. `fallback` plays if it cannot be used.
    File {
        path: PathBuf,
        fallback: Option<Tone>,
    },
}

fn named(name: &str) -> Option<Tone> {
    Some(match name {
        "message-new-instant"
        | "message-new-email"
        | "message"
        | "dialog-information"
        | "bell"
        | "complete"
        | "device-added"
        | "device-removed"
        | "power-plug"
        | "power-unplug" => Tone::Blip,
        "dialog-warning" | "battery-low" | "network-connectivity-lost" => Tone::Warning,
        "dialog-error" | "dialog-error-critical" | "battery-caution" | "suspend-error" => Tone::Critical,
        _ => return None,
    })
}

/// A sound hint that asks for nothing: silence.
fn tone_for(name: &str, urgency: Urgency) -> Option<Tone> {
    if name.is_empty() {
        return None;
    }
    match (named(name), urgency) {
        (Some(t), _) => Some(t),
        (None, Urgency::Low) => None,
        (None, Urgency::Normal) => Some(Tone::Blip),
        (None, Urgency::Critical) => Some(Tone::Critical),
    }
}

/// Decide what, if anything, to play. Pure. Mode, DND and the `mute` rule are
/// the caller's verdict; this covers the notification's own hints. The
/// compositor's config notice never sounds.
pub(super) fn plan(n: &Notification) -> Option<Source> {
    if n.sound.suppress || n.id == CONFIG_ID {
        return None;
    }
    let fallback = tone_for(&n.sound.name, n.urgency);
    let file = n.sound.file.strip_prefix("file://").unwrap_or(&n.sound.file);
    if !file.is_empty() {
        let path = PathBuf::from(file);
        if path.is_absolute() {
            return Some(Source::File { path, fallback });
        }
    }
    fallback.map(Source::Tone)
}

/// Lets one sound through per [`MIN_GAP`].
#[derive(Debug, Default)]
pub(super) struct RateLimit {
    last: Option<Instant>,
}

impl RateLimit {
    pub(super) fn allow(&mut self, now: Instant) -> bool {
        if self
            .last
            .is_some_and(|t| now.saturating_duration_since(t) < MIN_GAP)
        {
            return false;
        }
        self.last = Some(now);
        true
    }
}

// ------------------------------------------------------------------ player

/// The notification server's handle on the worker.
pub(super) struct Player {
    tx: SyncSender<Source>,
    limit: RateLimit,
}

impl Player {
    pub(super) fn new() -> Self {
        let (tx, rx) = mpsc::sync_channel::<Source>(1);
        let spawned = std::thread::Builder::new()
            .name("eclipse-notification-sound".into())
            .spawn(move || {
                while let Ok(source) = rx.recv() {
                    if let Err(e) = play_source(&source) {
                        debug(&e);
                    }
                }
            });
        if spawned.is_err() {
            debug("no worker thread");
        }
        Self {
            tx,
            limit: RateLimit::default(),
        }
    }

    /// Call for a notification that is being shown. Never blocks.
    pub(super) fn notify(&mut self, n: &Notification) {
        let Some(source) = plan(n) else { return };
        if self.limit.allow(Instant::now()) {
            let _ = self.tx.try_send(source);
        }
    }
}

fn play_source(source: &Source) -> Result<(), String> {
    let pcm = match source {
        Source::Tone(t) => synth(*t),
        Source::File { path, fallback } => match load(path) {
            Ok(p) => p,
            Err(e) => match fallback {
                Some(t) => {
                    debug(&format!("sound file unusable ({e}); using tone"));
                    synth(*t)
                }
                None => return Err(e),
            },
        },
    };
    play(&pcm).map_err(|e| format!("playback failed: {e}"))
}

/// Read a WAV without ever blocking on it or reading more than the cap.
fn load(path: &Path) -> Result<Pcm, String> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(path)
        .map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("not a regular file".into());
    }
    let mut buf = Vec::new();
    file.take(MAX_FILE as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    parse_wav(&buf).map_err(str::to_owned)
}

// -------------------------------------------------------------- the stream

/// The `CreatePlaybackStream` request for `pcm`. The buffer is sized to hold
/// the whole sound and pre-buffering is off, so it starts at once and one
/// drain waits for the end.
fn stream_params(pcm: &Pcm) -> PlaybackStreamParams {
    let mut props = Props::new();
    props.set(Prop::MediaName, c"Notification");
    props.set(Prop::MediaRole, c"event");
    PlaybackStreamParams {
        sample_spec: SampleSpec {
            format: pcm.format,
            channels: pcm.channels,
            sample_rate: pcm.rate,
        },
        channel_map: if pcm.channels == 1 {
            ChannelMap::mono()
        } else {
            ChannelMap::stereo()
        },
        buffer_attr: BufferAttr {
            target_length: pcm.data.len() as u32,
            pre_buffering: 0,
            ..BufferAttr::default()
        },
        props,
        ..PlaybackStreamParams::default()
    }
}

fn play(pcm: &Pcm) -> wire::Result<()> {
    let path = pulseaudio::socket_path_from_env()
        .ok_or_else(|| protocol::ProtocolError::Io(std::io::ErrorKind::NotFound.into()))?;
    let name: CString = CString::from(c"Eclipse notifications");
    let mut conn = wire::connect(&path, &name)?;
    let reply: CreatePlaybackStreamReply =
        conn.request(&Command::CreatePlaybackStream(stream_params(pcm)))?;
    // Draining answers when the sound has been heard.
    conn.stream()
        .set_read_timeout(Some(pcm.duration() + Duration::from_secs(3)))?;

    let mut credit = reply.requested_bytes as usize;
    let mut sent = 0;
    while sent < pcm.data.len() {
        if credit == 0 {
            credit += wait_for_credit(&mut conn, reply.channel)?;
            continue;
        }
        let end = (sent + credit.min(CHUNK)).min(pcm.data.len());
        let chunk = &pcm.data[sent..end];
        protocol::write_memblock(&mut conn.stream(), reply.channel, chunk, 0)?;
        credit -= chunk.len();
        sent = end;
    }

    let seq = conn.send(&Command::DrainPlaybackStream(reply.channel))?;
    loop {
        if conn.read_frame()?.channel != u32::MAX {
            continue;
        }
        match Command::read_tag_prefixed(&mut conn.payload(), conn.version())? {
            (s, Command::Reply) if s == seq => return Ok(()),
            (_, Command::Error(e)) => return Err(protocol::ProtocolError::ServerError(e)),
            (_, Command::PlaybackStreamKilled(_)) => return Ok(()),
            _ => {}
        }
    }
}

/// Block until the server asks for more bytes on `channel`.
fn wait_for_credit(conn: &mut Conn, channel: u32) -> wire::Result<usize> {
    loop {
        if conn.read_frame()?.channel != u32::MAX {
            continue;
        }
        match Command::read_tag_prefixed(&mut conn.payload(), conn.version())?.1 {
            Command::Request(r) if r.channel == channel => return Ok(r.length as usize),
            Command::Error(e) => return Err(protocol::ProtocolError::ServerError(e)),
            Command::PlaybackStreamKilled(_) => {
                return Err(protocol::ProtocolError::Invalid("stream killed".into()))
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::Sound;

    fn wav(tag: u16, channels: u16, rate: u32, bits: u16, data: &[u8]) -> Vec<u8> {
        let align = channels * bits / 8;
        let mut v = b"RIFF".to_vec();
        v.extend((36 + data.len() as u32).to_le_bytes());
        v.extend(b"WAVEfmt ");
        v.extend(16u32.to_le_bytes());
        v.extend(tag.to_le_bytes());
        v.extend(channels.to_le_bytes());
        v.extend(rate.to_le_bytes());
        v.extend((rate * u32::from(align)).to_le_bytes());
        v.extend(align.to_le_bytes());
        v.extend(bits.to_le_bytes());
        v.extend(b"data");
        v.extend((data.len() as u32).to_le_bytes());
        v.extend(data);
        v
    }

    fn note(urgency: Urgency, name: &str, file: &str, suppress: bool) -> Notification {
        Notification {
            id: 7,
            urgency,
            sound: Sound {
                name: name.into(),
                file: file.into(),
                suppress,
            },
            ..Notification::default()
        }
    }

    #[test]
    fn wav_pcm16_parses() {
        let p = parse_wav(&wav(1, 2, 44_100, 16, &[0, 1, 2, 3, 4, 5, 6, 7])).unwrap();
        assert_eq!((p.format, p.channels, p.rate), (SampleFormat::S16Le, 2, 44_100));
        assert_eq!(p.data.len(), 8);
    }

    #[test]
    fn wav_pcm8_and_float_parse_and_float_is_sanitised() {
        let p = parse_wav(&wav(1, 1, 8_000, 8, &[1, 2, 3])).unwrap();
        assert_eq!(p.format, SampleFormat::U8);
        let mut d = f32::NAN.to_le_bytes().to_vec();
        d.extend(9.0f32.to_le_bytes());
        let p = parse_wav(&wav(3, 1, 48_000, 32, &d)).unwrap();
        assert_eq!(p.format, SampleFormat::Float32Le);
        assert_eq!(&p.data[0..4], &0.0f32.to_le_bytes());
        assert_eq!(&p.data[4..8], &1.0f32.to_le_bytes());
    }

    #[test]
    fn wav_partial_frame_is_trimmed_and_truncated_data_tolerated() {
        let mut w = wav(1, 1, 8_000, 16, &[0; 6]);
        w.truncate(w.len() - 1); // header still claims 6 bytes
        assert_eq!(parse_wav(&w).unwrap().data.len(), 4);
    }

    #[test]
    fn wav_rejects_what_it_does_not_understand() {
        assert!(parse_wav(b"").is_err());
        assert!(parse_wav(b"RIFFxxxxAVI LIST").is_err());
        assert!(parse_wav(&wav(1, 1, 8_000, 24, &[0; 6])).is_err()); // 24-bit
        assert!(parse_wav(&wav(2, 1, 8_000, 4, &[0; 6])).is_err()); // ADPCM
        assert!(parse_wav(&wav(1, 6, 8_000, 16, &[0; 12])).is_err()); // 6 channels
        assert!(parse_wav(&wav(1, 1, 100, 16, &[0; 4])).is_err()); // rate
        assert!(parse_wav(&wav(1, 1, 8_000, 16, &[])).is_err()); // empty
    }

    #[test]
    fn wav_size_and_length_are_capped() {
        // 11 s at 8 kHz mono 8-bit: small file, too long.
        assert_eq!(
            parse_wav(&wav(1, 1, 8_000, 8, &vec![128; 88_000])),
            Err("too long")
        );
        assert!(parse_wav(&wav(1, 1, 8_000, 8, &vec![128; 80_000])).is_ok());
        assert_eq!(
            parse_wav(&wav(1, 1, 48_000, 16, &vec![0; MAX_FILE])),
            Err("file too large")
        );
    }

    #[test]
    fn tones_are_short_soft_and_end_silent() {
        for t in [Tone::Blip, Tone::Warning, Tone::Critical] {
            let p = synth(t);
            assert!(p.duration() >= Duration::from_millis(100));
            assert!(p.duration() <= Duration::from_millis(500));
            let samples: Vec<i16> = p
                .data
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]))
                .collect();
            let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
            assert!(peak > 3_000, "{t:?} is audible");
            assert!(f32::from(peak) <= PEAK * 32_768.0, "{t:?} is soft");
            assert!(samples.last().unwrap().unsigned_abs() < 50, "{t:?} ends silent");
        }
        assert_ne!(synth(Tone::Blip), synth(Tone::Critical));
    }

    #[test]
    fn rate_limit_allows_one_per_gap() {
        let mut l = RateLimit::default();
        let t0 = Instant::now();
        assert!(l.allow(t0));
        assert!(!l.allow(t0 + Duration::from_millis(100)));
        assert!(!l.allow(t0 + Duration::from_millis(249)));
        assert!(l.allow(t0 + MIN_GAP));
        assert!(!l.allow(t0 + MIN_GAP + Duration::from_millis(1)));
    }

    #[test]
    fn gating_follows_the_hints() {
        // No hint, no sound.
        assert_eq!(plan(&note(Urgency::Critical, "", "", false)), None);
        // suppress-sound wins over everything.
        assert_eq!(plan(&note(Urgency::Normal, "message", "/a.wav", true)), None);
        // Known names map to tones.
        assert_eq!(
            plan(&note(Urgency::Low, "message-new-instant", "", false)),
            Some(Source::Tone(Tone::Blip))
        );
        assert_eq!(
            plan(&note(Urgency::Normal, "dialog-error", "", false)),
            Some(Source::Tone(Tone::Critical))
        );
        // Unknown names: default blip for Normal/Critical, nothing for Low.
        assert_eq!(plan(&note(Urgency::Low, "x-custom", "", false)), None);
        assert_eq!(
            plan(&note(Urgency::Normal, "x-custom", "", false)),
            Some(Source::Tone(Tone::Blip))
        );
        assert_eq!(
            plan(&note(Urgency::Critical, "x-custom", "", false)),
            Some(Source::Tone(Tone::Critical))
        );
    }

    #[test]
    fn gating_files() {
        assert_eq!(
            plan(&note(Urgency::Normal, "message", "file:///s/a.wav", false)),
            Some(Source::File {
                path: "/s/a.wav".into(),
                fallback: Some(Tone::Blip)
            })
        );
        // A relative path is not a path we will open.
        assert_eq!(plan(&note(Urgency::Normal, "", "a.wav", false)), None);
    }

    #[test]
    fn the_config_notice_never_sounds() {
        let mut n = note(Urgency::Critical, "dialog-error", "", false);
        n.id = CONFIG_ID;
        assert_eq!(plan(&n), None);
    }

    #[test]
    fn playback_request_encodes_for_the_wire() {
        // No private pulse server in CI, so check the bytes we would send.
        let pcm = synth(Tone::Blip);
        let params = stream_params(&pcm);
        let mut buf = Vec::new();
        protocol::write_command_message(
            &mut buf,
            3,
            &Command::CreatePlaybackStream(params.clone()),
            protocol::MAX_VERSION,
        )
        .unwrap();
        let desc = protocol::read_descriptor(&mut &buf[..]).unwrap();
        assert_eq!(desc.channel, u32::MAX);
        let payload = &buf[protocol::DESCRIPTOR_SIZE..];
        assert_eq!(desc.length as usize, payload.len());
        match Command::read_tag_prefixed(&mut &payload[..], protocol::MAX_VERSION).unwrap() {
            (3, Command::CreatePlaybackStream(back)) => {
                assert_eq!(back.sample_spec, params.sample_spec);
                assert_eq!(back.sample_spec.format, SampleFormat::S16Le);
                assert_eq!(back.buffer_attr.pre_buffering, 0);
                assert_eq!(back.buffer_attr.target_length as usize, pcm.data.len());
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn stream_data_is_framed_on_its_channel() {
        let mut buf = Vec::new();
        protocol::write_memblock(&mut buf, 5, &[1, 2, 3, 4], 0).unwrap();
        let desc = protocol::read_descriptor(&mut &buf[..]).unwrap();
        assert_eq!((desc.channel, desc.length), (5, 4));
        assert_eq!(&buf[protocol::DESCRIPTOR_SIZE..], &[1, 2, 3, 4]);
    }
}
