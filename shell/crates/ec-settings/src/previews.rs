// SPDX-License-Identifier: AGPL-3.0-only
//! Live previews: small drawn pictures that move with the setting beside
//! them. A mini window set that follows the gaps, borders, rounding and
//! layout; a pane of glass that follows the rim, refraction and tint; and the
//! light and dark scheme cards.
//!
//! All of them are canvas programs drawn from the schema's current values at
//! `view` time: no timer, no state, nothing running while the window is idle.
//! They are diagrams, not renderings. A gap of 10px is not drawn 10px, it is
//! drawn so that a change from 5 to 20 is visible at a 300px card; the value
//! beside the picture is the true one. They never carry meaning alone.
//!
//! Colours are tokens and the two built-in palettes (`ec_theme::derive` of
//! the dark and light seeds): the scheme cards must show both schemes whatever
//! the window is in. Nothing here is gold.

use iced::widget::canvas::{self, gradient::Linear, Frame, Gradient, Path, Stroke};
use iced::widget::{button, column, text, Row};
use iced::{border, Color, Element, Length, Point, Rectangle, Size, Theme};

use ec_theme::{Palette, DARK_SEEDS, LIGHT_SEEDS};
use ec_ui::theme;
use ec_ui::tokens::{art, color, font, radius, size, space};

use crate::app::{App, Message};
use crate::schema::Control;

fn alpha(c: Color, a: f32) -> Color {
    Color { a, ..c }
}

/// A key's value as a share of its range, `fallback` when it is not there.
fn share(app: &App, path: &str, fallback: f32) -> f32 {
    let Some(key) = app.key(path) else {
        return fallback;
    };
    match key.control {
        Control::Slider { min, max, .. } if max > min => {
            (((key.as_f64() - min) / (max - min)) as f32).clamp(0.0, 1.0)
        }
        _ => fallback,
    }
}

fn value(app: &App, path: &str, fallback: f32) -> f32 {
    app.key(path).map_or(fallback, |k| k.as_f64() as f32)
}

fn rrect(at: Point, size: Size, r: f32) -> Path {
    let r = r.min(size.width / 2.0).min(size.height / 2.0).max(0.0);
    Path::rounded_rectangle(
        at,
        Size::new(size.width.max(0.0), size.height.max(0.0)),
        border::Radius::from(r),
    )
}

/// The backdrop every preview sits on: a soft diagonal of two group hues with
/// a few hard-edged shapes on it, so there is something for glass to bend.
fn backdrop(frame: &mut Frame, w: f32, h: f32, r: f32, line: bool) {
    let ground = Gradient::Linear(
        Linear::new(Point::new(0.0, 0.0), Point::new(w, h))
            .add_stop(0.0, alpha(art::HUE_LOOK, art::BACKDROP_A))
            .add_stop(1.0, alpha(art::HUE_DEVICES, art::BACKDROP_A)),
    );
    let body = rrect(Point::ORIGIN, Size::new(w, h), r);
    frame.fill(&body, ground);
    frame.with_clip(Rectangle::new(Point::ORIGIN, Size::new(w, h)), |f| {
        shapes(f, w, h, 1.0, Point::new(w / 2.0, h / 2.0), line);
    });
    frame.stroke(
        &rrect(Point::new(0.5, 0.5), Size::new(w - 1.0, h - 1.0), r),
        Stroke::default().with_color(color::border()).with_width(1.0),
    );
}

/// The shapes on the backdrop, magnified `by` about `about`: what a lens
/// shows is these, a little larger.
fn shapes(frame: &mut Frame, w: f32, h: f32, by: f32, about: Point, line: bool) {
    let at = |x: f32, y: f32| Point::new(about.x + (x * w - about.x) * by, about.y + (y * h - about.y) * by);
    frame.fill(
        &Path::circle(at(0.2, 0.3), h * 0.2 * by),
        alpha(art::HUE_APPS, art::SHAPE_A),
    );
    frame.fill(
        &rrect(
            at(0.62, 0.12),
            Size::new(w * 0.28 * by, h * 0.3 * by),
            h * 0.06 * by,
        ),
        alpha(art::HUE_PRIVACY, art::SHAPE_A),
    );
    frame.fill(
        &Path::circle(at(0.82, 0.78), h * 0.16 * by),
        alpha(art::HUE_LOOK, art::SHAPE_A),
    );
    if !line {
        return;
    }
    frame.stroke(
        &Path::line(at(0.0, 0.86), at(1.0, 0.7)),
        Stroke::default()
            .with_color(alpha(color::text(), art::SHAPE_LINE))
            .with_width(art::SHAPE_LINE_W * by),
    );
}

struct GlassCard {
    rim: f32,
    refraction: f32,
    dispersion: f32,
    bevel: f32,
    tint: f32,
}

