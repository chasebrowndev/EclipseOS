// SPDX-License-Identifier: AGPL-3.0-only
//! The structural pieces of a pane.
//!
//! These are constructors over iced's stock containers, not new widgets: the
//! spec's structure rules (one level of inset, a 214px sidebar, a header row
//! with its subtitle) are layout, and layout is what `container` and `column`
//! already do. Keeping them here rather than in each app is what stops the
//! third settings pane from inventing its own padding.

use iced::widget::{button, column, container, row, rule, stack, svg, text, Column, Row, Space};
use iced::{Alignment, Color, Element, Font, Length, Theme};

use crate::theme;
use crate::tokens::{breakpoint, color, drawer, font, menu, radius, size, space};
use crate::widget::Fold;

/// A glass panel. The one container every block on a pane sits in.
///
/// `radius` is the caller's live-synced glass radius (`decoration.rounding`),
/// or [`radius::CARD`] before the first fetch answers — see
/// `ec_ui::ipc::fetch_config_radius`.
pub fn panel<'a, Message: 'a>(
    radius: f32,
    content: impl Into<Element<'a, Message, Theme>>,
) -> container::Container<'a, Message, Theme> {
    container(content)
        .padding(space::CARD)
        .style(theme::panel(radius))
        .width(Length::Fill)
}

/// One level of inset inside a panel — and the only level. There is no
/// `inset(inset(..))` guard in code, so this is the honour system the style
/// spec asks for.
pub fn inset<'a, Message: 'a>(
    content: impl Into<Element<'a, Message, Theme>>,
) -> container::Container<'a, Message, Theme> {
    container(content).style(theme::inset).width(Length::Fill)
}

/// A small uppercase mono section label.
pub fn micro_label<'a, Message: 'a>(label: &str) -> Element<'a, Message, Theme> {
    // Tracking is not a `text` property in iced, so the spacing the spec asks
    // for is put into the string itself. Ugly, honest, and it renders right.
    let spaced: String = label
        .to_uppercase()
        .chars()
        .flat_map(|c| [c, '\u{2009}'])
        .collect();
    text(spaced)
        .font(font::DATA_MEDIUM)
        .size(size::MICRO)
        .style(theme::text_tertiary)
        .into()
}

/// A pane title with its one-line subtitle, and up to three controls at the
/// right. The subtitle is where a pane is allowed to say one thing in yellow.
///
/// When the title block and the controls do not both fit on one line, the
/// controls drop beneath the subtitle ([`fold`]) rather than squeezing it into
/// a wrapped column beside them or running off the edge of the window.
pub fn header<'a, Message: 'a>(
    title: &'a str,
    subtitle: impl Into<Element<'a, Message, Theme>>,
    controls: Vec<Element<'a, Message, Theme>>,
) -> Element<'a, Message, Theme> {
    let left = column![
        text(title)
            .font(font::UI_SEMIBOLD)
            .size(size::PANE_TITLE)
            .style(theme::text_primary),
        subtitle.into(),
    ]
    .spacing(space::TITLE_GAP);

    let right = Row::with_children(controls)
        .spacing(space::HEADER_GAP)
        .align_y(Alignment::Center)
        .wrap()
        .vertical_spacing(space::HEADER_GAP);

    Fold::new(left, right, space::FOLD_X, space::ROW_Y).into()
}

/// A lead and a trail on one line when both fit at their natural width, and
/// the trail beneath the lead when they do not.
///
/// A widget and not a `row` because a row cannot change axis: given too
/// little room it squeezes, which wraps a label into two lines and pushes a
/// fixed-width control past the edge of its card. Nor is it iced's
/// `responsive`, which rebuilds its content per layout and so cannot take an
/// element the caller has already built — and every settings row is exactly
/// that. The decision is made on the widget's *own* width, so a row folds
/// because its card is narrow, whatever the window is doing.
pub fn fold<'a, Message: 'a>(
    lead: impl Into<Element<'a, Message, Theme>>,
    trail: impl Into<Element<'a, Message, Theme>>,
) -> Element<'a, Message, Theme> {
    Fold::new(lead, trail, space::FOLD_X, space::FOLD_Y).into()
}

/// Two equal columns of settings rows, stacked once a half could no longer
/// hold a numeric row folded under its label ([`space::CONTROL_COL_W`]).
///
/// A widget for the same reason as [`fold`]: a `row` of two fill columns
/// halves them however narrow the card gets, and past that point every
/// slider's typed entry is cut off at the column edge.
pub fn halves<'a, Message: 'a>(
    left: impl Into<Element<'a, Message, Theme>>,
    right: impl Into<Element<'a, Message, Theme>>,
) -> Element<'a, Message, Theme> {
    Fold::halves(left, right, space::BLOCK, space::BLOCK, space::CONTROL_COL_W).into()
}

/// How much room the window gives the frame around a pane.
///
/// Only the frame reads this — sidebar width, content padding. Rows do not:
/// they [`fold`] on their own width, which is the one that matters to them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Density {
    /// The spec's frame: 214px sidebar, 26/30 content padding.
    Regular,
    /// A narrow window: a slimmer sidebar with no icon squares, tighter
    /// padding.
    Compact,
}

impl Density {
    /// The density for a window `width` logical pixels wide.
    pub fn for_width(width: f32) -> Self {
        if width < breakpoint::COMPACT {
            Density::Compact
        } else {
            Density::Regular
        }
    }
}

/// A subtitle line whose middle clause carries the accent.
pub fn subtitle<'a, Message: 'a>(before: &str, accented: &str, after: &str) -> Element<'a, Message, Theme> {
    row![
        text(before.to_string())
            .font(font::UI)
            .size(size::BODY)
            .style(theme::text_secondary),
        text(accented.to_string())
            .font(font::UI)
            .size(size::BODY)
            .style(theme::text_accent),
        text(after.to_string())
            .font(font::UI)
            .size(size::BODY)
            .style(theme::text_secondary),
    ]
    .into()
}

/// A hairline. One pixel, drawn between list rows and nowhere else.
pub fn hairline<'a, Message: 'a>() -> Element<'a, Message, Theme> {
    rule::horizontal(1).style(theme::hairline).into()
}

/// A flat quad of one colour, sized on both axes.
///
/// A widget and not a styled container because a one-sided edge is not
/// expressible as a container border in iced, so every marked edge in the
/// desktop — the sidebar's 3px nav bar, a choice row's left bar, a task
/// button's marker — has to be a sibling quad. Three inlined copies of
/// that trick is how they drift apart; this is the one copy.
pub fn edge_quad<'a, Message: 'a>(width: Length, height: Length, fill: Color) -> Element<'a, Message, Theme> {
    quad(width, height, fill, 0.0)
}

/// [`edge_quad`] with a corner radius — a chip ground, a disc, a ring.
///
/// The same widget and not a parameter default because a rounded quad is the
/// common case on glass: the style spec puts a radius on everything but the
/// screen's own edges, and a caller reaching for a square one should have to
/// say so.
pub fn quad<'a, Message: 'a>(
    width: Length,
    height: Length,
    fill: Color,
    radius: f32,
) -> Element<'a, Message, Theme> {
    container(Space::new())
        .width(width)
        .height(height)
        .style(move |_t: &Theme| container::Style {
            background: Some(iced::Background::Color(fill)),
            border: iced::border::rounded(radius),
            ..container::Style::default()
        })
        .into()
}

/// A ring: a circle drawn as a border with nothing inside it.
///
/// A widget because the desktop has exactly one — the launcher mark — and the
/// obvious alternative (a lit disc with an occluding quad of the surface
/// colour moved across it) is a lie on a translucent surface, where the
/// occluder would cut a hole through to the wallpaper.
pub fn ring<'a, Message: 'a>(diameter: f32, thickness: f32, stroke: Color) -> Element<'a, Message, Theme> {
    container(Space::new())
        .width(Length::Fixed(diameter))
        .height(Length::Fixed(diameter))
        .style(move |_t: &Theme| container::Style {
            border: iced::Border {
                color: stroke,
                width: thickness,
                radius: radius::PILL.into(),
            },
            ..container::Style::default()
        })
        .into()
}

/// A [`ring`] that brightens under the pointer, filling whatever box it is
/// given and centred in it.
///
/// A widget, not a styled [`ring`], because the ring sits inside a button
/// and iced hands a button's hover status to the button's own style only —
/// its content never sees it. A canvas does see the cursor, so laid over the
/// button's whole box it knows the pointer is on the button and strokes
/// `hover` instead of `rest`. It captures nothing, so the press still lands
/// on the button.
pub fn hover_ring<'a, Message: 'a>(
    diameter: f32,
    thickness: f32,
    rest: Color,
    hover: Color,
) -> Element<'a, Message, Theme> {
    iced::widget::canvas(HoverRing {
        diameter,
        thickness,
        rest,
        hover,
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

struct HoverRing {
    diameter: f32,
    thickness: f32,
    rest: Color,
    hover: Color,
}

impl<Message> iced::widget::canvas::Program<Message> for HoverRing {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: iced::Rectangle,
        cursor: iced::mouse::Cursor,
    ) -> Vec<iced::widget::canvas::Geometry> {
        use iced::widget::canvas;
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        // The ring's band, between its outer edge and the hole: the same
        // circle `ring` draws as a container border.
        let outer = self.diameter / 2.0;
        let path = canvas::Path::new(|b| {
            b.circle(frame.center(), outer);
            b.circle(frame.center(), (outer - self.thickness).max(0.0));
        });
        let stroke = if cursor.is_over(bounds) {
            self.hover
        } else {
            self.rest
        };
        frame.fill(
            &path,
            canvas::Fill {
                style: canvas::Style::Solid(stroke),
                rule: canvas::fill::Rule::EvenOdd,
            },
        );
        vec![frame.into_geometry()]
    }
}

/// A hollow rounded rect: a frame with nothing inside it.
///
/// A widget for the same reason [`ring`] is one — on a translucent surface a
/// frame cannot be faked with a filled quad plus an occluder, because the
/// occluder would punch a hole through the glass to the wallpaper. It is the
/// rectangular sibling of [`ring`], and it is what every stand-in mark that
/// has to read as "a window" is drawn with.
pub fn outline<'a, Message: 'a>(
    width: f32,
    height: f32,
    thickness: f32,
    stroke: Color,
    radius: f32,
) -> Element<'a, Message, Theme> {
    container(Space::new())
        .width(Length::Fixed(width))
        .height(Length::Fixed(height))
        .style(move |_t: &Theme| container::Style {
            border: iced::Border {
                color: stroke,
                width: thickness,
                radius: radius.into(),
            },
            ..container::Style::default()
        })
        .into()
}

/// A disclosure chevron: the mark on a control that opens a drawer.
///
/// A widget and not an inlined shape because iced has no rotation and the
/// desktop ships no icon font, so the only way to draw one is a staircase of
/// quads — and a staircase open-coded in a `view` is four magic numbers that
/// will not match the next one.
///
/// It is an open **V**, apex at the bottom, and deliberately not a filled
/// triangle: a solid wedge at this size reads as a play button or a battery
/// mark, while two strokes meeting read as "there is more, below". The apex
/// points the way the drawer travels — down, out of a bar anchored to the top
/// of the screen.
///
/// `extent` is the mark's height; its width is twice that, the proportion a
/// chevron reads as rather than a caret.
pub fn chevron<'a, Message: 'a>(extent: f32, thickness: f32, stroke: Color) -> Element<'a, Message, Theme> {
    let steps = (extent / thickness).round().max(2.0) as usize;
    let t = thickness;
    let cell = |w: f32| edge_quad(Length::Fixed(w), Length::Fixed(t), stroke);
    let air = |w: f32| Element::from(Space::new().width(Length::Fixed(w)).height(Length::Fixed(t)));

    let mut col = Column::new();
    for step in 0..steps {
        let left = step as f32 * t;
        let line = if step + 1 == steps {
            // The apex: the two strokes have met and are one quad.
            Row::new().push(air(left)).push(cell(2.0 * t))
        } else {
            let middle = (2 * steps - 3 - 2 * step) as f32 * t;
            Row::new()
                .push(air(left))
                .push(cell(t))
                .push(air(middle))
                .push(cell(t))
        };
        col = col.push(line);
    }
    container(col)
        .width(Length::Fixed((2 * steps - 1) as f32 * t))
        .height(Length::Fixed(steps as f32 * t))
        .into()
}

