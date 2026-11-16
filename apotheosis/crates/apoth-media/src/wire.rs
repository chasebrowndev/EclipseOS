// SPDX-License-Identifier: AGPL-3.0-only
//! The terminal ↔ helper protocol (T-16 §4): length-prefixed messages over a unix stream
//! socket; file descriptors (the input, the frame ring) travel as `SCM_RIGHTS` with the
//! message that names them. Frame metadata travels here, never through the ring.
//!
//! Frame: `u32 len` (little-endian, tag and body, at most [`MAX_MESSAGE`]), `u8 tag`, body.
//! Every decoder is total: any byte string decodes to a message or an error, never a panic
//! (`helper_wire` fuzz target). Semantic validation against the negotiated geometry is the
//! receiver's job ([`crate::client`]).

/// Largest message (tag and body). Image requests and replies are the big ones.
pub const MAX_MESSAGE: usize = 448 << 20;
/// Largest PCM message body.
pub const MAX_PCM: usize = 1 << 20;
/// Frames in one image reply.
pub const MAX_IMAGE_FRAMES: usize = 4096;
/// Ring slots at most.
pub const MAX_SLOTS: u8 = 16;

/// Error codes the helper reports; also the `error=<code>` the placement reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Fail {
    /// Landlock not fully enforced, or `prctl`/seccomp failed (T-16 §4.3).
    SandboxUnavailable = 1,
    /// Not a container or codec we play.
    Unsupported = 2,
    /// The container or a stream is broken.
    BadInput = 3,
    /// Larger than the resolution quota.
    TooLarge = 4,
    /// The peer broke the protocol.
    Protocol = 5,
    /// Decoder failure.
    Decode = 6,
}

impl Fail {
    pub fn code(self) -> &'static str {
        match self {
            Fail::SandboxUnavailable => "sandbox-unavailable",
            Fail::Unsupported => "unsupported",
            Fail::BadInput => "bad-input",
            Fail::TooLarge => "too-large",
            Fail::Protocol => "helper-protocol",
            Fail::Decode => "decode",
        }
    }

    fn from_u8(v: u8) -> Option<Fail> {
        Some(match v {
            1 => Fail::SandboxUnavailable,
            2 => Fail::Unsupported,
            3 => Fail::BadInput,
            4 => Fail::TooLarge,
            5 => Fail::Protocol,
            6 => Fail::Decode,
            _ => return None,
        })
    }
}

/// Which image decoder an image request runs (graphics.sandbox-decode, T-15 §5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ImageKind {
    /// A still PNG (kitty `f=100`).
    Png = 0,
    /// Any supported container, animations included (iTerm2, `apoth icat`).
    Any = 1,
}

/// Terminal → helper.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToHelper {
    /// Play the input fd (attached). Output is scaled to fit `box_w` x `box_h`.
    Open {
        audio: bool,
        box_w: u32,
        box_h: u32,
        max_w: u32,
        max_h: u32,
    },
    /// The frame ring (memfd attached): `slots` slots of `slot_bytes` each.
    Ring {
        slots: u8,
        slot_bytes: u32,
    },
    /// Slot `slot` is free again.
    Release {
        slot: u8,
    },
    /// Audio credit: the terminal will take `bytes` more PCM bytes.
    Want {
        bytes: u32,
    },
    /// Seek to `ms` from the start.
    Seek {
        ms: u64,
    },
    /// Decode one image (image mode).
    Image {
        kind: ImageKind,
        max_axis: u32,
        max_decoded: u64,
        anim_quota: u64,
        data: Vec<u8>,
    },
    Quit,
}

/// One decoded image frame (image mode).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageFrame {
    pub width: u32,
    pub height: u32,
    pub gap_ms: i32,
    pub rgba: Vec<u8>,
}

