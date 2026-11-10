// SPDX-License-Identifier: AGPL-3.0-only
//! The panes' pictures. All state and behaviour live in [`crate::model`].
//!
//! These are ordinary dialogs of a session client. They do not borrow the
//! look of the compositor's own confirmation (ADR 0009): no centred modal on
//! a dimmed screen, no fixed-text consent wording. Whatever the requester
//! wrote is shown as a quoted claim, cleaned, and the identity hint beside
//! the header says how the name was derived.
//!
//! Accent ledger, per pane: the source chooser and the screenshot cards spend
//! the one yellow on the selected tile's caption; every other pane spends it
//! on the one phrase in the header that says what is being asked for.

use iced::widget::{button, column, container, row, scrollable, svg, text, text_input, Space};
use iced::{Alignment, Background, Border, Element, Length, Padding, Theme};

use ecl_portal_proto::{IdentitySource, Requester, ScreenshotMode, Source};

use ec_ui::theme;
use ec_ui::tokens::{color, font, radius, size, space};
use ec_ui::widget::{
    badge, big_value, color_picker, edge_note, inset, inset_list, key_hints, pick_pill, pill_group,
    prompt_band, status_chip, status_grid, subtitle, value, Toggle,
};

use crate::app::{App, Message};
use crate::metrics;
use crate::model::{clean, Choice, Chooser, Confirm, Nav, Pane, Tab, MODES};
use crate::tile::{source_tile, Glyph, TileSpec};

type El<'a> = Element<'a, Message, Theme>;

pub fn view(app: &App) -> El<'_> {
    let body = match &app.pane {
        Pane::Source(c) => source(app, c),
        Pane::Permission {
            title,
            detail,
            interface,
            confirm,
        } => permission(app, title, detail, interface, confirm),
        Pane::Shot(s) => shot(app, s.mode),
        Pane::Color(p) => colour(app, p),
        Pane::Account { reason, form } => account(app, reason, form),
        Pane::Email { addresses, confirm } => email(app, addresses, confirm),
    };

    // The content settles in from slightly above its resting place.
    let drop = (1.0 - app.enter.value()).clamp(0.0, 1.0) * metrics::ENTER_DROP;
    let (blur, rounding) = (app.blur, app.radius);
    let framed = container(body)
        .padding(Padding {
            top: space::PANE_Y - drop,
            right: space::PANE_X,
            bottom: space::PANE_Y + drop,
            left: space::PANE_X,
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |t: &Theme| {
            // Blur on: a light tint over the compositor's blur. Off: an
            // opaque ground, or the tint would land on nothing.
            if blur {
                theme::window_pane(true)(t)
            } else {
                theme::window(rounding)(t)
            }
        });
    ec_ui::glass::root_sole(framed).into()
}

// ---- shared pieces ---------------------------------------------------------

fn who(req: &Requester) -> String {
    clean(
        req.display_name
            .as_deref()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or(&req.app_id),
        metrics::MAX_NAME,
    )
}

fn how(src: IdentitySource) -> &'static str {
    match src {
        IdentitySource::Flatpak => "via flatpak",
        IdentitySource::SecurityContext => "via security context",
        IdentitySource::DesktopFile => "via desktop file",
        IdentitySource::Host => "via host process",
    }
}

fn plain<'a>(s: &str) -> El<'a> {
    text(s.to_string())
        .font(font::UI)
        .size(size::BODY)
        .style(theme::text_secondary)
        .into()
}

fn note<'a>(s: &str) -> El<'a> {
    text(s.to_string())
        .font(font::UI)
        .size(size::BODY_SMALL)
        .style(theme::text_tertiary)
        .into()
}

