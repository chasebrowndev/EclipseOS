// SPDX-License-Identifier: AGPL-3.0-only
//! The bar's one row — a desktop taskbar, in Plasma's structure and the
//! Eclipse palette.
//!
//! Left to right, four zones with four different silhouettes, so that no two
//! neighbours read as the same block:
//!
//! 1. **launcher button** — one ring, the only circle on the row;
//! 2. **workspace pager** — a run of small square chips, all identical;
//! 3. **task chips** — the hero: wide icon-led glass cells, left-aligned and
//!    flowing right, the zone that gives way first;
//! 4. **widgets** (ADR 0065) — glass cells of the same family behind a grip,
//!    right-aligned and ending at the clock, the only two-line cell.
//!
//! Where every cell goes is [`crate::layout::solve`]'s answer; how far each
//! has got there is [`crate::motion::Bar`]'s. This file only draws the two.
//!
//! ## The accent ledger
//!
//! Yellow means **up**. By explicit direction the accent is not focus and not
//! "one live thing": every window that is on the current workspace and has not
//! been sent away wears it, and a minimized window does not. The chip is
//! present either way — it is the only way back from minimized — so the accent
//! is the whole of the difference between a window that is on screen and a
//! window that is put away, and the chip is a toggle between those two states.
//!
//! The current workspace tile keeps its yellow on the same reasoning: "which
//! desktop am I on" is the same kind of fact as "what is up on it". The two
//! never compete for area — the tile is 24px, the chips are a strip.
//!
//! A chip spends its yellow the way the reference panes spend theirs on a
//! selected pill — an `ACCENT_BORDER` edge and an `ACCENT_TEXT` label, with the
//! ground earned by the pointer — and not on a 2px underline, which on a
//! rounded chip read as a sticker bolted to the bottom.
//!
//! The launcher mark is the third and last, also by explicit direction: the
//! corona is the desktop's own mark, and a white ring read as a disabled
//! button rather than as a logo. At rest it is that fixed ring. While
//! Oracle-Eyes is working it opens into an eye — a gold iris whose pupil
//! wanders while it watches and narrows to a point while it thinks — so the
//! mark reports that daemon's live state (ADR 0055). It stays one small
//! circle of the same yellow and never grows, so it still does not compete
//! for area with the two above.
//!
//! The widgets keep the rule with one exception, which is the fourth: Now
//! Playing's visualizer is yellow while media plays and the widget is open —
//! the one live value on the right of the row. Every other widget glyph and
//! reading is white at 1.0 / 0.64 / 0.40 — a full-bar signal and an idle
//! bluetooth adapter included, which as tray cells once spent yellow (and
//! blue) on states that are true nearly all the time. A low battery or a
//! custom widget's `critical` state may go `DANGER`, which is the one alarm
//! and not an accent.
//!
//! ## Glass
//!
//! The bar is a translucent sheet over the compositor's blur of whatever is
//! behind it, never an opaque ground. Its depth is three things: the smoked
//! fill of `theme::bar_ground`, the inset highlight along its top edge, and
//! the hairline along its bottom. Cells are rounded chips lying *on* that
//! sheet, on the spec's 9–11 inset-chip radius scale.
//!
//! Every colour and size comes from `eclipse_ui::tokens` — a literal anywhere
//! in this file is a bug, because the tokens are the only transcription of
//! `docs/STYLE.md`.

use iced::widget::{
    button, canvas, column, container, image, mouse_area, row, svg, text, Column, Row, Space,
};
use iced::{Alignment, Color, Element, Length, Theme};

use eclipse_ui::theme;
use eclipse_ui::tokens::{bar, color, drawer, font, menu, size, space};
use eclipse_ui::widget::{self as parts, ClipEdge};

use crate::app::Message;
use crate::icons::Icon;
use crate::layout::{chip_detail, Detail};
use crate::model::{Snapshot, Window, Workspace};

/// A bar, the one popup over the bars, or a bar's eye.
///
/// `iced_layershell`'s daemon pattern draws every surface through one function,
/// so the id is the branch. Whatever the surface draws, its messages leave
/// wrapped in [`Message::On`] with that surface's id, so `update` knows which
/// bar a press, a hover or a grip came from.
pub fn view(app: &crate::app::App, id: iced::window::Id) -> Element<'_, Message, Theme> {
    surface(app, id).map(move |m| Message::On(id, Box::new(m)))
}

fn surface(app: &crate::app::App, id: iced::window::Id) -> Element<'_, Message, Theme> {
    if app.bars.values().any(|b| b.eye_surface == Some(id)) {
        return eye_view(&app.iris);
    }
    match app.popup.as_ref() {
        Some(popup) if popup.id == id => {
            return match &popup.kind {
                crate::app::Kind::Menu { handle, items } => context_menu(*handle, items, app.menu_radius),
                crate::app::Kind::Drawer { which, .. } => drawer_view(app, *which),
                crate::app::Kind::TrayMenu { id, entries } => tray_menu(id, entries, app.menu_radius),
            };
        }
        _ => {}
    }
    // A surface that is none of these is a bar being torn down, or one whose
    // first configure beat its entry into the map: draw nothing.
    let Some(bar) = app.bars.get(&id) else {
        return Space::new().into();
    };
    match bar.fold.target {
        // Hidden is *gone*, not thin: nothing is drawn, and the surface it
        // still owns claims no exclusive zone, so a fullscreen video has the
        // whole output.
        crate::app::FoldTarget::Hidden => return Space::new().into(),
        // Folded, or mid-slide back out: still the folded strip until there
        // is room for a cell. `pill` is the same test the surface geometry
        // uses, so the view and the surface cannot disagree about which one
        // is up.
        _ if !bar.fold.pill() => return folded_row(app, bar),
        _ => {}
    }
    bar_row(app, bar)
}

/// The bar shrunk to a rule along the top of an output nobody is looking at.
///
/// A folded bar is a *state*, not a smaller bar: at `fold_height` (2..=16 px)
/// there is no room for a cell, and a clock cut off at its waist reads as a
/// bug. So the strip keeps only what makes the bar the bar — the smoked sheet
/// of [`theme::bar_ground`], its lit top edge, and a dormant hairline along
/// its bottom — and drops every zone.
///
/// Every part of it is `Fill` or a hairline, so the two pixels at the bottom
/// of the setting's range are squeezed out of the glass and never out of a
/// fixed child: the strip cannot overflow its own surface at any height.
fn folded_row<'a>(app: &'a crate::app::App, bar: &'a crate::app::Bar) -> Element<'a, Message, Theme> {
    let edges = column![
        Space::new().width(Length::Fill).height(Length::Fill),
        parts::quad(
            Length::Fill,
            Length::Fixed(bar::HAIRLINE),
            // Deliberately *not* the accent: a folded bar is by construction
            // the output the pointer is not on, and the ledger's one live
            // yellow belongs to a live value, never to the dormant head.
            color::HIGHLIGHT_SOFT,
            app.bar_radius,
        ),
    ];

    let sheet = parts::lit(
        container(edges)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::bar_ground(app.bar_radius)),
        app.bar_radius,
        color::HIGHLIGHT_SOFT,
    );

    // The horizontal inset is the unfolded bar's layer-shell margin, so the
    // strip is the same sheet seen edge-on rather than a second, wider
    // object. The surface *is* the sheet.
    container(sheet)
        .width(Length::Fill)
        // The animated height, not the settled one: during a slide the strip
        // must fill exactly the surface the compositor just sized.
        .height(Length::Fixed(bar.fold.height as f32))
        .into()
}

