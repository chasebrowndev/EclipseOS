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

/// A panel: the spec's glass — a thin white fill and a hairline border. A
/// tile inside a window, not a sheet floating over the desktop, so it casts
/// no drop of its own: the window already has the compositor's shadow, and a
/// 70px shadow per card under translucent glass is a grey bruise around
/// every block.
///
/// The fill is deliberately thin (`.045`, inside the spec's `.035–.06`). On a
/// settings pane it composites over the content column's smoked glass (or
/// its opaque fallback); on a layer surface over the compositor's blur of
/// whatever is behind. Depth is the border doing its job — never fill
/// opacity, which is what turns glass back into grey paint.
///
/// `radius` is a parameter rather than [`radius::CARD`] baked in, because the
/// glass this fn draws has to live-sync to the compositor's own
/// `decoration.rounding` (`eclipse_ui::ipc::fetch_config_radius`) — a caller
/// with no live value yet still passes `radius::CARD` explicitly, so the
/// fallback stays visible at the call site instead of hiding in here.
pub fn panel(radius: f32) -> impl Fn(&Theme) -> container::Style {
    move |_t: &Theme| container::Style {
        background: Some(Background::Color(color::GLASS)),
        border: Border {
            color: color::BORDER,
            width: space::HAIRLINE,
            radius: radius.into(),
        },
        ..container::Style::default()
    }
}

/// A surface that floats over live wallpaper: a toast card, the launcher
/// sheet, the control centre. Distinct from [`panel`] because it has no
/// window ground underneath it — only the compositor's material.
///
/// `blur` is `decoration.blur.mode != "off"`, read over `get_config`
/// ([`crate::ipc::fetch_blur`]). With it on, the compositor draws the whole
/// material *under* this surface — blur, vibrancy, a near-white hairline rim
/// and a soft offset shadow — so the client lays only a light tint
/// ([`color::SHEET_TINT`]) and no border or shadow of its own: a second rim
/// or a second shadow on top of the compositor's is exactly the doubled edge
/// this avoids.
///
/// With it off there is nothing behind the tint, so the ground is the opaque
/// [`color::GLASS_DEEP_BACKED`] (BLUR-01) and the client draws its own
/// hairline to hold an edge against an arbitrary photograph.
pub fn surface(radius: f32, blur: bool) -> impl Fn(&Theme) -> container::Style {
    move |_t: &Theme| container::Style {
        background: Some(Background::Color(if blur {
            color::SHEET_TINT
        } else {
            color::GLASS_DEEP_BACKED
        })),
        border: Border {
            color: if blur {
                Color::TRANSPARENT
            } else {
                color::BORDER_STRONG
            },
            width: space::HAIRLINE,
            radius: radius.into(),
        },
        ..container::Style::default()
    }
}

/// A context menu's ground.
///
/// Opaque whatever the blur setting. A menu is aimed at, not glanced at: the
/// row under the pointer has to be the most definite thing on the screen for
/// the moment it is open, and smoked glass over a paragraph of text is how a
/// verb becomes unreadable. It is also an xdg popup, which the compositor
/// does not give the layer material, so it keeps [`surface`]'s blur-off
/// hairline as its own edge.
pub fn menu_surface(radius: f32) -> impl Fn(&Theme) -> container::Style {
    move |t: &Theme| container::Style {
        background: Some(Background::Color(color::MENU_GROUND)),
        ..surface(radius, false)(t)
    }
}

