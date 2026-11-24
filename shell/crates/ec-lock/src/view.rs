// SPDX-License-Identifier: AGPL-3.0-only
//! The lock screen (ADR 0095, 0126). Owned by eclipse-frontend.
//!
//! Drawn from the user's tile layout (`face.rs`): each tile is placed on the
//! 24x14 grid, scaled to whichever output this is, and holds one thing.
//!
//! ```text
//!                    9:41 AM            Clock: bare type, the one numeral
//!             Wednesday, November 11
//!            ╭───────────────────────╮
//!            │ (R) robin      LOCKED │  Greeting: a pill, the account
//!            ╰───────────────────────╯
//!            ╭───────────────────────╮
//!            │ ›  ● ● ● ●|  (Unlock) │  Password: the field, the note, hints
//!            │ Enter your password   │
//!            │ [enter] unlock [esc]  │
//!            ╰───────────────────────╯   (+ Battery, Wifi where placed)
//! ```
//!
//! No two neighbours share a silhouette: bare type, a pill, a field pane, a
//! figure with a rail, a figure with bars.
//!
//! **Accent ledger.** The one yellow is the typed length: the dot run and its
//! caret, while the field takes keys. Unlock is a white lift, not a gold
//! button; a failure is `danger`, not accent. Focus is a brighter fill and a
//! crisp rim, never a glow.
//!
//! **Glass, with an opaque fall-back (ADR 0129).** Under abyss in `glass` blur
//! mode the compositor draws the wallpaper, then this surface's
//! `eclipse_glass_v1` panes (blurring *the wallpaper only*: no window, bar or
//! layer is ever sampled while locked), then this surface. So when
//! [`glass_live`] the client draws **no ground** and every pane (the clock, the
//! greeting, the password card, its field, Unlock, the Glances) pushes a pane
//! and keeps only its content. Anywhere else (another compositor, a blur mode
//! other than `glass`, no global, an output the compositor will not draw glass
//! on) the surface keeps today's look: an **opaque** ground and glass-styled
//! panes (a tint, a rim, a soft drop), so the lock is never see-through and
//! never unreadable. `style`/[`ground_for`] decide per frame; the glass is
//! live only once the compositor has said so, before the first frame, and the
//! panes travel in the same commit as the buffer, so no frame has text on a
//! ground that is not there.
//!
//! **Panic-free and never in the way.** Sizes go through `ec_glance::cells`, whose plans
//! never return more than the room it was given; every index is checked; a
//! layout with no Password tile gets the built-in one added. The layout and
//! the Glance numbers are only *drawn*: nothing here reaches `update`.
//!
//! The contract (see `app.rs`): read only `App::{status, typed, user, clock,
//! can_submit, face}`; emit only `Message::Submit`; no `text_input`; never a
//! window-close task.

use iced::widget::{button, column, container, pin, responsive, row, stack, text, Space};
use iced::{alignment, Alignment, Color, Element, Font, Length, Shadow, Size, Theme, Vector};

use ec_glance::cells;
use ec_glance::tile::{self, BatteryPlan, Inner, WifiPlan};
use ec_glance::{clean, Charge};
use ec_layout::{Kind, Rect};
use ec_ui::theme::{self, CellTone};
use ec_ui::tokens::{color, font, lock, radius, size, space};
use ec_ui::widget::{dot_run, glass_card, glass_ground, secret_field, spinner, Fade, FieldState};

use crate::app::{App, Message, Status};
use crate::clock::Reading;
use crate::face::{self, Face};
use crate::machine::Reason;
use crate::tokens::{self, cell};

/// The field's marker, as in the secret prompt.
const MARKER: &str = cells::lock::MARKER;

/// Everything the view draws, read once from the model. `pub` so the preview
/// example can draw any state without a lock, a PAM stack or a grant.
#[derive(Debug, Clone, Copy)]
pub struct Scene<'a> {
    pub status: Status,
    pub typed: usize,
    pub user: &'a str,
    pub clock: &'a Reading,
    pub can_submit: bool,
    pub face: &'a Face,
}

impl<'a> Scene<'a> {
    pub fn of(app: &'a App) -> Self {
        Scene {
            status: app.status(),
            typed: app.typed(),
            user: app.user(),
            clock: app.clock(),
            can_submit: app.can_submit(),
            face: app.face(),
        }
    }

    /// Whether keys land in the field (the machine's `accepts_input`).
    fn taking(&self) -> bool {
        matches!(
            self.status,
            Status::Locking | Status::Ready | Status::Failed { .. }
        )
    }
}

