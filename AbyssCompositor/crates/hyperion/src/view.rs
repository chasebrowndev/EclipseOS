// SPDX-License-Identifier: AGPL-3.0-only
//! The bar's one row — a desktop taskbar, in Plasma's structure and the
//! Eclipse palette.
//!
//! Left to right, five zones with five different silhouettes, so that no two
//! neighbours read as the same block:
//!
//! 1. **launcher button** — one ring, the only circle on the row;
//! 2. **workspace pager** — a run of small square chips, all identical;
//! 3. **task buttons** — the hero: wide icon-led chips, left-aligned and
//!    flowing right, the only zone that changes width;
//! 4. **tray** — compact mark-plus-number readings, the only zone in mono;
//! 5. **clock** — two centred mono lines, the only two-line cell on the bar.
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
//! corona is the desktop's own mark, the one fixed thing on the row, and a
//! white ring read as a disabled button rather than as a logo. It is not live
//! state, so it never competes with the two that are — it never changes.
//!
//! Nothing else on the row is allowed to be yellow: the tray and the clock
//! are white at 1.0 / 0.64 / 0.40 throughout. A low battery may go `DANGER`, which is the
//! one alarm and not an accent.
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

use iced::widget::{button, column, container, image, mouse_area, row, svg, text, Column, Row, Space};
use iced::{Alignment, Color, Element, Length, Theme};

use eclipse_services::status::{Battery, Bluetooth, Charge, Network};
use eclipse_ui::theme;
use eclipse_ui::tokens::{bar, color, drawer, font, menu, size, space};
use eclipse_ui::widget as parts;

use crate::app::Message;
use crate::icons::Icon;
use crate::model::{Snapshot, Window, Workspace};

/// How many characters of a title survive before the ellipsis. The clamp is
/// in characters and not pixels because iced has no eliding text and the row
/// must stay a pure function of the snapshot — a measured elide would depend
/// on the layout pass that has not run yet.
const TITLE_CHARS: usize = 18;

/// How much detail one task chip is showing.
///
/// The ladder the bar walks down as windows multiply. It is chosen from the
/// *width a chip actually got*, never from a window count: "what fits" is the
/// question, and the rung is the answer to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    /// Icon and title — the document.
    Full,
    /// Icon and application name — `kitty`, not the directory it is in.
    Name,
    /// Icon alone.
    Icon,
    /// Not even the icon: a bare tab of accent or grey. The last resort,
    /// before which everything else has already been spent.
    Bare,
}

/// How the strip will draw `count` windows in `avail` pixels.
///
/// Returns the per-chip width, the rung of the ladder that width buys, and
/// how many chips are drawn — fewer than `count` only when even bare tabs
/// will not fit, in which case the remainder becomes a `+N` cell so that the
/// bar overflows *visibly* instead of running off its own end.
///
/// This is the one place the strip's geometry is decided, and it is a pure
/// function so it can be tested without a compositor. It is also the fix for
/// the old `width(Fill).max_width(cap)` chip: iced hands a `Fill` child of a
/// `Row` exact `min == max` limits, so `max_width` on it does nothing at all
/// and the chips ran four times their cap. A fixed width cannot be argued
/// with.
pub fn ladder(avail: f32, count: usize) -> (f32, Detail, usize) {
    debug_assert!(count > 0);
    let n = count as f32;
    let each = ((avail - (n - 1.0) * bar::GAP) / n).floor();
    if each >= bar::TASK_BARE {
        let width = each.min(bar::TASK_MAX);
        let detail = if width >= bar::TASK_FULL {
            Detail::Full
        } else if width >= bar::TASK_NAME {
            Detail::Name
        } else if width >= bar::TASK_MIN {
            Detail::Icon
        } else {
            Detail::Bare
        };
        return (width, detail, count);
    }
    // Past the bottom of the ladder: bare tabs plus a counter for the rest.
    let room = (avail - bar::OVERFLOW_W - bar::GAP + bar::GAP).max(0.0);
    let shown = (room / (bar::TASK_BARE + bar::GAP)).floor().max(0.0) as usize;
    (bar::TASK_BARE, Detail::Bare, shown.min(count))
}

/// The strip's genuine slack, handed out to the chips that can use it.
///
/// `ladder` gives every shown chip the same floor on purpose — that uniform
/// share is what keeps the strip a grid — but dividing `avail` evenly rarely
/// uses every pixel of it, and a title that would have fit whole in that
/// leftover room still dropped to its process name for no reason but that the
/// room went undrawn. This is a second, later pass over that leftover alone:
/// a chip's own floor is never taken from it, only the strip's genuine
/// surplus is up for grabs, so `ladder`'s no-overflow guarantee still holds
/// for the total.
///
/// Several chips can want the same slack at once, so the order they are
/// offered it in has to be fixed rather than incidental: non-minimized
/// windows first (an accent chip earns the room a put-away one has no state
/// left to show off), then left-to-right by original position. Anything else
/// would make which chip grows depend on iteration order or timing.
fn expand(windows: &[&Window], base_width: f32, avail: f32) -> Vec<f32> {
    let shown = windows.len();
    if shown == 0 {
        return Vec::new();
    }
    let gaps = (shown - 1) as f32 * bar::GAP;
    let mut remaining = (avail - shown as f32 * base_width - gaps).max(0.0);

    let mut order: Vec<usize> = (0..shown).collect();
    order.sort_by_key(|&i| (windows[i].minimized, i));

    let mut widths = vec![base_width; shown];
    for i in order {
        if remaining <= 0.0 {
            break;
        }
        let want = (whole_width(windows[i].label()) - base_width).max(0.0);
        let take = want.min(remaining);
        widths[i] += take;
        remaining -= take;
    }
    widths
}

/// The rung one chip can actually draw at, given the rung the strip's width
/// bought and the words this particular window wants to say.
///
/// An ellipsis is worse than no detail: `…/syncedprojec…` spends a chip's whole
/// width to say nothing that the process name would not have said better, and
/// says it less legibly. So the rung is conditional **per chip**, not per bar —
/// detail is drawn only when it fits *whole*, and a window whose path is too
/// long drops to its process name while the chip beside it, whose title is
/// short, keeps its detail. Chips differing in rung is the intended outcome,
/// not a glitch: they still share a width, so the strip stays a grid.
///
/// The process name is the floor for text. If even `kitty` will not fit whole
/// the chip becomes an icon — a clipped word is not a cheaper word, it is a
/// worse one.
pub fn rung(label: &str, name: &str, width: f32, strip: Detail) -> Detail {
    let fits = |s: &str, d: Detail| s.chars().count() <= budget(width, d);
    match strip {
        Detail::Full if fits(label, Detail::Full) => Detail::Full,
        Detail::Full | Detail::Name if fits(name, Detail::Name) => Detail::Name,
        Detail::Full | Detail::Name => Detail::Icon,
        other => other,
    }
}

/// How many characters of label a chip of `width` pixels can hold once the
/// icon and the padding have taken theirs.
fn budget(width: f32, detail: Detail) -> usize {
    let icon = if detail == Detail::Full || detail == Detail::Name {
        size::ICON + bar::GAP + bar::GAP
    } else {
        0.0
    };
    let text = width - icon - 2.0 * bar::CELL_X;
    ((text / bar::CHAR_W).floor().max(0.0) as usize).min(TITLE_CHARS)
}