/// A magnitude as a filled track: the mark for a row whose reading is a
/// proportion.
///
/// A widget because "percentage drawn as a bar" recurs the moment a second
/// drawer exists, and because the alternative in the tray drawer was to lead
/// a signal row with the battery gauge — a mark that means *battery*, on a
/// row that means nothing of the sort.
pub fn meter_bar<'a, Message: 'a>(width: f32, fraction: f32, fill: Color) -> Element<'a, Message, Theme> {
    let f = fraction.clamp(0.0, 1.0);
    let lit_w = (width * f).round();
    let row = Row::new()
        .push(quad(
            Length::Fixed(lit_w),
            Length::Fixed(drawer::METER_H),
            fill,
            drawer::RADIUS_METER,
        ))
        .push(quad(
            Length::Fixed(width - lit_w),
            Length::Fixed(drawer::METER_H),
            color::TRACK,
            drawer::RADIUS_METER,
        ));
    container(row)
        .width(Length::Fixed(width))
        .height(Length::Fixed(drawer::METER_H))
        .into()
}

/// The glass sheet a tray drawer is drawn on.
///
/// A widget because a drawer is a *reusable container* and not one applet's
/// popup: the bar's tray overflow is its first tenant and the clock's calendar
/// is meant to be its second, so the sheet, its padding, its radius and its
/// top-edge highlight are decided once here rather than re-derived per applet.
/// It is the same lit glass as a context menu with a different interior rhythm
/// — a menu is verbs on a mark rail, a drawer is readings on a label/value
/// rail — which is what keeps the two from reading as the same popup.
pub fn drawer_sheet<'a, Message: 'a>(
    radius: f32,
    heading: &str,
    rows: Vec<Element<'a, Message, Theme>>,
) -> Element<'a, Message, Theme> {
    drawer_frame(radius, drawer_sheet_head(heading), rows)
}

/// The plain heading strip of a [`drawer_sheet`], for a caller that assembles
/// the sheet through [`drawer_frame`] itself. [`drawer::HEAD_H`] tall.
pub fn drawer_sheet_head<'a, Message: 'a>(heading: &str) -> Element<'a, Message, Theme> {
    container(micro_label(heading))
        .height(Length::Fixed(drawer::HEAD_H))
        .align_y(Alignment::Center)
        .padding([0.0, drawer::ROW_X])
        .into()
}

/// [`drawer_sheet`] with a head the caller builds — a [`drawer_switch_head`]
/// rather than a bare label. The glass, padding and edge highlight are still
/// decided once, here.
pub fn drawer_frame<'a, Message: 'a>(
    radius: f32,
    head: Element<'a, Message, Theme>,
    rows: Vec<Element<'a, Message, Theme>>,
) -> Element<'a, Message, Theme> {
    let mut col = Column::new().push(head);
    for r in rows {
        col = col.push(r);
    }
    lit(
        container(col)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(drawer::PAD)
            .style(theme::menu_surface(radius)),
        radius,
        color::HIGHLIGHT,
    )
}

/// One row of a drawer: a mark, a label, and a mono reading at the right.
///
/// A widget rather than a [`list_row`] because a drawer row is a fixed
/// [`drawer::ROW_H`] tall — the popup's pixel height is fixed at creation, so
/// the row that draws it and the arithmetic that sized it have to agree on one
/// number, exactly the reason [`menu_separator`] exists.
pub fn drawer_row<'a, Message: 'a>(
    mark: Element<'a, Message, Theme>,
    label: &str,
    reading: &str,
    accented: bool,
) -> Element<'a, Message, Theme> {
    let tint = if accented {
        color::ACCENT_TEXT
    } else {
        color::TEXT_SECONDARY
    };
    container(
        row![
            container(mark)
                .width(Length::Fixed(drawer::MARK))
                .height(Length::Fixed(drawer::MARK))
                .align_x(Alignment::Center)
                .align_y(Alignment::Center),
            text(label.to_string())
                .font(font::UI_MEDIUM)
                .size(size::BODY_SMALL)
                .color(color::TEXT),
            Space::new().width(Length::Fill),
            text(reading.to_string())
                .font(font::DATA)
                .size(size::MONO)
                .color(tint),
        ]
        .spacing(drawer::MARK_GAP)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .height(Length::Fixed(drawer::ROW_H))
    .align_y(Alignment::Center)
    .padding([0.0, drawer::ROW_X])
    .into()
}

/// A radio drawer's heading: its label at the left and the radio's on/off
/// [`crate::widget::Toggle`] at the right.
///
/// A widget because the head is the drawer's one control and both radio
/// drawers (wi-fi, bluetooth) must put it in the same place at the same
/// height — [`drawer::HEAD_SWITCH_H`], which the popup's size arithmetic
/// also reads. The toggle's lit track is the drawer's one yellow.
pub fn drawer_switch_head<'a, Message: 'a>(
    heading: &str,
    toggle: impl Into<Element<'a, Message, Theme>>,
) -> Element<'a, Message, Theme> {
    container(
        row![
            micro_label(heading),
            Space::new().width(Length::Fill),
            toggle.into()
        ]
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .height(Length::Fixed(drawer::HEAD_SWITCH_H))
    .align_y(Alignment::Center)
    .padding([0.0, drawer::ROW_X])
    .into()
}

/// The hero row of a radio drawer: the one network or device the radio is
/// on, two lines tall, with the verb that ends it at the right.
///
/// A widget because it is the drawer's hero and must not share a silhouette
/// with the rows under it: taller ([`drawer::CURRENT_H`]), a larger mark, a
/// semibold name over a mono state line. Built from [`drawer_row`] it would
/// read as the first item of the list, which is the exact failure the
/// composition rules exist to stop.
pub fn drawer_current<'a, Message: Clone + 'a>(
    mark: Element<'a, Message, Theme>,
    name: &str,
    state: &str,
    state_tint: Color,
    action: Option<(&str, Message)>,
) -> Element<'a, Message, Theme> {
    let lines = column![
        text(name.to_string())
            .font(font::UI_SEMIBOLD)
            .size(size::BODY)
            .color(color::TEXT),
        text(state.to_string())
            .font(font::DATA)
            .size(size::MICRO)
            .color(state_tint),
    ]
    .spacing(drawer::LINE_GAP);
    // The larger mark eats into its own gap, not the name's position: the
    // name stays on the list's gridline (`ROW_X + MARK + MARK_GAP`) so the
    // choices below hang off it.
    let mut r = row![
        container(mark)
            .width(Length::Fixed(drawer::CURRENT_MARK))
            .height(Length::Fixed(drawer::CURRENT_MARK))
            .align_x(Alignment::Center)
            .align_y(Alignment::Center),
        Space::new().width(Length::Fixed(
            drawer::MARK + drawer::MARK_GAP - drawer::CURRENT_MARK
        )),
        lines,
        Space::new().width(Length::Fill),
    ]
    .align_y(Alignment::Center);
    if let Some((label, msg)) = action {
        r = r.push(
            button(
                text(label.to_string())
                    .font(font::UI_MEDIUM)
                    .size(size::BODY_SMALL)
                    .color(color::TEXT_SECONDARY),
            )
            .padding([space::ROW_Y / 2.0, drawer::ROW_X])
            .style(drawer_lift)
            .on_press(msg),
        );
    }
    container(r)
        .width(Length::Fill)
        .height(Length::Fixed(drawer::CURRENT_H))
        .align_y(Alignment::Center)
        .padding([0.0, drawer::ROW_X])
        .into()
}

/// One pickable row of a drawer: a mark, a name, an optional trailing mark,
/// and a ground that lifts under the pointer.
///
/// A widget rather than a [`drawer_row`] because this row is a *choice* — a
/// click on it does something — and a pickable row that does not answer the
/// pointer reads as a picture of one. It is the same fixed [`drawer::ROW_H`]
/// so the popup arithmetic still holds, and the name sits on the same
/// gridline as the hero row's so the list hangs off the hero.
pub fn drawer_choice<'a, Message: Clone + 'a>(
    mark: Element<'a, Message, Theme>,
    label: &str,
    trailing: Option<Element<'a, Message, Theme>>,
    on_press: Option<Message>,
) -> Element<'a, Message, Theme> {
    let mut r = row![
        container(mark)
            .width(Length::Fixed(drawer::MARK))
            .height(Length::Fixed(drawer::MARK))
            .align_x(Alignment::Center)
            .align_y(Alignment::Center),
        text(label.to_string())
            .font(font::UI)
            .size(size::BODY_SMALL)
            .color(color::TEXT),
        Space::new().width(Length::Fill),
    ]
    .spacing(drawer::MARK_GAP)
    .align_y(Alignment::Center);
    if let Some(t) = trailing {
        r = r.push(t);
    }
    let face = container(r)
        .height(Length::Fill)
        .align_y(Alignment::Center)
        .padding([0.0, drawer::ROW_X]);
    let b = button(face)
        .width(Length::Fill)
        .height(Length::Fixed(drawer::ROW_H))
        .padding(0)
        .style(drawer_lift);
    match on_press {
        Some(m) => b.on_press(m).into(),
        None => b.into(),
    }
}

/// The drawer's way out to the full pane: a quiet label and an arrow, on a
/// row that lifts under the pointer.
///
/// A widget because every drawer ends in one ("Network settings",
/// "Bluetooth settings") and they must read as the same exit — secondary
/// ink, never a mark, so it is not taken for one more item of the list.
pub fn drawer_link<'a, Message: Clone + 'a>(label: &str, on_press: Message) -> Element<'a, Message, Theme> {
    let face = container(
        row![
            text(label.to_string())
                .font(font::UI_MEDIUM)
                .size(size::BODY_SMALL)
                .color(color::TEXT_SECONDARY),
            Space::new().width(Length::Fill),
            text("\u{2192}")
                .font(font::DATA)
                .size(size::MONO)
                .color(color::TEXT_TERTIARY),
        ]
        .align_y(Alignment::Center),
    )
    .height(Length::Fill)
    .align_y(Alignment::Center)
    .padding([0.0, drawer::ROW_X]);
    button(face)
        .width(Length::Fill)
        .height(Length::Fixed(drawer::ROW_H))
        .padding(0)
        .style(drawer_lift)
        .on_press(on_press)
        .into()
}

/// A drawer row that says why the list is empty — "off", "searching…".
///
/// A widget because an empty drawer that shows nothing reads as broken, and
/// the note must take exactly one [`drawer::ROW_H`] so the popup that was
/// sized for it does not clip. Mono and tertiary: it is a state, not an item.
pub fn drawer_note<'a, Message: 'a>(note: &str) -> Element<'a, Message, Theme> {
    container(
        text(note.to_string())
            .font(font::DATA)
            .size(size::MONO)
            .color(color::TEXT_TERTIARY),
    )
    .width(Length::Fill)
    .height(Length::Fixed(drawer::ROW_H))
    .align_y(Alignment::Center)
    .padding([0.0, drawer::ROW_X + drawer::MARK + drawer::MARK_GAP])
    .into()
}

