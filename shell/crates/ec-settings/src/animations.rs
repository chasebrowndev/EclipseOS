// SPDX-License-Identifier: AGPL-3.0-only
//! Settings → Effects → Animations: the preset, the speed, and every event.
//!
//! The pane, top to bottom (`docs/COMPOSITION.md`):
//!
//! 1. the header, with a status chip: the preset, and how far it is bent;
//! 2. the hero, a strip of preset cards, each looping a live picture of its
//!    own motion — a window popping in, gliding, closing;
//! 3. a groundless band: the speed as a magnitude, beside the speed and
//!    reduce-motion controls;
//! 4. one panel of event rows, grouped, each with its own tiny loop.
//!
//! Accent ledger: the pane's one live yellow is the selected preset card's
//! label (the [`CellTone::Focused`] glass cell). The speed slider's fill and
//! the reduce-motion toggle are control chrome, which `docs/STYLE.md` allows.
//!
//! Every picture is resolved with the compositor's own
//! [`Animations::resolve`], so a card cannot preview a table abyss does not
//! run, and `bounce` previews on `ec_ui::motion`'s own underdamped curve.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use iced::widget::{button, column, container, pick_list, row, text, Column, Space};
use iced::{Alignment, Element, Length, Task, Theme};
use serde_json::{json, Value};

use ec_abyss_config::animations::{
    is_addon_style, Animations, Curve as AnimCurve, Event, Override, Preset, Resolved,
};
use ec_ui::motion::{Animated, Curve, Motion};
use ec_ui::theme::{self, CellTone};
use ec_ui::tokens::{canvas, color, font, motion as tmotion, radius, size, space};
use ec_ui::widget::{
    badge, big_value, hairline, micro_label, motion_stage, panel, pill, value as mono, StageFrame,
};

use crate::app::{App, Message};
use crate::pane::Page;

const PRESET: &str = "animations.preset";
const SPEED: &str = "animations.speed";
const REDUCE: &str = "animations.reduce-motion";
/// The three keys an event's override is made of.
const FIELDS: [&str; 3] = ["style", "duration-ms", "curve"];

/// The event rows, in the groups the pane shows them in, each with the name
/// it goes by under its group's heading.
const GROUPS: [(&str, &[(Event, &str)]); 4] = [
    (
        "windows",
        &[
            (Event::WindowOpen, "Open"),
            (Event::WindowClose, "Close"),
            (Event::WindowMove, "Move"),
            (Event::Minimize, "Minimize"),
            (Event::Unminimize, "Unminimize"),
            (Event::Fullscreen, "Fullscreen"),
            (Event::Focus, "Focus"),
        ],
    ),
    (
        "workspaces",
        &[
            (Event::WorkspaceSwitch, "Switch"),
            (Event::WindowToWorkspace, "Window to workspace"),
        ],
    ),
    (
        "panels",
        &[
            (Event::LayerOpen, "Open"),
            (Event::LayerClose, "Close"),
            (Event::Toast, "Notification"),
        ],
    ),
    (
        "taskbar",
        &[
            (Event::ChipAdd, "Window added"),
            (Event::ChipRemove, "Window removed"),
            (Event::BarLayout, "Layout change"),
        ],
    ),
];

/// The durations an event row's stepper walks, in ms. Dense where the
/// presets live, sparse past them.
const LADDER: [u32; 18] = [
    0, 60, 80, 100, 120, 150, 180, 220, 250, 280, 320, 400, 500, 650, 800, 1000, 1500, 2000,
];

#[derive(Debug, Clone)]
pub enum Msg {
    /// A preset card: the preset, and every override cleared, in one write.
    Preset(Preset),
    /// An event row's stepper: its `duration-ms` override.
    Duration(Event, u32),
    /// An event row's reset: its override removed.
    Reset(Event),
    /// The previews' frame clock.
    Frame(Instant),
}

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    match msg {
        Msg::Preset(p) => {
            let model = model(app);
            let mut edits = vec![(PRESET.to_owned(), json!(p.key()))];
            for ev in model.overrides.keys() {
                for f in FIELDS {
                    edits.push((path(*ev, f), Value::Null));
                }
            }
            app.write_many(&edits);
        }
        Msg::Duration(ev, ms) => {
            if ms != base(&model(app), ev).duration_ms {
                app.write(&path(ev, "duration-ms"), json!(ms));
            }
        }
        Msg::Reset(ev) => {
            let edits: Vec<_> = FIELDS.iter().map(|f| (path(ev, f), Value::Null)).collect();
            app.write_many(&edits);
        }
        Msg::Frame(now) => {
            for (_, p) in &mut app.anim.cards {
                p.tick(now);
            }
            for p in app.anim.rows.values_mut() {
                p.tick(now);
            }
        }
    }
    Task::none()
}