/// The pixel width a chip needs to show `label` whole, with no [`TITLE_CHARS`]
/// ceiling and no [`bar::TASK_MAX`] cap — the mirror image of [`budget`]'s
/// icon-and-padding arithmetic, run in reverse from a character count instead
/// of a width. `budget` answers "how much text fits a width"; this answers
/// "how much width a label needs", which is the question [`expand`] has to ask
/// before it can hand a chip more than its uniform floor.
fn whole_width(label: &str) -> f32 {
    size::ICON + 2.0 * bar::GAP + 2.0 * bar::CELL_X + label.chars().count() as f32 * bar::CHAR_W
}

/// The bar, or the one popup over it.
///
/// `iced_layershell`'s daemon pattern draws every surface through one function,
/// so the id is the branch: the bar owns exactly one popup at a time and
/// anything that is not it is the row.
pub fn view(app: &crate::app::App, id: iced::window::Id) -> Element<'_, Message, Theme> {
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
    match app.fold.target {
        // Hidden is *gone*, not thin: nothing is drawn, and the surface it
        // still owns claims no exclusive zone, so a fullscreen video has the
        // whole output.
        crate::app::FoldTarget::Hidden => return Space::new().into(),
        // Folded, or mid-slide back out: still the folded strip until there
        // is room for a cell. `pill` is the same test the surface geometry
        // uses, so the view and the surface cannot disagree about which one
        // is up.
        _ if !app.fold.pill() => return folded_row(app),
        _ => {}
    }
    bar_row(app)
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
fn folded_row(app: &crate::app::App) -> Element<'_, Message, Theme> {
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
        .height(Length::Fixed(app.fold.height as f32))
        .into()
}

fn bar_row(app: &crate::app::App) -> Element<'_, Message, Theme> {
    let snapshot = &app.snapshot;

    let bar_row = row![
        launcher_button(),
        pager(app),
        // The task strip is also the row's spacer: it takes exactly the space
        // the fixed zones leave, so tray and clock cannot be pushed off.
        tasks(app),
        tray(app),
        clock(snapshot),
    ]
    .spacing(bar::ZONE_GAP)
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