fn bar_row<'a>(app: &'a crate::app::App, bar: &'a crate::app::Bar) -> Element<'a, Message, Theme> {
    let mut bar_row = Row::new()
        .push(launcher_button())
        .push(Space::new().width(Length::Fixed(bar::ZONE_GAP)))
        .push(pager(app, bar))
        .push(Space::new().width(Length::Fixed(bar::ZONE_GAP)))
        // The task strip is also the row's spacer: it takes exactly the space
        // the widgets leave, so they cannot be pushed off.
        .push(tasks(app, bar));

    // Each widget carries its own leading gap, scaled by its presence, so a
    // widget arriving or leaving opens and closes its gap with its glass —
    // the neighbours glide instead of stepping by a gap at either end.
    let cells: Vec<_> = (0..app.widget_cfg.order.len())
        .filter_map(|i| crate::widgets::cell(app, bar, i))
        .collect();
    if !cells.is_empty() {
        bar_row = bar_row.push(Space::new().width(Length::Fixed(bar::ZONE_GAP - bar::GAP)));
    }
    // Two neighbours both compressed to their grips close up to one rack of
    // handles; the gap follows the less-compressed of the pair, so it opens
    // continuously as either widget does.
    let mut before: Option<f32> = None;
    for cell in cells {
        let gap = crate::layout::widget_gap(before.unwrap_or(0.0), cell.closed);
        bar_row = bar_row
            .push(Space::new().width(Length::Fixed((gap * cell.presence).round())))
            .push(cell.element);
        before = Some(cell.closed);
    }
    let bar_row = bar_row
        .padding([0.0, bar::EDGE])
        .align_y(Alignment::Center)
        .height(Length::Fixed(bar::PILL_H));

    // A wide pill floating in the strip it reserves, not a slab bounded by
    // two hard rules. The rules were the loudest marks on the desktop and
    // `docs/STYLE.md` never asked for them — it describes rounded, bordered,
    // softly-lit panels and nothing else. What survives of the edge is the
    // faintest of the spec's own top-highlight range and a hairline border,
    // which is the difference between glass and a grey rectangle; it is not a
    // licence to frost the whole bar.
    //
    // The pill fills its surface edge to edge. The compositor blurs the whole
    // surface at `bar.rounding`, so the float gap around the pill is
    // layer-shell margin (`FoldState::geometry`) and never padding in here —
    // padding is how a blurred rim came to show outside the pill.
    parts::lit(
        container(bar_row)
            .width(Length::Fill)
            .style(theme::bar_ground(app.bar_radius)),
        app.bar_radius,
        color::HIGHLIGHT_SOFT,
    )
}

/// The ground under a bar cell: a chip on glass.
///
/// Hover and press are *layered* lifts and not fill swaps — the fill, the
/// border and the top highlight move together, which is what reads as the
/// chip rising out of the sheet. A lone grey rectangle appearing under the
/// pointer is the thing this replaced.
fn cell_style(fill: Color, edge: Color) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_t: &Theme, status: button::Status| {
        let (background, border) = match status {
            // The fill stays under the outline it sits inside — see
            // `color::LIFT_SOFT`. Hover brightens the *edge*; the wash only
            // says where the pointer is.
            button::Status::Hovered => (color::LIFT_SOFT, color::BORDER_STRONG),
            button::Status::Pressed => (color::LIFT, color::BORDER_STRONG),
            _ => (fill, edge),
        };
        button::Style {
            background: Some(iced::Background::Color(background)),
            text_color: color::TEXT,
            border: iced::Border {
                color: border,
                width: bar::HAIRLINE,
                radius: bar::RADIUS_CELL.into(),
            },
            ..button::Style::default()
        }
    }
}

// ---------------------------------------------------------------- launcher

/// The far-left button. The mark is an eclipse: a ring, the corona left
/// behind when the body has covered the disc. It is drawn rather than shipped
/// as a bitmap because the DE ships no icon font and a raster logo would not
/// follow the palette — and it is a *ring* rather than a lit disc with a dark
/// quad across it because on a translucent bar that occluder would punch a
/// hole through to the wallpaper. It is also the only circle on the row,
/// which is what stops it reading as a task button.
///
/// The bar always draws the plain ring here, whatever Oracle-Eyes is doing:
/// the live eye is [`eye_view`], on a surface of its own laid over this one,
/// so a capture that leaves that surface out shows this ring, unchanging.
fn launcher_button() -> Element<'static, Message, Theme> {
    let mark = parts::ring(bar::EYE_DISC, bar::RING, color::ACCENT);

    button(container(mark).center(Length::Fill))
        .width(Length::Fixed(bar::TASK_MIN))
        .height(Length::Fixed(bar::TASK_H))
        .padding(0)
        .style(cell_style(Color::TRANSPARENT, Color::TRANSPARENT))
        .on_press(Message::Launch)
        .into()
}

/// Oracle-Eyes' status (ADR 0055), on the eye's own surface over the
/// launcher mark. While the daemon watches, the corona thickens into an iris
/// and the hole becomes a pupil that darts about; while it thinks, the pupil
/// narrows to a point.
///
/// The surface is the launcher button's box and the disc is centred in it
/// exactly as the ring is centred in the button, so the iris covers the ring
/// edge for edge. The hole is a real hole — the gold is one even-odd path and
/// the surface is transparent — and since the pupil never reaches past the
/// ring's inner edge (`EYE_PUPIL + EYE_WANDER`), what shows through it is the
/// bar's glass, never the ring's gold.
fn eye_view(iris: &crate::eye::Iris) -> Element<'static, Message, Theme> {
    let mark = canvas(EyeMark {
        pupil: iris.pupil,
        offset: iris.offset,
    })
    .width(Length::Fixed(bar::EYE_DISC))
    .height(Length::Fixed(bar::EYE_DISC));
    // The surface is sized to the button (`eye_placement`), so filling it is
    // the button's own centring.
    container(mark).center(Length::Fill).into()
}

/// The eye, one frame of it: a gold disc with the pupil cut out of it.
struct EyeMark {
    pupil: f32,
    offset: (f32, f32),
}

