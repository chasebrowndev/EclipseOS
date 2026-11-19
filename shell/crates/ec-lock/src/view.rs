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
//! **Opaque.** Panes are glass-styled (a tint, a rim, a soft drop) on the
//! opaque ground. A lock surface that let the desktop through would show what
//! it locks, so there is no `eclipse_glass_v1` here.
//!
//! **Panic-free and never in the way.** Sizes go through [`fit_in`], which
//! never returns more than the room it was given; every index is checked; a
//! layout with no Password tile gets the built-in one added. The layout and
//! the Glance numbers are only *drawn*: nothing here reaches `update`.
//!
//! The contract (see `app.rs`): read only `App::{status, typed, user, clock,
//! can_submit, face}`; emit only `Message::Submit`; no `text_input`; never a
//! window-close task.

use iced::widget::{button, column, container, pin, responsive, row, stack, text, Space};
use iced::{alignment, Alignment, Color, Element, Font, Length, Shadow, Size, Theme, Vector};

use ec_glance::{clean, Charge};
use ec_layout::{Kind, Rect};
use ec_ui::theme;
use ec_ui::tokens::{color, font, lock, size, space};
use ec_ui::widget::{dot_run, key_hints, micro_label, secret_field, spinner, FieldState};

use crate::app::{App, Message, Status};
use crate::clock::Reading;
use crate::face::{self, Face};
use crate::machine::Reason;
use crate::tokens::{self, cell, scale};

/// The field's marker, as in the secret prompt.
const MARKER: &str = "\u{203a}";

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

pub fn view(app: &App, _window: iced::window::Id) -> Element<'_, Message, Theme> {
    scene(Scene::of(app))
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

/// The inner margin of a pane: a share of its short side, clamped, and never
/// more than a fifth of that side, so a one-row tile keeps its content.
fn pad_for(w: f32, h: f32) -> f32 {
    let short = w.min(h).max(0.0);
    (short * tokens::PAD_FRACTION)
        .clamp(tokens::PAD_MIN, tokens::PAD_MAX)
        .min(short * tokens::PAD_SHARE_MAX)
}

/// Clamp `avail` into `[lo, hi]`, but never above `avail` itself: a size is a
/// hint, and a tile never gets a size it cannot hold.
fn fit_in(avail: f32, lo: f32, hi: f32) -> f32 {
    let avail = avail.max(0.0);
    avail.min(hi).max(lo.min(avail))
}

/// `fit_in` of the smaller of a height share and a width share.
fn fit(h: f32, w: f32, h_share: f32, w_share: f32, lo: f32, hi: f32) -> f32 {
    fit_in((h * h_share).min(w * w_share), lo, hi)
}

fn tile<'a>(s: Scene<'a>, kind: Kind, w: f32, h: f32, appeared: f32) -> Option<Element<'a, Message, Theme>> {
    let pad = pad_for(w, h);
    // A pill's content keeps clear of its curve: the horizontal pad is most
    // of the radius, never less than the pane's own.
    let pill = if kind == Kind::Greeting {
        w.min(h) / 2.0
    } else {
        0.0
    };
    let pad_x = pad.max(pill * tokens::PILL_PAD_X).min(w / 4.0);
    let cx = Cx {
        s,
        iw: (w - 2.0 * pad_x).max(0.0),
        ih: (h - 2.0 * pad).max(0.0),
    };
    // The clock is bare type; every other tile is a pane.
    let (content, radius) = match kind {
        Kind::Clock => return Some(fading(clock(&cx), appeared)),
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
    let numeral = fit(
        cx.ih,
        cx.iw,
        scale::CLOCK_H,
        scale::CLOCK_W,
        scale::CLOCK_MIN,
        scale::CLOCK_MAX,
    );
    let mut col = column![label(r.time.clone(), numeral, font::UI_MEDIUM, color::text())]
        .spacing(lock::DATE_GAP)
        .align_x(Alignment::Center);
    if !r.date.is_empty() {
        let date = (numeral * scale::DATE_OF_CLOCK).clamp(scale::DATE_MIN, scale::DATE_MAX);
        col = col.push(label(r.date.clone(), date, font::UI, color::text_secondary()));
    }
    container(col).center(Length::Fill).clip(true).into()
}

