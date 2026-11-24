// SPDX-License-Identifier: AGPL-3.0-only
//! The screen: each placed tile as a pane of ground with its content.
//!
//! The hero is the clock: one large numeral, bottom-left, the only type that
//! scales to the screen. Beneath it the rest of the layout reads in order:
//! a one-line greeting, the account list, the field with its Sign in pill, the
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

use ec_glance::cells;
use ec_glance::tile::{self, Inner, WifiPlan};
use ec_layout::Kind;
use ec_theme::{Palette, Rgba};
use iced::widget::image::FilterMethod;
use iced::widget::text::Wrapping;
use iced::widget::{column, container, image, pin, row, stack, text, Space};
use iced::{alignment, ContentFit, Element, Font, Length};

use crate::app::{App, Msg};
use crate::auth::{AuthState, Power, Refusal};
use crate::gauge::{self, Spec};
use crate::glass::Tone;
use crate::motion::{content_scale, Placed};
use crate::parts::{swatch, Ground};
use crate::tokens::{
    self, cell, font, gauge as g, size, space, to_iced, CARD_RADIUS, CELL_RADIUS, PILL_RADIUS,
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
    // A battery pane is content-sized (`ec_layout::intrinsic`), so it keeps its
    // own margin; every other tile takes the shared Glance one.
    let (pad, scale) = if p.kind == Kind::Battery {
        let s = content_scale(p.w, p.h, app.stage.shown());
        (g::PAD_PX as f32 * s, s)
    } else {
        (tile::pad_for(p.w, p.h), 1.0)
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
    // Sized by the shared rules against the preset's widest strings, so the
    // minute rolling over never moves it (and the builder draws the same).
    let fmt = app.format;
    let plan = tile::clock(Inner { w: cx.iw, h: cx.ih }, fmt.twelve, fmt.date);
    let mut col = column![label(
        app.clock.time.clone(),
        plan.numeral,
        font::UI_MEDIUM,
        cx.pal.text
    )]
    .spacing(tile::CLOCK_GAP);
    if let (Some(date), false) = (plan.date, app.clock.date.is_empty()) {
        col = col.push(label(
            app.clock.date.clone(),
            date,
            font::UI,
            cx.pal.text_secondary,
        ));
    }
    let c = container(col).width(Length::Fill).height(Length::Fill);
    match plan.align {
        tile::Align::Centre => c.center(Length::Fill).into(),
        tile::Align::BottomLeft => c.align_y(alignment::Vertical::Bottom).into(),
    }
}

fn greeting<'a>(cx: &Cx<'a>) -> Element<'a, Msg> {
    let px = cells::greeting(Inner { w: cx.iw, h: cx.ih });
    container(label(cx.app.greeting.clone(), px, font::UI_MEDIUM, cx.pal.text))
        .width(Length::Fill)
        .height(Length::Fill)
        .align_y(alignment::Vertical::Center)
        .into()
}

fn users<'a>(cx: &Cx<'a>) -> Element<'a, Msg> {
    let app = cx.app;
    let pal = cx.pal;
    let head = label(
        cells::ACCOUNTS_HEAD,
        size::MICRO,
        font::DATA_MEDIUM,
        pal.text_tertiary,
    );
    let plan = cells::users(Inner { w: cx.iw, h: cx.ih }, app.users.len());
    if app.users.is_empty() {
        return column![
            head,
            label(cells::NO_ACCOUNTS, size::BODY_SMALL, font::UI, pal.text_secondary)
        ]
        .spacing(cell::LINE_GAP)
        .height(Length::Fill)
        .into();
    }
    let mut list = column![].spacing(cell::ROW_GAP);
    for name in app.users.iter().take(plan.shown) {
        let selected = app.selected.as_deref() == Some(name.as_str()) && cx.state != AuthState::Idle;
        let initial = name
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_default();
        let disc = container(label(
            initial,
            plan.avatar * cell::INITIAL_FRAC,
            font::UI_MEDIUM,
            pal.text,
        ))
        .width(Length::Fixed(plan.avatar))
        .height(Length::Fixed(plan.avatar))
        .center(Length::Fixed(plan.avatar))
        .style(move |_| container::Style {
            background: Some(iced::Background::Color(to_iced(pal.lift))),
            border: iced::Border {
                radius: (plan.avatar / 2.0).into(),
                ..iced::Border::default()
            },
            ..container::Style::default()
        });
        let line = row![disc, label(name.clone(), size::BODY, font::UI_MEDIUM, pal.text)]
            .spacing(cell::LINE_GAP)
            .align_y(alignment::Vertical::Center);
        let cell_body = container(line)
            .padding([space::ZERO, cell::ROW_PAD_X])
            .width(Length::Fill)
            .height(Length::Fixed(plan.row_h))
            .align_y(alignment::Vertical::Center);
        let tone = if selected { Tone::Focused } else { Tone::Plain };
        list = list.push(
            ground(cx, cell_body.into(), CELL_RADIUS)
                .tone(tone)
                .on_press(Msg::Pick(name.clone())),
        );
    }
    if plan.more > 0 {
        list = list.push(label(
            cells::more_label(plan.more),
            size::MICRO,
            font::DATA,
            pal.text_tertiary,
        ));
    }
    column![head, list]
        .spacing(cell::LINE_GAP)
        .height(Length::Fill)
        .into()
}

