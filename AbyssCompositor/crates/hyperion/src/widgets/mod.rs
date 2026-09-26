// SPDX-License-Identifier: AGPL-3.0-only
//! The bar's widgets (ADR 0065): everything right of the task strip.
//!
//! A widget is a glass cell of the same family as a window chip —
//! [`eclipse_ui::theme::bar_cell`]'s ground and hairline — holding a *core*
//! and, optionally, a *revealed* section, behind a grip that drags it between
//! compressed, core and revealed. Which widgets exist and in what order is
//! `bar.widgets.order`; the ones in `bar.widgets.important` never compress.
//!
//! Every widget module has the same four parts:
//!
//! - `State`, what it last heard (the service-fed widgets own theirs; the
//!   status widgets — network, bluetooth, battery, tray, clock — read the
//!   app's status fields, which other parts of the bar share);
//! - `update(&mut State, Feed) -> Option<Action>`, where a `Feed` is a
//!   service's report or the human's input, and the `Action` is what the app
//!   has to ask a service to do;
//! - `spans(..) -> Spans`, its core and revealed widths and whether it has
//!   anything to show — what [`crate::layout::solve`] reads;
//! - `view(.., frame) -> Parts`, the core and the revealed section, which
//!   [`cell`] mounts on the shell.
//!
//! The shell, the grip and the width animation are this module's, never a
//! widget's: a widget draws its content at its natural width and nothing
//! else.

use std::collections::HashMap;

use iced::widget::{button, container};
use iced::{Alignment, Element, Length, Theme};

use eclipse_services::custom::{Kind, Output, WidgetSpec};
use eclipse_ui::motion::Motion;
use eclipse_ui::tokens::{bar, color};
use eclipse_ui::widget::{self as parts, ClipEdge, Grip, ShellFrame, ShellSpan};

use crate::app::{App, Message};

pub mod battery;
pub mod bluetooth;
pub mod clock;
pub mod custom;
pub mod network;
pub mod now_playing;
pub mod system_usage;
pub mod tray;
pub mod volume;

/// One widget, by its `bar.widgets.order` name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WidgetId {
    NowPlaying,
    SystemUsage,
    Volume,
    Network,
    Bluetooth,
    Battery,
    Tray,
    Clock,
    /// A `widget "<name>" { … }` block, written `custom:<name>`.
    Custom(String),
}

impl WidgetId {
    /// The config spelling; `None` for a name the bar does not know, which
    /// the compositor's validator has already refused.
    pub fn parse(s: &str) -> Option<WidgetId> {
        Some(match s {
            "now-playing" => WidgetId::NowPlaying,
            "system-usage" => WidgetId::SystemUsage,
            "volume" => WidgetId::Volume,
            "network" => WidgetId::Network,
            "bluetooth" => WidgetId::Bluetooth,
            "battery" => WidgetId::Battery,
            "tray" => WidgetId::Tray,
            "clock" => WidgetId::Clock,
            other => {
                let name = other.strip_prefix("custom:")?;
                if name.is_empty() {
                    return None;
                }
                WidgetId::Custom(name.to_owned())
            }
        })
    }

    /// The config spelling back: the key motion, pins and drags are kept by.
    pub fn key(&self) -> String {
        match self {
            WidgetId::NowPlaying => "now-playing".to_owned(),
            WidgetId::SystemUsage => "system-usage".to_owned(),
            WidgetId::Volume => "volume".to_owned(),
            WidgetId::Network => "network".to_owned(),
            WidgetId::Bluetooth => "bluetooth".to_owned(),
            WidgetId::Battery => "battery".to_owned(),
            WidgetId::Tray => "tray".to_owned(),
            WidgetId::Clock => "clock".to_owned(),
            WidgetId::Custom(name) => format!("custom:{name}"),
        }
    }
}

/// What the solver needs from a widget.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Spans {
    /// The core's own width, shell padding excluded.
    pub core: f32,
    /// The revealed section's; 0 for none.
    pub revealed: f32,
    /// Anything to show at all. An absent widget animates out to zero.
    pub present: bool,
}

/// What a widget draws.
pub struct Parts<'a> {
    pub core: Element<'a, Message, Theme>,
    pub revealed: Option<Element<'a, Message, Theme>>,
}

impl Parts<'_> {
    fn empty() -> Self {
        Parts {
            core: iced::widget::Space::new().into(),
            revealed: None,
        }
    }
}

/// `bar.widgets.*` and `bar.motion.*`, and the `widget` blocks.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub order: Vec<WidgetId>,
    pub important: Vec<WidgetId>,
    pub now_playing: now_playing::Cfg,
    pub usage: system_usage::Cfg,
    pub volume: volume::Cfg,
    pub motion: Motion,
    pub custom: Vec<WidgetSpec>,
}

