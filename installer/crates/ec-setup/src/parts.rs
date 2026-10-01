// SPDX-License-Identifier: AGPL-3.0-only
//! Pieces the wizard needs that `eclipse_ui::widget::parts` does not have yet.
//!
//! Each of these is here rather than in the shared crate because this crate may
//! only touch itself; each is a widget and not a styled container for the
//! reason in its doc comment, and each is a candidate to move to
//! `eclipse-ui/src/widget/parts.rs` as-is. Nothing here inlines a colour or a
//! size: it is all `tokens` and `metrics`.

use crate::metrics;
use eclipse_ui::theme;
use eclipse_ui::tokens::{color, font, radius, size, space};
use eclipse_ui::widget::{edge_quad, micro_label, quad};
use iced::widget::{button, column, container, row, text, text_input, Column, Row, Space};
use iced::{Alignment, Color, Element, Length, Theme};

pub type El<'a, M> = Element<'a, M, Theme>;

// ------------------------------------------------------------------ text

/// A line of body copy in the secondary ink.
pub fn body<'a, M: 'a>(s: impl Into<String>) -> El<'a, M> {
    text(s.into())
        .font(font::UI)
        .size(size::BODY)
        .style(theme::text_secondary)
        .into()
}

/// Primary-ink body text.
pub fn strong<'a, M: 'a>(s: impl Into<String>) -> El<'a, M> {
    text(s.into())
        .font(font::UI_MEDIUM)
        .size(size::BODY)
        .style(theme::text_primary)
        .into()
}

/// A mono reading in the given ink.
pub fn mono<'a, M: 'a>(s: impl Into<String>, ink: Color) -> El<'a, M> {
    text(s.into()).font(font::DATA).size(size::MONO).color(ink).into()
}

/// A pane subtitle in plain secondary ink.
///
/// `eclipse_ui::widget::subtitle` spends the accent on its middle clause; on a
/// step whose one yellow is elsewhere, the subtitle has to be quiet.
pub fn plain_subtitle<'a, M: 'a>(s: &str) -> El<'a, M> {
    body(s.to_owned())
}

/// A sentence that says what is wrong, in the danger ink.
pub fn problem<'a, M: 'a>(s: &str) -> El<'a, M> {
    text(s.to_owned())
        .font(font::UI)
        .size(size::BODY_SMALL)
        .style(theme::text_danger)
        .into()
}

// ---------------------------------------------------------------- banners

/// What a banner says about the thing it states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// True and live: the pane's one yellow.
    Accent,
    /// Destructive or blocked: the status red, never the accent.
    Danger,
    /// Information. No colour.
    Quiet,
}

impl Tone {
    fn rule(self) -> Color {
        match self {
            Tone::Accent => color::ACCENT,
            Tone::Danger => color::DANGER,
            Tone::Quiet => color::TEXT_TERTIARY,
        }
    }

    fn ground(self) -> Color {
        match self {
            Tone::Accent => color::ACCENT_FILL,
            Tone::Danger => color::DANGER_FILL,
            Tone::Quiet => color::GLASS,
        }
    }

    fn edge(self) -> Color {
        match self {
            Tone::Accent => color::ACCENT_BORDER,
            Tone::Danger => color::DANGER.scale_alpha(0.32),
            Tone::Quiet => color::BORDER,
        }
    }

    fn title_ink(self) -> Color {
        match self {
            Tone::Accent => color::ACCENT_TEXT,
            Tone::Danger => color::DANGER,
            Tone::Quiet => color::TEXT,
        }
    }
}