/// A chip that is the current one: accent ground, accent edge. The single
/// place on the row that is allowed to be yellow.
fn accent_cell_style() -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_t: &Theme, status: button::Status| {
        let lift = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            // At rest the chip carries no ground at all: it can be a third of
            // the bar wide, and any yellow fill at that area stops being a
            // state mark and becomes paint. Focus is the accent *outline* and
            // the accent *word*; the pointer is what earns a wash.
            background: Some(iced::Background::Color(if lift {
                color::ACCENT_WASH
            } else {
                Color::TRANSPARENT
            })),
            text_color: color::ACCENT_TEXT,
            border: iced::Border {
                color: color::ACCENT_BORDER,
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
fn launcher_button() -> Element<'static, Message, Theme> {
    let mark = parts::ring(size::ICON * MARK_DISC, bar::RING, color::ACCENT);

    button(container(mark).center(Length::Fill))
        .width(Length::Fixed(bar::TASK_MIN))
        .height(Length::Fixed(bar::TASK_H))
        .padding(0)
        .style(cell_style(Color::TRANSPARENT, Color::TRANSPARENT))
        .on_press(Message::Launch)
        .into()
}

/// Diameter of the corona ring, as a fraction of the icon square.
const MARK_DISC: f32 = 0.85;

// ------------------------------------------------------------------ pager

/// One tile per workspace, in wire order. Plasma's pager: the current desktop
/// is accented, an occupied one is outlined, an empty one is dim.
///
/// The current desktop is the bar's *second* yellow, by explicit direction:
/// "which desktop am I on" is live state of the same kind as "which window
/// has focus", and marking one and not the other reads as an oversight. The
/// two never compete for area — the tile is 24px, the chip is a strip — and
/// nothing else on the row is allowed to join them.
fn pager(app: &crate::app::App) -> Element<'_, Message, Theme> {
    let mut r = Row::new().spacing(bar::GAP).align_y(Alignment::Center);
    for ws in live_workspaces(&app.snapshot, app.output_id) {
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

/// The windows on the focused workspace — the bar's hero zone.
///
/// Click focuses, middle-click closes, as before. Windows are *not* grouped
/// by `app_id`: grouping means a popup list for the group, which means a
/// second surface and a second focus path, and neither falls out cheaply from
/// a model whose whole content is three flat lists.
fn tasks(app: &crate::app::App) -> Element<'_, Message, Theme> {
    let snapshot = &app.snapshot;
    let active = active_workspace(snapshot, app.output_id);
    let on_workspace: Vec<&Window> = windows_on(snapshot, app.output_id)
        .filter(|w| active.is_none() || w.workspace == active)
        .collect();

    if on_workspace.is_empty() {
        return Space::new().width(Length::Fill).into();
    }

    let avail = strip_room(app);
    let (base_width, detail, shown) = ladder(avail, on_workspace.len());
    let hidden = on_workspace.len() - shown;
    let widths = expand(&on_workspace[..shown], base_width, avail);

    let mut r = Row::new().spacing(bar::GAP).align_y(Alignment::Center);
    for (w, width) in on_workspace.into_iter().take(shown).zip(widths) {
        r = r.push(task_button(w, app.icons.for_window(w), width, detail));
    }
    if hidden > 0 {
        r = r.push(overflow_cell(hidden));
    }
    container(r).width(Length::Fill).align_x(Alignment::Start).into()
}

/// Where the task strip's first chip begins, in surface-local pixels.
///
/// The strip's geometry is already pure arithmetic — [`ladder`] exists so that
/// the chips' widths are decided before layout rather than discovered after
/// it — so the position of any one chip is arithmetic too, and a popup can be
/// anchored under a chip without iced handing back a widget's bounds.
fn strip_left(app: &crate::app::App) -> f32 {
    let count = live_workspaces(&app.snapshot, app.output_id).count() as f32;
    let pager = (count * bar::PAGER_W + (count - 1.0).max(0.0) * bar::GAP).max(0.0);
    bar::EDGE + bar::TASK_MIN + bar::ZONE_GAP + pager + bar::ZONE_GAP
}

/// The horizontal span of the chip a window is drawn as: `(left, right)`.
///
/// `None` when the window is not on the strip at all — it is on another
/// workspace, or it fell past the end into the `+N` cell — in which case there
/// is no cell to hang a popup under and the caller falls back to the pointer.
pub fn chip_span(app: &crate::app::App, handle: u64) -> Option<(f32, f32)> {
    let snapshot = &app.snapshot;
    let active = active_workspace(snapshot, app.output_id);
    let on_workspace: Vec<&Window> = windows_on(snapshot, app.output_id)
        .filter(|w| active.is_none() || w.workspace == active)
        .collect();
    if on_workspace.is_empty() {
        return None;
    }
    let (width, _, shown) = ladder(strip_room(app), on_workspace.len());
    let index = on_workspace.iter().take(shown).position(|w| w.handle == handle)?;
    let left = strip_left(app) + index as f32 * (width + bar::GAP);
    Some((left, left + width))
}

/// The horizontal span of the tray cell a drawer belongs to: `(left, right)`.
///
/// Measured inward from the right edge, because that is how the tray itself is
/// laid out — [`tray_width`] is the same sum the task strip subtracts.
pub fn tray_span(app: &crate::app::App, drawer: crate::app::Drawer) -> Option<(f32, f32)> {
    if app.width <= 0.0 {
        return None;
    }
    let right = app.width - bar::EDGE - bar::CLOCK_W - bar::ZONE_GAP;
    let arrow = (right - bar::ARROW_W, right);
    let wanted = match drawer {
        crate::app::Drawer::Overflow => return Some(arrow),
        crate::app::Drawer::Network => Entry::Network,
        crate::app::Drawer::Bluetooth => Entry::Bluetooth,
    };
    // A drawer whose applet is not pinned opened from the overflow, so it
    // hangs off the arrow that leads to it.
    let (pinned, _) = tray_entries(app);
    let mut left = right - tray_width(app);
    for entry in pinned {
        if entry == wanted {
            return Some((left, left + entry_width(entry)));
        }
        left += entry_width(entry) + bar::TRAY_GAP;
    }
    Some(arrow)
}

/// The pixels the task strip has to itself: the bar, less every zone whose
/// width is fixed and less the air between them.
///
/// Every one of those zones is deliberately a *fixed* width — the tray cells
/// and the clock could each have been intrinsically sized, and then the strip
/// could not know its own room until after layout, which is exactly one pass
/// too late to choose a rung of the ladder with.
///
/// A bar that has not been told its width yet assumes the chips can have
/// their cap; the first frame is the only one that ever runs on that guess.
fn strip_room(app: &crate::app::App) -> f32 {
    let count = live_workspaces(&app.snapshot, app.output_id).count() as f32;
    let pager = (count * bar::PAGER_W + (count - 1.0).max(0.0) * bar::GAP).max(0.0);
    let fixed = bar::TASK_MIN + pager + tray_width(app) + bar::CLOCK_W;
    if app.width <= 0.0 {
        return bar::TASK_MAX * app.snapshot.windows.len().max(1) as f32;
    }
    (app.width - 2.0 * bar::EDGE - 4.0 * bar::ZONE_GAP - fixed).max(0.0)
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

fn task_button<'a>(w: &'a Window, icon: Icon, width: f32, detail: Detail) -> Element<'a, Message, Theme> {
    // Up or put away — see the accent ledger. A minimized window reads as a
    // chip with no state on it at all, which is the point: it is a placeholder
    // for something that is not on the screen.
    let up = !w.minimized;
    let (tint, face) = if up {
        (color::ACCENT_TEXT, font::UI_MEDIUM)
    } else {
        (color::TEXT_SECONDARY, font::UI)
    };
    // The width came from the strip; the rung is this chip's own business —
    // unless `expand` already gave this chip enough room to show its title
    // whole, in which case the ladder's rung (chosen for the *floor* every
    // chip shares) no longer applies to it.
    let detail = if width >= whole_width(w.label()) {
        Detail::Full
    } else {
        rung(w.label(), w.name(), width, detail)
    };
    let mut face_row = Row::new().spacing(bar::GAP + bar::GAP).align_y(Alignment::Center);
    if detail != Detail::Bare {
        face_row = face_row.push(icon_view(icon));
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
        face_row = face_row.push(text(label).size(size::BODY_SMALL).color(tint).font(face));
    }
    // Labelled chips are read from a left gridline; iconic ones are marks and
    // centre, which is what keeps a row of them from looking like a row of
    // chips that lost their words.
    let (align, pad) = if label.is_some() {
        (Alignment::Start, bar::CELL_X)
    } else {
        (Alignment::Center, 0.0)
    };
    let body = container(face_row)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(align)
        .align_y(Alignment::Center)
        .padding([0.0, pad]);
    let cell = button(body)
        .width(Length::Fixed(width))
        .height(Length::Fixed(bar::TASK_H))
        .padding(0)
        // The chip is a toggle: click a window that is up to send it away,
        // click one that is away to bring it back.
        .on_press(Message::ToggleMinimize(w.handle));
    let cell = if up {
        cell.style(accent_cell_style())
    } else {
        cell.style(cell_style(Color::TRANSPARENT, color::BORDER))
    };
    // `button` swallows only the left press, so the middle-click close and the
    // right-click menu still reach the `mouse_area` around it.
    mouse_area(cell)
        .on_middle_press(Message::Close(w.handle))
        .on_right_press(Message::Menu(w.handle))
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
fn icon_view(icon: Icon) -> Element<'static, Message, Theme> {
    let square = Length::Fixed(size::ICON);
    match icon {
        Icon::Svg(path) => svg(svg::Handle::from_path(path))
            .width(square)
            .height(square)
            .into(),
        Icon::Raster(path) => image(image::Handle::from_path(path))
            .width(square)
            .height(square)
            .into(),
        // A rounded square on the reference panes' 6px-on-18px placeholder
        // scale, so a missing icon still reads as a member of this row.
        Icon::Placeholder => container(Space::new())
            .width(square)
            .height(square)
            .style(|_t: &Theme| container::Style {
                background: Some(iced::Background::Color(color::CONTROL_OFF)),
                border: iced::Border {
                    color: color::BORDER,
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

// -------------------------------------------------------------------- tray

/// One thing the tray can show. The built-ins are the status feed's readings;
/// `Item` is a StatusNotifierItem, by its index in `app.radios.tray`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Entry {
    Network,
    Bluetooth,
    Battery,
    Item(usize),
}

/// What the tray pins when `bar.tray.pinned` is unset: the two readings that
/// earned the bar before the tray was configurable. Bluetooth starts in the
/// overflow, where it always lived.
const DEFAULT_PINNED: [&str; 2] = ["network", "battery"];

/// Split every entry the bar can show into `(pinned, overflow)`.
///
/// Pinned follows `bar.tray.pinned` order; the overflow takes what is left in
/// the canonical order (network, bluetooth, battery, then items as they
/// arrived). `bar.tray.hidden` wins over both. An entry with nothing to report
/// — a machine with no battery — is in neither: a pinned id is a wish, not a
/// promise to draw an empty cell.
///
/// `volume` is a valid id in the config and is deliberately absent here: the
/// status feed carries no volume, and a cell that cannot read or set it would
/// be a picture of a control.
pub(crate) fn tray_entries(app: &crate::app::App) -> (Vec<Entry>, Vec<Entry>) {
    let mut all: Vec<(String, Entry)> = vec![
        ("network".to_owned(), Entry::Network),
        ("bluetooth".to_owned(), Entry::Bluetooth),
    ];
    if app.battery.is_some() {
        all.push(("battery".to_owned(), Entry::Battery));
    }
    for (i, item) in app.radios.tray.iter().enumerate() {
        all.push((item.id.clone(), Entry::Item(i)));
    }
    all.retain(|(id, _)| !app.tray.hidden.iter().any(|h| h == id));

    let wanted: Vec<&str> = match &app.tray.pinned {
        Some(ids) => ids.iter().map(String::as_str).collect(),
        None => DEFAULT_PINNED.to_vec(),
    };
    let mut pinned = Vec::new();
    for id in wanted {
        if let Some((_, e)) = all.iter().find(|(have, _)| have == id) {
            if !pinned.contains(e) {
                pinned.push(*e);
            }
        }
    }
    let overflow = all
        .into_iter()
        .map(|(_, e)| e)
        .filter(|e| !pinned.contains(e))
        .collect();
    (pinned, overflow)
}

/// The bar width one pinned entry takes: a mark alone is the narrow cell, the
/// battery's reading the wide one.
fn entry_width(entry: Entry) -> f32 {
    match entry {
        Entry::Battery => bar::TRAY_CELL_W,
        _ => bar::TRAY_MARK_W,
    }
}

/// The pinned tray entries and the disclosure arrow, to the left of the clock.
///
/// Each is a mark plus, where it earns one, a number rather than a bare word:
/// the DE ships no Nerd Font (a glyph would render as a box on the machine it
/// is supposed to inform), so the marks are the icon theme's symbolic art and
/// a drawn battery gauge — both of which show a *magnitude* a word cannot.
/// Which entries are here, and in what order, is `bar.tray.pinned`; the rest
/// sit behind the arrow.
fn tray(app: &crate::app::App) -> Element<'_, Message, Theme> {
    let (pinned, _) = tray_entries(app);
    let mut r = Row::new().spacing(bar::TRAY_GAP).align_y(Alignment::Center);
    for entry in &pinned {
        r = r.push(tray_cell(app, *entry));
    }
    // The arrow closes the tray rather than leading it: reading order runs
    // outward from the task strip — the pinned readings first, then the door
    // to the ones that did not earn permanent space, then the clock at the
    // screen's corner where every desktop puts it. The arrow is set off from
    // the cells by its own lead, not the tray's shoulder-to-shoulder gap, so
    // it sits centred between the last reading and the clock. With nothing
    // pinned it is the tray, alone, and owes no lead to anything.
    if pinned.is_empty() {
        return disclosure();
    }
    row![r, disclosure()]
        .spacing(bar::ARROW_LEAD)
        .align_y(Alignment::Center)
        .into()
}

fn tray_cell(app: &crate::app::App, entry: Entry) -> Element<'_, Message, Theme> {
    match entry {
        Entry::Network => {
            let live = app.network != Network::Offline;
            // No reading beside the cone. A signal percentage is a number no
            // ordinary person acts on, and it ticks: a digit that changes on
            // its own drags the eye to the one corner of the bar that had
            // nothing to report. The cone already carries the magnitude.
            applet(
                network_mark(&app.network, bar::MARK),
                None,
                if live {
                    color::TEXT_SECONDARY
                } else {
                    color::TEXT_TERTIARY
                },
                Some(Message::Open(crate::app::Drawer::Network)),
            )
        }
        Entry::Bluetooth => applet(
            bluetooth_mark(app.bluetooth, bar::MARK),
            None,
            color::TEXT_SECONDARY,
            Some(Message::Open(crate::app::Drawer::Bluetooth)),
        ),
        Entry::Battery => {
            let Some(battery) = app.battery else {
                return Space::new().into();
            };
            // Low and not charging is the one status the human has to act on,
            // so it is the one status allowed to leave the neutral palette.
            let tint = if battery.percent <= LOW && battery.state == Charge::Discharging {
                color::DANGER
            } else {
                color::TEXT_SECONDARY
            };
            applet(
                parts::battery_gauge(battery.percent, tint),
                Some(battery_text(battery)),
                tint,
                None,
            )
        }
        Entry::Item(i) => {
            let Some(item) = app.radios.tray.get(i) else {
                return Space::new().into();
            };
            // An application's tray art is recoloured to the tray's ink like
            // every other mark: a row of vendor-coloured logos is the one
            // place a desktop loses its own palette to its guests.
            mouse_area(applet(
                tray_mark(&item.icon, bar::MARK, color::TEXT_SECONDARY),
                None,
                color::TEXT_SECONDARY,
                Some(Message::TrayActivate(item.address.clone())),
            ))
            .on_right_press(Message::TrayMenu(item.address.clone()))
            .into()
        }
    }
}

/// The width [`tray`] will occupy. Fixed, and known before layout, because
/// the task strip's ladder is arithmetic on what the fixed zones leave. This
/// has to agree with `tray` exactly or every popup anchored off the tray
/// drifts.
fn tray_width(app: &crate::app::App) -> f32 {
    let (pinned, _) = tray_entries(app);
    if pinned.is_empty() {
        return bar::ARROW_W;
    }
    let cells: f32 = pinned.iter().map(|e| entry_width(*e)).sum();
    let gaps = (pinned.len() - 1) as f32 * bar::TRAY_GAP;
    cells + gaps + bar::ARROW_LEAD + bar::ARROW_W
}

/// The arrow that opens the tray drawer.
///
/// The drawer behind it is a *container*, not any one applet's popup: every
/// entry that is neither pinned nor hidden lives there, so an applet that
/// loses its bar space moves in without a second popup path being written.
fn disclosure() -> Element<'static, Message, Theme> {
    button(
        container(parts::chevron(
            bar::ARROW,
            bar::ARROW_STROKE,
            color::TEXT_SECONDARY,
        ))
        .center(Length::Fill),
    )
    .width(Length::Fixed(bar::ARROW_W))
    .height(Length::Fixed(bar::TASK_H))
    .padding(0)
    .style(cell_style(Color::TRANSPARENT, Color::TRANSPARENT))
    .on_press(Message::Open(crate::app::Drawer::Overflow))
    .into()
}

/// One tray applet: a mark, its reading in mono, and — if it has somewhere to
/// go — a ground that answers the pointer.
///
/// A reading with no drawer behind it is deliberately *not* a button: a cell
/// that lights up under the pointer and then does nothing when clicked is a
/// worse lie than a cell that never lights up.
fn applet<'a>(
    mark: Element<'a, Message, Theme>,
    body: Option<String>,
    tint: Color,
    press: Option<Message>,
) -> Element<'a, Message, Theme> {
    // A cell with no reading is the mark alone in a narrower cell, not the mark
    // pushed left inside a cell sized for digits that are no longer there.
    let width = Length::Fixed(match body {
        Some(_) => bar::TRAY_CELL_W,
        None => bar::TRAY_MARK_W,
    });
    let mut face = Row::new().spacing(bar::GAP + bar::GAP).align_y(Alignment::Center);
    face = face.push(mark);
    if let Some(body) = body {
        face = face.push(text(body).size(size::MONO).font(font::DATA).color(tint));
    }
    match press {
        Some(message) => button(container(face).center(Length::Fill))
            .width(width)
            .height(Length::Fixed(bar::TASK_H))
            .padding(0)
            .style(cell_style(Color::TRANSPARENT, Color::TRANSPARENT))
            .on_press(message)
            .into(),
        None => container(face)
            .width(width)
            .height(Length::Fixed(bar::TASK_H))
            .align_x(Alignment::Center)
            .align_y(Alignment::Center)
            .into(),
    }
}

