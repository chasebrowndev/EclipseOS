// SPDX-License-Identifier: AGPL-3.0-only
//! The lock screen's tiles in the layout builder: what `ec-lock`'s view draws,
//! scaled, in the state the lock opens in (up, nothing typed, the field taking
//! keys).
//!
//! The same contract as the greeter's mocks (`layout_view::mock`): nothing here
//! decides how a tile looks. The tile's box is the real one, its frame and
//! inner box are `ec_glance::cells::lock_frame`, the greeting and the password
//! pane are `lock_greeting` and `lock_password`, and the clock and the Glances
//! are the shared `ec_glance::tile` plans (so they are the same drawing code
//! as the greeter's, in the lock's palette and headings). Colours are the
//! desktop palette the lock reads (`ec_ui::tokens::color::palette`), the words
//! are `cells::lock`, and the widgets the lock builds from `ec_ui`
//! (`secret_field`, `key_hints`) are redrawn from the same `ec_ui` tokens.
//!
//! The lock is drawn as it is on glass (ADR 0129), the shipped mode: every tile,
//! the clock included, is a pane, here the same stand-in the greeter mocks use
//! for the compositor's glass (a surface, a rim, a soft drop). The deviations are
//! the greeter mocks': text under the floor is drawn at the floor, and a tile too
//! small to draw in is a glyph.

use ec_glance::cells::{self, lock};
use ec_glance::tile::{self, Inner};
use ec_layout::Kind;
use ec_ui::tokens::{color, font, layout_edit as t, radius, size, space};
use iced::advanced::renderer;
use iced::advanced::text;
use iced::advanced::Renderer as _;
use iced::{border, Color, Point, Rectangle, Shadow, Size, Vector};

use super::layout_view::{
    battery_mock, clock_mock, fill, glyph_card, lerp, measure, real_size, wifi_mock, Pen, Skin,
};

/// One lock tile: its pane, then its content.
/// `b` is the animated rectangle, `full` the resting one the content is laid
/// out in, as in the greeter's `Editor::tile`.
#[allow(clippy::too_many_arguments)]
pub(super) fn tile(
    r: &mut iced::Renderer,
    kind: Kind,
    b: Rectangle,
    full: Rectangle,
    hot: f32,
    alpha: f32,
    _lifted: bool,
    pv_w: f32,
    sk: &Skin,
) {
    let k = pv_w / t::REAL_W;
    if k <= 0.0 {
        return;
    }
    let pal = color::palette();
    let ink = color::to_iced(pal.text);
    let pill = kind == Kind::Greeting;
    let ask = if pill {
        lock::PILL_RADIUS
    } else {
        lock::PANE_RADIUS
    };
    let radius_px = cells::radius(ask * k, b.width.min(b.height));
    {
        // A glass-styled pane on the opaque ground: surface, rim, a soft drop.
        r.fill_quad(
            renderer::Quad {
                bounds: b,
                border: border::rounded(radius_px),
                shadow: Shadow {
                    color: color::to_iced(pal.cell_shadow).scale_alpha(alpha),
                    offset: Vector::new(0.0, 16.0 * k),
                    blur_radius: 36.0 * k,
                },
                ..renderer::Quad::default()
            },
            color::to_iced(pal.surface_2).scale_alpha(alpha),
        );
        fill(
            r,
            b,
            radius_px,
            Color::TRANSPARENT,
            Some((
                color::to_iced(pal.border_strong).scale_alpha(alpha),
                space::HAIRLINE,
            )),
        );
        if hot > 0.01 {
            fill(
                r,
                b,
                radius_px,
                ink.scale_alpha(lerp(0.0, t::TILE_FILL_HOT, hot) * alpha),
                Some((
                    ink.scale_alpha(lerp(0.0, t::TILE_RIM_HOT, hot) * alpha),
                    space::HAIRLINE,
                )),
            );
        }
    }
    // The content, laid out in the resting rectangle, centred in the animated one.
    let c = b.center();
    let full = Rectangle::new(
        Point::new(c.x - full.width / 2.0, c.y - full.height / 2.0),
        full.size(),
    );
    let (rw, rh) = real_size(full.size(), k);
    let frame = cells::lock_frame(rw, rh, pill);
    let (ox, oy) = (full.x + frame.pad_x * k, full.y + frame.pad * k);
    let at = Rectangle::new(
        Point::new(ox, oy),
        Size::new(frame.inner.w * k, frame.inner.h * k),
    );
    if at.width < t::GLYPH_W || at.height < t::GLYPH_H {
        let g = Rectangle::new(at.position(), Size::new(at.width.max(1.0), at.height.max(1.0)));
        return glyph_card(r, kind, g, b, sk, alpha);
    }
    let mut pen = Pen {
        r,
        k,
        clip: b,
        a: alpha,
        pal: &pal,
        lock: true,
    };
    let inner = frame.inner;
    match kind {
        Kind::Clock => clock_mock(&mut pen, inner, at, sk),
        Kind::Battery => battery_mock(&mut pen, inner, at, sk),
        Kind::Wifi => wifi_mock(&mut pen, inner, at, sk),
        Kind::Greeting => greeting(&mut pen, inner, at),
        Kind::Password => password(&mut pen, inner, at),
        // The lock draws none of these; the editor never offers them.
        Kind::Users | Kind::Power | Kind::NowPlaying => {}
    }
}