/// The account as one line: its disc, its name, and whether it is locked.
fn greeting<'a>(cx: &Cx<'a>) -> Element<'a, Message, Theme> {
    let s = &cx.s;
    let disc = fit_in(cx.ih * cell::AVATAR_FRAC, cell::AVATAR_MIN, cell::AVATAR_MAX).min(cx.ih);
    let word = if s.status == Status::Unlocking {
        "unlocked"
    } else {
        "locked"
    };
    let mut r = row![
        initial_disc(s.user, disc),
        label(clean(s.user), size::PROMPT, font::UI_MEDIUM, color::text()),
        Space::new().width(Length::Fill),
    ]
    .spacing(space::CONTROL_GAP)
    .align_y(Alignment::Center);
    if cx.iw >= cell::GREETING_WORD_W {
        r = r.push(micro_label(word));
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
fn initial_disc<'a>(name: &str, side: f32) -> Element<'a, Message, Theme> {
    let initial: String = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default();
    container(label(
        initial,
        side * cell::INITIAL_FRAC,
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
    let note_room = cell::NOTE_H + cell::NOTE_GAP;
    let hint_room = cell::HINT_H + cell::NOTE_GAP;
    let hints = cx.ih >= cell::FIELD_MIN + note_room + hint_room;
    let avail = cx.ih - note_room - if hints { hint_room } else { 0.0 };
    let field_h = fit_in(avail, cell::FIELD_MIN, cell::FIELD_MAX);

    let mut col = column![field(s, field_h), note(s)]
        .spacing(cell::NOTE_GAP)
        .width(Length::Fill);
    if hints {
        // Nothing to press while PAM holds the password or the lock is going;
        // the line keeps its height so the pane does not move.
        let line: Element<'a, Message, Theme> = if s.taking() {
            key_hints(&[("enter", "unlock"), ("esc", "clear")])
        } else {
            Space::new().height(space::KEYCAP_H).into()
        };
        col = col.push(line);
    }
    container(col).center(Length::Fill).into()
}

fn battery<'a>(cx: &Cx<'a>) -> Element<'a, Message, Theme> {
    let snap = cx.s.face.glance();
    let Some(first) = snap.batteries.first() else {
        return quiet("No battery");
    };
    if cx.ih < cell::COMPACT_H {
        // One row: the figure and, if there is room, the rail beside it.
        let px = fit_in(cx.ih * cell::COMPACT_FIG, cell::COMPACT_MIN, scale::FIGURE_MAX);
        let mut r = row![label(
            format!("{}%", first.percent),
            px,
            font::DATA_MEDIUM,
            color::text()
        )]
        .spacing(space::CONTROL_GAP)
        .align_y(Alignment::Center);
        if cx.iw >= cell::COMPACT_RAIL_W {
            r = r.push(rail(first.percent));
        }
        return container(r).center_y(Length::Fill).into();
    }
    let detail = cx.ih >= cell::DETAIL_MIN_H;
    let mut col = column![micro_label("battery")]
        .spacing(space::ROW_Y)
        .height(Length::Fill);
    if snap.batteries.len() == 1 {
        let fig = fit(
            cx.ih,
            cx.iw,
            scale::FIGURE_H,
            scale::FIGURE_W,
            scale::FIGURE_MIN,
            scale::FIGURE_MAX,
        );
        col = col.push(label(
            format!("{}%", first.percent),
            fig,
            font::DATA_MEDIUM,
            color::text(),
        ));
        if detail {
            col = col.push(rail(first.percent));
        }
        col = col.push(label(
            charge_text(first.charge),
            size::MICRO,
            font::UI,
            color::text_secondary(),
        ));
    } else {
        for b in snap.batteries.iter().take(cell::DEVICES_MAX) {
            let name = if b.internal {
                "Internal".to_owned()
            } else {
                clean(&b.name)
            };
            col = col.push(
                row![
                    label(name, size::BODY_SMALL, font::UI, color::text_secondary()),
                    label(
                        format!("{}%", b.percent),
                        size::BODY,
                        font::DATA_MEDIUM,
                        color::text()
                    ),
                    label(
                        if b.charge == Charge::Charging {
                            "Charging"
                        } else {
                            ""
                        },
                        size::MICRO,
                        font::UI,
                        color::text_secondary(),
                    ),
                ]
                .align_y(Alignment::Center)
                .spacing(space::ROW_Y),
            );
            if detail {
                col = col.push(rail(b.percent));
            }
        }
    }
    col.into()
}

/// The charge rail: a track with the lit part in text ink.
fn rail<'a>(percent: u8) -> Element<'a, Message, Theme> {
    let p = u16::from(percent.min(100));
    let mut bar = row![];
    if p > 0 {
        bar = bar.push(swatch(
            color::text(),
            cell::RAIL_H / 2.0,
            Length::FillPortion(p),
            Length::Fill,
        ));
    }
    if p < 100 {
        bar = bar.push(swatch(
            color::track(),
            cell::RAIL_H / 2.0,
            Length::FillPortion(100 - p),
            Length::Fill,
        ));
    }
    container(bar)
        .width(Length::Fill)
        .height(Length::Fixed(cell::RAIL_H))
        .into()
}