// ----------------------------------------------------------------- drawers

/// A drawer's width. Fixed for the same reason the menu's is: a popup's pixel
/// size is handed to the compositor when the surface is created and cannot be
/// renegotiated from the layout pass.
pub const DRAWER_W: u32 = drawer::W as u32;

/// A drawer under construction: its head, its rows, and the height they add
/// up to, kept together so the arithmetic that sizes the popup and the column
/// that fills it are one computation and cannot disagree.
struct Sheet {
    head: Element<'static, Message, Theme>,
    rows: Vec<Element<'static, Message, Theme>>,
    height: f32,
}

impl Sheet {
    fn new(head: Element<'static, Message, Theme>, head_h: f32) -> Self {
        Self {
            head,
            rows: Vec::new(),
            height: head_h + 2.0 * drawer::PAD,
        }
    }

    fn row(&mut self, row: Element<'static, Message, Theme>) {
        self.rows.push(row);
        self.height += drawer::ROW_H;
    }

    fn current(&mut self, row: Element<'static, Message, Theme>) {
        self.rows.push(row);
        self.height += drawer::CURRENT_H;
    }

    fn rule(&mut self) {
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
        crate::app::Drawer::Network => wifi_sheet(app),
        crate::app::Drawer::Bluetooth => bluetooth_sheet(app),
        crate::app::Drawer::Overflow => overflow_sheet(app),
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
///   is [`color::CONNECTED`] blue, the same hue the bar uses for a linked
///   device, and never the accent: that is why the drawers draw their own
///   glyphs through [`network_glyph`] instead of the bar's `network_mark` and
///   `bluetooth_mark`, which spend yellow on a full-bar signal and an idle
///   adapter.
/// - **Overflow:** no yellow at all. It is a shelf of other applets' doors;
///   the state behind each one is that applet's drawer's to colour.
fn drawer_view(app: &crate::app::App, which: crate::app::Drawer) -> Element<'static, Message, Theme> {
    let Sheet { head, rows, .. } = sheet(app, which);
    parts::drawer_frame(app.menu_radius, head, rows)
}

fn wifi_sheet(app: &crate::app::App) -> Sheet {
    let on = app.radios.wifi_enabled;
    let mut s = Sheet::new(
        parts::drawer_switch_head("wi-fi", parts::Toggle::new(on, Message::WifiEnable)),
        drawer::HEAD_SWITCH_H,
    );
    if !on {
        s.row(parts::drawer_note("off"));
    } else {
        // The service's list says which network is active; before it has
        // answered, the status feed's link id is the same fact.
        let current = app
            .radios
            .active_network()
            .map(|n| n.ssid.clone())
            .or(match &app.network {
                Network::Wifi { id, .. } => Some(id.clone()),
                _ => None,
            });
        if let Some(ssid) = &current {
            s.current(parts::drawer_current(
                parts::mark(
                    crate::icons::symbolic(network_glyph(&app.network)),
                    drawer::CURRENT_MARK,
                    color::CONNECTED,
                ),
                &elide(ssid, DRAWER_CHARS),
                "connected",
                color::CONNECTED,
                Some(("Disconnect", Message::WifiDisconnect)),
            ));
        }
        let others: Vec<_> = app
            .radios
            .networks
            .iter()
            .filter(|n| Some(&n.ssid) != current.as_ref())
            .take(drawer::MAX_ROWS)
            .collect();
        if current.is_some() && !others.is_empty() {
            s.rule();
        }
        for n in others {
            // A lock and nothing else: whether joining will ask for a secret
            // is the only thing about a stranger's network the drawer owes
            // the human before they click it.
            let lock = n.secured.then(|| {
                parts::mark(
                    crate::icons::symbolic("network-wireless-encrypted"),
                    drawer::MARK,
                    color::TEXT_TERTIARY,
                )
            });
            s.row(parts::drawer_choice(
                blank_mark(),
                &elide(&n.ssid, DRAWER_CHARS),
                lock,
                Some(Message::WifiConnect(n.ssid.clone())),
            ));
        }
        if current.is_none() && app.radios.networks.is_empty() {
            s.row(parts::drawer_note("searching\u{2026}"));
        }
    }
    s.rule();
    s.row(parts::drawer_link(
        "Network settings",
        Message::OpenSettings("network"),
    ));
    s
}

fn bluetooth_sheet(app: &crate::app::App) -> Sheet {
    let on = app.bluetooth.powered;
    let mut s = Sheet::new(
        parts::drawer_switch_head("bluetooth", parts::Toggle::new(on, Message::BtPower)),
        drawer::HEAD_SWITCH_H,
    );
    if !on {
        s.row(parts::drawer_note("off"));
    } else {
        let devices = &app.radios.devices;
        let connected: Vec<_> = devices.iter().filter(|d| d.connected).collect();
        let paired: Vec<_> = devices
            .iter()
            .filter(|d| d.paired && !d.connected)
            .take(drawer::MAX_ROWS)
            .collect();
        for d in &connected {
            s.current(parts::drawer_current(
                device_mark(d.kind, drawer::CURRENT_MARK, color::CONNECTED),
                &elide(&d.name, DRAWER_CHARS),
                "connected",
                color::CONNECTED,
                Some(("Disconnect", Message::BtDisconnect(d.addr.clone()))),
            ));
        }
        if !connected.is_empty() && !paired.is_empty() {
            s.rule();
        }
        for d in &paired {
            s.row(parts::drawer_choice(
                device_mark(d.kind, drawer::MARK, color::TEXT_SECONDARY),
                &elide(&d.name, DRAWER_CHARS),
                None,
                Some(Message::BtConnect(d.addr.clone())),
            ));
        }
        if connected.is_empty() && paired.is_empty() {
            s.row(parts::drawer_note("no paired devices"));
        }
        s.rule();
        // Discovery is a verb on the list rather than a second switch: it
        // runs for as long as the human is looking, and a toggle would claim
        // it is a setting that stays on.
        let scanning = app.radios.scanning;
        s.row(parts::drawer_choice(
            parts::mark(
                crate::icons::symbolic("view-refresh"),
                drawer::MARK,
                color::TEXT_SECONDARY,
            ),
            if scanning {
                "Stop scanning"
            } else {
                "Scan for devices"
            },
            None,
            Some(Message::BtScan),
        ));
        if scanning {
            let nearby: Vec<_> = devices
                .iter()
                .filter(|d| !d.paired)
                .take(drawer::MAX_ROWS)
                .collect();
            for d in &nearby {
                s.row(parts::drawer_choice(
                    device_mark(d.kind, drawer::MARK, color::TEXT_TERTIARY),
                    &elide(&d.name, DRAWER_CHARS),
                    None,
                    Some(Message::BtConnect(d.addr.clone())),
                ));
            }
            if nearby.is_empty() {
                s.row(parts::drawer_note("searching\u{2026}"));
            }
        }
    }
    s.rule();
    s.row(parts::drawer_link(
        "Bluetooth settings",
        Message::OpenSettings("network"),
    ));
    s
}

fn overflow_sheet(app: &crate::app::App) -> Sheet {
    let mut s = Sheet::new(parts::drawer_sheet_head("tray"), drawer::HEAD_H);
    let (_, overflow) = tray_entries(app);
    if overflow.is_empty() {
        s.row(parts::drawer_note("nothing here"));
    }
    for entry in overflow {
        s.row(overflow_row(app, entry));
    }
    s
}

/// One entry of the overflow: the applet's mark, its name, its reading, and
/// the door to its own drawer. All neutral — see [`drawer_view`]'s ledger.
fn overflow_row(app: &crate::app::App, entry: Entry) -> Element<'static, Message, Theme> {
    let reading = |r: String| -> Option<Element<'static, Message, Theme>> {
        Some(
            text(r)
                .font(font::DATA)
                .size(size::MONO)
                .color(color::TEXT_SECONDARY)
                .into(),
        )
    };
    match entry {
        Entry::Network => {
            let r = match &app.network {
                Network::Offline => "offline".to_owned(),
                Network::Wired { .. } => "wired".to_owned(),
                Network::Wifi { strength, .. } => format!("{strength}%"),
                Network::Other { .. } => "on".to_owned(),
            };
            parts::drawer_choice(
                parts::mark(
                    crate::icons::symbolic(network_glyph(&app.network)),
                    drawer::MARK,
                    color::TEXT_SECONDARY,
                ),
                "Network",
                reading(r),
                Some(Message::Open(crate::app::Drawer::Network)),
            )
        }
        Entry::Bluetooth => {
            let bt = app.bluetooth;
            let r = match (bt.powered, bt.connected) {
                (false, _) => "off".to_owned(),
                (true, 0) => "on".to_owned(),
                (true, n) => format!("{n} connected"),
            };
            parts::drawer_choice(
                parts::mark(
                    crate::icons::symbolic(bluetooth_glyph(bt)),
                    drawer::MARK,
                    color::TEXT_SECONDARY,
                ),
                "Bluetooth",
                reading(r),
                Some(Message::Open(crate::app::Drawer::Bluetooth)),
            )
        }
        Entry::Battery => {
            let r = app.battery.map(battery_text).unwrap_or_default();
            let pct = app.battery.map_or(0, |b| b.percent);
            // No press: the battery has no drawer, and a row that lifts and
            // then does nothing is the lie `applet` already refuses to tell.
            parts::drawer_choice(
                parts::battery_gauge(pct, color::TEXT_SECONDARY),
                "Battery",
                reading(r),
                None,
            )
        }
        Entry::Item(i) => {
            let Some(item) = app.radios.tray.get(i) else {
                return Space::new().into();
            };
            mouse_area(parts::drawer_choice(
                tray_mark(&item.icon, drawer::MARK, color::TEXT_SECONDARY),
                &elide(&item.title, DRAWER_CHARS),
                None,
                Some(Message::TrayActivate(item.address.clone())),
            ))
            .on_right_press(Message::TrayMenu(item.address.clone()))
            .into()
        }
    }
}

