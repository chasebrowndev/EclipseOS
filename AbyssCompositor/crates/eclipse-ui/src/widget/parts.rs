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
use crate::tokens::{color, drawer, font, menu, radius, size, space};

/// A glass panel. The one container every block on a pane sits in.
pub fn panel<'a, Message: 'a>(
    content: impl Into<Element<'a, Message, Theme>>,
) -> container::Container<'a, Message, Theme> {
    container(content)
        .padding(space::CARD)
        .style(theme::panel)
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
    .spacing(6);

    let mut right = Row::new().spacing(8).align_y(Alignment::Center);
    for control in controls {
        right = right.push(control);
    }

    row![left, Space::new().width(Length::Fill), right]
        .align_y(Alignment::Center)
        .into()
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
    heading: &str,
    rows: Vec<Element<'a, Message, Theme>>,
) -> Element<'a, Message, Theme> {
    let mut col = Column::new().push(
        container(micro_label(heading))
            .height(Length::Fixed(drawer::HEAD_H))
            .align_y(Alignment::Center)
            .padding([0.0, drawer::ROW_X]),
    );
    for r in rows {
        col = col.push(r);
    }
    lit(
        container(col)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(drawer::PAD)
            .style(theme::menu_surface),
        radius::CARD,
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
    svg(svg::Handle::from_path(path))
        .width(Length::Fixed(side))
        .height(Length::Fixed(side))
        .style(move |_t: &Theme, _s: svg::Status| svg::Style { color: Some(tint) })
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
/// A widget because the sheet is three things at once — the smoked ground of
/// [`crate::theme::surface`], its border, and the [`lit`] top edge — and a
/// caller that assembled two of the three would get the flat grey slab the
/// style spec exists to prevent.
pub fn surface<'a, Message: 'a>(
    content: impl Into<Element<'a, Message, Theme>>,
) -> Element<'a, Message, Theme> {
    lit(
        container(content)
            .padding(space::CARD)
            .width(Length::Fill)
            .style(crate::theme::surface),
        radius::CARD,
        color::HIGHLIGHT,
    )
}

/// A battery gauge: a hard-edged cell with a fill proportional to charge, and
/// a nub on its right the way every battery pictogram has had since 1995.
///
/// A widget because the fill is a *magnitude* and magnitudes are this design
/// system's job — a caller that drew it inline would be reinventing
/// [`big_value`] at 10 pixels.
pub fn battery_gauge<'a, Message: 'a>(percent: u8, fill: Color) -> Element<'a, Message, Theme> {
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
        .style(|_t: &Theme| container::Style {
            border: iced::Border {
                color: color::TRACK,
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
            color::TRACK,
        ),
    ]
    .align_y(Alignment::Center)
    .into()
}

/// A list row: label at the left, mono value at the right.
pub fn list_row<'a, Message: 'a>(
    label: &str,
    value: impl Into<Element<'a, Message, Theme>>,
) -> Element<'a, Message, Theme> {
    container(
        row![
            text(label.to_string())
                .font(font::UI)
                .size(size::BODY)
                .style(theme::text_secondary),
            Space::new().width(Length::Fill),
            value.into(),
        ]
        .align_y(Alignment::Center),
    )
    .padding([space::ROW_Y, space::CARD])
    .width(Length::Fill)
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
    .padding([6, 14])
    .on_press(on_press)
    .style(theme::pill(selected))
    .into()
}

/// A segmented choice: pills in a row, one of them accented.
pub fn segmented<'a, T, Message>(
    options: &'a [(T, &'a str)],
    current: &T,
    on_select: impl Fn(&T) -> Message + 'a,
) -> Element<'a, Message, Theme>
where
    T: PartialEq,
    Message: Clone + 'a,
{
    let mut r = Row::new().spacing(6);
    for (v, label) in options {
        r = r.push(pill(label, v == current, on_select(v)));
    }
    r.into()
}

/// A sidebar nav item. The 3px accent bar is a sibling quad, because a
/// container border cannot be applied to one edge only.
pub fn nav_item<'a, Message: Clone + 'a>(
    label: &'a str,
    active: bool,
    on_press: Message,
) -> Element<'a, Message, Theme> {
    let bar = container(Space::new())
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
        });

    // The placeholder icon square the spec calls for: a small rounded rect,
    // not an icon. Icons are a later problem and a worse one.
    let glyph = container(Space::new())
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
                width: 1.0,
                radius: 4.5.into(),
            },
            ..container::Style::default()
        });

    row![
        bar,
        button(
            row![
                glyph,
                text(label)
                    .font(if active { font::UI_MEDIUM } else { font::UI })
                    .size(size::BODY),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        )
        .padding([7, 10])
        .width(Length::Fill)
        .on_press(on_press)
        .style(theme::nav(active)),
    ]
    .spacing(5)
    .align_y(Alignment::Center)
    .into()
}

/// The 214px left column: nav items above, a mono status block at the foot.
pub fn sidebar<'a, Message: 'a>(
    items: Vec<Element<'a, Message, Theme>>,
    footer: Vec<(&'a str, String)>,
) -> Element<'a, Message, Theme> {
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

    container(
        column![nav, Space::new().height(Length::Fill), foot]
            .spacing(space::BLOCK)
            .width(Length::Fill),
    )
    .width(space::SIDEBAR_W)
    .height(Length::Fill)
    .padding([space::PANE_Y, 12.0])
    .style(theme::sidebar)
    .into()
}

/// The content column to the right of the sidebar: 26/30 padding, 18 between
/// blocks.
pub fn content<'a, Message: 'a>(blocks: Vec<Element<'a, Message, Theme>>) -> Element<'a, Message, Theme> {
    let mut col = Column::new().spacing(space::BLOCK);
    for b in blocks {
        col = col.push(b);
    }
    container(col)
        .padding([space::PANE_Y, space::PANE_X])
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
pub fn status_chip<'a, Message: 'a>(state: &str, measure: &str) -> Element<'a, Message, Theme> {
    container(
        column![
            text(state.to_string())
                .font(font::DATA_MEDIUM)
                .size(size::MICRO)
                .style(theme::text_secondary),
            text(measure.to_string())
                .font(font::DATA)
                .size(size::MICRO)
                .style(theme::text_tertiary),
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