/// `animations.<event>.<field>`.
fn path(ev: Event, field: &str) -> String {
    format!("animations.{}.{field}", ev.key())
}

/// The row a search hit on `target` should land on: an event's three keys
/// share one row, found by its style key. Anything else is its own row.
pub fn row_of(target: &str) -> String {
    let event = target
        .strip_prefix("animations.")
        .and_then(|rest| rest.split_once('.'))
        .and_then(|(ev, _)| Event::parse(ev));
    match event {
        Some(ev) => path(ev, "style"),
        None => target.to_owned(),
    }
}

/// The config as the rows report it, with a held speed slider's position in
/// place of the written speed, so the cards follow the knob.
fn model(app: &App) -> Animations {
    let v = |p: &str| app.key(p).map(|k| &k.value);
    let s = |p: &str| v(p).and_then(Value::as_str).map(str::to_owned);
    let fallback = Animations::default();
    let mut overrides = BTreeMap::new();
    for ev in Event::ALL {
        let o = Override {
            style: s(&path(ev, "style")),
            duration_ms: v(&path(ev, "duration-ms"))
                .and_then(Value::as_u64)
                .map(|ms| ms as u32),
            curve: s(&path(ev, "curve")).as_deref().and_then(AnimCurve::parse),
        };
        if !o.is_empty() {
            overrides.insert(ev, o);
        }
    }
    Animations {
        preset: s(PRESET)
            .as_deref()
            .and_then(Preset::parse)
            .unwrap_or(fallback.preset),
        speed: app
            .live
            .get(SPEED)
            .copied()
            .or_else(|| v(SPEED).and_then(Value::as_f64))
            .unwrap_or(fallback.speed),
        reduce_motion: v(REDUCE)
            .and_then(Value::as_bool)
            .unwrap_or(fallback.reduce_motion),
        overrides,
    }
}

/// What an event row's controls show: the preset and the override, before
/// speed and reduce-motion bend it. It is what the controls write.
fn base(m: &Animations, ev: Event) -> Resolved {
    Animations {
        speed: 1.0,
        reduce_motion: false,
        ..m.clone()
    }
    .resolve(ev)
}

/// The styles an event's picker offers: the compositor's list
/// (`get_config.animations.styles`) when it sent one, else the built-ins and
/// the current add-on style.
fn styles(app: &App, m: &Animations, ev: Event) -> Vec<String> {
    let sent = app.conn.animations["styles"][ev.key()].as_array().map(|a| {
        a.iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>()
    });
    sent.filter(|l| !l.is_empty()).unwrap_or_else(|| {
        let mut l: Vec<String> = ev.styles().iter().map(|s| (*s).to_owned()).collect();
        let style = base(m, ev).style;
        if !ev.is_builtin(&style) {
            l.push(style);
        }
        l
    })
}

// ------------------------------------------------------------------ previews

/// One leg of a preview's loop: the window from one pose to the next under
/// one motion, then a rest before the next leg.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Leg {
    from: StageFrame,
    to: StageFrame,
    motion: Motion,
    hold: Duration,
}

/// A looping picture of one or more events, driven by one [`Animated`]
/// progress per leg — the same motion clock the taskbar runs on.
#[derive(Debug, Clone)]
pub struct Preview {
    legs: Vec<Leg>,
    at: usize,
    p: Animated,
    held: Option<Instant>,
}

impl Preview {
    /// Starts resting at the end of its last leg, so the loop opens on the
    /// last leg's rest rather than mid-movement.
    fn new(legs: Vec<Leg>) -> Self {
        let at = legs.len().saturating_sub(1);
        Preview {
            legs,
            at,
            p: Animated::new(1.0, Motion::SNAP),
            held: None,
        }
    }