/// A tray item's art at `side`, in `tint`'s ink. An SVG is recoloured by
/// `parts::mark`; pixels were silhouetted when the item arrived, so here they
/// only take the ink's alpha — the same arithmetic `parts::glyph` applies.
/// The handle is cloned, not built: its id is what keeps the texture cached.
fn tray_mark(icon: &crate::radio::TrayIcon, side: f32, tint: Color) -> Element<'static, Message, Theme> {
    use crate::radio::TrayIcon;
    match icon {
        TrayIcon::Svg(path) => parts::mark(Some(path.clone()), side, tint),
        TrayIcon::Image(handle) => image(handle.clone())
            .width(Length::Fixed(side))
            .height(Length::Fixed(side))
            .opacity(tint.a)
            .into(),
        TrayIcon::None => parts::mark(None, side, tint),
    }
}

/// The empty square a list row keeps where its mark would be, so its name
/// sits on the same gridline as the rows that do have one.
fn blank_mark() -> Element<'static, Message, Theme> {
    Space::new()
        .width(Length::Fixed(drawer::MARK))
        .height(Length::Fixed(drawer::MARK))
        .into()
}

/// A bluetooth device's mark, by what the device is.
fn device_mark(kind: crate::radio::BtKind, side: f32, tint: Color) -> Element<'static, Message, Theme> {
    use crate::radio::BtKind;
    let name = match kind {
        BtKind::Audio => "audio-headphones",
        BtKind::Input => "input-mouse",
        BtKind::Phone => "phone",
        BtKind::Other => "bluetooth-active",
    };
    parts::mark(crate::icons::symbolic(name), side, tint)
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

