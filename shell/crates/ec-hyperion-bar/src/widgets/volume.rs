// SPDX-License-Identifier: AGPL-3.0-only
//! Volume: the speaker mark as the core (click opens the audio picker,
//! [`sheet`]), the level slider as the revealed section, and the wheel
//! anywhere on the widget steps the level ([`wheel`]).
//!
//! Neutral throughout: a sink level is state, not the bar's live value.

use iced::mouse::ScrollDelta;
use iced::widget::mouse_area;

use ec_services::audio::{Device, DeviceKind, Sink};
use ec_ui::tokens::{bar, color, drawer};
use ec_ui::widget::{self as parts, ShellFrame, VOLUME_SLIDER_W};

use super::{Action, Feed as Routed, Parts, Spans};
use crate::app::{App, Drawer, Message};
use crate::view::{elide, Sheet};

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
    /// The wheel over the widget, counted in notches.
    pub wheel: super::Notches,
}

#[derive(Debug, Clone)]
pub enum Feed {
    Sink(Option<Sink>),
    /// Set the level, `1.0` = 100%.
    Set(f32),
    Mute(bool),
    /// The wheel moved over the widget; a whole notch steps by `cfg.step`.
    Wheel(ScrollDelta, Cfg),
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
        Feed::Wheel(delta, cfg) => {
            let notches = state.wheel.count(delta, std::time::Instant::now());
            let sink = state.sink.as_mut().filter(|_| notches != 0)?;
            let v = stepped(sink.volume, &cfg, notches);
            sink.volume = v;
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
        lead: 0.0,
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

/// `notches` wheel notches' worth of level; positive is louder.
fn stepped(volume: f32, cfg: &Cfg, notches: i32) -> f32 {
    // Snapped to the step grid, so a level set by the slider rejoins it.
    let step = cfg.step.max(1) as f32 / 100.0;
    let at = (volume / step).round() * step;
    (at + notches as f32 * step).clamp(0.0, cfg.max())
}

/// The whole widget — grip, speaker and slider — as the wheel's target, not
/// the speaker alone: a 14px mark is too small a place to aim a wheel at.
/// With no sink, or scrolling off, the cell is returned as it was.
pub fn wheel<'a>(
    state: &State,
    cfg: &Cfg,
    cell: iced::Element<'a, Message, iced::Theme>,
) -> iced::Element<'a, Message, iced::Theme> {
    if !cfg.scroll || state.sink.is_none() {
        return cell;
    }
    let cfg = *cfg;
    mouse_area(cell)
        .on_scroll(move |d| msg(Feed::Wheel(d, cfg)))
        .into()
}

/// Room for a device's name: audio names are the whole row, with no lock or
/// hero verb beside them, so they take more than a network's.
const DEVICE_CHARS: usize = 26;

/// The theme's glyph for a device's kind.
fn kind_glyph(kind: DeviceKind) -> &'static str {
    match kind {
        DeviceKind::Speakers => "audio-speakers",
        DeviceKind::Headphones => "audio-headphones",
        DeviceKind::Headset => "audio-headset",
        DeviceKind::Hdmi => "video-display",
        DeviceKind::Bluetooth => "bluetooth",
        DeviceKind::Mic => "audio-input-microphone",
        DeviceKind::Usb | DeviceKind::Other => "audio-card",
    }
}

/// The hero row's second line: what the device is, in the mono ramp.
fn kind_label(kind: DeviceKind) -> &'static str {
    match kind {
        DeviceKind::Speakers => "speakers",
        DeviceKind::Headphones => "headphones",
        DeviceKind::Headset => "headset",
        DeviceKind::Hdmi => "hdmi",
        DeviceKind::Usb => "usb",
        DeviceKind::Bluetooth => "bluetooth",
        DeviceKind::Mic => "microphone",
        DeviceKind::Other => "default",
    }
}