    fn tick(&mut self, now: Instant) {
        if self.legs.is_empty() {
            return;
        }
        self.p.tick(now);
        if self.p.animating() {
            return;
        }
        let since = *self.held.get_or_insert(now);
        if now.duration_since(since) < self.legs[self.at].hold {
            return;
        }
        self.at = (self.at + 1) % self.legs.len();
        self.held = None;
        self.p = Animated::new(0.0, self.legs[self.at].motion);
        self.p.set_target(1.0, now);
    }

    fn frame(&self) -> StageFrame {
        match self.legs.get(self.at) {
            Some(l) => lerp(l.from, l.to, self.p.value()),
            None => StageFrame::REST,
        }
    }
}

/// The previews on screen, rebuilt only when what they picture changes.
#[derive(Debug, Clone, Default)]
pub struct Anim {
    cards: Vec<(Card, Preview)>,
    rows: BTreeMap<Event, Preview>,
}

/// A card in the hero strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Card {
    Preset(Preset),
    /// The live config, overrides and all, named for the preset under it.
    Custom(Preset),
}

/// Bring the previews in line with the config. A preview whose legs are
/// unchanged keeps its place in the loop.
pub fn sync(app: &mut App) {
    let m = model(app);
    let mut cards: Vec<Card> = Preset::ALL.into_iter().map(Card::Preset).collect();
    if m.custom() {
        cards.push(Card::Custom(m.preset));
    }
    let old = std::mem::take(&mut app.anim.cards);
    app.anim.cards = cards
        .into_iter()
        .map(|card| {
            let source = match card {
                Card::Preset(p) => Animations {
                    preset: p,
                    overrides: BTreeMap::new(),
                    ..m.clone()
                },
                Card::Custom(_) => m.clone(),
            };
            let legs = card_legs(&source);
            match old.iter().find(|(c, p)| *c == card && p.legs == legs) {
                Some((_, p)) => (card, p.clone()),
                None => (card, Preview::new(legs)),
            }
        })
        .collect();
    for ev in Event::ALL {
        let legs = event_legs(ev, &m.resolve(ev));
        let keep = app.anim.rows.get(&ev).is_some_and(|p| p.legs == legs);
        if !keep {
            app.anim.rows.insert(ev, Preview::new(legs));
        }
    }
}

fn lerp(a: StageFrame, b: StageFrame, t: f32) -> StageFrame {
    let l = |x: f32, y: f32| x + (y - x) * t;
    StageFrame {
        x: l(a.x, b.x),
        y: l(a.y, b.y),
        scale_x: l(a.scale_x, b.scale_x),
        scale_y: l(a.scale_y, b.scale_y),
        alpha: l(a.alpha, b.alpha),
        lit: l(a.lit, b.lit),
    }
}

/// A resolved row as the preview's motion.
fn motion(r: &Resolved) -> Motion {
    Motion {
        enabled: !r.off(),
        curve: match r.curve {
            AnimCurve::Linear => Curve::Linear,
            AnimCurve::EaseIn => Curve::EaseIn,
            AnimCurve::EaseOut => Curve::EaseOut,
            AnimCurve::EaseInOut => Curve::EaseInOut,
            AnimCurve::Spring => Curve::Spring,
            AnimCurve::Bounce => Curve::Bounce,
        },
        duration: Duration::from_millis(u64::from(r.duration_ms)),
    }
}

/// The style the engine runs: an add-on style it does not know falls back
/// to the event's first built-in, as the compositor does.
fn style(ev: Event, r: &Resolved) -> &'static str {
    ev.styles()
        .iter()
        .copied()
        .find(|s| *s == r.style)
        .unwrap_or(ev.styles()[0])
}

fn at(x: f32) -> StageFrame {
    StageFrame {
        x,
        ..StageFrame::REST
    }
}

/// Where a window is before it appears, or after it goes, under `style`.
fn gone(ev: Event, style: &str, rest: StageFrame) -> StageFrame {
    let scaled = |s: f32| StageFrame {
        scale_x: rest.scale_x * s,
        scale_y: rest.scale_y * s,
        alpha: 0.0,
        ..rest
    };
    match style {
        "pop" => scaled(0.85),
        "zoom" => scaled(0.4),
        "slide" if matches!(ev, Event::LayerOpen | Event::LayerClose) => StageFrame {
            y: 0.0,
            alpha: 0.0,
            ..rest
        },
        "slide" if ev == Event::Toast => StageFrame {
            x: 1.0,
            alpha: 0.0,
            ..rest
        },
        "slide" => StageFrame {
            y: 1.0,
            alpha: 0.0,
            ..rest
        },
        "grow" => StageFrame { scale_x: 0.0, ..rest },
        "shrink" => StageFrame {
            y: 1.0,
            ..scaled(0.2)
        },
        _ => StageFrame { alpha: 0.0, ..rest },
    }
}