/// The bar's ground. `radius` live-syncs to the compositor's `bar.rounding`,
/// kept apart from [`panel`]/[`surface`]'s `decoration.rounding` because the
/// bar sheet's corner is its own setting (COMP-13 §1.2).
///
/// With `blur` on it is the thinnest glass on screen — [`color::BAR_TINT`]
/// and no border, the compositor's rim and shadow being the whole of its
/// edge. With it off it falls back to the opaque [`color::GLASS_DEEP_BACKED`]
/// and a faint hairline (BLUR-01): the bar's silhouette should be read from
/// its shape, not announced by its outline.
pub fn bar_ground(radius: f32, blur: bool) -> impl Fn(&Theme) -> container::Style {
    move |_t: &Theme| container::Style {
        background: Some(Background::Color(if blur {
            color::BAR_TINT
        } else {
            color::GLASS_DEEP_BACKED
        })),
        border: Border {
            color: if blur { Color::TRANSPARENT } else { color::HAIRLINE },
            width: space::HAIRLINE,
            radius: radius.into(),
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

/// The window ground, at `radius` — the compositor's live
/// `decoration.rounding`, so the ground is concentric with the corner mask
/// abyss cuts the window to.
pub fn window(radius: f32) -> impl Fn(&Theme) -> container::Style {
    move |_t: &Theme| container::Style {
        background: Some(Background::Color(color::BASE)),
        border: border::rounded(radius),
        ..container::Style::default()
    }
}

/// The left sidebar: darker than anything beside it. With `blur` on it is
/// the translucent [`color::SIDEBAR`] over the compositor's blur — the one
/// part of a settings window that lets the desktop through — and with it off
/// the same colour pre-composited over the window base.
pub fn sidebar(blur: bool) -> impl Fn(&Theme) -> container::Style {
    move |_t: &Theme| container::Style {
        background: Some(Background::Color(if blur {
            color::SIDEBAR
        } else {
            color::SIDEBAR_BACKED
        })),
        ..container::Style::default()
    }
}

/// The content column beside a [`sidebar`], in a window that is itself
/// transparent. With `blur` on it is [`color::CONTENT`] — the same smoked
/// glass as the sidebar, a step lighter — so the window reads as one pane of
/// glass with a hairline down it, not a glass rail beside an opaque slab.
/// With it off there is nothing behind the window, and the ground is the
/// opaque [`color::CONTENT_BACKED`]. Square: the compositor's corner mask
/// rounds it.
pub fn content_ground(blur: bool) -> impl Fn(&Theme) -> container::Style {
    move |_t: &Theme| container::Style {
        background: Some(Background::Color(if blur {
            color::CONTENT
        } else {
            color::CONTENT_BACKED
        })),
        ..container::Style::default()
    }
}

/// The window-level style of a transparent toplevel: no background, so the
/// compositor's blur shows wherever a child does not paint.
pub fn clear_window<S>(_state: &S, theme: &Theme) -> iced::theme::Style {
    iced::theme::Style {
        background_color: Color::TRANSPARENT,
        text_color: theme.palette().text,
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

/// Which of the bar cell's three looks a cell wears.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CellTone {
    /// Every cell at rest: the neutral glass lozenge.
    #[default]
    Plain,
    /// The focused window, and the current workspace tile: the lozenge on a
    /// gold fill with a gold label. Its rim stays neutral.
    Focused,
    /// A minimized window: no fill, a ghost rim, no shadow.
    Away,
}

/// `true` is the focused cell, `false` a plain one — the two every caller
/// but a minimized chip needs.
impl From<bool> for CellTone {
    fn from(accent: bool) -> Self {
        if accent {
            CellTone::Focused
        } else {
            CellTone::Plain
        }
    }
}

/// A cell on the taskbar: a window chip, a widget, a pager tile, the
/// launcher. One ground for all of them, so a chip and a widget sitting side
/// by side on the bar read as one family (ADR 0065).
///
/// Each is a small glass lozenge on the bar's glass: a faint white fill
/// ([`color::CELL`]), a hairline just inside the edge ([`color::CELL_RIM`])
/// and a soft drop under it ([`color::CELL_SHADOW`]). The pointer brightens
/// the fill and the rim together; a press brightens the fill again. A
/// [`CellTone::Focused`] cell rests on [`color::ACCENT_FILL`] with a gold
/// label — the bar's one resting yellow besides the current workspace tile —
/// and keeps the neutral rim: the accent is never an outline. A
/// [`CellTone::Away`] (minimized) cell has no fill and no drop, only a ghost
/// rim, and is lifted by the pointer like any other.
pub fn bar_cell(tone: impl Into<CellTone>) -> impl Fn(&Theme, button::Status) -> button::Style {
    let tone = tone.into();
    move |_t, status| {
        let lit = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let pressed = matches!(status, button::Status::Pressed);
        let background = match (tone, status) {
            (CellTone::Focused, button::Status::Pressed) => color::ACCENT_FILL_STRONG,
            (CellTone::Focused, button::Status::Hovered) => color::ACCENT_FILL_HOVER,
            (CellTone::Focused, _) => color::ACCENT_FILL,
            (_, button::Status::Pressed) => color::CELL_PRESS,
            (_, button::Status::Hovered) => color::CELL_HOVER,
            (CellTone::Away, _) => Color::TRANSPARENT,
            (CellTone::Plain, _) => color::CELL,
        };
        let rim = match (tone, lit) {
            (_, true) => color::CELL_RIM_HOVER,
            (CellTone::Away, false) => color::CELL_RIM_AWAY,
            (_, false) => color::CELL_RIM,
        };
        // A pressed cell is pushed into the glass, and a minimized one at
        // rest is not lifted off it at all: neither casts a drop.
        let shadow = if pressed || (tone == CellTone::Away && !lit) {
            Shadow::default()
        } else {
            Shadow {
                color: color::CELL_SHADOW,
                offset: Vector::new(0.0, bar::CELL_SHADOW_Y),
                blur_radius: bar::CELL_SHADOW_BLUR,
            }
        };
        button::Style {
            background: Some(Background::Color(background)),
            text_color: if tone == CellTone::Focused {
                color::ACCENT_TEXT
            } else {
                color::TEXT
            },
            border: Border {
                color: rim,
                width: space::HAIRLINE,
                radius: bar::RADIUS_CELL.into(),
            },
            shadow,
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