fn charge_text(charge: Charge) -> &'static str {
    match charge {
        Charge::Charging => "Charging",
        Charge::Discharging => "On battery",
        Charge::Full => "Full",
        Charge::Unknown => "Unknown",
    }
}

fn wifi<'a>(cx: &Cx<'a>) -> Element<'a, Message, Theme> {
    let Some(link) = cx.s.face.glance().wifi else {
        return quiet("No link");
    };
    let compact = cx.ih < cell::COMPACT_H;
    let fig = if compact {
        fit_in(cx.ih * cell::COMPACT_FIG, cell::COMPACT_MIN, scale::FIGURE_MAX)
    } else {
        fit(
            cx.ih,
            cx.iw,
            scale::FIGURE_H,
            scale::FIGURE_W,
            scale::FIGURE_MIN,
            scale::FIGURE_MAX,
        )
    };
    let pct = usize::from(link.percent.min(100));
    let lit = pct.div_ceil(cell::WIFI_STEP_PCT);
    let mut bars = row![].spacing(cell::BAR_GAP).align_y(Alignment::End);
    for i in 0..cell::WIFI_BARS {
        let h = fig * (cell::BAR_BASE + cell::BAR_STEP * i as f32);
        let ink = if i < lit { color::text() } else { color::track() };
        bars = bars.push(swatch(
            ink,
            cell::BAR_RADIUS,
            Length::Fixed(cell::BAR_W),
            Length::Fixed(h),
        ));
    }
    let figure = row![
        label(
            format!("{}%", link.percent),
            fig,
            font::DATA_MEDIUM,
            color::text()
        ),
        bars,
    ]
    .spacing(space::BLOCK)
    .align_y(Alignment::End);
    if compact {
        return container(figure).center_y(Length::Fill).into();
    }
    column![micro_label("link"), figure]
        .spacing(space::ROW_Y)
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
fn field<'a>(s: &Scene<'a>, height: f32) -> Element<'a, Message, Theme> {
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
                    text("Password")
                        .font(font::UI)
                        .size(size::BODY)
                        .style(theme::text_tertiary),
                ]
                .spacing(space::CONTROL_GAP)
                .align_y(Alignment::Center)
                .into()
            } else {
                dot_run(s.typed, color::accent())
            };
            (
                state,
                body,
                Some(unlock(s.can_submit, height < cell::FIELD_ROOMY)),
            )
        }
    };
    secret_field(state, MARKER, body, trailing)
        .height(Length::Fixed(height))
        .into()
}

/// Unlock: a ghost until there is something PAM may check, then a white lift.
fn unlock<'a>(ready: bool, tight: bool) -> Element<'a, Message, Theme> {
    let pad_y = if tight {
        cell::UNLOCK_PAD_TIGHT
    } else {
        space::PILL_Y
    };
    button(text("Unlock").font(font::UI_MEDIUM).size(size::BODY_SMALL))
        .padding([pad_y, space::PILL_X])
        .style(theme::send(ready))
        .on_press_maybe(ready.then_some(Message::Submit))
        .into()
}

/// One line under the field: what is happening at the left, the machine's
/// reading of it (the wait, the count) in mono at the right.
fn note<'a>(s: &Scene<'a>) -> Element<'a, Message, Theme> {
    let (line, tone, reading): (&str, fn(&Theme) -> text::Style, Option<String>) = match s.status {
        Status::Locking | Status::Ready => ("Enter your password to unlock", theme::text_tertiary, None),
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
            (line, theme::text_danger, Some(reading))
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
                .style(theme::text_tertiary)
                .wrapping(text::Wrapping::None),
        );
    }
    container(r).height(Length::Fixed(cell::NOTE_H)).into()
}

/// The surface ground: opaque, the theme's base. A lock surface that let the
/// desktop through would show what it locks.
pub fn style(_app: &App, theme: &Theme) -> iced::theme::Style {
    ground(theme)
}

/// [`style`] without the model, for the preview.
pub fn ground(theme: &Theme) -> iced::theme::Style {
    iced::theme::Style {
        background_color: theme.palette().background,
        text_color: theme.palette().text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ground_is_opaque() {
        let t = theme::theme();
        assert_eq!(ground(&t).background_color.a, 1.0);
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
