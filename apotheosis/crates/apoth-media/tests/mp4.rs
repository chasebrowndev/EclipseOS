// SPDX-License-Identifier: AGPL-3.0-only
//! MP4 / H.264 (T-16 §3, ADR 0118): the demuxer and the software decoder against ffmpeg.
//! Each fixture is a tiny clip (`tests/fixtures/h264-*.mp4`, under 50 KB) whose decoded
//! frames, in display order, have the md5 ffmpeg gives
//! (`ffmpeg -i x.mp4 -map 0:v -f rawvideo -pix_fmt <native> - | md5sum`).

use std::fs::File;
use std::path::{Path, PathBuf};

use apoth_media::av1::PicView;
use apoth_media::demux::{AudioCodec, Source, VideoCodec};
use apoth_media::h264;
use apoth_media::wire::Fail;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn open(name: &str) -> Result<Source, Fail> {
    Source::open(Box::new(File::open(fixture(name)).unwrap()))
}

/// MD5 (RFC 1321), for comparing against ffmpeg's `md5sum` output.
fn md5(data: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64)
        .map(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32)
        .collect();
    let (mut a0, mut b0, mut c0, mut d0) = (
        0x6745_2301u32,
        0xefcd_ab89u32,
        0x98ba_dcfeu32,
        0x1032_5476u32,
    );
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_le_bytes());
    for chunk in msg.chunks_exact(64) {
        let m: Vec<u32> = chunk
            .chunks_exact(4)
            .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
            .collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    [a0, b0, c0, d0]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn md5_is_md5() {
    assert_eq!(md5(b""), "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(md5(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
}

struct Decoded {
    /// Planar frames, display order, native layout (what `-f rawvideo` writes).
    raw: Vec<u8>,
    pts: Vec<i64>,
    size: (u32, u32),
    audio_packets: usize,
    packets: Vec<(bool, i64)>,
}

fn append(raw: &mut Vec<u8>, p: &PicView<'_>) {
    let bps = if p.bpc > 8 { 2 } else { 1 };
    let (cw, ch) = p.chroma();
    for (i, (w, h)) in [(p.width as usize, p.height as usize), (cw, ch), (cw, ch)]
        .into_iter()
        .enumerate()
    {
        for y in 0..h {
            raw.extend_from_slice(&p.planes[i][y * p.strides[i]..y * p.strides[i] + w * bps]);
        }
    }
}

/// Everything a file plays: the packets in order, the video through the decoder.
fn decode(src: &mut Source) -> Decoded {
    let Some(VideoCodec::H264(avcc)) = src.vcodec.clone() else {
        panic!("not H.264");
    };
    let mut dec = h264::Decoder::new(&avcc).expect("decoder");
    let mut d = Decoded {
        raw: Vec::new(),
        pts: Vec::new(),
        size: (0, 0),
        audio_packets: 0,
        packets: Vec::new(),
    };
    let mut f = |p: &PicView<'_>| {
        d.size = (p.width, p.height);
        d.pts.push(p.pts);
        append(&mut d.raw, p);
    };
    let mut packets = Vec::new();
    let mut audio = 0;
    while let Some(p) = src.next_packet().expect("packet") {
        packets.push((p.video, p.pts_us));
        if p.video {
            let _ = dec.decode(&p.data, p.pts_us, &mut f);
        } else {
            audio += 1;
        }
    }
    dec.finish(&mut f).unwrap();
    d.audio_packets = audio;
    d.packets = packets;
    d
}

/// (fixture, frames, md5 of the display-order planar output ffmpeg gives).
const CLIPS: &[(&str, usize, &str)] = &[
    ("h264-baseline.mp4", 30, "8ead4d81c5760302023071c2df9211f4"),
    (
        "h264-main-bframes.mp4",
        30,
        "59acf3ad63fff720c2ee88e25f523991",
    ),
    ("h264-high.mp4", 30, "cf572a67e965cbd14a05035f74bf869d"),
    (
        "h264-high-interlaced.mp4",
        30,
        "f6e9ef689beb48c8af13df39877f3998",
    ),
    // Fragmented MP4 of the same stream as h264-high.
    ("h264-frag.mp4", 30, "cf572a67e965cbd14a05035f74bf869d"),
    // `-ss 0.4 -c copy`: an edit list cuts the first 12 frames and the B-frame delay.
    ("h264-elst.mp4", 18, "47de5316dae9460d94e9cff040d0a801"),
    ("h264-aac.mp4", 30, "1166c5ede8c4418dc6fccd80a8e6431e"),
    // Not a multiple of 16 (62x38): cropped from 64x48.
    ("h264-62x38.mp4", 30, "e8a7d25b8d9f9b106a910c2218fea7b3"),
    // High 10 (yuv420p10le, two bytes a sample) and High 4:2:2 (yuv422p).
    ("h264-high10.mp4", 30, "669fa35ae716aac2f9584ae047b20af8"),
    ("h264-high422.mp4", 30, "4d4724111a436d3d7e584492b9dd7ff9"),
];

#[test]
fn decoded_frames_match_ffmpeg() {
    for &(name, frames, want) in CLIPS {
        let mut src = open(name).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let d = decode(&mut src);
        assert_eq!(d.pts.len(), frames, "{name}: frame count");
        assert!(
            d.pts.windows(2).all(|w| w[0] < w[1]),
            "{name}: pts strictly increasing {:?}",
            d.pts
        );
        assert_eq!(md5(&d.raw), want, "{name}: frames differ from ffmpeg's");
    }
}

#[test]
fn monochrome_with_odd_dimensions() {
    // 63x37 gray: ffmpeg's `-pix_fmt gray` md5 covers luma only.
    let mut src = open("h264-gray-63x37.mp4").unwrap();
    let v = src.video.unwrap();
    assert_eq!((v.width, v.height), (63, 37));
    let Some(VideoCodec::H264(avcc)) = src.vcodec.clone() else {
        panic!()
    };
    let mut dec = h264::Decoder::new(&avcc).unwrap();
    let mut luma = Vec::new();
    let mut n = 0;
    let mut f = |p: &PicView<'_>| {
        assert_eq!((p.width, p.height), (63, 37));
        let (cw, ch) = p.chroma();
        assert_eq!((cw, ch), (32, 19));
        // Chroma is mid-grey, padded to the size the converter expects.
        for plane in 1..3 {
            assert!(
                p.strides[plane] >= cw && p.planes[plane].len() >= (ch - 1) * p.strides[plane] + cw
            );
            assert!(p.planes[plane].iter().all(|&b| b == 128));
        }
        for y in 0..37 {
            luma.extend_from_slice(&p.planes[0][y * p.strides[0]..y * p.strides[0] + 63]);
        }
        n += 1;
    };
    while let Some(p) = src.next_packet().unwrap() {
        let _ = dec.decode(&p.data, p.pts_us, &mut f);
    }
    dec.finish(&mut f).unwrap();
    assert_eq!(n, 30);
    assert_eq!(md5(&luma), "e5107259d97f55deaa7584aeadccad9f");
}

