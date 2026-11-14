// SPDX-License-Identifier: AGPL-3.0-only
//! Kitty animation (T-15 §1 R2, §4, §6; GRAPHICS-11, -12; PERFORMANCE-51; RENDERING-37).
//!
//! Frames are composed eagerly to full-size RGBA when they load (GRAPHICS-11 takes eager
//! composition) and charged to `graphics.animation-quota`. Frame 1 is the root image.
//!
//! Playback is a pure function of the clock: the state stores the frame shown and when it
//! started, and [`Animation::at`] walks the gaps to any later instant. Nothing here arms a
//! timer: the renderer asks for the frame at its own frame time and gets the next due time
//! back, which it hands to the frame scheduler (T-07 §2) -- so an occluded window, which
//! renders nothing, also advances nothing and wakes nobody.
//!
//! Semantics (published protocol): a new image is stopped on frame 1; `s=1` stops (and
//! resets the loop counter), `s=2` plays and waits at the last frame for more, `s=3` plays
//! and loops; `v=1` loops forever, `v=n` plays `n - 1` passes; a gap `<= 0` frame is skipped
//! instantly (gapless, never shown during playback); `c=` makes a frame current.

use crate::{ErrCode, Image};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Default gap of a new frame (ms).
pub const DEFAULT_GAP_MS: i32 = 40;

/// Frames per image (spec gap: the protocol names no cap; chosen to bound the walk).
pub const MAX_FRAMES: usize = 4096;

static NEXT_FRAME_SERIAL: AtomicU64 = AtomicU64::new(1 << 62);

/// A process-unique pixel serial for a composed frame (disjoint from blob serials).
pub fn frame_serial() -> u64 {
    NEXT_FRAME_SERIAL.fetch_add(1, Ordering::Relaxed)
}

/// One frame.
#[derive(Debug, Clone)]
pub struct Frame {
    pub image: Arc<Image>,
    /// Texture key for these pixels.
    pub serial: u64,
    /// Milliseconds this frame shows; `<= 0` is gapless.
    pub gap: i32,
    /// Pixels that differ from the previous frame (x, y, w, h); `None` = unknown/all.
    pub delta: Option<[u32; 4]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Stopped,
    /// `s=2`: play, then wait at the last frame for more.
    Loading,
    /// `s=3`: play and loop.
    Running,
}

/// Where playback is at an instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pos {
    pub index: usize,
    /// When the frame at `index` started showing (ms on the pane clock).
    pub shown_at: u64,
    pub loops_done: u32,
    pub state: State,
    /// The next frame change, if one is scheduled.
    pub due: Option<u64>,
    /// At the last frame in loading mode, its time already over.
    pub waiting: bool,
}

/// The frames of one image and its playback state.
#[derive(Debug, Clone)]
pub struct Animation {
    frames: Vec<Frame>,
    state: State,
    /// `v=`: 0/1 forever, `n` = `n - 1` passes.
    loops: u32,
    loops_done: u32,
    current: usize,
    shown_at: u64,
    /// Bytes of frames 2.. (frame 1 is charged as the image).
    bytes: u64,
}