fn password<'a>(cx: &Cx<'a>) -> Element<'a, Msg> {
    let app = cx.app;
    let pal = cx.pal;
    let plan = cells::password(Inner { w: cx.iw, h: cx.ih });
    let (field_h, submit_w) = (plan.field_h, plan.submit_w);
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
            cells::PLACEHOLDER,
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
        cells::SIGN_IN,
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
    .spacing(cell::LINE_GAP)
    .align_y(alignment::Vertical::Center);
    let (note, colour) = note(cx.state, pal);
    column![line, label(note, size::BODY_SMALL, font::UI, colour)]
        .spacing(cell::NOTE_GAP)
        .height(Length::Fill)
        .into()
}

fn note(state: AuthState, pal: &Palette) -> (&'static str, Rgba) {
    match state {
        AuthState::Idle => (cells::NOTE_IDLE, pal.text_tertiary),
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
    let plan = cells::power(Inner { w: cx.iw, h: cx.ih });
    let pill_h = plan.pill_h;
    // Full labels need room for three pills; a narrower tile uses short ones.
    let full = plan.full;
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

/// A Glance with nothing to show: one quiet line, centred.
fn quiet<'a>(cx: &Cx<'a>, line: &'static str) -> Element<'a, Msg> {
    container(label(line, size::BODY_SMALL, font::UI, cx.pal.text_secondary))
        .width(Length::Fill)
        .height(Length::Fill)
        .center(Length::Fill)
        .into()
}

fn heading<'a>(cx: &Cx<'a>, s: &'static str) -> Element<'a, Msg> {
    label(s, size::MICRO, font::DATA_MEDIUM, cx.pal.text_tertiary)
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
    let link = cx.app.glance.wifi;
    let plan = tile::wifi(
        Inner { w: cx.iw, h: cx.ih },
        link.map(|l| l.percent),
        tokens::GLANCE_TYPE,
    );
    let (Some(link), WifiPlan::Compact { figure, bars, gap } | WifiPlan::Full { figure, bars, gap }) =
        (link, plan)
    else {
        return quiet(cx, "No link");
    };
    let mut figure_row = row![label(
        format!("{}%", link.percent),
        figure,
        font::DATA_MEDIUM,
        pal.text
    )]
    .spacing(gap)
    .align_y(alignment::Vertical::Bottom);
    if let Some(b) = bars {
        let mut bs = row![].spacing(b.gap).align_y(alignment::Vertical::Bottom);
        for i in 0..tile::WIFI_BARS {
            let colour = if i < b.lit { pal.text } else { pal.track };
            bs = bs.push(swatch(
                colour,
                tokens::BAR_RADIUS,
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
    column![heading(cx, "LINK"), Space::new().height(Length::Fill), figure_row]
        .spacing(tile::GAP)
        .height(Length::Fill)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The screen holds no sizing of its own for these tiles: the plans are
    /// `ec_glance::cells`, which the layout builder's preview draws too.
    #[test]
    fn the_tiles_ask_the_shared_plans() {
        let src = include_str!("view.rs");
        let body = src.split("#[cfg(test)]").next().unwrap_or(src);
        for asked in [
            "cells::greeting(",
            "cells::users(",
            "cells::password(",
            "cells::power(",
            "tile::clock(",
            "gauge::cell(",
            "tile::wifi(",
        ] {
            assert!(body.contains(asked), "{asked} is not asked");
        }
    }

    #[test]
    fn the_power_words_are_the_shared_table() {
        for (p, (full, short)) in Power::ALL.iter().zip(cells::POWER) {
            assert_eq!((p.label(), p.short()), (full, short));
        }
    }
}
