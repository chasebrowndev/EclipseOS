// SPDX-License-Identifier: AGPL-3.0-only

//! Capture-source boundary: a [`CaptureSource`] yields CPU frames (wl_shm
//! first, dmabuf later). The wlr-screencopy implementation is in `wlr`.

use std::time::Duration;

use ecl_portal_proto::Source;

use crate::session::CursorMode;

/// wl_shm pixel formats abyss can hand out, little-endian word order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// bytes B,G,R,A
    Argb8888,
    /// bytes B,G,R,x
    Xrgb8888,
    /// bytes R,G,B,A
    Abgr8888,
    /// bytes R,G,B,x
    Xbgr8888,
}

impl PixelFormat {
    pub const ARGB8888: u32 = 0;
    pub const XRGB8888: u32 = 1;
    pub const ABGR8888: u32 = 0x3432_4241;
    pub const XBGR8888: u32 = 0x3432_4258;

    pub fn from_wl(code: u32) -> Option<Self> {
        Some(match code {
            Self::ARGB8888 => Self::Argb8888,
            Self::XRGB8888 => Self::Xrgb8888,
            Self::ABGR8888 => Self::Abgr8888,
            Self::XBGR8888 => Self::Xbgr8888,
            _ => return None,
        })
    }
    pub fn has_alpha(self) -> bool {
        matches!(self, Self::Argb8888 | Self::Abgr8888)
    }
    /// True when bytes are B,G,R,*.
    pub fn is_bgr_order(self) -> bool {
        matches!(self, Self::Argb8888 | Self::Xrgb8888)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// One captured frame, top row first, tightly owned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: PixelFormat,
    pub data: Vec<u8>,
    /// Empty = unknown, treat as full-frame.
    pub damage: Vec<Rect>,
    /// Compositor timestamp, ns.
    pub pts_ns: u64,
}

impl Frame {
    pub fn validate(&self) -> bool {
        self.stride >= self.width.saturating_mul(4)
            && self.data.len() >= (self.stride as usize).saturating_mul(self.height as usize)
    }

    /// Copy into a destination of `dst_stride`, same channel order. If the
    /// source has no alpha and `dst_alpha`, the A byte is forced to 255.
    /// Returns bytes written (`dst_stride * height`), or None if `dst` is
    /// too small or the frame is malformed.
    pub fn copy_into(&self, dst: &mut [u8], dst_stride: u32, dst_alpha: bool) -> Option<usize> {
        if !self.validate() || dst_stride < self.width * 4 {
            return None;
        }
        let need = dst_stride as usize * self.height as usize;
        if dst.len() < need {
            return None;
        }
        let row = self.width as usize * 4;
        let force_a = dst_alpha && !self.format.has_alpha();
        for y in 0..self.height as usize {
            let s = &self.data[y * self.stride as usize..][..row];
            let d = &mut dst[y * dst_stride as usize..][..row];
            d.copy_from_slice(s);
            if force_a {
                for px in d.chunks_exact_mut(4) {
                    px[3] = 255;
                }
            }
        }
        Some(need)
    }

    /// Tightly packed RGBA8 (for PNG).
    pub fn to_rgba(&self) -> Option<Vec<u8>> {
        let mut out = vec![0u8; self.width as usize * self.height as usize * 4];
        self.copy_into(&mut out, self.width * 4, true)?;
        if self.format.is_bgr_order() {
            for px in out.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
        }
        Some(out)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("source not supported: {0}")]
    Unsupported(&'static str),
    #[error("capture denied or source gone")]
    Denied,
    #[error("compositor reported capture failure")]
    Failed,
    #[error("unsupported pixel format {0:#x}")]
    Format(u32),
    #[error("protocol/io error: {0}")]
    Io(String),
}

pub trait CaptureSource: Send {
    /// Block up to `timeout` for the next frame. A damage-driven source only
    /// returns when pixels changed (except the very first frame, which is
    /// unconditional). `Ok(None)` = timeout, try again.
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame>, CaptureError>;
}

/// Opens sources. Lane A's backend gets one of these; `wlr::WlrCapturer` is
/// the real one, [`mock::MockCapturer`] the test one.
pub trait Capturer: Send + Sync {
    /// `cursor` is Hidden or Embedded; Metadata degrades to Hidden upstream of
    /// here (abyss refuses cursor sessions today).
    fn open(
        &self,
        source: &Source,
        cursor: CursorMode,
    ) -> Result<Box<dyn CaptureSource>, CaptureError>;
}

pub mod mock {
    use super::*;
    use std::collections::VecDeque;

    pub struct MockSource {
        pub frames: VecDeque<Frame>,
    }

    impl CaptureSource for MockSource {
        fn next_frame(&mut self, _t: Duration) -> Result<Option<Frame>, CaptureError> {
            Ok(self.frames.pop_front())
        }
    }

    /// Yields `n` solid-colour frames for any Output; refuses windows.
    pub struct MockCapturer {
        pub frames: usize,
    }

    pub fn solid(w: u32, h: u32, format: PixelFormat, px: [u8; 4]) -> Frame {
        Frame {
            width: w,
            height: h,
            stride: w * 4,
            format,
            data: px
                .iter()
                .copied()
                .cycle()
                .take((w * h * 4) as usize)
                .collect(),
            damage: vec![Rect { x: 0, y: 0, w, h }],
            pts_ns: 0,
        }
    }

    impl Capturer for MockCapturer {
        fn open(
            &self,
            source: &Source,
            _c: CursorMode,
        ) -> Result<Box<dyn CaptureSource>, CaptureError> {
            match source {
                Source::Output { width, height, .. } => Ok(Box::new(MockSource {
                    frames: (0..self.frames)
                        .map(|_| solid(*width, *height, PixelFormat::Xrgb8888, [1, 2, 3, 0]))
                        .collect(),
                })),
                Source::Window { .. } => Err(CaptureError::Unsupported("window")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mock::solid;
    use super::*;

    #[test]
    fn rgba_swizzles_and_forces_alpha() {
        let f = solid(2, 1, PixelFormat::Xrgb8888, [10, 20, 30, 0]); // B,G,R,x
        assert_eq!(f.to_rgba().unwrap(), vec![30, 20, 10, 255, 30, 20, 10, 255]);
        let f = solid(1, 1, PixelFormat::Abgr8888, [1, 2, 3, 4]);
        assert_eq!(f.to_rgba().unwrap(), vec![1, 2, 3, 4]);
    }

    #[test]
    fn copy_respects_strides() {
        let mut f = solid(1, 2, PixelFormat::Argb8888, [9, 9, 9, 9]);
        f.stride = 8;
        f.data = vec![1, 1, 1, 1, 0, 0, 0, 0, 2, 2, 2, 2, 0, 0, 0, 0];
        let mut dst = vec![7u8; 8];
        assert_eq!(f.copy_into(&mut dst, 4, true), Some(8));
        assert_eq!(dst, vec![1, 1, 1, 1, 2, 2, 2, 2]);
    }

    #[test]
    fn copy_rejects_short_buffers() {
        let f = solid(2, 2, PixelFormat::Argb8888, [0; 4]);
        assert!(f.copy_into(&mut [0u8; 15], 8, false).is_none());
        assert!(f.copy_into(&mut [0u8; 64], 7, false).is_none());
    }

    #[test]
    fn format_codes() {
        assert_eq!(PixelFormat::from_wl(1), Some(PixelFormat::Xrgb8888));
        assert_eq!(
            PixelFormat::from_wl(0x3432_4241),
            Some(PixelFormat::Abgr8888)
        );
        assert_eq!(PixelFormat::from_wl(77), None);
    }
}
