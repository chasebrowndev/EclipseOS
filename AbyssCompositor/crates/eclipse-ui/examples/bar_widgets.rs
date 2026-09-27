// SPDX-License-Identifier: AGPL-3.0-only
//! A specimen sheet of the bar widget vocabulary (ADR 0065): every primitive
//! in `eclipse_ui::widget`'s bar set, in every state, at bar height.
//!
//! `cargo run -p eclipse-ui --example bar_widgets`
//!
//! It is a screenshot subject, so it is deterministic unless asked not to be:
//! `BAR_WIDGETS_FREEZE=<open>,<reveal>` pins the animated strip's frame
//! (`1,0.5` is half-revealed); unset, the strip cycles through
//! full → revealed → full → compressed on the default spring, retargeting
//! mid-flight, to show that nothing reflows while the width moves.
//!
//! Accent ledger — one yellow per strip:
//! - `full`, `revealed`, `animating`: the visualizer of the playing track.
//! - `compressed`: none; nothing live is on screen.
//! - `states`: the volume fill, to show the flag spending it elsewhere while
//!   the paused track's visualizer rests in tertiary ink.

use std::time::{Duration, Instant};

use eclipse_ui::motion::{Animated, Motion};
use eclipse_ui::theme;
use eclipse_ui::tokens::{bar, color, space};
use eclipse_ui::widget::{
    art_handle, art_thumb, drag_bar, lit, micro_label, mini_meter, track_label, transport, viz_bars,
    volume_slider, widget_shell, Grip, ShellFrame, ShellSpan, TRANSPORT_W, VOLUME_SLIDER_W,
};
use iced::widget::{column, container, image, row, Row, Space};
use iced::{Alignment, Element, Length, Subscription, Theme};

/// Which strip of the sheet a message came from.
#[derive(Debug, Clone)]
enum Msg {
    Frame(Instant),
    Volume(f32),
    Grab,
    Drag(f32),
    Release,
    Transport,
}

/// The legs the animated strip cycles through, as `(open, reveal)`.
const LEGS: [(f32, f32); 4] = [(1.0, 0.0), (1.0, 1.0), (1.0, 0.0), (0.0, 0.0)];

struct Sheet {
    art: image::Handle,
    now: Instant,
    t0: Instant,
    freeze: Option<(f32, f32)>,
    open: Animated,
    reveal: Animated,
    leg: usize,
    next_leg: Instant,
    volume: f32,
    drag: Option<f32>,
}

impl Sheet {
    fn new() -> Self {
        let now = Instant::now();
        let freeze = std::env::var("BAR_WIDGETS_FREEZE").ok().and_then(|v| {
            let (a, b) = v.split_once(',')?;
            Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
        });
        Sheet {
            art: art_handle(include_bytes!("../../../dist/greetd/wallpaper.png")),
            now,
            t0: now,
            freeze,
            open: Animated::new(1.0, Motion::DEFAULT),
            reveal: Animated::new(0.0, Motion::DEFAULT),
            leg: 0,
            // Deliberately shorter than a leg plus its settle, so some
            // retargets land mid-flight.
            next_leg: now + Duration::from_millis(900),
            volume: 0.64,
            drag: None,
        }
    }

    /// Band levels at the sheet's clock: a deterministic stand-in for the
    /// audio tap, shaped like music (heavier low end).
    fn levels(&self, gain: f32) -> [f32; bar::VIZ_BANDS] {
        let t = if self.freeze.is_some() {
            1.3
        } else {
            (self.now - self.t0).as_secs_f32()
        };
        std::array::from_fn(|i| {
            let f = i as f32;
            let tilt = 1.0 - f / (bar::VIZ_BANDS as f32 * 1.4);
            let wobble = 0.5 + 0.5 * (t * (2.1 + f * 0.37) + f * 0.9).sin() * (t * 0.7 + f * 0.2).cos();
            (gain * tilt * wobble).clamp(0.0, 1.0)
        })
    }

    fn animated_frame(&self) -> ShellFrame {
        let (open, reveal) = self.freeze.unwrap_or((self.open.value(), self.reveal.value()));
        ShellFrame {
            open,
            reveal,
            presence: 1.0,
        }
    }
}

fn update(s: &mut Sheet, msg: Msg) {
    match msg {
        Msg::Frame(now) => {
            s.now = now;
            if s.drag.is_none() && now >= s.next_leg {
                s.leg = (s.leg + 1) % LEGS.len();
                let (open, reveal) = LEGS[s.leg];
                s.open.set_target(open, now);
                s.reveal.set_target(reveal, now);
                s.next_leg = now + Duration::from_millis(900);
            }
            s.open.tick(now);
            s.reveal.tick(now);
        }
        Msg::Volume(v) => s.volume = v,
        Msg::Grab => s.drag = Some(s.reveal.value()),
        Msg::Drag(dx) => {
            if let Some(start) = s.drag {
                let span = TRANSPORT_W + bar::WIDGET_GAP;
                s.reveal.snap((start - dx / span).clamp(0.0, 1.0));
            }
        }
        Msg::Release => {
            s.drag = None;
            let snap_to = if s.reveal.value() > 0.5 { 1.0 } else { 0.0 };
            s.reveal.set_target(snap_to, s.now);
        }
        Msg::Transport => {}
    }
}

// ------------------------------------------------------------- the widgets

