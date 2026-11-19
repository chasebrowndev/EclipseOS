// SPDX-License-Identifier: AGPL-3.0-only
//! MP4 / fragmented MP4 (T-16 §3, ADR 0118): `shiguredo_mp4` reads the sample tables, this
//! module builds our own sample index from them. H.264 (`avc1`) video and AAC (`mp4a`)
//! audio play; any other codec is `Unsupported`. Runs only in the helper, on untrusted
//! bytes; covered by the `mp4_demux` fuzz target.
//!
//! Why an index: the demuxer's seek is sample-accurate rather than keyframe-snapped and the
//! fragmented demuxer has none. Every wanted sample is collected once as
//! (dts, pts, keyframe, offset, length) with the track's edit list applied (the demuxer
//! ignores `elst`, and x264's B-frame files carry a `media_time` offset), merged by
//! decode time, and a seek is a lookup in that index: the previous keyframe.
//!
//! Limits: at most [`MAX_SAMPLES`] samples, any one `moov` or `moof`+`mdat` range read
//! at most [`RANGE_MAX`] bytes, samples at most [`MAX_PACKET`](crate::demux::MAX_PACKET).
//! Only the first non-empty edit of a track and its leading empty edits are applied.

use std::io::{Read, Seek, SeekFrom};

use shiguredo_mp4::boxes::{MoovBox, SampleEntry};
use shiguredo_mp4::demux::{
    DemuxError, Fmp4FileDemuxer, Input as DemuxInput, Mp4FileDemuxer, Mp4FileKind,
    Mp4FileKindDetector, RequiredInput,
};
use shiguredo_mp4::{Decode, TrackKind};

use crate::demux::{AudioInfo, Input, MAX_PACKET, Packet, VideoInfo, aac_codec};
use crate::h264;
use crate::wire::Fail;

/// Samples (video and audio together) one file may index.
pub const MAX_SAMPLES: usize = 2_000_000;
/// Largest byte range the demuxer may ask for (a `moov`, or one fragment).
pub const RANGE_MAX: u64 = 64 << 20;
/// Demuxer round trips before the file counts as broken (guards a loop that never ends).
const ROUNDS_MAX: usize = 4_000_000;

/// `ftyp` at offset 4: an ISO base media file.
pub fn is_mp4(head: &[u8]) -> bool {
    head.get(4..8) == Some(b"ftyp")
}

/// One indexed sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub video: bool,
    pub key: bool,
    /// Presentation time on the edited timeline.
    pub pts_us: i64,
    /// Decode time on the edited timeline.
    pub dts_us: i64,
    pub off: u64,
    pub len: u32,
}

/// An open file: the index and a cursor over it.
pub struct Mp4 {
    input: Input,
    index: Vec<Entry>,
    next: usize,
    has_video: bool,
}