impl canvas::Program<Message> for EyeMark {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: iced::Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let centre = frame.center();
        let pupil = iced::Point::new(centre.x + self.offset.0, centre.y + self.offset.1);
        let path = canvas::Path::new(|b| {
            b.circle(centre, crate::eye::outer());
            b.circle(pupil, self.pupil);
        });
        frame.fill(
            &path,
            canvas::Fill {
                style: canvas::Style::Solid(color::ACCENT),
                rule: canvas::fill::Rule::EvenOdd,
            },
        );
        vec![frame.into_geometry()]
    }
}

// ------------------------------------------------------------------ pager

/// One tile per workspace, in wire order. Plasma's pager: the current desktop
/// is accented, an occupied one is outlined, an empty one is dim.
///
/// The current desktop is the bar's *second* yellow, by explicit direction:
/// "which desktop am I on" is live state of the same kind as "which window
/// has focus", and marking one and not the other reads as an oversight. The
/// two never compete for area — the tile is 24px, the chip is a strip — and
/// nothing else on the row is allowed to join them.
fn pager<'a>(app: &'a crate::app::App, on: &crate::app::Bar) -> Element<'a, Message, Theme> {
    let mut r = Row::new().spacing(bar::GAP).align_y(Alignment::Center);
    for ws in live_workspaces(&app.snapshot, on.output_id) {
        r = r.push(tile(ws));
    }
    r.into()
}

/// The workspaces worth drawing: the ones with something on them, plus the
/// one the human is standing on even when it is empty.
///
/// Ten tiles of which seven are permanently dim is a ruler, not a pager — it
/// says nothing and costs the task strip a quarter of its room. A workspace
/// whose windows are all minimized still counts: the windows are still there,
/// which is why `snapshot.windows` is consulted and not only the wire's own
/// count, whose meaning is the compositor's business and may not include
/// them.
pub fn live_workspaces(snapshot: &Snapshot, output: u64) -> impl Iterator<Item = &Workspace> {
    snapshot
        .workspaces
        .iter()
        .filter(move |ws| output == 0 || ws.output == output)
        .filter(move |ws| {
            ws.active || ws.windows > 0 || windows_on(snapshot, output).any(|w| w.workspace == Some(ws.index))
        })
}

/// The windows this bar is allowed to speak for.
///
/// One `hyperion` runs per monitor and the compositor's lists are the whole
/// desktop's, so every live list on the row is filtered here first. `output ==
/// 0` is the bar that was started without `--output` — a single-output dev run
/// — and filters nothing, because on one monitor "every window" and "my
/// windows" are the same set.
fn windows_on(snapshot: &Snapshot, output: u64) -> impl Iterator<Item = &Window> {
    snapshot
        .windows
        .iter()
        .filter(move |w| output == 0 || w.output == Some(output))
}

/// The workspace the human is standing on *on this output*.
///
/// Workspace indices are 1-based per output, so the unfiltered `find` used to
/// be able to answer with another monitor's row — and then this bar's task
/// strip drew that monitor's windows.
fn active_workspace(snapshot: &Snapshot, output: u64) -> Option<usize> {
    snapshot
        .workspaces
        .iter()
        .filter(|w| output == 0 || w.output == output)
        .find(|w| w.active)
        .map(|w| w.index)
}

fn tile(ws: &Workspace) -> Element<'_, Message, Theme> {
    let ws_active = ws.active;
    let (fill, border, tint) = if ws.active {
        (color::ACCENT_FILL, color::ACCENT_BORDER, color::ACCENT_TEXT)
    } else if ws.windows > 0 {
        (Color::TRANSPARENT, color::BORDER, color::TEXT_SECONDARY)
    } else {
        (Color::TRANSPARENT, color::HAIRLINE, color::TEXT_TERTIARY)
    };
    let face = if ws.active { font::DATA_MEDIUM } else { font::DATA };
    let label = text(ws.index.to_string())
        .size(size::MICRO)
        .font(face)
        .color(tint);

    button(container(label).center(Length::Fill))
        .width(Length::Fixed(bar::PAGER_W))
        .height(Length::Fixed(bar::PAGER_H))
        .padding(0)
        .style(move |_t: &Theme, status: button::Status| {
            let background = match status {
                button::Status::Hovered | button::Status::Pressed if !ws_active => color::LIFT_STRONG,
                button::Status::Hovered | button::Status::Pressed => color::ACCENT_FILL_STRONG,
                _ => fill,
            };
            button::Style {
                background: Some(iced::Background::Color(background)),
                text_color: tint,
                border: iced::Border {
                    color: border,
                    width: bar::HAIRLINE,
                    radius: bar::RADIUS_TILE.into(),
                },
                ..button::Style::default()
            }
        })
        .on_press(Message::Switch(ws.index))
        .into()
}

// ------------------------------------------------------------------- tasks

/// The windows the strip speaks for, in strip order: this output's, on the
/// workspace the human is standing on. What [`crate::layout::solve`] is
/// given as its chips, and what [`crate::motion::Bar`] retargets against.
pub(crate) fn strip_windows<'a>(app: &'a crate::app::App, on: &crate::app::Bar) -> Vec<&'a Window> {
    let snapshot = &app.snapshot;
    let active = active_workspace(snapshot, on.output_id);
    windows_on(snapshot, on.output_id)
        .filter(|w| active.is_none() || w.workspace == active)
        .collect()
}

/// The workspace the strip is drawing; a change lands the chips at once
/// rather than animating one desktop's windows into another's.
pub(crate) fn strip_workspace(app: &crate::app::App, on: &crate::app::Bar) -> Option<usize> {
    active_workspace(&app.snapshot, on.output_id)
}

/// The windows on the focused workspace — the bar's hero zone.
///
/// Click brings a window forward, sends the focused one away and brings a
/// minimized one back; middle-click closes, right-click is the menu.
/// Windows are *not* grouped by `app_id`: grouping means a popup list for the
/// group, which means a second surface and a second focus path.
///
/// Every chip is drawn at the width its animation has reached, including
/// the ghosts of windows that just closed, and each carries its own trailing
/// gap scaled by its presence, so a chip arriving or leaving pushes its
/// neighbours along smoothly. The strip clips: mid-flight, a growing chip
/// and a shrinking one may briefly sum past the room, and that overlap must
/// fall under the widgets' edge rather than over it.
fn tasks<'a>(app: &'a crate::app::App, on: &'a crate::app::Bar) -> Element<'a, Message, Theme> {
    let mut r = Row::new().align_y(Alignment::Center);
    for chip in &on.motion.chips {
        let visible = chip.visible();
        if visible <= 0.0 && chip.presence.value() <= 0.0 {
            continue;
        }
        let presence = chip.presence.value().clamp(0.0, 1.0);
        r = r
            .push(task_chip(
                chip,
                app.icons.for_window(&chip.window),
                visible,
                focused(app, &chip.window),
            ))
            .push(Space::new().width(Length::Fixed((bar::GAP * presence).round())));
    }
    if on.layout.hidden > 0 {
        r = r.push(overflow_cell(on.layout.hidden));
    }
    container(r)
        .width(Length::Fill)
        .align_x(Alignment::Start)
        .clip(true)
        .into()
}

