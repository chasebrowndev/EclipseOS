// SPDX-License-Identifier: AGPL-3.0-only
//! The screen: each placed tile as a pane of ground with its content.
//!
//! The hero is the clock: one large numeral, bottom-left, the only type that
//! scales to the screen. Beneath it the rest of the layout reads in order:
//! a one-line greeting, the account list, the field with its Unlock pill, the
//! power pills, and the Glances. No two neighbours share a silhouette: a
//! numeral, a line, a list, a field, a row of pills, a row of gauges.
//!
//! Accent: the one live yellow is the typed-length dot run and its caret, and
//! only while the field takes keys. Nothing else in the screen is accented.
//! Focus is a brighter fill and a crisp rim, never a glow.
//!
//! Panic-free: every size is fitted through [`fit_in`], which never returns
//! more than the space it was given, and every index is checked.

use std::time::Instant;

use ec_layout::Kind;
use ec_theme::{Palette, Rgba};
use iced::widget::image::FilterMethod;
use iced::widget::text::Wrapping;
use iced::widget::{column, container, image, pin, row, stack, text};
use iced::{alignment, ContentFit, Element, Font, Length};

use crate::app::{App, Msg};
use crate::auth::{AuthState, Power, Refusal};
use crate::gauge::{self, Spec};
use crate::glass::Tone;
use crate::motion::{content_scale, Placed};
use crate::parts::{swatch, Ground};
use crate::tokens::{
    self, cell, font, gauge as g, scale, size, space, to_iced, CARD_RADIUS, CELL_RADIUS, PAD_FRACTION,
    PAD_MAX, PAD_MIN, PAD_SHARE_MAX, PILL_RADIUS,
};

/// What one tile's content needs: its inner box, how far it has appeared, and
/// the backend's state.
struct Cx<'a> {
    app: &'a App,
    pal: &'a Palette,
    iw: f32,
    ih: f32,
    fade: f32,
    state: AuthState,
    /// How much a content-sized tile's reference sizes are scaled (1 unless the
    /// output is too small for them).
    scale: f32,
}

/// The whole screen for this frame.
pub fn view(app: &App) -> Element<'_, Msg> {
    let state = app.auth.state();
    let mut tiles: Vec<Element<'_, Msg>> = Vec::new();
    for p in app.stage.placed() {
        if let Some(tile) = tile(app, &p, state) {
            // `pin` lays its child out in the room left after its own offset, so
            // the pin must fill the window and the tile carries the box. A pin
            // sized to the tile would leave the tile negative room and zero size.
            let boxed = container(tile)
                .width(Length::Fixed(p.w))
                .height(Length::Fixed(p.h));
            tiles.push(
                pin(boxed)
                    .x(p.x)
                    .y(p.y + p.rise)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into(),
            );
        }
    }
    let animating = state.busy() || app.stage.animating(Instant::now());
    let tiles = stack(tiles).width(Length::Fill).height(Length::Fill);
    // Glass is drawn by the compositor under this window, so an image here
    // would hide it. Only the opaque fallback paints the wallpaper itself.
    let screen: Element<'_, Msg> = match (&app.backdrop, crate::glass::available()) {
        (Some(b), false) => stack![
            image(b.sharp.clone())
                .content_fit(ContentFit::Cover)
                .filter_method(FilterMethod::Linear)
                .width(Length::Fill)
                .height(Length::Fill),
            tiles
        ]
        .into(),
        _ => tiles.into(),
    };
    crate::glass::root_sole(screen, animating).into()
}