pub fn view(app: &App, window: iced::window::Id) -> Element<'_, Message, Theme> {
    // Sends this output's panes once the content has drawn them (a no-op in
    // the fall-back).
    // The entrance advances on frames, and a lock surface has no input to
    // cause them, so it asks for them while it plays.
    ec_ui::glass::root(window, scene(Scene::of(app)))
        .animating(app.face().animating())
        .into()
}

/// Whether the compositor is drawing this surface's panes, so the client draws
/// no ground of its own.
pub fn glass_live() -> bool {
    ec_ui::glass::available()
}

/// The lock screen for one output. Every output gets the same one, laid out
/// on its own size.
pub fn scene(s: Scene<'_>) -> Element<'_, Message, Theme> {
    responsive(move |output| tiles(s, output)).into()
}

/// What one tile's content needs: its inner box.
struct Cx<'a> {
    s: Scene<'a>,
    iw: f32,
    ih: f32,
}

fn tiles<'a>(s: Scene<'a>, output: Size) -> Element<'a, Message, Theme> {
    let layout = s.face.layout();
    let mut placed: Vec<(usize, Kind, Rect)> = layout
        .tiles
        .iter()
        .enumerate()
        .map(|(i, t)| (i, t.kind, t.rect))
        .collect();
    // The way in is never optional, whatever the layout says.
    if layout.get(Kind::Password).is_none() {
        if let Some(r) = face::builtin().get(Kind::Password) {
            placed.push((placed.len(), Kind::Password, r));
        }
    }
    let (ow, oh) = (output.width.max(0.0) as u32, output.height.max(0.0) as u32);
    let mut out: Vec<Element<'a, Message, Theme>> = Vec::new();
    for (i, kind, rect) in placed {
        let (x, y, w, h) = rect.to_px(ow, oh, tokens::TILE_GAP);
        let (w, h) = (w as f32, h as f32);
        let appeared = s.face.appeared(i, kind);
        let Some(tile) = tile(s, kind, w, h, appeared) else {
            continue;
        };
        // `pin` lays its child out in the room left after its own offset, so
        // the pin fills the output and the box carries the tile's size.
        let boxed = container(tile).width(Length::Fixed(w)).height(Length::Fixed(h));
        out.push(
            pin(boxed)
                .x(x as f32)
                .y(y as f32 + (1.0 - appeared) * tokens::motion::RISE)
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
        );
    }
    stack(out).width(Length::Fill).height(Length::Fill).into()
}

/// The Glances' text sizes, for `ec_glance::tile`.
const GLANCE_TYPE: tile::Type = tile::Type {
    micro: size::MICRO,
    body_small: size::BODY_SMALL,
    body: size::BODY,
};

fn tile<'a>(s: Scene<'a>, kind: Kind, w: f32, h: f32, appeared: f32) -> Option<Element<'a, Message, Theme>> {
    // The shared frame: a pill's content keeps clear of its curve.
    let frame = cells::lock_frame(w, h, kind == Kind::Greeting);
    let (pad, pad_x) = (frame.pad, frame.pad_x);
    let cx = Cx {
        s,
        iw: frame.inner.w,
        ih: frame.inner.h,
    };
    let glass = glass_live();
    // The clock is bare type on the opaque fall-back, where it needs no
    // ground. On glass it has none to lean on (bare type on any wallpaper can
    // fail contrast), so it is a pane like the rest.
    let (content, radius) = match kind {
        Kind::Clock if !glass => return Some(fading(clock(&cx), appeared)),
        Kind::Clock => (clock(&cx), tokens::PANE_RADIUS),
        Kind::Greeting => (greeting(&cx), tokens::PILL_RADIUS),
        Kind::Password => (password(&cx), tokens::PANE_RADIUS),
        Kind::Battery => (battery(&cx), tokens::PANE_RADIUS),
        Kind::Wifi => (wifi(&cx), tokens::PANE_RADIUS),
        // Not drawn on the lock: one account, no power menu, and a session
        // bus is not for a locked screen.
        Kind::Users | Kind::Power | Kind::NowPlaying => return None,
    };
    let radius = radius.min(w.min(h) / 2.0);
    let body = container(content)
        .padding([pad, pad_x])
        .width(Length::Fill)
        .height(Length::Fill)
        .clip(true);
    if glass {
        // The compositor draws the ground; `Fade` fades its pane and the
        // content stays whole (no veil colour could stand in for glass).
        let fade = Fade::new(
            glass_card(body, CellTone::Plain, radius),
            appeared,
            space::ZERO,
            Color::TRANSPARENT,
        );
        return Some(fade.into());
    }
    // A tile still appearing hides its content under the pane's own ground,
    // so text and cells fade with the pane rather than popping in.
    let body: Element<'a, Message, Theme> = if appeared < 1.0 {
        stack![
            body,
            swatch(
                pane_ground().scale_alpha(1.0 - appeared.clamp(0.0, 1.0)),
                0.0,
                Length::Fill,
                Length::Fill
            )
        ]
        .into()
    } else {
        body.into()
    };
    let fade = appeared.clamp(0.0, 1.0);
    Some(
        container(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_: &Theme| pane_style(radius, fade))
            .into(),
    )
}