/// Helper → terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FromHelper {
    /// The sandbox is in force; the helper accepts input now.
    Ready,
    /// The helper stops with this error.
    Failed(Fail),
    /// Stream info after open: output size (0x0: no video), audio format (rate 0: none),
    /// the coded video size, and the declared display aspect (`0:0`: square pixels).
    Info {
        width: u32,
        height: u32,
        rate: u32,
        channels: u8,
        duration_ms: u64,
        video_w: u32,
        video_h: u32,
        dar_w: u32,
        dar_h: u32,
    },
    /// A frame in a ring slot: RGBA, `stride` bytes per row, `len` bytes used.
    Frame {
        slot: u8,
        width: u32,
        height: u32,
        stride: u32,
        len: u32,
        pts_us: i64,
    },
    /// Interleaved s16le PCM in the negotiated format.
    Pcm { pts_us: i64, data: Vec<u8> },
    /// The input is played to its end.
    End,
    /// A `Seek` is done: frames and PCM before this message predate it.
    Seeked,
    /// Image mode: decoded frames.
    ImageOk(Vec<ImageFrame>),
    /// Image mode: the `apoth_image::ErrCode` index of the failure.
    ImageErr(u8),
}

/// Why a buffer is not a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    /// More bytes are needed.
    Short,
    /// Length, tag or field is invalid.
    Bad,
}

struct W(Vec<u8>);

impl W {
    fn new(tag: u8) -> W {
        let mut v = Vec::with_capacity(64);
        v.extend_from_slice(&[0, 0, 0, 0, tag]);
        W(v)
    }
    fn u8(mut self, v: u8) -> W {
        self.0.push(v);
        self
    }
    fn u32(mut self, v: u32) -> W {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u64(mut self, v: u64) -> W {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn i64(mut self, v: i64) -> W {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn bytes(mut self, b: &[u8]) -> W {
        self.0.extend_from_slice(&(b.len() as u32).to_le_bytes());
        self.0.extend_from_slice(b);
        self
    }
    fn done(mut self) -> Vec<u8> {
        let n = (self.0.len() - 4) as u32;
        self.0[..4].copy_from_slice(&n.to_le_bytes());
        self.0
    }
}

struct R<'a>(&'a [u8]);

impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], WireError> {
        if self.0.len() < n {
            return Err(WireError::Bad);
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, WireError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Result<u64, WireError> {
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_le_bytes(a))
    }
    fn i64(&mut self) -> Result<i64, WireError> {
        Ok(self.u64()? as i64)
    }
    fn bool(&mut self) -> Result<bool, WireError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(WireError::Bad),
        }
    }
    fn bytes(&mut self, max: usize) -> Result<Vec<u8>, WireError> {
        let n = self.u32()? as usize;
        if n > max {
            return Err(WireError::Bad);
        }
        Ok(self.take(n)?.to_vec())
    }
    fn end(&self) -> Result<(), WireError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(WireError::Bad)
        }
    }
}

/// Split one frame off the front of `buf`: (tag, body, bytes consumed).
pub fn split(buf: &[u8]) -> Result<(u8, &[u8], usize), WireError> {
    if buf.len() < 4 {
        return Err(WireError::Short);
    }
    let n = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if n == 0 || n > MAX_MESSAGE {
        return Err(WireError::Bad);
    }
    if buf.len() < 4 + n {
        return Err(WireError::Short);
    }
    Ok((buf[4], &buf[5..4 + n], 4 + n))
}

impl ToHelper {
    /// Whether this message carries one fd.
    pub fn has_fd(&self) -> bool {
        matches!(self, ToHelper::Open { .. } | ToHelper::Ring { .. })
    }

    pub fn encode(&self) -> Vec<u8> {
        match self {
            ToHelper::Open {
                audio,
                box_w,
                box_h,
                max_w,
                max_h,
            } => W::new(1)
                .u8(u8::from(*audio))
                .u32(*box_w)
                .u32(*box_h)
                .u32(*max_w)
                .u32(*max_h)
                .done(),
            ToHelper::Ring { slots, slot_bytes } => W::new(2).u8(*slots).u32(*slot_bytes).done(),
            ToHelper::Release { slot } => W::new(3).u8(*slot).done(),
            ToHelper::Want { bytes } => W::new(4).u32(*bytes).done(),
            ToHelper::Seek { ms } => W::new(5).u64(*ms).done(),
            ToHelper::Image {
                kind,
                max_axis,
                max_decoded,
                anim_quota,
                data,
            } => W::new(6)
                .u8(*kind as u8)
                .u32(*max_axis)
                .u64(*max_decoded)
                .u64(*anim_quota)
                .bytes(data)
                .done(),
            ToHelper::Quit => W::new(7).done(),
        }
    }