/// A drawer row's ground: nothing at rest, a flat white lift under the
/// pointer. Never the accent — a hover is not a state.
fn drawer_lift(_t: &Theme, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Hovered => color::LIFT,
        button::Status::Pressed => color::LIFT_STRONG,
        _ => Color::TRANSPARENT,
    };
    button::Style {
        background: Some(iced::Background::Color(background)),
        text_color: color::TEXT,
        border: iced::border::rounded(drawer::RADIUS_ROW),
        ..button::Style::default()
    }
}

/// A symbolic icon-theme glyph, recoloured and squared off.
///
/// A widget because tinting is the whole point and it is easy to get wrong: a
/// symbolic SVG ships as black paths, so drawn as-is on a black menu it is
/// invisible. iced's `svg` can filter a whole tree to one colour, which is
/// exactly what "symbolic" means, and wrapping that here keeps every caller
/// from re-deriving the closure — and from ever drawing an un-tinted
/// application-coloured icon into a menu row.
///
/// The path comes from the caller's own icon-theme lookup; this widget does
/// no filesystem work and is safe to call from a `view`.
/// A named symbolic icon, with the placeholder that keeps a rail from
/// growing a hole when a host's icon theme is missing one.
///
/// A widget because *every* mark in this desktop is a themed icon that may
/// not be installed, and the fallback must be identical everywhere — the
/// alternative is each call site inventing its own consolation shape, which
/// is how a bar ends up with a hand-drawn glyph next to a real one.
pub fn mark<'a, Message: 'a>(
    icon: Option<std::path::PathBuf>,
    side: f32,
    tint: Color,
) -> Element<'a, Message, Theme> {
    match icon {
        Some(path) => glyph(path, side, tint),
        None => quad(
            Length::Fixed(side * menu::MARK_INNER / menu::MARK),
            Length::Fixed(side * menu::MARK_INNER / menu::MARK),
            tint,
            menu::RADIUS_MARK,
        ),
    }
}

pub fn glyph<'a, Message: 'a>(
    path: std::path::PathBuf,
    side: f32,
    tint: Color,
) -> Element<'a, Message, Theme> {
    // iced's svg colour filter replaces each pixel's RGB and keeps the art's
    // own alpha — the tint's alpha is dropped. The ink ramp (`TEXT_SECONDARY`,
    // `TEXT_TERTIARY`) *is* alpha, so without this every mark drew at full
    // white regardless of the ink it was given.
    svg(svg::Handle::from_path(path))
        .width(Length::Fixed(side))
        .height(Length::Fixed(side))
        .opacity(tint.a)
        .style(move |_t: &Theme, _s: svg::Status| svg::Style {
            color: Some(Color::from_rgb(tint.r, tint.g, tint.b)),
        })
        .into()
}

/// The gap-hairline-gap that divides a context menu into groups.
///
/// A widget and not a bare [`hairline`] because its *height* is load-bearing:
/// a popup surface is given its pixel size before layout runs, so the caller
/// computing that size and the caller drawing the rule have to agree on one
/// number. That number is `tokens::menu::SEP_H`, and this is the only thing
/// that draws it.
pub fn menu_separator<'a, Message: 'a>() -> Element<'a, Message, Theme> {
    container(edge_quad(
        Length::Fill,
        Length::Fixed(space::HAIRLINE),
        color::BORDER,
    ))
    .height(Length::Fixed(menu::SEP_H))
    .align_y(iced::Alignment::Center)
    .into()
}

/// Put the spec's inset top edge highlight on a glass surface.
///
/// A widget and not a style because iced's `Shadow` is outer-only: there is no
/// `inset 0 1px 0 rgba(255,255,255,.07-.2)`, and that one line is the whole
/// difference between "translucent fill" and "glass". So the highlight is a
/// sibling hairline laid over the top of the surface, inset horizontally by
/// the radius so it stops where the corner curve starts.
///
/// The overlay is inert — a `container` of `Space` never captures the pointer,
/// so buttons underneath keep their hover and press states — and the base
/// layer sets the stack's size, so wrapping a block does not change its
/// layout.
pub fn lit<'a, Message: 'a>(
    body: impl Into<Element<'a, Message, Theme>>,
    corner: f32,
    highlight: Color,
) -> Element<'a, Message, Theme> {
    let line = column![
        container(quad(
            Length::Fill,
            Length::Fixed(space::HAIRLINE),
            highlight,
            space::HAIRLINE,
        ))
        .padding([0.0, corner]),
        Space::new().height(Length::Fill),
    ];
    stack![body.into(), line].into()
}

/// A glass sheet that floats over live wallpaper rather than inside a window:
/// a toast card, the launcher, the control centre.
///
/// A widget because the sheet's ground, edge and light source are decided
/// together by one switch, `blur` ([`crate::ipc::fetch_blur`]). With the
/// compositor's material on, the sheet is only the light tint of
/// [`crate::theme::surface`]: abyss already draws the blur, the rim and the
/// shadow underneath, and a client-drawn [`lit`] line on top would be a
/// second, misplaced light source. With it off the sheet is opaque and
/// carries its own hairline and [`lit`] top edge, because nothing else will.
///
/// `radius` is the compositor's live `decoration.rounding`. The sheet fills
/// its layer surface edge to edge — the air around it is layer-shell margin,
/// never padding inside the surface, since abyss blurs the whole surface and
/// any clear band inside it shows as a blurred rim — so this is the same
/// corner abyss masks the blur to. It also feeds [`lit`]'s inset, so the two
/// cannot drift apart.
pub fn surface<'a, Message: 'a>(
    radius: f32,
    blur: bool,
    content: impl Into<Element<'a, Message, Theme>>,
) -> Element<'a, Message, Theme> {
    let sheet = container(content)
        .padding(space::CARD)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(crate::theme::surface(radius, blur));
    if blur {
        sheet.into()
    } else {
        lit(sheet, radius, color::HIGHLIGHT)
    }
}

/// A battery gauge: a hard-edged cell with a fill proportional to charge, and
/// a nub on its right the way every battery pictogram has had since 1995.
///
/// A widget because the fill is a *magnitude* and magnitudes are this design
/// system's job — a caller that drew it inline would be reinventing
/// [`big_value`] at 10 pixels.
pub fn battery_gauge<'a, Message: 'a>(percent: u8, fill: Color) -> Element<'a, Message, Theme> {
    battery_gauge_faded(percent, fill, 1.0)
}

/// [`battery_gauge`] at `alpha` of its ink — frame and level together — for
/// a bar cell in motion. A gauge whose frame stayed at full ink while its
/// cell faded around it would be the last thing on screen.
pub fn battery_gauge_faded<'a, Message: 'a>(
    percent: u8,
    fill: Color,
    alpha: f32,
) -> Element<'a, Message, Theme> {
    let a = alpha.clamp(0.0, 1.0);
    let (fill, track) = (fill.scale_alpha(a), color::TRACK.scale_alpha(a));
    let inner = space::MARK_CELL_W - 2.0 * space::MARK_BORDER;
    let filled = inner * (percent.min(100) as f32 / 100.0);
    let level = row![
        edge_quad(Length::Fixed(filled), Length::Fill, fill),
        Space::new().width(Length::Fill),
    ];
    let cell = container(level)
        .width(Length::Fixed(space::MARK_CELL_W))
        .height(Length::Fixed(space::MARK))
        .padding(space::MARK_BORDER)
        .style(move |_t: &Theme| container::Style {
            border: iced::Border {
                color: track,
                width: space::MARK_BORDER,
                radius: 0.0.into(),
            },
            ..container::Style::default()
        });
    row![
        cell,
        edge_quad(
            Length::Fixed(space::MARK_BORDER),
            Length::Fixed(space::MARK * 0.45),
            track,
        ),
    ]
    .align_y(Alignment::Center)
    .into()
}

/// A list row: label at the left, mono value at the right — or, when the row
/// is too narrow for both on one line, the value beneath the label ([`fold`]).
/// Either way the value stays inside the row.
pub fn list_row<'a, Message: 'a>(
    label: &str,
    value: impl Into<Element<'a, Message, Theme>>,
) -> Element<'a, Message, Theme> {
    list_row_at(label, value, false)
}

/// [`list_row`] whose label can step down to tertiary: how a setting that
/// does not apply right now reads on a translucent column, where
/// [`dimmed_at`] cannot repaint the ground (see there).
pub fn list_row_at<'a, Message: 'a>(
    label: &str,
    value: impl Into<Element<'a, Message, Theme>>,
    dim: bool,
) -> Element<'a, Message, Theme> {
    container(fold(
        text(label.to_string())
            .font(font::UI)
            .size(size::BODY)
            .style(if dim {
                theme::text_tertiary
            } else {
                theme::text_secondary
            }),
        value,
    ))
    .padding([space::ROW_Y, space::CARD])
    .width(Length::Fill)
    .into()
}

/// A setting that does not apply right now, drawn at reduced strength and
/// still fully editable — never hidden. `dim == false` passes it through, so
/// a row keeps its place in the widget tree as it dims and undims.
///
/// A widget ([`crate::widget::Veil`]) and not a styled container, because the
/// dimming has to reach every control the row might hold, and iced styles
/// each control separately: there is no container property that fades what
/// is inside it.
pub fn dimmed<'a, Message: 'a>(
    content: impl Into<Element<'a, Message, Theme>>,
    dim: bool,
) -> Element<'a, Message, Theme> {
    dimmed_at(content, dim, false)
}

/// [`dimmed`] on a column that may be translucent. With `blur` on, the
/// ground under a row is the compositor's backdrop, which no client can
/// repaint: any veil there ([`color::VEIL_GLASS`] included) lands as a
/// darker inset box that dims the ground about as much as the text. So on
/// glass this paints nothing, and the caller dims what it can style — the
/// labels ([`list_row_at`]) and the run's caption. The tree is the same
/// either way.
pub fn dimmed_at<'a, Message: 'a>(
    content: impl Into<Element<'a, Message, Theme>>,
    dim: bool,
    blur: bool,
) -> Element<'a, Message, Theme> {
    let veil: &'static [iced::Color] = &color::VEIL;
    crate::widget::Veil::new(content, (dim && !blur).then_some(veil), radius::INSET).into()
}

/// A caption over a run of rows inside one panel — "Frost" above the rows
/// that tune frost. Quieter than the panel's [`micro_label`] heading, and
/// inset like a [`list_row`]'s label so it reads as belonging to the rows.
pub fn row_caption<'a, Message: 'a>(caption: &str) -> Element<'a, Message, Theme> {
    container(
        text(caption.to_string())
            .font(font::UI_MEDIUM)
            .size(size::BODY_SMALL)
            .style(theme::text_tertiary),
    )
    .padding([0.0, space::CARD])
    .into()
}

/// The colour a colour field holds, as a small disc beside the field.
/// Display only: the field is the editor. The hairline ring keeps a colour
/// that matches the panel, or a transparent one, from vanishing; the top-edge
/// highlight is the same light source every glass surface catches.
pub fn swatch<'a, Message: 'a>(fill: Color) -> Element<'a, Message, Theme> {
    container(Space::new())
        .width(Length::Fixed(space::SWATCH))
        .height(Length::Fixed(space::SWATCH))
        .style(move |_t: &Theme| container::Style {
            background: Some(iced::Background::Color(fill)),
            border: iced::Border {
                color: color::BORDER_STRONG,
                width: space::HAIRLINE,
                radius: radius::PILL.into(),
            },
            shadow: iced::Shadow {
                color: color::HIGHLIGHT_SOFT,
                offset: iced::Vector::new(0.0, -space::HAIRLINE),
                blur_radius: 0.0,
            },
            ..container::Style::default()
        })
        .into()
}