#[test]
fn streams_and_timeline() {
    let src = open("h264-main-bframes.mp4").unwrap();
    let v = src.video.unwrap();
    assert_eq!((v.width, v.height, v.dar), (64, 64, None));
    assert!(src.audio.is_none());
    assert!(
        (900..=1100).contains(&src.duration_ms),
        "{}",
        src.duration_ms
    );
    // The edit list is applied: the first shown frame is at 0, not at the B-frame delay.
    let d = decode(&mut open("h264-main-bframes.mp4").unwrap());
    assert_eq!(d.pts[0], 0);
    assert!((d.pts[1] - 33_333).abs() <= 1, "{}", d.pts[1]);

    let mut a = open("h264-aac.mp4").unwrap();
    let info = a.audio.clone().expect("AAC audio");
    assert_eq!((info.rate, info.channels), (22_050, 1));
    assert!(matches!(info.codec, AudioCodec::Symphonia(_)));
    let d = decode(&mut a);
    assert!(d.audio_packets > 20, "{}", d.audio_packets);
    // Audio and video interleave by decode time, so neither starves the other.
    let first_audio = d.packets.iter().position(|p| !p.0).unwrap();
    let last_video = d.packets.iter().rposition(|p| p.0).unwrap();
    assert!(
        first_audio < 6 && last_video > d.packets.len() - 12,
        "{first_audio} {last_video}"
    );
}

#[test]
fn elst_cuts_the_start() {
    let d = decode(&mut open("h264-elst.mp4").unwrap());
    assert_eq!(d.pts.len(), 18);
    assert_eq!(d.pts[0], 0);
    assert!(d.pts[17] < 700_000, "{:?}", d.pts[17]);
}

/// Frames a seek to `ms` plays: decode from the packets it hands out.
fn after_seek(name: &str, ms: u64) -> (Vec<i64>, Vec<u8>) {
    let mut src = open(name).unwrap();
    let Some(VideoCodec::H264(avcc)) = src.vcodec.clone() else {
        panic!()
    };
    let mut dec = h264::Decoder::new(&avcc).unwrap();
    // Play a little, then seek (the decoder is flushed as the helper does).
    for _ in 0..5 {
        let p = src.next_packet().unwrap().unwrap();
        let _ = dec.decode(&p.data, p.pts_us, &mut |_| {});
    }
    src.seek(ms).unwrap();
    dec.flush();
    let mut out = (Vec::new(), Vec::new());
    let mut f = |p: &PicView<'_>| {
        out.0.push(p.pts);
        append(&mut out.1, p);
    };
    while let Some(p) = src.next_packet().unwrap() {
        if p.video {
            let _ = dec.decode(&p.data, p.pts_us, &mut f);
        }
    }
    dec.finish(&mut f).unwrap();
    out
}

