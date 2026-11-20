// SPDX-License-Identifier: AGPL-3.0-only
//! Software H.264 in the helper (T-16 §3, ADR 0118): `rust_h264`, pure Rust, behind a safe
//! [`Decoder`] with the same shape as [`crate::av1::Decoder`]. No `unsafe` of ours.
//!
//! Input is MP4 samples (length-prefixed NAL units) plus the `avcC` record's parameter
//! sets. Pictures come out in display order carrying the sample's timestamp, as
//! [`PicView`]s lent to a callback. Supported: Baseline, Main, High, High 10 and
//! High 4:2:2 (8 and 10 bit; 4:2:0, 4:2:2 and monochrome), progressive and interlaced,
//! CAVLC and CABAC. Everything the decoder refuses (4:4:4, lossless, FMO/ASO, SP/SI,
//! other bit depths) is [`DecodeError::Unsupported`] / [`Fail::Unsupported`], never a panic.
//! Covered by the `h264` fuzz target.

use rust_h264::decoder::{ChromaFormat, Frame, OrderedDecoder};
use rust_h264::error::DecodeError as H264Error;
use rust_h264::nal::{NalUnitType, SpsInfo, parse_avcc, parse_avcc_config, parse_sps_info};

use crate::av1::{FRAME_SIZE_LIMIT, Layout, Matrix, PicView};
use crate::wire::Fail;

/// Largest side accepted, as for every codec here.
const MAX_SIDE: u32 = 16384;

/// What the stream's first sequence parameter set says about the video.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Params {
    pub width: u32,
    pub height: u32,
    /// Bits per sample, 8 or 10. The VUI aspect ratio is not read: square pixels.
    pub bpc: u8,
    pub matrix: Matrix,
    pub full_range: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// A feature the decoder does not implement: the stream cannot play.
    Unsupported,
    /// A NAL unit that did not decode: skip it, the decoder resyncs at a keyframe.
    Bad,
}

/// Check an SPS against what the decoder and the helper accept.
fn check(info: &SpsInfo) -> Result<Params, Fail> {
    if info.chroma_format > 2 {
        return Err(Fail::Unsupported);
    }
    if !matches!(info.bit_depth_luma, 8 | 10)
        || (info.chroma_format != 0 && info.bit_depth_chroma != info.bit_depth_luma)
    {
        return Err(Fail::Unsupported);
    }
    // Lossless (transform bypass) exists only in the High 4:4:4 Predictive profile.
    if info.profile == 244 || info.profile == 44 {
        return Err(Fail::Unsupported);
    }
    let (w, h) = (info.width, info.height);
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
        return Err(Fail::TooLarge);
    }
    if u64::from(w) * u64::from(h) > u64::from(FRAME_SIZE_LIMIT) {
        return Err(Fail::TooLarge);
    }
    // H.273 matrix coefficients: 1 BT.709, 5 and 6 BT.601, 9 BT.2020 non-constant.
    let matrix = match info.matrix {
        1 => Matrix::Bt709,
        9 => Matrix::Bt2020,
        5 | 6 => Matrix::Bt601,
        _ if h >= 720 => Matrix::Bt709,
        _ => Matrix::Bt601,
    };
    Ok(Params {
        width: w,
        height: h,
        bpc: info.bit_depth_luma as u8,
        matrix,
        full_range: info.full_range,
    })
}

/// Read the `avcC` record (the raw box payload) and describe its first SPS.
/// `Unsupported`: a stream this decoder cannot play; `BadInput`: a broken record.
pub fn probe(avcc: &[u8]) -> Result<Params, Fail> {
    let cfg = parse_avcc_config(avcc).map_err(|_| Fail::BadInput)?;
    let sps = cfg.sps_nals.first().ok_or(Fail::BadInput)?;
    let info = parse_sps_info(sps).map_err(|e| match e {
        H264Error::Unsupported(_) => Fail::Unsupported,
        _ => Fail::BadInput,
    })?;
    check(&info)
}

/// Build an `avcC` record from the parts a demuxer gives (profile, compatibility, level,
/// NAL length size, SPS and PPS lists).
pub fn avcc_record(
    profile: u8,
    compat: u8,
    level: u8,
    length_size: u8,
    sps: &[Vec<u8>],
    pps: &[Vec<u8>],
) -> Option<Vec<u8>> {
    if sps.len() > 31 || pps.len() > 255 || !(1..=4).contains(&length_size) {
        return None;
    }
    let mut v = vec![1, profile, compat, level, 0xfc | (length_size - 1)];
    v.push(0xe0 | sps.len() as u8);
    for s in sps {
        v.extend_from_slice(&u16::try_from(s.len()).ok()?.to_be_bytes());
        v.extend_from_slice(s);
    }
    v.push(pps.len() as u8);
    for p in pps {
        v.extend_from_slice(&u16::try_from(p.len()).ok()?.to_be_bytes());
        v.extend_from_slice(p);
    }
    Some(v)
}

pub struct Decoder {
    avcc: Vec<u8>,
    length_size: usize,
    params: Params,
    dec: OrderedDecoder<i64>,
    /// Padded chroma planes for pictures whose chroma is smaller than `PicView` expects.
    pad: [Vec<u8>; 2],
}

impl std::fmt::Debug for Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decoder").finish_non_exhaustive()
    }
}

impl Decoder {
    /// A decoder for the stream `avcc` describes.
    pub fn new(avcc: &[u8]) -> Result<Decoder, Fail> {
        let params = probe(avcc)?;
        let mut d = Decoder {
            avcc: avcc.to_vec(),
            length_size: 4,
            params,
            dec: OrderedDecoder::new(),
            pad: [Vec::new(), Vec::new()],
        };
        d.prime()?;
        Ok(d)
    }