/// A [`swatch`] that opens its colour's picker. A widget and not a bare
/// button because the rim is the state: none at rest, a hairline on hover, a
/// crisp brighter one while the picker is `open`. Glassy, not glowy — nothing
/// lights past the rim.
pub fn swatch_button<'a, Message: Clone + 'a>(
    fill: Color,
    open: bool,
    on_press: Message,
) -> Element<'a, Message, Theme> {
    button(swatch(fill))
        .padding(space::SWATCH_PAD)
        .on_press(on_press)
        .style(move |_t: &Theme, status| {
            let rim = if open {
                color::TEXT_SECONDARY
            } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
                color::BORDER_STRONG
            } else {
                Color::TRANSPARENT
            };
            button::Style {
                background: None,
                border: iced::Border {
                    color: rim,
                    width: space::HAIRLINE,
                    radius: radius::PILL.into(),
                },
                ..button::Style::default()
            }
        })
        .into()
}

/// The mono right-hand side of a list row — a path, a rate, a device id.
pub fn value<'a, Message: 'a>(v: &str) -> Element<'a, Message, Theme> {
    text(v.to_string())
        .font(font::DATA)
        .size(size::MONO)
        .style(theme::text_primary)
        .into()
}

/// An inset list panel: a mono header label, then hairline-separated rows.
pub fn inset_list<'a, Message: 'a>(
    heading: &str,
    rows: Vec<Element<'a, Message, Theme>>,
) -> Element<'a, Message, Theme> {
    let mut col = Column::new().push(container(micro_label(heading)).padding([space::ROW_Y, space::CARD]));
    // A hairline before every row, the heading included — the heading is a
    // row of the list, not a caption floating above it.
    for r in rows {
        col = col.push(hairline());
        col = col.push(r);
    }
    inset(col).into()
}

/// A pill button. `selected` is the accent state; a group of these is a
/// segmented choice, and only one of them may be selected.
pub fn pill<'a, Message: Clone + 'a>(
    label: &str,
    selected: bool,
    on_press: Message,
) -> Element<'a, Message, Theme> {
    button(
        text(label.to_string())
            .font(font::UI_MEDIUM)
            .size(size::BODY_SMALL),
    )
    .padding([space::PILL_Y, space::PILL_X])
    .on_press(on_press)
    .style(theme::pill(selected))
    .into()
}

/// A segmented choice: pills in a row, one of them accented. The row wraps
/// onto a second line rather than running past its panel.
pub fn segmented<'a, T, Message>(
    options: &'a [(T, &'a str)],
    current: &T,
    on_select: impl Fn(&T) -> Message + 'a,
) -> Element<'a, Message, Theme>
where
    T: PartialEq,
    Message: Clone + 'a,
{
    pill_group(
        options
            .iter()
            .map(|(v, label)| pill(label, v == current, on_select(v)))
            .collect(),
    )
}

/// Pills side by side, wrapping onto further lines when the row runs out.
///
/// A widget so that every exclusive choice in the desktop wraps the same way
/// — `segmented` for a typed option list, a schema pane for its strings —
/// instead of one of them remembering `.wrap()` and the next one clipping
/// "Dwindle Classic" off the edge of a card.
pub fn pill_group<'a, Message: 'a>(pills: Vec<Element<'a, Message, Theme>>) -> Element<'a, Message, Theme> {
    Row::with_children(pills)
        .spacing(space::PILL_GAP)
        .wrap()
        .vertical_spacing(space::PILL_GAP)
        .into()
}

/// A sidebar nav item. The 3px accent bar is a sibling quad, because a
/// container border cannot be applied to one edge only.
pub fn nav_item<'a, Message: Clone + 'a>(
    label: &'a str,
    active: bool,
    on_press: Message,
) -> Element<'a, Message, Theme> {
    nav_item_at(Density::Regular, label, active, on_press)
}

/// [`nav_item`] at a [`Density`]. Compact drops the icon square — it is a
/// placeholder, and in a narrow sidebar the label is what identifies a pane.
pub fn nav_item_at<'a, Message: Clone + 'a>(
    density: Density,
    label: &'a str,
    active: bool,
    on_press: Message,
) -> Element<'a, Message, Theme> {
    let compact = density == Density::Compact;
    let bar = nav_bar(active);
    let glyph = nav_glyph(active);

    let name = text(label)
        .font(if active { font::UI_MEDIUM } else { font::UI })
        .size(size::BODY)
        .wrapping(text::Wrapping::None);
    let face: Element<'a, Message, Theme> = if compact {
        name.into()
    } else {
        row![glyph, name]
            .spacing(space::NAV_GLYPH_GAP)
            .align_y(Alignment::Center)
            .into()
    };
    let pad = if compact {
        [space::NAV_Y_COMPACT, space::NAV_X_COMPACT]
    } else {
        [space::NAV_Y, space::NAV_X]
    };

    row![
        bar,
        button(face)
            .padding(pad)
            .width(Length::Fill)
            .on_press(on_press)
            .style(theme::nav(active)),
    ]
    .spacing(space::NAV_BAR_GAP)
    .align_y(Alignment::Center)
    .into()
}

/// A nav item's 3px selected bar: [`color::ACCENT`] when `active`, and the
/// same room left empty when not, so a row does not shift as it is picked.
fn nav_bar<'a, Message: 'a>(active: bool) -> Element<'a, Message, Theme> {
    container(Space::new())
        .width(space::BAR_W)
        .height(space::NAV_BAR_H)
        .style(move |_t: &Theme| container::Style {
            background: Some(iced::Background::Color(if active {
                color::ACCENT
            } else {
                Color::TRANSPARENT
            })),
            border: iced::border::rounded(radius::BAR),
            ..container::Style::default()
        })
        .into()
}

/// The placeholder icon square the spec calls for: a small rounded rect,
/// not an icon. Icons are a later problem and a worse one.
fn nav_glyph<'a, Message: 'a>(active: bool) -> Element<'a, Message, Theme> {
    container(Space::new())
        .width(space::NAV_GLYPH)
        .height(space::NAV_GLYPH)
        .style(move |_t: &Theme| container::Style {
            background: Some(iced::Background::Color(if active {
                color::ACCENT_FILL
            } else {
                color::HIGHLIGHT_SOFT
            })),
            border: iced::Border {
                color: if active {
                    color::ACCENT_BORDER
                } else {
                    color::BORDER
                },
                width: space::HAIRLINE,
                radius: radius::GLYPH.into(),
            },
            ..container::Style::default()
        })
        .into()
}

/// A sidebar section that expands in place to list its sub-pages: a
/// [`nav_item_at`]-shaped head with an open/closed chevron at its far end,
/// and under it the `pages` (built with [`nav_page_at`]) in a well.
///
/// A widget and not a column in the app, because the head, the well and the
/// chevron have to agree on whether the section is open.
///
/// `open` is where the section is going and decides the layout at once: the
/// well takes its full height the moment a section opens and none the moment
/// it folds, so every row's hit target is where it will be drawn and a click
/// during the motion lands where it is aimed. `reveal` (0 to 1, animated by
/// the caller toward `open`) only uncovers the opening well top-down
/// ([`Reveal`]); it never moves a row. The tree is the same open or folded,
/// so the head's button keeps its press across the change.
///
/// `current` is whether the shown page is one of this section's. The head
/// takes the accent bar only while the section is folded over it — open, the
/// selected sub-page carries the one yellow, never both. A section with no
/// `pages` is a plain [`nav_item_at`].
pub fn nav_section_at<'a, Message: Clone + 'a>(
    density: Density,
    label: &'a str,
    current: bool,
    open: bool,
    reveal: f32,
    on_press: Message,
    pages: Vec<Element<'a, Message, Theme>>,
) -> Element<'a, Message, Theme> {
    if pages.is_empty() {
        return nav_item_at(density, label, current, on_press);
    }
    let compact = density == Density::Compact;
    let lit = current && !open;

    let name = text(label)
        .font(if current { font::UI_MEDIUM } else { font::UI })
        .size(size::BODY)
        .wrapping(text::Wrapping::None);
    // The chevron's slot is square and the size of the open one, so the
    // turn from ▸ to ▾ never moves the label.
    let s = space::NAV_CHEVRON;
    let t = space::NAV_CHEVRON_STROKE;
    let slot = 2.0 * s - t;
    let mark = if open {
        chevron(s, t, color::TEXT_TERTIARY)
    } else {
        chevron_right(s, t, color::TEXT_TERTIARY)
    };
    let mark = container(mark)
        .width(slot)
        .height(slot)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center);
    let mut face = Row::new()
        .spacing(space::NAV_GLYPH_GAP)
        .align_y(Alignment::Center);
    if !compact {
        face = face.push(nav_glyph(lit));
    }
    let face = face.push(name).push(Space::new().width(Length::Fill)).push(mark);
    let pad = if compact {
        [space::NAV_Y_COMPACT, space::NAV_X_COMPACT]
    } else {
        [space::NAV_Y, space::NAV_X]
    };
    let head = row![
        nav_bar(lit),
        button(face)
            .padding(pad)
            .width(Length::Fill)
            .on_press(on_press)
            .style(theme::nav(lit)),
    ]
    .spacing(space::NAV_BAR_GAP)
    .align_y(Alignment::Center);

    let mut list = Column::new()
        .spacing(space::NAV_SUB_GAP)
        .padding(iced::Padding::ZERO.top(space::NAV_SUB_GAP));
    for page in pages {
        list = list.push(page);
    }
    // Folded, the well is there with no height, so the tree keeps its shape.
    // `max_height`, not `height(Fixed(0.0))`: a column drops a `Fixed(0)`
    // child from the tree altogether.
    let well = container(list).width(Length::Fill).clip(true);
    let well = if open { well } else { well.max_height(0.0) };
    let reveal = if open { reveal.clamp(0.0, 1.0) } else { 0.0 };
    column![head, Reveal::new(well, reveal)].into()
}

/// Content laid out whole and drawn only down to `fraction` of its height:
/// a top-down wipe whose layout never moves.
///
/// A widget and not a clipped container, because a container that clips
/// also lays its content out in the clipped height — the rows below it ride
/// the animation, and a click aimed at one lands on whatever has moved under
/// the pointer by the time the button is released. Here layout is final from
/// the first frame and only drawing animates. Input outside the uncovered
/// part sees no cursor, so nothing undrawn can be hovered or clicked.
pub struct Reveal<'a, Message> {
    content: Element<'a, Message, Theme>,
    fraction: f32,
}

impl<'a, Message> Reveal<'a, Message> {
    pub fn new(content: impl Into<Element<'a, Message, Theme>>, fraction: f32) -> Self {
        Self {
            content: content.into(),
            fraction: fraction.clamp(0.0, 1.0),
        }
    }

    /// The uncovered part of `bounds`.
    fn shown(&self, bounds: iced::Rectangle) -> iced::Rectangle {
        iced::Rectangle {
            height: bounds.height * self.fraction,
            ..bounds
        }
    }

