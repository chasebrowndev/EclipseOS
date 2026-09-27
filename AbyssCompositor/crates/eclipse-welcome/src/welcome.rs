// SPDX-License-Identifier: AGPL-3.0-only
//! The welcome step's state, messages and wiring.
//!
//! The animation itself is [`crate::timeline`]; this holds the one thing that
//! is not a function of time: whether Space has been pressed, and when. Time
//! reaches it only as frame ticks, and everything drawn is recomputed from the
//! elapsed time on every `view`, so a frozen instant (`--at`) and a live run go
//! through exactly the same code.

use crate::draw::Stage;
use crate::fonts::Faces;
use crate::logo::{Layers, LogoSource};
use crate::timeline::{self, Frame};
use iced::advanced::graphics::geometry::Renderer as _;
use iced::advanced::Renderer as _;
use iced::widget::canvas::{self, Cache};
use iced::{
    event, keyboard, mouse, touch, window, Element, Length, Rectangle, Renderer, Size, Subscription, Task,
    Theme,
};
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
    /// The window's logical size, from `window::size` or a resize event.
    Resized(Size),
    /// The output scale factor, from `window::scale_factor` or a rescale
    /// event. Needed to resample the logo to the pixels it will land on.
    Rescaled(f32),
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
    logo_src: Option<LogoSource>,
    /// The logo as last resampled, and the pixel width it was resampled to
    /// (`None`: still the asset's own pixels).
    logo: Layers,
    logo_px: Option<(u32, u32)>,
    logo_aspect: f32,
    /// Logical window size and output scale, once known.
    size: Option<Size>,
    scale: f32,
    /// A multiplier the application applies on top of the output's scale
    /// (`iced::application::scale_factor`); 1 unless the embedder sets one.
    ui_scale: f32,
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

impl Welcome {
    /// `version` is the label's number: it renders as `Version {version}`.
    /// Pass `env!("CARGO_PKG_VERSION")` of the embedding binary, or
    /// [`Welcome::default_version`] for this crate's own.
    pub fn new(version: impl AsRef<str>) -> Welcome {
        let logo_src = LogoSource::decode(LOGO_PNG);
        // The asset is compiled in and checked by a test; a build that passed
        // it never takes the fallback, but a welcome screen must not panic.
        let (logo, logo_aspect) = match &logo_src {
            Some(src) => (src.native(), src.aspect),
            None => (Layers::whole(LOGO_PNG), 144.0 / 796.0),
        };
        Welcome {
            label: format!("Version {}", version.as_ref()),
            reduced: false,
            faces: Faces::detect(),
            logo_src,
            logo,
            logo_px: None,
            logo_aspect,
            size: None,
            scale: 1.0,
            ui_scale: 1.0,
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

    /// The multiplier the embedding application passes to
    /// `iced::application::scale_factor`, if any. The logo is resampled to
    /// physical pixels, so it has to know.
    pub fn ui_scale(mut self, factor: f32) -> Welcome {
        self.ui_scale = factor;
        self
    }

    /// Ask the window for its size and scale, so the logo can be resampled to
    /// the pixels it lands on. Return this from the embedder's first task. It
    /// is also re-issued when the window reports being opened or focused,
    /// which is why an embedder that forgets it still ends up sharp.
    pub fn probe() -> Task<Message> {
        window::oldest().and_then(|id| {
            Task::batch([
                window::size(id).map(Message::Resized),
                window::scale_factor(id).map(Message::Rescaled),
            ])
        })
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
            Message::Resized(size) => {
                let first = self.size.is_none();
                self.size = Some(size);
                self.refresh_logo();
                if first {
                    // The scale factor rarely arrives with the size.
                    return Welcome::probe();
                }
            }
            Message::Rescaled(scale) => {
                self.scale = scale.max(0.1);
                self.refresh_logo();
            }
            Message::Begin => {}
        }
        Task::none()
    }

    /// Effective pixels per logical pixel.
    fn pixel_ratio(&self) -> f32 {
        self.scale * self.ui_scale
    }

    /// Resample the logo to the physical width it is drawn at: 50cqw.
    fn refresh_logo(&mut self) {
        let (Some(src), Some(size)) = (&self.logo_src, self.size) else {
            return;
        };
        let width = (0.5 * size.width * self.pixel_ratio()).round().max(1.0) as u32;
        if self.logo_px.map(|(w, _)| w) != Some(width) {
            self.logo = src.sized(width);
            self.logo_px = Some((width, src.height_for(width)));
        }
    }

    fn stage(&self) -> Stage<'_> {
        Stage {
            frame: Frame::at(self.t(), self.reduced),
            fade: self.fade(),
            label: &self.label,
            faces: &self.faces,
            logo: &self.logo,
            logo_aspect: self.logo_aspect,
            logo_px: self.logo_px,
            pixel_ratio: self.pixel_ratio(),
            background: &self.background,
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        canvas::Canvas::new(self.stage())
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    /// Draw the current frame straight into `renderer`, with no window, no
    /// event loop and no display connection: `size` is the logical size and
    /// `scale` the output's scale factor, so the frame is `size * scale`
    /// physical pixels. This is how the standalone binary's `--shot` renders
    /// (it must never open a window on the session it is run from) and how
    /// the screen is checked at 1080p, 1440p and fractional scales.
    pub fn paint(&mut self, renderer: &mut Renderer, size: Size, scale: f32) {
        let _ = self.update(Message::Rescaled(scale));
        let _ = self.update(Message::Resized(size));
        let bounds = Rectangle::with_size(size);
        let stage = self.stage();
        let geometry = canvas::Program::draw(
            &stage,
            &(),
            renderer,
            &Theme::Dark,
            bounds,
            mouse::Cursor::Unavailable,
        );
        renderer.with_layer(bounds, |r| {
            for g in geometry {
                r.draw_geometry(g);
            }
        });
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
            iced::Event::Window(window::Event::Opened { size, .. }) => Some(Message::Resized(size)),
            iced::Event::Window(window::Event::Resized(size)) => Some(Message::Resized(size)),
            iced::Event::Window(window::Event::Rescaled(scale)) => Some(Message::Rescaled(scale)),
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
    fn the_logo_is_resampled_to_the_physical_width_it_is_drawn_at() {
        let mut w = welcome();
        assert!(w.logo_px.is_none());
        let _ = w.update(Message::Rescaled(1.5));
        assert!(w.logo_px.is_none(), "no size yet, nothing to resample to");
        let _ = w.update(Message::Resized(Size::new(1000.0, 600.0)));
        // 50cqw of 1000 logical px, at 1.5: 750 physical.
        assert_eq!(w.logo_px, Some((750, 136)));
        let _ = w.update(Message::Rescaled(2.0));
        assert_eq!(w.logo_px, Some((1000, 181)));
        // The application's own multiplier counts too.
        let mut w = welcome().ui_scale(0.5);
        let _ = w.update(Message::Rescaled(2.0));
        let _ = w.update(Message::Resized(Size::new(1000.0, 600.0)));
        assert_eq!(w.logo_px.map(|p| p.0), Some(500));
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
