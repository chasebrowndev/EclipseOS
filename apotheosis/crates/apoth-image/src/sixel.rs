// SPDX-License-Identifier: AGPL-3.0-only
//! Streaming sixel decoder with limits built in (T-15 §2, GRAPHICS-17, -18, -34, -41).
//!
//! The caller strips the DCS introducer and `q` final byte, hands the DCS parameters to
//! [`SixelParams::from_dcs`], then streams the data bytes through [`SixelDecoder::feed`]
//! in slices of any size and calls [`SixelDecoder::finish`] at ST.
//!
//! Limits: a byte cap and, independently, a decoded-pixel cap (a tiny stream can declare a
//! huge raster); raster attribute dimensions are cap-checked but never allocate. The
//! DECSDM / `?1070` / `?8452` modes are grid wiring (M25); this decoder only takes the
//! initial registers (`?1070` reset = shared registers) and returns the final ones.

use crate::{Image, Limits};

/// DCS `P1;P2;P3 q` parameters plus the context the decoder needs.
#[derive(Debug, Clone)]
pub struct SixelParams {
    /// P1 pixel aspect selector (default 0 = 2:1).
    pub p1: u32,
    /// P2: 1 = unwritten pixels transparent; 0 or 2 = filled with `background`.
    pub transparent: bool,
    /// Pane background (opaque RGBA) for P2 = 0|2.
    pub background: [u8; 4],
    /// Registers inherited from the previous image when `?1070` is reset.
    pub registers: Option<Vec<[u8; 3]>>,
}