    /// The cursor as the content may see it: absent over the covered part.
    fn cursor(&self, bounds: iced::Rectangle, cursor: iced::mouse::Cursor) -> iced::mouse::Cursor {
        match cursor.position() {
            Some(p) if bounds.contains(p) && !self.shown(bounds).contains(p) => {
                iced::mouse::Cursor::Unavailable
            }
            _ => cursor,
        }
    }
}

impl<Message> iced::advanced::Widget<Message, Theme, iced::Renderer> for Reveal<'_, Message> {
    fn children(&self) -> Vec<iced::advanced::widget::Tree> {
        vec![iced::advanced::widget::Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut iced::advanced::widget::Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> iced::Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut iced::advanced::widget::Tree,
        renderer: &iced::Renderer,
        limits: &iced::advanced::layout::Limits,
    ) -> iced::advanced::layout::Node {
        let child = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        iced::advanced::layout::Node::with_children(child.size(), vec![child])
    }

    fn operate(
        &mut self,
        tree: &mut iced::advanced::widget::Tree,
        layout: iced::advanced::Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn iced::advanced::widget::Operation,
    ) {
        self.content.as_widget_mut().operate(
            &mut tree.children[0],
            layout.children().next().expect("a reveal has one child"),
            renderer,
            operation,
        );
    }

    fn update(
        &mut self,
        tree: &mut iced::advanced::widget::Tree,
        event: &iced::Event,
        layout: iced::advanced::Layout<'_>,
        cursor: iced::mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn iced::advanced::Clipboard,
        shell: &mut iced::advanced::Shell<'_, Message>,
        viewport: &iced::Rectangle,
    ) {
        let cursor = self.cursor(layout.bounds(), cursor);
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout.children().next().expect("a reveal has one child"),
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &iced::advanced::widget::Tree,
        layout: iced::advanced::Layout<'_>,
        cursor: iced::mouse::Cursor,
        viewport: &iced::Rectangle,
        renderer: &iced::Renderer,
    ) -> iced::mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout.children().next().expect("a reveal has one child"),
            self.cursor(layout.bounds(), cursor),
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &iced::advanced::widget::Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &iced::advanced::renderer::Style,
        layout: iced::advanced::Layout<'_>,
        cursor: iced::mouse::Cursor,
        viewport: &iced::Rectangle,
    ) {
        use iced::advanced::Renderer as _;
        let shown = self.shown(layout.bounds());
        if shown.height <= 0.0 {
            return;
        }
        let child = layout.children().next().expect("a reveal has one child");
        let cursor = self.cursor(layout.bounds(), cursor);
        if self.fraction >= 1.0 {
            self.content
                .as_widget()
                .draw(&tree.children[0], renderer, theme, style, child, cursor, viewport);
            return;
        }
        renderer.with_layer(shown, |renderer| {
            self.content
                .as_widget()
                .draw(&tree.children[0], renderer, theme, style, child, cursor, viewport);
        });
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut iced::advanced::widget::Tree,
        layout: iced::advanced::Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &iced::Rectangle,
        translation: iced::Vector,
    ) -> Option<iced::advanced::overlay::Element<'b, Message, Theme, iced::Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout.children().next().expect("a reveal has one child"),
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: 'a> From<Reveal<'a, Message>> for Element<'a, Message, Theme> {
    fn from(reveal: Reveal<'a, Message>) -> Self {
        Element::new(reveal)
    }
}

/// One sub-page under an expanded [`nav_section_at`]: shorter than a
/// section row, its label indented under the section's, and no icon square.
///
/// A widget because its selected state is the sidebar's glassy one — the
/// row a step brighter, a crisp one-pixel rim, the 3px accent bar beside it
/// — and that has to be the same in every app with a two-level sidebar.
pub fn nav_page_at<'a, Message: Clone + 'a>(
    density: Density,
    label: &'a str,
    active: bool,
    on_press: Message,
) -> Element<'a, Message, Theme> {
    let indent = match density {
        Density::Regular => space::NAV_SUB_INDENT,
        Density::Compact => space::NAV_SUB_INDENT_COMPACT,
    };
    let name = text(label)
        .font(if active { font::UI_MEDIUM } else { font::UI })
        .size(size::BODY_SMALL)
        .wrapping(text::Wrapping::None);
    let face = container(name).height(Length::Fill).align_y(Alignment::Center);
    row![
        nav_bar(active),
        button(face)
            .padding(iced::Padding::ZERO.left(indent).right(space::NAV_X))
            .width(Length::Fill)
            .height(space::NAV_SUB_H)
            .on_press(on_press)
            .style(nav_page_style(active)),
    ]
    .spacing(space::NAV_BAR_GAP)
    .align_y(Alignment::Center)
    .into()
}

/// [`nav_page_at`]'s button: selected is a brighter fill and a crisp rim,
/// hovered a softer lift. Never a glow.
fn nav_page_style(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_t, status| {
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let fill = match (active, hovered) {
            (true, _) => color::HIGHLIGHT_SOFT,
            (false, true) => color::LIFT_SOFT,
            (false, false) => Color::TRANSPARENT,
        };
        button::Style {
            background: Some(iced::Background::Color(fill)),
            text_color: if active || hovered {
                color::TEXT
            } else {
                color::TEXT_SECONDARY
            },
            border: iced::Border {
                color: if active { color::BORDER } else { Color::TRANSPARENT },
                width: space::HAIRLINE,
                radius: radius::INSET.into(),
            },
            ..button::Style::default()
        }
    }
}

/// [`chevron`] turned a quarter to point right (▸): a section that is
/// folded. Drawn the same way — a staircase of quads, one per row — so the
/// two read as one mark turning.
fn chevron_right<'a, Message: 'a>(extent: f32, thickness: f32, stroke: Color) -> Element<'a, Message, Theme> {
    let steps = (extent / thickness).round().max(2.0) as usize;
    let t = thickness;
    let mut col = Column::new();
    for r in 0..(2 * steps - 1) {
        let at = r.min(2 * steps - 2 - r) as f32 * t;
        col = col.push(
            Row::new()
                .push(Space::new().width(Length::Fixed(at)).height(Length::Fixed(t)))
                .push(edge_quad(Length::Fixed(t), Length::Fixed(t), stroke)),
        );
    }
    container(col)
        .width(Length::Fixed(steps as f32 * t))
        .height(Length::Fixed((2 * steps - 1) as f32 * t))
        .into()
}

/// The 214px left column: nav items above, a mono status block at the foot.
pub fn sidebar<'a, Message: 'a>(
    items: Vec<Element<'a, Message, Theme>>,
    footer: Vec<(&'a str, String)>,
) -> Element<'a, Message, Theme> {
    sidebar_at(Density::Regular, false, items, footer)
}

/// [`sidebar`] at a [`Density`]: [`space::SIDEBAR_W`] regular,
/// [`space::SIDEBAR_W_COMPACT`] compact. Build its items with
/// [`nav_item_at`] at the same density. `blur` is whether the compositor's
/// material is behind the window ([`crate::theme::sidebar`]).
pub fn sidebar_at<'a, Message: 'a>(
    density: Density,
    blur: bool,
    items: Vec<Element<'a, Message, Theme>>,
    footer: Vec<(&'a str, String)>,
) -> Element<'a, Message, Theme> {
    let (width, pad_x) = match density {
        Density::Regular => (space::SIDEBAR_W, space::SIDEBAR_X),
        Density::Compact => (space::SIDEBAR_W_COMPACT, space::SIDEBAR_X_COMPACT),
    };
    let mut nav = Column::new().spacing(2);
    for item in items {
        nav = nav.push(item);
    }

    let mut foot = Column::new().spacing(4);
    for (k, v) in footer {
        foot = foot.push(
            row![
                text(k)
                    .font(font::DATA)
                    .size(size::MICRO)
                    .style(theme::text_tertiary),
                Space::new().width(Length::Fill),
                text(v)
                    .font(font::DATA)
                    .size(size::MICRO)
                    .style(theme::text_secondary),
            ]
            .align_y(Alignment::Center),
        );
    }

    let rail = container(
        column![nav, Space::new().height(Length::Fill), foot]
            .spacing(space::BLOCK)
            .width(Length::Fill),
    )
    .width(width)
    .height(Length::Fill)
    .padding([space::PANE_Y, pad_x])
    .style(theme::sidebar(blur));
    // The one line between the rail and the content: both are the same
    // glass a step apart in density, so the edge is a hairline, not a slab
    // meeting a slab.
    row![rail, rule::vertical(space::HAIRLINE).style(theme::hairline)]
        .height(Length::Fill)
        .into()
}

/// The content column to the right of the sidebar: 26/30 padding, 18 between
/// blocks.
pub fn content<'a, Message: 'a>(blocks: Vec<Element<'a, Message, Theme>>) -> Element<'a, Message, Theme> {
    content_at(Density::Regular, blocks)
}

/// [`content`] at a [`Density`]. The column fills a narrow window and stops
/// at [`space::CONTENT_MAX`] in a wide one, held against the sidebar with the
/// pane's own padding: centred, a 2560px window opened a gap of several
/// hundred pixels between the nav and the pane it selects.
pub fn content_at<'a, Message: 'a>(
    density: Density,
    blocks: Vec<Element<'a, Message, Theme>>,
) -> Element<'a, Message, Theme> {
    let col = Column::with_children(blocks)
        .spacing(space::BLOCK)
        .width(Length::Fill)
        .max_width(space::CONTENT_MAX);
    let pad = match density {
        Density::Regular => [space::PANE_Y, space::PANE_X],
        Density::Compact => [space::PANE_Y_COMPACT, space::PANE_X_COMPACT],
    };
    container(col)
        .padding(pad)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// A big number with a mono unit beside it — the one live value on a pane.
pub fn big_value<'a, Message: 'a>(number: &str, unit: &str, accented: bool) -> Element<'a, Message, Theme> {
    row![
        text(number.to_string())
            .font(Font {
                weight: iced::font::Weight::Medium,
                ..font::DATA
            })
            .size(size::BIG_NUMBER)
            .style(if accented {
                theme::text_accent
            } else {
                theme::text_primary
            }),
        text(unit.to_string())
            .font(font::DATA)
            .size(size::BODY_SMALL)
            .style(theme::text_tertiary),
    ]
    .spacing(5)
    .align_y(Alignment::End)
    .into()
}

/// The two-line mono status chip every reference pane carries at the far
/// right of its header: line one is the state, line two is the measurement.
///
/// A widget and not a styled column because the pairing is the rule — a pane
/// that shows only the state, or only the number, is the thing this exists to
/// stop. It is deliberately colourless: the chip reports, it does not mark
/// state, so it never spends the pane's yellow.
// A chip never wraps: it sits beside a subtitle that is free to, and a
// state split over two lines reads as two states.
pub fn status_chip<'a, Message: 'a>(state: &str, measure: &str) -> Element<'a, Message, Theme> {
    container(
        column![
            text(state.to_string())
                .font(font::DATA_MEDIUM)
                .size(size::MICRO)
                .style(theme::text_secondary)
                .wrapping(text::Wrapping::None),
            text(measure.to_string())
                .font(font::DATA)
                .size(size::MICRO)
                .style(theme::text_tertiary)
                .wrapping(text::Wrapping::None),
        ]
        .spacing(3)
        .align_x(Alignment::End),
    )
    .into()
}

