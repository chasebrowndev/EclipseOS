// SPDX-License-Identifier: AGPL-3.0-only
//! `apoth-media-helper` (T-16 §4): one process per playing placement (media mode) or per
//! decode worker (image mode, T-15 §5 `graphics.sandbox-decode`). Its stdin is the socket
//! to the terminal; it has no other fd it did not receive.
//!
//! Order (§4.1): tier B has no initialisation step, so the helper (2) puts the sandbox in
//! force first and (3) only then accepts the input fd. A sandbox failure is reported as
//! `sandbox-unavailable` and the helper exits before reading any untrusted byte.
//!
//! Media mode flow: `Open` (+ input fd) → `Info` → `Ring` (+ memfd, when there is video)
//! → packets: video frames go into free ring slots (pwrite) announced by `Frame`; PCM goes
//! out in `Pcm` messages within the audio credit the terminal grants with `Want`. Slots
//! come back with `Release`. `Seek` restarts from a keyframe; `Quit` or a closed socket ends
//! the process.

use std::collections::VecDeque;
use std::fs::File;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;

use apoth_image::{ErrCode, Limits};

use crate::audio_dec::AudioDec;
use crate::av1::{Decoder, PicView};
use crate::chan::Chan;
use crate::demux::{Packet, Source};
use crate::sandbox::{self, Knobs};
use crate::wire::{self, Fail, FromHelper, ImageFrame, ImageKind, ToHelper};
use crate::yuv::{Converter, fit};

/// Exit statuses.
pub const EXIT_OK: i32 = 0;
pub const EXIT_SANDBOX: i32 = 3;
pub const EXIT_PROTOCOL: i32 = 4;
pub const EXIT_FAILED: i32 = 5;

/// The `apoth_image::ErrCode` alphabet as wire indices.
pub const ERR_CODES: [ErrCode; 10] = [
    ErrCode::Einval,
    ErrCode::Enoent,
    ErrCode::EnoentFile,
    ErrCode::Enospc,
    ErrCode::Efbig,
    ErrCode::Ebadpng,
    ErrCode::Enodata,
    ErrCode::Etoodeep,
    ErrCode::Ecycle,
    ErrCode::Enoparent,
];

pub fn err_index(e: ErrCode) -> u8 {
    ERR_CODES.iter().position(|c| *c == e).unwrap_or(0) as u8
}

/// Entry point of the binary: `apoth-media-helper media|image [--test-partial-landlock]`.
pub fn main(args: &[String]) -> i32 {
    let image = match args.first().map(String::as_str) {
        Some("media") => false,
        Some("image") => true,
        _ => return EXIT_PROTOCOL,
    };
    let knobs = Knobs {
        simulate_partial_landlock: args.iter().any(|a| a == "--test-partial-landlock"),
    };
    // stdin is the socket.
    let Ok(fd) = std::io::stdin().lock().as_fd_owned() else {
        return EXIT_PROTOCOL;
    };
    let chan = Chan::new(UnixStream::from(fd), wire::MAX_MESSAGE);
    // Decoder threads, decided while /sys and /proc are still readable.
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get().min(4)) as u32;
    let limit = if image {
        sandbox::AS_IMAGE
    } else {
        sandbox::AS_MEDIA
    };
    if sandbox::enter(limit, knobs).is_err() {
        let _ = chan.send(&FromHelper::Failed(Fail::SandboxUnavailable).encode(), None);
        return EXIT_SANDBOX;
    }
    if args.iter().any(|a| a == "--test-probe-sandbox") {
        // Tests: everything the sandbox forbids must fail from here on.
        return probe_sandbox();
    }
    if chan.send(&FromHelper::Ready.encode(), None).is_err() {
        return EXIT_PROTOCOL;
    }
    if image {
        image_loop(chan)
    } else {
        // Tests: play a hostile helper (it can only hurt itself; the terminal must kill it).
        let forge = args
            .iter()
            .find_map(|a| a.strip_prefix("--test-forge="))
            .map(str::to_owned);
        match Media::run(chan, threads, forge.as_deref()) {
            Ok(()) => EXIT_OK,
            Err(code) => code,
        }
    }
}

/// Exit code of the probe when something the sandbox forbids worked.
pub const EXIT_PROBE_ESCAPED: i32 = 6;

/// Try what the sandbox forbids: open a path, list a directory, make a socket, exec.
fn probe_sandbox() -> i32 {
    let escaped = std::fs::File::open("/proc/self/status").is_ok()
        || std::fs::read_dir("/").is_ok()
        || std::fs::File::create("/tmp/apoth-media-probe").is_ok()
        || std::net::UdpSocket::bind("127.0.0.1:0").is_ok()
        || std::os::unix::net::UnixStream::connect("/run/user").is_ok()
        || std::process::Command::new("/bin/true").status().is_ok();
    if escaped { EXIT_PROBE_ESCAPED } else { EXIT_OK }
}

