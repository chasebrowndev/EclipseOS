// SPDX-License-Identifier: AGPL-3.0-only
//! What Oracle-Eyes says it is doing, for the eclipse mark (ADR 0055).
//!
//! Oracle-Eyes writes one word per line on its own socket; the compositor is
//! not involved and must not be. Anything but a live connection saying
//! `watch` or `think` is [`Eye::Off`] — a daemon that is gone, never ran, or
//! said something unexpected leaves the plain eclipse, never a staring eye.

use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use eclipse_ui::tokens::bar;
use iced::Subscription;

use crate::app::Message;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Eye {
    /// The plain eclipse.
    #[default]
    Off,
    /// Automatic mode is on and between passes: the pupil wanders.
    Watch,
    /// A model call is in flight: the pupil pinpoints.
    Think,
}

impl Eye {
    pub fn parse(word: &str) -> Eye {
        match word.trim() {
            "watch" => Eye::Watch,
            "think" => Eye::Think,
            _ => Eye::Off,
        }
    }
}

/// The mark's animated geometry: where the pupil is and how wide it is.
///
/// A small state machine rather than a function of time, because the view has
/// to stay a pure function of [`App`](crate::app::App): [`Iris::tick`] samples
/// the tweens into [`Iris::pupil`] and [`Iris::offset`] and the view only
/// reads those. Between darts nothing is in flight and nothing ticks — the
/// next dart is a single deadline ([`Iris::dart_at`]), not a frame clock.
#[derive(Debug, Clone)]
pub struct Iris {
    /// What the beacon last said.
    pub eye: Eye,
    /// Pupil radius, in logical pixels, as of the last tick.
    pub pupil: f32,
    /// Pupil centre relative to the iris centre, in logical pixels.
    pub offset: (f32, f32),
    radius: Tween,
    x: Tween,
    y: Tween,
    dart_at: Option<Instant>,
    rng: u64,
}

impl Default for Iris {
    fn default() -> Self {
        let off = pupil_for(Eye::Off);
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        Iris {
            eye: Eye::Off,
            pupil: off,
            offset: (0.0, 0.0),
            radius: Tween::rest(off),
            x: Tween::rest(0.0),
            y: Tween::rest(0.0),
            dart_at: None,
            // xorshift has one fixed point, zero; never seed it there.
            rng: seed | 1,
        }
    }
}

/// The iris's radius: the corona's outer edge, fixed in every state.
pub fn outer() -> f32 {
    bar::EYE_DISC / 2.0
}

/// The pupil radius each state settles at. Off is the ring's own hole, so a
/// settled Off is the plain eclipse.
fn pupil_for(eye: Eye) -> f32 {
    match eye {
        Eye::Off => outer() - bar::RING,
        Eye::Watch => outer() * bar::EYE_PUPIL,
        Eye::Think => bar::EYE_PINPOINT,
    }
}

impl Iris {
    /// Settled on the plain eclipse: the view may draw the ring itself.
    pub fn is_plain(&self) -> bool {
        self.eye == Eye::Off && self.radius.to == self.pupil && self.offset == (0.0, 0.0)
    }

    /// A dart or a resize is in flight: the frame clock must run.
    pub fn animating(&self) -> bool {
        self.radius.live() || self.x.live() || self.y.live()
    }

    /// When the held pupil next darts, if it is holding.
    pub fn dart_at(&self) -> Option<Instant> {
        self.dart_at
    }

    /// The beacon said something. Retarget from wherever the pupil is now,
    /// so a state change mid-dart does not jump.
    pub fn set(&mut self, eye: Eye, now: Instant) {
        if eye == self.eye {
            return;
        }
        self.eye = eye;
        let resize = Duration::from_millis(bar::EYE_RESIZE_MS);
        self.radius = Tween::new(self.pupil, pupil_for(eye), now, resize);
        self.dart_at = None;
        if eye == Eye::Watch {
            // Open where it is, then glance once the pupil has opened.
            self.dart_at = Some(now + resize);
        } else {
            // Thinking and the plain eclipse both look straight ahead.
            self.x = Tween::new(self.offset.0, 0.0, now, resize);
            self.y = Tween::new(self.offset.1, 0.0, now, resize);
        }
        self.sample(now);
    }

