// SPDX-License-Identifier: AGPL-3.0-only
//! Fuzz bodies (T-16 §6): `helper_wire` (both directions of the helper protocol),
//! `mkv_demux` (the container readers) and `av1_obu` (the rav1d wrapper). Each must never
//! panic. The cargo-fuzz wrappers live next to `Cargo.toml` (`fuzz_*.rs`, not compiled by
//! the workspace); the seed corpora are under `corpus/`. A wrapper may call
//! [`crate::sandbox::enter`] once at start to run under the helper's own profile; the
//! default leaves it off because sanitizer runtimes need address space past `RLIMIT_AS`.

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
            .join("corpus")
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
    fn av1_obu_seeds() {
        assert!(corpus("av1_obu", fuzz_av1_obu) >= 2);
    }
}