/// The band: a rule at the left in the tone's colour, a micro label, a title at
/// prompt scale, one sentence, and up to two mono readings at the right.
///
/// A widget because it is the *schedule band* hero of COMPOSITION.md, and the
/// catalogue says a hero is a shape with its own primitive, not a container
/// somebody styled twice. It is the same shape the notifications pane's focus
/// banner is; only the tone differs.
pub fn banner<'a, M: 'a>(
    tone: Tone,
    label: &str,
    title: &str,
    sentence: &str,
    readings: Vec<(String, String)>,
) -> El<'a, M> {
    let left = column![
        micro_label(label),
        text(title.to_owned())
            .font(font::UI_SEMIBOLD)
            .size(size::PROMPT)
            .color(tone.title_ink()),
        text(sentence.to_owned())
            .font(font::UI)
            .size(size::BODY)
            .style(theme::text_secondary),
    ]
    .spacing(space::CHIP_GAP)
    .width(Length::Fill);

    let mut right = Column::new().spacing(space::CHIP_GAP).align_x(Alignment::End);
    for (state, measure) in readings {
        right = right.push(
            column![
                text(state)
                    .font(font::DATA_MEDIUM)
                    .size(size::MICRO)
                    .style(theme::text_secondary)
                    .wrapping(text::Wrapping::None),
                text(measure)
                    .font(font::DATA)
                    .size(size::MICRO)
                    .style(theme::text_tertiary)
                    .wrapping(text::Wrapping::None),
            ]
            .align_x(Alignment::End),
        );
    }

    // The rule is a second layer over the body, not a sibling of it: a
    // full-height child inside a row would make the row as tall as the pane.
    let body = container(row![left, right].spacing(space::CARD).align_y(Alignment::Center))
        .padding(iced::Padding {
            left: space::CARD + metrics::BANNER_RULE_W,
            ..iced::Padding::from([space::CARD, space::CARD])
        })
        .width(Length::Fill);
    let rule = row![edge_quad(
        Length::Fixed(metrics::BANNER_RULE_W),
        Length::Fill,
        tone.rule()
    )]
    .width(Length::Fill)
    .height(Length::Fill);
    container(iced::widget::stack![body, rule])
        .width(Length::Fill)
        .style(move |_t: &Theme| container::Style {
            background: Some(iced::Background::Color(tone.ground())),
            border: iced::Border {
                color: tone.edge(),
                width: metrics::BANNER_BORDER,
                radius: radius::CARD.into(),
            },
            ..container::Style::default()
        })
        .into()
}

// ------------------------------------------------------------ pick + chips

/// A row of a list that can be the current choice.
///
/// `eclipse_ui::widget::choice_row` always draws its bar and its wash in the
/// accent, which is right for a pane's one live selection and wrong for every
/// other list on the same pane, where a second yellow would break the ledger.
/// This is that row with the tone as a parameter and the click target built
/// in: `accent` picks the yellow one, otherwise the selection reads as a white
/// bar over a lifted ground.
pub fn pick<'a, M: Clone + 'a>(
    content: impl Into<El<'a, M>>,
    selected: bool,
    accent: bool,
    on_press: Option<M>,
) -> El<'a, M> {
    let bar = match (selected, accent) {
        (false, _) => Color::TRANSPARENT,
        (true, true) => color::ACCENT,
        (true, false) => color::TEXT_SECONDARY,
    };
    let face = row![
        edge_quad(Length::Fixed(space::BAR_W), Length::Fill, bar),
        container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_y(Alignment::Center)
            .padding([0.0, space::CARD]),
    ];
    let b = button(face)
        .width(Length::Fill)
        .height(Length::Fixed(metrics::ROW_H))
        .padding(0)
        .style(move |_t: &Theme, status: button::Status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            let ground = match (selected, accent, hovered) {
                (true, true, _) => color::ACCENT_FILL,
                (true, false, _) => color::LIFT,
                (false, _, true) => color::LIFT_SOFT,
                (false, _, false) => Color::TRANSPARENT,
            };
            button::Style {
                background: Some(iced::Background::Color(ground)),
                text_color: color::TEXT,
                border: iced::border::rounded(radius::CHIP),
                ..button::Style::default()
            }
        });
    match on_press {
        Some(m) => b.on_press(m).into(),
        None => b.into(),
    }
}