/// The icon-theme name for a link's state: the signal cone at its rung, or
/// the plain *connected* glyph where there is no magnitude.
///
/// Name only, no tint — the bar and the drawers colour the same glyph by
/// different ledgers.
fn network_glyph(network: &Network) -> &'static str {
    match network {
        Network::Offline => "network-wireless-offline",
        Network::Wired { .. } | Network::Other { .. } => "network-wireless-connected",
        Network::Wifi { strength, .. } => match strength {
            0..=10 => "network-wireless-signal-none",
            11..=35 => "network-wireless-signal-weak",
            36..=60 => "network-wireless-signal-ok",
            61..=80 => "network-wireless-signal-good",
            _ => "network-wireless-signal-excellent",
        },
    }
}

/// The network mark: the platform's own signal cone, tinted.
///
/// Real theme art rather than drawn bars — a stack of rising rectangles reads
/// as a cellular meter, and this is the arc every desktop uses for wifi. The
/// rungs are the icon theme's five `network-wireless-signal-*` levels, so the
/// mark says the same thing here that it says in every other application on
/// the machine; a wired or unknown link gets the plain *connected* glyph
/// because there is no magnitude to report for it.
fn network_mark(network: &Network, side: f32) -> Element<'static, Message, Theme> {
    let tint = match network {
        Network::Offline => color::TEXT_TERTIARY,
        // Accent at full bars and nowhere else. A link is either as good as it
        // gets or it is merely working, and only the first is worth a colour:
        // tinting every usable signal would spend the row's one yellow on a
        // reading that is true almost all the time and therefore says nothing.
        Network::Wifi { strength, .. } if *strength > 80 => color::ACCENT,
        _ => color::TEXT_SECONDARY,
    };
    parts::mark(crate::icons::symbolic(network_glyph(network)), side, tint)
}

fn bluetooth_glyph(bt: Bluetooth) -> &'static str {
    match (bt.powered, bt.connected) {
        (false, _) => "bluetooth-disabled",
        (true, 0) => "bluetooth-disconnected",
        (true, _) => "bluetooth-active",
    }
}

/// The bluetooth mark: the theme's own rune, in one of three hues.
///
/// Three states and no room for a word, so the hue carries it — inert grey
/// when the radio is off, the accent when it is powered but idle, and
/// [`color::CONNECTED`] when a device is actually on the other end. The glyph
/// changes with it so the state survives for anyone who cannot tell the hues
/// apart.
fn bluetooth_mark(bt: Bluetooth, side: f32) -> Element<'static, Message, Theme> {
    let tint = match (bt.powered, bt.connected) {
        (false, _) => color::NEUTRAL,
        (true, 0) => color::ACCENT,
        (true, _) => color::CONNECTED,
    };
    parts::mark(crate::icons::symbolic(bluetooth_glyph(bt)), side, tint)
}

/// How much of a network's name a drawer row will show.
const DRAWER_CHARS: usize = 14;

/// Percentage at or below which a discharging battery is drawn as a warning.
pub(crate) const LOW: u8 = 15;

/// Charging is a leading `+`, discharging bare, full the word. Time remaining
/// is deliberately absent: it is the least trustworthy number UPower reports.
pub(crate) fn battery_text(battery: Battery) -> String {
    match battery.state {
        Charge::Charging => format!("+{}%", battery.percent),
        Charge::Full => "full".to_owned(),
        Charge::Discharging | Charge::Unknown => format!("{}%", battery.percent),
    }
}