    /// One frame, or the hold deadline arriving.
    pub fn tick(&mut self, now: Instant) {
        if self.eye == Eye::Watch && self.dart_at.is_some_and(|t| now >= t) {
            self.dart_at = None;
            self.dart(now);
        }
        self.sample(now);
        if self.eye == Eye::Watch && !self.animating() && self.dart_at.is_none() {
            let span = bar::EYE_HOLD_MAX_MS - bar::EYE_HOLD_MIN_MS;
            let hold = bar::EYE_HOLD_MIN_MS + self.next() % (span + 1);
            self.dart_at = Some(now + Duration::from_millis(hold));
        }
    }

    /// Glance somewhere near the edge of the wander disc, or now and then
    /// back to the middle.
    fn dart(&mut self, now: Instant) {
        let (tx, ty) = if self.next() % bar::EYE_RECENTRE == 0 && self.offset != (0.0, 0.0) {
            (0.0, 0.0)
        } else {
            let reach = outer() * bar::EYE_WANDER;
            let angle = self.unit() * std::f32::consts::PI;
            let span = 1.0 - bar::EYE_GLANCE;
            let dist = reach * (bar::EYE_GLANCE + span * (self.unit() + 1.0) / 2.0);
            (dist * angle.cos(), dist * angle.sin())
        };
        let dart = Duration::from_millis(bar::EYE_DART_MS);
        self.x = Tween::new(self.offset.0, tx, now, dart);
        self.y = Tween::new(self.offset.1, ty, now, dart);
    }

    fn sample(&mut self, now: Instant) {
        self.pupil = self.radius.at(now, ease_in_out);
        self.offset = (self.x.at(now, ease_out), self.y.at(now, ease_out));
    }

    fn next(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        x
    }

    /// Uniform in [-1, 1].
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
    }
}

/// One eased value from `from` to `to`. Lands exactly on `to` and then stops
/// being live, which is what lets the frame clock go away.
#[derive(Debug, Clone, Copy)]
struct Tween {
    from: f32,
    to: f32,
    start: Option<Instant>,
    dur: Duration,
}

impl Tween {
    fn rest(v: f32) -> Tween {
        Tween {
            from: v,
            to: v,
            start: None,
            dur: Duration::ZERO,
        }
    }

    fn new(from: f32, to: f32, now: Instant, dur: Duration) -> Tween {
        if from == to || dur.is_zero() {
            return Tween::rest(to);
        }
        Tween {
            from,
            to,
            start: Some(now),
            dur,
        }
    }

    fn live(&self) -> bool {
        self.start.is_some()
    }

    fn at(&mut self, now: Instant, ease: fn(f32) -> f32) -> f32 {
        let Some(start) = self.start else {
            return self.to;
        };
        let t = now.saturating_duration_since(start).as_secs_f32() / self.dur.as_secs_f32();
        if t >= 1.0 {
            self.start = None;
            return self.to;
        }
        self.from + (self.to - self.from) * ease(t)
    }
}

/// A saccade: fast off the mark, settling onto the target.
fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// An opening or narrowing: no snap at either end.
fn ease_in_out(t: f32) -> f32 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// A frame clock for the mark, alive only while [`Iris::animating`].
pub fn frames() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(8, async move |mut sender| {
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_millis(bar::EYE_FRAME_MS));
                if !crate::app::send(&mut sender, Message::EyeTick) {
                    return;
                }
            });
        })
    })
}

/// One tick at `at`: the end of a hold. Keyed by the deadline, so a new hold
/// replaces the old subscription and a dropped one never fires.
pub fn after(at: Instant) -> Subscription<Message> {
    Subscription::run_with(at, |at| {
        let at = *at;
        iced::stream::channel(1, async move |mut sender| {
            std::thread::spawn(move || {
                std::thread::sleep(at.saturating_duration_since(Instant::now()));
                let _ = crate::app::send(&mut sender, Message::EyeTick);
            });
        })
    })
}

