// SPDX-License-Identifier: AGPL-3.0-only
//! Fuzz bodies (T-16 §6): `helper_wire` (both directions of the helper protocol),
//! `mkv_demux` (the container readers), `mp4_demux` (MP4 and fragmented MP4), `av1_obu`
//! (the rav1d wrapper) and `h264` (the `rust_h264` wrapper). Each must never
//! panic. The cargo-fuzz wrappers are in `apotheosis/fuzz/fuzz_targets/`, the seed corpora
//! in `apotheosis/fuzz/corpus/`. A wrapper may call [`crate::sandbox::enter`] once at start
//! to run under the helper's own profile; the default leaves it off because sanitizer
//! runtimes need address space past `RLIMIT_AS`.

use std::io::Cursor;

use crate::av1::Decoder;
use crate::demux::Source;
use crate::wire::{self, FromHelper, ToHelper};

/// Frames of either direction, back to back: whatever decodes re-encodes identically.
pub fn fuzz_helper_wire(data: &[u8]) {
    let mut rest = data;
    while let Ok((tag, body, n)) = wire::split(rest) {
        if let Ok(m) = ToHelper::decode(tag, body) {
            let e = m.encode();
            let (t2, b2, _) = wire::split(&e).expect("own encoding splits");
            assert_eq!(
                ToHelper::decode(t2, b2).as_ref(),
                Ok(&m),
                "to-helper round trip"
            );
        }
        if let Ok(m) = FromHelper::decode(tag, body) {
            let e = m.encode();
            let (t2, b2, _) = wire::split(&e).expect("own encoding splits");
            assert_eq!(
                FromHelper::decode(t2, b2).as_ref(),
                Ok(&m),
                "from-helper round trip"
            );
        }
        rest = &rest[n..];
    }
}

/// The container readers over arbitrary bytes: open, read some packets, seek, read more.
pub fn fuzz_mkv_demux(data: &[u8]) {
    let Ok(mut src) = Source::open(Box::new(Cursor::new(data.to_vec()))) else {
        return;
    };
    for _ in 0..256 {
        match src.next_packet() {
            Ok(Some(p)) => assert!(p.data.len() <= crate::demux::MAX_PACKET),
            _ => break,
        }
    }
    let _ = src.seek(u64::from(data.first().copied().unwrap_or(0)) * 10);
    for _ in 0..16 {
        if !matches!(src.next_packet(), Ok(Some(_))) {
            break;
        }
    }
}

/// The MP4 reader over arbitrary bytes, forced down the MP4 path by an `ftyp` prefix when
/// the input lacks one: open, read packets, seek to several places, read more. Every
/// packet is within the limits; when the video is H.264 its packets go to the decoder.
pub fn fuzz_mp4_demux(data: &[u8]) {
    let mut bytes = data.to_vec();
    if !crate::mp4::is_mp4(&bytes) {
        let mut v = b"\0\0\0\x14ftypisom\0\0\0\0isom".to_vec();
        v.extend_from_slice(data);
        bytes = v;
    }
    let Ok(mut src) = Source::open(Box::new(Cursor::new(bytes))) else {
        return;
    };
    let mut dec = match &src.vcodec {
        Some(crate::demux::VideoCodec::H264(avcc)) => crate::h264::Decoder::new(avcc).ok(),
        _ => None,
    };
    let mut seen = 0usize;
    let mut f = |p: &crate::av1::PicView<'_>| {
        assert!(p.width > 0 && p.height > 0);
        seen += 1;
    };
    for round in 0..3u64 {
        for _ in 0..64 {
            match src.next_packet() {
                Ok(Some(p)) => {
                    assert!(p.data.len() <= crate::demux::MAX_PACKET);
                    if let (true, Some(d)) = (p.video, dec.as_mut()) {
                        let _ = d.decode(&p.data, p.pts_us, &mut f);
                    }
                }
                _ => break,
            }
        }
        let ms = u64::from(data.first().copied().unwrap_or(0)) * 10 * round;
        let _ = src.seek(ms);
        if let Some(d) = dec.as_mut() {
            d.flush();
        }
    }
}

/// The H.264 decoder: `[u16 len][avcC record][u16 len][sample]...`, big-endian lengths,
/// the samples length-prefixed NAL units as in MP4.
pub fn fuzz_h264(data: &[u8]) {
    fn take<'a>(rest: &mut &'a [u8]) -> Option<&'a [u8]> {
        let n = usize::from(u16::from_be_bytes([*rest.first()?, *rest.get(1)?]));
        let body = rest.get(2..2 + n)?;
        *rest = &rest[2 + n..];
        Some(body)
    }
    let mut rest = data;
    let Some(avcc) = take(&mut rest) else {
        return;
    };
    let Ok(mut d) = crate::h264::Decoder::new(avcc) else {
        return;
    };
    let mut f = |p: &crate::av1::PicView<'_>| assert!(p.width > 0 && p.height > 0);
    let mut i = 0;
    while let Some(sample) = take(&mut rest) {
        let _ = d.decode(sample, i, &mut f);
        i += 1;
    }
    let _ = d.finish(&mut f);
    d.flush();
}

/// The AV1 decoder over arbitrary OBUs: byte 0 splits the rest into packets.
pub fn fuzz_av1_obu(data: &[u8]) {
    let [cut, rest @ ..] = data else {
        return;
    };
    let Ok(mut d) = Decoder::new(1) else {
        return;
    };
    let mut seen = 0usize;
    let mut f = |p: &crate::av1::PicView<'_>| {
        assert!(p.width > 0 && p.height > 0);
        seen += 1;
    };
    for (i, pkt) in rest.chunks(usize::from(*cut).max(1) * 16).enumerate() {
        let _ = d.decode(pkt, i as i64, &mut f);
    }
    let _ = d.finish(&mut f);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus(name: &str, f: fn(&[u8])) -> usize {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fuzz/corpus")
            .join(name);
        let mut n = 0;
        for e in std::fs::read_dir(dir).unwrap() {
            f(&std::fs::read(e.unwrap().path()).unwrap());
            n += 1;
        }
        n
    }

    #[test]
    fn helper_wire_seeds() {
        assert!(corpus("helper_wire", fuzz_helper_wire) >= 3);
    }

    #[test]
    fn mkv_demux_seeds() {
        assert!(corpus("mkv_demux", fuzz_mkv_demux) >= 3);
    }

    #[test]
    fn mp4_demux_seeds() {
        assert!(corpus("mp4_demux", fuzz_mp4_demux) >= 5);
    }

    #[test]
    fn h264_seeds() {
        assert!(corpus("h264", fuzz_h264) >= 3);
    }

    #[test]
    fn av1_obu_seeds() {
        assert!(corpus("av1_obu", fuzz_av1_obu) >= 2);
    }
}