/// The prompt band: one live line of input at hero scale, a mono marker at
/// its left, and an optional reading at its right.
///
/// A widget because it is a *hero shape*, not a styled text field — the band
/// is the block a launcher-shaped pane looks at, and the whole point is that
/// it does not share a silhouette with the list under it. The field inside it
/// should use [`crate::theme::prompt_input`]; the band is already the box.
pub fn prompt_band<'a, Message: 'a>(
    marker: &str,
    field: impl Into<Element<'a, Message, Theme>>,
    trailing: Option<Element<'a, Message, Theme>>,
) -> Element<'a, Message, Theme> {
    let mut r = row![
        text(marker.to_string())
            .font(font::DATA_MEDIUM)
            .size(size::PROMPT)
            .style(theme::text_tertiary),
        container(field.into()).width(Length::Fill),
    ]
    .spacing(space::CARD * 0.75)
    .align_y(Alignment::Center);
    if let Some(t) = trailing {
        r = r.push(t);
    }

    container(r)
        .padding([space::ROW_Y, space::CARD])
        .width(Length::Fill)
        .style(|_t: &Theme| container::Style {
            background: Some(iced::Background::Color(color::GLASS_STRONG)),
            border: iced::Border {
                color: color::BORDER_STRONG,
                width: 1.0,
                radius: radius::INSET.into(),
            },
            ..container::Style::default()
        })
        .into()
}

/// A glass cell's look, at rest, as a container ground rather than a button's.
///
/// [`theme::glass_cell`] is a button style because nearly every glass cell is
/// pressed; the few that are not (a prompt, a keycap) still have to be the
/// same material, so they take the same style at `Active` rather than a
/// hand-copied fill, rim and drop that would drift from it.
fn glass_ground(tone: theme::CellTone, radius: f32) -> impl Fn(&Theme) -> container::Style {
    move |t: &Theme| {
        let cell = theme::glass_cell(tone, radius)(t, button::Status::Active);
        container::Style {
            background: cell.background,
            border: cell.border,
            shadow: cell.shadow,
            ..container::Style::default()
        }
    }
}

/// The prompt cell: [`prompt_band`]'s hero — one live line of input at hero
/// scale, a mono marker at its left, an optional reading at its right — cut
/// from the glass-cell material ([`theme::glass_cell`]) instead of a stroked
/// band.
///
/// A widget for the reason [`prompt_band`] is one (it is a hero shape, not a
/// styled field), and separate from it because the two grounds belong to two
/// kinds of surface. The band's strong frame holds an edge on an opaque pane;
/// on a floating glass sheet a stroke used as structure fights the
/// compositor's own rim, so here the depth is the cell's faint fill, its
/// inside hairline and its soft drop — the same lozenge as a taskbar chip, at
/// the sheet's inset radius. The field inside should use
/// [`crate::theme::prompt_input`]; the cell is already the box.
pub fn prompt_cell<'a, Message: 'a>(
    marker: &str,
    field: impl Into<Element<'a, Message, Theme>>,
    trailing: Option<Element<'a, Message, Theme>>,
) -> container::Container<'a, Message, Theme> {
    let mut r = row![
        text(marker.to_string())
            .font(font::DATA_MEDIUM)
            .size(size::PROMPT)
            .style(theme::text_tertiary),
        container(field.into()).width(Length::Fill),
    ]
    .spacing(space::CARD * 0.75)
    .align_y(Alignment::Center);
    if let Some(t) = trailing {
        r = r.push(t);
    }

    container(r)
        .padding([space::ROW_Y, space::CARD])
        .width(Length::Fill)
        .align_y(Alignment::Center)
        .style(glass_ground(theme::CellTone::Plain, radius::INSET))
}

/// A key and what it does: `esc close`, with the key in a small glass cap.
///
/// A widget because a hint line is a sentence of alternating keys and verbs,
/// and a key typed as plain text (`enter run`) reads as two words of prose.
/// The cap is the glass cell at keycap scale — never accented: a hint is
/// furniture and must not spend a pane's yellow.
pub fn key_hint<'a, Message: 'a>(key: &str, verb: &str) -> Element<'a, Message, Theme> {
    row![
        container(
            text(key.to_string())
                .font(font::DATA_MEDIUM)
                .size(size::MICRO)
                .style(theme::text_secondary)
                .wrapping(text::Wrapping::None),
        )
        .padding([space::BADGE_Y, space::BADGE_X])
        .style(glass_ground(theme::CellTone::Plain, radius::KEYCAP)),
        text(verb.to_string())
            .font(font::DATA)
            .size(size::MICRO)
            .style(theme::text_tertiary)
            .wrapping(text::Wrapping::None),
    ]
    .spacing(space::KEY_GAP)
    .align_y(Alignment::Center)
    .into()
}

/// A hint line: [`key_hint`]s in order, [`space::HINT_GAP`] apart.
///
/// A widget so the launcher and the bar's start menu space their hint
/// lines the same way, and so `ui.show-key-hints` has one shape to hide.
pub fn key_hints<'a, Message: 'a>(hints: &[(&str, &str)]) -> Element<'a, Message, Theme> {
    hints
        .iter()
        .fold(
            Row::new().spacing(space::HINT_GAP).align_y(Alignment::Center),
            |line, &(key, verb)| line.push(key_hint(key, verb)),
        )
        .into()
}

/// One object on a spatial canvas: a mono name, with its position in an
/// ordered lane as a tertiary ordinal before it.
///
/// A widget and not a [`pill`] because a pill is a verb and this is a noun —
/// on a canvas the two sit side by side (the chips, and the pills that move
/// them), and they must not be mistaken for each other. Squarer corners and a
/// mono face are what keep them apart. `selected` is the canvas's one yellow.
pub fn chip<'a, Message: Clone + 'a>(
    ordinal: Option<usize>,
    label: &str,
    selected: bool,
    on_press: Message,
) -> Element<'a, Message, Theme> {
    let mut r = Row::new()
        .spacing(space::CHIP_ORDINAL_GAP)
        .align_y(Alignment::Center);
    if let Some(n) = ordinal {
        r = r.push(
            text(n.to_string())
                .font(font::DATA)
                .size(size::MICRO)
                .style(if selected {
                    theme::text_accent
                } else {
                    theme::text_tertiary
                }),
        );
    }
    r = r.push(text(label.to_string()).font(font::DATA_MEDIUM).size(size::MONO));
    button(r)
        .padding([space::CHIP_Y, space::CHIP_X])
        .on_press(on_press)
        .style(theme::chip(selected))
        .into()
}

/// One region of a spatial canvas: a caption and a count over the chips that
/// live there, wrapping, on the inset ground.
///
/// A widget because a canvas is a hero shape (COMPOSITION.md) and there was
/// no primitive for one. The lane keeps its height when it empties, so the
/// canvas does not jump under the pointer when the last chip leaves.
pub fn lane<'a, Message: 'a>(
    label: &str,
    measure: &str,
    chips: Vec<Element<'a, Message, Theme>>,
) -> Element<'a, Message, Theme> {
    let head = row![
        micro_label(label),
        Space::new().width(Length::Fill),
        text(measure.to_string())
            .font(font::DATA)
            .size(size::MICRO)
            .style(theme::text_tertiary),
    ]
    .align_y(Alignment::Center);

    let body: Element<'a, Message, Theme> = if chips.is_empty() {
        text("empty")
            .font(font::DATA)
            .size(size::MONO)
            .style(theme::text_tertiary)
            .into()
    } else {
        Row::with_children(chips)
            .spacing(space::CHIP_GAP)
            .wrap()
            .vertical_spacing(space::CHIP_GAP)
            .into()
    };

    let body = row![Space::new().height(space::CHIP_H), body].align_y(Alignment::Center);
    inset(column![head, body].spacing(space::ROW_Y))
        .padding([space::ROW_Y, space::CARD])
        .height(Length::Shrink)
        .into()
}

/// A row of a list that can be the current choice: a 3px accent bar at the
/// left edge and a tinted ground when it is.
///
/// A widget and not a styled container for the same reason [`nav_item`] is
/// one — a one-sided border is not expressible as a container border, so the
/// bar has to be a sibling quad, and every list in the desktop that has a
/// current row should draw it identically.
pub fn choice_row<'a, Message: 'a>(
    content: impl Into<Element<'a, Message, Theme>>,
    selected: bool,
) -> Element<'a, Message, Theme> {
    let bar = container(Space::new())
        .width(space::BAR_W)
        .height(Length::Fill)
        .style(move |_t: &Theme| container::Style {
            background: Some(iced::Background::Color(if selected {
                color::ACCENT
            } else {
                Color::TRANSPARENT
            })),
            ..container::Style::default()
        });

    let body = container(row![bar, content.into()].align_y(Alignment::Center)).width(Length::Fill);

    if selected {
        body.style(|_t: &Theme| container::Style {
            background: Some(iced::Background::Color(color::ACCENT_FILL)),
            ..container::Style::default()
        })
        .into()
    } else {
        body.into()
    }
}

/// A draggable number that can also be typed: a slider and the entry box that
/// shows its reading.
///
/// A widget and not two styled children because the pairing is the rule — the
/// readout beside a slider is the only place the exact value is legible, and
/// a readout you cannot type into is a control with a resolution limit set by
/// how wide the track happens to be. Every draggable number in the desktop is
/// built from this, so the commit contract (Enter applies, a draft that does
/// not parse is refused and reverts) is written once.
///
/// The caller owns formatting and clamping: it passes the text to show and
/// receives the text typed. This widget decides nothing about the value.
pub struct NumericSlider<'a, Message> {
    range: std::ops::RangeInclusive<f64>,
    value: f64,
    step: f64,
    shown: String,
    invalid: bool,
    on_slide: Box<dyn Fn(f64) -> Message + 'a>,
    on_release: Option<Message>,
    on_type: Box<dyn Fn(String) -> Message + 'a>,
    on_commit: Option<Message>,
    /// `None`: no unit slot. `Some("")`: the slot, reserved and empty.
    unit: Option<&'a str>,
}

impl<'a, Message: Clone + 'a> NumericSlider<'a, Message> {
    pub fn new(
        range: std::ops::RangeInclusive<f64>,
        value: f64,
        shown: impl Into<String>,
        on_slide: impl Fn(f64) -> Message + 'a,
        on_type: impl Fn(String) -> Message + 'a,
    ) -> Self {
        Self {
            range,
            value,
            step: 1.0,
            shown: shown.into(),
            invalid: false,
            on_slide: Box::new(on_slide),
            on_release: None,
            on_type: Box::new(on_type),
            on_commit: None,
            unit: None,
        }
    }

    pub fn step(mut self, step: f64) -> Self {
        self.step = step;
        self
    }

    /// The write happens here, not on every pixel of the drag.
    pub fn on_release(mut self, message: Message) -> Self {
        self.on_release = Some(message);
        self
    }

    pub fn on_commit(mut self, message: Message) -> Self {
        self.on_commit = Some(message);
        self
    }

    /// A draft that does not parse. It keeps no commit path at all, rather
    /// than a commit that fails after the fact.
    pub fn invalid(mut self, invalid: bool) -> Self {
        self.invalid = invalid;
        self
    }

    /// The unit the reading is in ("px"), set after the entry in tertiary
    /// ink so the number stays the thing read. Outside the entry, so typing
    /// never has to step around it. Calling this at all reserves the slot —
    /// `None` leaves it empty — so a panel mixing lengths and ratios keeps
    /// its entries in one column.
    pub fn unit(mut self, unit: Option<&'a str>) -> Self {
        self.unit = Some(unit.unwrap_or_default());
        self
    }
}

