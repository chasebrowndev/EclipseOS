// SPDX-License-Identifier: AGPL-3.0-only
//! The Eclipse theme, and style functions for iced's stock widgets.
//!
//! iced's own widgets are kept wherever they can be styled into the spec —
//! a hand-written text input would be a worse text input. Only the controls
//! the spec describes geometrically (the 44×25 toggle, the bar chart) are
//! written from scratch, in [`crate::widget`].

use iced::{
    border,
    widget::{button, container, radio, rule, scrollable, slider, text, text_input},
    Background, Border, Color, Shadow, Theme, Vector,
};

use crate::tokens::{bar, color, radius, size, space};

/// The theme every Eclipse binary runs.
///
/// The palette is the fallback for anything not explicitly styled; the style
/// functions below are what actually produce the look. `background` is the
/// warm base so that an unstyled surface is at worst plain, never blue-grey.
pub fn theme() -> Theme {
    Theme::custom(
        "Eclipse",
        iced::theme::Palette {
            background: color::BASE,
            text: color::TEXT,
            primary: color::ACCENT,
            success: color::OK,
            warning: color::ACCENT,
            danger: color::DANGER,
        },
    )
}

/// A panel: the spec's glass — a thin white fill, a hairline border and a
/// soft outer shadow.
///
/// The fill is deliberately thin (`.045`, inside the spec's `.035–.06`). On a
/// settings pane it composites over the warm base; on a layer surface it
/// composites over the compositor's blur of whatever is behind. Depth is the
/// border and the top edge highlight ([`crate::widget::lit`]) doing their
/// job — never fill opacity, which is what turns glass back into grey paint.
pub fn panel(_t: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(color::GLASS)),
        border: Border {
            color: color::BORDER,
            width: space::HAIRLINE,
            radius: radius::CARD.into(),
        },
        shadow: Shadow {
            color: Color {
                a: 0.88,
                ..Color::BLACK
            },
            offset: Vector::new(0.0, 24.0),
            blur_radius: 70.0,
        },
        ..container::Style::default()
    }
}

/// A surface that floats over live wallpaper: a toast card, the launcher
/// sheet, the control centre. Distinct from [`panel`] because it has no
/// window ground underneath it — only the compositor's blur — so it carries
/// its own smoked-glass tint and a stronger border to hold an edge against
/// an arbitrary photograph.
///
/// The background is [`color::GLASS_DEEP_BACKED`], not `GLASS_DEEP` itself:
/// the compositor's blur-backdrop decision is geometric and compositor-side
/// only (`shows_through` in `abyss/src/render/mod.rs`), so this client is
/// never told whether it is about to happen. `GLASS_DEEP` alone has nothing
/// opaque under it when blur is off or degrades, and reads as a broken
/// translucent smear (BLUR-01). The backed token is `GLASS_DEEP` pre-composited
/// over an opaque token, so the panel reads the same intentional solid glass
/// either way.
pub fn surface(_t: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(color::GLASS_DEEP_BACKED)),
        border: Border {
            color: color::BORDER_STRONG,
            width: space::HAIRLINE,
            radius: radius::CARD.into(),
        },
        shadow: Shadow {
            color: Color {
                a: 0.9,
                ..Color::BLACK
            },
            offset: Vector::new(0.0, 28.0),
            blur_radius: 80.0,
        },
        ..container::Style::default()
    }
}

/// A context menu's ground.
///
/// [`surface`] with the transparency taken out of it. A menu is aimed at, not
/// glanced at: the row under the pointer has to be the most definite thing on
/// the screen for the moment it is open, and smoked glass over a paragraph of
/// text is how a verb becomes unreadable. It keeps the surface's border and
/// shadow, because it is still a sheet lying on the desktop.
pub fn menu_surface(_t: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(color::MENU_GROUND)),
        ..surface(_t)
    }
}

/// The bar's ground: the same smoked glass as [`surface`], but square, with
/// only its bottom edge drawn. A bar is an edge of the screen and not a
/// floating sheet, so it has no radius and no shadow — the hairline under it
/// and the highlight along its top are the whole of its depth.
pub fn bar_ground(_t: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(color::GLASS_DEEP)),
        // The faint end of the spec's border range, not the strong one: the
        // bar's silhouette should be read from its shape, not announced by
        // its outline.
        border: Border {
            color: color::HAIRLINE,
            width: space::HAIRLINE,
            radius: bar::RADIUS_SHEET.into(),
        },
        ..container::Style::default()
    }
}