/// A canvas chip whose selection is a white outline, not the accent.
///
/// `eclipse_ui::widget::chip` marks its selection in yellow, which a step with
/// a live value elsewhere cannot afford. Same squarer-than-a-pill silhouette,
/// same mono face, so it still reads as an object and not a verb.
pub fn tab<'a, M: Clone + 'a>(label: &str, selected: bool, on_press: M) -> El<'a, M> {
    button(text(label.to_owned()).font(font::DATA_MEDIUM).size(size::MONO))
        .padding([space::CHIP_Y, space::CHIP_X])
        .on_press(on_press)
        .style(move |_t: &Theme, status: button::Status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: Some(iced::Background::Color(match (selected, hovered) {
                    (true, _) => color::LIFT_STRONG,
                    (false, true) => color::LIFT,
                    (false, false) => color::LIFT_SOFT,
                })),
                text_color: if selected {
                    color::TEXT
                } else {
                    color::TEXT_SECONDARY
                },
                border: iced::Border {
                    color: if selected {
                        color::HIGHLIGHT_STRONG
                    } else {
                        color::BORDER
                    },
                    width: space::HAIRLINE,
                    radius: radius::CHIP.into(),
                },
                ..button::Style::default()
            }
        })
        .into()
}

// ---------------------------------------------------------------- buttons

fn pill_style(
    enabled_ground: Color,
    enabled_ink: Color,
    hover_ground: Color,
) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_t, status| match status {
        button::Status::Disabled => button::Style {
            background: Some(iced::Background::Color(color::LIFT_SOFT)),
            text_color: color::TEXT_TERTIARY,
            border: iced::Border {
                color: color::BORDER,
                width: space::HAIRLINE,
                radius: radius::PILL.into(),
            },
            ..button::Style::default()
        },
        s => button::Style {
            background: Some(iced::Background::Color(
                if matches!(s, button::Status::Hovered | button::Status::Pressed) {
                    hover_ground
                } else {
                    enabled_ground
                },
            )),
            text_color: enabled_ink,
            border: iced::Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: radius::PILL.into(),
            },
            ..button::Style::default()
        },
    }
}

fn action<'a, M: Clone + 'a>(
    label: &str,
    on_press: Option<M>,
    style: impl Fn(&Theme, button::Status) -> button::Style + 'a,
) -> El<'a, M> {
    let b = button(text(label.to_owned()).font(font::UI_SEMIBOLD).size(size::BODY))
        .padding([metrics::BUTTON_Y, metrics::BUTTON_X])
        .style(style);
    match on_press {
        Some(m) => b.on_press(m).into(),
        None => b.into(),
    }
}

/// The step's forward button: white on the dark ground, hard contrast, and no
/// yellow, so it never competes with the step's one live value.
pub fn primary<'a, M: Clone + 'a>(label: &str, on_press: Option<M>) -> El<'a, M> {
    action(
        label,
        on_press,
        pill_style(color::TEXT, color::BASE, color::TEXT.scale_alpha(0.86)),
    )
}

/// The commit: solid gold. It is the pane's one yellow on the two steps that
/// end in an irreversible act (Apply, Restart), and it is only ever enabled
/// when that act is allowed.
pub fn commit<'a, M: Clone + 'a>(label: &str, on_press: Option<M>) -> El<'a, M> {
    action(
        label,
        on_press,
        pill_style(color::ACCENT, color::BASE, color::ACCENT_TEXT),
    )
}

/// The way back: an outlined pill. `eclipse_ui::widget::pill` unselected.
pub fn ghost<'a, M: Clone + 'a>(label: &str, on_press: Option<M>) -> El<'a, M> {
    let b = button(text(label.to_owned()).font(font::UI_MEDIUM).size(size::BODY))
        .padding([metrics::BUTTON_Y, metrics::BUTTON_X])
        .style(theme::pill(false));
    match on_press {
        Some(m) => b.on_press(m).into(),
        None => b.into(),
    }
}