impl Animation {
    /// An animation whose only frame is the root image (gap 0, stopped).
    pub fn new(root: Arc<Image>, serial: u64) -> Self {
        Self {
            frames: vec![Frame {
                image: root,
                serial,
                gap: 0,
                delta: None,
            }],
            state: State::Stopped,
            loops: 0,
            loops_done: 0,
            current: 0,
            shown_at: 0,
            bytes: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn frame(&self, i: usize) -> Option<&Frame> {
        self.frames.get(i)
    }

    pub(crate) fn frame_mut(&mut self, i: usize) -> Option<&mut Frame> {
        self.frames.get_mut(i)
    }

    pub fn frames(&self) -> &[Frame] {
        &self.frames
    }

    /// Bytes charged to the animation quota.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// Playback at `now` (pure).
    pub fn at(&self, now: u64) -> Pos {
        let n = self.frames.len();
        let mut p = Pos {
            index: self.current.min(n.saturating_sub(1)),
            shown_at: self.shown_at,
            loops_done: self.loops_done,
            state: self.state,
            due: None,
            waiting: false,
        };
        let period: u64 = self
            .frames
            .iter()
            .map(|f| u64::try_from(f.gap).unwrap_or(0))
            .sum();
        if n <= 1 || p.state == State::Stopped || period == 0 {
            return p;
        }
        let infinite = self.loops <= 1;
        // Each step moves one frame; a whole period is skipped at a wrap, so this bounds the
        // walk at a few passes over the frames.
        for _ in 0..4 * n + 4 {
            let g = u64::try_from(self.frames[p.index].gap).unwrap_or(0);
            let end = p.shown_at.saturating_add(g);
            if g > 0 && now < end {
                p.due = Some(end);
                return p;
            }
            if p.index + 1 < n {
                p.shown_at = end;
                p.index += 1;
                continue;
            }
            match p.state {
                State::Loading => {
                    p.waiting = true;
                    return p;
                }
                State::Running if !infinite && p.loops_done + 2 >= self.loops => {
                    p.state = State::Stopped;
                    return p;
                }
                _ => {
                    p.shown_at = end;
                    p.loops_done = p.loops_done.saturating_add(1);
                    p.index = 0;
                    if infinite && now > p.shown_at.saturating_add(period) {
                        p.shown_at += (now - p.shown_at) / period * period;
                    }
                }
            }
        }
        p
    }

    /// Fold the clock into the state (before any change).
    pub fn settle(&mut self, now: u64) -> Pos {
        let p = self.at(now);
        self.current = p.index;
        self.shown_at = p.shown_at;
        self.loops_done = p.loops_done;
        self.state = p.state;
        p
    }

    /// `a=a`: `state` 1/2/3, `loops` (`v`), gap edit of frame `edit` (1-based, `gap` 0 =
    /// none), current frame `current` (1-based, 0 = none).
    pub fn control(
        &mut self,
        state: u32,
        loops: u32,
        edit: u32,
        gap: i32,
        current: u32,
        now: u64,
    ) -> Result<(), ErrCode> {
        self.settle(now);
        if edit != 0 && gap != 0 {
            let f = self
                .frames
                .get_mut(edit as usize - 1)
                .ok_or(ErrCode::Enoent)?;
            f.gap = gap;
        }
        if current != 0 {
            if current as usize > self.frames.len() {
                return Err(ErrCode::Enoent);
            }
            self.current = current as usize - 1;
            self.shown_at = now;
        }
        if loops != 0 {
            self.loops = loops;
        }
        let next = match state {
            1 => Some(State::Stopped),
            2 => Some(State::Loading),
            3 => Some(State::Running),
            _ => None,
        };
        if let Some(s) = next {
            if s == State::Stopped {
                self.loops_done = 0;
            } else if self.state == State::Stopped {
                // Playback starts now from the current frame.
                self.shown_at = now;
                self.loops_done = 0;
            }
            self.state = s;
        }
        Ok(())
    }

    /// Append a composed frame. Over `quota` (bytes of frames 2..) is `ENOSPC`.
    pub fn push(&mut self, frame: Frame, quota: u64, now: u64) -> Result<u32, ErrCode> {
        if self.frames.len() >= MAX_FRAMES {
            return Err(ErrCode::Enospc);
        }
        let b = frame.image.bytes();
        if self.bytes + b > quota {
            return Err(ErrCode::Enospc);
        }
        let p = self.settle(now);
        if p.waiting {
            // Loading mode sat at the last frame: the new one starts now.
            let g = u64::try_from(self.frames[p.index].gap).unwrap_or(0);
            self.shown_at = now.saturating_sub(g);
        }
        self.bytes += b;
        self.frames.push(frame);
        Ok(self.frames.len() as u32)
    }

    /// Replace frame `index` (0-based) with new pixels that changed inside `rect`; the next
    /// frame's delta widens by the same rect. Frame 0 is the root: the caller re-charges it.
    pub fn replace(&mut self, index: usize, frame: Frame, rect: Option<[u32; 4]>) {
        let Some(old) = self.frames.get(index) else {
            return;
        };
        if index > 0 {
            self.bytes = self.bytes - old.image.bytes() + frame.image.bytes();
        }
        let gap = old.gap;
        let delta = if index == 0 {
            None
        } else {
            widen(old.delta, rect)
        };
        self.frames[index] = Frame {
            gap,
            delta,
            ..frame
        };
        if let Some(next) = self.frames.get_mut(index + 1) {
            next.delta = widen(next.delta, rect);
        }
    }

    /// `d=f`/`d=F`: drop every frame but the root and stop.
    pub fn clear_frames(&mut self) {
        self.frames.truncate(1);
        self.bytes = 0;
        self.state = State::Stopped;
        self.current = 0;
        self.loops_done = 0;
    }
}

/// The union of two known change rects; unknown stays unknown.
fn widen(a: Option<[u32; 4]>, b: Option<[u32; 4]>) -> Option<[u32; 4]> {
    match (a, b) {
        (Some(a), Some(b)) => {
            let x0 = a[0].min(b[0]);
            let y0 = a[1].min(b[1]);
            let x1 = (a[0] + a[2]).max(b[0] + b[2]);
            let y1 = (a[1] + a[3]).max(b[1] + b[3]);
            Some([x0, y0, x1 - x0, y1 - y0])
        }
        _ => None,
    }
}

/// Composite `w`x`h` pixels of `src` at (`sx`, `sy`) onto `dst` at (`dx`, `dy`): source-over
/// in straight alpha, or a plain overwrite. The caller has bounds-checked both rects.
#[allow(clippy::too_many_arguments)]
pub fn composite(
    dst: &mut Image,
    src: &Image,
    sx: u32,
    sy: u32,
    dx: u32,
    dy: u32,
    w: u32,
    h: u32,
    overwrite: bool,
) {
    let (dw, sw) = (dst.width as usize, src.width as usize);
    for row in 0..h as usize {
        let s0 = ((sy as usize + row) * sw + sx as usize) * 4;
        let d0 = ((dy as usize + row) * dw + dx as usize) * 4;
        let n = w as usize * 4;
        let s = &src.rgba[s0..s0 + n];
        let d = &mut dst.rgba[d0..d0 + n];
        if overwrite {
            d.copy_from_slice(s);
            continue;
        }
        for (dp, sp) in d.chunks_exact_mut(4).zip(s.chunks_exact(4)) {
            let sa = u32::from(sp[3]);
            if sa == 255 {
                dp.copy_from_slice(sp);
            } else if sa != 0 {
                let da = u32::from(dp[3]);
                // out_a = sa + da (1 - sa), in 0..=255 units.
                let oa = sa * 255 + da * (255 - sa);
                if oa == 0 {
                    dp.copy_from_slice(&[0, 0, 0, 0]);
                    continue;
                }
                for c in 0..3 {
                    let v = u32::from(sp[c]) * sa * 255 + u32::from(dp[c]) * da * (255 - sa);
                    dp[c] = ((v + oa / 2) / oa) as u8;
                }
                dp[3] = ((oa + 127) / 255) as u8;
            }
        }
    }
}

/// True when `x, y, w, h` lies inside a `iw` x `ih` image.
pub fn rect_inside(x: u32, y: u32, w: u32, h: u32, iw: u32, ih: u32) -> bool {
    u64::from(x) + u64::from(w) <= u64::from(iw) && u64::from(y) + u64::from(h) <= u64::from(ih)
}

/// The bounding box of the pixels where `a` and `b` (same size) differ.
pub fn diff_bbox(a: &Image, b: &Image) -> Option<[u32; 4]> {
    if a.width != b.width || a.height != b.height {
        return None;
    }
    let w = a.width as usize;
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for (y, (ra, rb)) in a
        .rgba
        .chunks_exact(w * 4)
        .zip(b.rgba.chunks_exact(w * 4))
        .enumerate()
    {
        if ra == rb {
            continue;
        }
        let first = ra
            .chunks_exact(4)
            .zip(rb.chunks_exact(4))
            .position(|(p, q)| p != q)
            .unwrap_or(0);
        let last = w
            - 1
            - ra.chunks_exact(4)
                .rev()
                .zip(rb.chunks_exact(4).rev())
                .position(|(p, q)| p != q)
                .unwrap_or(0);
        x0 = x0.min(first);
        x1 = x1.max(last + 1);
        y0 = y0.min(y);
        y1 = y + 1;
    }
    // No difference: an empty rect at the origin (nothing to upload).
    if x0 == usize::MAX {
        return Some([0, 0, 0, 0]);
    }
    Some([x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(px: [u8; 4]) -> Arc<Image> {
        Arc::new(Image {
            width: 2,
            height: 1,
            rgba: px.repeat(2),
        })
    }

    fn anim(gaps: &[i32]) -> Animation {
        let mut a = Animation::new(img([0; 4]), 1);
        a.frames[0].gap = gaps[0];
        for (i, &g) in gaps.iter().enumerate().skip(1) {
            a.push(
                Frame {
                    image: img([i as u8; 4]),
                    serial: 1 + i as u64,
                    gap: g,
                    delta: None,
                },
                u64::MAX,
                0,
            )
            .unwrap();
        }
        a
    }

    #[test]
    fn a_new_image_is_stopped_on_frame_one() {
        let a = anim(&[10, 10]);
        assert_eq!(a.at(1000).index, 0);
        assert_eq!(a.at(1000).due, None);
    }

    #[test]
    fn looping_walks_gaps_and_skips_gapless_frames() {
        let mut a = anim(&[10, -1, 20]);
        a.control(3, 0, 0, 0, 0, 100).unwrap();
        assert_eq!(a.at(105).index, 0);
        assert_eq!(a.at(105).due, Some(110));
        // Frame 2 is gapless: straight to frame 3 at 110.
        assert_eq!(a.at(110).index, 2);
        assert_eq!(a.at(129).index, 2);
        assert_eq!(a.at(130).index, 0);
        // Far in the future the walk is bounded and lands on the right frame.
        let p = a.at(100 + 30 * 1_000_000 + 15);
        assert_eq!(p.index, 2);
    }

    #[test]
    fn loop_count_plays_n_minus_one_passes_then_stops_on_the_last_frame() {
        let mut a = anim(&[10, 10]);
        a.control(3, 3, 0, 0, 0, 0).unwrap(); // v=3: two passes
        assert_eq!(a.at(25).index, 0); // second pass
        let p = a.at(1000);
        assert_eq!((p.index, p.state), (1, State::Stopped));
    }

    #[test]
    fn loading_waits_at_the_last_frame_and_a_new_frame_starts_on_arrival() {
        let mut a = anim(&[10, 10]);
        a.control(2, 0, 0, 0, 0, 0).unwrap();
        let p = a.at(500);
        assert!(p.waiting);
        assert_eq!(p.index, 1);
        a.push(
            Frame {
                image: img([9; 4]),
                serial: 99,
                gap: 30,
                delta: None,
            },
            u64::MAX,
            500,
        )
        .unwrap();
        assert_eq!(a.at(500).index, 2);
        assert_eq!(a.at(500).due, Some(530));
        assert_eq!(a.at(529).index, 2);
        assert!(a.at(600).waiting, "waiting again at the new last frame");
    }

    #[test]
    fn stop_and_current_frame_and_gap_edit() {
        let mut a = anim(&[10, 10, 10]);
        a.control(3, 0, 0, 0, 0, 0).unwrap();
        a.control(1, 0, 0, 0, 2, 15).unwrap();
        assert_eq!(a.at(10_000).index, 1);
        assert_eq!(a.control(0, 0, 0, 0, 9, 20), Err(ErrCode::Enoent));
        a.control(0, 0, 1, 500, 0, 20).unwrap();
        assert_eq!(a.frames()[0].gap, 500);
    }

    #[test]
    fn quota_and_blend() {
        let mut a = anim(&[10]);
        let f = Frame {
            image: img([1; 4]),
            serial: 5,
            gap: 10,
            delta: None,
        };
        assert_eq!(a.push(f, 4, 0), Err(ErrCode::Enospc));
        let mut d = Image {
            width: 1,
            height: 1,
            rgba: vec![0, 0, 255, 255],
        };
        let s = Image {
            width: 1,
            height: 1,
            rgba: vec![255, 0, 0, 128],
        };
        composite(&mut d, &s, 0, 0, 0, 0, 1, 1, false);
        assert_eq!(d.rgba[3], 255);
        assert!(d.rgba[0].abs_diff(128) <= 1 && d.rgba[2].abs_diff(127) <= 1);
        composite(&mut d, &s, 0, 0, 0, 0, 1, 1, true);
        assert_eq!(d.rgba, vec![255, 0, 0, 128]);
    }

    #[test]
    fn diff_box_bounds_the_changes() {
        let a = Image {
            width: 3,
            height: 2,
            rgba: vec![0; 24],
        };
        let mut b = a.clone();
        b.rgba[(3 + 1) * 4] = 7; // (1, 1)
        assert_eq!(diff_bbox(&a, &b), Some([1, 1, 1, 1]));
        assert_eq!(diff_bbox(&a, &a), Some([0, 0, 0, 0]));
    }
}
