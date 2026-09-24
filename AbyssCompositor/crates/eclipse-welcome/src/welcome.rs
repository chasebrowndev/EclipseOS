// SPDX-License-Identifier: AGPL-3.0-only
//! The welcome step's state, messages and wiring.
//!
//! The animation itself is [`crate::timeline`]; this holds the one thing that
//! is not a function of time: whether Space has been pressed, and when. Time
//! reaches it only as frame ticks, and everything drawn is recomputed from the
//! elapsed time on every `view`, so a frozen instant (`--at`) and a live run go
//! through exactly the same code.

use crate::draw::{lit, Stage};
use crate::fonts::Faces;
use crate::timeline::{self, Frame};
use iced::widget::canvas::{self, Cache};
use iced::widget::image::Handle;
use iced::{event, keyboard, mouse, touch, window, Element, Length, Subscription, Task};
use std::time::{Duration, Instant};

const LOGO_PNG: &[u8] = include_bytes!("../assets/eclipseos-logo.png");

/// Environment switch for reduced motion, per the spec's `EclipseOS_` prefix.
pub const REDUCED_MOTION_ENV: &str = "EclipseOS_REDUCED_MOTION";

#[derive(Debug, Clone)]
pub enum Message {
    /// A frame tick from the window.
    Tick(Instant),
    /// Space or a pointer press. Ignored until the prompt is ready.
    Press,
    /// The fade to black has finished: the hand-off. Emitted exactly once, and
    /// never before [`timeline::READY`]. An embedding step advances on it;
    /// the standalone binary exits 0.
    Begin,
}

/// A pinned instant, for `--at` and for tests: the clock is not consulted.
#[derive(Debug, Clone, Copy)]
struct Frozen {
    t: f32,
    fade: f32,
}

pub struct Welcome {
    label: String,
    reduced: bool,
    faces: Faces,
    logo: Handle,
    logo_aspect: f32,
    background: Cache,
    /// The first frame's instant; wall-clock zero.
    start: Option<Instant>,
    elapsed: Duration,
    /// Wall-clock time of the press that began the exit.
    exit_from: Option<Duration>,
    handed_off: bool,
    frozen: Option<Frozen>,
}

/// True when `EclipseOS_REDUCED_MOTION=1` is set.
pub fn reduced_motion_from_env() -> bool {
    std::env::var(REDUCED_MOTION_ENV).is_ok_and(|v| v == "1")
}

/// The logo, with its alpha remapped for linear-light blending (see
/// `draw::lit`): its soft glow is the one translucent thing in the PNG.
fn load_logo() -> (Handle, f32) {
    match image::load_from_memory_with_format(LOGO_PNG, image::ImageFormat::Png) {
        Ok(img) => {
            let mut rgba = img.to_rgba8();
            for px in rgba.pixels_mut() {
                px.0[3] = (lit(f32::from(px.0[3]) / 255.0) * 255.0).round() as u8;
            }
            let (w, h) = rgba.dimensions();
            (Handle::from_rgba(w, h, rgba.into_raw()), h as f32 / w as f32)
        }
        // The asset is compiled in and checked by a test; this is unreachable
        // in a build that passed it, but a welcome screen must not panic.
        Err(_) => (Handle::from_bytes(LOGO_PNG), 144.0 / 796.0),
    }
}

impl Welcome {
    /// `version` is the label's number: it renders as `Version {version}`.
    /// Pass `env!("CARGO_PKG_VERSION")` of the embedding binary, or
    /// [`Welcome::default_version`] for this crate's own.
    pub fn new(version: impl AsRef<str>) -> Welcome {
        let (logo, logo_aspect) = load_logo();
        Welcome {
            label: format!("Version {}", version.as_ref()),
            reduced: false,
            faces: Faces::detect(),
            logo,
            logo_aspect,
            background: Cache::default(),
            start: None,
            elapsed: Duration::ZERO,
            exit_from: None,
            handed_off: false,
            frozen: None,
        }
    }