impl Default for Config {
    fn default() -> Self {
        let ids = |names: &[&str]| names.iter().filter_map(|n| WidgetId::parse(n)).collect();
        Config {
            order: ids(&[
                "now-playing",
                "volume",
                "network",
                "bluetooth",
                "battery",
                "tray",
                "clock",
            ]),
            important: ids(&["clock", "battery"]),
            now_playing: now_playing::Cfg::default(),
            usage: system_usage::Cfg::default(),
            volume: volume::Cfg::default(),
            motion: Motion::DEFAULT,
            custom: Vec::new(),
        }
    }
}

impl Config {
    /// The `widget` block a `custom:<name>` refers to.
    pub fn spec(&self, name: &str) -> Option<&WidgetSpec> {
        self.custom.iter().find(|s| s.name == name)
    }
}

/// The service-fed widgets' state.
#[derive(Debug, Default)]
pub struct State {
    pub now_playing: now_playing::State,
    pub usage: system_usage::State,
    pub volume: volume::State,
    /// By block name.
    pub custom: HashMap<String, custom::State>,
}

/// A report or an input, routed to one widget.
#[derive(Debug, Clone)]
pub enum Feed {
    NowPlaying(now_playing::Feed),
    Usage(system_usage::Feed),
    Volume(volume::Feed),
    Custom(String, custom::Feed),
}

/// What the app has to ask a service to do on a widget's behalf.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Previous,
    PlayPause,
    Next,
    SetVolume(f32),
    SetMuted(bool),
    /// A custom widget's `on-click`/`on-scroll-*` argv. Never logged.
    Run(Vec<String>),
}

/// A wheel's events, counted as notches.
///
/// One notch of a wheel reaches the bar as up to three events
/// (layershellev): the axis source with no motion at all, the discrete step
/// (`ScrollDelta::Lines`) and the same step again in pixels
/// (`ScrollDelta::Pixels`). Acting on each is three steps per click. A zero
/// delta is dropped; a pixel delta that trails a discrete one is its twin and
/// dropped too; and a pixel stream with no discrete steps at all — a
/// touchpad — counts a notch per [`PX_PER_NOTCH`] of travel.
#[derive(Debug, Default)]
pub struct Notches {
    /// When the last discrete step arrived.
    lines_at: Option<std::time::Instant>,
    /// Pixel travel not yet counted as a notch.
    px: f32,
}

/// How long after a discrete step its pixel twin may still arrive. The two
/// come out of one `wl_pointer.frame`, so this is far longer than they are
/// ever apart and far shorter than two human notches.
const TWIN: std::time::Duration = std::time::Duration::from_millis(40);

/// Smooth-scroll travel that counts as one notch: libinput's pixel value for
/// one wheel click.
const PX_PER_NOTCH: f32 = 15.0;

impl Notches {
    /// The notches this event is worth: positive is up, zero is nothing.
    pub fn count(&mut self, delta: iced::mouse::ScrollDelta, now: std::time::Instant) -> i32 {
        use iced::mouse::ScrollDelta;
        match delta {
            ScrollDelta::Lines { y, .. } => {
                if y == 0.0 || !y.is_finite() {
                    return 0;
                }
                self.lines_at = Some(now);
                self.px = 0.0;
                // A fast spin reports two clicks as one step of 2.
                (y.abs().round().max(1.0) as i32) * y.signum() as i32
            }
            ScrollDelta::Pixels { y, .. } => {
                if y == 0.0 || !y.is_finite() {
                    return 0;
                }
                if self
                    .lines_at
                    .is_some_and(|at| now.saturating_duration_since(at) <= TWIN)
                {
                    return 0;
                }
                if self.px != 0.0 && self.px.signum() != y.signum() {
                    self.px = 0.0;
                }
                self.px += y;
                let n = (self.px / PX_PER_NOTCH).trunc();
                self.px -= n * PX_PER_NOTCH;
                n as i32
            }
        }
    }
}

/// A grip gesture, keyed by widget in [`Message::Grip`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GripEv {
    Press,
    /// Travel since the press; negative is leftward, the reveal direction.
    Drag(f32),
    Release,
}

/// Fold a feed into the state it belongs to.
pub fn update(state: &mut State, feed: Feed) -> Option<Action> {
    match feed {
        Feed::NowPlaying(f) => now_playing::update(&mut state.now_playing, f),
        Feed::Usage(f) => system_usage::update(&mut state.usage, f),
        Feed::Volume(f) => volume::update(&mut state.volume, f),
        Feed::Custom(name, f) => custom::update(state.custom.entry(name).or_default(), f),
    }
}