/// A bare tile fades in by sitting under its own ground.
fn fading<'a>(content: Element<'a, Message, Theme>, appeared: f32) -> Element<'a, Message, Theme> {
    if appeared >= 1.0 {
        return content;
    }
    stack![
        content,
        swatch(
            color::base().scale_alpha(1.0 - appeared.clamp(0.0, 1.0)),
            0.0,
            Length::Fill,
            Length::Fill
        )
    ]
    .into()
}

/// The pane's fill: the next warm surface up from the ground.
fn pane_ground() -> Color {
    color::surface_2()
}

/// A glass-styled pane on an opaque ground: the panel's fill and rim, with a
/// soft drop. The ground is opaque, so the drop is depth, not a halo.
fn pane_style(radius: f32, fade: f32) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(pane_ground().scale_alpha(fade))),
        border: iced::Border {
            color: color::border_strong().scale_alpha(fade * tokens::RIM),
            width: 1.0,
            radius: radius.into(),
        },
        shadow: Shadow {
            color: color::cell_shadow().scale_alpha(fade),
            offset: Vector::new(space::ZERO, tokens::SHADOW_Y),
            blur_radius: tokens::SHADOW_BLUR,
        },
        ..container::Style::default()
    }
}

/// A small uppercase mono label, in the secondary ink. The desktop's section label
/// is tertiary, which on a light scheme is under 3:1; the lock holds 4.5:1.
fn micro<'a>(word: &str) -> Element<'a, Message, Theme> {
    let spaced: String = word
        .to_uppercase()
        .chars()
        .flat_map(|c| [c, '\u{2009}'])
        .collect();
    text(spaced)
        .font(font::DATA_MEDIUM)
        .size(size::MICRO)
        .style(theme::text_secondary)
        .wrapping(text::Wrapping::None)
        .into()
}

/// The desktop's key hints with the verb in the secondary ink (see [`micro`]): a
/// keycap pane (glass on glass, a styled cell otherwise) and its verb.
fn hints<'a>(pairs: &[(&str, &str)]) -> Element<'a, Message, Theme> {
    pairs
        .iter()
        .fold(
            row![].spacing(space::HINT_GAP).align_y(Alignment::Center),
            |line, &(key, verb)| {
                let cap = container(
                    text(key.to_owned())
                        .font(font::DATA_MEDIUM)
                        .size(size::MICRO)
                        .style(theme::text_secondary)
                        .wrapping(text::Wrapping::None),
                )
                .padding([space::BADGE_Y, space::BADGE_X])
                .style(|t: &Theme| {
                    let c = theme::glass_cell(CellTone::Plain, radius::KEYCAP)(t, button::Status::Active);
                    theme::bare_container_if_glass(container::Style {
                        background: c.background,
                        border: c.border,
                        shadow: c.shadow,
                        ..container::Style::default()
                    })
                });
                line.push(
                    row![
                        glass_card(cap, CellTone::Plain, radius::KEYCAP),
                        text(verb.to_owned())
                            .font(font::DATA)
                            .size(size::MICRO)
                            .style(theme::text_secondary)
                            .wrapping(text::Wrapping::None),
                    ]
                    .spacing(space::KEY_GAP)
                    .align_y(Alignment::Center),
                )
            },
        )
        .into()
}

/// A plain filled shape: a rail, a bar, a veil.
fn swatch<'a>(ink: Color, radius: f32, w: Length, h: Length) -> Element<'a, Message, Theme> {
    container(Space::new())
        .width(w)
        .height(h)
        .style(move |_: &Theme| container::Style {
            background: Some(iced::Background::Color(ink)),
            border: iced::Border {
                radius: radius.into(),
                ..iced::Border::default()
            },
            ..container::Style::default()
        })
        .into()
}

/// One line of text. It never wraps: a tile too small for its words clips them
/// rather than breaking a row into a second line.
fn label<'a>(s: impl Into<String>, px: f32, font: Font, ink: Color) -> Element<'a, Message, Theme> {
    text(s.into())
        .size(px)
        .font(font)
        .color(ink)
        .wrapping(text::Wrapping::None)
        .into()
}