impl SixelParams {
    pub fn from_dcs(params: &[u16], background: [u8; 4], registers: Option<Vec<[u8; 3]>>) -> Self {
        Self {
            p1: u32::from(params.first().copied().unwrap_or(0)),
            transparent: params.get(1).copied() == Some(1),
            background,
            registers,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SixelError {
    /// Stream exceeded the byte cap.
    TooManyBytes,
    /// Decoded raster exceeds the pixel cap or an axis cap.
    TooLarge,
    /// No pixels were drawn.
    Empty,
}

/// A finished sixel image and the register file it ended with.
#[derive(Debug)]
pub struct SixelOutput {
    pub image: Image,
    pub registers: Vec<[u8; 3]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Raster,
    Color,
    Repeat,
}

/// VT340 default colours for registers 0..15, in percent.
const DEFAULT_PALETTE: [[u8; 3]; 16] = [
    [0, 0, 0],
    [20, 20, 80],
    [80, 13, 13],
    [20, 80, 20],
    [80, 20, 80],
    [20, 80, 80],
    [80, 80, 20],
    [53, 53, 53],
    [26, 26, 26],
    [33, 33, 60],
    [60, 26, 26],
    [33, 60, 33],
    [60, 33, 60],
    [33, 60, 60],
    [60, 60, 33],
    [80, 80, 80],
];

fn pct(v: u32) -> u8 {
    ((v.min(100) * 255 + 50) / 100) as u8
}

/// DEC HLS (hue 0 = blue, 120 = red, 240 = green; L,S percent) to RGB.
fn hls(h: u32, l: u32, s: u32) -> [u8; 3] {
    let h = ((h % 360) + 240) % 360;
    let (l, s) = (l.min(100) as f32 / 100.0, s.min(100) as f32 / 100.0);
    if s == 0.0 {
        let g = (l * 255.0 + 0.5) as u8;
        return [g, g, g];
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let hk = h as f32 / 360.0;
    let ch = |t: f32| {
        let t = t.rem_euclid(1.0);
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (v * 255.0 + 0.5) as u8
    };
    [ch(hk + 1.0 / 3.0), ch(hk), ch(hk - 1.0 / 3.0)]
}

/// Streaming decoder. Memory is bounded by the pixel cap (at most about 2x during a
/// width-growth repack).
#[derive(Debug)]
pub struct SixelDecoder {
    limits: Limits,
    transparent: bool,
    init_px: [u8; 4],
    state: State,
    nums: [u32; 5],
    nn: usize,
    cur_num: u32,
    has_digit: bool,
    registers: Vec<[u8; 3]>,
    cur: usize,
    x: u32,
    band: u32,
    repeat: u32,
    stride: u32,
    rows: u32,
    pix: Vec<u8>,
    max_x: u32,
    written_rows: u32,
    pan: u32,
    pad: u32,
    bytes: usize,
}

impl SixelDecoder {
    pub fn new(params: SixelParams, limits: &Limits) -> Self {
        let n = limits.sixel_max_registers.clamp(2, 1024);
        let registers = match params.registers {
            Some(mut r) => {
                r.resize(n, [0, 0, 0]);
                r
            }
            None => {
                let mut r = vec![[0u8; 3]; n];
                for (i, c) in DEFAULT_PALETTE.iter().enumerate().take(n) {
                    r[i] = [
                        pct(u32::from(c[0])),
                        pct(u32::from(c[1])),
                        pct(u32::from(c[2])),
                    ];
                }
                r
            }
        };
        let (pan, pad) = match params.p1 {
            2 => (5, 1),
            3 | 4 => (3, 1),
            7..=9 => (1, 1),
            _ => (2, 1),
        };
        Self {
            limits: limits.clone(),
            transparent: params.transparent,
            init_px: if params.transparent {
                [0; 4]
            } else {
                params.background
            },
            state: State::Ground,
            nums: [0; 5],
            nn: 0,
            cur_num: 0,
            has_digit: false,
            registers,
            cur: 0,
            x: 0,
            band: 0,
            repeat: 1,
            stride: 0,
            rows: 0,
            pix: Vec::new(),
            max_x: 0,
            written_rows: 0,
            pan,
            pad,
            bytes: 0,
        }
    }

    /// Whether unwritten pixels are transparent (P2 = 1).
    pub fn is_transparent(&self) -> bool {
        self.transparent
    }

    pub fn feed(&mut self, data: &[u8]) -> Result<(), SixelError> {
        self.bytes = self.bytes.saturating_add(data.len());
        if self.bytes > self.limits.sixel_max_bytes {
            return Err(SixelError::TooManyBytes);
        }
        for &c in data {
            match self.state {
                State::Ground => self.dispatch(c)?,
                _ => match c {
                    b'0'..=b'9' => {
                        self.cur_num = (self.cur_num * 10 + u32::from(c - b'0')).min(1_000_000);
                        self.has_digit = true;
                    }
                    b';' => self.end_param(),
                    _ => {
                        self.end_param_if_any();
                        self.leave_state()?;
                        self.dispatch(c)?;
                    }
                },
            }
        }
        Ok(())
    }

    fn end_param(&mut self) {
        if self.nn < self.nums.len() {
            self.nums[self.nn] = self.cur_num;
        }
        self.nn = (self.nn + 1).min(self.nums.len() + 1);
        self.cur_num = 0;
        self.has_digit = false;
    }

    fn end_param_if_any(&mut self) {
        if self.has_digit || self.nn > 0 {
            self.end_param();
        }
    }

    fn start(&mut self, s: State) {
        self.state = s;
        self.nums = [0; 5];
        self.nn = 0;
        self.cur_num = 0;
        self.has_digit = false;
    }

    fn dispatch(&mut self, c: u8) -> Result<(), SixelError> {
        match c {
            b'"' => self.start(State::Raster),
            b'#' => self.start(State::Color),
            b'!' => self.start(State::Repeat),
            b'$' => {
                self.x = 0;
                self.repeat = 1;
            }
            b'-' => {
                self.x = 0;
                self.band = self.band.saturating_add(1);
                self.repeat = 1;
            }
            0x3F..=0x7E => {
                let n = std::mem::replace(&mut self.repeat, 1);
                self.draw(c - 0x3F, n)?;
            }
            _ => {} // CR, LF, spaces and unknown bytes are ignored
        }
        Ok(())
    }

    fn leave_state(&mut self) -> Result<(), SixelError> {
        let prev = std::mem::replace(&mut self.state, State::Ground);
        let n = self.nn.min(self.nums.len());
        match prev {
            State::Raster => {
                if n >= 2 && self.nums[1] > 0 && self.nums[0] > 0 {
                    self.pan = self.nums[0];
                    self.pad = self.nums[1];
                }
                if n >= 4 {
                    // Declared raster: cap-checked, never allocated.
                    let (w, h) = (self.nums[2], self.nums[3]);
                    if w > self.limits.max_axis
                        || h > self.limits.max_axis
                        || u64::from(w) * u64::from(h) > self.limits.sixel_max_pixels
                    {
                        return Err(SixelError::TooLarge);
                    }
                }
            }
            State::Color => {
                if n >= 1 {
                    let max = self.registers.len() - 1;
                    self.cur = (self.nums[0] as usize).min(max);
                }
                if n >= 5 {
                    let (a, b, c) = (self.nums[2], self.nums[3], self.nums[4]);
                    match self.nums[1] {
                        1 => self.registers[self.cur] = hls(a, b, c),
                        2 => self.registers[self.cur] = [pct(a), pct(b), pct(c)],
                        _ => {}
                    }
                }
            }
            State::Repeat => {
                self.repeat = if n >= 1 { self.nums[0].max(1) } else { 1 };
            }
            State::Ground => {}
        }
        Ok(())
    }

    fn ensure(&mut self, w: u32, h: u32) -> Result<(), SixelError> {
        if w <= self.stride && h <= self.rows {
            return Ok(());
        }
        if w > self.limits.max_axis || h > self.limits.max_axis {
            return Err(SixelError::TooLarge);
        }
        let cap = self.limits.sixel_max_pixels;
        let mut stride = self.stride;
        if w > stride {
            stride = stride
                .saturating_mul(2)
                .max(w)
                .min(self.limits.max_axis)
                .max(w);
            if u64::from(stride) * u64::from(h.max(self.rows)) > cap {
                stride = w;
            }
        }
        let rows = h.max(self.rows);
        if u64::from(stride) * u64::from(rows) > cap {
            return Err(SixelError::TooLarge);
        }
        let px = self.init_px;
        if stride != self.stride {
            let mut np = Vec::with_capacity(stride as usize * rows as usize * 4);
            for r in 0..self.rows as usize {
                let off = r * self.stride as usize * 4;
                np.extend_from_slice(&self.pix[off..off + self.stride as usize * 4]);
                for _ in self.stride..stride {
                    np.extend_from_slice(&px);
                }
            }
            self.pix = np;
            self.stride = stride;
        }
        for _ in self.rows..rows {
            for _ in 0..stride {
                self.pix.extend_from_slice(&px);
            }
        }
        self.rows = rows;
        Ok(())
    }

    fn draw(&mut self, bits: u8, n: u32) -> Result<(), SixelError> {
        let end_x = self.x.saturating_add(n);
        let y0 = self.band.saturating_mul(6);
        self.ensure(end_x, y0.saturating_add(6))?;
        let c = self.registers[self.cur];
        let rgba = [c[0], c[1], c[2], 255];
        for bit in 0..6u32 {
            if bits >> bit & 1 == 1 {
                let y = (y0 + bit) as usize;
                for xx in self.x..end_x {
                    let o = (y * self.stride as usize + xx as usize) * 4;
                    self.pix[o..o + 4].copy_from_slice(&rgba);
                }
                self.written_rows = self.written_rows.max(y0 + bit + 1);
            }
        }
        self.x = end_x;
        self.max_x = self.max_x.max(end_x);
        Ok(())
    }

    /// Finish at ST: crop to the drawn extent, trim trailing blank rows, apply the pixel
    /// aspect by integer row replication.
    pub fn finish(mut self) -> Result<SixelOutput, SixelError> {
        if self.state != State::Ground {
            self.end_param_if_any();
            self.leave_state()?;
        }
        if self.max_x == 0 || self.written_rows == 0 {
            return Err(SixelError::Empty);
        }
        let scale = (self.pan + self.pad / 2)
            .checked_div(self.pad)
            .unwrap_or(1)
            .max(1);
        let (w, h) = (self.max_x, self.written_rows);
        let oh = h.saturating_mul(scale);
        if oh > self.limits.max_axis || u64::from(w) * u64::from(oh) > self.limits.sixel_max_pixels
        {
            return Err(SixelError::TooLarge);
        }
        let mut rgba = Vec::with_capacity(w as usize * oh as usize * 4);
        for r in 0..h as usize {
            let off = r * self.stride as usize * 4;
            let row = &self.pix[off..off + w as usize * 4];
            for _ in 0..scale {
                rgba.extend_from_slice(row);
            }
        }
        Ok(SixelOutput {
            image: Image {
                width: w,
                height: oh,
                rgba,
            },
            registers: self.registers,
        })
    }
}