/// One half of the picker: a section label, the level row, then the devices
/// with the default as the hero. `accent` lights the level's fill, so only
/// one half ever does.
#[allow(clippy::too_many_arguments)]
fn section(
    s: &mut Sheet,
    label: Option<&str>,
    devices: &[Device],
    default: Option<&str>,
    level: Option<(f32, bool)>,
    accent: bool,
    on_mute: Message,
    on_level: impl Fn(f32) -> Message + 'static,
    pick: impl Fn(String) -> Message,
    empty: &str,
    mic: bool,
) {
    if let Some(label) = label {
        s.section(label);
    }
    let (volume, muted) = level.unwrap_or((0.0, false));
    let lead = if mic {
        if muted {
            "microphone-sensitivity-muted"
        } else {
            "microphone-sensitivity-high"
        }
    } else if muted || volume <= 0.0 {
        "audio-volume-muted"
    } else if volume < 0.34 {
        "audio-volume-low"
    } else if volume < 0.67 {
        "audio-volume-medium"
    } else {
        "audio-volume-high"
    };
    s.row(parts::drawer_level(
        parts::mark(
            crate::icons::symbolic(lead),
            drawer::MARK,
            if muted {
                color::TEXT_TERTIARY
            } else {
                color::TEXT_SECONDARY
            },
        ),
        on_mute,
        volume,
        muted,
        accent,
        on_level,
    ));
    let hero = devices.iter().find(|d| Some(d.name.as_str()) == default);
    if let Some(d) = hero {
        s.current(parts::drawer_current(
            parts::mark(
                crate::icons::symbolic(kind_glyph(d.kind)),
                drawer::CURRENT_MARK,
                color::TEXT,
            ),
            &elide(&d.description, DEVICE_CHARS),
            kind_label(d.kind),
            color::TEXT_TERTIARY,
            None,
        ));
    }
    for d in devices
        .iter()
        .filter(|d| Some(d.name.as_str()) != default)
        .take(drawer::MAX_ROWS)
    {
        s.row(parts::drawer_choice(
            parts::mark(
                crate::icons::symbolic(kind_glyph(d.kind)),
                drawer::MARK,
                color::TEXT_SECONDARY,
            ),
            &elide(&d.description, DEVICE_CHARS),
            None,
            Some(pick(d.name.clone())),
        ));
    }
    if devices.is_empty() {
        s.row(parts::drawer_note(empty));
    }
}

/// The audio picker: output, a rule, input. The one yellow is the output
/// level's fill — the number the human came to change; the input's rail and
/// both heroes stay on the ink ramp.
pub(crate) fn sheet(app: &App) -> Sheet {
    let a = &app.audio;
    // The sink feed is the live level; before it answers, the device list's.
    let out_default = a
        .outputs
        .iter()
        .find(|d| Some(&d.name) == a.default_output.as_ref());
    let out_level = app
        .widgets
        .volume
        .sink
        .as_ref()
        .map(|k| (k.volume, k.muted))
        .or_else(|| out_default.map(|d| (d.volume, d.muted)));
    let in_level = a
        .inputs
        .iter()
        .find(|d| Some(&d.name) == a.default_input.as_ref())
        .map(|d| (d.volume, d.muted));
    let mut s = Sheet::new(parts::drawer_sheet_head("output"), drawer::HEAD_H);
    // The output label is the sheet's head; the input's is a mid-sheet strip.
    section(
        &mut s,
        None,
        &a.outputs,
        a.default_output.as_deref(),
        out_level,
        true,
        msg(Feed::Mute(!out_level.is_some_and(|l| l.1))),
        |v| msg(Feed::Set(v)),
        Message::AudioOutput,
        "no output devices",
        false,
    );
    s.rule();
    section(
        &mut s,
        Some("input"),
        &a.inputs,
        a.default_input.as_deref(),
        in_level,
        false,
        Message::InputMute(!in_level.is_some_and(|l| l.1)),
        Message::InputVolume,
        Message::AudioInput,
        "no input devices",
        true,
    );
    s
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
    let core = super::press(mark, bar::MARK, Message::Open(Drawer::Audio));
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
        assert!((stepped(0.52, &cfg, 1) - 0.55).abs() < 1e-4);
        assert!((stepped(0.52, &cfg, -1) - 0.45).abs() < 1e-4);
        assert_eq!(stepped(0.99, &cfg, 1), 1.0);
        assert_eq!(stepped(0.01, &cfg, -1), 0.0);
    }

    /// A wheel click's three events step the level once; the empty one never
    /// turns it down.
    #[test]
    fn one_click_steps_once() {
        let mut s = State {
            sink: Some(Sink {
                volume: 0.5,
                muted: false,
                description: String::new(),
            }),
            ..State::default()
        };
        let cfg = Cfg::default();
        let feeds = [
            ScrollDelta::Pixels { x: 0.0, y: 0.0 },
            ScrollDelta::Lines { x: 0.0, y: 1.0 },
            ScrollDelta::Pixels { x: 0.0, y: 15.0 },
        ];
        for d in feeds {
            let _ = update(&mut s, Feed::Wheel(d, cfg));
        }
        assert!((s.sink.as_ref().map_or(0.0, |s| s.volume) - 0.55).abs() < 1e-4);
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
            ..State::default()
        };
        assert_eq!(update(&mut s, Feed::Set(0.6)), Some(Action::SetVolume(0.6)));
        assert_eq!(s.sink.as_ref().map(|s| s.volume), Some(0.6));
    }
}