    /// This crate's own version, the default label.
    pub fn default_version() -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    /// Reduced motion: jump to `t = 20`, no keycap pulse.
    pub fn reduced_motion(mut self, on: bool) -> Welcome {
        self.reduced = on;
        self
    }

    /// Pin the screen at animation time `t` with the exit fade at `fade`
    /// (`0..=1`). No clock, no input: a still frame for screenshots.
    pub fn frozen(mut self, t: f32, fade: f32) -> Welcome {
        self.frozen = Some(Frozen {
            t,
            fade: fade.clamp(0.0, 1.0),
        });
        self
    }

    /// Animation time, seconds.
    pub fn t(&self) -> f32 {
        match self.frozen {
            Some(f) => f.t,
            None => timeline::anim_time(self.elapsed.as_secs_f32(), self.reduced),
        }
    }

    /// The exit fade, `0..=1`.
    pub fn fade(&self) -> f32 {
        if let Some(f) = self.frozen {
            return f.fade;
        }
        match self.exit_from {
            Some(from) => timeline::fade(self.elapsed.saturating_sub(from).as_secs_f32() * 1000.0),
            None => 0.0,
        }
    }

    /// Space would be accepted now.
    pub fn is_ready(&self) -> bool {
        timeline::is_ready(self.t())
    }

    /// True once Space has been accepted and the fade is running or done.
    pub fn is_leaving(&self) -> bool {
        self.exit_from.is_some()
    }

    /// Whether the clock has to keep ticking. Under reduced motion nothing
    /// moves until the exit begins, and a frozen or finished screen never does.
    fn wants_frames(&self) -> bool {
        self.frozen.is_none() && !self.handed_off && (!self.reduced || self.exit_from.is_some())
    }

    /// A frame at wall-clock `at`. Returns true exactly once: when the fade
    /// has completed and the hand-off is due.
    pub fn advance(&mut self, at: Instant) -> bool {
        if self.frozen.is_some() {
            return false;
        }
        let start = *self.start.get_or_insert(at);
        self.elapsed = at.saturating_duration_since(start);
        if self.exit_from.is_some() && self.fade() >= 1.0 && !self.handed_off {
            self.handed_off = true;
            return true;
        }
        false
    }

    /// Space or a press. Only counts once [`Welcome::is_ready`], and only the
    /// first time.
    pub fn press(&mut self) {
        if self.frozen.is_none() && self.is_ready() && self.exit_from.is_none() {
            self.exit_from = Some(self.elapsed);
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tick(at) => {
                if self.advance(at) {
                    return Task::done(Message::Begin);
                }
            }
            Message::Press => self.press(),
            Message::Begin => {}
        }
        Task::none()
    }