impl<M> canvas::Program<M> for GlassCard {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let (w, h) = (bounds.width, bounds.height);
        if w < 8.0 || h < 8.0 {
            return vec![frame.into_geometry()];
        }
        backdrop(&mut frame, w, h, art::PREVIEW_R, true);
        // The pane: centred, a third of the width, most of the height.
        let (cw, ch) = (w * art::GLASS_W, h * art::GLASS_H);
        let at = Point::new((w - cw) / 2.0, (h - ch) / 2.0);
        let cr = art::PREVIEW_R * 0.8;
        let body = rrect(at, Size::new(cw, ch), cr);
        let centre = Point::new(w / 2.0, h / 2.0);
        // Inside the pane the backdrop is bent: the same shapes, larger.
        frame.with_clip(Rectangle::new(at, Size::new(cw, ch)), |f| {
            shapes(f, w, h, 1.0 + self.refraction * art::REFRACT_MAX, centre, true);
        });
        frame.fill(
            &body,
            alpha(
                color::glass_strong(),
                art::GLASS_FILL_MIN + self.tint * art::GLASS_FILL_SPAN,
            ),
        );
        let fringe = self.dispersion * art::FRINGE_MAX;
        if fringe > 0.2 {
            for (dx, hue) in [(-fringe, art::HUE_APPS), (fringe, art::HUE_DEVICES)] {
                frame.stroke(
                    &rrect(Point::new(at.x + dx, at.y), Size::new(cw, ch), cr),
                    Stroke::default()
                        .with_color(alpha(hue, art::FRINGE_A))
                        .with_width(1.0),
                );
            }
        }
        if self.bevel > 0.02 {
            let inset = 2.0 + self.bevel * art::BEVEL_MAX;
            frame.stroke(
                &rrect(
                    Point::new(at.x + inset, at.y + inset),
                    Size::new(cw - 2.0 * inset, ch - 2.0 * inset),
                    (cr - inset).max(1.0),
                ),
                Stroke::default()
                    .with_color(alpha(color::highlight_rim(), art::BEVEL_A))
                    .with_width(1.0),
            );
        }
        frame.stroke(
            &body,
            Stroke::default()
                .with_color(color::border_strong())
                .with_width(1.0 + self.rim * art::RIM_MAX),
        );
        vec![frame.into_geometry()]
    }
}

/// The Blur & glass hero: a pane of glass over a backdrop, following the
/// rim, refraction, dispersion, bevel and surface tint.
pub fn glass(app: &App) -> Element<'_, Message, Theme> {
    let g = |leaf: &str, d: f32| share(app, &format!("decoration.blur.glass.{leaf}"), d);
    caption_card(
        canvas::Canvas::new(GlassCard {
            rim: g("rim", 0.5),
            refraction: g("refraction", 0.5),
            dispersion: g("dispersion", 0.3),
            bevel: g("bevel", 0.5),
            tint: share(app, "decoration.surface.tint", 0.7),
        }),
        "Preview \u{b7} follows the sliders below",
        app,
    )
}

/// How a layout fills the stage, as unit rectangles.
fn tiles(layout: &str) -> &'static [(f32, f32, f32, f32)] {
    match layout {
        "dwindle" => &[
            (0.0, 0.0, 0.5, 1.0),
            (0.5, 0.0, 0.5, 0.5),
            (0.5, 0.5, 0.25, 0.5),
            (0.75, 0.5, 0.25, 0.5),
        ],
        "master" => &[
            (0.0, 0.0, 0.55, 1.0),
            (0.55, 0.0, 0.45, 0.34),
            (0.55, 0.34, 0.45, 0.33),
            (0.55, 0.67, 0.45, 0.33),
        ],
        _ => &[
            (0.0, 0.0, 0.58, 1.0),
            (0.58, 0.0, 0.42, 0.62),
            (0.58, 0.62, 0.42, 0.38),
        ],
    }
}

struct Windows {
    layout: String,
    gap_in: f32,
    gap_out: f32,
    border: f32,
    round: f32,
}