fn avatar<'a>(app: &App, side: f32) -> El<'a> {
    if let Some(path) = &app.icon {
        return svg(svg::Handle::from_path(path))
            .width(Length::Fixed(side))
            .height(Length::Fixed(side))
            .into();
    }
    let initial = who(&app.requester)
        .chars()
        .find(|c| c.is_alphanumeric())
        .map_or_else(|| "?".to_owned(), |c| c.to_uppercase().to_string());
    container(
        text(initial)
            .font(font::UI_SEMIBOLD)
            .size(metrics::AVATAR_INITIAL)
            .style(theme::text_secondary),
    )
    .width(Length::Fixed(side))
    .height(Length::Fixed(side))
    .center_x(Length::Fixed(side))
    .center_y(Length::Fixed(side))
    .style(|_t: &Theme| container::Style {
        background: Some(Background::Color(color::lift())),
        border: Border {
            color: color::border(),
            width: space::HAIRLINE,
            radius: radius::INSET.into(),
        },
        ..container::Style::default()
    })
    .into()
}

/// Avatar, the requester's name, one line of what it wants, and at the right
/// the subtle reading of how the name was derived. A claim, shown as one.
fn header<'a>(app: &App, sub: El<'a>) -> El<'a> {
    let req = &app.requester;
    let id = clean(&req.app_id, metrics::MAX_ID);
    let name = text(who(req))
        .font(font::UI_SEMIBOLD)
        .size(size::PANE_TITLE)
        .style(theme::text_primary)
        .wrapping(text::Wrapping::None);
    // A narrow sheet has no room beside the title for the identity hint, and
    // the shared chip right-aligns its lines, which reads ragged once folded
    // under the title. There it is one left-aligned line in the title block.
    if app.pane.window().0 < metrics::SHEET_FOLD_W {
        let hint = text(format!("{} · {id}", how(req.source)))
            .font(font::DATA)
            .size(size::MICRO)
            .style(theme::text_tertiary)
            .wrapping(text::Wrapping::None);
        let titles = column![name, sub, hint].spacing(space::TITLE_GAP);
        return row![avatar(app, metrics::AVATAR), titles]
            .spacing(metrics::AVATAR_GAP)
            .align_y(Alignment::Center)
            .into();
    }
    let titles = column![name, sub].spacing(space::TITLE_GAP);
    let lead = row![avatar(app, metrics::AVATAR), titles]
        .spacing(metrics::AVATAR_GAP)
        .align_y(Alignment::Center);
    ec_ui::widget::fold(lead, status_chip(how(req.source), &id))
}

/// A button. `strong` is the lifted-white state; with no message it is inert.
fn action<'a>(label: &str, strong: bool, msg: Option<Message>) -> El<'a> {
    let live = msg.is_some();
    button(
        text(label.to_string())
            .font(font::UI_MEDIUM)
            .size(size::BODY_SMALL),
    )
    .padding([space::PILL_Y, space::PILL_X])
    .on_press_maybe(msg)
    .style(move |t: &Theme, s| {
        if live {
            theme::pick(strong)(t, s)
        } else {
            theme::pill_disabled(t, s)
        }
    })
    .into()
}

/// A switch and its label; the label presses it too.
fn switch<'a>(label: &str, on: bool, msg: fn(bool) -> Message) -> El<'a> {
    row![
        Toggle::new(on, msg),
        button(
            text(label.to_string())
                .font(font::UI)
                .size(size::BODY)
                .style(theme::text_secondary)
        )
        .padding(0)
        .on_press(msg(!on))
        .style(theme::link),
    ]
    .spacing(space::CONTROL_GAP)
    .align_y(Alignment::Center)
    .into()
}

fn footer<'a>(left: El<'a>, buttons: Vec<El<'a>>) -> El<'a> {
    let mut r = row![container(left).width(Length::Fill)]
        .spacing(space::CONTROL_GAP)
        .align_y(Alignment::Center);
    for b in buttons {
        r = r.push(b);
    }
    r.into()
}

fn fill_gap<'a>() -> El<'a> {
    Space::new().height(Length::Fill).into()
}

// ---- source chooser --------------------------------------------------------