/// One level of inset inside a panel. The spec's limit — there is no
/// `inset_inside_inset`, on purpose.
pub fn inset(_t: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color {
            a: 0.03,
            ..Color::WHITE
        })),
        border: Border {
            color: color::HAIRLINE,
            width: 1.0,
            radius: radius::INSET.into(),
        },
        ..container::Style::default()
    }
}

/// The window ground.
pub fn window(_t: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(color::BASE)),
        border: border::rounded(radius::WINDOW),
        ..container::Style::default()
    }
}

/// The left sidebar: darker than anything beside it.
pub fn sidebar(_t: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(color::SIDEBAR)),
        ..container::Style::default()
    }
}

/// A nav item. `active` gets the translucent fill; the 3px accent bar at its
/// left edge is drawn by [`crate::widget::nav_item`], because a container
/// border cannot be one-sided.
pub fn nav(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_t, status| {
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: Some(Background::Color(match (active, hovered) {
                (true, _) => Color {
                    a: 0.07,
                    ..Color::WHITE
                },
                (false, true) => Color {
                    a: 0.04,
                    ..Color::WHITE
                },
                (false, false) => Color::TRANSPARENT,
            })),
            text_color: if active {
                color::TEXT
            } else {
                color::TEXT_SECONDARY
            },
            border: border::rounded(radius::INSET),
            ..button::Style::default()
        }
    }
}

/// A pill control. `selected` is the one accent state — segmented choices use
/// this, and only one member of a group may be selected at a time, which is
/// what keeps a pane from showing two competing yellows.
pub fn pill(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_t, status| {
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: Some(Background::Color(if selected {
                color::ACCENT_FILL
            } else if hovered {
                Color {
                    a: 0.07,
                    ..Color::WHITE
                }
            } else {
                Color {
                    a: 0.035,
                    ..Color::WHITE
                }
            })),
            text_color: if selected {
                color::ACCENT_TEXT
            } else {
                color::TEXT_SECONDARY
            },
            border: Border {
                color: if selected {
                    color::ACCENT_BORDER
                } else {
                    color::BORDER
                },
                width: 1.0,
                radius: radius::PILL.into(),
            },
            ..button::Style::default()
        }
    }
}

/// A chip on a spatial canvas: an object, not a verb. `selected` is the
/// pane's one yellow when a canvas is its hero.
pub fn chip(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_t, status| {
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: Some(Background::Color(match (selected, hovered) {
                (true, _) => color::ACCENT_FILL,
                (false, true) => color::LIFT,
                (false, false) => color::LIFT_SOFT,
            })),
            text_color: if selected { color::ACCENT_TEXT } else { color::TEXT },
            border: Border {
                color: if selected {
                    color::ACCENT_BORDER
                } else {
                    color::BORDER
                },
                width: 1.0,
                radius: radius::CHIP.into(),
            },
            ..button::Style::default()
        }
    }
}

/// A destructive action. Never accented — see the note on [`color::DANGER`].
pub fn danger(_t: &Theme, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: Some(Background::Color(Color {
            a: if hovered { 0.18 } else { 0.10 },
            ..color::DANGER
        })),
        text_color: color::DANGER,
        border: Border {
            color: Color {
                a: 0.32,
                ..color::DANGER
            },
            width: 1.0,
            radius: radius::PILL.into(),
        },
        ..button::Style::default()
    }
}

pub fn text_primary(_t: &Theme) -> text::Style {
    text::Style {
        color: Some(color::TEXT),
    }
}

pub fn text_secondary(_t: &Theme) -> text::Style {
    text::Style {
        color: Some(color::TEXT_SECONDARY),
    }
}

pub fn text_tertiary(_t: &Theme) -> text::Style {
    text::Style {
        color: Some(color::TEXT_TERTIARY),
    }
}

/// The one live value on a pane.
pub fn text_accent(_t: &Theme) -> text::Style {
    text::Style {
        color: Some(color::ACCENT_TEXT),
    }
}

pub fn text_danger(_t: &Theme) -> text::Style {
    text::Style {
        color: Some(color::DANGER),
    }
}