    /// Feed the record's parameter sets to a fresh decoder.
    fn prime(&mut self) -> Result<(), Fail> {
        let cfg = parse_avcc_config(&self.avcc).map_err(|_| Fail::BadInput)?;
        self.length_size = cfg.length_size;
        for n in cfg.sps_nals.iter().chain(cfg.pps_nals.iter()) {
            match self.dec.decode_nal_with_meta(n, 0) {
                Ok(_) => {}
                Err(H264Error::Unsupported(_)) => return Err(Fail::Unsupported),
                Err(_) => return Err(Fail::BadInput),
            }
        }
        Ok(())
    }

    /// Feed one sample; `f` sees every picture that becomes ready, in display order.
    /// Returns how many were shown. Pictures timestamped before zero (cut by the edit
    /// list) are decoded as references and not shown.
    pub fn decode(
        &mut self,
        data: &[u8],
        pts: i64,
        f: &mut dyn FnMut(&PicView<'_>),
    ) -> Result<usize, DecodeError> {
        let mut shown = 0;
        let mut bad = false;
        for nal in parse_avcc(data, self.length_size) {
            if nal.nal_unit_type == NalUnitType::Sps {
                // An in-band SPS may change the size: hold it to the same limits.
                match parse_sps_info(&nal) {
                    Ok(i) => match check(&i) {
                        Ok(_) => {}
                        Err(Fail::Unsupported) => return Err(DecodeError::Unsupported),
                        Err(_) => {
                            bad = true;
                            continue;
                        }
                    },
                    Err(_) => {
                        bad = true;
                        continue;
                    }
                }
            }
            match self.dec.decode_nal_with_meta(&nal, pts) {
                Ok(frames) => {
                    for (fr, p) in frames {
                        shown += self.emit(&fr, p, f);
                    }
                }
                Err(H264Error::Unsupported(_)) => return Err(DecodeError::Unsupported),
                Err(_) => bad = true,
            }
        }
        if bad && shown == 0 {
            return Err(DecodeError::Bad);
        }
        Ok(shown)
    }

    /// End of input: hand out every picture still in flight.
    pub fn finish(&mut self, f: &mut dyn FnMut(&PicView<'_>)) -> Result<usize, DecodeError> {
        let mut shown = 0;
        for (fr, p) in self.dec.flush_with_meta() {
            shown += self.emit(&fr, p, f);
        }
        Ok(shown)
    }

    /// Drop all state (after a seek): a fresh decoder primed with the parameter sets.
    pub fn flush(&mut self) {
        self.dec = OrderedDecoder::new();
        let _ = self.prime();
    }

    fn emit(&mut self, fr: &Frame, pts: i64, f: &mut dyn FnMut(&PicView<'_>)) -> usize {
        if pts < 0 {
            return 0;
        }
        let (w, h) = (fr.width, fr.height);
        if w == 0
            || h == 0
            || w > MAX_SIDE
            || h > MAX_SIDE
            || u64::from(w) * u64::from(h) > u64::from(FRAME_SIZE_LIMIT)
        {
            return 0;
        }
        let bps = fr.bytes_per_sample();
        let layout = match fr.chroma_format {
            ChromaFormat::Yuv420 => Layout::I420,
            ChromaFormat::Yuv422 => Layout::I422,
        };
        let mut v = PicView {
            width: w,
            height: h,
            layout,
            bpc: fr.bit_depth,
            matrix: self.params.matrix,
            full_range: self.params.full_range,
            planes: [&fr.y, &[], &[]],
            strides: [w as usize * bps, 0, 0],
            pts,
        };
        if fr.y.len() < w as usize * h as usize * bps {
            return 0;
        }
        // PicView sizes chroma with div_ceil; the decoder floors (odd monochrome crops).
        let (cw, ch) = v.chroma();
        let (fw, fh) = (fr.chroma_width() as usize, fr.chroma_height() as usize);
        if fr.u.len() < fw * fh * bps || fr.v.len() < fw * fh * bps || fw == 0 || fh == 0 {
            return 0;
        }
        if (fw, fh) == (cw, ch) {
            v.planes[1] = &fr.u;
            v.planes[2] = &fr.v;
            v.strides[1] = cw * bps;
            v.strides[2] = cw * bps;
        } else {
            for (i, src) in [&fr.u, &fr.v].into_iter().enumerate() {
                let dst = &mut self.pad[i];
                dst.clear();
                for y in 0..ch {
                    let row = y.min(fh - 1) * fw * bps;
                    dst.extend_from_slice(&src[row..row + fw * bps]);
                    for _ in fw..cw {
                        dst.extend_from_within(dst.len() - bps..);
                    }
                }
            }
            v.planes[1] = &self.pad[0];
            v.planes[2] = &self.pad[1];
            v.strides[1] = cw * bps;
            v.strides[2] = cw * bps;
        }
        f(&v);
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_is_an_error_not_a_crash() {
        assert!(matches!(
            probe(&[]),
            Err(Fail::BadInput | Fail::Unsupported)
        ));
        assert!(probe(&[1, 100, 0, 30, 0xff, 0xe1, 0, 4, 0x67, 1, 2, 3, 0]).is_err());
        assert_eq!(avcc_record(100, 0, 30, 4, &[vec![0x67; 70_000]], &[]), None);
    }

    #[test]
    fn avcc_round_trips_through_the_parser() {
        let r = avcc_record(100, 0, 30, 4, &[vec![0x67, 1, 2]], &[vec![0x68, 3]]).unwrap();
        let c = parse_avcc_config(&r).unwrap();
        assert_eq!(c.length_size, 4);
        assert_eq!((c.sps_nals.len(), c.pps_nals.len()), (1, 1));
    }
}
