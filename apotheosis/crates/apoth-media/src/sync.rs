// SPDX-License-Identifier: AGPL-3.0-only
//! A/V sync (T-16 §3, MEDIA-10): audio is the master clock when there is an audio stream;
//! otherwise a monotonic frame clock is. Video shows the latest frame whose PTS is at most
//! the clock plus one refresh; older frames are dropped; with nothing due the current frame
//! holds (an audio underrun stops the clock, so video holds with it).

use std::time::Instant;

use crate::sink::AudioSink;

/// One display refresh at 60 Hz (µs): how far ahead of the clock a frame may be shown.
pub const REFRESH_US: i64 = 16_667;

/// The master clock in media time (µs from the start of the stream).
#[derive(Debug)]
pub struct Clock {
    /// Media time at the anchor.
    base_us: i64,
    /// Frame clock: when `base_us` was current; `None` while paused.
    since: Option<Instant>,
    /// Audio master: media time of the first PCM since the last flush.
    audio_base: Option<i64>,
    audio: bool,
}

impl Clock {
    pub fn new(audio: bool) -> Clock {
        Clock {
            base_us: 0,
            since: None,
            audio_base: None,
            audio,
        }
    }

    pub fn audio_master(&self) -> bool {
        self.audio
    }

    /// The audio sink went away: the frame clock takes over from `now_us`.
    pub fn drop_audio(&mut self, now_us: i64, playing: bool) {
        self.audio = false;
        self.base_us = now_us;
        self.since = playing.then(Instant::now);
    }

    /// PCM with media time `pts_us` is the first queued since the last restart.
    pub fn audio_started_at(&mut self, pts_us: i64) {
        if self.audio_base.is_none() {
            self.audio_base = Some(pts_us);
        }
    }

    pub fn play(&mut self) {
        if self.since.is_none() {
            self.since = Some(Instant::now());
        }
    }

    pub fn pause(&mut self, sink: Option<&dyn AudioSink>) {
        self.base_us = self.now_us(sink);
        self.since = None;
    }

    /// Restart at `pts_us` (seek, loop).
    pub fn restart(&mut self, pts_us: i64, playing: bool) {
        self.base_us = pts_us;
        self.since = playing.then(Instant::now);
        self.audio_base = None;
    }

    /// The media time now.
    pub fn now_us(&self, sink: Option<&dyn AudioSink>) -> i64 {
        if self.audio {
            let played = sink.and_then(|s| s.played_us());
            return match (self.audio_base, played) {
                (Some(b), Some(p)) => b + p as i64,
                // Before the first sample plays the clock stands at its start.
                _ => self.base_us,
            };
        }
        match self.since {
            Some(t) => self.base_us + t.elapsed().as_micros() as i64,
            None => self.base_us,
        }
    }
}

/// What to do with the queued frames (sorted by PTS) at clock `now_us`: the index of the
/// frame to show (if one is due) and how many leading frames are consumed (the shown one
/// and every late frame before it).
pub fn pick(pts: &[i64], now_us: i64) -> (Option<usize>, usize) {
    let due = pts
        .iter()
        .take_while(|&&p| p <= now_us + REFRESH_US)
        .count();
    if due == 0 {
        (None, 0)
    } else {
        (Some(due - 1), due)
    }
}

/// When the next queued frame is due, relative to `now_us` (µs; 0 = now).
pub fn wait_us(next_pts: Option<i64>, now_us: i64) -> Option<i64> {
    next_pts.map(|p| (p - REFRESH_US - now_us).max(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::FakeSink;

    #[test]
    fn latest_due_frame_wins_and_late_ones_drop() {
        let pts = [0, 33_000, 66_000, 100_000];
        assert_eq!(pick(&pts, -20_000), (None, 0));
        assert_eq!(pick(&pts, 0), (Some(0), 1));
        // 70 ms: 0, 33 and 66 are due; 66 shows, 0 and 33 are dropped.
        assert_eq!(pick(&pts, 70_000), (Some(2), 3));
        assert_eq!(pick(&[], 5), (None, 0));
        assert_eq!(
            wait_us(Some(100_000), 50_000),
            Some(100_000 - REFRESH_US - 50_000)
        );
        assert_eq!(wait_us(None, 0), None);
    }

    #[test]
    fn audio_master_follows_the_sink() {
        let sink = FakeSink::default();
        let mut c = Clock::new(true);
        c.restart(0, true);
        assert_eq!(c.now_us(Some(&sink)), 0);
        c.audio_started_at(1_000_000);
        sink.state.lock().unwrap().played_us = Some(250_000);
        assert_eq!(c.now_us(Some(&sink)), 1_250_000);
        // An underrun: the sink's position stops, and so does the clock.
        assert_eq!(c.now_us(Some(&sink)), 1_250_000);
    }

    /// T-16 §6: a synthetic clip (a flash every second, a click at the same media time) and
    /// a fake sink clock; the frame on screen when each click plays is within 40 ms.
    #[test]
    fn av_skew_stays_within_40ms() {
        let sink = FakeSink::default();
        let mut c = Clock::new(true);
        c.restart(0, true);
        c.audio_started_at(0);
        // 30 fps frames; flash frames at whole seconds.
        let mut queue: Vec<i64> = (0..300).map(|i| i * 1_000_000 / 30).collect();
        let mut shown: Option<i64> = None;
        let mut worst = 0i64;
        // The display ticks at 60 Hz with jitter; the audio clock advances in 10 ms
        // steps (the sink's reporting granularity) plus a constant 5 ms latency error.
        let mut t = 0i64;
        while t < 9_500_000 {
            sink.state.lock().unwrap().played_us = Some((t / 10_000 * 10_000 + 5_000) as u64);
            let now = c.now_us(Some(&sink));
            let (show, used) = pick(&queue, now);
            if let Some(i) = show {
                shown = Some(queue[i]);
            }
            queue.drain(..used);
            // A click at each whole second of audio: compare with the flash on screen.
            if t % 1_000_000 < 16_667
                && t > 0
                && let Some(s) = shown
            {
                worst = worst.max((s - t).abs());
            }
            t += 16_667 + (t / 16_667 % 3) * 700;
        }
        assert!(worst <= 40_000, "worst skew {worst} µs");
    }
}