fn clock<'a>(cx: &Cx<'a>) -> Element<'a, Message, Theme> {
    let r = cx.s.clock;
    // The shared rules: sized against the widest strings of the 12/24-hour
    // choice and the date, so the minute rolling over never moves it.
    let plan = tile::clock(Inner { w: cx.iw, h: cx.ih }, r.format.twelve, r.format.date);
    let mut col = column![label(
        r.time.clone(),
        plan.numeral,
        font::UI_MEDIUM,
        color::text()
    )]
    .spacing(tile::CLOCK_GAP);
    if let (Some(date), false) = (plan.date, r.date.is_empty()) {
        col = col.push(label(r.date.clone(), date, font::UI, color::text_secondary()));
    }
    let c = container(col).width(Length::Fill).height(Length::Fill).clip(true);
    match plan.align {
        tile::Align::Centre => c.center(Length::Fill).into(),
        tile::Align::BottomLeft => c.align_y(alignment::Vertical::Bottom).into(),
    }
}

/// The account as one line: its disc, its name, and whether it is locked.
fn greeting<'a>(cx: &Cx<'a>) -> Element<'a, Message, Theme> {
    let s = &cx.s;
    let plan = cells::lock_greeting(Inner { w: cx.iw, h: cx.ih });
    let word = if s.status == Status::Unlocking {
        cells::lock::WORD_UNLOCKED
    } else {
        cells::lock::WORD_LOCKED
    };
    let mut r = row![
        initial_disc(s.user, plan.disc, plan.initial_px),
        label(clean(s.user), plan.name_px, font::UI_MEDIUM, color::text()),
        Space::new().width(Length::Fill),
    ]
    .spacing(space::CONTROL_GAP)
    .align_y(Alignment::Center);
    if plan.word {
        r = r.push(micro(word));
    }
    container(r)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_y(alignment::Vertical::Center)
        .into()
}

/// The account's initial in a pill-round disc, sized to the line it sits on.
/// Not `ec_ui`'s `avatar`: that one's letter is a fixed size, and this disc
/// follows the tile.
fn initial_disc<'a>(name: &str, side: f32, initial_px: f32) -> Element<'a, Message, Theme> {
    let initial: String = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default();
    container(label(
        initial,
        initial_px,
        font::UI_MEDIUM,
        color::text_secondary(),
    ))
    .center(Length::Fixed(side))
    .style(move |_: &Theme| container::Style {
        background: Some(iced::Background::Color(color::lift())),
        border: iced::Border {
            radius: (side / 2.0).into(),
            ..iced::Border::default()
        },
        ..container::Style::default()
    })
    .into()
}

/// The field, its note and, where there is room, the key hints. Centred in
/// whatever the tile gives it.
fn password<'a>(cx: &Cx<'a>) -> Element<'a, Message, Theme> {
    let s = &cx.s;
    let plan = cells::lock_password(Inner { w: cx.iw, h: cx.ih });
    let (room_for_hints, field_h) = (plan.hints, plan.field_h);

    let mut col = column![field(s, field_h, plan.tight), note(s)]
        .spacing(cell::NOTE_GAP)
        .width(Length::Fill);
    if room_for_hints {
        // Nothing to press while PAM holds the password or the lock is going;
        // the line keeps its height so the pane does not move.
        let line: Element<'a, Message, Theme> = if s.taking() {
            hints(&cells::lock::HINTS)
        } else {
            Space::new().height(space::KEYCAP_H).into()
        };
        col = col.push(line);
    }
    container(col).center(Length::Fill).into()
}