impl<M> canvas::Program<M> for Windows {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let (w, h) = (bounds.width, bounds.height);
        if w < 8.0 || h < 8.0 {
            return vec![frame.into_geometry()];
        }
        backdrop(&mut frame, w, h, art::PREVIEW_R, false);
        let out = (self.gap_out * art::GAP_SCALE).min(h * 0.22) + art::STAGE_PAD;
        let gap = (self.gap_in * art::GAP_SCALE).min(h * 0.12);
        let stage = Rectangle::new(Point::new(out, out), Size::new(w - 2.0 * out, h - 2.0 * out));
        let rim_w = (self.border * art::BORDER_SCALE).clamp(1.0, art::BORDER_MAX);
        let round = (self.round * art::ROUND_SCALE).min(art::ROUND_MAX);
        for (i, (x, y, tw, th)) in tiles(&self.layout).iter().enumerate() {
            let r = Rectangle::new(
                Point::new(
                    stage.x + x * stage.width + gap / 2.0,
                    stage.y + y * stage.height + gap / 2.0,
                ),
                Size::new(
                    (tw * stage.width - gap).max(1.0),
                    (th * stage.height - gap).max(1.0),
                ),
            );
            let body = rrect(r.position(), r.size(), round);
            frame.fill(&body, color::glass_strong());
            frame.stroke(
                &body,
                Stroke::default()
                    .with_color(if i == 0 {
                        color::border_strong()
                    } else {
                        color::border()
                    })
                    .with_width(rim_w),
            );
            // A title line and a body line, so it reads as a window.
            if r.width > 36.0 && r.height > 28.0 {
                let mut line = |y: f32, f: f32, a: f32| {
                    frame.fill(
                        &rrect(
                            Point::new(r.x + 8.0, r.y + y),
                            Size::new((r.width - 16.0) * f, 3.0),
                            1.5,
                        ),
                        alpha(color::text(), a),
                    );
                };
                line(9.0, 0.4, art::TITLE_A);
                line(18.0, 0.7, art::BODY_A);
            }
        }
        vec![frame.into_geometry()]
    }
}

/// The Windows pages' hero: the layout, drawn with the gaps, border and
/// corner the settings say.
pub fn windows(app: &App) -> Element<'_, Message, Theme> {
    let layout = app
        .key("general.layout")
        .and_then(|k| k.value.as_str())
        .unwrap_or("radiant")
        .to_owned();
    caption_card(
        canvas::Canvas::new(Windows {
            layout,
            gap_in: value(app, "general.gaps-in", 5.0),
            gap_out: value(app, "general.gaps-out", 10.0),
            border: value(app, "general.border-size", 2.0),
            round: value(app, "decoration.rounding", 12.0),
        }),
        "Preview \u{b7} not to scale",
        app,
    )
}

/// A preview's canvas in a glass card with its one-line caption under it.
fn caption_card<'a, P: canvas::Program<Message> + 'a>(
    canvas: canvas::Canvas<P, Message, Theme, iced::Renderer>,
    caption: &str,
    app: &'a App,
) -> Element<'a, Message, Theme> {
    ec_ui::widget::glass_panel(
        app.glass_radius,
        app.hairline_cards,
        column![
            canvas.width(Length::Fill).height(Length::Fixed(art::PREVIEW_H)),
            text(caption.to_owned())
                .font(font::UI)
                .size(size::BODY_SMALL)
                .style(theme::text_tertiary),
        ]
        .spacing(space::ROW_Y),
    )
}

/// One built-in scheme, drawn as a miniature of a Settings window.
fn mock(frame: &mut Frame, p: &Palette, w: f32, h: f32) {
    let c = |x| color::to_iced(x);
    let r = art::MOCK_R.min(h / 3.0);
    let body = rrect(Point::ORIGIN, Size::new(w, h), r);
    frame.fill(&body, c(p.base));
    let side = w * art::MOCK_SIDE;
    frame.with_clip(Rectangle::new(Point::ORIGIN, Size::new(side, h)), |f| {
        f.fill(&body, c(p.surface_1));
    });
    let pad = h * 0.1;
    for i in 0..3 {
        let y = pad + i as f32 * h * 0.19;
        let at = Point::new(pad * 0.7, y);
        let cell = Size::new(side - pad * 1.4, h * 0.12);
        frame.fill(
            &rrect(at, cell, 3.0),
            if i == 0 {
                c(p.highlight_soft)
            } else {
                c(p.lift_soft)
            },
        );
        frame.fill(
            &rrect(
                Point::new(at.x + 5.0, y + h * 0.045),
                Size::new(cell.width * 0.55, h * 0.03),
                1.5,
            ),
            c(p.text_secondary),
        );
    }
    let x = side + pad;
    frame.fill(
        &rrect(Point::new(x, pad), Size::new(w * 0.26, h * 0.07), 2.0),
        c(p.text),
    );
    frame.fill(
        &rrect(Point::new(x, pad + h * 0.13), Size::new(w * 0.4, h * 0.04), 1.5),
        c(p.text_tertiary),
    );
    let card = Size::new(w - x - pad, h * 0.36);
    let card_at = Point::new(x, h * 0.5);
    let card_body = rrect(card_at, card, 5.0);
    frame.fill(&card_body, c(p.glass));
    frame.stroke(
        &card_body,
        Stroke::default().with_color(c(p.border)).with_width(1.0),
    );
    let track = Size::new(card.width * 0.26, card.height * 0.28);
    let track_at = Point::new(
        card_at.x + card.width - track.width - 7.0,
        card_at.y + (card.height - track.height) / 2.0,
    );
    frame.fill(&rrect(track_at, track, track.height / 2.0), c(p.control_off));
    frame.fill(
        &Path::circle(
            Point::new(track_at.x + track.height / 2.0, track_at.y + track.height / 2.0),
            track.height * 0.36,
        ),
        c(p.knob_off),
    );
    frame.fill(
        &rrect(
            Point::new(card_at.x + 8.0, card_at.y + card.height * 0.38),
            Size::new(card.width * 0.4, h * 0.035),
            1.5,
        ),
        c(p.text_secondary),
    );
    frame.stroke(&body, Stroke::default().with_color(c(p.border)).with_width(1.0));
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scheme {
    Dark,
    Light,
    Auto,
}

struct SchemeMock {
    scheme: Scheme,
}

impl<M> canvas::Program<M> for SchemeMock {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let (w, h) = (bounds.width, bounds.height);
        if w >= 16.0 && h >= 16.0 {
            let dark = ec_theme::derive(&DARK_SEEDS);
            let light = ec_theme::derive(&LIGHT_SEEDS);
            match self.scheme {
                Scheme::Dark => mock(&mut frame, &dark, w, h),
                Scheme::Light => mock(&mut frame, &light, w, h),
                Scheme::Auto => {
                    mock(&mut frame, &dark, w, h);
                    frame.with_clip(
                        Rectangle::new(Point::new(w / 2.0, 0.0), Size::new(w / 2.0, h)),
                        |f| {
                            mock(f, &light, w, h);
                        },
                    );
                }
            }
        }
        vec![frame.into_geometry()]
    }
}

