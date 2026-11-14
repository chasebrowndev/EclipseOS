// SPDX-License-Identifier: AGPL-3.0-only
//! The real helper binary under its real sandbox (T-16 §4, §6): it decodes the fixtures,
//! refuses to start without a full sandbox, and cannot open, connect or exec once inside.

use std::fs::File;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use apoth_media::client::{Helper, Mode, StartError};
use apoth_media::wire::{Fail, FromHelper, ToHelper};

fn bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_apoth-media-helper"))
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn recv(h: &mut Helper) -> FromHelper {
    assert!(h.rx.readable(10_000), "helper went quiet");
    let (tag, body) = match h.rx.recv() {
        Ok(Some(m)) => m,
        r => {
            let st = h.proc.try_status();
            panic!("helper gone ({r:?}, exit {st:?})");
        }
    };
    FromHelper::decode(tag, &body).unwrap()
}

struct Played {
    frames: Vec<(i64, u32, u32)>,
    pcm_bytes: usize,
    rate: u32,
    channels: u8,
    nonblank: bool,
}

fn play(name: &str, audio: bool) -> Played {
    let mut h = Helper::start_at(bin(), Mode::Media, &[], 64 << 20).expect("helper starts");
    let f = File::open(fixture(name)).unwrap();
    let open = ToHelper::Open {
        audio,
        box_w: 160,
        box_h: 90,
        max_w: 3840,
        max_h: 2160,
    };
    h.tx.send(&open.encode(), Some(f.as_fd())).unwrap();
    let FromHelper::Info {
        width,
        height,
        rate,
        channels,
        ..
    } = recv(&mut h)
    else {
        panic!("no info");
    };
    const SLOTS: u8 = 4;
    let slot = width as usize * height as usize * 4;
    let ring: OwnedFd = rustix::fs::memfd_create("ring", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
    rustix::fs::ftruncate(&ring, (slot * usize::from(SLOTS)) as u64).unwrap();
    if width > 0 {
        let r = ToHelper::Ring {
            slots: SLOTS,
            slot_bytes: slot as u32,
        };
        h.tx.send(&r.encode(), Some(ring.as_fd())).unwrap();
    }
    h.tx.send(&ToHelper::Want { bytes: 1 << 20 }.encode(), None)
        .unwrap();
    let mut out = Played {
        frames: Vec::new(),
        pcm_bytes: 0,
        rate,
        channels,
        nonblank: false,
    };
    let mut px = vec![0u8; slot];
    loop {
        match recv(&mut h) {
            FromHelper::Frame {
                slot: s,
                width: w,
                height: hh,
                len,
                pts_us,
                ..
            } => {
                assert_eq!((w, hh, len as usize), (width, height, slot));
                rustix::io::pread(&ring, &mut px, u64::from(s) * slot as u64).unwrap();
                out.nonblank |= px.chunks(4).any(|p| p[0] > 40 || p[1] > 40 || p[2] > 40);
                out.frames.push((pts_us, w, hh));
                h.tx.send(&ToHelper::Release { slot: s }.encode(), None)
                    .unwrap();
            }
            FromHelper::Pcm { data, .. } => {
                out.pcm_bytes += data.len();
                let more = ToHelper::Want {
                    bytes: data.len() as u32,
                };
                h.tx.send(&more.encode(), None).unwrap();
            }
            FromHelper::End => break,
            other => panic!("unexpected {other:?}"),
        }
    }
    h.tx.send(&ToHelper::Quit.encode(), None).unwrap();
    assert_eq!(h.proc.finish(5_000).unwrap(), Some(0));
    out
}

#[test]
fn plays_av1_opus_webm() {
    let p = play("av1-opus-160x90.webm", true);
    // 2 s at 30 fps, fitted into the 160x90 box.
    assert!(p.frames.len() >= 58, "{} frames", p.frames.len());
    assert_eq!((p.frames[0].1, p.frames[0].2), (160, 90));
    assert!(p.frames.windows(2).all(|w| w[0].0 < w[1].0), "pts in order");
    assert!(p.nonblank);
    assert_eq!((p.rate, p.channels), (48_000, 2));
    // About 2 s of 48 kHz stereo s16 (Opus pre-skip and padding aside).
    let want = 2 * 48_000 * 2 * 2;
    assert!(
        p.pcm_bytes > want * 9 / 10 && p.pcm_bytes < want * 11 / 10,
        "{}",
        p.pcm_bytes
    );
}

#[test]
fn audio_off_sends_no_pcm() {
    let p = play("av1-opus-160x90.webm", false);
    assert_eq!((p.rate, p.pcm_bytes), (0, 0));
    assert!(p.frames.len() >= 58);
}

#[test]
fn plays_10bit_av1() {
    let p = play("av1-10bit-64x36.mkv", false);
    assert!(p.frames.len() >= 28, "{} frames", p.frames.len());
    // No upscale: the 64x36 source stays 64x36.
    assert_eq!((p.frames[0].1, p.frames[0].2), (64, 36));
    assert!(p.nonblank);
}

#[test]
fn plays_audio_only_flac() {
    let p = play("sine-1s.flac", true);
    assert!(p.frames.is_empty());
    assert_eq!(p.rate, 48_000);
    let want = 48_000 * usize::from(p.channels) * 2;
    assert!(p.pcm_bytes >= want * 9 / 10, "{}", p.pcm_bytes);
}

#[test]
fn garbage_input_fails_cleanly() {
    let mut h = Helper::start_at(bin(), Mode::Media, &[], 1 << 20).unwrap();
    let junk: OwnedFd = rustix::fs::memfd_create("j", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
    rustix::io::write(&junk, &[0x1a, 0x45, 0xdf, 0xa3, 0xff, 0xff, 0x00, 0x13]).unwrap();
    rustix::fs::seek(&junk, rustix::fs::SeekFrom::Start(0)).unwrap();
    let open = ToHelper::Open {
        audio: true,
        box_w: 64,
        box_h: 64,
        max_w: 3840,
        max_h: 2160,
    };
    h.tx.send(&open.encode(), Some(junk.as_fd())).unwrap();
    assert!(matches!(recv(&mut h), FromHelper::Failed(_)));
    assert_eq!(
        h.proc.finish(5_000).unwrap(),
        Some(apoth_media::helper::EXIT_FAILED)
    );
}

#[test]
fn oversized_source_is_refused() {
    let mut h = Helper::start_at(bin(), Mode::Media, &[], 1 << 20).unwrap();
    let f = File::open(fixture("av1-opus-160x90.webm")).unwrap();
    let open = ToHelper::Open {
        audio: false,
        box_w: 64,
        box_h: 64,
        max_w: 100,
        max_h: 100,
    };
    h.tx.send(&open.encode(), Some(f.as_fd())).unwrap();
    assert_eq!(recv(&mut h), FromHelper::Failed(Fail::TooLarge));
}

#[test]
fn partial_landlock_is_sandbox_unavailable() {
    let r = Helper::start_at(bin(), Mode::Media, &["--test-partial-landlock"], 1 << 20);
    assert_eq!(r.err(), Some(StartError::Failed(Fail::SandboxUnavailable)));
}

#[test]
fn inside_the_sandbox_nothing_opens() {
    let (_ours, theirs) = UnixStream::pair().unwrap();
    let st = Command::new(bin())
        .args(["media", "--test-probe-sandbox"])
        .env_clear()
        .stdin(Stdio::from(OwnedFd::from(theirs)))
        .status()
        .unwrap();
    assert_eq!(st.code(), Some(apoth_media::helper::EXIT_OK), "{st:?}");
}

#[test]
fn image_mode_decodes_png_and_rejects_junk() {
    let mut h = Helper::start_at(bin(), Mode::Image, &[], 64 << 20).unwrap();
    const PNG: &[u8] = include_bytes!("fixtures/red-1x1.png");
    for (data, ok) in [(PNG.to_vec(), true), (b"not an image".to_vec(), false)] {
        let req = ToHelper::Image {
            kind: apoth_media::wire::ImageKind::Any,
            max_axis: 10_000,
            max_decoded: 1 << 26,
            anim_quota: 1 << 26,
            data,
        };
        h.tx.send(&req.encode(), None).unwrap();
        match recv(&mut h) {
            FromHelper::ImageOk(f) => {
                assert!(ok);
                assert_eq!((f[0].width, f[0].height), (1, 1));
                assert_eq!(&f[0].rgba[..3], &[255, 0, 0]);
            }
            FromHelper::ImageErr(_) => assert!(!ok),
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn sandboxed_pixel_decoder_matches_in_process_and_fails_closed() {
    use apoth_image::Limits;
    use apoth_image::decoder::{InProcess, PixelDecoder};
    use apoth_media::image::Sandboxed;
    const PNG: &[u8] = include_bytes!("fixtures/red-1x1.png");
    let l = Limits::default();
    let mut sb = Sandboxed::with_helper(bin().to_path_buf(), Vec::new());
    let a = sb.png(PNG, &l).unwrap();
    let b = InProcess.png(PNG, &l).unwrap();
    assert_eq!((a.width, a.height, &a.rgba), (b.width, b.height, &b.rgba));
    assert!(sb.any(b"junk", &l, 0).is_err());
    // The same helper keeps serving after an error.
    assert!(sb.any(PNG, &l, 0).is_ok());
    let mut broken =
        Sandboxed::with_helper(bin().to_path_buf(), vec!["--test-partial-landlock".into()]);
    assert!(broken.png(PNG, &l).is_err());
}