fn tile<'a>(app: &'a App, p: &Placed, state: AuthState) -> Option<Element<'a, Msg>> {
    if p.kind == Kind::Battery && (app.stage.shown() == 0 || p.w < 1.0 || p.h < 1.0) {
        return None;
    }
    let (pad, scale) = if p.kind == Kind::Battery {
        let s = content_scale(p.w, p.h, app.stage.shown());
        (g::PAD_PX as f32 * s, s)
    } else {
        (pad_for(p.w, p.h), 1.0)
    };
    let iw = (p.w - 2.0 * pad).max(0.0);
    let ih = (p.h - 2.0 * pad).max(0.0);
    let cx = Cx {
        app,
        pal: &app.pal,
        iw,
        ih,
        fade: p.fade,
        state,
        scale,
    };
    let content = match p.kind {
        Kind::Clock => clock(&cx),
        Kind::Greeting => greeting(&cx),
        Kind::Users => users(&cx),
        Kind::Password => password(&cx),
        Kind::Power => power(&cx),
        Kind::Battery => battery(&cx),
        Kind::Wifi => wifi(&cx),
        // Needs a session: the layout reader refuses it, so it is never drawn.
        Kind::NowPlaying => return None,
    };
    let body = container(content)
        .padding(pad)
        .width(Length::Fill)
        .height(Length::Fill)
        .clip(true);
    // A tile that is still appearing hides its content under the base colour,
    // so text and cells fade with the pane rather than popping in.
    let body: Element<'a, Msg> = if p.fade < 1.0 {
        stack(vec![
            body.into(),
            swatch(
                app.pal.base.with_alpha(1.0 - p.fade),
                0.0,
                Length::Fill,
                Length::Fill,
            ),
        ])
        .into()
    } else {
        body.into()
    };
    Some(
        Ground::new(body, &app.pal)
            .frost(app.backdrop.as_ref())
            .radius(CARD_RADIUS)
            .fade(p.fade)
            .shadow()
            .into(),
    )
}

/// The inner margin of a card: a share of its short side, clamped, and never
/// more than a fifth of that side, so a one-row tile keeps its content.
fn pad_for(w: f32, h: f32) -> f32 {
    let short = w.min(h).max(0.0);
    (short * PAD_FRACTION)
        .clamp(PAD_MIN, PAD_MAX)
        .min(short * PAD_SHARE_MAX)
}

/// `fit_in` of the smaller of a height share and a width share.
fn fit(h: f32, w: f32, h_share: f32, w_share: f32, lo: f32, hi: f32) -> f32 {
    fit_in((h * h_share).min(w * w_share), lo, hi)
}

/// Clamp `avail` into `[lo, hi]`, but never above `avail` itself: a size is a
/// hint, and a tile never gets a size it cannot hold.
fn fit_in(avail: f32, lo: f32, hi: f32) -> f32 {
    let avail = avail.max(0.0);
    avail.min(hi).max(lo.min(avail))
}

/// One line of text. It never wraps: a tile too small for its words clips them
/// rather than breaking a pill or a row into a second line.
fn label<'a>(s: impl Into<String>, px: f32, font: Font, colour: Rgba) -> Element<'a, Msg> {
    text(s.into())
        .size(px)
        .font(font)
        .color(to_iced(colour))
        .wrapping(Wrapping::None)
        .into()
}

fn ground<'a>(cx: &Cx<'a>, content: Element<'a, Msg>, radius: f32) -> Ground<'a, Msg> {
    Ground::new(content, cx.pal)
        .frost(cx.app.backdrop.as_ref())
        .radius(radius)
        .fade(cx.fade)
}

fn clock<'a>(cx: &Cx<'a>) -> Element<'a, Msg> {
    let app = cx.app;
    let numeral = fit(
        cx.ih,
        cx.iw,
        scale::CLOCK_H,
        scale::CLOCK_W,
        scale::CLOCK_MIN,
        scale::CLOCK_MAX,
    );
    let mut col = column![label(
        app.clock.time.clone(),
        numeral,
        font::UI_MEDIUM,
        cx.pal.text
    )]
    .spacing(space::ROW_Y);
    if !app.clock.date.is_empty() {
        let date = (numeral * scale::DATE_OF_CLOCK).clamp(scale::DATE_MIN, scale::DATE_MAX);
        col = col.push(label(
            app.clock.date.clone(),
            date,
            font::UI,
            cx.pal.text_secondary,
        ));
    }
    // A tall, narrow tile centres its numeral; a wide one sets it low-left.
    let portrait = cx.ih > cx.iw;
    let c = container(col).width(Length::Fill).height(Length::Fill);
    if portrait {
        c.center(Length::Fill).into()
    } else {
        c.align_y(alignment::Vertical::Bottom).into()
    }
}