/// A widget's spans, from whichever state it reads.
pub fn spans(app: &App, id: &WidgetId) -> Spans {
    let cfg = &app.widget_cfg;
    match id {
        WidgetId::NowPlaying => now_playing::spans(&app.widgets.now_playing, &cfg.now_playing),
        WidgetId::SystemUsage => system_usage::spans(&app.widgets.usage, &cfg.usage),
        WidgetId::Volume => volume::spans(&app.widgets.volume, &cfg.volume),
        WidgetId::Network => network::spans(app),
        WidgetId::Bluetooth => bluetooth::spans(app),
        WidgetId::Battery => battery::spans(app),
        WidgetId::Tray => tray::spans(app),
        WidgetId::Clock => clock::spans(app),
        WidgetId::Custom(name) => match cfg.spec(name) {
            Some(spec) => custom::spans(custom_out(app, spec).as_ref(), spec),
            None => Spans::default(),
        },
    }
}

/// A custom widget's output: its command's, or a `source` block's reading.
fn custom_out(app: &App, spec: &WidgetSpec) -> Option<Output> {
    match &spec.kind {
        Kind::Source { source, format } => custom::resolve(source, format, &app.widgets).map(|text| Output {
            text,
            detail: Vec::new(),
            tooltip: None,
            state: None,
        }),
        _ => app.widgets.custom.get(&spec.name).and_then(|s| s.out.clone()),
    }
}

/// What a pin is tied to: a pin lasts until the widget's content changes
/// category (ADR 0065) — for Now Playing, a different player.
pub fn category(app: &App, id: &WidgetId) -> String {
    match id {
        WidgetId::NowPlaying => app
            .widgets
            .now_playing
            .player
            .as_ref()
            .map(|p| p.player.clone())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn parts<'a>(app: &'a App, id: &WidgetId, frame: ShellFrame) -> Parts<'a> {
    let cfg = &app.widget_cfg;
    match id {
        WidgetId::NowPlaying => now_playing::view(
            &app.widgets.now_playing,
            &cfg.now_playing,
            frame,
            app.motion.art.value(),
        ),
        WidgetId::SystemUsage => system_usage::view(&app.widgets.usage, &cfg.usage, frame),
        WidgetId::Volume => volume::view(&app.widgets.volume, &cfg.volume, frame),
        WidgetId::Network => network::view(app, frame),
        WidgetId::Bluetooth => bluetooth::view(app, frame),
        WidgetId::Battery => battery::view(app, frame),
        WidgetId::Tray => tray::view(app, frame),
        WidgetId::Clock => clock::view(app, frame),
        WidgetId::Custom(name) => match cfg.spec(name) {
            Some(spec) => custom::view(custom_out(app, spec), spec, frame),
            None => Parts::empty(),
        },
    }
}

/// One widget as the row places it.
pub struct Cell<'a> {
    /// How far it has arrived: its leading gap opens with it.
    pub presence: f32,
    /// How far it is compressed to its grip: `1.0` is the grip alone. Two
    /// neighbouring grips close their gap up to [`bar::GRIP_RUN_GAP`].
    pub closed: f32,
    pub element: Element<'a, Message, Theme>,
}

/// The widget at `index` of `bar.widgets.order`, on its shell, at the width
/// its animation has reached. `None` once it has animated all the way out.
pub fn cell(app: &App, index: usize) -> Option<Cell<'_>> {
    let mut c = shell(app, index)?;
    if app.widget_cfg.order.get(index) == Some(&WidgetId::Volume) {
        c.element = volume::wheel(&app.widgets.volume, &app.widget_cfg.volume, c.element);
    }
    Some(c)
}

fn shell(app: &App, index: usize) -> Option<Cell<'_>> {
    let id = app.widget_cfg.order.get(index)?;
    let input = app.widget_inputs.get(index)?;
    let key = id.key();
    let anim = app.motion.widgets.get(&key)?;
    let presence = anim.presence.value().clamp(0.0, 1.0);
    if presence <= 0.0 {
        return None;
    }
    let extent = anim.extent.value().max(0.0);
    let core_run = input.core_run();
    let reveal_run = input.reveal_run();
    let frame = ShellFrame {
        open: (extent.min(core_run) / core_run).clamp(0.0, 1.0),
        reveal: if reveal_run > 0.0 {
            ((extent - core_run).max(0.0) / reveal_run).clamp(0.0, 1.0)
        } else {
            0.0
        },
        presence,
    };
    let Parts { core, revealed } = parts(app, id, frame);

    if !input.grip() {
        // Nothing to drag: an important widget with no revealed section is a
        // plain glass cell, clipped the same way while it comes and goes.
        let body = container(fixed(core, input.core)).padding([0.0, bar::WIDGET_X]);
        let visible = (core_run * presence).round();
        return Some(Cell {
            presence,
            closed: 0.0,
            element: parts::glass_cell_faded(
                body,
                core_run,
                visible,
                false,
                ClipEdge::Right,
                frame.ground_alpha(),
            ),
        });
    }

    let live = app.motion.drag.as_ref().is_some_and(|d| d.key == key);
    let (press, drag_key, release) = (key.clone(), key.clone(), key);
    let grip = parts::drag_bar()
        .state(if live { Grip::Active } else { Grip::Rest })
        .opacity(frame.grip_alpha())
        .on_press(Message::Grip(press, GripEv::Press))
        .on_drag(move |dx| Message::Grip(drag_key.clone(), GripEv::Drag(dx)))
        .on_release(Message::Grip(release, GripEv::Release));
    let span = ShellSpan {
        core: input.core,
        revealed: input.revealed,
    };
    let revealed = revealed.filter(|_| input.revealed > 0.0);
    Some(Cell {
        presence,
        closed: 1.0 - frame.open.max(frame.reveal),
        element: parts::widget_shell(grip, core, revealed, span, frame),
    })
}