/// A small verb beside a list heading (`rescan`, `reload`).
pub fn verb<'a, M: Clone + 'a>(label: &str, on_press: M) -> El<'a, M> {
    eclipse_ui::widget::pill(label, false, on_press)
}

// ----------------------------------------------------------------- fields

/// A single-line field. `secure` masks it; `invalid` puts the danger border on
/// it. Enter is routed to `on_submit`, so the wizard's keys behave the same in
/// a field as out of one.
pub fn input<'a, M: Clone + 'a>(
    id: &'static str,
    placeholder: &str,
    value: &str,
    secure: bool,
    invalid: bool,
    on_input: impl Fn(String) -> M + 'a,
    on_submit: M,
) -> El<'a, M> {
    text_input(placeholder, value)
        .id(id)
        .on_input(on_input)
        .on_submit(on_submit)
        .secure(secure)
        .font(font::DATA)
        .size(size::BODY)
        .padding(metrics::INPUT_PAD)
        .style(if invalid {
            theme::eclipse_input_invalid
        } else {
            theme::eclipse_input
        })
        .into()
}

/// A field with its name above it, in plain words.
pub fn labeled<'a, M: 'a>(label: &str, field: El<'a, M>) -> El<'a, M> {
    column![strong(label.to_owned()), field]
        .spacing(space::CHIP_GAP)
        .into()
}

// ------------------------------------------------------------------ marks

/// The signal mark: four bars of rising height, lit up to `strength`.
///
/// A widget because a signal is a magnitude, and a magnitude drawn as
/// "82%" in a list row is the coverage smell COMPOSITION.md names.
pub fn signal_bars<'a, M: 'a>(strength: u8) -> El<'a, M> {
    let lit = ((f32::from(strength.min(100)) / 100.0) * metrics::SIGNAL_HEIGHTS.len() as f32).ceil() as usize;
    let tallest = metrics::SIGNAL_HEIGHTS[metrics::SIGNAL_HEIGHTS.len() - 1];
    let mut r = Row::new()
        .spacing(metrics::SIGNAL_BAR_GAP)
        .align_y(Alignment::End);
    for (i, h) in metrics::SIGNAL_HEIGHTS.iter().enumerate() {
        r = r.push(quad(
            Length::Fixed(metrics::SIGNAL_BAR_W),
            Length::Fixed(*h),
            if i < lit {
                color::TEXT_SECONDARY
            } else {
                color::TRACK
            },
            0.0,
        ));
    }
    container(r)
        .height(Length::Fixed(tallest))
        .align_y(Alignment::End)
        .into()
}

/// Which ink a disk bar is drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ink {
    Neutral,
    /// What is about to be erased.
    Erase,
}

/// A disk as a proportional bar: one segment per partition, sized by bytes,
/// then the unpartitioned rest as an empty track.
///
/// A widget because it is the *spatial* half of the disk hero: a table of
/// sizes says what is on the disk, and this says how much of it and where.
pub fn disk_bar<'a, M: 'a>(sizes: &[u64], total: u64, ink: Ink) -> El<'a, M> {
    let total = total.max(1);
    let base = match ink {
        Ink::Neutral => color::TEXT,
        Ink::Erase => color::DANGER,
    };
    let shades = [0.70_f32, 0.46, 0.60, 0.34];
    let portion = |bytes: u64| -> u16 {
        let p = (bytes as f64 / total as f64 * f64::from(metrics::DISK_PORTIONS)).round();
        (p as u16).max(metrics::DISK_MIN_PORTION)
    };

    let mut r = Row::new().spacing(metrics::DISK_BAR_GAP);
    let mut used = 0u64;
    for (i, bytes) in sizes.iter().enumerate() {
        used = used.saturating_add(*bytes);
        r = r.push(quad(
            Length::FillPortion(portion(*bytes)),
            Length::Fixed(metrics::DISK_BAR_H),
            base.scale_alpha(shades[i % shades.len()]),
            radius::BAR,
        ));
    }
    let free = total.saturating_sub(used);
    // A gap under half a percent is rounding, not free space.
    if free as f64 / total as f64 > 0.005 || sizes.is_empty() {
        r = r.push(quad(
            Length::FillPortion(portion(free).max(if sizes.is_empty() {
                metrics::DISK_PORTIONS as u16
            } else {
                1
            })),
            Length::Fixed(metrics::DISK_BAR_H),
            color::TRACK,
            radius::BAR,
        ));
    }
    container(r)
        .width(Length::Fill)
        .height(Length::Fixed(metrics::DISK_BAR_H))
        .into()
}