fn greeting<'a>(cx: &Cx<'a>) -> Element<'a, Msg> {
    let px = fit(
        cx.ih,
        cx.iw,
        scale::GREETING_H,
        scale::GREETING_W,
        scale::GREETING_MIN,
        scale::GREETING_MAX,
    );
    container(label(cx.app.greeting.clone(), px, font::UI_MEDIUM, cx.pal.text))
        .width(Length::Fill)
        .height(Length::Fill)
        .align_y(alignment::Vertical::Center)
        .into()
}

fn users<'a>(cx: &Cx<'a>) -> Element<'a, Msg> {
    let app = cx.app;
    let pal = cx.pal;
    let head = cell::HEAD_H;
    let avail = (cx.ih - head - space::ROW_Y).max(0.0);
    let n = app.users.len();
    let mut col = column![label(
        "ACCOUNTS",
        size::MICRO,
        font::DATA_MEDIUM,
        pal.text_tertiary
    )]
    .spacing(space::ROW_Y)
    .height(Length::Fill);
    if n == 0 {
        return col
            .push(label(
                "No accounts",
                size::BODY_SMALL,
                font::UI,
                pal.text_secondary,
            ))
            .into();
    }
    // How many rows the tile can hold at the smallest comfortable height.
    let gap = cell::ROW_GAP;
    let fit_rows = (((avail + gap) / (cell::ROW_MIN + gap)).floor().max(1.0)) as usize;
    let shown = if n > fit_rows {
        fit_rows.saturating_sub(1).max(1)
    } else {
        n
    };
    let more = n.saturating_sub(shown);
    let total = shown + usize::from(more > 0);
    let total_f = total.max(1) as f32;
    let row_h = ((avail - gap * (total_f - 1.0)) / total_f).clamp(0.0, cell::ROW_MAX);

    for name in app.users.iter().take(shown) {
        let selected = app.selected.as_deref() == Some(name.as_str()) && cx.state != AuthState::Idle;
        let avatar = fit_in(row_h * cell::AVATAR_FRAC, cell::AVATAR_MIN, cell::AVATAR_MAX).min(row_h);
        let initial = name
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_default();
        let disc = container(label(initial, avatar * 0.45, font::UI_MEDIUM, pal.text))
            .width(Length::Fixed(avatar))
            .height(Length::Fixed(avatar))
            .center(Length::Fixed(avatar))
            .style(move |_| container::Style {
                background: Some(iced::Background::Color(to_iced(pal.lift))),
                border: iced::Border {
                    radius: (avatar / 2.0).into(),
                    ..iced::Border::default()
                },
                ..container::Style::default()
            });
        let line = row![disc, label(name.clone(), size::BODY, font::UI_MEDIUM, pal.text)]
            .spacing(space::ROW_Y)
            .align_y(alignment::Vertical::Center);
        let cell_body = container(line)
            .padding([space::ZERO, cell::ROW_PAD_X])
            .width(Length::Fill)
            .height(Length::Fixed(row_h))
            .align_y(alignment::Vertical::Center);
        let tone = if selected { Tone::Focused } else { Tone::Plain };
        col = col.push(
            ground(cx, cell_body.into(), CELL_RADIUS)
                .tone(tone)
                .on_press(Msg::Pick(name.clone())),
        );
    }
    if more > 0 {
        col = col.push(label(
            format!("+{more} more"),
            size::MICRO,
            font::DATA,
            pal.text_tertiary,
        ));
    }
    col.into()
}

