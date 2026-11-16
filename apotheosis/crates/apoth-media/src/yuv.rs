// SPDX-License-Identifier: AGPL-3.0-only
//! YUV → RGBA with scaling, in the helper (T-16 §4.4: frames return as opaque pixels sized
//! from the negotiated geometry). Scaling picks the nearest source sample (the renderer
//! filters the quad); colour uses the stream's matrix and range in 16.16 fixed point.

use crate::av1::{Layout, Matrix, PicView};

/// The output size for a `sw` x `sh` source shown in a `bw` x `bh` pixel box: the source's
/// aspect ratio, as large as fits, never larger than the source. Both at least 1.
pub fn fit(sw: u32, sh: u32, bw: u32, bh: u32) -> (u32, u32) {
    let (sw, sh) = (u64::from(sw.max(1)), u64::from(sh.max(1)));
    let (bw, bh) = (u64::from(bw.max(1)).min(sw), u64::from(bh.max(1)).min(sh));
    // Width-limited or height-limited, whichever is tighter.
    let (w, h) = if bw * sh <= bh * sw {
        (bw, (bw * sh / sw).max(1))
    } else {
        ((bh * sw / sh).max(1), bh)
    };
    (w as u32, h as u32)
}

/// [`fit`] for a source shown at display aspect `dar` (anamorphic video): the source is
/// first stretched to that aspect along one axis (never shrunk), then fitted into the box.
/// `None` is square pixels.
pub fn fit_aspect(sw: u32, sh: u32, dar: Option<(u32, u32)>, bw: u32, bh: u32) -> (u32, u32) {
    let Some((dw, dh)) = dar.filter(|d| d.0 > 0 && d.1 > 0) else {
        return fit(sw, sh, bw, bh);
    };
    let (sw64, sh64) = (u64::from(sw.max(1)), u64::from(sh.max(1)));
    let (dw, dh) = (u64::from(dw), u64::from(dh));
    // Wider than the pixels: widen; taller: heighten.
    let (nw, nh) = if dw * sh64 >= dh * sw64 {
        ((sh64 * dw / dh).max(1), sh64)
    } else {
        (sw64, (sw64 * dh / dw).max(1))
    };
    let cap = |v: u64| v.min(u64::from(u16::MAX)) as u32;
    fit(cap(nw), cap(nh), bw, bh)
}

/// Converts pictures to RGBA at one output size; caches the sample maps.
#[derive(Debug, Default)]
pub struct Converter {
    key: (u32, u32, u32, u32, u8),
    xs: Vec<u32>,
    xc: Vec<u32>,
    ys: Vec<u32>,
    yc: Vec<u32>,
}

struct Coef {
    y_mul: i32,
    y_off: i32,
    rv: i32,
    gu: i32,
    gv: i32,
    bu: i32,
}

fn coef(m: Matrix, full: bool) -> Coef {
    let (kr, kgu, kgv, kb) = match m {
        Matrix::Bt601 => (1.402, 0.344_136, 0.714_136, 1.772),
        Matrix::Bt709 => (1.5748, 0.187_324, 0.468_124, 1.8556),
        Matrix::Bt2020 => (1.4746, 0.164_553, 0.571_353, 1.8814),
    };
    let (ys, cs, off) = if full {
        (1.0, 1.0, 0)
    } else {
        (255.0 / 219.0, 255.0 / 224.0, 16)
    };
    let f = |v: f64| (v * 65536.0).round() as i32;
    Coef {
        y_mul: f(ys),
        y_off: off,
        rv: f(kr * cs),
        gu: f(kgu * cs),
        gv: f(kgv * cs),
        bu: f(kb * cs),
    }
}

fn clamp8(v: i32) -> u8 {
    (v >> 16).clamp(0, 255) as u8
}

