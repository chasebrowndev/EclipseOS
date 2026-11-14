// SPDX-License-Identifier: AGPL-3.0-only
//! The helper's only rav1d `unsafe` (T-16 §3, T-18 §7): rav1d's dav1d-compatible
//! `extern "C"` API (`dav1d_open`, `dav1d_send_data`, `dav1d_get_picture`, …) behind a
//! safe [`Decoder`]. Covered by the `av1_obu` fuzz target.
//!
//! A picture is lent to a callback as a [`PicView`] borrowing rav1d's planes and released
//! right after, so no rav1d reference outlives the call that produced it.
#![allow(unsafe_code)]

use std::mem::MaybeUninit;
use std::ptr::NonNull;

use rav1d::include::dav1d::data::Dav1dData;
use rav1d::include::dav1d::dav1d::{Dav1dContext, Dav1dSettings};
use rav1d::include::dav1d::headers::{
    DAV1D_MC_BT709, DAV1D_MC_BT2020_NCL, DAV1D_PIXEL_LAYOUT_I400, DAV1D_PIXEL_LAYOUT_I420,
    DAV1D_PIXEL_LAYOUT_I422, DAV1D_PIXEL_LAYOUT_I444,
};
use rav1d::include::dav1d::picture::Dav1dPicture;
use rav1d::src::lib as d;

/// `-EAGAIN` as rav1d returns it.
const EAGAIN: i32 = -11;
/// Largest frame (pixels) the decoder will allocate.
pub const FRAME_SIZE_LIMIT: u32 = 8192 * 4352;

/// Chroma layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    I400,
    I420,
    I422,
    I444,
}

/// YUV → RGB matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Matrix {
    Bt601,
    Bt709,
    Bt2020,
}

/// One decoded picture, borrowed from rav1d for the duration of a callback.
#[derive(Clone, Copy, Debug)]
pub struct PicView<'a> {
    pub width: u32,
    pub height: u32,
    pub layout: Layout,
    /// Bits per component (8, 10, 12); above 8 samples are little-endian u16.
    pub bpc: u8,
    pub matrix: Matrix,
    pub full_range: bool,
    /// Y, U, V plane bytes (`rows * stride`, the last row possibly short).
    pub planes: [&'a [u8]; 3],
    /// Bytes per row of each plane.
    pub strides: [usize; 3],
    /// The timestamp passed with the data that produced it.
    pub pts: i64,
}

impl PicView<'_> {
    /// Chroma plane size.
    pub fn chroma(&self) -> (usize, usize) {
        let (w, h) = (self.width as usize, self.height as usize);
        match self.layout {
            Layout::I400 => (0, 0),
            Layout::I420 => (w.div_ceil(2), h.div_ceil(2)),
            Layout::I422 => (w.div_ceil(2), h),
            Layout::I444 => (w, h),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeError(pub i32);

pub struct Decoder {
    ctx: Option<Dav1dContext>,
}

impl std::fmt::Debug for Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decoder").finish_non_exhaustive()
    }
}

// SAFETY: the context is an `Arc` of a `Send + Sync` rav1d context; we only use it from
// `&mut self` methods.
unsafe impl Send for Decoder {}

impl Decoder {
    /// `threads` worker threads (0: rav1d picks; the sandbox forbids the file reads it
    /// would use, so the caller passes a count). One frame of delay: low latency.
    pub fn new(threads: u32) -> Result<Decoder, DecodeError> {
        let mut s = MaybeUninit::<Dav1dSettings>::uninit();
        // SAFETY: `s` is valid to write; `dav1d_default_settings` initialises every field.
        unsafe { d::dav1d_default_settings(NonNull::new_unchecked(s.as_mut_ptr())) };
        // SAFETY: initialised just above.
        let mut s = unsafe { s.assume_init() };
        s.n_threads = threads.min(16) as i32;
        s.max_frame_delay = 1;
        s.frame_size_limit = FRAME_SIZE_LIMIT;
        s.apply_grain = 1;
        let mut ctx: Option<Dav1dContext> = None;
        // SAFETY: both pointers are valid for the call; `ctx` receives the context.
        let r =
            unsafe { d::dav1d_open(Some(NonNull::from(&mut ctx)), Some(NonNull::from(&mut s))) };
        if r.0 < 0 || ctx.is_none() {
            return Err(DecodeError(r.0));
        }
        Ok(Decoder { ctx })
    }