fn hold() -> Duration {
    Duration::from_millis(tmotion::PREVIEW_HOLD_MS)
}

fn rest() -> Duration {
    Duration::from_millis(tmotion::PREVIEW_REST_MS)
}

fn leg(from: StageFrame, to: StageFrame, motion: Motion, hold: Duration) -> Leg {
    Leg {
        from,
        to,
        motion,
        hold,
    }
}

/// `a` to `b` by fading out, jumping, and fading back in, each half of the
/// motion's time: how `fade` moves a window that has somewhere to go.
fn fade_swap(a: StageFrame, b: StageFrame, m: Motion, hold: Duration) -> [Leg; 3] {
    let half = Motion {
        duration: m.duration / 2,
        ..m
    };
    let out = |f: StageFrame| StageFrame { alpha: 0.0, ..f };
    [
        leg(a, out(a), half, Duration::ZERO),
        leg(out(a), out(b), Motion::SNAP, Duration::ZERO),
        leg(out(b), b, half, hold),
    ]
}

/// One event's loop on its row's stage.
fn event_legs(ev: Event, r: &Resolved) -> Vec<Leg> {
    let m = motion(r);
    let style = style(ev, r);
    let rest_at = StageFrame::REST;
    match ev {
        Event::WindowOpen | Event::Unminimize | Event::LayerOpen | Event::Toast | Event::ChipAdd => {
            let g = gone(ev, style, rest_at);
            vec![leg(g, rest_at, m, hold()), leg(rest_at, g, Motion::SNAP, rest())]
        }
        Event::WindowClose | Event::Minimize | Event::LayerClose | Event::ChipRemove => {
            let g = gone(ev, style, rest_at);
            vec![leg(rest_at, g, m, rest()), leg(g, rest_at, Motion::SNAP, hold())]
        }
        Event::Focus => {
            let lit = StageFrame { lit: 1.0, ..rest_at };
            vec![leg(rest_at, lit, m, hold()), leg(lit, rest_at, m, hold())]
        }
        Event::Fullscreen => {
            // Scaled far past the stage: the mark clamps to fill it.
            let full = StageFrame {
                scale_x: canvas::STAGE_ROW_W / canvas::STAGE_ROW_WIN_W,
                scale_y: canvas::STAGE_ROW_H / canvas::STAGE_ROW_WIN_H,
                ..rest_at
            };
            there_and_back(style, rest_at, full, m)
        }
        Event::WindowMove | Event::BarLayout | Event::WindowToWorkspace | Event::WorkspaceSwitch => {
            let (a, b) = match style {
                "slide-vertical" => (StageFrame { y: 0.0, ..rest_at }, StageFrame { y: 1.0, ..rest_at }),
                "morph" => (
                    StageFrame {
                        scale_x: 0.8,
                        scale_y: 0.8,
                        ..at(0.0)
                    },
                    StageFrame {
                        scale_x: 1.25,
                        scale_y: 1.25,
                        ..at(1.0)
                    },
                ),
                _ => (at(0.0), at(1.0)),
            };
            there_and_back(style, a, b, m)
        }
    }
}

/// `a` to `b` and back under `m`, or by fading when the style is `fade`.
fn there_and_back(style: &str, a: StageFrame, b: StageFrame, m: Motion) -> Vec<Leg> {
    if style == "fade" {
        let mut legs = fade_swap(a, b, m, hold()).to_vec();
        legs.extend(fade_swap(b, a, m, hold()));
        legs
    } else {
        vec![leg(a, b, m, hold()), leg(b, a, m, hold())]
    }
}

