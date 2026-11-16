// SPDX-License-Identifier: AGPL-3.0-only
//! Audio decoding in the helper (T-16 §3, MEDIA-07, -08): Opus through `opus-decoder`,
//! AAC-LC, Vorbis, FLAC, MP3 (and PCM WAV) through Symphonia. Output is interleaved s16
//! at the stream's own rate and channel count (the sound server resamples).

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::packet::Packet;
use symphonia::core::units::{Duration, Timestamp};

use crate::demux::{AudioCodec, AudioInfo};
use crate::wire::Fail;

enum Inner {
    Opus {
        dec: Box<opus_decoder::OpusDecoder>,
        /// Samples per channel still to drop (pre-skip).
        skip: usize,
    },
    Sym(Box<dyn AudioDecoder>),
}

pub struct AudioDec {
    inner: Inner,
    channels: usize,
    scratch: Vec<i16>,
}

impl std::fmt::Debug for AudioDec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioDec")
            .field("channels", &self.channels)
            .finish_non_exhaustive()
    }
}

/// Opus frames are at most 120 ms: 5760 samples per channel at 48 kHz.
const OPUS_MAX: usize = 5760;

impl AudioDec {
    pub fn new(info: &AudioInfo) -> Result<AudioDec, Fail> {
        let channels = usize::from(info.channels);
        let inner = match &info.codec {
            AudioCodec::Opus { pre_skip_us } => Inner::Opus {
                dec: Box::new(
                    opus_decoder::OpusDecoder::new(48_000, channels)
                        .map_err(|_| Fail::Unsupported)?,
                ),
                skip: (*pre_skip_us * 48 / 1000) as usize,
            },
            AudioCodec::Symphonia(p) => Inner::Sym(
                symphonia::default::get_codecs()
                    .make_audio_decoder(p, &AudioDecoderOptions::default())
                    .map_err(|_| Fail::Unsupported)?,
            ),
        };
        Ok(AudioDec {
            inner,
            channels,
            scratch: Vec::new(),
        })
    }

    /// Decode one packet, appending interleaved s16 samples to `out`. A packet that does
    /// not decode is skipped (`Ok`), as decoders recover at the next one.
    pub fn decode(&mut self, data: &[u8], out: &mut Vec<i16>) -> Result<(), Fail> {
        match &mut self.inner {
            Inner::Opus { dec, skip } => {
                self.scratch.resize(OPUS_MAX * self.channels, 0);
                let Ok(n) = dec.decode(data, &mut self.scratch, false) else {
                    return Ok(());
                };
                let n = n.min(OPUS_MAX);
                let drop = (*skip).min(n);
                *skip -= drop;
                out.extend_from_slice(&self.scratch[drop * self.channels..n * self.channels]);
            }
            Inner::Sym(dec) => {
                let p = Packet::new(0, Timestamp::new(0), Duration::new(0), data);
                let Ok(buf) = dec.decode(&p) else {
                    return Ok(());
                };
                if buf.spec().channels().count() != self.channels {
                    return Err(Fail::Decode);
                }
                buf.copy_to_vec_interleaved::<i16>(&mut self.scratch);
                out.extend_from_slice(&self.scratch);
            }
        }
        Ok(())
    }

    /// After a seek.
    pub fn reset(&mut self) {
        match &mut self.inner {
            Inner::Opus { dec, .. } => dec.reset(),
            Inner::Sym(d) => d.reset(),
        }
    }
}