fn source_tile_for<'a>(c: &Chooser, i: usize, cursor: bool) -> El<'a> {
    let (glyph, title, caption, well_note) = match c.source(i) {
        Source::Output {
            name,
            width,
            height,
        } => (
            Glyph::Screen,
            clean(name, metrics::MAX_CAPTION),
            "Display".to_owned(),
            Some(format!("{width} × {height}")),
        ),
        Source::Window { title, app_id, .. } => {
            let t = clean(title, metrics::MAX_CAPTION);
            (
                Glyph::Window,
                if t.is_empty() {
                    clean(app_id, metrics::MAX_CAPTION)
                } else {
                    t
                },
                clean(app_id, metrics::MAX_CAPTION),
                None,
            )
        }
    };
    source_tile(
        TileSpec {
            glyph,
            title,
            caption,
            well_note,
            // The daemon sends frames later; until then the well shows the
            // type icon and the resolution.
            thumb: None,
            well_h: metrics::THUMB_H,
            selected: c.is_selected(i),
            cursor,
        },
        Message::Pick(i),
    )
}

fn source<'a>(app: &App, c: &Chooser) -> El<'a> {
    let wants = match c.tabs() {
        [Tab::Screen] => "wants to share your screen",
        [Tab::Window] => "wants to share a window",
        _ => "wants to share your screen or a window",
    };

    // Tabs at the left, the keyboard's reading at the right.
    let tabs: Vec<El<'a>> = c
        .tabs()
        .iter()
        .map(|t| {
            let label = match t {
                Tab::Screen => "Screen",
                Tab::Window => "Window",
            };
            pick_pill(label, *t == c.tab(), Some(Message::Tab(*t)))
        })
        .collect();
    let bar = row![
        pill_group(tabs),
        Space::new().width(Length::Fill),
        key_hints(&[
            ("←↑↓→", "move"),
            ("space", "pick"),
            ("tab", "switch"),
            ("esc", "cancel")
        ]),
    ]
    .align_y(Alignment::Center);

    let visible = c.visible();
    let at = c.cursor_source();
    let grid: El<'a> = if visible.is_empty() {
        container(
            text(match c.tab() {
                Tab::Screen => "No screens to share",
                Tab::Window => "No windows to share",
            })
            .font(font::DATA)
            .size(size::MONO)
            .style(theme::text_tertiary),
        )
        .width(Length::Fill)
        .height(Length::Fixed(metrics::GRID_H))
        .center_x(Length::Fill)
        .center_y(Length::Fixed(metrics::GRID_H))
        .into()
    } else {
        let tiles = visible
            .iter()
            .map(|i| source_tile_for(c, *i, at == Some(*i)))
            .collect();
        // A short list hugs its tiles; only a long one takes the scrolling
        // viewport, so a few displays do not sit in an empty well.
        let h = if visible.len() > metrics::GRID_COLS * 2 {
            Length::Fixed(metrics::GRID_H)
        } else {
            Length::Shrink
        };
        scrollable(status_grid(tiles, metrics::GRID_COLS))
            .height(h)
            .style(theme::eclipse_scrollable)
            .into()
    };
    let hero = column![bar, inset(grid).padding(space::ROW_Y)].spacing(space::ROW_Y);

    let mut options = row![].spacing(space::BLOCK).align_y(Alignment::Center);
    if c.allow_audio() {
        options = options.push(switch("Share audio", c.audio, Message::Audio));
    }
    options = options.push(switch(
        "Remember this choice",
        c.remember,
        Message::Remember,
    ));

    let share = match c.selected_count() {
        0 | 1 => "Share".to_owned(),
        n => format!("Share {n}"),
    };
    let go = c.can_share().then_some(Message::Key(Nav::Enter));
    let bottom = footer(
        note("Eclipse will ask you to confirm before anything is shared."),
        vec![
            action("Cancel", false, Some(Message::Key(Nav::Escape))),
            action(&share, true, go),
        ],
    );

    column![header(app, plain(wants)), hero, options, fill_gap(), bottom]
        .spacing(space::BLOCK)
        .into()
}