/// Where the task strip's first chip begins, in surface-local pixels: the
/// solver's `lead`.
pub(crate) fn strip_left(app: &crate::app::App, on: &crate::app::Bar) -> f32 {
    let count = live_workspaces(&app.snapshot, on.output_id).count() as f32;
    let pager = (count * bar::PAGER_W + (count - 1.0).max(0.0) * bar::GAP).max(0.0);
    bar::EDGE + bar::TASK_MIN + bar::ZONE_GAP + pager + bar::ZONE_GAP
}

/// The horizontal span of the chip a window is drawn as: `(left, right)`,
/// from the solver's targets — where the chip is going, which is where a
/// popup should hang.
///
/// `None` when the window is not on the strip at all — it is on another
/// workspace, or it fell past the end into the `+N` cell — in which case there
/// is no cell to hang a popup under and the caller falls back to the pointer.
pub fn chip_span(app: &crate::app::App, on: &crate::app::Bar, handle: u64) -> Option<(f32, f32)> {
    let index = strip_windows(app, on).iter().position(|w| w.handle == handle)?;
    let c = on.layout.chips.get(index)?;
    Some((c.x, c.x + c.width))
}

/// The horizontal span of a widget's glass, `(left, right)`; `None` when it
/// is not on the bar.
pub fn widget_span(
    app: &crate::app::App,
    on: &crate::app::Bar,
    id: &crate::widgets::WidgetId,
) -> Option<(f32, f32)> {
    let i = app.widget_cfg.order.iter().position(|w| w == id)?;
    let w = on.layout.widgets.get(i)?;
    (w.width > 0.0).then_some((w.x, w.x + w.width))
}

/// The span a drawer hangs from: the network or bluetooth widget, or the
/// tray's disclosure arrow at the right end of its core.
pub fn drawer_span(
    app: &crate::app::App,
    on: &crate::app::Bar,
    drawer: crate::app::Drawer,
) -> Option<(f32, f32)> {
    use crate::widgets::WidgetId;
    match drawer {
        crate::app::Drawer::Network => widget_span(app, on, &WidgetId::Network),
        crate::app::Drawer::Bluetooth => widget_span(app, on, &WidgetId::Bluetooth),
        crate::app::Drawer::Overflow => {
            let (_, right) = widget_span(app, on, &WidgetId::Tray)?;
            let right = right - bar::WIDGET_X;
            Some((right - crate::widgets::tray::ARROW_FROM_RIGHT, right))
        }
    }
}

/// The span a tray item's menu hangs from: its own mark when it is pinned on
/// the bar, else the disclosure arrow its overflow drawer hung from. The
/// pointer is no answer here: it is only tracked over the bar, so a right
/// click in the drawer would read wherever it last crossed the bar.
pub fn tray_item_span(app: &crate::app::App, on: &crate::app::Bar, address: &str) -> Option<(f32, f32)> {
    let (pinned, _) = crate::widgets::tray::split(&app.radios.tray, &app.tray);
    let Some(k) = pinned
        .iter()
        .position(|&i| app.radios.tray.get(i).is_some_and(|t| t.address == address))
    else {
        return drawer_span(app, on, crate::app::Drawer::Overflow);
    };
    let (_, right) = widget_span(app, on, &crate::widgets::WidgetId::Tray)?;
    let pitch = bar::TRAY_MARK_W + bar::TRAY_GAP;
    let core_left = right - bar::WIDGET_X - bar::ARROW_W - pinned.len() as f32 * pitch;
    let left = core_left + k as f32 * pitch;
    Some((left, left + bar::TRAY_MARK_W))
}

/// The tail of a strip that ran out of room: `+3`, in the neutral ink.
///
/// It is not a chip — no icon, no accent, no press — because it stands for
/// windows rather than being one. Dropping the surplus silently was never an
/// option: a taskbar that hides windows without saying so is a taskbar that
/// loses them.
fn overflow_cell(hidden: usize) -> Element<'static, Message, Theme> {
    container(
        text(format!("+{hidden}"))
            .size(size::MICRO)
            .font(font::DATA)
            .color(color::TEXT_TERTIARY),
    )
    .width(Length::Fixed(bar::OVERFLOW_W))
    .height(Length::Fixed(bar::TASK_H))
    .align_x(Alignment::Center)
    .align_y(Alignment::Center)
    .into()
}

/// One window chip: a glass cell (`theme::bar_cell`, the ground every widget
/// shares) `visible` wide over a face laid out once at the chip's target
/// width, so an animating chip uncovers or covers its label and never
/// re-wraps it.
/// Whether `w` holds focus: `get_focused` when it answered, else the
/// window's own flag.
fn focused(app: &crate::app::App, w: &crate::model::Window) -> bool {
    app.snapshot.focused.map_or(w.focused, |f| f == w.handle)
}

fn task_chip(
    chip: &crate::motion::Chip,
    icon: Icon,
    visible: f32,
    focused: bool,
) -> Element<'_, Message, Theme> {
    let w = &chip.window;
    let width = chip.content;
    // Up or put away — see the accent ledger. A minimized window reads as a
    // chip with no state on it at all, which is the point: it is a placeholder
    // for something that is not on the screen.
    let up = !w.minimized;
    // Ink leads the glass: an arriving chip's face shows once there is room
    // to read it, a closing one's is gone before the edge reaches a glyph.
    let presence = chip.presence.value().clamp(0.0, 1.0);
    let ink = parts::lead(presence);
    let swap = chip.swap.value().clamp(0.0, 1.0);
    let natural = width.max(visible).max(chip.prev.unwrap_or(0.0));
    let body: Element<'_, Message, Theme> = match chip.prev {
        // A rung change in flight: the old face fades through to the new.
        // Each takes its ink through `lead`, so the two are never both
        // legible at once — two labels on one gridline read as one garbled
        // word — and the crossover is a brief quiet, not a double exposure.
        Some(prev) if swap < 1.0 => iced::widget::stack![
            chip_face(w, icon.clone(), prev, visible, up, ink * parts::lead(1.0 - swap)),
            chip_face(w, icon, width, visible, up, ink * parts::lead(swap)),
        ]
        .into(),
        _ => chip_face(w, icon, width, visible, up, ink),
    };
    let body = container(body)
        .width(Length::Fixed(natural))
        .height(Length::Fixed(bar::TASK_H));
    let width = natural;
    let press = button(body)
        .width(Length::Fixed(width))
        .height(Length::Fixed(bar::TASK_H))
        .padding(0)
        .style(crate::widgets::bare);
    // A closing chip is a ghost: drawn, not pressable.
    let face: Element<'_, Message, Theme> = if chip.gone {
        press.into()
    } else {
        // The chip is a toggle: click the focused window to send it away,
        // click one that is away to bring it back. A window that is up but
        // behind another is brought forward first — sending away the window
        // you reached for is the one answer that is never wanted. `button`
        // swallows only the left press, so the middle-click close and the
        // right-click menu still reach the `mouse_area` around it.
        let click = if up && !focused {
            Message::Focus(w.handle)
        } else {
            Message::ToggleMinimize(w.handle)
        };
        mouse_area(press.on_press(click))
            .on_middle_press(Message::Close(w.handle))
            .on_right_press(Message::Menu(w.handle))
            .into()
    };
    parts::glass_cell_faded(face, width, visible, up, ClipEdge::Left, presence)
}