/// The eye surface's layer-shell namespace. Abyss's capture policy names it
/// (`hide-layer "hyperion:eclipse-eye"`, ADR 0056), so it must not drift.
pub const NAMESPACE: &str = "eclipse-eye";

/// How long to wait before reconnecting to a daemon that is not there.
const RETRY: Duration = Duration::from_secs(2);

fn socket() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("oracle-eyes").join("eye.sock"))
}

/// The beacon's words as messages, for as long as `bar.eye` keeps this
/// subscription alive.
pub fn watch() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(8, async move |mut sender| {
            std::thread::spawn(move || loop {
                if let Some(path) = socket() {
                    if let Ok(stream) = UnixStream::connect(&path) {
                        for line in BufReader::new(stream).lines() {
                            let Ok(line) = line else { break };
                            if !crate::app::send(&mut sender, Message::Eye(Eye::parse(&line))) {
                                return;
                            }
                        }
                    }
                }
                // Lost, refused or absent: the eclipse goes back to a ring.
                if !crate::app::send(&mut sender, Message::Eye(Eye::Off)) {
                    return;
                }
                std::thread::sleep(RETRY);
            });
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settle(iris: &mut Iris, mut now: Instant) -> Instant {
        while iris.animating() {
            now += Duration::from_millis(bar::EYE_FRAME_MS);
            iris.tick(now);
        }
        now
    }

    #[test]
    fn off_is_the_plain_ring_and_costs_nothing() {
        let iris = Iris::default();
        assert!(iris.is_plain());
        assert!(!iris.animating());
        assert_eq!(iris.dart_at(), None);
        assert_eq!(iris.pupil, outer() - bar::RING);
    }

    #[test]
    fn watch_opens_holds_and_darts_inside_the_iris() {
        let mut iris = Iris::default();
        let now = Instant::now();
        iris.set(Eye::Watch, now);
        assert!(iris.animating());
        settle(&mut iris, now);
        assert_eq!(iris.pupil, outer() * bar::EYE_PUPIL);
        for _ in 0..50 {
            // Holding: no frame clock, one deadline.
            let at = iris.dart_at().expect("a held pupil schedules its next dart");
            assert!(!iris.animating());
            iris.tick(at);
            assert!(iris.animating(), "the deadline starts a dart");
            settle(&mut iris, at);
            let (x, y) = iris.offset;
            assert!((x * x + y * y).sqrt() <= outer() * bar::EYE_WANDER + 1e-3);
        }
        // At full reach the pupil stays inside the plain ring's hole.
        assert!(outer() * (bar::EYE_PUPIL + bar::EYE_WANDER) <= outer() - bar::RING);
    }

    #[test]
    fn think_pinpoints_centred_and_off_returns_to_the_ring() {
        let mut iris = Iris::default();
        let mut now = Instant::now();
        iris.set(Eye::Watch, now);
        now = settle(&mut iris, now);
        iris.tick(iris.dart_at().unwrap());
        iris.set(Eye::Think, now);
        now = settle(&mut iris, now);
        assert_eq!(iris.pupil, bar::EYE_PINPOINT);
        assert_eq!(iris.offset, (0.0, 0.0));
        assert_eq!(iris.dart_at(), None);
        iris.set(Eye::Off, now);
        settle(&mut iris, now);
        assert!(iris.is_plain());
        assert_eq!(iris.dart_at(), None);
    }

    #[test]
    fn only_the_two_live_words_open_the_eye() {
        assert_eq!(Eye::parse("watch\n"), Eye::Watch);
        assert_eq!(Eye::parse("think"), Eye::Think);
        assert_eq!(Eye::parse("off"), Eye::Off);
        assert_eq!(Eye::parse("WATCH"), Eye::Off);
        assert_eq!(Eye::parse(""), Eye::Off);
    }
}