/// A preset card's loop: a window opens at the left, glides right, closes.
fn card_legs(source: &Animations) -> Vec<Leg> {
    let open = source.resolve(Event::WindowOpen);
    let mv = source.resolve(Event::WindowMove);
    let close = source.resolve(Event::WindowClose);
    let left = at(0.1);
    let right = match style(Event::WindowMove, &mv) {
        "morph" => StageFrame {
            scale_x: 1.2,
            scale_y: 1.2,
            ..at(0.9)
        },
        _ => at(0.9),
    };
    let born = gone(Event::WindowOpen, style(Event::WindowOpen, &open), left);
    let died = gone(Event::WindowClose, style(Event::WindowClose, &close), right);
    vec![
        leg(born, left, motion(&open), hold()),
        leg(left, right, motion(&mv), hold()),
        leg(right, died, motion(&close), rest()),
        leg(died, born, Motion::SNAP, Duration::ZERO),
    ]
}

// ---------------------------------------------------------------------- view

/// The header chip: which preset, and how far it is bent.
pub fn status(app: &App) -> (String, String) {
    let m = model(app);
    let state = if m.custom() {
        format!("custom · {}", m.preset.key())
    } else if !m.any() {
        "animations off".to_owned()
    } else {
        format!("preset {}", m.preset.key())
    };
    let bent = m.overrides.len();
    let measure = match bent {
        0 => format!("{:.2}× speed", m.speed),
        1 => format!("1 event changed · {:.2}×", m.speed),
        n => format!("{n} events changed · {:.2}×", m.speed),
    };
    (state, measure)
}

pub fn blocks(app: &App) -> Vec<Element<'_, Message, Theme>> {
    let m = model(app);
    vec![hero(app, &m), speed_band(app, &m), events(app, &m)]
}

/// Sentence-case name of a preset.
fn preset_name(p: Preset) -> &'static str {
    match p {
        Preset::Off => "Off",
        Preset::Subtle => "Subtle",
        Preset::Smooth => "Smooth",
        Preset::Lively => "Lively",
    }
}

/// How a resolved row reads in mono: `pop · 220 ms`, or `snaps`.
fn reading(r: &Resolved) -> String {
    if r.off() {
        "snaps".to_owned()
    } else {
        format!("{} · {} ms", r.style, r.duration_ms)
    }
}

/// A pill with nothing behind it: no press, tertiary ink.
fn dead_pill<'a>(label: &str) -> Element<'a, Message, Theme> {
    button(
        text(label.to_owned())
            .font(font::UI_MEDIUM)
            .size(size::BODY_SMALL)
            .color(color::TEXT_TERTIARY),
    )
    .padding([space::PILL_Y, space::PILL_X])
    .style(theme::pill(false))
    .into()
}

fn caption<'a>(t: String) -> Element<'a, Message, Theme> {
    text(t)
        .font(font::DATA)
        .size(size::MICRO)
        .style(theme::text_tertiary)
        .wrapping(text::Wrapping::None)
        .into()
}

/// The hero: one glass card per preset, each looping its own motion. The
/// selected card is the glass a step clearer, its label in gold.
fn hero<'a>(app: &'a App, m: &Animations) -> Element<'a, Message, Theme> {
    let mut strip = iced::widget::Row::new().spacing(space::GRID_GAP);
    for (card, preview) in &app.anim.cards {
        let (name, note, selected, press) = match *card {
            Card::Preset(p) => {
                let shown = Animations {
                    preset: p,
                    overrides: BTreeMap::new(),
                    ..m.clone()
                };
                (
                    preset_name(p).to_owned(),
                    reading(&shown.resolve(Event::WindowOpen)),
                    !m.custom() && m.preset == p,
                    p,
                )
            }
            Card::Custom(p) => ("Custom".to_owned(), format!("based on {}", p.key()), true, p),
        };
        let body = column![
            container(motion_stage(
                (canvas::STAGE_W, canvas::STAGE_H),
                (canvas::STAGE_WIN_W, canvas::STAGE_WIN_H),
                preview.frame(),
            ))
            .width(Length::Fill)
            .align_x(Alignment::Center),
            // No style of its own: the cell's text colour is the gold.
            text(name).font(font::UI_MEDIUM).size(size::CARD_TITLE),
            caption(note),
        ]
        .spacing(space::LINE_GAP * 2.0);
        strip = strip.push(
            button(body)
                .padding(space::CONTROL_GAP)
                .width(Length::Fill)
                .on_press(Message::Anim(Msg::Preset(press)))
                .style(theme::glass_cell(
                    if selected {
                        CellTone::Focused
                    } else {
                        CellTone::Plain
                    },
                    radius::INSET,
                )),
        );
    }
    panel(
        app.glass_radius,
        column![micro_label("preset"), strip].spacing(space::ROW_Y),
    )
    .into()
}