// ------------------------------------------------------------------- clock

/// Time over date, both mono and centred: the only two-line cell on the row,
/// which is what makes the right end read as an end. It is also the one place
/// the bar admits the compositor is gone — a disconnected bar dims its clock
/// rather than freezing it.
fn clock(snapshot: &Snapshot) -> Element<'_, Message, Theme> {
    let tint = if snapshot.connected {
        color::TEXT
    } else {
        color::TEXT_TERTIARY
    };
    let time = if snapshot.clock.is_empty() {
        crate::clock::time()
    } else {
        snapshot.clock.clone()
    };
    let stack = column![
        text(time)
            .size(size::BODY_SMALL)
            .font(font::DATA_MEDIUM)
            .color(tint),
        text(crate::clock::date())
            .size(size::MICRO)
            .font(font::DATA)
            .color(color::TEXT_TERTIARY),
    ]
    .spacing(0)
    .align_x(Alignment::Center);

    // TODO: clicking the clock should open a full calendar drawer,
    // Windows-style — month grid over the day's agenda — on the same popup
    // machinery as the tray drawer (`parts::drawer_sheet`, sized by
    // `drawer_height`). Not wired yet: it needs a calendar data path, and
    // inventing one here would be a fake backend.
    //
    // Fixed width, like every other zone right of the task strip: the strip's
    // condensation ladder subtracts these from the bar to find its own room,
    // and a clock that changed width as the minute rolled over would make
    // every chip on the bar twitch.
    container(stack)
        .width(Length::Fixed(bar::CLOCK_W))
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .into()
}

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

    /// The cell is a reading, not a guess: offline has its own glyph rather
    /// than the weakest rung, because a dead link and a faint one look the
    /// same on a bar and only one of them is worth telling the human about.
    #[test]
    fn an_offline_link_still_says_something() {
        assert_eq!(network_glyph(&Network::Offline), "network-wireless-offline");
    }

    #[test]
    fn wifi_reports_its_rung() {
        let wifi = Network::Wifi {
            id: "House".into(),
            strength: 49,
        };
        assert_eq!(network_glyph(&wifi), "network-wireless-signal-ok");
    }

    #[test]
    fn charging_is_distinguishable_from_draining() {
        let at = |state| Battery {
            percent: 80,
            state,
            remaining: None,
        };
        assert_eq!(battery_text(at(Charge::Charging)), "+80%");
        assert_eq!(battery_text(at(Charge::Discharging)), "80%");
        assert_eq!(battery_text(at(Charge::Full)), "full");
    }

    /// Room for two is room for two full chips — and no more than the cap,
    /// or two windows become two half-bar slabs.
    #[test]
    fn a_roomy_strip_shows_full_chips_at_the_cap() {
        let (w, detail, shown) = ladder(4000.0, 2);
        assert_eq!(w, bar::TASK_MAX);
        assert_eq!(detail, Detail::Full);
        assert_eq!(shown, 2);
    }

    /// The rung is a consequence of the room, never of a window count: the
    /// same eight windows read three different ways in three different bars.
    /// The owner's rule: an ellipsized detail is worse than no detail. A chip
    /// never renders one, at any width, for any label.
    #[test]
    fn a_chip_never_renders_an_ellipsis() {
        let long = "…/syncedprojects/EclipseOS/AbyssCompositor";
        for width in [
            bar::TASK_BARE,
            bar::TASK_MIN,
            bar::TASK_NAME,
            bar::TASK_FULL,
            bar::TASK_MAX,
        ] {
            for strip in [Detail::Full, Detail::Name, Detail::Icon, Detail::Bare] {
                let r = rung(long, "kitty", width, strip);
                let drawn = match r {
                    Detail::Full => Some(long),
                    Detail::Name => Some("kitty"),
                    Detail::Icon | Detail::Bare => None,
                };
                if let Some(drawn) = drawn {
                    assert!(
                        drawn.chars().count() <= budget(width, r),
                        "{drawn:?} at {width} would have had to be cut"
                    );
                }
            }
        }
    }

    /// Per chip, not per bar: a short title keeps its detail while the long one
    /// beside it — same width, same strip rung — drops to its process name.
    #[test]
    fn one_chips_long_title_does_not_demote_its_neighbour() {
        let w = bar::TASK_MAX;
        assert_eq!(rung("notes.md", "micro", w, Detail::Full), Detail::Full);
        assert_eq!(
            rung("…/syncedprojects/EclipseOS", "kitty", w, Detail::Full),
            Detail::Name
        );
    }

    /// The process name is the floor for text, and it is never cut either.
    #[test]
    fn a_name_that_will_not_fit_whole_becomes_an_icon() {
        let narrow = bar::TASK_NAME;
        assert_eq!(
            rung("whatever", "a-very-long-application-name", narrow, Detail::Full),
            Detail::Icon
        );
    }

    #[test]
    fn the_ladder_is_driven_by_room_and_not_by_count() {
        assert_eq!(ladder(1400.0, 8).1, Detail::Full);
        assert_eq!(ladder(700.0, 8).1, Detail::Name);
        assert_eq!(ladder(300.0, 8).1, Detail::Icon);
        assert_eq!(ladder(160.0, 8).1, Detail::Bare);
    }

    /// The whole point: whatever the rung, the chips plus their gaps fit.
    #[test]
    fn the_strip_never_exceeds_its_room() {
        for avail in [90.0f32, 160.0, 300.0, 700.0, 1400.0] {
            for count in 1..40usize {
                let (w, _, shown) = ladder(avail, count);
                let drawn = shown as f32 * w + (shown.max(1) - 1) as f32 * bar::GAP;
                let tail = if shown < count {
                    bar::OVERFLOW_W + bar::GAP
                } else {
                    0.0
                };
                assert!(drawn + tail <= avail + 0.5, "{avail} / {count}");
            }
        }
    }

    /// Windows that do not fit are counted, not dropped in silence.
    #[test]
    fn windows_past_the_end_become_a_counter() {
        let (_, detail, shown) = ladder(120.0, 30);
        assert_eq!(detail, Detail::Bare);
        assert!(shown < 30);
    }

    /// A test-only window fixture for the expansion tests: only `title` and
    /// `minimized` vary, everything else is filler no chip logic reads.
    fn expand_fixture(handle: u64, title: &str, minimized: bool) -> Window {
        Window {
            handle,
            app_id: "kitty".into(),
            title: title.into(),
            workspace: Some(1),
            output: Some(0),
            focused: false,
            minimized,
            pid: None,
            trust: crate::model::Trust::Private,
        }
    }

    /// A roomy strip's genuine slack lets a long title through whole — past
    /// both the 18-char clamp and `TASK_MAX` — while a short-titled neighbour,
    /// which was never using its own share, keeps exactly the floor.
    #[test]
    fn a_long_title_expands_past_the_cap_when_room_allows() {
        let long = expand_fixture(1, "a title much longer than eighteen characters wide", false);
        let short = expand_fixture(2, "short", false);
        let windows = [&long, &short];
        let base_width = bar::TASK_MAX;
        let avail = whole_width(long.label()) + base_width + bar::GAP + 400.0;

        let widths = expand(&windows, base_width, avail);

        assert!(widths[0] >= whole_width(long.label()));
        assert_eq!(widths[1], base_width);
    }

    /// Two windows both want more than the strip has spare; the one that is
    /// up wins the room over the one that has been sent away.
    #[test]
    fn expansion_favours_the_window_that_is_up() {
        let away = expand_fixture(1, "a title much longer than eighteen characters wide", true);
        let up = expand_fixture(2, "a title much longer than eighteen characters wide", false);
        let windows = [&away, &up];
        let base_width = bar::TASK_MAX;
        let want = whole_width(away.label()) - base_width;
        // Just enough surplus for one of the two, not both.
        let avail = 2.0 * base_width + bar::GAP + want;

        let widths = expand(&windows, base_width, avail);

        assert_eq!(widths[0], base_width, "the minimized chip yields its priority");
        assert!(widths[1] > base_width, "the chip that is up is satisfied first");
    }

    /// With two non-minimized windows tied on state, the left one — the lower
    /// original index — is offered the slack first, so the outcome never
    /// depends on iteration order.
    #[test]
    fn expansion_prefers_the_left_window_among_equals() {
        let left = expand_fixture(1, "a title much longer than eighteen characters wide", false);
        let right = expand_fixture(2, "a title much longer than eighteen characters wide", false);
        let windows = [&left, &right];
        let base_width = bar::TASK_MAX;
        let want = whole_width(left.label()) - base_width;
        let avail = 2.0 * base_width + bar::GAP + want;

        let widths = expand(&windows, base_width, avail);

        assert!(widths[0] > base_width, "the left chip is satisfied first");
        assert_eq!(widths[1], base_width, "no slack is left for the right chip");
    }

    /// A packed strip has no slack to give: every chip stays at the floor
    /// `ladder` already gave it, exactly as it did before expansion existed.
    #[test]
    fn a_packed_strip_leaves_every_chip_at_its_floor() {
        let a = expand_fixture(1, "a title much longer than eighteen characters wide", false);
        let b = expand_fixture(2, "another quite long title past the character cap", false);
        let windows = [&a, &b];
        let base_width = bar::TASK_MAX;
        let avail = 2.0 * base_width + bar::GAP;

        let widths = expand(&windows, base_width, avail);

        assert_eq!(widths, vec![base_width, base_width]);
    }

    /// A workspace with only minimized windows still has windows on it.
    #[test]
    fn the_pager_keeps_occupied_and_current_workspaces_only() {
        let ws = |index: usize, active: bool, windows: usize| Workspace {
            index,
            output: 0,
            output_name: String::new(),
            active,
            windows,
        };
        let snapshot = Snapshot {
            workspaces: vec![ws(1, true, 0), ws(2, false, 0), ws(3, false, 2), ws(4, false, 0)],
            windows: vec![Window {
                handle: 1,
                app_id: "kitty".into(),
                title: "t".into(),
                workspace: Some(4),
                output: Some(0),
                focused: false,
                minimized: true,
                pid: None,
                trust: crate::model::Trust::Private,
            }],
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
        let ws = |index: usize, output: u64, active: bool, windows: usize| Workspace {
            index,
            output,
            output_name: String::new(),
            active,
            windows,
        };
        let w = |handle: u64, output: u64, workspace: usize| Window {
            handle,
            app_id: "kitty".into(),
            title: "t".into(),
            workspace: Some(workspace),
            output: Some(output),
            focused: false,
            minimized: false,
            pid: None,
            trust: crate::model::Trust::Private,
        };
        let snapshot = Snapshot {
            workspaces: vec![ws(1, 7, false, 1), ws(2, 7, true, 1), ws(1, 9, true, 1)],
            windows: vec![w(1, 7, 1), w(2, 7, 2), w(3, 9, 1)],
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
        use crate::model::{Trust, Window};
        let w = Window {
            handle: 1,
            app_id: "org.x.Vault".into(),
            title: "seed phrase correct horse".into(),
            workspace: Some(1),
            output: Some(0),
            focused: true,
            minimized: false,
            pid: None,
            trust: Trust::Secret,
        };
        let drawn = elide(w.label(), TITLE_CHARS);
        assert!(!drawn.contains("seed"));
        assert!(drawn.starts_with("Protected"));
    }
    /// A popup anchored under a cell has to know where the cell is, and the
    /// only claim worth testing is that the arithmetic agrees with the strip's
    /// own: chip *n* sits one width-plus-gap past chip *n-1*, every chip is
    /// inside the bar, and a window that is not drawn has no span at all.
    #[test]
    fn a_chip_span_follows_the_strip_it_describes() {
        let w = |handle: u64, workspace: usize| Window {
            handle,
            app_id: "kitty".into(),
            title: "t".into(),
            workspace: Some(workspace),
            output: Some(0),
            focused: false,
            minimized: false,
            pid: None,
            trust: crate::model::Trust::Private,
        };
        let mut app = crate::app::App::new();
        app.width = 1830.0;
        app.snapshot = Snapshot {
            workspaces: vec![Workspace {
                index: 1,
                output: 0,
                output_name: String::new(),
                active: true,
                windows: 3,
            }],
            windows: vec![w(1, 1), w(2, 1), w(3, 1), w(9, 2)],
            ..Snapshot::default()
        };

        let first = chip_span(&app, 1).expect("chip 1 is drawn");
        let second = chip_span(&app, 2).expect("chip 2 is drawn");
        assert!(first.0 >= bar::EDGE);
        assert!(first.1 < second.0);
        assert!((second.0 - first.0 - (first.1 - first.0) - bar::GAP).abs() < 0.01);
        assert!(second.1 < app.width);
        // On another workspace, so it is not on the strip and has no cell.
        assert_eq!(chip_span(&app, 9), None);

        // The tray is measured inward from the right edge and stays there.
        let tray = tray_span(&app, crate::app::Drawer::Overflow).expect("sized bar");
        assert!(tray.1 <= app.width - bar::EDGE);
        assert!(tray.0 > chip_span(&app, 3).expect("chip 3 is drawn").1);
    }

    /// Hidden beats pinned, pinned keeps its own order, and a drawer whose
    /// applet is not on the bar hangs off the arrow that leads to it.
    #[test]
    fn the_tray_follows_its_config() {
        let mut app = crate::app::App::new();
        app.width = 1440.0;
        app.battery = None;
        app.tray.pinned = Some(vec!["bluetooth".into(), "network".into()]);
        app.tray.hidden = vec!["network".into()];
        let (pinned, overflow) = tray_entries(&app);
        assert_eq!(pinned, vec![Entry::Bluetooth]);
        assert!(overflow.is_empty());

        let arrow = tray_span(&app, crate::app::Drawer::Overflow).expect("sized bar");
        let bt = tray_span(&app, crate::app::Drawer::Bluetooth).expect("sized bar");
        assert!(bt.1 < arrow.0);
        assert_eq!(tray_span(&app, crate::app::Drawer::Network), Some(arrow));

        // Unset pins network and battery; bluetooth waits in the overflow.
        app.tray = crate::conn::TrayConfig::default();
        let (pinned, overflow) = tray_entries(&app);
        assert_eq!(pinned, vec![Entry::Network]);
        assert_eq!(overflow, vec![Entry::Bluetooth]);
    }
}