const NOW_PLAYING: ShellSpan = ShellSpan {
    core: bar::ART + bar::WIDGET_GAP + bar::MEDIA_TEXT_W + bar::WIDGET_GAP + bar::VIZ_W,
    revealed: TRANSPORT_W,
};
const USAGE: ShellSpan = ShellSpan {
    core: bar::METER_W,
    revealed: 3.0 * bar::METER_W + 2.0 * bar::WIDGET_GAP,
};
const VOLUME: ShellSpan = ShellSpan {
    core: VOLUME_SLIDER_W,
    revealed: 0.0,
};

fn wired_grip<'a>(state: Grip) -> Element<'a, Msg, Theme> {
    drag_bar()
        .state(state)
        .on_press(Msg::Grab)
        .on_drag(Msg::Drag)
        .on_release(Msg::Release)
        .into()
}

fn now_playing<'a>(
    s: &Sheet,
    frame: ShellFrame,
    playing: bool,
    gain: f32,
    grip: Grip,
) -> Element<'a, Msg, Theme> {
    let levels = if playing {
        s.levels(gain)
    } else {
        [0.0; bar::VIZ_BANDS]
    };
    let core = row![
        art_thumb(Some(&s.art), false),
        track_label(
            "Halflight Transit",
            "Low Orbit Choir — Parallax",
            bar::MEDIA_TEXT_W
        ),
        viz_bars(&levels, playing),
    ]
    .spacing(bar::WIDGET_GAP)
    .align_y(Alignment::Center);
    let revealed = transport(
        playing,
        Some(Msg::Transport),
        Some(Msg::Transport),
        Some(Msg::Transport),
    );
    widget_shell(wired_grip(grip), core, Some(revealed), NOW_PLAYING, frame)
}

fn usage<'a>(frame: ShellFrame, grip: Grip) -> Element<'a, Msg, Theme> {
    let revealed = row![
        mini_meter("mem", 0.61, false),
        mini_meter("gpu", 0.18, false),
        mini_meter("disk", 0.83, false),
    ]
    .spacing(bar::WIDGET_GAP)
    .align_y(Alignment::Center);
    widget_shell(
        wired_grip(grip),
        mini_meter("cpu", 0.42, false),
        Some(revealed.into()),
        USAGE,
        frame,
    )
}

fn volume<'a>(
    s: &Sheet,
    frame: ShellFrame,
    accent: bool,
    muted: bool,
    grip: Grip,
) -> Element<'a, Msg, Theme> {
    widget_shell(
        wired_grip(grip),
        volume_slider(s.volume, 1.0, muted, accent, Msg::Volume, None),
        None,
        VOLUME,
        frame,
    )
}

// ---------------------------------------------------------------- the sheet

/// One bar: the real sheet's pill ground and lit top edge, widgets at the
/// right as they sit after the task strip.
fn strip<'a>(label: &str, cells: Vec<Element<'a, Msg, Theme>>) -> Element<'a, Msg, Theme> {
    let cells = Row::with_children(cells)
        .spacing(bar::GAP)
        .align_y(Alignment::Center);
    let sheet = lit(
        container(cells)
            .padding([0.0, bar::EDGE])
            .height(Length::Fixed(bar::PILL_H))
            .align_y(Alignment::Center)
            .style(theme::bar_ground(bar::RADIUS_SHEET)),
        bar::RADIUS_SHEET,
        color::HIGHLIGHT_SOFT,
    );
    row![
        container(micro_label(label)).width(Length::Fixed(space::SIDEBAR_W / 2.0)),
        Space::new().width(Length::Fill),
        sheet,
    ]
    .align_y(Alignment::Center)
    .into()
}

fn view(s: &Sheet) -> Element<'_, Msg, Theme> {
    let full = ShellFrame::FULL;
    let rest = Grip::Rest;
    let strips = column![
        strip(
            "full",
            vec![
                now_playing(s, full, true, 0.9, rest),
                usage(full, rest),
                volume(s, full, false, false, rest)
            ],
        ),
        strip(
            "revealed",
            vec![
                now_playing(s, ShellFrame::REVEALED, true, 0.9, rest),
                usage(ShellFrame::REVEALED, rest),
                volume(s, full, false, false, rest),
            ],
        ),
        strip(
            "compressed",
            vec![
                now_playing(
                    s,
                    ShellFrame {
                        presence: 0.5,
                        ..ShellFrame::COMPRESSED
                    },
                    true,
                    0.9,
                    rest
                ),
                now_playing(s, ShellFrame::COMPRESSED, true, 0.9, rest),
                usage(ShellFrame::COMPRESSED, rest),
                volume(s, ShellFrame::COMPRESSED, false, false, rest),
            ],
        ),
        strip(
            "animating",
            vec![
                now_playing(s, s.animated_frame(), true, 0.5, rest),
                usage(full, rest)
            ],
        ),
        strip(
            "states",
            vec![
                now_playing(s, full, false, 0.0, rest),
                usage(ShellFrame::COMPRESSED, Grip::Hover),
                usage(ShellFrame::COMPRESSED, Grip::Active),
                volume(s, full, true, false, Grip::Hover),
                volume(s, full, false, true, rest),
            ],
        ),
    ]
    .spacing(space::BLOCK);

    container(strips)
        .padding([space::PANE_Y, space::PANE_X])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::window)
        .into()
}

fn subscription(_s: &Sheet) -> Subscription<Msg> {
    iced::window::frames().map(Msg::Frame)
}

fn main() -> iced::Result {
    let mut app = iced::application(Sheet::new, update, view)
        .title("bar widgets")
        .subscription(subscription)
        .theme(|_: &Sheet| theme::theme())
        .window_size((1120.0, 420.0))
        .antialiasing(true);
    for face in eclipse_ui::FONTS {
        app = app.font(*face);
    }
    app.run()
}