fn password<'a>(cx: &Cx<'a>) -> Element<'a, Msg> {
    let app = cx.app;
    let pal = cx.pal;
    let field_h = fit_in(
        cx.ih - cell::NOTE_H - cell::NOTE_GAP,
        cell::FIELD_MIN,
        cell::FIELD_MAX,
    );
    let submit_w = fit_in(cx.iw * cell::SUBMIT_W_FRAC, cell::SUBMIT_MIN, cell::SUBMIT_MAX);
    let focused = cx.state.takes_keys();

    // Before an account is chosen the field and its pill are inert: dimmed
    // ground, dimmed ink, no press.
    let idle = cx.state == AuthState::Idle;
    let dim = if idle { tokens::backdrop::DISABLED } else { 1.0 };
    // A disabled control's ink steps back less on light glass, where a halved
    // ink would vanish into the frost.
    let ink_dim = if idle {
        tokens::disabled_ink(pal.scheme)
    } else {
        1.0
    };
    let mark = tokens::typed_mark(pal);
    let typed = app.secret.chars().min(cell::DOTS_MAX);
    let mut ink = row![].spacing(cell::DOT_GAP).align_y(alignment::Vertical::Center);
    for _ in 0..typed {
        ink = ink.push(swatch(
            mark,
            cell::DOT / 2.0,
            Length::Fixed(cell::DOT),
            Length::Fixed(cell::DOT),
        ));
    }
    if focused {
        ink = ink.push(swatch(
            mark,
            cell::CARET_W / 2.0,
            Length::Fixed(cell::CARET_W),
            Length::Fixed(cell::CARET_H),
        ));
    }
    if typed == 0 {
        ink = ink.push(label(
            "Password",
            size::BODY,
            font::UI,
            pal.text_tertiary.with_alpha(pal.text_tertiary.0[3] * ink_dim),
        ));
    }
    let field_body = container(ink)
        .padding([space::ZERO, cell::ROW_PAD_X])
        .width(Length::Fill)
        .height(Length::Fill)
        .align_y(alignment::Vertical::Center);
    let field = ground(cx, field_body.into(), CELL_RADIUS)
        .tone(if focused { Tone::Focused } else { Tone::Plain })
        .fade(cx.fade * dim);

    let unlock = container(label(
        "Unlock",
        size::BODY_SMALL,
        font::UI_MEDIUM,
        pal.text.with_alpha(pal.text.0[3] * ink_dim),
    ))
    .width(Length::Fill)
    .height(Length::Fill)
    .center(Length::Fill);
    let submit = ground(cx, unlock.into(), PILL_RADIUS).fade(cx.fade * dim);
    let submit = if idle {
        submit
    } else {
        submit.on_press(Msg::Submit)
    };

    let line = row![
        container(field)
            .width(Length::Fill)
            .height(Length::Fixed(field_h)),
        container(submit)
            .width(Length::Fixed(submit_w))
            .height(Length::Fixed(field_h)),
    ]
    .spacing(space::ROW_Y)
    .align_y(alignment::Vertical::Center);
    let (note, colour) = note(cx.state, pal);
    column![line, label(note, size::BODY_SMALL, font::UI, colour)]
        .spacing(cell::NOTE_GAP)
        .height(Length::Fill)
        .into()
}

fn note(state: AuthState, pal: &Palette) -> (&'static str, Rgba) {
    match state {
        AuthState::Idle => ("Choose an account to sign in", pal.text_tertiary),
        AuthState::Starting => ("Starting...", pal.text_secondary),
        AuthState::Prompting => ("Enter your password", pal.text_tertiary),
        AuthState::Checking => ("Checking...", pal.text_secondary),
        AuthState::Refused(Refusal::Wrong) => ("Wrong password. Try again.", pal.danger),
        AuthState::Refused(Refusal::Unavailable) => ("Sign-in is unavailable right now.", pal.danger),
        AuthState::Granted => ("Signed in", pal.text),
    }
}