/// The speed as a magnitude, beside the two controls that bend every event.
fn speed_band<'a>(app: &'a App, m: &Animations) -> Element<'a, Message, Theme> {
    let mut controls = Column::new();
    for p in [SPEED, REDUCE] {
        if let Some(key) = app.key(p) {
            controls = controls.push(crate::app::setting_row(app, &key.label(), key));
        }
    }
    let note = if m.reduce_motion {
        "reduce motion: fades only"
    } else {
        "divides every duration"
    };
    container(
        row![
            column![
                micro_label("speed"),
                big_value(&format!("{:.2}", m.speed), "×", false),
                caption(note.to_owned()),
            ]
            .spacing(space::PILL_GAP)
            .width(Length::Fixed(space::FIELD_W)),
            container(controls).width(Length::Fill),
        ]
        .spacing(space::BLOCK)
        .align_y(Alignment::Center),
    )
    .padding([0.0, space::CARD])
    .width(Length::Fill)
    .into()
}

/// Every event, grouped, with the add-on note at the foot.
fn events<'a>(app: &'a App, m: &Animations) -> Element<'a, Message, Theme> {
    let mut col = Column::new().spacing(space::LINE_GAP);
    let mut any_addon = false;
    for (i, (group, evs)) in GROUPS.iter().enumerate() {
        if i > 0 {
            col = col.push(hairline());
        }
        col = col.push(container(micro_label(group)).padding([space::ROW_Y, space::CARD]));
        for (ev, name) in *evs {
            let list = styles(app, m, *ev);
            any_addon |= list.iter().any(|s| is_addon_style(s));
            col = col.push(event_row(app, m, *ev, name, list));
        }
    }
    if !any_addon {
        col = col.push(hairline()).push(
            container(
                row![
                    text("More styles come with add-on packs.")
                        .font(font::UI)
                        .size(size::BODY_SMALL)
                        .style(theme::text_tertiary),
                    Space::new().width(Length::Fill),
                    pill("Add-ons", false, Message::Open(Page::Addons)),
                ]
                .spacing(space::CONTROL_GAP)
                .align_y(Alignment::Center),
            )
            .padding([space::ROW_Y, space::CARD]),
        );
    }
    panel(app.glass_radius, col).into()
}