fn battery<'a>(cx: &Cx<'a>) -> Element<'a, Message, Theme> {
    let snap = cx.s.face.glance();
    let plan = tile::battery(Inner { w: cx.iw, h: cx.ih }, &snap.batteries, GLANCE_TYPE);
    let Some(first) = snap.batteries.first() else {
        return quiet("No battery");
    };
    match plan {
        BatteryPlan::Empty => quiet("No battery"),
        BatteryPlan::Compact {
            figure,
            rail: with_rail,
        } => {
            let mut r = row![label(
                format!("{}%", first.percent),
                figure,
                font::DATA_MEDIUM,
                color::text()
            )]
            .spacing(tile::CELL_GAP)
            .align_y(Alignment::Center);
            if with_rail {
                r = r.push(rail(first.percent, tile::RAIL_H));
            }
            container(r).width(Length::Fill).center_y(Length::Fill).into()
        }
        BatteryPlan::One {
            figure,
            rail: with_rail,
            note,
        } => {
            let mut col = column![
                micro(cells::lock::HEAD_BATTERY),
                Space::new().height(Length::Fill),
                label(
                    format!("{}%", first.percent),
                    figure,
                    font::DATA_MEDIUM,
                    color::text()
                ),
            ]
            .spacing(tile::GAP)
            .height(Length::Fill);
            if with_rail {
                col = col.push(rail(first.percent, tile::RAIL_H));
            }
            if note {
                col = col.push(label(
                    tile::charge_note(first.charge),
                    size::MICRO,
                    font::UI,
                    color::text_secondary(),
                ));
            }
            col.into()
        }
        BatteryPlan::Many {
            shown,
            rail: with_rail,
            tag,
            gap,
            name_chars,
            scale,
        } => {
            let mut list = column![].spacing(gap);
            for b in snap.batteries.iter().take(shown) {
                let name = if b.internal {
                    "Internal".to_owned()
                } else {
                    clean(&b.name)
                };
                let mut line = row![container(label(
                    tile::truncate(&name, name_chars),
                    size::BODY_SMALL * scale,
                    font::UI,
                    color::text_secondary()
                ))
                .width(Length::Fill)]
                .spacing(tile::CELL_GAP)
                .align_y(Alignment::Center);
                if tag && b.charge == Charge::Charging {
                    line = line.push(label(
                        "Charging",
                        size::MICRO * scale,
                        font::UI,
                        color::text_secondary(),
                    ));
                }
                line = line.push(
                    container(label(
                        format!("{}%", b.percent),
                        size::BODY * scale,
                        font::DATA_MEDIUM,
                        color::text(),
                    ))
                    .align_x(alignment::Horizontal::Right),
                );
                let mut group = column![line].spacing(tile::GAP);
                if with_rail {
                    group = group.push(rail(b.percent, tile::RAIL_H * scale));
                }
                list = list.push(group);
            }
            column![micro(cells::lock::HEAD_BATTERY), list]
                .spacing(tile::GAP)
                .height(Length::Fill)
                .into()
        }
    }
}

/// The charge rail: a track with the lit part in text ink.
fn rail<'a>(percent: u8, h: f32) -> Element<'a, Message, Theme> {
    let p = u16::from(percent.min(100));
    let mut bar = row![];
    if p > 0 {
        bar = bar.push(swatch(
            color::text(),
            h / 2.0,
            Length::FillPortion(p),
            Length::Fill,
        ));
    }
    if p < 100 {
        bar = bar.push(swatch(
            color::track(),
            h / 2.0,
            Length::FillPortion(100 - p),
            Length::Fill,
        ));
    }
    container(bar).width(Length::Fill).height(Length::Fixed(h)).into()
}

fn wifi<'a>(cx: &Cx<'a>) -> Element<'a, Message, Theme> {
    let link = cx.s.face.glance().wifi;
    let plan = tile::wifi(Inner { w: cx.iw, h: cx.ih }, link.map(|l| l.percent), GLANCE_TYPE);
    let (Some(link), WifiPlan::Compact { figure, bars, gap } | WifiPlan::Full { figure, bars, gap }) =
        (link, plan)
    else {
        return quiet("No link");
    };
    let mut figure_row = row![label(
        format!("{}%", link.percent),
        figure,
        font::DATA_MEDIUM,
        color::text()
    )]
    .spacing(gap)
    .align_y(Alignment::End);
    if let Some(b) = bars {
        let mut bs = row![].spacing(b.gap).align_y(Alignment::End);
        for i in 0..tile::WIFI_BARS {
            let ink = if i < b.lit { color::text() } else { color::track() };
            bs = bs.push(swatch(
                ink,
                cell::BAR_RADIUS,
                Length::Fixed(b.w),
                Length::Fixed(tile::bar_h(figure, i)),
            ));
        }
        figure_row = figure_row.push(bs);
    }
    if matches!(plan, WifiPlan::Compact { .. }) {
        return container(figure_row)
            .width(Length::Fill)
            .center_y(Length::Fill)
            .into();
    }
    column![
        micro(cells::lock::HEAD_LINK),
        Space::new().height(Length::Fill),
        figure_row
    ]
    .spacing(tile::GAP)
    .height(Length::Fill)
    .into()
}

/// A Glance with nothing to show: one quiet line, centred.
fn quiet<'a>(line: &str) -> Element<'a, Message, Theme> {
    container(label(
        line.to_owned(),
        size::BODY_SMALL,
        font::UI,
        color::text_secondary(),
    ))
    .center(Length::Fill)
    .into()
}