    pub fn view(&self) -> Element<'_, Message> {
        canvas::Canvas::new(Stage {
            frame: Frame::at(self.t(), self.reduced),
            fade: self.fade(),
            label: &self.label,
            faces: &self.faces,
            logo: &self.logo,
            logo_aspect: self.logo_aspect,
            background: &self.background,
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        if self.frozen.is_some() || self.handed_off {
            return Subscription::none();
        }
        let input = event::listen_with(|event, _status, _window| match event {
            iced::Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Space),
                repeat: false,
                ..
            })
            | iced::Event::Mouse(mouse::Event::ButtonPressed(_))
            | iced::Event::Touch(touch::Event::FingerPressed { .. }) => Some(Message::Press),
            _ => None,
        });
        if self.wants_frames() {
            Subscription::batch([input, window::frames().map(Message::Tick)])
        } else {
            input
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn welcome() -> Welcome {
        Welcome::new("9.9.9")
    }

    /// Tick at `secs` wall-clock seconds after `t0`.
    fn tick(w: &mut Welcome, t0: Instant, secs: f32) -> bool {
        w.advance(t0 + Duration::from_secs_f32(secs))
    }

    /// Wall-clock seconds at which animation time reaches `t`.
    fn wall(t: f32) -> f32 {
        t / timeline::SPEED
    }

    #[test]
    fn the_logo_asset_decodes() {
        let (_, aspect) = load_logo();
        assert!((aspect - 144.0 / 796.0).abs() < 1e-6);
    }

    #[test]
    fn the_label_is_the_version_passed_in() {
        assert_eq!(welcome().label, "Version 9.9.9");
        assert_eq!(
            Welcome::new(Welcome::default_version()).label,
            format!("Version {}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn a_press_before_ready_does_nothing() {
        let (mut w, t0) = (welcome(), Instant::now());
        tick(&mut w, t0, 0.0);
        for secs in [1.0, 5.0, wall(8.39)] {
            tick(&mut w, t0, secs);
            assert!(!w.is_ready());
            w.press();
            assert!(!w.is_leaving());
            assert_eq!(w.fade(), 0.0);
        }
    }

    #[test]
    fn a_press_at_ready_starts_the_fade_and_the_hand_off_follows_700ms_later() {
        let (mut w, t0) = (welcome(), Instant::now());
        let ready = wall(timeline::READY) + 0.01;
        tick(&mut w, t0, 0.0);
        tick(&mut w, t0, ready);
        assert!(w.is_ready());
        w.press();
        assert!(w.is_leaving());
        assert_eq!(w.fade(), 0.0);
        assert!(!tick(&mut w, t0, ready + 0.35));
        assert!((w.fade() - 0.5).abs() < 1e-3);
        assert!(!tick(&mut w, t0, ready + 0.69));
        assert!(tick(&mut w, t0, ready + 0.71));
        assert_eq!(w.fade(), 1.0);
    }

    #[test]
    fn the_hand_off_fires_once_and_a_second_press_does_not_restart_the_fade() {
        let (mut w, t0) = (welcome(), Instant::now());
        let ready = wall(timeline::READY) + 0.01;
        tick(&mut w, t0, 0.0);
        tick(&mut w, t0, ready);
        w.press();
        tick(&mut w, t0, ready + 0.2);
        w.press();
        assert!((w.fade() - 0.2 / 0.7).abs() < 1e-3);
        assert!(tick(&mut w, t0, ready + 0.8));
        assert!(!tick(&mut w, t0, ready + 0.9));
        assert!(!tick(&mut w, t0, ready + 5.0));
    }

    #[test]
    fn no_hand_off_without_a_press_however_long_it_waits() {
        let (mut w, t0) = (welcome(), Instant::now());
        tick(&mut w, t0, 0.0);
        for secs in [10.0, 60.0, 600.0] {
            assert!(!tick(&mut w, t0, secs));
        }
    }

    #[test]
    fn reduced_motion_is_ready_immediately_and_still_waits_for_space() {
        let (mut w, t0) = (welcome().reduced_motion(true), Instant::now());
        assert!(w.is_ready());
        assert_eq!(w.t(), timeline::REDUCED_T);
        // No frames needed while nothing moves.
        assert!(!w.wants_frames());
        assert!(!tick(&mut w, t0, 0.0));
        w.press();
        assert!(w.wants_frames());
        assert!(!tick(&mut w, t0, 0.3));
        assert!(tick(&mut w, t0, 0.8));
        assert!(!w.wants_frames());
    }

    #[test]
    fn a_frozen_frame_has_no_clock_and_ignores_input() {
        let (mut w, t0) = (welcome().frozen(9.0, 0.25), Instant::now());
        assert_eq!(w.t(), 9.0);
        assert_eq!(w.fade(), 0.25);
        assert!(!tick(&mut w, t0, 100.0));
        w.press();
        assert!(!w.is_leaving());
        assert_eq!(w.t(), 9.0);
        assert!(!w.wants_frames());
    }
}