// -------------------------------------------------------------- readouts

/// Label over value, for a summary grid cell.
pub fn stat<'a, M: 'a>(label: &str, value: &str) -> El<'a, M> {
    column![
        micro_label(label),
        text(value.to_owned())
            .font(font::UI_MEDIUM)
            .size(size::BODY)
            .style(theme::text_primary),
    ]
    .spacing(space::CHIP_GAP)
    .width(Length::Fill)
    .into()
}

/// A grid of [`stat`]s, `columns` wide, on the inset ground.
pub fn stat_grid<'a, M: 'a>(cells: Vec<(&str, String)>, columns: usize) -> El<'a, M> {
    let columns = columns.max(1);
    let mut rows = Column::new().spacing(space::CARD);
    let mut it = cells.into_iter().peekable();
    while it.peek().is_some() {
        let mut r = Row::new().spacing(space::CARD);
        let mut n = 0;
        for (label, value) in it.by_ref().take(columns) {
            r = r.push(stat(label, &value));
            n += 1;
        }
        while n < columns {
            r = r.push(Space::new().width(Length::Fill));
            n += 1;
        }
        rows = rows.push(r);
    }
    eclipse_ui::widget::inset(rows).padding(space::CARD).into()
}

// --------------------------------------------------------------- progress

/// Where you are in the flow: one short bar per step, the current one longer
/// and white, the ones behind you grey, and "3 of 8" beside them.
///
/// A widget because the wizard has no sidebar to say where you are, and eight
/// squares in a row are a magnitude (how far along), which `eclipse_ui` has no
/// primitive for. It is white, never yellow: the yellow belongs to the step's
/// own value.
pub fn progress<'a, M: 'a>(here: usize, total: usize) -> El<'a, M> {
    let mut bars = Row::new()
        .spacing(metrics::PROGRESS_GAP)
        .align_y(Alignment::Center);
    for n in 1..=total {
        let (w, ink) = match n.cmp(&here) {
            std::cmp::Ordering::Less => (metrics::PROGRESS_W, color::TEXT_TERTIARY),
            std::cmp::Ordering::Equal => (metrics::PROGRESS_W * 2.0, color::TEXT),
            std::cmp::Ordering::Greater => (metrics::PROGRESS_W, color::TRACK),
        };
        bars = bars.push(quad(
            Length::Fixed(w),
            Length::Fixed(metrics::PROGRESS_H),
            ink,
            0.0,
        ));
    }
    row![
        bars,
        Space::new().width(Length::Fill),
        text(format!("{here} of {total}"))
            .font(font::DATA)
            .size(size::MICRO)
            .style(theme::text_tertiary),
    ]
    .align_y(Alignment::Center)
    .into()
}

/// A quiet text button: "More options", "Connect to Wi-Fi".
///
/// A widget and not a `ghost` because a disclosure is not a verb of the step;
/// it must read as smaller than Back and Continue so nobody takes it for the
/// next thing to press.
pub fn link<'a, M: Clone + 'a>(label: &str, on_press: M) -> El<'a, M> {
    button(text(label.to_owned()).font(font::UI_MEDIUM).size(size::BODY))
        .padding([space::CHIP_Y, 0.0])
        .on_press(on_press)
        .style(|_t: &Theme, status: button::Status| button::Style {
            background: None,
            text_color: if matches!(status, button::Status::Hovered | button::Status::Pressed) {
                color::TEXT
            } else {
                color::TEXT_SECONDARY
            },
            ..button::Style::default()
        })
        .into()
}