impl<'a, Message: Clone + 'a> From<NumericSlider<'a, Message>> for Element<'a, Message, Theme> {
    fn from(n: NumericSlider<'a, Message>) -> Self {
        let mut track = iced::widget::slider(n.range, n.value, n.on_slide)
            .step(n.step)
            .style(theme::eclipse_slider)
            .width(Length::Fixed(space::SLIDER_W));
        if let Some(release) = n.on_release {
            track = track.on_release(release);
        }

        let mut entry = iced::widget::text_input("", &n.shown)
            .on_input(n.on_type)
            .font(font::DATA)
            .size(size::MONO)
            .padding(space::ROW_Y / 2.0)
            .align_x(Alignment::End)
            .width(Length::Fixed(space::NUMBER_W))
            .style(if n.invalid {
                theme::eclipse_input_invalid
            } else {
                theme::eclipse_input
            });
        if let (false, Some(commit)) = (n.invalid, n.on_commit) {
            entry = entry.on_submit(commit);
        }

        // A fixed track, not a filling one: a `Shrink` row compresses its
        // main axis, so a fill child would get no width at all. [`fold`]
        // stacks the pair under its label when the row is too narrow.
        let mut pair = row![track, entry]
            .spacing(space::CONTROL_GAP)
            .align_y(Alignment::Center);
        if let Some(unit) = n.unit {
            pair = pair.push(
                iced::widget::text(unit)
                    .font(font::DATA)
                    .size(size::MONO)
                    .style(theme::text_tertiary)
                    .width(Length::Fixed(space::UNIT_W)),
            );
        }
        pair.into()
    }
}

// ------------------------------------------------------- the bar, in a pane
//
// ADR 0065. A settings pane that shows the bar has to *draw* the bar, from the
// same metrics the bar uses, without being the bar: no services, no layer
// shell, and no dependency on the taskbar crate. These are the pieces that
// exist only in that picture, or only in the pane around it.

/// The bar's sheet, drawn inside a pane: the pill ground, its hairline and its
/// lit top edge, [`crate::tokens::bar::PILL_H`] tall.
///
/// A widget for the reason [`surface`] is one: the sheet is three things at
/// once, and a pane that assembled two of them would draw a bar that is not
/// quite the bar. `content` is laid out inside the sheet's edge padding; a
/// preview that positions its cells itself passes a `stack` of [`placed`]
/// layers.
pub fn bar_sheet<'a, Message: 'a>(
    content: impl Into<Element<'a, Message, Theme>>,
    width: Length,
) -> Element<'a, Message, Theme> {
    use crate::tokens::bar;
    lit(
        container(content)
            .padding([0.0, bar::EDGE])
            .width(width)
            .height(Length::Fixed(bar::PILL_H))
            .align_y(Alignment::Center)
            .clip(true)
            // Always the blur-off ground: this sheet is a picture inside an
            // opaque pane, with no compositor material behind it.
            .style(theme::bar_ground(bar::RADIUS_SHEET, false)),
        bar::RADIUS_SHEET,
        color::HIGHLIGHT_SOFT,
    )
}

/// A bar cell that is a picture of one: a window chip or the launcher, drawn
/// still, at `width`, with its content clipped rather than squashed.
///
/// A widget and not a styled container because a preview animates the width
/// through every value between two rungs of the chip ladder, and the content
/// must be clipped at the cell's edge the whole way; a container that let its
/// content decide its width would snap from rung to rung instead. The ground
/// is the bar's own cell at rest ([`theme::bar_cell`]): the neutral glass
/// lozenge, or the focused window's gold fill when `accent` is set, for a
/// pane whose single live value is on the bar.
pub fn bar_cell_frame<'a, Message: 'a>(
    content: impl Into<Element<'a, Message, Theme>>,
    width: f32,
    accent: bool,
) -> Element<'a, Message, Theme> {
    use crate::tokens::bar;
    let width = width.max(0.0);
    container(content)
        .width(Length::Fixed(width))
        .height(Length::Fixed(bar::TASK_H))
        .padding([0.0, bar::CELL_X.min(width / 2.0)])
        .align_y(Alignment::Center)
        .clip(true)
        .style(move |t: &Theme| {
            let cell = theme::bar_cell(accent)(t, iced::widget::button::Status::Active);
            container::Style {
                background: cell.background,
                border: cell.border,
                shadow: cell.shadow,
                ..container::Style::default()
            }
        })
        .into()
}

/// `content` at `x` pixels from the left of a layer, centred on the layer's
/// height.
///
/// A widget because iced has no absolute positioning, and a picture of the bar
/// needs it: cells slide to new places as the order changes, and a `Row`
/// would jump them there. A `stack` of these, one per cell, is a canvas whose
/// every `x` can be an animated value.
pub fn placed<'a, Message: 'a>(
    x: f32,
    content: impl Into<Element<'a, Message, Theme>>,
) -> Element<'a, Message, Theme> {
    row![Space::new().width(Length::Fixed(x.max(0.0))), content.into()]
        .height(Length::Fill)
        .align_y(Alignment::Center)
        .into()
}

/// The pin: a square head on a short stem, the mark of a widget that never
/// compresses.
///
/// A widget because it is a glyph the desktop has no icon for, drawn from
/// quads the way [`chevron`] is, and it has to be the same glyph on a lane
/// tile and on the sheet row that sets it.
pub fn pin<'a, Message: 'a>(ink: Color) -> Element<'a, Message, Theme> {
    use crate::tokens::canvas;
    container(
        column![
            edge_quad(
                Length::Fixed(canvas::PIN_HEAD),
                Length::Fixed(canvas::PIN_HEAD),
                ink
            ),
            edge_quad(
                Length::Fixed(canvas::PIN_STEM_W),
                Length::Fixed(canvas::PIN_STEM_H),
                ink
            ),
        ]
        .align_x(Alignment::Center),
    )
    .width(Length::Fixed(canvas::PIN_W))
    .align_x(Alignment::Center)
    .into()
}

/// One widget in an ordered lane: a grip to drag it by, its name, and a
/// [`pin`] when it never compresses. Pressing the name selects it.
///
/// A widget and not a [`chip`] because it is two gestures in one object (the
/// grip drags, the body selects) and because it has to read as the thing it
/// stands for: the bar's own cell, hairline and radius, so the lane under a
/// bar preview is visibly the same objects in the same order. `selected` is
/// the lane's one yellow. `width` is the caller's, so a lane of many widgets
/// stays one row.
pub fn widget_tile<'a, Message: Clone + 'a>(
    grip: impl Into<Element<'a, Message, Theme>>,
    label: &str,
    width: f32,
    pinned: bool,
    selected: bool,
    on_select: Message,
) -> Element<'a, Message, Theme> {
    use crate::tokens::{bar, canvas};
    let room = width - bar::GRIP_W - canvas::PIN_W - 2.0 * bar::WIDGET_X;
    let chars = (room / canvas::MONO_CHAR_W).floor().max(1.0) as usize;
    let ink = if selected { color::ACCENT_TEXT } else { color::TEXT };
    let mark: Element<'a, Message, Theme> = if pinned {
        pin(if selected {
            color::ACCENT_TEXT
        } else {
            color::TEXT_SECONDARY
        })
    } else {
        Space::new().width(Length::Fixed(canvas::PIN_W)).into()
    };
    let body = button(
        row![
            text(crate::widget::elide(label, chars))
                .font(font::DATA_MEDIUM)
                .size(size::MONO)
                .color(ink)
                .wrapping(text::Wrapping::None),
            Space::new().width(Length::Fill),
            mark,
        ]
        .height(Length::Fill)
        .align_y(Alignment::Center),
    )
    .padding([0.0, bar::WIDGET_X])
    .width(Length::Fill)
    .height(Length::Fill)
    .on_press(on_select)
    .style(move |_t: &Theme, status: button::Status| button::Style {
        background: match status {
            button::Status::Hovered => Some(iced::Background::Color(color::LIFT_SOFT)),
            button::Status::Pressed => Some(iced::Background::Color(color::LIFT)),
            _ => None,
        },
        text_color: ink,
        border: iced::border::rounded(bar::RADIUS_CELL),
        ..button::Style::default()
    });
    container(
        row![grip.into(), body]
            .height(Length::Fill)
            .align_y(Alignment::Center),
    )
    .width(Length::Fixed(width))
    .height(Length::Fixed(canvas::TILE_H))
    .style(move |_t: &Theme| container::Style {
        background: selected.then_some(iced::Background::Color(color::ACCENT_WASH)),
        border: iced::Border {
            color: if selected {
                color::ACCENT_BORDER
            } else {
                color::BORDER
            },
            width: crate::tokens::bar::HAIRLINE,
            radius: crate::tokens::bar::RADIUS_CELL.into(),
        },
        ..container::Style::default()
    })
    .into()
}

/// A hairline rail with one bar cell on it at `position` (0 left, 1 right):
/// the demo a motion setting moves, so a curve can be seen and not just named.
///
/// A widget because the cell's travel is the whole point and has to be exact
/// (it never leaves the rail at either end, whatever its width), and because
/// the motion section reads as a different silhouette from the lists around
/// it only if this is drawn the same way every time.
pub fn glide_track<'a, Message: 'a>(position: f32, label: &str) -> Element<'a, Message, Theme> {
    use crate::tokens::canvas;
    let travel = canvas::GLIDE_W - canvas::GLIDE_CHIP_W;
    let rail = container(edge_quad(
        Length::Fill,
        Length::Fixed(canvas::GLIDE_RAIL),
        color::TRACK,
    ))
    .height(Length::Fill)
    .align_y(Alignment::Center);
    let cell = bar_cell_frame(
        container(
            text(label.to_string())
                .font(font::DATA_MEDIUM)
                .size(size::MICRO)
                .style(theme::text_secondary)
                .wrapping(text::Wrapping::None),
        )
        .width(Length::Fill)
        .align_x(Alignment::Center),
        canvas::GLIDE_CHIP_W,
        false,
    );
    container(stack![rail, placed(travel * position.clamp(0.0, 1.0), cell)])
        .width(Length::Fixed(canvas::GLIDE_W))
        .height(Length::Fixed(canvas::TILE_H))
        .into()
}

/// One argument of a command, as a chip with a remove mark.
///
/// A widget because an argv editor's whole promise is that each chip is
/// exactly one argument, passed as-is with no shell in between; a text field
/// with spaces in it would be quietly re-split. It is a [`chip`]'s ground so
/// arguments read as objects, and the remove mark is inside it so the object
/// and its verb cannot drift apart. Past [`canvas::ARG_MAX_CHARS`](crate::tokens::canvas::ARG_MAX_CHARS)
/// characters the text ends in an ellipsis; the argument itself is untouched.
pub fn arg_chip<'a, Message: Clone + 'a>(arg: &str, on_remove: Message) -> Element<'a, Message, Theme> {
    use crate::tokens::canvas;
    // An empty argument is legal and invisible; show it as the quotes it is.
    // A long one is cut short: the chip is a handle on the argument, not its
    // full text, and one URL must not run the row off the panel.
    let shown = if arg.is_empty() {
        "\"\"".to_owned()
    } else {
        super::bar_widget::elide(arg, canvas::ARG_MAX_CHARS)
    };
    button(
        row![
            text(shown)
                .font(font::DATA_MEDIUM)
                .size(size::MONO)
                .wrapping(text::Wrapping::None),
            text("\u{d7}")
                .font(font::DATA)
                .size(size::MONO)
                .style(theme::text_tertiary),
        ]
        .spacing(canvas::ARG_GAP)
        .align_y(Alignment::Center),
    )
    .padding([space::CHIP_Y, space::CHIP_X])
    .on_press(on_remove)
    .style(theme::chip(false))
    .into()
}