/// One face of a window chip, laid out at the content width `width` and drawn
/// at `alpha` of its ink.
///
/// A labelled face is laid out once at its content width and read from a
/// left gridline, so the moving edge uncovers or covers it and never
/// re-wraps it. An iconic face has no text to reflow, so it centres in the
/// width on screen (`visible`) and glides with the edge instead of jumping to
/// the centre of where the chip is going.
fn chip_face(
    w: &Window,
    icon: Icon,
    width: f32,
    visible: f32,
    up: bool,
    alpha: f32,
) -> Element<'_, Message, Theme> {
    let (tint, face) = if up {
        (color::ACCENT_TEXT, font::UI_MEDIUM)
    } else {
        (color::TEXT_SECONDARY, font::UI)
    };
    // The rung is this chip's own business: the width it got buys a rung,
    // and the words this window wants to say decide whether it can use it.
    let detail = chip_detail(w.label(), w.name(), width);
    let mut face_row = Row::new().spacing(bar::GAP + bar::GAP).align_y(Alignment::Center);
    if detail != Detail::Bare {
        face_row = face_row.push(icon_view(icon, alpha));
    }
    // The second rung says the application, the first says the document: three
    // terminals condense to three `kitty`s rather than three copies of the
    // same truncated path. Neither is ever cut — `rung` already promised that
    // whichever of the two is chosen fits whole.
    let label = match detail {
        Detail::Full => Some(w.label()),
        Detail::Name => Some(w.name()),
        Detail::Icon | Detail::Bare => None,
    };
    if let Some(label) = label {
        face_row = face_row.push(
            text(label)
                .size(size::BODY_SMALL)
                .color(tint.scale_alpha(alpha))
                .font(face)
                // `whole_width` is an estimate: a label a hair wider than it
                // runs under the glass's clip rather than folding in two.
                .wrapping(iced::widget::text::Wrapping::None),
        );
    }
    // Labelled chips are read from a left gridline; iconic ones are marks and
    // centre, which is what keeps a row of them from looking like a row of
    // chips that lost their words.
    let (align, pad, laid) = if label.is_some() {
        (Alignment::Start, bar::CELL_X, width)
    } else {
        (Alignment::Center, 0.0, visible)
    };
    container(face_row)
        .width(Length::Fixed(laid))
        .height(Length::Fixed(bar::TASK_H))
        .align_x(align)
        .align_y(Alignment::Center)
        .padding([0.0, pad])
        .into()
}

// -------------------------------------------------------------- context menu

/// The menu's width. Fixed, because a popup's size is given to the compositor
/// when the surface is created and cannot be negotiated from the layout pass.
pub const MENU_W: u32 = menu::W as u32;

/// The popup size that fits `items`. `app::open_menu` asks for this and the
/// view must agree with it exactly, or the menu clips its own last line.
///
/// It takes the items and not a count because the separator above `Close` is
/// height too: a menu whose destructive row is fenced off is nine pixels
/// taller than one whose rows are all alike, and only the items know which
/// this is.
pub fn menu_height(items: &[crate::app::Item]) -> u32 {
    let rows = items.len() as f32 * menu::ROW_H;
    let rules = separators(items) as f32 * menu::SEP_H;
    (rows + rules + 2.0 * menu::PAD).ceil() as u32
}

/// How many separators [`context_menu`] will draw. One, above `Close`, unless
/// `Close` is the only thing on the menu and has nothing to be fenced from.
fn separators(items: &[crate::app::Item]) -> usize {
    let close = items.iter().position(|i| matches!(i, crate::app::Item::Close));
    usize::from(matches!(close, Some(at) if at > 0))
}

/// The right-click menu: one verb per line, each led by a mark, and the
/// destructive one fenced off below a hairline.
///
/// Everything on it is a thing that can be done to *this* window, and the
/// wording is the action and not the state — "Minimize" sends it away,
/// "Unminimize" brings it back, so the line says what the click will do.
///
/// ## Shape
///
/// A menu is not a pane and has no hero; what it has instead is a **mark
/// rail** — every row leads with a 13px stand-in mark on one left-hand
/// gridline, so the list is scanned down the marks and not read word by word.
/// That rail plus the hairline above `Close` are the whole of its structure,
/// and they are what keep six identical rows from being six identical rows:
/// the verbs are a lit group, the separator is air, and `Close` is alone and
/// in `DANGER` beneath it.
///
/// ## The accent ledger
///
/// **Nothing here is yellow.** On the bar, yellow means a window is up
/// (`bar_row`'s ledger); a menu shows no window's state, only offers to change
/// it, so spending the accent on a hover or a heading would be decoration —
/// exactly what the style spec forbids. The pointer is answered with a white
/// lift, `Close` with a red one, and that is all the colour the menu owns.
fn context_menu(handle: u64, items: &[crate::app::Item], radius: f32) -> Element<'static, Message, Theme> {
    let mut rows = Column::new();
    for item in items {
        if matches!(item, crate::app::Item::Close) && separators(items) > 0 {
            rows = rows.push(parts::menu_separator());
        }
        rows = rows.push(menu_row(*item, handle));
    }
    // The same lit glass as any other floating surface: the menu is a sheet
    // over the wallpaper, so it gets the top edge highlight that tells the eye
    // it is lying on the blur rather than cut out of it.
    parts::lit(
        container(rows)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(menu::PAD)
            .style(theme::menu_surface(radius)),
        radius,
        color::HIGHLIGHT,
    )
}

