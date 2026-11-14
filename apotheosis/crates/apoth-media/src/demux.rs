// SPDX-License-Identifier: AGPL-3.0-only
//! Containers (T-16 §3, MEDIA-05, -07): Matroska/WebM through `matroska-demuxer` (video and
//! audio), and audio-only files (Ogg, FLAC, MP3, WAV, ADTS) through Symphonia's readers.
//! Runs only in the helper, on untrusted bytes; covered by the `mkv_demux` fuzz target.
//! MP4 is not demuxed here: it stays Tier A (T-16 §7, the M34 spike).

use std::io::BufReader;

use matroska_demuxer::{Frame, MatroskaFile, TrackType};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::AudioCodecParameters;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{
    FormatOptions, FormatReader, SeekMode, SeekTo, TrackType as SymTrack,
};
use symphonia::core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase, Timestamp};

use crate::wire::Fail;

/// EBML magic: a Matroska or WebM file.
const EBML: [u8; 4] = [0x1a, 0x45, 0xdf, 0xa3];
/// Largest packet accepted from any container.
pub const MAX_PACKET: usize = 16 << 20;

/// An untrusted input: an fd the terminal opened (`File`) or bytes (tests, fuzzing).
pub type Input = Box<dyn MediaSource>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Video,
    Audio,
}

/// One compressed packet.
#[derive(Clone, Debug, Default)]
pub struct Packet {
    pub video: bool,
    pub pts_us: i64,
    pub data: Vec<u8>,
}

/// The video stream (AV1 only in tier B).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
}

/// The audio codec of the chosen audio track.
#[derive(Clone, Debug)]
pub enum AudioCodec {
    /// Opus (`opus-decoder`), with the pre-skip to drop (µs).
    Opus { pre_skip_us: u64 },
    /// Anything Symphonia decodes, with its codec parameters.
    Symphonia(Box<AudioCodecParameters>),
}

#[derive(Clone, Debug)]
pub struct AudioInfo {
    pub codec: AudioCodec,
    pub rate: u32,
    pub channels: u8,
}

enum Inner {
    Mkv {
        file: Box<MatroskaFile<BufReader<Input>>>,
        vtrack: u64,
        atrack: u64,
        /// Nanoseconds per timestamp tick.
        scale: u64,
        frame: Frame,
    },
    Sym {
        reader: Box<dyn FormatReader>,
        track: u32,
        tb: Option<TimeBase>,
    },
}

/// An open container positioned at its first packet.
pub struct Source {
    inner: Inner,
    pub video: Option<VideoInfo>,
    pub audio: Option<AudioInfo>,
    pub duration_ms: u64,
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Source")
            .field("video", &self.video)
            .field("audio", &self.audio.as_ref().map(|a| (a.rate, a.channels)))
            .finish_non_exhaustive()
    }
}

fn sym_channels(n: u8) -> symphonia::core::audio::Channels {
    symphonia::core::audio::Channels::Discrete(u16::from(n))
}

/// Codec parameters for a Matroska audio track Symphonia decodes.
fn mkv_audio(codec: &str, private: Option<&[u8]>, rate: u32, ch: u8) -> Option<AudioCodec> {
    use symphonia::core::codecs::audio::well_known::{
        CODEC_ID_AAC, CODEC_ID_FLAC, CODEC_ID_MP3, CODEC_ID_VORBIS,
    };
    let (id, extra): (_, Option<Vec<u8>>) = match codec {
        "A_VORBIS" => (CODEC_ID_VORBIS, Some(private?.to_vec())),
        "A_AAC" | "A_AAC/MPEG4/LC" | "A_AAC/MPEG2/LC" => {
            (CODEC_ID_AAC, private.map(<[u8]>::to_vec))
        }
        // CodecPrivate: "fLaC", a metadata block header, then STREAMINFO (34 bytes).
        "A_FLAC" => {
            let p = private?;
            (CODEC_ID_FLAC, Some(p.get(8..42)?.to_vec()))
        }
        "A_MPEG/L3" => (CODEC_ID_MP3, None),
        _ => return None,
    };
    let mut p = AudioCodecParameters::new();
    p.codec = id;
    p.with_sample_rate(rate).with_channels(sym_channels(ch));
    if let Some(e) = extra {
        p.with_extra_data(e.into_boxed_slice());
    }
    Some(AudioCodec::Symphonia(Box::new(p)))
}

impl Source {
    /// Open `input`. `Unsupported`: no container or codec we play; `BadInput`: broken.
    pub fn open(mut input: Input) -> Result<Source, Fail> {
        let mut magic = [0u8; 4];
        let is_mkv = {
            use std::io::{Read, Seek, SeekFrom};
            let ok = input.read_exact(&mut magic).is_ok();
            input.seek(SeekFrom::Start(0)).map_err(|_| Fail::BadInput)?;
            ok && magic == EBML
        };
        if is_mkv {
            Self::open_mkv(input)
        } else {
            Self::open_sym(input)
        }
    }