/// `content` in a box exactly `width` wide and the cell's height, centred.
pub(crate) fn fixed<'a>(
    content: impl Into<Element<'a, Message, Theme>>,
    width: f32,
) -> Element<'a, Message, Theme> {
    container(content)
        .width(Length::Fixed(width))
        .height(Length::Fixed(bar::WIDGET_H))
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .into()
}

/// `content` as the whole of a cell's press: the glass cell under it already
/// answers the pointer, so the button itself draws nothing.
pub(crate) fn press<'a>(
    content: impl Into<Element<'a, Message, Theme>>,
    width: f32,
    message: Message,
) -> Element<'a, Message, Theme> {
    button(fixed(content, width))
        .width(Length::Fixed(width))
        .height(Length::Fixed(bar::WIDGET_H))
        .padding(0)
        .style(bare)
        .on_press(message)
        .into()
}

/// A button with no ground of its own: the cell it sits on is the ground.
pub(crate) fn bare(_t: &Theme, _s: button::Status) -> button::Style {
    button::Style {
        background: None,
        text_color: color::TEXT,
        ..button::Style::default()
    }
}

/// One of several presses inside one cell (the tray's items): a small lift
/// of its own under the pointer, so it is clear *which* of them answers.
pub(crate) fn inner(_t: &Theme, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Hovered => Some(iced::Background::Color(color::LIFT_SOFT)),
        button::Status::Pressed => Some(iced::Background::Color(color::LIFT)),
        _ => None,
    };
    button::Style {
        background,
        text_color: color::TEXT,
        border: iced::border::rounded(bar::RADIUS_ICON),
        ..button::Style::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_through_their_config_spelling() {
        for name in [
            "now-playing",
            "system-usage",
            "volume",
            "network",
            "bluetooth",
            "battery",
            "tray",
            "clock",
            "custom:weather",
        ] {
            let id = WidgetId::parse(name).expect("known id");
            assert_eq!(id.key(), name);
        }
        assert_eq!(WidgetId::parse("custom:"), None);
        assert_eq!(WidgetId::parse("sparkles"), None);
    }

    #[test]
    fn the_default_order_ends_at_the_clock() {
        let cfg = Config::default();
        assert_eq!(cfg.order.first(), Some(&WidgetId::NowPlaying));
        assert_eq!(cfg.order.last(), Some(&WidgetId::Clock));
        assert!(cfg.important.contains(&WidgetId::Battery));
    }

    /// One wheel click as layershellev delivers it — the axis source's empty
    /// event, the discrete step, its pixel twin — is one notch.
    #[test]
    fn one_click_of_the_wheel_is_one_notch() {
        use iced::mouse::ScrollDelta;
        let mut n = Notches::default();
        let t = std::time::Instant::now();
        let zero = ScrollDelta::Pixels { x: 0.0, y: 0.0 };
        let total: i32 = [
            zero,
            ScrollDelta::Lines { x: 0.0, y: -1.0 },
            ScrollDelta::Pixels { x: 0.0, y: -15.0 },
        ]
        .into_iter()
        .map(|d| n.count(d, t))
        .sum();
        assert_eq!(total, -1);
        assert_eq!(n.count(zero, t), 0, "a zero delta is never a step down");
    }

    /// A touchpad sends pixels only; they count once a notch's worth builds up.
    #[test]
    fn smooth_scroll_counts_by_travel() {
        use iced::mouse::ScrollDelta;
        let mut n = Notches::default();
        let t = std::time::Instant::now();
        let px = |y| ScrollDelta::Pixels { x: 0.0, y };
        assert_eq!(n.count(px(6.0), t), 0);
        assert_eq!(n.count(px(6.0), t), 0);
        assert_eq!(n.count(px(6.0), t), 1);
        assert_eq!(n.count(px(-4.0), t), 0, "a reversal starts over");
    }
}