/// The field: the typed length while it takes keys, the check while PAM has
/// the password, the exit while the lock goes.
fn field<'a>(s: &Scene<'a>, height: f32, tight: bool) -> Element<'a, Message, Theme> {
    let quiet = |label: &str| {
        text(label.to_owned())
            .font(font::UI)
            .size(size::BODY)
            .style(theme::text_secondary)
    };
    let (state, body, trailing): (FieldState, Element<'a, Message, Theme>, _) = match s.status {
        Status::Checking => (
            FieldState::Resting,
            row![
                spinner(lock::SPIN_PHASE, color::text_secondary()),
                quiet("Checking\u{2026}")
            ]
            .spacing(space::CONTROL_GAP)
            .align_y(Alignment::Center)
            .into(),
            None,
        ),
        Status::Unlocking => (FieldState::Resting, quiet("Unlocking\u{2026}").into(), None),
        Status::Failed { .. } | Status::Locking | Status::Ready => {
            let state = if matches!(s.status, Status::Failed { .. }) {
                FieldState::Refused
            } else {
                FieldState::Taking
            };
            let body: Element<'a, Message, Theme> = if s.typed == 0 {
                row![
                    dot_run(0, color::accent()),
                    text(cells::PLACEHOLDER)
                        .font(font::UI)
                        .size(size::BODY)
                        .style(theme::text_secondary),
                ]
                .spacing(space::CONTROL_GAP)
                .align_y(Alignment::Center)
                .into()
            } else {
                dot_run(s.typed, color::accent())
            };
            (state, body, Some(unlock(s.can_submit, tight)))
        }
    };
    let field = secret_field(state, MARKER, body, trailing).height(Length::Fixed(height));
    if !glass_live() {
        return field.into();
    }
    // A pane of the inset's tone, as `ec_ui`'s `secret_field_pane`: the pane is
    // the ground, and the client keeps only a rim that says something (focus,
    // refusal).
    let field = field.style(move |t: &Theme| {
        let mut s = theme::inset_field(state != FieldState::Resting)(t);
        if state == FieldState::Refused {
            s.border.color = color::danger().scale_alpha(lock::FAIL_RIM);
        }
        s.background = None;
        if state == FieldState::Resting {
            s.border.color = Color::TRANSPARENT;
        }
        s
    });
    glass_card(field, CellTone::Plain, radius::INSET)
}

/// Unlock: a ghost until there is something PAM may check, then a white lift.
fn unlock<'a>(ready: bool, tight: bool) -> Element<'a, Message, Theme> {
    let pad_y = if tight {
        cell::UNLOCK_PAD_TIGHT
    } else {
        space::PILL_Y
    };
    let label = text(cells::lock::UNLOCK)
        .font(font::UI_MEDIUM)
        .size(size::BODY_SMALL);
    if !glass_live() {
        return button(label)
            .padding([pad_y, space::PILL_X])
            .style(theme::send(ready))
            .on_press_maybe(ready.then_some(Message::Submit))
            .into();
    }
    // A pane of its own: the brighter tone once there is something to check,
    // lit by the pointer through the pane's hover and press bits. The button
    // keeps only its label.
    let send = theme::send(ready);
    let tone = if ready { CellTone::Focused } else { CellTone::Plain };
    glass_ground(
        button(label)
            .padding([pad_y, space::PILL_X])
            .style(move |t: &Theme, st| theme::bare_if_glass(send(t, st)))
            .on_press_maybe(ready.then_some(Message::Submit)),
        tone,
        radius::PILL,
    )
}

/// One line under the field: what is happening at the left, the machine's
/// reading of it (the wait, the count) in mono at the right.
fn note<'a>(s: &Scene<'a>) -> Element<'a, Message, Theme> {
    let (line, tone, reading): (&str, fn(&Theme) -> text::Style, Option<String>) = match s.status {
        Status::Locking | Status::Ready => (cells::lock::NOTE_READY, theme::text_secondary, None),
        Status::Checking => ("Checking your password", theme::text_secondary, None),
        Status::Unlocking => ("Welcome back", theme::text_secondary, None),
        Status::Failed {
            reason,
            failures,
            retry_in_secs,
        } => {
            let line = match reason {
                Reason::Wrong => "Wrong password",
                Reason::Unavailable => "Authentication is unavailable",
            };
            let reading = match (retry_in_secs, reason) {
                (Some(secs), _) => format!("retry in {secs} s"),
                (None, Reason::Unavailable) => "try again".to_owned(),
                (None, Reason::Wrong) if failures == 1 => "1 failed try".to_owned(),
                (None, Reason::Wrong) => format!("{failures} failed tries"),
            };
            (line, theme::text_primary, Some(reading))
        }
    };
    let mut r = row![container(
        text(line)
            .font(font::UI)
            .size(size::BODY_SMALL)
            .style(tone)
            .wrapping(text::Wrapping::None)
    )
    .width(Length::Fill)]
    .align_y(Alignment::Center);
    if let Some(reading) = reading {
        r = r.push(
            text(reading)
                .font(font::DATA)
                .size(size::MONO)
                .style(theme::text_secondary)
                .wrapping(text::Wrapping::None),
        );
    }
    container(r).height(Length::Fixed(cell::NOTE_H)).into()
}