/// Hairline between rows. Thickness is the `Rule`'s own, not the style's.
pub fn hairline(_t: &Theme) -> rule::Style {
    rule::Style {
        color: color::HAIRLINE,
        radius: 0.0.into(),
        fill_mode: rule::FillMode::Full,
        snap: true,
    }
}

pub fn eclipse_slider(_t: &Theme, status: slider::Status) -> slider::Style {
    let knob = if matches!(status, slider::Status::Dragged) {
        9.0
    } else {
        8.5
    };
    slider::Style {
        rail: slider::Rail {
            backgrounds: (Background::Color(color::ACCENT), Background::Color(color::TRACK)),
            width: 5.0,
            border: border::rounded(radius::PILL),
        },
        handle: slider::Handle {
            shape: slider::HandleShape::Circle { radius: knob },
            background: Background::Color(color::ACCENT),
            border_color: Color::TRANSPARENT,
            border_width: 0.0,
        },
    }
}

pub fn eclipse_radio(_t: &Theme, status: radio::Status) -> radio::Style {
    let selected = match status {
        radio::Status::Active { is_selected } | radio::Status::Hovered { is_selected } => is_selected,
    };
    radio::Style {
        background: Background::Color(Color::TRANSPARENT),
        dot_color: color::ACCENT,
        border_width: 1.0,
        border_color: if selected {
            color::ACCENT
        } else {
            color::BORDER_STRONG
        },
        text_color: None,
    }
}

pub fn eclipse_input(_t: &Theme, status: text_input::Status) -> text_input::Style {
    let focused = matches!(status, text_input::Status::Focused { .. });
    text_input::Style {
        background: Background::Color(Color {
            a: 0.04,
            ..Color::WHITE
        }),
        border: Border {
            color: if focused {
                color::ACCENT_BORDER
            } else {
                color::BORDER
            },
            width: 1.0,
            radius: radius::INSET.into(),
        },
        icon: color::TEXT_TERTIARY,
        placeholder: color::TEXT_TERTIARY,
        value: color::TEXT,
        selection: Color {
            a: 0.28,
            ..color::ACCENT
        },
    }
}

/// The same field while its text does not parse: a danger border and no
/// commit path. The tint is deliberately not the accent — a rejected draft is
/// not state, it is a refusal.
pub fn eclipse_input_invalid(t: &Theme, status: text_input::Status) -> text_input::Style {
    text_input::Style {
        border: Border {
            color: color::DANGER,
            ..eclipse_input(t, status).border
        },
        ..eclipse_input(t, status)
    }
}

/// A text field that is not a box: the band around it (see
/// [`crate::widget::prompt_band`]) is already the visible container, and a
/// second border inside it would be a card in a card. The selection tint is
/// the only accent it carries, because a caret is furniture, not state.
pub fn prompt_input(_t: &Theme, _status: text_input::Status) -> text_input::Style {
    text_input::Style {
        background: Background::Color(Color::TRANSPARENT),
        border: Border::default(),
        icon: color::TEXT_TERTIARY,
        placeholder: color::TEXT_TERTIARY,
        value: color::TEXT,
        selection: Color {
            a: 0.28,
            ..color::ACCENT
        },
    }
}

/// Scrollbars stay neutral: a scrollbar is not state, and accenting one would
/// spend the pane's single yellow on furniture.
pub fn eclipse_scrollable(_t: &Theme, status: scrollable::Status) -> scrollable::Style {
    let hovered = !matches!(status, scrollable::Status::Active { .. });
    let rail = scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: Background::Color(Color {
                a: if hovered { 0.22 } else { 0.13 },
                ..Color::WHITE
            }),
            border: border::rounded(radius::PILL),
        },
    };
    scrollable::Style {
        container: container::Style::default(),
        vertical_rail: rail,
        horizontal_rail: rail,
        gap: None,
        auto_scroll: scrollable::AutoScroll {
            background: Background::Color(color::SURFACE_2),
            border: Border {
                color: color::BORDER,
                width: 1.0,
                radius: radius::PILL.into(),
            },
            shadow: Shadow::default(),
            icon: color::TEXT_SECONDARY,
        },
    }
}

/// Default text size for a pane's body copy, for `iced::Settings`.
pub const DEFAULT_TEXT_SIZE: f32 = size::BODY;