/// `stdin` as an owned fd (dup, so std's handle stays valid).
trait OwnedStdin {
    fn as_fd_owned(&self) -> std::io::Result<OwnedFd>;
}

impl OwnedStdin for std::io::StdinLock<'_> {
    fn as_fd_owned(&self) -> std::io::Result<OwnedFd> {
        use std::os::fd::AsFd;
        self.as_fd().try_clone_to_owned()
    }
}

fn image_loop(mut chan: Chan) -> i32 {
    loop {
        let msg = match chan.recv() {
            Ok(Some((tag, body))) => ToHelper::decode(tag, &body),
            Ok(None) => return EXIT_OK,
            Err(_) => return EXIT_PROTOCOL,
        };
        let reply = match msg {
            Ok(ToHelper::Image {
                kind,
                max_axis,
                max_decoded,
                anim_quota,
                data,
            }) => decode_image(kind, max_axis, max_decoded, anim_quota, &data),
            Ok(ToHelper::Quit) => return EXIT_OK,
            _ => return EXIT_PROTOCOL,
        };
        if chan.send(&reply.encode(), None).is_err() {
            return EXIT_PROTOCOL;
        }
    }
}

/// The decode an image request asks for, under the caps it names.
pub fn decode_image(
    kind: ImageKind,
    max_axis: u32,
    max_decoded: u64,
    anim_quota: u64,
    data: &[u8],
) -> FromHelper {
    let limits = Limits {
        max_axis,
        max_decoded_bytes: max_decoded,
        animation_quota: anim_quota,
        ..Limits::default()
    };
    let frames = match kind {
        ImageKind::Png => apoth_image::kitty::decode_png(data, &limits).map(|i| vec![(i, 0)]),
        ImageKind::Any => {
            apoth_image::formats::decode_any(data, &limits, anim_quota).map(|d| d.frames)
        }
    };
    match frames {
        Ok(f) => FromHelper::ImageOk(
            f.into_iter()
                .map(|(i, gap_ms)| ImageFrame {
                    width: i.width,
                    height: i.height,
                    gap_ms,
                    rgba: i.rgba,
                })
                .collect(),
        ),
        Err(e) => FromHelper::ImageErr(err_index(e)),
    }
}

struct Ring {
    fd: OwnedFd,
    slot_bytes: usize,
    slots: u8,
    free: Vec<u8>,
}

struct Media {
    chan: Chan,
    src: Source,
    video: Option<(Decoder, u32, u32)>,
    audio: Option<AudioDec>,
    ring: Option<Ring>,
    conv: Converter,
    /// Converted frames waiting for a free slot: (pts, pixels).
    ready: VecDeque<(i64, Vec<u8>)>,
    spare: Vec<Vec<u8>>,
    credit: u64,
    pcm: Vec<i16>,
    /// Bytes per PCM frame.
    audio_frame: usize,
    ended: bool,
}

/// Converted frames the helper holds beyond the ring before it stops demuxing.
const READY_MAX: usize = 12;

/// What a message changed.
enum Step {
    Go,
    Quit,
}