    fn open_mkv(input: Input) -> Result<Source, Fail> {
        let file = MatroskaFile::open(BufReader::new(input)).map_err(|_| Fail::BadInput)?;
        let scale = file.info().timestamp_scale().get();
        let duration_ms = file
            .info()
            .duration()
            .filter(|d| d.is_finite() && *d > 0.0)
            .map_or(0, |d| (d * scale as f64 / 1e6) as u64);
        let mut video = None;
        let mut vtrack = 0;
        let mut audio = None;
        let mut atrack = 0;
        let mut other_video = false;
        for t in file.tracks() {
            match t.track_type() {
                TrackType::Video if vtrack == 0 => {
                    if t.codec_id() != "V_AV1" {
                        other_video = true;
                        continue;
                    }
                    let Some(v) = t.video() else { continue };
                    let (w, h) = (v.pixel_width().get(), v.pixel_height().get());
                    if w > 16384 || h > 16384 {
                        return Err(Fail::TooLarge);
                    }
                    video = Some(VideoInfo {
                        width: w as u32,
                        height: h as u32,
                    });
                    vtrack = t.track_number().get();
                }
                TrackType::Audio if atrack == 0 => {
                    let Some(a) = t.audio() else { continue };
                    let rate = a
                        .output_sampling_frequency()
                        .unwrap_or(a.sampling_frequency());
                    let ch = a.channels().get();
                    if !(1.0..=192_000.0).contains(&rate) || ch > 8 {
                        continue;
                    }
                    let (rate, ch) = (rate as u32, ch as u8);
                    let codec = if t.codec_id() == "A_OPUS" {
                        if ch > 2 {
                            continue; // multistream Opus is not decoded (tier B gap)
                        }
                        let pre_skip_us = t.codec_delay().unwrap_or(0) / 1000;
                        Some(AudioCodec::Opus { pre_skip_us })
                    } else {
                        mkv_audio(t.codec_id(), t.codec_private(), rate, ch)
                    };
                    let Some(codec) = codec else { continue };
                    let rate = if matches!(codec, AudioCodec::Opus { .. }) {
                        48_000
                    } else {
                        rate
                    };
                    audio = Some(AudioInfo {
                        codec,
                        rate,
                        channels: ch,
                    });
                    atrack = t.track_number().get();
                }
                _ => {}
            }
        }
        if video.is_none() && (other_video || audio.is_none()) {
            return Err(Fail::Unsupported);
        }
        Ok(Source {
            inner: Inner::Mkv {
                file: Box::new(file),
                vtrack,
                atrack,
                scale,
                frame: Frame::default(),
            },
            video,
            audio,
            duration_ms,
        })
    }

    fn open_sym(input: Input) -> Result<Source, Fail> {
        let mss = MediaSourceStream::new(input, MediaSourceStreamOptions::default());
        let reader = symphonia::default::get_probe()
            .probe(
                &Hint::new(),
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .map_err(|_| Fail::Unsupported)?;
        let track = reader
            .default_track(SymTrack::Audio)
            .ok_or(Fail::Unsupported)?;
        let Some(CodecParameters::Audio(p)) = track.codec_params.clone() else {
            return Err(Fail::Unsupported);
        };
        let rate = p.sample_rate.ok_or(Fail::Unsupported)?;
        let channels = p.channels.as_ref().map_or(0, |c| c.count());
        if !(1..=192_000).contains(&rate) || !(1..=8).contains(&channels) {
            return Err(Fail::Unsupported);
        }
        let tb = track.time_base;
        let duration_ms = match (tb, track.num_frames) {
            (Some(tb), Some(n)) => i64::try_from(n)
                .ok()
                .and_then(|n| tb.calc_time(Timestamp::new(n)))
                .map_or(0, |t| t.as_millis().max(0) as u64),
            _ => 0,
        };
        let id = track.id;
        Ok(Source {
            inner: Inner::Sym {
                reader,
                track: id,
                tb,
            },
            video: None,
            audio: Some(AudioInfo {
                codec: AudioCodec::Symphonia(Box::new(p)),
                rate,
                channels: channels as u8,
            }),
            duration_ms,
        })
    }

    /// The next packet of the chosen tracks, or `None` at the end.
    pub fn next_packet(&mut self) -> Result<Option<Packet>, Fail> {
        match &mut self.inner {
            Inner::Mkv {
                file,
                vtrack,
                atrack,
                scale,
                frame,
            } => loop {
                if !file.next_frame(frame).map_err(|_| Fail::BadInput)? {
                    return Ok(None);
                }
                let video = frame.track == *vtrack && *vtrack != 0;
                if !video && (frame.track != *atrack || *atrack == 0) {
                    continue;
                }
                if frame.data.len() > MAX_PACKET {
                    return Err(Fail::BadInput);
                }
                let ns = u128::from(frame.timestamp) * u128::from(*scale);
                return Ok(Some(Packet {
                    video,
                    pts_us: i64::try_from(ns / 1000).unwrap_or(i64::MAX),
                    data: std::mem::take(&mut frame.data),
                }));
            },
            Inner::Sym { reader, track, tb } => loop {
                let Some(p) = reader.next_packet().map_err(|_| Fail::BadInput)? else {
                    return Ok(None);
                };
                if p.track_id != *track {
                    continue;
                }
                if p.data.len() > MAX_PACKET {
                    return Err(Fail::BadInput);
                }
                let pts_us = tb.and_then(|tb| tb.calc_time(p.pts)).map_or(0, |t| {
                    i64::try_from(t.as_nanos() / 1000).unwrap_or(i64::MAX)
                });
                return Ok(Some(Packet {
                    video: false,
                    pts_us,
                    data: p.data.into_vec(),
                }));
            },
        }
    }

    /// Seek near `ms` (the next packets start at or before it, at a keyframe for video).
    pub fn seek(&mut self, ms: u64) -> Result<(), Fail> {
        match &mut self.inner {
            Inner::Mkv { file, scale, .. } => {
                let ticks = u128::from(ms) * 1_000_000 / u128::from((*scale).max(1));
                file.seek(u64::try_from(ticks).unwrap_or(u64::MAX))
                    .map_err(|_| Fail::BadInput)
            }
            Inner::Sym { reader, track, .. } => reader
                .seek(
                    SeekMode::Coarse,
                    SeekTo::Time {
                        time: Time::from_millis_u64(ms),
                        track_id: Some(*track),
                    },
                )
                .map(|_| ())
                .map_err(|_| Fail::BadInput),
        }
    }
}