/// A config error with its position: `file:line:col`, the message, the
/// offending line, and a caret under the span it names.
///
/// A widget because the compositor already says *where* (a line, a column and
/// a span length) and a pane that flattened that into a sentence would throw
/// the most useful part away. The caret is measured in mono character cells
/// ([`crate::tokens::canvas::MONO_CHAR_W`]), so it lines up under the snippet
/// it annotates. Danger ink, never the accent: a refusal is not state.
pub fn config_error<'a, Message: 'a>(
    position: &str,
    message: &str,
    snippet: &str,
    col: usize,
    span: usize,
) -> Element<'a, Message, Theme> {
    use crate::tokens::canvas;
    let mut body = Column::new().spacing(space::PILL_GAP).push(
        row![
            text(position.to_string())
                .font(font::DATA_MEDIUM)
                .size(size::MONO)
                .style(theme::text_danger)
                .wrapping(text::Wrapping::None),
            text(message.to_string())
                .font(font::UI)
                .size(size::BODY_SMALL)
                .style(theme::text_secondary),
        ]
        .spacing(space::CONTROL_GAP)
        .align_y(Alignment::Center),
    );
    if !snippet.is_empty() {
        let lead = col.saturating_sub(1) as f32 * canvas::MONO_CHAR_W;
        let run = span.max(1) as f32 * canvas::MONO_CHAR_W;
        body = body.push(
            column![
                text(snippet.to_string())
                    .font(font::DATA)
                    .size(size::MONO)
                    .style(theme::text_primary)
                    .wrapping(text::Wrapping::None),
                row![
                    Space::new().width(Length::Fixed(lead)),
                    edge_quad(Length::Fixed(run), Length::Fixed(canvas::CARET_H), color::DANGER),
                ],
            ]
            .spacing(space::HAIRLINE),
        );
    }
    container(
        row![
            edge_quad(Length::Fixed(space::BAR_W), Length::Fill, color::DANGER),
            container(body).padding([space::ROW_Y / 2.0, space::CARD]),
        ]
        .height(Length::Shrink),
    )
    .width(Length::Fill)
    .style(|_t: &Theme| container::Style {
        background: Some(iced::Background::Color(color::DANGER_FILL)),
        ..container::Style::default()
    })
    .into()
}

/// A small outlined mono label that says what an object *is* — "Premade".
///
/// A widget and not a [`chip`] because a chip is pickable and this is not: it
/// classifies, it does not select or act. Nearly square ([`radius::BADGE`])
/// and hairline-bordered so it cannot be mistaken for a pill or a chip beside
/// it, and never the accent — a class is not a state.
pub fn badge<'a, Message: 'a>(label: &str) -> Element<'a, Message, Theme> {
    container(
        text(label.to_string())
            .font(font::DATA_MEDIUM)
            .size(size::MICRO)
            .style(theme::text_secondary)
            .wrapping(text::Wrapping::None),
    )
    .padding([space::BADGE_Y, space::BADGE_X])
    .style(|_t: &Theme| container::Style {
        border: iced::Border {
            color: color::BORDER_STRONG,
            width: space::HAIRLINE,
            radius: radius::BADGE.into(),
        },
        ..container::Style::default()
    })
    .into()
}

/// One subject of a status grid: the subject, its state, and a measure.
///
/// A widget because a status grid is a hero shape (COMPOSITION.md) with no
/// primitive until now, and because the three-line cell is the rule — a
/// subject with no state, or a state with no measure, is the half-reading the
/// shape exists to stop. `lit` puts the state in the accent: it is the grid's
/// one yellow, and marks what is currently true, never a heading.
pub fn status_cell<'a, Message: 'a>(
    subject: &str,
    state: &str,
    measure: &str,
    lit: bool,
) -> Element<'a, Message, Theme> {
    inset(
        column![
            text(subject.to_string())
                .font(font::DATA_MEDIUM)
                .size(size::MONO)
                .style(theme::text_primary)
                .wrapping(text::Wrapping::None),
            text(state.to_string())
                .font(font::UI_MEDIUM)
                .size(size::CARD_TITLE)
                .style(if lit {
                    theme::text_accent
                } else {
                    theme::text_secondary
                }),
            text(measure.to_string())
                .font(font::DATA)
                .size(size::MICRO)
                .style(theme::text_tertiary),
        ]
        .spacing(space::LINE_GAP),
    )
    .padding([space::ROW_Y, space::CARD])
    .into()
}

/// Cells in rows of `cols`, every cell the same width.
///
/// A widget so that a short last row keeps the grid's column widths (the gap
/// is filled with empty room, not stretched cells) — the thing a hand-rolled
/// `Row` of `Column`s gets wrong the first time a grid has an odd count.
pub fn status_grid<'a, Message: 'a>(
    cells: Vec<Element<'a, Message, Theme>>,
    cols: usize,
) -> Element<'a, Message, Theme> {
    let cols = cols.max(1);
    let mut grid = Column::new().spacing(space::GRID_GAP);
    let mut cells = cells.into_iter().peekable();
    while cells.peek().is_some() {
        let mut r = Row::new().spacing(space::GRID_GAP);
        for _ in 0..cols {
            r = r.push(match cells.next() {
                Some(c) => container(c).width(Length::FillPortion(1)),
                None => container(Space::new()).width(Length::FillPortion(1)),
            });
        }
        grid = grid.push(r);
    }
    grid.into()
}

/// A note with a hard coloured edge: a headline, and a paragraph under it.
///
/// A widget and not a styled column because the edge is a sibling quad (see
/// [`edge_quad`]) and because the same notes recur: a caution before a
/// command runs as the owner, a notice that part of a pane is switched off.
/// `edge` is [`color::DANGER`] for a caution and [`color::NEUTRAL`] for a
/// notice — never the accent, which marks state and not warnings.
pub fn edge_note<'a, Message: 'a>(headline: &str, body: &str, edge: Color) -> Element<'a, Message, Theme> {
    container(
        row![
            edge_quad(Length::Fixed(space::BAR_W), Length::Fill, edge),
            container(
                column![
                    text(headline.to_string())
                        .font(font::UI_MEDIUM)
                        .size(size::BODY)
                        .style(theme::text_primary),
                    text(body.to_string())
                        .font(font::UI)
                        .size(size::BODY_SMALL)
                        .style(theme::text_secondary),
                ]
                .spacing(space::LINE_GAP),
            )
            .padding([space::ROW_Y, space::CARD]),
        ]
        .height(Length::Shrink),
    )
    .width(Length::Fill)
    .into()
}

/// Where a [`shaped_sheet`] hangs: the edge its pill sits on, which is the
/// bar's edge. The panel grows away from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SheetEdge {
    Top,
    Bottom,
}

/// The outline of a pill with a panel growing out of its left end: the bar
/// with its start menu open.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SheetShape {
    /// The pill's height; the panel is everything past it.
    pub pill_h: f32,
    /// The panel's width, from the sheet's left edge.
    pub panel_w: f32,
    /// The glass radius, clamped per corner to what each box has room for.
    pub radius: f32,
    /// The size of the curve that joins the pill's free edge to the panel's
    /// side, where the two meet in a concave corner.
    pub fillet: f32,
    pub edge: SheetEdge,
}

/// The ground of a bar whose pill has grown a panel: one sheet in the shape
/// of their union, with a curved fillet where the panel leaves the pill.
///
/// A widget and not a styled container because a container is one rounded
/// box, and this ground is not a box: two boxes of glass laid under one
/// another would paint their overlap twice as dark, and the concave corner
/// between them would be a hard right angle where the compositor's mask
/// (abyss: `render/blur.rs`, a smooth union of the input region's boxes)
/// rounds it. So the outline is traced as a single path — the pill's four
/// corners, the fillet as a quadratic curve (which lands on the mask's
/// smooth-min contour for two perpendicular edges), the panel's two far
/// corners — and filled once, in the bar's own tint. With blur off it is the
/// darker fallback ground with the hairline traced along the same outline.
pub fn shaped_sheet<'a, Message: 'a>(shape: SheetShape, blur: bool) -> Element<'a, Message, Theme> {
    iced::widget::canvas(ShapedSheet { shape, blur })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

struct ShapedSheet {
    shape: SheetShape,
    blur: bool,
}

impl<Message> iced::widget::canvas::Program<Message> for ShapedSheet {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: iced::Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<iced::widget::canvas::Geometry> {
        use iced::widget::canvas;
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let path = sheet_outline(self.shape, bounds.width, bounds.height);
        let fill = if self.blur {
            color::BAR_TINT
        } else {
            color::GLASS_DEEP_BACKED
        };
        frame.fill(&path, fill);
        if !self.blur {
            frame.stroke(
                &path,
                canvas::Stroke::default()
                    .with_color(color::HAIRLINE)
                    .with_width(space::HAIRLINE),
            );
        }
        vec![frame.into_geometry()]
    }
}

/// [`shaped_sheet`]'s outline in a `w`×`h` box. With no room below the pill
/// it is the pill alone.
pub fn sheet_outline(shape: SheetShape, w: f32, h: f32) -> iced::widget::canvas::Path {
    use iced::Point;
    let SheetShape {
        pill_h,
        panel_w,
        radius,
        fillet,
        edge,
    } = shape;
    let pill_h = pill_h.min(h);
    let panel_w = panel_w.min(w);
    // Mirrored for a bottom bar: the pill sits on the bottom edge and the
    // panel grows up from it. `arc_to` is tangent-based, so the same corner
    // sequence traces either way up.
    let at = |x: f32, y: f32| match edge {
        SheetEdge::Top => Point::new(x, y),
        SheetEdge::Bottom => Point::new(x, h - y),
    };
    let rp = radius.min(pill_h / 2.0).min(w / 2.0).max(0.0);
    let drop = h - pill_h;
    iced::widget::canvas::Path::new(|b| {
        if drop <= 0.0 {
            b.move_to(at(rp, 0.0));
            b.line_to(at(w - rp, 0.0));
            b.arc_to(at(w, 0.0), at(w, rp), rp);
            b.line_to(at(w, h - rp));
            b.arc_to(at(w, h), at(w - rp, h), rp);
            b.line_to(at(rp, h));
            b.arc_to(at(0.0, h), at(0.0, h - rp), rp);
            b.line_to(at(0.0, rp));
            b.arc_to(at(0.0, 0.0), at(rp, 0.0), rp);
            b.close();
            return;
        }
        // The panel's far corners have only the drop below the pill to turn
        // in while it is still opening; the fillet takes what they leave.
        let rb = radius.min(panel_w / 2.0).min(drop).max(0.0);
        let kf = fillet.min(drop - rb).min(w - panel_w - rp).max(0.0);
        // The panel is the one box at the top-left corner, so its radius —
        // clamped to its own size, as the mask clamps it — is the corner's.
        let top_left = radius.min(panel_w / 2.0).min(h / 2.0).max(0.0);
        b.move_to(at(top_left, 0.0));
        b.line_to(at(w - rp, 0.0));
        b.arc_to(at(w, 0.0), at(w, rp), rp);
        b.line_to(at(w, pill_h - rp));
        b.arc_to(at(w, pill_h), at(w - rp, pill_h), rp);
        b.line_to(at(panel_w + kf, pill_h));
        if kf > 0.0 {
            b.quadratic_curve_to(at(panel_w, pill_h), at(panel_w, pill_h + kf));
        }
        b.line_to(at(panel_w, h - rb));
        b.arc_to(at(panel_w, h), at(panel_w - rb, h), rb);
        b.line_to(at(rb, h));
        b.arc_to(at(0.0, h), at(0.0, h - rb), rb);
        b.line_to(at(0.0, top_left));
        b.arc_to(at(0.0, 0.0), at(top_left, 0.0), top_left);
        b.close();
    })
}