impl Media {
    fn run(mut chan: Chan, threads: u32, forge: Option<&str>) -> Result<(), i32> {
        let fail = |chan: &Chan, f: Fail, code: i32| {
            let _ = chan.send(&FromHelper::Failed(f).encode(), None);
            code
        };
        let (audio_on, bw, bh, max_w, max_h) = match chan.recv() {
            Ok(Some((tag, body))) => match ToHelper::decode(tag, &body) {
                Ok(ToHelper::Open {
                    audio,
                    box_w,
                    box_h,
                    max_w,
                    max_h,
                }) => (audio, box_w, box_h, max_w, max_h),
                _ => return Err(fail(&chan, Fail::Protocol, EXIT_PROTOCOL)),
            },
            Ok(None) => return Ok(()),
            Err(_) => return Err(fail(&chan, Fail::Protocol, EXIT_PROTOCOL)),
        };
        let Some(input) = chan.take_fd() else {
            return Err(fail(&chan, Fail::Protocol, EXIT_PROTOCOL));
        };
        // Step 3: untrusted bytes from here on.
        let src = match Source::open(Box::new(File::from(input))) {
            Ok(s) => s,
            Err(f) => return Err(fail(&chan, f, EXIT_FAILED)),
        };
        let mut video = None;
        let (mut ow, mut oh) = (0, 0);
        if let Some(v) = src.video {
            if v.width > max_w || v.height > max_h {
                return Err(fail(&chan, Fail::TooLarge, EXIT_FAILED));
            }
            (ow, oh) = fit(v.width, v.height, bw, bh);
            let d = Decoder::new(threads).map_err(|_| fail(&chan, Fail::Decode, EXIT_FAILED))?;
            video = Some((d, ow, oh));
        }
        let audio = match (&src.audio, audio_on) {
            (Some(a), true) => AudioDec::new(a).ok(),
            _ => None,
        };
        let (rate, channels) = match (&audio, &src.audio) {
            (Some(_), Some(a)) => (a.rate, a.channels),
            _ => (0, 0),
        };
        let info = FromHelper::Info {
            width: ow,
            height: oh,
            rate,
            channels,
            duration_ms: src.duration_ms,
        };
        chan.send(&info.encode(), None).map_err(|_| EXIT_PROTOCOL)?;
        let mut m = Media {
            chan,
            src,
            video,
            audio,
            ring: None,
            conv: Converter::default(),
            ready: VecDeque::new(),
            spare: Vec::new(),
            credit: 0,
            pcm: Vec::new(),
            audio_frame: usize::from(channels) * 2,
            ended: false,
        };
        if m.video.is_some() {
            // The ring comes next.
            match m.wait()? {
                Step::Go if m.ring.is_some() => {}
                _ => return Ok(()),
            }
        }
        match forge {
            Some("frame") => {
                let msg = FromHelper::Frame {
                    slot: 0,
                    width: ow + 16,
                    height: oh,
                    stride: (ow + 16) * 4,
                    len: (ow + 16) * oh * 4,
                    pts_us: 0,
                };
                m.chan
                    .send(&msg.encode(), None)
                    .map_err(|_| EXIT_PROTOCOL)?;
            }
            Some("pcm") => {
                let msg = FromHelper::Pcm {
                    pts_us: 0,
                    data: vec![0; wire::MAX_PCM],
                };
                for _ in 0..64 {
                    m.chan
                        .send(&msg.encode(), None)
                        .map_err(|_| EXIT_PROTOCOL)?;
                }
            }
            _ => {}
        }
        m.pump()
    }

    /// Block for one message and apply it.
    fn wait(&mut self) -> Result<Step, i32> {
        let (tag, body) = match self.chan.recv() {
            Ok(Some(m)) => m,
            Ok(None) => return Ok(Step::Quit),
            Err(_) => return Err(EXIT_PROTOCOL),
        };
        let msg = ToHelper::decode(tag, &body).map_err(|_| EXIT_PROTOCOL)?;
        match msg {
            ToHelper::Ring { slots, slot_bytes } => {
                let fd = self.chan.take_fd().ok_or(EXIT_PROTOCOL)?;
                let need = self
                    .video
                    .as_ref()
                    .map_or(0, |v| v.1 as usize * v.2 as usize * 4);
                if self.ring.is_some() || (slot_bytes as usize) < need {
                    return Err(EXIT_PROTOCOL);
                }
                self.ring = Some(Ring {
                    fd,
                    slot_bytes: slot_bytes as usize,
                    slots,
                    free: (0..slots).rev().collect(),
                });
            }
            ToHelper::Release { slot } => {
                let r = self.ring.as_mut().ok_or(EXIT_PROTOCOL)?;
                if r.free.contains(&slot) || slot >= r.slots {
                    return Err(EXIT_PROTOCOL);
                }
                r.free.push(slot);
            }
            ToHelper::Want { bytes } => self.credit = self.credit.saturating_add(u64::from(bytes)),
            ToHelper::Seek { ms } => {
                self.src.seek(ms).map_err(|_| EXIT_FAILED)?;
                if let Some(v) = &mut self.video {
                    v.0.flush();
                }
                if let Some(a) = &mut self.audio {
                    a.reset();
                }
                while let Some((_, b)) = self.ready.pop_front() {
                    self.spare.push(b);
                }
                self.ended = false;
                self.chan
                    .send(&FromHelper::Seeked.encode(), None)
                    .map_err(|_| EXIT_PROTOCOL)?;
            }
            ToHelper::Quit => return Ok(Step::Quit),
            _ => return Err(EXIT_PROTOCOL),
        }
        Ok(Step::Go)
    }

    /// Apply every message already waiting, without blocking.
    fn poll(&mut self) -> Result<Step, i32> {
        while self.chan.readable(0) {
            if let Step::Quit = self.wait()? {
                return Ok(Step::Quit);
            }
        }
        Ok(Step::Go)
    }