/// The account as one line: its disc, its name, and the state word.
fn greeting(p: &mut Pen<'_>, inner: Inner, at: Rectangle) {
    let k = p.k;
    let pal = p.pal;
    let plan = cells::lock_greeting(inner);
    let user = lock::SAMPLE_USER;
    let d = plan.disc * k;
    let cy = at.y + at.height / 2.0;
    let disc = Rectangle::new(Point::new(at.x, cy - d / 2.0), Size::new(d, d));
    let lift = p.colour(pal.lift);
    fill(p.r, disc, d / 2.0, lift, None);
    let initial = user
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    p.text(
        &initial,
        disc.x,
        cy - tile::line(plan.initial_px) * k / 2.0,
        d,
        plan.initial_px,
        font::UI_MEDIUM,
        pal.text_secondary,
        text::Alignment::Center,
    );
    let name_x = at.x + (plan.disc + space::CONTROL_GAP) * k;
    p.text(
        user,
        name_x,
        cy - tile::line(plan.name_px) * k / 2.0,
        at.x + at.width - name_x,
        plan.name_px,
        font::UI_MEDIUM,
        pal.text,
        text::Alignment::Left,
    );
    if plan.word {
        let word = ec_ui::widget::tracked(lock::WORD_LOCKED);
        let w = measure(&word, p.fs(size::MICRO), font::DATA_MEDIUM);
        p.text(
            &word,
            at.x + at.width - w,
            cy - tile::line(size::MICRO) * k / 2.0,
            w + 2.0,
            size::MICRO,
            font::DATA_MEDIUM,
            pal.text_secondary,
            text::Alignment::Left,
        );
    }
}