#[test]
fn seek_snaps_to_the_previous_keyframe() {
    // h264-high has keyframes at frames 0 and 15 (0 and 500 ms).
    let full = decode(&mut open("h264-high.mp4").unwrap());
    let frame = full.raw.len() / full.pts.len();
    for (ms, first_pts) in [
        (0, 0),
        (499, 0),
        (700, 500_000),
        (990, 500_000),
        (60_000, 500_000),
    ] {
        let (pts, raw) = after_seek("h264-high.mp4", ms);
        assert_eq!(pts[0], first_pts, "seek {ms}");
        let from = full.pts.iter().position(|&p| p == first_pts).unwrap();
        assert_eq!(
            pts,
            full.pts[from..],
            "seek {ms}: same frames as straight playback"
        );
        assert_eq!(raw, full.raw[from * frame..], "seek {ms}: same pixels");
    }
    // Fragmented MP4 has no demuxer seek of its own: ours works the same.
    // (no edit list there: its timeline starts at the B-frame delay).
    let full = decode(&mut open("h264-frag.mp4").unwrap());
    let (pts, _) = after_seek("h264-frag.mp4", 700);
    assert_eq!(pts, full.pts[15..]);
}

#[test]
fn unsupported_streams_are_unsupported_not_a_crash() {
    for name in [
        "h264-high444.mp4",
        "h264-lossless.mp4",
        "hevc.mp4",
        "vp9.mp4",
        "av1.mp4",
    ] {
        assert_eq!(open(name).err(), Some(Fail::Unsupported), "{name}");
    }
}

#[test]
fn damaged_files_never_panic() {
    let good = std::fs::read(fixture("h264-high.mp4")).unwrap();
    // Every truncation.
    for n in (0..good.len()).step_by(37) {
        run(good[..n].to_vec());
    }
    // Flipped bytes, spread over the file.
    for i in (0..good.len()).step_by(61) {
        let mut b = good.clone();
        b[i] ^= 0xff;
        run(b);
    }
    // Hostile size fields.
    let mut b = good.clone();
    b[0..4].copy_from_slice(&u32::MAX.to_be_bytes());
    run(b);
    run(b"\0\0\0\x08ftyp".to_vec());
    run(vec![
        0, 0, 0, 0x14, b'f', b't', b'y', b'p', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ]);
}

fn run(bytes: Vec<u8>) {
    apoth_media::fuzz::fuzz_mp4_demux(&bytes);
}

/// Seeds with bytes flipped, set and cut, through the decoder and the whole MP4 path.
#[test]
fn mutated_seeds_never_panic() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/corpus");
    let mut rng = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    for (dir, f) in [
        ("h264", apoth_media::fuzz::fuzz_h264 as fn(&[u8])),
        ("mp4_demux", apoth_media::fuzz::fuzz_mp4_demux as fn(&[u8])),
    ] {
        for e in std::fs::read_dir(root.join(dir)).unwrap() {
            let seed = std::fs::read(e.unwrap().path()).unwrap();
            for _ in 0..60 {
                let mut b = seed.clone();
                for _ in 0..1 + next() % 12 {
                    let i = next() as usize % b.len();
                    match next() % 3 {
                        0 => b[i] ^= 1 << (next() % 8),
                        1 => b[i] = next() as u8,
                        _ => b.truncate(i.max(1)),
                    }
                }
                f(&b);
            }
        }
    }
}

/// Regenerates `fuzz/corpus/h264/` from the fixtures: `cargo test -- --ignored h264_seeds`.
#[test]
#[ignore]
fn h264_seeds() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/corpus/h264");
    for (name, out) in [
        ("h264-baseline.mp4", "baseline"),
        ("h264-high.mp4", "high-bframes"),
        ("h264-high10.mp4", "high10"),
        ("h264-62x38.mp4", "crop-62x38"),
    ] {
        let mut src = open(name).unwrap();
        let Some(VideoCodec::H264(avcc)) = src.vcodec.clone() else {
            panic!()
        };
        let mut v = Vec::new();
        v.extend_from_slice(&u16::try_from(avcc.len()).unwrap().to_be_bytes());
        v.extend_from_slice(&avcc);
        while let Some(p) = src.next_packet().unwrap() {
            v.extend_from_slice(&u16::try_from(p.data.len()).unwrap().to_be_bytes());
            v.extend_from_slice(&p.data);
        }
        std::fs::write(dir.join(out), v).unwrap();
    }
}