/// One event: its loop, its name and what it runs as, then its style,
/// speed and curve — and, while it differs from the preset, a way back.
fn event_row<'a>(
    app: &'a App,
    m: &Animations,
    ev: Event,
    name: &str,
    list: Vec<String>,
) -> Element<'a, Message, Theme> {
    let b = base(m, ev);
    let overridden = m.overrides.contains_key(&ev);
    let stage = match app.anim.rows.get(&ev) {
        Some(p) => p.frame(),
        None => StageFrame::REST,
    };
    let style_path = path(ev, "style");
    let curve_path = path(ev, "curve");
    let ms = b.duration_ms;
    let down = LADDER
        .iter()
        .rev()
        .copied()
        .find(|v| *v < ms)
        .unwrap_or(LADDER[0]);
    let up = LADDER
        .iter()
        .copied()
        .find(|v| *v > ms)
        .unwrap_or(LADDER[LADDER.len() - 1]);

    let mut marks = iced::widget::Row::new()
        .spacing(space::PILL_GAP)
        .align_y(Alignment::Center);
    if is_addon_style(&b.style) {
        marks = marks.push(badge("add-on"));
    }
    if overridden {
        marks = marks
            .push(badge("custom"))
            .push(pill("Reset", false, Message::Anim(Msg::Reset(ev))));
    }

    // A style that snaps has no duration or curve to bend: the stepper and
    // curve picker are shown dim and do nothing, so "+" cannot write an
    // override the engine would ignore. The style picker stays live — it is
    // the way back.
    let inert = m.preset == Preset::Off || b.style == "none";
    let stepper = if inert {
        row![
            dead_pill("−"),
            container(caption(format!("{ms} ms")))
                .width(Length::Fixed(canvas::EVENT_MS_W))
                .align_x(Alignment::Center),
            dead_pill("+"),
        ]
    } else {
        row![
            pill("−", false, Message::Anim(Msg::Duration(ev, down))),
            container(mono(&format!("{ms} ms")))
                .width(Length::Fixed(canvas::EVENT_MS_W))
                .align_x(Alignment::Center),
            pill("+", false, Message::Anim(Msg::Duration(ev, up))),
        ]
    }
    .spacing(space::PILL_GAP)
    .align_y(Alignment::Center);

    let curves: Vec<String> = AnimCurve::ALL.iter().map(|c| c.key().to_owned()).collect();
    let curve_pick: Element<'a, Message, Theme> = if inert {
        container(caption(b.curve.key().to_owned()))
            .padding([space::PILL_Y, space::PILL_X])
            .width(Length::Fixed(canvas::EVENT_PICK_W))
            .style(theme::inset)
            .into()
    } else {
        pick_list(curves, Some(b.curve.key().to_owned()), move |v| {
            Message::Chose(curve_path.clone(), v)
        })
        .width(Length::Fixed(canvas::EVENT_PICK_W))
        .into()
    };
    let lit = app.lit(&style_path);
    container(
        row![
            motion_stage(
                (canvas::STAGE_ROW_W, canvas::STAGE_ROW_H),
                (canvas::STAGE_ROW_WIN_W, canvas::STAGE_ROW_WIN_H),
                stage,
            ),
            column![
                text(name.to_owned())
                    .font(font::UI)
                    .size(size::BODY)
                    .style(theme::text_secondary),
                caption(reading(&m.resolve(ev))),
            ]
            .spacing(space::LINE_GAP)
            .width(Length::Fixed(canvas::EVENT_NAME_W)),
            Space::new().width(Length::Fill),
            marks,
            pick_list(list, Some(b.style.clone()), move |v| {
                Message::Chose(style_path.clone(), v)
            })
            .width(Length::Fixed(canvas::EVENT_PICK_W)),
            stepper,
            curve_pick,
        ]
        .spacing(space::CONTROL_GAP)
        .align_y(Alignment::Center),
    )
    .id(crate::app::row_id(&path(ev, "style")))
    .padding([space::LINE_GAP * 2.0, space::CARD])
    .width(Length::Fill)
    .style(move |_t: &Theme| container::Style {
        background: Some(iced::Background::Color(color::HIGHLIGHT.scale_alpha(lit))),
        border: iced::Border {
            color: color::BORDER_STRONG.scale_alpha(lit),
            width: space::HAIRLINE,
            radius: radius::INSET.into(),
        },
        ..container::Style::default()
    })
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_event_has_a_row() {
        let shown: Vec<Event> = GROUPS
            .iter()
            .flat_map(|(_, e)| e.iter().map(|(ev, _)| *ev))
            .collect();
        for ev in Event::ALL {
            assert_eq!(shown.iter().filter(|e| **e == ev).count(), 1, "{}", ev.key());
        }
    }

    #[test]
    fn an_event_key_lands_on_its_style_row() {
        assert_eq!(row_of("animations.focus.curve"), "animations.focus.style");
        assert_eq!(row_of("animations.speed"), "animations.speed");
    }

    #[test]
    fn off_snaps_every_leg_of_a_card() {
        let legs = card_legs(&Animations::default());
        assert!(legs.iter().all(|l| l.motion.snaps()));
        let smooth = Animations {
            preset: Preset::Smooth,
            ..Animations::default()
        };
        assert!(!card_legs(&smooth)[0].motion.snaps());
    }

    #[test]
    fn a_preview_loops_back_to_its_first_leg() {
        let mut p = Preview::new(event_legs(
            Event::WindowOpen,
            &Animations {
                preset: Preset::Smooth,
                ..Animations::default()
            }
            .resolve(Event::WindowOpen),
        ));
        let t0 = Instant::now();
        let mut now = t0;
        let mut seen = std::collections::HashSet::new();
        while now.duration_since(t0) < Duration::from_secs(4) {
            p.tick(now);
            seen.insert(p.at);
            now += Duration::from_millis(16);
        }
        assert_eq!(seen.len(), 2);
    }

    #[test]
    fn bounce_previews_as_bounce() {
        let r = Resolved {
            style: "zoom".into(),
            duration_ms: 320,
            curve: AnimCurve::Bounce,
        };
        assert_eq!(motion(&r).curve, Curve::Bounce);
    }
}