/// The field, its note and, where there is room, the key hints, centred in the
/// tile: the lock's `password`, at rest.
fn password(p: &mut Pen<'_>, inner: Inner, at: Rectangle) {
    let k = p.k;
    let pal = p.pal;
    let plan = cells::lock_password(inner);
    let left = text::Alignment::Left;
    let hint_h = space::KEYCAP_H;
    let total =
        plan.field_h + lock::NOTE_GAP + lock::NOTE_H + if plan.hints { lock::NOTE_GAP + hint_h } else { 0.0 };
    let y0 = at.y + (at.height - total * k) / 2.0;

    // The field: the focused cell (a brighter fill and a crisp rim), a marker,
    // the caret, the placeholder and the Unlock ghost at the right.
    let fh = plan.field_h * k;
    let field = Rectangle::new(Point::new(at.x, y0), Size::new(at.width, fh));
    let ground = cells::radius(radius::INSET * k, field.width.min(field.height));
    let (f, rim) = (p.colour(pal.cell_focus), p.colour(pal.cell_focus_rim));
    // The cell's own soft drop (`theme::glass_cell`), then its fill and rim.
    let shadow = Shadow {
        color: p.colour(pal.cell_shadow),
        offset: Vector::new(0.0, ec_ui::tokens::bar::CELL_SHADOW_Y * k),
        blur_radius: ec_ui::tokens::bar::CELL_SHADOW_BLUR * k,
    };
    p.r.fill_quad(
        renderer::Quad {
            bounds: field,
            border: border::rounded(ground),
            shadow,
            ..renderer::Quad::default()
        },
        Color::TRANSPARENT,
    );
    fill(p.r, field, ground, f, Some((rim, space::HAIRLINE)));
    let cy = field.y + fh / 2.0;
    let line_y = |px: f32| cy - tile::line(px) * k / 2.0;
    let mx = field.x + space::CARD * k;
    let mw = measure(lock::MARKER, p.fs(size::PROMPT), font::DATA_MEDIUM);
    p.text(
        lock::MARKER,
        mx,
        line_y(size::PROMPT),
        mw + 2.0,
        size::PROMPT,
        font::DATA_MEDIUM,
        pal.text_tertiary,
        left,
    );
    // The trailing Unlock, a ghost until there is something to check.
    let pad_y = if plan.tight {
        lock::UNLOCK_PAD_TIGHT
    } else {
        space::PILL_Y
    };
    let uw = measure(cells::lock::UNLOCK, p.fs(size::BODY_SMALL), font::UI_MEDIUM) + 2.0 * space::PILL_X * k;
    let uh = (tile::line(size::BODY_SMALL) + 2.0 * pad_y) * k;
    let button = Rectangle::new(
        Point::new(field.x + field.width - space::CARD * k - uw, cy - uh / 2.0),
        Size::new(uw, uh),
    );
    let lift_soft = p.colour(pal.lift_soft);
    fill(
        p.r,
        button,
        cells::radius(radius::PILL * k, uw.min(uh)),
        lift_soft,
        None,
    );
    p.text(
        cells::lock::UNLOCK,
        button.x,
        line_y(size::BODY_SMALL),
        uw,
        size::BODY_SMALL,
        font::UI_MEDIUM,
        pal.text_tertiary,
        text::Alignment::Center,
    );
    // The body: the caret alone (nothing typed), then the placeholder.
    let bx = mx + mw + space::CARD * 0.75 * k;
    let caret = Rectangle::new(
        Point::new(bx, cy - ec_ui::tokens::lock::CARET_H * k / 2.0),
        Size::new(ec_ui::tokens::lock::CARET_W * k, ec_ui::tokens::lock::CARET_H * k),
    );
    let accent = p.colour(pal.accent);
    fill(p.r, caret, 0.0, accent, None);
    let px = bx + (ec_ui::tokens::lock::CARET_W + space::CONTROL_GAP) * k;
    p.text(
        cells::PLACEHOLDER,
        px,
        line_y(size::BODY),
        button.x - px,
        size::BODY,
        font::UI,
        pal.text_secondary,
        left,
    );

    // The note.
    let ny = y0 + (plan.field_h + lock::NOTE_GAP) * k;
    p.text(
        lock::NOTE_READY,
        at.x,
        ny + (lock::NOTE_H * k - tile::line(size::BODY_SMALL) * k) / 2.0,
        at.width,
        size::BODY_SMALL,
        font::UI,
        pal.text_secondary,
        left,
    );
    // The hints: each key in a small cap, then what it does.
    if plan.hints {
        let hy = ny + (lock::NOTE_H + lock::NOTE_GAP) * k;
        let mut x = at.x;
        for (key, verb) in lock::HINTS {
            let kw = measure(key, p.fs(size::MICRO), font::DATA_MEDIUM);
            let cap = Rectangle::new(
                Point::new(x, hy),
                Size::new(kw + 2.0 * space::BADGE_X * k, hint_h * k),
            );
            let (cf, cr) = (p.colour(pal.cell), p.colour(pal.cell_rim));
            fill(
                p.r,
                cap,
                cells::radius(radius::KEYCAP * k, cap.width.min(cap.height)),
                cf,
                Some((cr, space::HAIRLINE)),
            );
            let mid = hy + hint_h * k / 2.0;
            p.text(
                key,
                cap.x + space::BADGE_X * k,
                mid - tile::line(size::MICRO) * k / 2.0,
                kw + 2.0,
                size::MICRO,
                font::DATA_MEDIUM,
                pal.text_secondary,
                left,
            );
            let vx = cap.x + cap.width + space::KEY_GAP * k;
            let vw = measure(verb, p.fs(size::MICRO), font::DATA);
            p.text(
                verb,
                vx,
                mid - tile::line(size::MICRO) * k / 2.0,
                vw + 2.0,
                size::MICRO,
                font::DATA,
                pal.text_secondary,
                left,
            );
            x = vx + vw + space::HINT_GAP * k;
        }
    }
}
