// SPDX-License-Identifier: AGPL-3.0-only
//! The monitor tap: a record stream on the default sink's monitor, reduced to
//! [`Bands`](super::Bands) on its own thread.
//!
//! The tap exists only while a [`Tap`] does. Dropping it shuts the socket,
//! which ends the pump thread and, with the connection, the record stream on
//! the server. Samples pass through the connection's frame buffer and the
//! analyzer's fixed ring and nowhere else (ADR 0065, "The monitor tap").

use std::ffi::{CStr, CString};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc::Sender;
use std::time::Instant;

use pulseaudio::protocol::{
    ChannelMap, Command, CreateRecordStreamReply, Prop, Props, RecordStreamParams, SampleFormat, SampleSpec,
};

use pulseaudio::protocol::stream::{BufferAttr, StreamFlags};

use super::spectrum::Analyzer;
use super::wire::{self, Conn, Result};
use super::{Internal, Update};

/// Mono at 48 kHz: the server downmixes, and 48 kHz keeps the top band
/// (16 kHz) under Nyquist.
const RATE: u32 = 48_000;

/// Ask for a packet about every 1/60 s, so frames arrive evenly rather than
/// in bursts.
const FRAGMENT_BYTES: u32 = (RATE / 60) * 4;

pub(super) struct Tap {
    id: u64,
    source: CString,
    stop: UnixStream,
}

impl Tap {
    /// Open a record stream on `source` and start pumping levels into
    /// `updates`. When the pump ends, for whatever reason, it says so on
    /// `internal` as [`Internal::TapEnded`] with this `id`.
    pub(super) fn start(
        path: &Path,
        source: &CStr,
        id: u64,
        updates: Sender<Update>,
        internal: Sender<Internal>,
    ) -> Result<Self> {
        let mut conn = wire::connect(path, c"Eclipse visualizer")?;
        let mut props = Props::new();
        props.set(Prop::MediaName, c"Visualizer");
        let reply: CreateRecordStreamReply =
            conn.request(&Command::CreateRecordStream(RecordStreamParams {
                sample_spec: SampleSpec {
                    format: SampleFormat::Float32Le,
                    channels: 1,
                    sample_rate: RATE,
                },
                channel_map: ChannelMap::mono(),
                source_name: Some(source.to_owned()),
                buffer_attr: BufferAttr {
                    fragment_size: FRAGMENT_BYTES,
                    ..BufferAttr::default()
                },
                flags: StreamFlags {
                    adjust_latency: true,
                    // Listening must not keep the device awake by itself.
                    no_inhibit_auto_suspend: true,
                    ..StreamFlags::default()
                },
                props,
                ..RecordStreamParams::default()
            }))?;
        // From here the pump blocks until data comes or the socket is shut.
        conn.stream().set_read_timeout(None)?;
        let stop = conn.stream().try_clone()?;
        std::thread::Builder::new()
            .name("eclipse-audio-tap".into())
            .spawn(move || {
                let started = Instant::now();
                pump(conn, &reply, &updates);
                let ran = started.elapsed();
                let _ = internal.send(Internal::TapEnded { id, ran });
            })?;
        Ok(Self {
            id,
            source: source.to_owned(),
            stop,
        })
    }

    pub(super) fn source(&self) -> &CStr {
        &self.source
    }

    pub(super) fn id(&self) -> u64 {
        self.id
    }

    /// A tap with no stream behind it, for tests of the service's
    /// bookkeeping.
    #[cfg(test)]
    pub(super) fn fake(id: u64, source: &CStr) -> Self {
        let (stop, _) = UnixStream::pair().expect("socketpair");
        Self {
            id,
            source: source.to_owned(),
            stop,
        }
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        let _ = self.stop.shutdown(Shutdown::Both);
    }
}

fn pump(mut conn: Conn, stream: &CreateRecordStreamReply, updates: &Sender<Update>) {
    let spec = stream.sample_spec;
    let width = match spec.format {
        SampleFormat::Float32Le | SampleFormat::S32Le => 4,
        SampleFormat::S16Le => 2,
        _ => return,
    };
    let channels = usize::from(spec.channels.max(1));
    let mut analyzer = Analyzer::new(spec.sample_rate);
    while let Ok(desc) = conn.read_frame() {
        if desc.channel == u32::MAX {
            let killed = matches!(
                Command::read_tag_prefixed(&mut conn.payload(), conn.version()),
                Ok((_, Command::RecordStreamKilled(_)))
            );
            if killed {
                return;
            }
            continue;
        }
        if desc.channel != stream.channel {
            continue;
        }
        for frame in conn.payload().chunks_exact(width * channels) {
            let sum: f32 = frame.chunks_exact(width).map(|s| decode(spec.format, s)).sum();
            if let Some(bands) = analyzer.push(sum / channels as f32) {
                if updates.send(Update::Levels(bands)).is_err() {
                    return;
                }
            }
        }
    }
}

/// One little-endian sample as `-1.0..=1.0`.
fn decode(format: SampleFormat, b: &[u8]) -> f32 {
    match (format, b) {
        (SampleFormat::Float32Le, &[a, b, c, d]) => f32::from_le_bytes([a, b, c, d]),
        (SampleFormat::S32Le, &[a, b, c, d]) => i32::from_le_bytes([a, b, c, d]) as f32 / 2_147_483_648.0,
        (SampleFormat::S16Le, &[a, b]) => f32::from(i16::from_le_bytes([a, b])) / 32_768.0,
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_decode_to_unit_range() {
        assert_eq!(decode(SampleFormat::Float32Le, &0.5f32.to_le_bytes()), 0.5);
        assert_eq!(decode(SampleFormat::S16Le, &i16::MIN.to_le_bytes()), -1.0);
        assert_eq!(decode(SampleFormat::S32Le, &0i32.to_le_bytes()), 0.0);
        assert_eq!(decode(SampleFormat::U8, &[1]), 0.0);
    }

    #[test]
    fn fragments_arrive_at_sixty_hertz() {
        assert_eq!(FRAGMENT_BYTES, 3200);
    }
}