impl std::fmt::Debug for Mp4 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mp4")
            .field("samples", &self.index.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct Opened {
    pub mp4: Mp4,
    /// The video stream and its `avcC` record.
    pub video: Option<(VideoInfo, Vec<u8>)>,
    pub audio: Option<AudioInfo>,
    pub duration_ms: u64,
}

fn us(ticks: i128, scale: u32) -> i64 {
    let v = ticks * 1_000_000 / i128::from(scale.max(1));
    i64::try_from(v.clamp(-(1 << 60), 1 << 60)).unwrap_or(0)
}

/// The shift (µs) from a track's media time to the shown timeline: the leading empty
/// edits delay it, the first real edit's `media_time` is where it starts. Entries are
/// (`edit_duration` in movie ticks, `media_time` in media ticks, -1 for an empty edit).
pub fn edit_shift_us(entries: &[(u64, i64)], movie_ts: u32, media_ts: u32) -> i64 {
    let mut delay = 0i64;
    for &(dur, media_time) in entries {
        if media_time < 0 {
            delay = delay.saturating_add(us(i128::from(dur), movie_ts));
        } else {
            return delay.saturating_sub(us(i128::from(media_time), media_ts));
        }
    }
    delay
}

/// Where a seek to `target_us` starts: the last keyframe of the video (any sample when
/// there is no video) shown at or before it, or the first sample.
pub fn snap(index: &[Entry], target_us: i64, has_video: bool) -> usize {
    let mut at = 0;
    for (i, e) in index.iter().enumerate() {
        if e.key && (e.video || !has_video) && e.pts_us <= target_us {
            at = i;
        }
    }
    at
}

fn read_range(input: &mut Input, file_len: u64, r: RequiredInput) -> Result<Vec<u8>, Fail> {
    if r.position > file_len {
        return Ok(Vec::new());
    }
    let avail = file_len - r.position;
    let want = r.size.map_or(avail, |n| (n as u64).min(avail));
    if want > RANGE_MAX {
        return Err(Fail::TooLarge);
    }
    input
        .seek(SeekFrom::Start(r.position))
        .map_err(|_| Fail::BadInput)?;
    let mut buf = vec![0u8; want as usize];
    input.read_exact(&mut buf).map_err(|_| Fail::BadInput)?;
    Ok(buf)
}

/// The `moov` box, found by walking the top-level boxes.
fn read_moov(input: &mut Input, file_len: u64) -> Option<MoovBox> {
    let mut pos = 0u64;
    for _ in 0..65_536 {
        if pos + 8 > file_len {
            return None;
        }
        input.seek(SeekFrom::Start(pos)).ok()?;
        let mut h = [0u8; 16];
        let n = (file_len - pos).min(16) as usize;
        input.read_exact(&mut h[..n]).ok()?;
        let mut size = u64::from(u32::from_be_bytes(h[..4].try_into().ok()?));
        let mut hdr = 8;
        if size == 1 {
            if n < 16 {
                return None;
            }
            size = u64::from_be_bytes(h[8..16].try_into().ok()?);
            hdr = 16;
        } else if size == 0 {
            size = file_len - pos;
        }
        if size < hdr {
            return None;
        }
        if &h[4..8] == b"moov" {
            if size > RANGE_MAX || pos + size > file_len {
                return None;
            }
            let buf = read_range(
                input,
                file_len,
                RequiredInput {
                    position: pos,
                    size: Some(size as usize),
                },
            )
            .ok()?;
            return MoovBox::decode(&buf).ok().map(|(m, _)| m);
        }
        pos = pos.checked_add(size)?;
    }
    None
}

/// Per track id: the shift its edit list gives.
fn shifts(moov: &MoovBox) -> Vec<(u32, i64)> {
    let movie_ts = moov.mvhd_box.timescale.get();
    moov.trak_boxes
        .iter()
        .map(|t| {
            let media_ts = t.mdia_box.mdhd_box.timescale.get();
            let list: Vec<(u64, i64)> = t
                .edts_box
                .as_ref()
                .and_then(|e| e.elst_box.as_ref())
                .map(|e| {
                    e.entries
                        .iter()
                        .map(|x| (x.edit_duration, x.media_time))
                        .collect()
                })
                .unwrap_or_default();
            (
                t.tkhd_box.track_id,
                edit_shift_us(&list, movie_ts, media_ts),
            )
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Video,
    Audio,
    Skip,
}

struct Collect {
    roles: Vec<(u32, Role)>,
    shifts: Vec<(u32, i64)>,
    index: Vec<Entry>,
    video: Option<(VideoInfo, Vec<u8>)>,
    audio: Option<AudioInfo>,
    other_video: bool,
    fail: Option<Fail>,
}

impl Collect {
    /// Take one demuxed sample.
    fn sample(&mut self, s: &shiguredo_mp4::demux::Sample<'_>) {
        let id = s.track.track_id;
        let role = match self.roles.iter().find(|r| r.0 == id) {
            Some(r) => r.1,
            None => {
                let r = self.classify(s);
                self.roles.push((id, r));
                r
            }
        };
        if role == Role::Skip {
            return;
        }
        if self.index.len() >= MAX_SAMPLES {
            self.fail = Some(Fail::TooLarge);
            return;
        }
        let Ok(len) = u32::try_from(s.data_size) else {
            self.fail = Some(Fail::BadInput);
            return;
        };
        let ts = s.track.timescale.get();
        let shift = self.shifts.iter().find(|x| x.0 == id).map_or(0, |x| x.1);
        let dts = i128::from(s.timestamp);
        let cto = i128::from(s.composition_time_offset.unwrap_or(0));
        let dts_us = us(dts, ts).saturating_add(shift);
        let mut pts_us = us(dts + cto, ts).saturating_add(shift);
        let video = role == Role::Video;
        if !video {
            // Audio before the edit starts is dropped; a packet straddling it starts at 0.
            let end = pts_us.saturating_add(us(i128::from(s.duration), ts));
            if end <= 0 {
                return;
            }
            pts_us = pts_us.max(0);
        }
        self.index.push(Entry {
            video,
            key: !video || s.keyframe,
            pts_us,
            dts_us,
            off: s.data_offset,
            len,
        });
    }

    /// Decide what a track is from its first sample entry.
    fn classify(&mut self, s: &shiguredo_mp4::demux::Sample<'_>) -> Role {
        match (s.track.kind, s.sample_entry) {
            (TrackKind::Video, Some(SampleEntry::Avc1(a))) if self.video.is_none() => {
                let c = &a.avcc_box;
                let Some(rec) = h264::avcc_record(
                    c.avc_profile_indication,
                    c.profile_compatibility,
                    c.avc_level_indication,
                    c.length_size_minus_one.get() + 1,
                    &c.sps_list,
                    &c.pps_list,
                ) else {
                    self.fail = Some(Fail::BadInput);
                    return Role::Skip;
                };
                match h264::probe(&rec) {
                    Ok(p) => {
                        self.video = Some((
                            VideoInfo {
                                width: p.width,
                                height: p.height,
                                dar: None,
                            },
                            rec,
                        ));
                        Role::Video
                    }
                    Err(f) => {
                        self.fail = Some(f);
                        Role::Skip
                    }
                }
            }
            (TrackKind::Video, _) => {
                self.other_video = true;
                Role::Skip
            }
            (TrackKind::Audio, Some(SampleEntry::Mp4a(a))) if self.audio.is_none() => {
                let d = &a.esds_box.es.dec_config_descr;
                let asc = d.dec_specific_info.as_ref().map(|i| i.payload.as_slice());
                let rate = u32::from(a.audio.samplerate.integer);
                let ch = a.audio.channelcount;
                let aac = d.object_type_indication == 0x40;
                match (aac, asc) {
                    (true, Some(asc)) if (1..=192_000).contains(&rate) && (1..=8).contains(&ch) => {
                        let Some(codec) = aac_codec(asc, rate, ch as u8) else {
                            return Role::Skip;
                        };
                        self.audio = Some(AudioInfo {
                            codec,
                            rate,
                            channels: ch as u8,
                        });
                        Role::Audio
                    }
                    _ => Role::Skip,
                }
            }
            _ => Role::Skip,
        }
    }
}

macro_rules! run {
    ($d:expr, $input:expr, $len:expr, $c:expr) => {{
        let d = &mut $d;
        let mut rounds = 0usize;
        while let Some(r) = d.required_input() {
            rounds += 1;
            if rounds > ROUNDS_MAX {
                return Err(Fail::BadInput);
            }
            let data = read_range($input, $len, r)?;
            d.handle_input(DemuxInput {
                position: r.position,
                data: &data,
            });
        }
        loop {
            match d.tracks() {
                Ok(_) => break,
                Err(DemuxError::InputRequired(r)) => {
                    rounds += 1;
                    if rounds > ROUNDS_MAX {
                        return Err(Fail::BadInput);
                    }
                    let data = read_range($input, $len, r)?;
                    d.handle_input(DemuxInput {
                        position: r.position,
                        data: &data,
                    });
                }
                Err(_) => return Err(Fail::BadInput),
            }
        }
        loop {
            rounds += 1;
            if rounds > ROUNDS_MAX {
                return Err(Fail::BadInput);
            }
            match d.next_sample() {
                Ok(Some(s)) => {
                    $c.sample(&s);
                    if let Some(f) = $c.fail {
                        return Err(f);
                    }
                }
                Ok(None) => break,
                Err(DemuxError::InputRequired(r)) => {
                    let data = read_range($input, $len, r)?;
                    d.handle_input(DemuxInput {
                        position: r.position,
                        data: &data,
                    });
                }
                Err(_) => return Err(Fail::BadInput),
            }
        }
    }};
}

/// Open `input` as MP4. `Unsupported`: a codec we do not play; `BadInput`: broken.
pub fn open(mut input: Input) -> Result<Opened, Fail> {
    let file_len = input.seek(SeekFrom::End(0)).map_err(|_| Fail::BadInput)?;
    let mut kd = Mp4FileKindDetector::new();
    let mut rounds = 0usize;
    while let Some(r) = kd.required_input() {
        rounds += 1;
        if rounds > ROUNDS_MAX {
            return Err(Fail::BadInput);
        }
        let data = read_range(&mut input, file_len, r)?;
        kd.handle_input(DemuxInput {
            position: r.position,
            data: &data,
        });
    }
    let kind = match kd.file_kind() {
        Ok(Some(k)) => k,
        _ => return Err(Fail::BadInput),
    };
    let sh = read_moov(&mut input, file_len)
        .map(|m| shifts(&m))
        .unwrap_or_default();
    let mut c = Collect {
        roles: Vec::new(),
        shifts: sh,
        index: Vec::new(),
        video: None,
        audio: None,
        other_video: false,
        fail: None,
    };
    match kind {
        Mp4FileKind::Mp4 => {
            let mut d = Mp4FileDemuxer::new();
            run!(d, &mut input, file_len, c);
        }
        Mp4FileKind::FragmentedMp4 => {
            let mut d = Fmp4FileDemuxer::new();
            run!(d, &mut input, file_len, c);
        }
    }
    if let Some(f) = c.fail {
        return Err(f);
    }
    if c.video.is_none() && (c.other_video || c.audio.is_none()) {
        return Err(Fail::Unsupported);
    }
    if c.index.is_empty() {
        return Err(Fail::Unsupported);
    }
    let mut index = c.index;
    index.sort_by_key(|e| e.dts_us);
    let end = index.iter().map(|e| e.pts_us).max().unwrap_or(0).max(0);
    Ok(Opened {
        mp4: Mp4 {
            input,
            index,
            next: 0,
            has_video: c.video.is_some(),
        },
        video: c.video,
        audio: c.audio,
        duration_ms: u64::try_from(end / 1000).unwrap_or(0),
    })
}

impl Mp4 {
    pub fn next_packet(&mut self) -> Result<Option<Packet>, Fail> {
        let Some(e) = self.index.get(self.next).copied() else {
            return Ok(None);
        };
        self.next += 1;
        if e.len as usize > MAX_PACKET {
            return Err(Fail::BadInput);
        }
        self.input
            .seek(SeekFrom::Start(e.off))
            .map_err(|_| Fail::BadInput)?;
        let mut data = vec![0u8; e.len as usize];
        self.input
            .read_exact(&mut data)
            .map_err(|_| Fail::BadInput)?;
        Ok(Some(Packet {
            video: e.video,
            pts_us: e.pts_us,
            data,
        }))
    }

    /// The next packets start at the keyframe at or before `ms`.
    pub fn seek(&mut self, ms: u64) {
        let t = i64::try_from(ms.saturating_mul(1000)).unwrap_or(i64::MAX);
        self.next = snap(&self.index, t, self.has_video);
    }

    pub fn index(&self) -> &[Entry] {
        &self.index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(video: bool, key: bool, pts_us: i64, dts_us: i64) -> Entry {
        Entry {
            video,
            key,
            pts_us,
            dts_us,
            off: 0,
            len: 0,
        }
    }

    #[test]
    fn sniff() {
        assert!(is_mp4(b"\0\0\0\x20ftypisom"));
        assert!(!is_mp4(b"\x1a\x45\xdf\xa3\0\0\0\0"));
        assert!(!is_mp4(b"ftyp"));
    }

    #[test]
    fn edit_list_shift() {
        // x264 with B-frames: media_time is the first composition offset (2 frames of 1/30 s
        // at a 15360 timescale = 1024 ticks each).
        assert_eq!(edit_shift_us(&[(1000, 2048)], 1000, 15360), -133_333);
        // No edit list.
        assert_eq!(edit_shift_us(&[], 1000, 15360), 0);
        // A leading empty edit delays the media (500 ms), then it starts at its beginning.
        assert_eq!(
            edit_shift_us(&[(500, -1), (2000, 0)], 1000, 90_000),
            500_000
        );
        // A cut: media starts 1 s in.
        assert_eq!(edit_shift_us(&[(1000, 90_000)], 1000, 90_000), -1_000_000);
        // Absurd values do not overflow.
        let _ = edit_shift_us(&[(u64::MAX, -1), (1, i64::MAX)], 1, 1);
    }

    #[test]
    fn seek_snaps_to_the_previous_video_keyframe() {
        // Decode order: I P B B | I P ; audio interleaved.
        let idx = [
            e(true, true, 100_000, 0),
            e(false, true, 0, 10_000),
            e(true, false, 400_000, 100_000),
            e(true, false, 200_000, 200_000),
            e(false, true, 300_000, 250_000),
            e(true, true, 1_100_000, 1_000_000),
            e(true, false, 1_200_000, 1_100_000),
        ];
        assert_eq!(snap(&idx, 0, true), 0);
        assert_eq!(snap(&idx, 900_000, true), 0);
        assert_eq!(snap(&idx, 1_100_000, true), 5);
        assert_eq!(snap(&idx, 9_000_000, true), 5);
        assert_eq!(snap(&idx, -5, true), 0);
        // Audio only: any sample is a keyframe, the last one at or before.
        let a = [e(false, true, 0, 0), e(false, true, 20_000, 20_000)];
        assert_eq!(snap(&a, 25_000, false), 1);
        assert_eq!(snap(&[], 5, true), 0);
    }
}
