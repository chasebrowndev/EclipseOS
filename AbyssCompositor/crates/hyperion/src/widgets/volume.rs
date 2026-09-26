// SPDX-License-Identifier: AGPL-3.0-only
//! Volume: the speaker mark as the core (click mutes, the wheel steps), the
//! level slider as the revealed section.
//!
//! Neutral throughout: a sink level is state, not the bar's live value.

use iced::mouse::ScrollDelta;
use iced::widget::mouse_area;

use eclipse_services::audio::Sink;
use eclipse_ui::tokens::{bar, color};
use eclipse_ui::widget::{self as parts, ShellFrame, VOLUME_SLIDER_W};

use super::{Action, Feed as Routed, Parts, Spans};
use crate::app::Message;

/// `bar.widgets.volume.*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cfg {
    /// Percent per wheel notch.
    pub step: u32,
    pub scroll: bool,
    /// The slider's ceiling, in percent.
    pub max_percent: u32,
}

impl Default for Cfg {
    fn default() -> Self {
        Cfg {
            step: 5,
            scroll: true,
            max_percent: 100,
        }
    }
}

impl Cfg {
    fn max(&self) -> f32 {
        self.max_percent.max(1) as f32 / 100.0
    }
}

#[derive(Debug, Default)]
pub struct State {
    /// The default sink; `None` is no audio server, and no widget.
    pub sink: Option<Sink>,
}

#[derive(Debug, Clone)]
pub enum Feed {
    Sink(Option<Sink>),
    /// Set the level, `1.0` = 100%.
    Set(f32),
    Mute(bool),
}

pub fn update(state: &mut State, feed: Feed) -> Option<Action> {
    match feed {
        Feed::Sink(sink) => {
            state.sink = sink;
            None
        }
        // Set here as well as asked for: a slider that springs back until
        // the server answers reads as broken. The next report corrects it.
        Feed::Set(v) => {
            let v = if v.is_finite() { v.max(0.0) } else { 0.0 };
            if let Some(s) = state.sink.as_mut() {
                s.volume = v;
            }
            Some(Action::SetVolume(v))
        }
        Feed::Mute(m) => {
            if let Some(s) = state.sink.as_mut() {
                s.muted = m;
            }
            Some(Action::SetMuted(m))
        }
    }
}

/// Always present: with no server the speaker reads muted and there is no
/// slider to reveal — an absent audio stack is a state to show, not 0%.
pub fn spans(state: &State, _cfg: &Cfg) -> Spans {
    Spans {
        core: bar::MARK,
        revealed: if state.sink.is_some() {
            VOLUME_SLIDER_W
        } else {
            0.0
        },
        present: true,
    }
}

/// The theme's speaker at the level's rung.
fn glyph(sink: &Sink) -> &'static str {
    match sink.volume {
        _ if sink.muted => "audio-volume-muted",
        v if v <= 0.0 => "audio-volume-muted",
        v if v < 0.34 => "audio-volume-low",
        v if v < 0.67 => "audio-volume-medium",
        _ => "audio-volume-high",
    }
}

fn msg(f: Feed) -> Message {
    Message::Widget(Routed::Volume(f))
}

/// One wheel notch's worth of level, in the direction the wheel went.
fn stepped(volume: f32, cfg: &Cfg, delta: ScrollDelta) -> f32 {
    let dy = match delta {
        ScrollDelta::Lines { y, .. } | ScrollDelta::Pixels { y, .. } => y,
    };
    let dir = if dy > 0.0 {
        1.0
    } else if dy < 0.0 {
        -1.0
    } else {
        0.0
    };
    // Snapped to the step grid, so a level set by the slider rejoins it.
    let step = cfg.step.max(1) as f32 / 100.0;
    let at = (volume / step).round() * step;
    (at + dir * step).clamp(0.0, cfg.max())
}

pub fn view<'a>(state: &'a State, cfg: &Cfg, frame: ShellFrame) -> Parts<'a> {
    let ink = frame.core_alpha();
    let Some(sink) = state.sink.as_ref() else {
        let mark = parts::mark(
            crate::icons::symbolic("audio-volume-muted"),
            bar::MARK,
            color::TEXT_TERTIARY.scale_alpha(ink),
        );
        return Parts {
            core: super::fixed(mark, bar::MARK),
            revealed: None,
        };
    };
    let tint = if sink.muted {
        color::TEXT_TERTIARY
    } else {
        color::TEXT_SECONDARY
    };
    let mark = parts::mark(
        crate::icons::symbolic(glyph(sink)),
        bar::MARK,
        tint.scale_alpha(ink),
    );
    let press = super::press(mark, bar::MARK, msg(Feed::Mute(!sink.muted)));
    let core = if cfg.scroll {
        let (volume, cfg) = (sink.volume, *cfg);
        mouse_area(press)
            .on_scroll(move |d| msg(Feed::Set(stepped(volume, &cfg, d))))
            .into()
    } else {
        press
    };
    let slider = parts::volume_slider_faded(
        sink.volume,
        cfg.max(),
        sink.muted,
        false,
        |v| msg(Feed::Set(v)),
        None,
        frame.revealed_alpha(),
    );
    Parts {
        core,
        revealed: Some(slider),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wheel_steps_on_the_grid_and_stops_at_the_ceiling() {
        let cfg = Cfg::default();
        let up = ScrollDelta::Lines { x: 0.0, y: 1.0 };
        let down = ScrollDelta::Lines { x: 0.0, y: -1.0 };
        assert!((stepped(0.52, &cfg, up) - 0.55).abs() < 1e-4);
        assert!((stepped(0.52, &cfg, down) - 0.45).abs() < 1e-4);
        assert_eq!(stepped(0.99, &cfg, up), 1.0);
        assert_eq!(stepped(0.01, &cfg, down), 0.0);
    }

    #[test]
    fn no_sink_is_a_muted_speaker_with_nothing_to_reveal() {
        let sp = spans(&State::default(), &Cfg::default());
        assert!(sp.present);
        assert_eq!(sp.revealed, 0.0);
    }

    #[test]
    fn a_slider_move_is_set_locally_and_asked_for() {
        let mut s = State {
            sink: Some(Sink {
                volume: 0.2,
                muted: false,
                description: String::new(),
            }),
        };
        assert_eq!(update(&mut s, Feed::Set(0.6)), Some(Action::SetVolume(0.6)));
        assert_eq!(s.sink.as_ref().map(|s| s.volume), Some(0.6));
    }
}