impl Converter {
    /// Write `pic` scaled to `ow` x `oh` into `out` (`oh` rows of `ow * 4` bytes).
    pub fn convert(&mut self, pic: &PicView<'_>, ow: u32, oh: u32, out: &mut [u8]) {
        let (ow, oh) = (ow.max(1), oh.max(1));
        let need = ow as usize * oh as usize * 4;
        if out.len() < need {
            return;
        }
        let sub = match pic.layout {
            Layout::I420 => 3,
            Layout::I422 => 1,
            Layout::I444 => 0,
            Layout::I400 => 4,
        };
        let key = (pic.width, pic.height, ow, oh, sub);
        if self.key != key {
            let map = |o: u32, s: u32| -> Vec<u32> {
                (0..o)
                    .map(|i| ((u64::from(i) * 2 + 1) * u64::from(s) / (u64::from(o) * 2)) as u32)
                    .map(|v| v.min(s - 1))
                    .collect()
            };
            self.xs = map(ow, pic.width);
            self.ys = map(oh, pic.height);
            let (hx, hy) = (sub & 1 != 0, sub & 2 != 0);
            self.xc = self
                .xs
                .iter()
                .map(|&x| if hx { x / 2 } else { x })
                .collect();
            self.yc = self
                .ys
                .iter()
                .map(|&y| if hy { y / 2 } else { y })
                .collect();
            self.key = key;
        }
        let c = coef(pic.matrix, pic.full_range);
        let hbd = pic.bpc > 8;
        let shift = u32::from(pic.bpc.saturating_sub(8));
        let sample = |plane: &[u8], stride: usize, x: u32, y: u32| -> i32 {
            if hbd {
                let i = y as usize * stride + x as usize * 2;
                match plane.get(i..i + 2) {
                    Some(b) => i32::from(u16::from_le_bytes([b[0], b[1]]) >> shift),
                    None => 0,
                }
            } else {
                plane
                    .get(y as usize * stride + x as usize)
                    .map_or(0, |&b| i32::from(b))
            }
        };
        let gray = pic.layout == Layout::I400;
        for (oy, row) in out[..need].chunks_exact_mut(ow as usize * 4).enumerate() {
            let (sy, cy) = (self.ys[oy], self.yc[oy]);
            for (ox, px) in row.chunks_exact_mut(4).enumerate() {
                let y = sample(pic.planes[0], pic.strides[0], self.xs[ox], sy);
                let y = (y - c.y_off) * c.y_mul;
                if gray {
                    let v = clamp8(y);
                    px.copy_from_slice(&[v, v, v, 255]);
                    continue;
                }
                let cx = self.xc[ox];
                let u = sample(pic.planes[1], pic.strides[1], cx, cy) - 128;
                let v = sample(pic.planes[2], pic.strides[2], cx, cy) - 128;
                px[0] = clamp8(y + c.rv * v);
                px[1] = clamp8(y - c.gu * u - c.gv * v);
                px[2] = clamp8(y + c.bu * u);
                px[3] = 255;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_keeps_aspect_and_never_upscales() {
        assert_eq!(fit(1920, 1080, 800, 800), (800, 450));
        assert_eq!(fit(1920, 1080, 4000, 300), (533, 300));
        assert_eq!(fit(320, 240, 4000, 4000), (320, 240));
        assert_eq!(fit(10, 1000, 5, 5), (1, 5));
        // Anamorphic: 720x480 at 16:9 shows 853x480, fitted into the box.
        assert_eq!(fit_aspect(720, 480, Some((16, 9)), 4000, 4000), (853, 480));
        assert_eq!(fit_aspect(720, 480, Some((16, 9)), 400, 400), (400, 225));
        // Taller than the pixels.
        assert_eq!(fit_aspect(640, 480, Some((1, 1)), 4000, 4000), (640, 640));
        assert_eq!(fit_aspect(160, 90, None, 80, 80), fit(160, 90, 80, 80));
    }

    #[test]
    fn limited_range_white_black_and_red() {
        let y = [235u8, 16, 63];
        let u = [128u8, 128, 102];
        let v = [128u8, 128, 240];
        for i in 0..3 {
            let pic = PicView {
                width: 2,
                height: 2,
                layout: Layout::I420,
                bpc: 8,
                matrix: Matrix::Bt709,
                full_range: false,
                planes: [&[y[i]; 4], &[u[i]], &[v[i]]],
                strides: [2, 1, 1],
                pts: 0,
            };
            let mut out = [0u8; 16];
            Converter::default().convert(&pic, 2, 2, &mut out);
            let px = &out[..4];
            match i {
                0 => assert!(px[..3].iter().all(|&c| c >= 253), "{px:?}"),
                1 => assert!(px[..3].iter().all(|&c| c <= 2), "{px:?}"),
                _ => assert!(px[0] > 240 && px[1] < 20 && px[2] < 20, "{px:?}"),
            }
            assert_eq!(px[3], 255);
        }
    }

    #[test]
    fn ten_bit_samples_scale_down() {
        let y = 940u16.to_le_bytes();
        let plane: Vec<u8> = y.iter().copied().cycle().take(8).collect();
        let c = 512u16.to_le_bytes();
        let pic = PicView {
            width: 2,
            height: 2,
            layout: Layout::I420,
            bpc: 10,
            matrix: Matrix::Bt709,
            full_range: false,
            planes: [&plane, &c, &c],
            strides: [4, 2, 2],
            pts: 0,
        };
        let mut out = [0u8; 4];
        Converter::default().convert(&pic, 1, 1, &mut out);
        assert!(out[0] >= 253 && out[1] >= 253 && out[2] >= 253, "{out:?}");
    }
}