// ---- confirm sheets --------------------------------------------------------

fn confirm_buttons<'a>(confirm: &Confirm, deny: &str, allow: &str) -> Vec<El<'a>> {
    vec![
        action(
            deny,
            confirm.focus == Choice::Deny,
            Some(Message::Choose(Choice::Deny)),
        ),
        action(
            allow,
            confirm.focus == Choice::Allow,
            Some(Message::Choose(Choice::Allow)),
        ),
    ]
}

fn permission<'a>(
    app: &App,
    title: &str,
    detail: &str,
    interface: &str,
    confirm: &Confirm,
) -> El<'a> {
    let subject = clean(title, metrics::MAX_SUBJECT);
    let sub = subtitle("asks for ", &subject, "");
    let banner = edge_note("Details", detail, color::neutral());

    // The shared prefix is noise; the tail names the portal interface.
    let short = interface
        .strip_prefix("org.freedesktop.impl.portal.")
        .unwrap_or(interface);
    let mut line = row![badge(short), Space::new().width(Length::Fill)]
        .spacing(space::BLOCK)
        .align_y(Alignment::Center);
    if confirm.allow_remember() {
        line = line.push(switch(
            "Remember my answer",
            confirm.remember,
            Message::Remember,
        ));
    }

    let bottom = footer(
        key_hints(&[("←→", "move"), ("enter", "choose"), ("esc", "dismiss")]),
        confirm_buttons(confirm, "Deny", "Allow"),
    );
    column![header(app, sub), banner, line, fill_gap(), bottom]
        .spacing(space::BLOCK)
        .into()
}

fn email<'a>(app: &App, addresses: &[String], confirm: &Confirm) -> El<'a> {
    let n = addresses.len();
    let count = if n == 1 {
        "1 recipient".to_owned()
    } else {
        format!("{n} recipients")
    };
    let sub = subtitle("wants to open your mail app for ", &count, "");

    let mut rows: Vec<El<'a>> = addresses
        .iter()
        .take(metrics::MAX_ADDRESSES)
        .map(|a| {
            container(value(a))
                .padding([space::ROW_Y, space::CARD])
                .width(Length::Fill)
                .into()
        })
        .collect();
    if n > metrics::MAX_ADDRESSES {
        rows.push(
            container(note(&format!("and {} more", n - metrics::MAX_ADDRESSES)))
                .padding([space::ROW_Y, space::CARD])
                .into(),
        );
    }
    let list = if rows.is_empty() {
        inset_list(
            "Recipients",
            vec![container(note("None given"))
                .padding([space::ROW_Y, space::CARD])
                .into()],
        )
    } else {
        inset_list("Recipients", rows)
    };

    let bottom = footer(
        note("Nothing is sent until you send it."),
        confirm_buttons(confirm, "Cancel", "Open mail app"),
    );
    column![header(app, sub), list, fill_gap(), bottom]
        .spacing(space::BLOCK)
        .into()
}

// ---- screenshot mode -------------------------------------------------------

fn shot<'a>(app: &App, mode: ScreenshotMode) -> El<'a> {
    let cards: Vec<El<'a>> = MODES
        .iter()
        .map(|m| {
            let (glyph, title, caption) = match m {
                ScreenshotMode::Screen => (Glyph::Screen, "Screen", "The whole display"),
                ScreenshotMode::Window => (Glyph::Window, "Window", "One window"),
                ScreenshotMode::Area => (Glyph::Area, "Area", "A region you drag"),
            };
            container(source_tile(
                TileSpec {
                    glyph,
                    title: title.to_owned(),
                    caption: caption.to_owned(),
                    well_note: None,
                    thumb: None,
                    well_h: metrics::MODE_WELL_H,
                    selected: *m == mode,
                    cursor: false,
                },
                Message::Mode(*m),
            ))
            .width(Length::FillPortion(1))
            .into()
        })
        .collect();
    let mut hero = row![].spacing(space::GRID_GAP);
    for c in cards {
        hero = hero.push(c);
    }

    let bottom = footer(
        key_hints(&[("←→", "move"), ("enter", "capture"), ("esc", "cancel")]),
        vec![
            action("Cancel", false, Some(Message::Key(Nav::Escape))),
            action("Capture", true, Some(Message::Key(Nav::Enter))),
        ],
    );
    column![
        header(app, plain("wants to take a screenshot")),
        hero,
        fill_gap(),
        bottom
    ]
    .spacing(space::BLOCK)
    .into()
}