/// The step's big plain question.
pub fn title<'a, M: 'a>(s: &str) -> El<'a, M> {
    text(s.to_owned())
        .font(font::UI_SEMIBOLD)
        .size(size::BIG_NUMBER)
        .style(theme::text_primary)
        .into()
}

/// The one plain sentence under it.
pub fn lead<'a, M: 'a>(s: &str) -> El<'a, M> {
    text(s.to_owned())
        .font(font::UI)
        .size(size::PROMPT)
        .style(theme::text_secondary)
        .into()
}

// ---------------------------------------------------------- cards and tags

/// A large selectable card: a disk, a profile.
///
/// A widget because a card is a click target with a ground that has to answer
/// the pointer, and `eclipse_ui::widget::panel` is a static container. `selected`
/// is the pane's one yellow (border and wash); `on_press: None` is a card that
/// is shown and refused, drawn flat with no hover.
pub fn card<'a, M: Clone + 'a>(
    content: impl Into<El<'a, M>>,
    height: Length,
    selected: bool,
    on_press: Option<M>,
) -> El<'a, M> {
    let enabled = on_press.is_some();
    let b = button(content)
        .width(Length::Fill)
        .height(height)
        .padding(space::CARD)
        .style(move |_t: &Theme, status: button::Status| {
            let hovered = enabled && matches!(status, button::Status::Hovered | button::Status::Pressed);
            let (ground, edge) = match (selected, hovered) {
                (true, _) => (color::ACCENT_FILL, color::ACCENT_BORDER),
                (false, true) => (color::LIFT_SOFT, color::BORDER_STRONG),
                (false, false) => (color::GLASS, color::BORDER),
            };
            button::Style {
                background: Some(iced::Background::Color(ground)),
                text_color: color::TEXT,
                border: iced::Border {
                    color: edge,
                    width: space::HAIRLINE,
                    radius: radius::CARD.into(),
                },
                ..button::Style::default()
            }
        });
    match on_press {
        Some(m) => b.on_press(m).into(),
        None => b.into(),
    }
}

/// A small outlined word: `coming`, `preview`, `not yet`. Inert, mono, quiet.
pub fn tag<'a, M: 'a>(label: &str) -> El<'a, M> {
    container(
        text(label.to_owned())
            .font(font::DATA_MEDIUM)
            .size(size::MICRO)
            .style(theme::text_tertiary),
    )
    .padding([metrics::CARD_TAG_Y, metrics::CARD_TAG_X])
    .style(|_t: &Theme| container::Style {
        border: iced::Border {
            color: color::BORDER,
            width: space::HAIRLINE,
            radius: radius::CHIP.into(),
        },
        ..container::Style::default()
    })
    .into()
}

// ---------------------------------------------------------------- diagrams

/// A tile that is `w` wide and `h` tall out of its neighbours' portions.
fn tile<'a, M: 'a>(w: u16, h: u16) -> El<'a, M> {
    quad(
        Length::FillPortion(w),
        Length::FillPortion(h),
        color::TEXT_TERTIARY,
        radius::BAR,
    )
}