    pub fn decode(tag: u8, body: &[u8]) -> Result<ToHelper, WireError> {
        let mut r = R(body);
        let m = match tag {
            1 => ToHelper::Open {
                audio: r.bool()?,
                box_w: r.u32()?,
                box_h: r.u32()?,
                max_w: r.u32()?,
                max_h: r.u32()?,
            },
            2 => {
                let slots = r.u8()?;
                if slots == 0 || slots > MAX_SLOTS {
                    return Err(WireError::Bad);
                }
                ToHelper::Ring {
                    slots,
                    slot_bytes: r.u32()?,
                }
            }
            3 => ToHelper::Release { slot: r.u8()? },
            4 => ToHelper::Want { bytes: r.u32()? },
            5 => ToHelper::Seek { ms: r.u64()? },
            6 => ToHelper::Image {
                kind: match r.u8()? {
                    0 => ImageKind::Png,
                    1 => ImageKind::Any,
                    _ => return Err(WireError::Bad),
                },
                max_axis: r.u32()?,
                max_decoded: r.u64()?,
                anim_quota: r.u64()?,
                data: r.bytes(MAX_MESSAGE)?,
            },
            7 => ToHelper::Quit,
            _ => return Err(WireError::Bad),
        };
        r.end()?;
        Ok(m)
    }
}