/// The surface ground: the theme's base, **opaque**, unless the compositor is
/// drawing this surface's glass ([`glass_live`]), when there is none and the
/// wallpaper shows through the panes' glass.
pub fn style(_app: &App, theme: &Theme) -> iced::theme::Style {
    ground_for(theme, glass_live())
}

/// [`style`] without the model, for the preview (a plain window: no glass).
pub fn ground(theme: &Theme) -> iced::theme::Style {
    ground_for(theme, glass_live())
}

/// The ground for a frame. Opaque in the fall-back (a lock surface that let
/// the desktop through would show what it locks); transparent only when the
/// compositor draws the wallpaper and the panes' glass beneath.
pub fn ground_for(theme: &Theme, glass: bool) -> iced::theme::Style {
    let base = theme.palette().background;
    iced::theme::Style {
        background_color: if glass {
            Color::TRANSPARENT
        } else {
            Color::from_rgb(base.r, base.g, base.b)
        },
        text_color: theme.palette().text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ec_theme::{contrast, derive, Palette, Rgba, DARK_SEEDS, LIGHT_SEEDS};

    #[test]
    fn the_fallback_ground_is_opaque() {
        let t = theme::theme();
        assert_eq!(ground_for(&t, false).background_color.a, 1.0);
        // Even a theme whose base carries alpha cannot make the fallback see-through.
        let clear = Theme::custom(
            "clear".to_owned(),
            iced::theme::Palette {
                background: Color::from_rgba(0.1, 0.1, 0.1, 0.2),
                ..t.palette()
            },
        );
        assert_eq!(ground_for(&clear, false).background_color.a, 1.0);
    }

    #[test]
    fn the_glass_path_draws_no_ground() {
        let t = theme::theme();
        assert_eq!(ground_for(&t, true).background_color.a, 0.0);
        // The text colour is still the theme's: the glass changes the ground only.
        assert_eq!(ground_for(&t, true).text_color, ground_for(&t, false).text_color);
        // Nothing in this process attached glass: the live styles are the fallback.
        assert!(!glass_live());
        assert_eq!(ground(&t).background_color.a, 1.0);
    }

    /// The lock draws only the primary and secondary inks: the tertiary step is
    /// 2.7:1 on the light scheme and the light `danger` 3.8:1, both under 4.5
    /// (found by `fallback_text_holds_4_5_to_1`), so the drawing code asks for
    /// neither. A refusal is its words in primary ink and the field's danger rim.
    #[test]
    fn the_lock_draws_no_tertiary_ink() {
        let src = include_str!("view.rs");
        let drawing = &src[..src.find("#[cfg(test)]").unwrap()];
        for banned in ["text_tertiary", "micro_label", "key_hints", "text_danger"] {
            assert!(!drawing.contains(banned), "{banned} in view.rs");
        }
    }

    fn palettes() -> [(&'static str, Palette); 2] {
        [("dark", derive(&DARK_SEEDS)), ("light", derive(&LIGHT_SEEDS))]
    }

    /// In the fall-back every ink the screen uses holds 4.5:1 on the surfaces
    /// it can sit on: the opaque ground and the pane's own ground over it.
    #[test]
    fn fallback_text_holds_4_5_to_1() {
        for (name, p) in palettes() {
            let pane = p.surface_2.over_opaque(p.base);
            for ground in [p.base, pane] {
                for (ink, what) in [(p.text, "text"), (p.text_secondary, "secondary")] {
                    let c = contrast(ink, ground);
                    assert!(c >= 4.5, "{name} {what} {c:.2} < 4.5");
                }
            }
        }
    }

    /// The compositor's pane ground over a grey wallpaper of sRGB level `b`,
    /// mirroring `effects.rs` (vibrancy, smoke, the focus lift or ink shade)
    /// and, where `adapt` is set, its adaptive tint at the strongest it goes.
    fn glass_ground(scheme: ec_theme::Scheme, b: f32, adapt: bool) -> Rgba {
        let mix = |a: f32, c: f32, t: f32| a + (c - a) * t;
        let light = scheme == ec_theme::Scheme::Light;
        let mut c = if light { b } else { b / (1.0 + b) };
        let (smoke, mixv) = if light { (0.975, 0.22) } else { (0.047, 0.28) };
        c = mix(c, smoke, mixv);
        if adapt {
            // `ADAPT_MAX_L` .8 on light, `ADAPT_MAX` .95 on dark, toward the surface.
            c = if light {
                mix(c, 0.975, 0.8)
            } else {
                mix(c, 0.047, 0.95)
            };
        }
        // The strongest pane state: dark glass lifts toward white .028 (focused,
        // pressed), light glass shades toward ink .15 (pressed).
        c = if light {
            mix(c, 0.102, 0.15)
        } else {
            mix(c, 1.0, 0.03)
        };
        Rgba::new(c, c, c, 1.0)
    }

    /// Over the worst plausible glass the ink still holds 3:1: light ink on
    /// dark glass, dark ink on light glass (STYLE.md Light: never the dark
    /// tokens flipped). Dark glass is held over every wallpaper, white included
    /// (white is the worst: the highlight knee and the smoke only go so far).
    /// Light glass is held over any wallpaper from a mid-grey (.5) up on its own,
    /// and over anything darker only with the compositor's adaptive tint, which
    /// exists for exactly that (ADAPT_MAX_L).
    #[test]
    fn glass_text_holds_3_to_1() {
        for (name, p) in palettes() {
            let light = p.scheme == ec_theme::Scheme::Light;
            let grounds: Vec<Rgba> = if light {
                [0.5, 0.65, 0.8, 1.0]
                    .iter()
                    .map(|b| glass_ground(p.scheme, *b, false))
                    .chain([glass_ground(p.scheme, 0.0, true)])
                    .collect()
            } else {
                [0.0, 0.25, 0.5, 0.75, 1.0]
                    .iter()
                    .map(|b| glass_ground(p.scheme, *b, false))
                    .collect()
            };
            for g in grounds {
                for (ink, what) in [(p.text, "text"), (p.text_secondary, "secondary")] {
                    let c = contrast(ink, g);
                    assert!(c >= 3.0, "{name} {what} {c:.2} < 3 on glass {:?}", g.0);
                }
            }
            // The right ink for the scheme: dark ink on light glass, light on dark.
            let ink = p.text.luminance();
            assert_eq!(light, ink < 0.5, "{name} ink is the wrong way round");
        }
    }

    /// Building the tiles never panics, whatever the output size or layout,
    /// and works whether or not the frame clock ever ran.
    #[test]
    fn every_size_and_layout_builds() {
        let clock = Reading::default();
        let mut tight = face::builtin();
        tight.set(Kind::Password, Rect::new(0, 0, 6, 2));
        tight.set(Kind::Battery, Rect::new(10, 0, 3, 1));
        tight.set(Kind::Wifi, Rect::new(14, 0, 2, 1));
        let no_field = ec_layout::Layout { tiles: vec![] };
        for layout in [face::builtin(), tight, no_field, ec_layout::Layout::default()] {
            for glance in [
                face::Snapshot::default(),
                face::Snapshot {
                    batteries: vec![ec_glance::Battery {
                        internal: true,
                        name: "\u{202e}x".to_owned(),
                        percent: 250,
                        charge: Charge::Unknown,
                    }],
                    wifi: Some(ec_glance::Wifi { percent: 255 }),
                },
            ] {
                let face = Face::settled(layout.clone(), glance);
                let fresh = Face::new(layout.clone());
                for f in [&face, &fresh] {
                    for (w, h) in [
                        (0.0, 0.0),
                        (1.0, 1.0),
                        (40.0, 30.0),
                        (320.0, 200.0),
                        (1280.0, 800.0),
                        (7680.0, 4320.0),
                    ] {
                        let s = Scene {
                            status: Status::Ready,
                            typed: 500,
                            user: "",
                            clock: &clock,
                            can_submit: true,
                            face: f,
                        };
                        let _ = tiles(s, Size::new(w, h));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod shared_plan_tests {
    /// The lock holds no sizing of its own for its panes: the frame, the
    /// greeting and the password are `ec_glance::cells`, which the layout
    /// builder's lock preview draws too (Settings > Lock screen).
    #[test]
    fn the_lock_asks_the_shared_plans() {
        let src = include_str!("view.rs");
        let body = src.split("#[cfg(test)]").next().unwrap_or(src);
        for asked in [
            "cells::lock_frame(",
            "cells::lock_greeting(",
            "cells::lock_password(",
            "tile::clock(",
            "tile::battery(",
            "tile::wifi(",
            "cells::lock::NOTE_READY",
            "cells::lock::HINTS",
            "cells::lock::UNLOCK",
        ] {
            assert!(body.contains(asked), "{asked} is not asked");
        }
        for gone in ["\"Enter your password to unlock\"", "fit_in(", "pad_for("] {
            assert!(!body.contains(gone), "{gone} is back in the lock view");
        }
    }
}