/// Tiles stacked top to bottom in a column `w` portions wide.
fn stack<'a, M: 'a>(w: u16, tiles: Vec<El<'a, M>>) -> El<'a, M> {
    Column::with_children(tiles)
        .spacing(metrics::GLYPH_GAP)
        .width(Length::FillPortion(w))
        .height(Length::Fill)
        .into()
}

/// Tiles side by side in a row `h` portions tall.
fn side<'a, M: 'a>(h: u16, tiles: Vec<El<'a, M>>) -> El<'a, M> {
    Row::with_children(tiles)
        .spacing(metrics::GLYPH_GAP)
        .width(Length::Fill)
        .height(Length::FillPortion(h))
        .into()
}

/// A small picture of how a layout divides a screen.
///
/// A widget because the three layouts differ only in geometry, and the words
/// for it ("weighted tree", "dwindle") mean nothing to someone choosing: the
/// picture is the explanation. A spatial canvas in COMPOSITION.md's terms.
pub fn tiles<'a, M: 'a>(layout: crate::choices::Tiling) -> El<'a, M> {
    use crate::choices::Tiling;
    let inner: El<'a, M> = match layout {
        // Uneven weights: the big window has more of the screen than its neighbours.
        Tiling::Radiant => Row::with_children(vec![stack(3, vec![tile(1, 2), tile(1, 1)]), tile(2, 1)])
            .spacing(metrics::GLYPH_GAP)
            .into(),
        // Each window splits the last one, alternating across and down.
        Tiling::Dwindle => Row::with_children(vec![
            tile(1, 1),
            stack(
                1,
                vec![
                    tile(1, 1),
                    side(1, vec![tile(1, 1), stack(1, vec![tile(1, 1), tile(1, 1)])]),
                ],
            ),
        ])
        .spacing(metrics::GLYPH_GAP)
        .into(),
        // One main window, the rest in a stack beside it.
        Tiling::Master => Row::with_children(vec![
            tile(3, 1),
            stack(2, vec![tile(1, 1), tile(1, 1), tile(1, 1)]),
        ])
        .spacing(metrics::GLYPH_GAP)
        .into(),
    };
    container(inner)
        .width(Length::Fill)
        .height(Length::Fixed(metrics::GLYPH_H))
        .into()
}

/// A small desktop: the bar's strip (the accent, because the bar is the thing
/// being placed) and two tiled windows, square or rounded.
///
/// A widget because the appearance step's hero is a picture of the result, and
/// `eclipse_ui` has no primitive for a stylised screen.
pub fn desk<'a, M: 'a>(rounded: bool, bottom: bool) -> El<'a, M> {
    let corner = if rounded { radius::CARD } else { 0.0 };
    let bar: El<'a, M> = quad(
        Length::Fill,
        Length::Fixed(metrics::DESK_BAR_H),
        color::ACCENT,
        radius::BAR,
    );
    let windows: El<'a, M> = Row::with_children(vec![
        quad(Length::FillPortion(3), Length::Fill, color::TRACK, corner),
        quad(Length::FillPortion(2), Length::Fill, color::TRACK, corner),
    ])
    .spacing(space::CHIP_GAP)
    .height(Length::Fill)
    .into();
    let mut col = Column::new().spacing(space::CHIP_GAP);
    col = if bottom {
        col.push(windows).push(bar)
    } else {
        col.push(bar).push(windows)
    };
    eclipse_ui::widget::inset(col.height(Length::Fill))
        .padding(space::CHIP_GAP)
        .height(Length::Fixed(metrics::DESK_H))
        .into()
}

/// A level bar that takes the width it is given, `fraction` lit.
///
/// `eclipse_ui::widget::meter_bar` is fixed-width for a drawer row; a hero
/// meter runs the width of the column, and the two cannot share a signature.
pub fn meter_fill<'a, M: 'a>(fraction: f32, fill: Color) -> El<'a, M> {
    let lit = (fraction.clamp(0.0, 1.0) * 100.0).round() as u16;
    let mut r = Row::new();
    if lit > 0 {
        r = r.push(quad(
            Length::FillPortion(lit),
            Length::Fixed(metrics::METER_H),
            fill,
            radius::BAR,
        ));
    }
    if lit < 100 {
        r = r.push(quad(
            Length::FillPortion(100 - lit),
            Length::Fixed(metrics::METER_H),
            color::TRACK,
            radius::BAR,
        ));
    }
    container(r)
        .width(Length::Fill)
        .height(Length::Fixed(metrics::METER_H))
        .into()
}