fn power<'a>(cx: &Cx<'a>) -> Element<'a, Msg> {
    let pal = cx.pal;
    let pill_h = fit_in(cx.ih, 0.0, cell::FIELD_MAX);
    // Full labels need room for three pills; a narrower tile uses short ones.
    let full = cx.iw >= cell::POWER_FULL_W;
    let items: Vec<Element<'a, Msg>> = Power::ALL
        .iter()
        .map(|&action| {
            let text = if full { action.label() } else { action.short() };
            let body = container(label(text, size::BODY_SMALL, font::UI_MEDIUM, pal.text))
                .width(Length::Fill)
                .height(Length::Fill)
                .center(Length::Fill);
            ground(cx, body.into(), PILL_RADIUS)
                .on_press(Msg::Power(action))
                .into()
        })
        .collect();
    let pills: Vec<Element<'a, Msg>> = items
        .into_iter()
        .map(|p| {
            container(p)
                .width(Length::Fill)
                .height(Length::Fixed(pill_h))
                .into()
        })
        .collect();
    container(
        row(pills)
            .spacing(cell::ROW_GAP)
            .align_y(alignment::Vertical::Center),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .align_y(alignment::Vertical::Center)
    .into()
}

/// The battery Glance: a row of gauges, each a device icon in a rounded-square
/// ring with its percentage below, four to a row and then a second row. The
/// pane is exactly as big as that content (`ec_layout::intrinsic`).
fn battery<'a>(cx: &Cx<'a>) -> Element<'a, Msg> {
    let batteries = &cx.app.glance.batteries;
    let shown = cx.app.stage.shown().min(batteries.len());
    let mut rows = column![].spacing(g::ROW_GAP_PX as f32 * cx.scale);
    for chunk in batteries[..shown].chunks(g::MAX_PER_ROW) {
        let mut r = row![].spacing(g::GAUGE_GAP_PX as f32 * cx.scale);
        for b in chunk {
            r = r.push(gauge::cell(Spec::of(b), cx.scale, cx.pal));
        }
        rows = rows.push(r);
    }
    rows.into()
}

fn wifi<'a>(cx: &Cx<'a>) -> Element<'a, Msg> {
    let pal = cx.pal;
    let Some(link) = cx.app.glance.wifi else {
        return container(label("No link", size::BODY_SMALL, font::UI, pal.text_secondary))
            .width(Length::Fill)
            .height(Length::Fill)
            .center(Length::Fill)
            .into();
    };
    let fig = fit(
        cx.ih,
        cx.iw,
        scale::FIGURE_H,
        0.25,
        scale::FIGURE_MIN,
        scale::FIGURE_MAX,
    );
    let pct = usize::from(link.percent.min(100));
    let lit = pct.div_ceil(cell::WIFI_STEP_PCT);
    let mut bars = row![].spacing(cell::BAR_GAP).align_y(alignment::Vertical::Bottom);
    for i in 0..cell::WIFI_BARS {
        let h = fig * (cell::BAR_BASE + cell::BAR_STEP * i as f32);
        let colour = if i < lit { pal.text } else { pal.track };
        bars = bars.push(swatch(
            colour,
            cell::BAR_RADIUS,
            Length::Fixed(cell::BAR_W),
            Length::Fixed(h),
        ));
    }
    let figure = row![
        label(format!("{}%", link.percent), fig, font::DATA_MEDIUM, pal.text),
        bars,
    ]
    .spacing(space::BLOCK)
    .align_y(alignment::Vertical::Bottom);
    column![
        label("LINK", size::MICRO, font::DATA_MEDIUM, pal.text_tertiary),
        figure
    ]
    .spacing(space::ROW_Y)
    .height(Length::Fill)
    .into()
}