    /// Feed one temporal unit; `f` sees every picture it makes available. Returns how
    /// many pictures were shown.
    pub fn decode(
        &mut self,
        data: &[u8],
        pts: i64,
        f: &mut dyn FnMut(&PicView<'_>),
    ) -> Result<usize, DecodeError> {
        let mut shown = 0;
        if data.is_empty() {
            return Ok(0);
        }
        let mut buf = Dav1dData::default();
        // SAFETY: `buf` is valid to write; on success it owns a rav1d allocation of
        // `data.len()` bytes that `ptr` points to.
        let ptr = unsafe { d::dav1d_data_create(Some(NonNull::from(&mut buf)), data.len()) };
        if ptr.is_null() {
            return Err(DecodeError(-12));
        }
        // SAFETY: `ptr` is a fresh allocation of exactly `data.len()` bytes.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len()) };
        buf.m.timestamp = pts;
        loop {
            // SAFETY: the context is open; `buf` is a valid Dav1dData.
            let r = unsafe { d::dav1d_send_data(self.ctx, Some(NonNull::from(&mut buf))) };
            if r.0 < 0 && r.0 != EAGAIN {
                // SAFETY: `buf` is ours to release.
                unsafe { d::dav1d_data_unref(Some(NonNull::from(&mut buf))) };
                return Err(DecodeError(r.0));
            }
            let n = self.drain(f)?;
            shown += n;
            if buf.sz == 0 {
                break;
            }
            if r.0 == EAGAIN && n == 0 {
                // Neither side made progress: drop the rest rather than spin.
                // SAFETY: as above.
                unsafe { d::dav1d_data_unref(Some(NonNull::from(&mut buf))) };
                break;
            }
        }
        Ok(shown)
    }

    fn drain(&mut self, f: &mut dyn FnMut(&PicView<'_>)) -> Result<usize, DecodeError> {
        let mut n = 0;
        loop {
            let mut p = Dav1dPicture::default();
            // SAFETY: the context is open; `p` is valid to write.
            let r = unsafe { d::dav1d_get_picture(self.ctx, Some(NonNull::from(&mut p))) };
            if r.0 == EAGAIN {
                return Ok(n);
            }
            if r.0 < 0 {
                return Err(DecodeError(r.0));
            }
            let ok = match view(&p) {
                Ok(v) => {
                    f(&v);
                    n += 1;
                    Ok(())
                }
                Err(e) => Err(e),
            };
            // SAFETY: `p` holds a picture reference from `dav1d_get_picture`; the view
            // borrowing it is gone.
            unsafe { d::dav1d_picture_unref(Some(NonNull::from(&mut p))) };
            ok?;
        }
    }

    /// End of input: hand out every picture still in flight.
    pub fn finish(&mut self, f: &mut dyn FnMut(&PicView<'_>)) -> Result<usize, DecodeError> {
        if self.ctx.is_none() {
            return Ok(0);
        }
        self.drain(f)
    }

    /// Drop all state (after a seek).
    pub fn flush(&mut self) {
        if let Some(c) = self.ctx {
            // SAFETY: the context is open.
            unsafe { d::dav1d_flush(c) };
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        if self.ctx.is_some() {
            // SAFETY: `ctx` came from `dav1d_open` and is closed once, here.
            unsafe { d::dav1d_close(Some(NonNull::from(&mut self.ctx))) };
        }
    }
}

fn view(p: &Dav1dPicture) -> Result<PicView<'_>, DecodeError> {
    let bad = DecodeError(-22);
    let (w, h) = (p.p.w, p.p.h);
    if w <= 0 || h <= 0 || (w as u64) * (h as u64) > u64::from(FRAME_SIZE_LIMIT) {
        return Err(bad);
    }
    let layout = match p.p.layout {
        DAV1D_PIXEL_LAYOUT_I400 => Layout::I400,
        DAV1D_PIXEL_LAYOUT_I420 => Layout::I420,
        DAV1D_PIXEL_LAYOUT_I422 => Layout::I422,
        DAV1D_PIXEL_LAYOUT_I444 => Layout::I444,
        _ => return Err(bad),
    };
    let bpc = p.p.bpc;
    if !(8..=12).contains(&bpc) {
        return Err(bad);
    }
    let tall = h >= 720;
    let (matrix, full_range) = match p.seq_hdr {
        Some(s) => {
            // SAFETY: a returned picture's `seq_hdr` points at its live sequence header.
            let s = unsafe { s.as_ref() };
            let m = match s.mtrx {
                DAV1D_MC_BT709 => Matrix::Bt709,
                DAV1D_MC_BT2020_NCL => Matrix::Bt2020,
                _ if tall => Matrix::Bt709,
                _ => Matrix::Bt601,
            };
            (m, s.color_range != 0)
        }
        None => (if tall { Matrix::Bt709 } else { Matrix::Bt601 }, false),
    };
    let mut v = PicView {
        width: w as u32,
        height: h as u32,
        layout,
        bpc: bpc as u8,
        matrix,
        full_range,
        planes: [&[], &[], &[]],
        strides: [0; 3],
        pts: p.m.timestamp,
    };
    let (cw, ch) = v.chroma();
    let bps = if bpc > 8 { 2 } else { 1 };
    let dims = [
        (w as usize, h as usize, p.stride[0]),
        (cw, ch, p.stride[1]),
        (cw, ch, p.stride[1]),
    ];
    for (i, (pw, ph, stride)) in dims.into_iter().enumerate() {
        if pw == 0 {
            continue;
        }
        let Some(base) = p.data[i] else {
            return Err(bad);
        };
        let row = pw * bps;
        if stride < 0 || (stride as usize) < row {
            return Err(bad);
        }
        let stride = stride as usize;
        let len = (ph - 1) * stride + row;
        // SAFETY: rav1d allocates each plane as `ph` rows of `stride` bytes (checked ≥ the
        // row's sample bytes above); the picture reference is held until after the view
        // is dropped (see `drain`).
        v.planes[i] = unsafe { std::slice::from_raw_parts(base.as_ptr().cast::<u8>(), len) };
        v.strides[i] = stride;
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_is_an_error_not_a_crash() {
        let mut d = Decoder::new(1).unwrap();
        let mut n = 0;
        let mut f = |_: &PicView<'_>| n += 1;
        let _ = d.decode(
            &[0x12, 0x00, 0x0a, 0x0b, 0, 0, 0, 0x24, 0xff, 0xff],
            0,
            &mut f,
        );
        let _ = d.decode(&[0xff; 64], 1, &mut f);
        assert_eq!(n, 0);
        d.flush();
    }
}