/// The appearance choice as three cards, each a miniature of the window in
/// that scheme. The chosen one is a brighter fill and a crisp rim; the choice
/// is the same `general.color-scheme` the row below sets.
pub fn scheme_cards(app: &App) -> Element<'_, Message, Theme> {
    let path = crate::schema::COLOR_SCHEME;
    let current = app
        .key(path)
        .and_then(|k| k.value.as_str())
        .unwrap_or("dark")
        .to_owned();
    let card = |n: usize, value: &'static str, label: &'static str, scheme: Scheme| {
        let on = current == value;
        let face = column![
            canvas::Canvas::new(SchemeMock { scheme })
                .width(Length::Fill)
                .height(Length::Fixed(art::SWATCH_H)),
            text(label)
                .font(if on { font::UI_MEDIUM } else { font::UI })
                .size(size::BODY_SMALL),
        ]
        .spacing(space::LINE_GAP * 2.0)
        .align_x(iced::Alignment::Center);
        let b = button(face)
            .padding(space::PILL_Y)
            .width(Length::Fill)
            .on_press(Message::Chose(path.to_owned(), value.to_owned()))
            .style(move |t: &Theme, status| {
                let mut s = theme::inset_cell(t, status);
                s.border.radius = radius::INSET.into();
                if on {
                    s.background = Some(iced::Background::Color(color::highlight_soft()));
                    s.border.color = color::highlight_rim();
                    s.text_color = color::text();
                }
                s
            });
        ec_ui::widget::focus_ring(
            b,
            app.focus == Some(crate::focus::Focus::Scheme(n)),
            radius::INSET,
            Length::FillPortion(1),
        )
    };
    Row::new()
        .push(card(0, "light", "Light", Scheme::Light))
        .push(card(1, "dark", "Dark", Scheme::Dark))
        .push(card(2, "auto", "Automatic", Scheme::Auto))
        .spacing(space::PILL_GAP * 2.0)
        .width(Length::Fill)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_layout_fills_the_stage_without_overlap() {
        for layout in ["radiant", "dwindle", "master", "something-new"] {
            let t = tiles(layout);
            let area: f32 = t.iter().map(|(_, _, w, h)| w * h).sum();
            assert!((area - 1.0).abs() < 1e-4, "{layout} covers {area}");
            for (i, a) in t.iter().enumerate() {
                for b in &t[i + 1..] {
                    let overlap = a.0 < b.0 + b.2 - 1e-4
                        && b.0 < a.0 + a.2 - 1e-4
                        && a.1 < b.1 + b.3 - 1e-4
                        && b.1 < a.1 + a.3 - 1e-4;
                    assert!(!overlap, "{layout}: {a:?} over {b:?}");
                }
            }
        }
    }

    #[test]
    fn a_rounded_rect_squeezed_to_nothing_is_still_a_path() {
        for (w, h, r) in [
            (0.0, 0.0, 9.0),
            (-3.0, 4.0, 9.0),
            (1.0, 1.0, 1e9),
            (f32::MAX, 1.0, 0.0),
        ] {
            let _ = rrect(Point::ORIGIN, Size::new(w, h), r);
        }
    }
}