// ---- colour ----------------------------------------------------------------

fn colour<'a>(app: &App, p: &crate::model::ColorPick) -> El<'a> {
    let [r, g, b, _] = p.rgba();
    let picked = iced::Color::from_rgb8(r, g, b);
    let swatch = container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(metrics::SWATCH_H))
        .style(move |_t: &Theme| container::Style {
            background: Some(Background::Color(picked)),
            border: Border {
                color: color::border_strong(),
                width: space::HAIRLINE,
                radius: radius::INSET.into(),
            },
            ..container::Style::default()
        });
    let hex = text_input("#rrggbb", &p.draft)
        .on_input(Message::Hex)
        .on_submit(Message::Key(Nav::Enter))
        .font(font::DATA)
        .size(size::BODY)
        .width(Length::Fill)
        .style(if p.draft_valid() {
            theme::eclipse_input
        } else {
            theme::eclipse_input_invalid
        });
    let reading = column![swatch, big_value(&p.hex(), "", false), hex]
        .spacing(space::ROW_Y)
        .width(Length::Fill);
    let hero = inset(
        row![
            color_picker(p.rgba(), false, Message::Rgba, Message::Released),
            reading
        ]
        .spacing(space::BLOCK),
    )
    .padding(space::CARD);

    let go = p.draft_valid().then_some(Message::Key(Nav::Enter));
    let bottom = footer(
        key_hints(&[("enter", "pick"), ("esc", "cancel")]),
        vec![
            action("Cancel", false, Some(Message::Key(Nav::Escape))),
            action("Pick", true, go),
        ],
    );
    column![
        header(app, subtitle("wants you to pick a ", "colour", "")),
        hero,
        fill_gap(),
        bottom
    ]
    .spacing(space::BLOCK)
    .into()
}

// ---- account ---------------------------------------------------------------

fn account<'a>(app: &App, reason: &str, form: &crate::model::AccountForm) -> El<'a> {
    let why = if reason.is_empty() {
        "No reason given.".to_owned()
    } else {
        reason.to_owned()
    };
    let quote = edge_note("Reason given by the app", &why, color::neutral());

    let name = text_input("login name", &form.name)
        .on_input(Message::Name)
        .on_submit(Message::Key(Nav::Enter))
        .font(font::DATA)
        .size(size::BODY)
        .style(theme::prompt_input);
    let real = text_input("display name", &form.real_name)
        .on_input(Message::RealName)
        .on_submit(Message::Key(Nav::Enter))
        .font(font::DATA)
        .size(size::BODY)
        .style(theme::prompt_input);
    let hero = row![
        avatar(app, metrics::AVATAR_LARGE),
        column![prompt_band("@", name, None), prompt_band("~", real, None)].spacing(space::ROW_Y),
    ]
    .spacing(metrics::AVATAR_GAP)
    .align_y(Alignment::Center);

    let go = form.can_share().then_some(Message::Key(Nav::Enter));
    let bottom = footer(
        note("Shared as shown. Edit anything first."),
        vec![
            action("Cancel", false, Some(Message::Key(Nav::Escape))),
            action("Share", true, go),
        ],
    );
    column![
        header(app, subtitle("asks for your ", "account details", "")),
        quote,
        hero,
        fill_gap(),
        bottom
    ]
    .spacing(space::BLOCK)
    .into()
}