/// One verb: mark, label, and a ground that answers the pointer.
///
/// The hover is the point of the rework — a menu row that does not light up
/// under the pointer reads as a picture of a menu. It is a full-bleed rounded
/// ground rather than a border, because a border appearing and disappearing
/// under the pointer makes the text jump.
fn menu_row(item: crate::app::Item, handle: u64) -> Element<'static, Message, Theme> {
    use crate::app::Item;
    let (label, message) = match item {
        Item::Close => ("Close", Message::Close(handle)),
        Item::Minimize => ("Minimize", Message::ToggleMinimize(handle)),
        Item::Unminimize => ("Unminimize", Message::ToggleMinimize(handle)),
        Item::Mute => ("Mute", Message::Mute(handle, true)),
        Item::Unmute => ("Unmute", Message::Mute(handle, false)),
        Item::NewInstance => ("Open in new window", Message::NewInstance(handle)),
    };
    let destructive = matches!(item, Item::Close);
    let tint = if destructive { color::DANGER } else { color::TEXT };

    let face = container(
        row![
            menu_mark(item),
            text(label)
                .size(size::BODY_SMALL)
                .color(tint)
                .font(font::UI_MEDIUM),
        ]
        .spacing(menu::MARK_GAP)
        .align_y(Alignment::Center),
    )
    .height(Length::Fill)
    .align_y(Alignment::Center)
    .padding([0.0, menu::ROW_X]);

    button(face)
        .width(Length::Fill)
        .height(Length::Fixed(menu::ROW_H))
        .padding(0)
        .style(menu_row_style(destructive))
        .on_press(message)
        .into()
}

/// A menu row's ground. Borderless at rest — the rows are one group inside the
/// menu's own edge and do not each need an outline — and a flat lift under the
/// pointer, in red on the row that destroys something.
fn menu_row_style(destructive: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_t: &Theme, status: button::Status| {
        let (hover, press) = if destructive {
            (color::DANGER_FILL, color::DANGER)
        } else {
            (color::LIFT, color::LIFT_STRONG)
        };
        let background = match status {
            button::Status::Hovered => hover,
            button::Status::Pressed => press,
            _ => Color::TRANSPARENT,
        };
        button::Style {
            background: Some(iced::Background::Color(background)),
            text_color: color::TEXT,
            border: iced::border::rounded(menu::RADIUS_ROW),
            ..button::Style::default()
        }
    }
}

/// The mark that leads a menu row.
///
/// **Real icons, not stand-in rects.** Each verb is named by its freedesktop
/// action name (`window-minimize`, `audio-volume-muted`, …) and drawn from the
/// system icon theme's *symbolic* set, tinted to the menu's ink by
/// [`parts::glyph`] — the same source the bar already uses for application
/// icons, so the menu and the chips it belongs to are drawn from one place.
/// A host missing one of these standard names gets the placeholder square
/// rather than a hole in the rail.
///
/// The lookup is a filesystem walk, but a cached one (`freedesktop_icons`
/// keeps its own cache) over six fixed names, and a context menu is built at
/// most once per right-click.
fn menu_mark(item: crate::app::Item) -> Element<'static, Message, Theme> {
    use crate::app::Item;
    let (name, tint) = match item {
        Item::Minimize => ("window-minimize", color::TEXT_SECONDARY),
        Item::Unminimize => ("window-restore", color::TEXT_SECONDARY),
        Item::Mute => ("audio-volume-muted", color::TEXT_SECONDARY),
        Item::Unmute => ("audio-volume-high", color::TEXT_SECONDARY),
        Item::NewInstance => ("window-new", color::TEXT_SECONDARY),
        // The one red mark on the menu, matching the one red label.
        Item::Close => ("window-close", color::DANGER),
    };
    let cell = Length::Fixed(menu::MARK);
    container(parts::mark(crate::icons::symbolic(name), menu::MARK, tint))
        .width(cell)
        .height(cell)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .into()
}

/// The icon square. A miss draws the placeholder rather than a hole, and a
/// `secret` window is handed [`Icon::Placeholder`] by `Icons::for_window`
/// before it ever gets here.
fn icon_view(icon: Icon, alpha: f32) -> Element<'static, Message, Theme> {
    let square = Length::Fixed(size::ICON);
    match icon {
        Icon::Svg(path) => svg(svg::Handle::from_path(path))
            .width(square)
            .height(square)
            .opacity(alpha)
            .into(),
        Icon::Raster(path) => image(image::Handle::from_path(path))
            .width(square)
            .height(square)
            .opacity(alpha)
            .into(),
        // A rounded square on the reference panes' 6px-on-18px placeholder
        // scale, so a missing icon still reads as a member of this row.
        Icon::Placeholder => container(Space::new())
            .width(square)
            .height(square)
            .style(move |_t: &Theme| container::Style {
                background: Some(iced::Background::Color(color::CONTROL_OFF.scale_alpha(alpha))),
                border: iced::Border {
                    color: color::BORDER.scale_alpha(alpha),
                    width: bar::HAIRLINE,
                    radius: bar::RADIUS_ICON.into(),
                },
                ..container::Style::default()
            })
            .into(),
    }
}

/// Cut a label to `max` characters, on a character boundary, with an
/// ellipsis. Never logs, never allocates more than the result.
pub(crate) fn elide(label: &str, max: usize) -> String {
    if label.chars().count() <= max {
        return label.to_owned();
    }
    let mut out: String = label.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

// ----------------------------------------------------------------- drawers

/// A drawer's width. Fixed for the same reason the menu's is: a popup's pixel
/// size is handed to the compositor when the surface is created and cannot be
/// renegotiated from the layout pass.
pub const DRAWER_W: u32 = drawer::W as u32;

/// A drawer under construction: its head, its rows, and the height they add
/// up to, kept together so the arithmetic that sizes the popup and the column
/// that fills it are one computation and cannot disagree.
pub(crate) struct Sheet {
    head: Element<'static, Message, Theme>,
    rows: Vec<Element<'static, Message, Theme>>,
    height: f32,
}

impl Sheet {
    pub(crate) fn new(head: Element<'static, Message, Theme>, head_h: f32) -> Self {
        Self {
            head,
            rows: Vec::new(),
            height: head_h + 2.0 * drawer::PAD,
        }
    }

    pub(crate) fn row(&mut self, row: Element<'static, Message, Theme>) {
        self.rows.push(row);
        self.height += drawer::ROW_H;
    }

    pub(crate) fn current(&mut self, row: Element<'static, Message, Theme>) {
        self.rows.push(row);
        self.height += drawer::CURRENT_H;
    }

    pub(crate) fn rule(&mut self) {
        self.rows.push(parts::menu_separator());
        self.height += menu::SEP_H;
    }
}

/// The popup height `which` needs right now. `app::open_drawer` sizes the
/// surface from this and `app::reflow` reopens it when it changes; both read
/// the same [`Sheet`] the view draws.
pub fn drawer_height(app: &crate::app::App, which: crate::app::Drawer) -> u32 {
    sheet(app, which).height.ceil() as u32
}

fn sheet(app: &crate::app::App, which: crate::app::Drawer) -> Sheet {
    match which {
        crate::app::Drawer::Network => crate::widgets::network::sheet(app),
        crate::app::Drawer::Bluetooth => crate::widgets::bluetooth::sheet(app),
        crate::app::Drawer::Overflow => crate::widgets::tray::sheet(app),
    }
}

/// A tray drawer.
///
/// ## Shape
///
/// Not a menu and not a pane. The two radio drawers share one anatomy, top to
/// bottom, and no two neighbours in it share a silhouette:
///
/// 1. **Switch head** — the micro label and the radio's toggle, on a strip
///    taller than a row.
/// 2. **Hero** — what the radio is on *now*, two lines tall with a larger
///    mark and its own Disconnect, so it cannot be read as the first item of
///    the list under it.
/// 3. **Choices** — a flat list of names, one line each, nothing on them but
///    a lock where joining needs a secret. No strength, band or rate: those
///    are Settings → Network's, and a list that ranks itself by signal is a
///    list that reorders under the pointer.
/// 4. **The way out** — a quiet link to the full pane, behind a hairline.
///
/// The overflow drawer is the plain heading over a mark rail of whatever the
/// tray did not pin, each with its reading at the right.
///
/// ## The accent ledger
///
/// - **Wi-fi, bluetooth:** the one yellow is the switch's lit track — the
///   radio being on is the drawer's live state. Whatever it is connected to
///   is [`color::CONNECTED`] blue, and never the accent. The drawers draw
///   their own glyphs through `widgets::network::glyph` rather than the bar's
///   `network_mark` and `bluetooth_mark`, which are the white ink ramp: on
///   the bar a link is a state, not a live value, and the widgets' one
///   yellow belongs to the visualizer.
/// - **Overflow:** no yellow at all. It is a shelf of other applets' doors;
///   the state behind each one is that applet's drawer's to colour.
fn drawer_view(app: &crate::app::App, which: crate::app::Drawer) -> Element<'static, Message, Theme> {
    let Sheet { head, rows, .. } = sheet(app, which);
    parts::drawer_frame(app.menu_radius, head, rows)
}