    /// Write ready frames into free slots.
    fn flush_frames(&mut self) -> Result<(), i32> {
        let Some((_, ow, oh)) = self.video.as_ref().map(|v| ((), v.1, v.2)) else {
            return Ok(());
        };
        let Some(ring) = self.ring.as_mut() else {
            return Ok(());
        };
        while !ring.free.is_empty() {
            let Some((pts, px)) = self.ready.pop_front() else {
                break;
            };
            let slot = ring.free.pop().unwrap_or(0);
            let off = slot as u64 * ring.slot_bytes as u64;
            let mut done = 0;
            while done < px.len() {
                match rustix::io::pwrite(&ring.fd, &px[done..], off + done as u64) {
                    Ok(0) | Err(_) => return Err(EXIT_FAILED),
                    Ok(n) => done += n,
                }
            }
            let msg = FromHelper::Frame {
                slot,
                width: ow,
                height: oh,
                stride: ow * 4,
                len: px.len() as u32,
                pts_us: pts,
            };
            self.chan
                .send(&msg.encode(), None)
                .map_err(|_| EXIT_PROTOCOL)?;
            self.spare.push(px);
        }
        Ok(())
    }

    fn video_packet(&mut self, p: &Packet) -> Result<(), i32> {
        let Some((dec, ow, oh)) = self.video.as_mut() else {
            return Ok(());
        };
        let (ow, oh) = (*ow, *oh);
        let conv = &mut self.conv;
        let ready = &mut self.ready;
        let spare = &mut self.spare;
        let mut f = |pic: &PicView<'_>| {
            let mut buf = spare.pop().unwrap_or_default();
            buf.resize(ow as usize * oh as usize * 4, 0);
            conv.convert(pic, ow, oh, &mut buf);
            ready.push_back((pic.pts, buf));
        };
        // A frame that does not decode is skipped; the decoder resyncs at a keyframe.
        let _ = dec.decode(&p.data, p.pts_us, &mut f);
        Ok(())
    }

    /// The input is done: take the decoder's last pictures.
    fn finish_video(&mut self) {
        let Some((dec, ow, oh)) = self.video.as_mut() else {
            return;
        };
        let (ow, oh) = (*ow, *oh);
        let conv = &mut self.conv;
        let ready = &mut self.ready;
        let spare = &mut self.spare;
        let mut f = |pic: &PicView<'_>| {
            let mut buf = spare.pop().unwrap_or_default();
            buf.resize(ow as usize * oh as usize * 4, 0);
            conv.convert(pic, ow, oh, &mut buf);
            ready.push_back((pic.pts, buf));
        };
        let _ = dec.finish(&mut f);
    }

    fn audio_packet(&mut self, p: &Packet) -> Result<Step, i32> {
        let Some(dec) = self.audio.as_mut() else {
            return Ok(Step::Go);
        };
        self.pcm.clear();
        dec.decode(&p.data, &mut self.pcm)
            .map_err(|_| EXIT_FAILED)?;
        let bytes: Vec<u8> = self.pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        let frame = self.audio_frame.max(2);
        let mut sent = 0;
        while sent < bytes.len() {
            // Within the terminal's credit only (T-16 §4.5: the terminal enforces the rate),
            // in whole PCM frames.
            while self.credit < frame as u64 {
                self.flush_frames()?;
                if let Step::Quit = self.wait()? {
                    return Ok(Step::Quit);
                }
            }
            let n = (bytes.len() - sent)
                .min(wire::MAX_PCM)
                .min(usize::try_from(self.credit).unwrap_or(usize::MAX));
            let n = n - n % frame;
            let msg = FromHelper::Pcm {
                pts_us: p.pts_us,
                data: bytes[sent..sent + n].to_vec(),
            };
            self.chan
                .send(&msg.encode(), None)
                .map_err(|_| EXIT_PROTOCOL)?;
            self.credit = self.credit.saturating_sub(n as u64);
            sent += n;
        }
        Ok(Step::Go)
    }

    fn pump(&mut self) -> Result<(), i32> {
        loop {
            if let Step::Quit = self.poll()? {
                return Ok(());
            }
            self.flush_frames()?;
            // Enough converted frames waiting: a slot must come back first. Below that
            // the demuxer keeps going, so audio does not starve behind full slots.
            if self.ready.len() >= READY_MAX || self.ended {
                if self.ended && self.ready.is_empty() {
                    self.chan
                        .send(&FromHelper::End.encode(), None)
                        .map_err(|_| EXIT_PROTOCOL)?;
                    // Stay for a seek (loop) or the end.
                    loop {
                        match self.wait()? {
                            Step::Quit => return Ok(()),
                            Step::Go if !self.ended => break,
                            Step::Go => {}
                        }
                    }
                    continue;
                }
                if let Step::Quit = self.wait()? {
                    return Ok(());
                }
                continue;
            }
            let p = match self.src.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => {
                    self.finish_video();
                    self.ended = true;
                    continue;
                }
                Err(f) => {
                    let _ = self.chan.send(&FromHelper::Failed(f).encode(), None);
                    return Err(EXIT_FAILED);
                }
            };
            if p.video {
                self.video_packet(&p)?;
            } else if let Step::Quit = self.audio_packet(&p)? {
                return Ok(());
            }
        }
    }
}