impl FromHelper {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            FromHelper::Ready => W::new(1).done(),
            FromHelper::Failed(f) => W::new(2).u8(*f as u8).done(),
            FromHelper::Info {
                width,
                height,
                rate,
                channels,
                duration_ms,
                video_w,
                video_h,
                dar_w,
                dar_h,
            } => W::new(3)
                .u32(*width)
                .u32(*height)
                .u32(*rate)
                .u8(*channels)
                .u64(*duration_ms)
                .u32(*video_w)
                .u32(*video_h)
                .u32(*dar_w)
                .u32(*dar_h)
                .done(),
            FromHelper::Frame {
                slot,
                width,
                height,
                stride,
                len,
                pts_us,
            } => W::new(4)
                .u8(*slot)
                .u32(*width)
                .u32(*height)
                .u32(*stride)
                .u32(*len)
                .i64(*pts_us)
                .done(),
            FromHelper::Pcm { pts_us, data } => W::new(5).i64(*pts_us).bytes(data).done(),
            FromHelper::End => W::new(6).done(),
            FromHelper::Seeked => W::new(9).done(),
            FromHelper::ImageOk(frames) => {
                let mut w = W::new(7).u32(frames.len() as u32);
                for f in frames {
                    w = w
                        .u32(f.width)
                        .u32(f.height)
                        .u32(f.gap_ms as u32)
                        .bytes(&f.rgba);
                }
                w.done()
            }
            FromHelper::ImageErr(c) => W::new(8).u8(*c).done(),
        }
    }

    pub fn decode(tag: u8, body: &[u8]) -> Result<FromHelper, WireError> {
        let mut r = R(body);
        let m = match tag {
            1 => FromHelper::Ready,
            2 => FromHelper::Failed(Fail::from_u8(r.u8()?).ok_or(WireError::Bad)?),
            3 => FromHelper::Info {
                width: r.u32()?,
                height: r.u32()?,
                rate: r.u32()?,
                channels: r.u8()?,
                duration_ms: r.u64()?,
                video_w: r.u32()?,
                video_h: r.u32()?,
                dar_w: r.u32()?,
                dar_h: r.u32()?,
            },
            4 => FromHelper::Frame {
                slot: r.u8()?,
                width: r.u32()?,
                height: r.u32()?,
                stride: r.u32()?,
                len: r.u32()?,
                pts_us: r.i64()?,
            },
            5 => FromHelper::Pcm {
                pts_us: r.i64()?,
                data: r.bytes(MAX_PCM)?,
            },
            6 => FromHelper::End,
            9 => FromHelper::Seeked,
            7 => {
                let n = r.u32()? as usize;
                if n > MAX_IMAGE_FRAMES {
                    return Err(WireError::Bad);
                }
                let mut frames = Vec::new();
                for _ in 0..n {
                    let width = r.u32()?;
                    let height = r.u32()?;
                    let gap_ms = r.u32()? as i32;
                    let rgba = r.bytes(MAX_MESSAGE)?;
                    if (width as u64) * (height as u64) * 4 != rgba.len() as u64 {
                        return Err(WireError::Bad);
                    }
                    frames.push(ImageFrame {
                        width,
                        height,
                        gap_ms,
                        rgba,
                    });
                }
                FromHelper::ImageOk(frames)
            }
            8 => FromHelper::ImageErr(r.u8()?),
            _ => return Err(WireError::Bad),
        };
        r.end()?;
        Ok(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_to(m: ToHelper) {
        let b = m.encode();
        let (tag, body, n) = split(&b).unwrap();
        assert_eq!(n, b.len());
        assert_eq!(ToHelper::decode(tag, body).unwrap(), m);
    }

    fn round_from(m: FromHelper) {
        let b = m.encode();
        let (tag, body, _) = split(&b).unwrap();
        assert_eq!(FromHelper::decode(tag, body).unwrap(), m);
    }

    #[test]
    fn every_message_round_trips() {
        round_to(ToHelper::Open {
            audio: true,
            box_w: 800,
            box_h: 480,
            max_w: 3840,
            max_h: 2160,
        });
        round_to(ToHelper::Ring {
            slots: 6,
            slot_bytes: 1 << 20,
        });
        round_to(ToHelper::Release { slot: 3 });
        round_to(ToHelper::Want { bytes: 4096 });
        round_to(ToHelper::Seek { ms: 1234 });
        round_to(ToHelper::Image {
            kind: ImageKind::Any,
            max_axis: 10_000,
            max_decoded: 1 << 30,
            anim_quota: 1 << 28,
            data: vec![1, 2, 3],
        });
        round_to(ToHelper::Quit);
        round_from(FromHelper::Ready);
        round_from(FromHelper::Failed(Fail::SandboxUnavailable));
        round_from(FromHelper::Info {
            width: 640,
            height: 360,
            rate: 48_000,
            channels: 2,
            duration_ms: 1000,
            video_w: 720,
            video_h: 480,
            dar_w: 16,
            dar_h: 9,
        });
        round_from(FromHelper::Frame {
            slot: 1,
            width: 2,
            height: 2,
            stride: 8,
            len: 16,
            pts_us: -5,
        });
        round_from(FromHelper::Pcm {
            pts_us: 7,
            data: vec![0; 8],
        });
        round_from(FromHelper::End);
        round_from(FromHelper::Seeked);
        round_from(FromHelper::ImageOk(vec![ImageFrame {
            width: 1,
            height: 1,
            gap_ms: 40,
            rgba: vec![1, 2, 3, 4],
        }]));
        round_from(FromHelper::ImageErr(2));
    }

    #[test]
    fn truncated_and_trailing_bytes_are_errors() {
        let b = ToHelper::Seek { ms: 9 }.encode();
        assert_eq!(split(&b[..3]), Err(WireError::Short));
        assert_eq!(split(&b[..b.len() - 1]), Err(WireError::Short));
        let (tag, body, _) = split(&b).unwrap();
        assert_eq!(ToHelper::decode(tag, &body[..4]), Err(WireError::Bad));
        let mut long = body.to_vec();
        long.push(0);
        assert_eq!(ToHelper::decode(tag, &long), Err(WireError::Bad));
        assert_eq!(split(&[0, 0, 0, 0, 1]), Err(WireError::Bad));
        // An image frame whose pixel count does not match its byte count.
        let bad = FromHelper::ImageOk(vec![ImageFrame {
            width: 2,
            height: 2,
            gap_ms: 0,
            rgba: vec![0; 4],
        }])
        .encode();
        let (tag, body, _) = split(&bad).unwrap();
        assert_eq!(FromHelper::decode(tag, body), Err(WireError::Bad));
    }
}