/// The empty square a list row keeps where its mark would be, so its name
/// sits on the same gridline as the rows that do have one.
pub(crate) fn blank_mark() -> Element<'static, Message, Theme> {
    Space::new()
        .width(Length::Fixed(drawer::MARK))
        .height(Length::Fixed(drawer::MARK))
        .into()
}

/// A tray item's own menu: its entries, in its order, on the context menu's
/// glass. The same rows and the same zero-yellow ledger as [`context_menu`]
/// — it is a menu, and a second menu style on one bar would be two.
fn tray_menu(id: &str, entries: &[crate::radio::MenuEntry], radius: f32) -> Element<'static, Message, Theme> {
    use eclipse_services::tray::MenuKind;
    // One checkable entry gives every item row the mark column, so the labels
    // stay on one gridline whether or not a given row is ticked.
    let marks = entries.iter().any(|e| e.checked.is_some());
    let mut rows = Column::new();
    for e in entries {
        rows = rows.push(match e.kind {
            MenuKind::Separator => parts::menu_separator(),
            // A submenu's title: the drawers' micro heading, never a button,
            // so it cannot lift under the pointer and look pressable.
            MenuKind::Header => container(parts::micro_label(&e.label))
                .height(Length::Fixed(menu::ROW_H))
                .align_y(Alignment::Center)
                .padding([0.0, menu::ROW_X])
                .into(),
            MenuKind::Item => tray_menu_row(id, e, marks),
        });
    }
    parts::lit(
        container(rows)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(menu::PAD)
            .style(theme::menu_surface(radius)),
        radius,
        color::HIGHLIGHT,
    )
}

/// One pressable line of a tray menu, with its tick column when the menu has
/// one. Zero yellow: an on checkbox is state, not the one live value.
fn tray_menu_row(id: &str, e: &crate::radio::MenuEntry, marks: bool) -> Element<'static, Message, Theme> {
    let ink = if e.enabled {
        color::TEXT
    } else {
        color::TEXT_TERTIARY
    };
    let mut line = Row::new().spacing(menu::MARK_GAP).align_y(Alignment::Center);
    if marks {
        line = line.push(tick(e.checked, ink));
    }
    line = line.push(
        text(e.label.clone())
            .size(size::BODY_SMALL)
            .font(font::UI_MEDIUM)
            .color(ink),
    );
    let face = container(line)
        .height(Length::Fill)
        .align_y(Alignment::Center)
        .padding([0.0, menu::ROW_X]);
    let b = button(face)
        .width(Length::Fill)
        .height(Length::Fixed(menu::ROW_H))
        .padding(0)
        .style(menu_row_style(false));
    if e.enabled {
        b.on_press(Message::TrayMenuClick(id.to_owned(), e.id)).into()
    } else {
        b.into()
    }
}

/// A row's [`menu::MARK`] cell: a hard square of the row's ink when on, the
/// same square as a hairline frame when off, and empty on a row that is not
/// checkable at all — it only holds the gridline. Drawn,
/// not an icon-theme glyph: a missing `object-select` would fall back to the
/// placeholder square, and "no icon" and "on" must not look alike.
fn tick(checked: Option<bool>, ink: Color) -> Element<'static, Message, Theme> {
    let cell = Length::Fixed(menu::MARK);
    let inner = menu::MARK_INNER;
    let body: Element<'static, Message, Theme> = match checked {
        Some(true) => parts::edge_quad(Length::Fixed(inner), Length::Fixed(inner), ink),
        Some(false) => parts::outline(
            inner,
            inner,
            space::HAIRLINE,
            color::TEXT_TERTIARY,
            menu::RADIUS_TICK,
        ),
        None => Space::new().into(),
    };
    container(body)
        .width(cell)
        .height(cell)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .into()
}

/// The popup height that fits a tray item's menu, by the same arithmetic
/// [`tray_menu`] draws: a rule is [`menu::SEP_H`], every other line a row.
pub fn tray_menu_height(entries: &[crate::radio::MenuEntry]) -> u32 {
    use eclipse_services::tray::MenuKind;
    let lines: f32 = entries
        .iter()
        .map(|e| match e.kind {
            MenuKind::Separator => menu::SEP_H,
            MenuKind::Item | MenuKind::Header => menu::ROW_H,
        })
        .sum();
    (lines + 2.0 * menu::PAD).ceil() as u32
}

/// How much of a network's name a drawer row will show.
pub(crate) const DRAWER_CHARS: usize = 14;

/// The bar draws on nothing: the layer surface itself is transparent, and the
/// opaque ground is the container in [`view`].
pub fn style(_app: &crate::app::App, theme: &Theme) -> iced::theme::Style {
    iced::theme::Style {
        background_color: Color::TRANSPARENT,
        text_color: theme.palette().text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The popup is sized before layout, so the sum has to be the drawing.
    #[test]
    fn a_tray_menu_is_as_tall_as_its_lines() {
        use eclipse_services::tray::MenuKind;
        let entries = crate::radio::preview_menu();
        let rows = entries.iter().filter(|e| e.kind != MenuKind::Separator).count() as f32;
        let rules = entries.len() as f32 - rows;
        let want = (rows * menu::ROW_H + rules * menu::SEP_H + 2.0 * menu::PAD).ceil() as u32;
        assert_eq!(tray_menu_height(&entries), want);
        assert_eq!(tray_menu_height(&entries), 210);
    }

    fn win(handle: u64, output: u64, workspace: usize, minimized: bool) -> Window {
        Window {
            handle,
            app_id: "kitty".into(),
            title: "t".into(),
            workspace: Some(workspace),
            output: Some(output),
            focused: false,
            minimized,
            pid: None,
            trust: crate::model::Trust::Private,
        }
    }

    fn ws(index: usize, output: u64, active: bool, windows: usize) -> Workspace {
        Workspace {
            index,
            output,
            output_name: String::new(),
            active,
            windows,
        }
    }

    /// A workspace with only minimized windows still has windows on it.
    #[test]
    fn the_pager_keeps_occupied_and_current_workspaces_only() {
        let snapshot = Snapshot {
            workspaces: vec![
                ws(1, 0, true, 0),
                ws(2, 0, false, 0),
                ws(3, 0, false, 2),
                ws(4, 0, false, 0),
            ],
            windows: vec![win(1, 0, 4, true)],
            ..Snapshot::default()
        };
        let live: Vec<usize> = live_workspaces(&snapshot, 0).map(|w| w.index).collect();
        assert_eq!(live, vec![1, 3, 4]);
    }

    /// Each monitor runs its own bar and the compositor answers for all of
    /// them, so a bar that did not filter drew the other monitor's pager —
    /// and, because indices are per-output, drew duplicates of its own.
    #[test]
    fn a_bar_speaks_only_for_its_own_output() {
        let snapshot = Snapshot {
            workspaces: vec![ws(1, 7, false, 1), ws(2, 7, true, 1), ws(1, 9, true, 1)],
            windows: vec![win(1, 7, 1, false), win(2, 7, 2, false), win(3, 9, 1, false)],
            ..Snapshot::default()
        };

        let live: Vec<usize> = live_workspaces(&snapshot, 7).map(|w| w.index).collect();
        assert_eq!(live, vec![1, 2]);
        // The other monitor's active row must not be mistaken for ours.
        assert_eq!(active_workspace(&snapshot, 7), Some(2));
        assert_eq!(active_workspace(&snapshot, 9), Some(1));
        let mine: Vec<u64> = windows_on(&snapshot, 7).map(|w| w.handle).collect();
        assert_eq!(mine, vec![1, 2]);
        // A bar with no --output is a single-monitor dev run: filter nothing.
        assert_eq!(windows_on(&snapshot, 0).count(), 3);
        assert_eq!(live_workspaces(&snapshot, 0).count(), 3);
    }

    #[test]
    fn a_long_title_is_cut_on_a_character_boundary() {
        assert_eq!(elide("short", 18), "short");
        assert_eq!(elide("abcdefghij", 5), "abcd…");
        // Multi-byte in, multi-byte out, and no panic on the boundary.
        assert_eq!(elide("ααααα", 3), "αα…");
    }

    /// The elided title is still the *label*, so a secret window's real title
    /// cannot reach the screen by way of the elide.
    #[test]
    fn eliding_a_secret_window_elides_its_placeholder_title() {
        let mut w = win(1, 0, 1, false);
        w.title = "seed phrase correct horse".into();
        w.trust = crate::model::Trust::Secret;
        let drawn = elide(w.label(), crate::layout::TITLE_CHARS);
        assert!(!drawn.contains("seed"));
        assert!(drawn.starts_with("Protected"));
    }

    /// A popup anchored under a cell hangs from the solver's own answer:
    /// chip *n* sits one width-plus-gap past chip *n-1*, every chip is left
    /// of the widgets, and a window that is not drawn has no span at all.
    #[test]
    fn a_chip_span_follows_the_strip_it_describes() {
        let mut app = crate::app::App::new();
        let mut on = crate::app::Bar::new(
            iced::window::Id::unique(),
            "DP-1".into(),
            0,
            eclipse_ui::motion::Motion::DEFAULT,
        );
        on.width = 1830.0;
        app.snapshot = Snapshot {
            workspaces: vec![ws(1, 0, true, 3)],
            windows: vec![
                win(1, 0, 1, false),
                win(2, 0, 1, false),
                win(3, 0, 1, false),
                win(9, 0, 2, false),
            ],
            ..Snapshot::default()
        };
        crate::app::relayout(&app, &mut on, std::time::Instant::now());

        let first = chip_span(&app, &on, 1).expect("chip 1 is drawn");
        let second = chip_span(&app, &on, 2).expect("chip 2 is drawn");
        assert!((first.0 - strip_left(&app, &on)).abs() < 0.01);
        assert!((second.0 - first.1 - bar::GAP).abs() < 0.01);
        assert!(second.1 < on.width);
        // On another workspace, so it is not on the strip and has no cell.
        assert_eq!(chip_span(&app, &on, 9), None);

        // The clock is measured inward from the right edge and stays there.
        let clock =
            widget_span(&app, &on, &crate::widgets::WidgetId::Clock).expect("the clock is on the bar");
        assert!((clock.1 - (on.width - bar::EDGE)).abs() < 0.01);
        assert!(clock.0 > chip_span(&app, &on, 3).expect("chip 3 is drawn").1);
    }

    /// A tray item's menu hangs from the item, never from wherever the
    /// pointer last crossed the bar: a pinned item from its own mark, left
    /// of the arrow and in pin order; an overflow item from the arrow its
    /// drawer hung from.
    #[test]
    fn a_tray_menu_hangs_from_the_item_or_its_drawer() {
        use crate::radio::{TrayIcon, TrayItem};
        let item = |id: &str| TrayItem {
            id: id.into(),
            address: format!(":1.{id}"),
            title: id.into(),
            icon: TrayIcon::None,
        };
        let mut app = crate::app::App::new();
        app.radios.tray = vec![item("steam"), item("discord"), item("slack")];
        app.tray = crate::conn::TrayConfig {
            pinned: Some(vec!["discord".into(), "steam".into()]),
            hidden: vec![],
        };
        let mut on = crate::app::Bar::new(
            iced::window::Id::unique(),
            "DP-1".into(),
            0,
            eclipse_ui::motion::Motion::DEFAULT,
        );
        on.width = 1830.0;
        on.cursor = iced::Point::ORIGIN;
        crate::app::relayout(&app, &mut on, std::time::Instant::now());

        let arrow = drawer_span(&app, &on, crate::app::Drawer::Overflow).expect("the arrow is drawn");
        let discord = tray_item_span(&app, &on, ":1.discord").expect("discord is pinned");
        let steam = tray_item_span(&app, &on, ":1.steam").expect("steam is pinned");
        assert!((steam.0 - discord.0 - (bar::TRAY_MARK_W + bar::TRAY_GAP)).abs() < 0.01);
        assert!((arrow.0 - steam.1 - bar::TRAY_GAP).abs() < 0.01);
        assert_eq!(tray_item_span(&app, &on, ":1.slack"), Some(arrow));
    }
}
