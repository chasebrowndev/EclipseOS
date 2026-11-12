// SPDX-License-Identifier: AGPL-3.0-only
//! The window switcher, the overview, hot corners and hover previews under
//! `mode "de"` (COMP-05 §7.1, C-24, COMP-17 §2, ADR 0090).
//!
//! These are compositor overlays like the window menu: drawn by the backends
//! after the windows and before trusted UI, never part of a capture pass list
//! (`render/scan_tests.rs` enforces it), never opened under the session lock,
//! and closed when the lock engages, when their output goes, when `de` is
//! left, or when a prompt or the region selector takes the seat. **None is
//! trusted UI**; being compositor-drawn confers no trust.
//!
//! - [`switcher`]: Alt+Tab. Holds the keyboard while the modifier is down.
//! - [`Overview`] (this file): Present Windows and the desktop strip.
//! - [`preview`]: the taskbar's hover preview, which holds no input.
//! - [`corners`]: the pointer dwelling in a screen corner.
//! - [`thumbs`]: which windows may be drawn, and when their textures refresh.
//!
//! **Redaction.** Every card goes through `render::capture::classify` (via
//! [`thumbs::shown`]) before any GPU work touches its window, every frame:
//! protected windows get no card, redacted ones a blank card with no title, and
//! node-redacted ones a thumbnail with opaque covers. A redacted window's title
//! is never drawn *or matched* (typing a filter would otherwise tell a
//! human-visible tell about a title that must not be shown).
//!
//! **One at a time, one owner each.** [`Switcher`](switcher::Switcher),
//! [`Overview`] and [`Preview`](preview::Preview) are fields of `AbyssState`;
//! opening one closes the others and the window menu. The switcher and the
//! overview take the keyboard only through the input filter (as the window
//! menu does) and the pointer through the `holds_pointer` gates; neither sets a
//! client keyboard grab, so nothing is left to release when they close.

pub mod corners;
pub mod preview;
pub mod switcher;
pub mod thumbs;

#[cfg(test)]
mod tests;

use std::time::Instant;

use smithay::{
    backend::renderer::gles::GlesRenderer,
    desktop::Window,
    input::{keyboard::Keysym, pointer::MotionEvent},
    output::Output,
    reexports::calloop::LoopHandle,
    utils::{Logical, Point, Rectangle, Size, SERIAL_COUNTER},
};

use ec_abyss_render::{
    overview::{present_grid, strip_layout, DragView, OverlayView, Placed, TileView, GAP, MARGIN},
    AbyssRenderElement,
};

use crate::{
    shell::{self, focus, workspace},
    state::AbyssState,
};

use thumbs::{Face, Wanted};

type Rect = Rectangle<i32, Logical>;

/// May an overlay exist right now? `de`, a live session, nothing else holding
/// the human's seat.
pub fn allowed(state: &AbyssState) -> bool {
    // Off for the session after a contained panic (ADR 0102).
    !state.contained.is_off(crate::contain::Feature::Overview)
        && shell::de::stacking(state)
        && !state.lock.locked
        && !crate::trusted_ui::holds_seat(state)
        && !state.region_select.active()
}

/// Whether the switcher or the overview is open: they own the keyboard.
pub fn holds_keys(state: &AbyssState) -> bool {
    state.switcher.is_open() || state.overview.is_open()
}

/// Whether the switcher or the overview is open: they own the pointer.
pub fn holds_pointer(state: &AbyssState) -> bool {
    state.switcher.is_open() || state.overview.is_open()
}

/// Whether anything is open, a hover preview included.
pub fn any_open(state: &AbyssState) -> bool {
    holds_keys(state) || state.preview.is_open()
}

/// Close everything this module owns, drop every texture it holds, and give
/// the pointer back. Nothing else is touched.
pub fn close_all(state: &mut AbyssState) {
    let was_open = any_open(state);
    state.switcher.open = None;
    state.overview.open = None;
    preview::close_quietly(state);
    state.corners.disarm(&state.loop_handle);
    state.thumbs.release();
    state.overlay_cache.clear();
    if was_open {
        crate::backend::damage_all(state);
    }
}

/// Close what is open, then let the pointer find what lies under it again.
pub fn close(state: &mut AbyssState) {
    dismiss(state);
}

fn dismiss(state: &mut AbyssState) {
    close_all(state);
    state.refresh_pointer_focus_keeping_keyboard();
}

/// Open one overlay at the exclusion of the others (and the window menu).
fn make_room(state: &mut AbyssState) {
    state.winmenu.close();
    state.switcher.open = None;
    state.overview.open = None;
    preview::close_quietly(state);
}

/// A holder of the pointer is opening: no client is pointed at while it is up.
fn take_pointer(state: &mut AbyssState) {
    if let Some(pointer) = state.seat.get_pointer() {
        let time = state.start_time.elapsed().as_millis() as u32;
        pointer.motion(
            state,
            None,
            &MotionEvent {
                location: state.pointer_location,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(state);
    }
    state.last_pointer_focus = None;
}

/// One window in a list: what a card needs to be laid out.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub window: Window,
    pub handle: u64,
    pub size: Size<i32, Logical>,
}

/// Entries for `windows`, leaving out any that must have no card (protected).
pub(crate) fn entries(state: &mut AbyssState, windows: &[Window]) -> Vec<Entry> {
    let cleared: Vec<(Window, Size<i32, Logical>)> = windows
        .iter()
        .filter_map(|w| thumbs::shown(state, w).map(|s| (w.clone(), s.size())))
        .collect();
    cleared
        .into_iter()
        .map(|(window, size)| Entry {
            handle: state.ipc.handle_for(&window),
            window,
            size,
        })
        .collect()
}

/// Switch to `window`'s desktop, unminimize it and focus it: what choosing a
/// card does. The window's output is the focused one by construction (the
/// lists are built from it); a window that has gone is ignored.
pub(crate) fn activate(state: &mut AbyssState, window: &Window) {
    use smithay::utils::IsAlive;
    if !window.alive() || state.lock.locked {
        return;
    }
    let Some(id) = shell::output_of_window(state, window) else {
        return;
    };
    let Some(entry) = state.outputs.get(id) else {
        return;
    };
    let ws = if entry.workspaces[entry.active].holds(window) {
        entry.active
    } else {
        entry
            .workspaces
            .iter()
            .position(|ws| ws.holds(window))
            .unwrap_or(entry.active)
    };
    if state.outputs.focused().map(|e| e.id) != Some(id) {
        state.outputs.set_focused(id);
    }
    if ws != state.outputs.get(id).map_or(ws, |e| e.active) {
        shell::switch_workspace(state, ws + 1);
    }
    if shell::is_minimized(state, window) {
        shell::unminimize_window(state, window);
    } else {
        focus::focus_window(state, window);
    }
}

fn pointer_distance(a: Point<f64, Logical>, b: Point<f64, Logical>) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

// ------------------------------------------------------------------ the overview

/// Pointer travel, logical px, before a press on a card is a drag.
const DRAG_THRESHOLD: f64 = 6.0;
/// Room kept under the cards for the filter pill.
const FILTER_ROOM: i32 = 44;
/// Longest filter accepted.
const FILTER_MAX: usize = 64;
const BTN_LEFT: u32 = 0x110;
const BTN_MIDDLE: u32 = 0x112;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hover {
    Nothing,
    Card(usize),
    Tile(usize),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Press {
    Card(usize, Point<f64, Logical>),
    Tile(usize),
}

/// The open overview. Plain data owned by `AbyssState`.
#[derive(Debug, Clone)]
pub(crate) struct OverviewOpen {
    output: u64,
    output_rect: Rect,
    /// The desktop whose windows these are, 0-based.
    desk: usize,
    /// What the desktop held when the cards were last built, protected windows
    /// included: a change means rebuilding.
    signature: Vec<Window>,
    all: Vec<Entry>,
    filter: String,
    /// Indexes into `all` of the cards shown, in layout order.
    shown: Vec<usize>,
    placed: Vec<Placed>,
    strip: Rect,
    tiles: Vec<Rect>,
    /// Windows on each desktop (counts only).
    counts: Vec<usize>,
    hover: Hover,
    press: Option<Press>,
    dragging: Option<usize>,
}

/// The overview, when open.
#[derive(Debug, Default)]
pub struct Overview {
    pub(crate) open: Option<OverviewOpen>,
}

impl Overview {
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// What has been typed, if open.
    pub fn filter(&self) -> Option<&str> {
        self.open.as_ref().map(|o| o.filter.as_str())
    }

    pub fn output(&self) -> Option<u64> {
        self.open.as_ref().map(|o| o.output)
    }

    /// The desktop the cards belong to, 0-based.
    pub fn desktop(&self) -> Option<usize> {
        self.open.as_ref().map(|o| o.desk)
    }

    /// The windows with a card, in layout order.
    pub fn cards(&self) -> Vec<Window> {
        self.open.as_ref().map_or_else(Vec::new, |o| {
            o.shown.iter().map(|&i| o.all[i].window.clone()).collect()
        })
    }

    /// Where each card is, parallel to [`Overview::cards`].
    pub fn placed(&self) -> &[Placed] {
        self.open.as_ref().map_or(&[], |o| o.placed.as_slice())
    }

    /// The desktop strip's tiles, in desktop order.
    pub fn tiles(&self) -> &[Rect] {
        self.open.as_ref().map_or(&[], |o| o.tiles.as_slice())
    }

    /// Whether a card is being dragged.
    pub fn dragging(&self) -> bool {
        self.open.as_ref().is_some_and(|o| o.dragging.is_some())
    }
}

/// The windows of output `id`'s active desktop, most recently used first.
fn desktop_windows(state: &AbyssState, id: u64) -> Vec<Window> {
    state
        .outputs
        .get(id)
        .map(|e| e.workspace().recent_live())
        .unwrap_or_default()
        .into_iter()
        .filter(|w| !shell::is_unmanaged_x11(state, w))
        .collect()
}

/// Lay the cards for the entries that match the filter, and the strip.
fn relayout(state: &AbyssState, o: &mut OverviewOpen) {
    let needle = o.filter.to_lowercase();
    o.shown = (0..o.all.len())
        .filter(|&i| {
            if needle.is_empty() {
                return true;
            }
            // A window that may not show its title is never matched by one:
            // otherwise typing would find it by its title.
            thumbs::shown(state, &o.all[i].window)
                .filter(|s| matches!(s.face, Face::Pixels { .. }))
                .and_then(|s| s.title)
                .is_some_and(|t| t.to_lowercase().contains(&needle))
        })
        .collect();
    let rect = o.output_rect;
    let (strip, tiles) = strip_layout(
        workspace::COUNT,
        state.config.general.desktop_rows.max(1) as usize,
        rect,
    );
    o.strip = strip;
    o.tiles = tiles;
    let top = strip.loc.y + strip.size.h + GAP;
    let area = Rectangle::new(
        Point::from((rect.loc.x + MARGIN, top)),
        Size::from((
            rect.size.w - 2 * MARGIN,
            (rect.loc.y + rect.size.h - MARGIN - FILTER_ROOM - top).max(1),
        )),
    );
    let sources: Vec<_> = o.shown.iter().map(|&i| o.all[i].size).collect();
    o.placed = present_grid(&sources, area);
    o.hover = Hover::Nothing;
    o.press = None;
    o.dragging = None;
}

/// Build the cards for output `id`'s active desktop.
fn build(state: &mut AbyssState, id: u64, filter: String) -> Option<OverviewOpen> {
    let entry = state.outputs.get(id)?;
    let output_rect = state.space.output_geometry(&entry.output)?;
    let desk = entry.active;
    let counts = entry.workspaces.iter().map(|w| w.all_windows().len()).collect();
    let signature = desktop_windows(state, id);
    let all = entries(state, &signature);
    let mut o = OverviewOpen {
        output: id,
        output_rect,
        desk,
        signature,
        all,
        filter,
        shown: Vec::new(),
        placed: Vec::new(),
        strip: Rectangle::default(),
        tiles: Vec::new(),
        counts,
        hover: Hover::Nothing,
        press: None,
        dragging: None,
    };
    relayout(state, &mut o);
    Some(o)
}

/// Open the overview if it is closed, close it if open. For the bind, the hot
/// corner and the socket.
pub fn toggle(state: &mut AbyssState) {
    if state.overview.is_open() {
        dismiss(state);
    } else {
        open(state);
    }
}

/// Open the overview on the focused output. False, and nothing opens, when the
/// mode is not `de`, the session is locked, a prompt or the region selector
/// holds the seat, or a grab or drag is under way.
pub fn open(state: &mut AbyssState) -> bool {
    if !allowed(state) || crate::input::grabs::drag_active(state) {
        return false;
    }
    let Some(id) = state.outputs.focused().map(|e| e.id) else {
        return false;
    };
    make_room(state);
    state.corners.disarm(&state.loop_handle);
    let Some(o) = build(state, id, String::new()) else {
        return false;
    };
    tracing::info!(cards = o.shown.len(), desktop = o.desk + 1, "overview opened");
    // Geometry only, never a title: the scripted checks aim clicks with it.
    let rects = |r: &mut dyn Iterator<Item = Rect>| {
        r.map(|r| format!("{},{},{},{}", r.loc.x, r.loc.y, r.size.w, r.size.h))
            .collect::<Vec<_>>()
            .join(";")
    };
    tracing::debug!(
        "overview layout frames={} tiles={}",
        rects(&mut o.placed.iter().map(|p| p.frame)),
        rects(&mut o.tiles.iter().copied())
    );
    state.overview.open = Some(o);
    take_pointer(state);
    crate::backend::damage_all(state);
    true
}

/// Rebuild the cards after the set of windows or the desktop changed, keeping
/// the filter.
fn reload(state: &mut AbyssState) {
    let Some(old) = state.overview.open.take() else {
        return;
    };
    let id = old.output;
    state.overview.open = build(state, id, old.filter);
    if state.overview.open.is_none() {
        close_all(state);
    }
    crate::backend::damage_all(state);
}

fn overview_key(state: &mut AbyssState, sym: Keysym) {
    let Some(o) = &mut state.overview.open else {
        return;
    };
    match sym {
        Keysym::Escape => dismiss(state),
        Keysym::BackSpace => {
            if o.filter.pop().is_some() {
                refilter(state);
            }
        }
        Keysym::Return | Keysym::KP_Enter => {
            // The card under the pointer, else the first.
            let k = match o.hover {
                Hover::Card(k) => k,
                _ => 0,
            };
            if let Some(w) = o.shown.get(k).map(|&i| o.all[i].window.clone()) {
                dismiss(state);
                activate(state, &w);
            }
        }
        _ => {
            // Typed text, never logged: the filter is the human's input.
            let c = smithay::input::keyboard::xkb::keysym_to_utf32(sym);
            if let Some(c) = char::from_u32(c).filter(|c| *c != '\0' && !c.is_control()) {
                if o.filter.chars().count() < FILTER_MAX {
                    o.filter.push(c);
                    refilter(state);
                }
            }
        }
    }
}

fn refilter(state: &mut AbyssState) {
    let Some(mut o) = state.overview.open.take() else {
        return;
    };
    relayout(state, &mut o);
    state.overview.open = Some(o);
    crate::backend::damage_all(state);
}

fn overview_motion(state: &mut AbyssState, pos: Point<f64, Logical>) {
    let Some(o) = &mut state.overview.open else {
        return;
    };
    let next = if let Some(k) = o.placed.iter().position(|p| p.frame.to_f64().contains(pos)) {
        Hover::Card(k)
    } else if let Some(t) = o.tiles.iter().position(|r| r.to_f64().contains(pos)) {
        Hover::Tile(t)
    } else {
        Hover::Nothing
    };
    let mut changed = next != o.hover;
    o.hover = next;
    if let (Some(Press::Card(k, from)), None) = (o.press, o.dragging) {
        if pointer_distance(from, pos) > DRAG_THRESHOLD {
            o.dragging = Some(k);
            changed = true;
        }
    }
    if changed || o.dragging.is_some() {
        crate::backend::damage_all(state);
    }
}

fn overview_button(state: &mut AbyssState, button: u32, pressed: bool) {
    let pos = state.pointer_location;
    let Some(o) = &mut state.overview.open else {
        return;
    };
    let card = o.placed.iter().position(|p| p.frame.to_f64().contains(pos));
    let tile = o.tiles.iter().position(|r| r.to_f64().contains(pos));
    if pressed {
        match (button, card, tile) {
            (BTN_LEFT, Some(k), _) => o.press = Some(Press::Card(k, pos)),
            (BTN_LEFT, None, Some(t)) => o.press = Some(Press::Tile(t)),
            (BTN_MIDDLE, Some(k), _) => {
                // Middle-click closes the window; its card goes when it does.
                if let Some(w) = o.shown.get(k).map(|&i| o.all[i].window.clone()) {
                    shell::close_window(state, &w);
                }
            }
            // A click on nothing leaves the overview, and its release goes
            // nowhere.
            (BTN_LEFT, None, None) => {
                state.bar_ui.swallow(button);
                dismiss(state);
            }
            _ => {}
        }
        return;
    }
    if button != BTN_LEFT {
        return;
    }
    let press = o.press.take();
    let dragged = o.dragging.take();
    match (press, dragged) {
        (Some(Press::Card(k, _)), Some(_)) => {
            let window = o.shown.get(k).map(|&i| o.all[i].window.clone());
            let here = o.desk;
            if let (Some(window), Some(t)) = (window, tile) {
                if t != here {
                    shell::move_window_to_workspace(state, &window, t + 1);
                    reload(state);
                    return;
                }
            }
            crate::backend::damage_all(state);
        }
        (Some(Press::Card(k, _)), None) if card == Some(k) => {
            if let Some(w) = o.shown.get(k).map(|&i| o.all[i].window.clone()) {
                dismiss(state);
                activate(state, &w);
            }
        }
        (Some(Press::Tile(t)), _) if tile == Some(t) => {
            if t != o.desk {
                shell::switch_workspace(state, t + 1);
            }
            reload(state);
        }
        _ => crate::backend::damage_all(state),
    }
}

fn overview_frame(state: &AbyssState) -> Option<Frame> {
    let o = state.overview.open.as_ref()?;
    let entry = state.outputs.get(o.output)?;
    let scale = entry.output.current_scale().fractional_scale();
    let mut cards = Vec::with_capacity(o.shown.len());
    let mut wanted = Vec::with_capacity(o.shown.len());
    let mut chip = None;
    for (k, &i) in o.shown.iter().enumerate() {
        let e = &o.all[i];
        // Decided afresh every frame, before any GPU work for this window.
        let Some(shown) = thumbs::shown(state, &e.window) else {
            continue;
        };
        let view = thumbs::card(
            &shown,
            e.handle,
            o.placed[k],
            false,
            o.hover == Hover::Card(k),
            false,
        );
        if o.dragging == Some(k) {
            chip = Some(DragView {
                at: state.pointer_location.to_i32_round(),
                label: shown.title.clone(),
            });
        }
        wanted.push(Wanted {
            window: e.window.clone(),
            handle: e.handle,
            size: thumb_px(view.thumb.size, scale),
            shown,
        });
        cards.push(view);
    }
    let over = match o.hover {
        Hover::Tile(t) if o.dragging.is_some() => Some(t),
        _ => None,
    };
    let tiles = o
        .tiles
        .iter()
        .enumerate()
        .map(|(t, r)| TileView {
            rect: *r,
            index: t,
            current: t == o.desk,
            hover: o.hover == Hover::Tile(t),
            target: over == Some(t),
            windows: o.counts.get(t).copied().unwrap_or(0),
        })
        .collect();
    Some(Frame {
        output: o.output,
        view: OverlayView {
            output_rect: o.output_rect,
            dim: true,
            panel: None,
            strip: Some(o.strip),
            cards,
            tiles,
            filter: Some(o.filter.clone()),
            drag: chip,
        },
        wanted,
    })
}

// -------------------------------------------------------------- routing and glue

fn thumb_px(size: Size<i32, Logical>, scale: f64) -> Size<i32, smithay::utils::Physical> {
    Size::from((
        ((f64::from(size.w) * scale).ceil() as i32).max(1),
        ((f64::from(size.h) * scale).ceil() as i32).max(1),
    ))
}

/// One frame of whatever is open.
pub(crate) struct Frame {
    pub output: u64,
    pub view: OverlayView,
    pub wanted: Vec<Wanted>,
}

pub(crate) fn frame(state: &AbyssState) -> Option<Frame> {
    switcher::frame(state)
        .or_else(|| overview_frame(state))
        .or_else(|| preview::frame(state))
}

/// A key while the switcher or the overview is open (the input filter has
/// already taken it from every client and binding). `sym` is the keysym as
/// typed.
pub fn key(state: &mut AbyssState, sym: Keysym, shift: bool) {
    if state.switcher.is_open() {
        switcher::key(state, sym, shift);
    } else {
        overview_key(state, sym);
    }
}

/// The pointer moved while the switcher or the overview is open.
pub fn motion(state: &mut AbyssState, pos: Point<f64, Logical>) {
    crate::contain::fault(crate::contain::Feature::Overview);
    if state.switcher.is_open() {
        switcher::motion(state, pos);
    } else {
        overview_motion(state, pos);
    }
}

/// A pointer button while the switcher or the overview is open. Always
/// consumed.
pub fn button(state: &mut AbyssState, button: u32, pressed: bool) {
    if state.switcher.is_open() {
        switcher::button(state, button, pressed);
    } else {
        overview_button(state, button, pressed);
    }
}

/// Close the overlays if what they stand on is gone: the session locked, the
/// mode left, a prompt or the selector took the seat, their output removed,
/// their windows closed. Run at the top of every layout pass; never re-enters
/// focus.
pub(crate) fn validate(state: &mut AbyssState) {
    if !any_open(state) {
        return;
    }
    let gone = |state: &AbyssState, id: u64| state.outputs.get(id).is_none();
    let stale = !allowed(state)
        || state.switcher.output().is_some_and(|id| gone(state, id))
        || state.overview.output().is_some_and(|id| gone(state, id));
    if stale {
        tracing::info!("overlay dismissed: what it stood on changed");
        dismiss(state);
        return;
    }
    switcher::validate(state);
    preview::validate(state);
    if let Some(o) = &state.overview.open {
        let entry = state.outputs.get(o.output);
        let same = entry.is_some_and(|e| e.active == o.desk)
            && desktop_windows(state, o.output) == o.signature
            && entry.is_some_and(|e| {
                e.workspaces
                    .iter()
                    .map(|w| w.all_windows().len())
                    .eq(o.counts.iter().copied())
            });
        // A window that became protected must lose its card.
        let withheld = o.all.iter().any(|e| thumbs::shown(state, &e.window).is_none());
        if !same || withheld {
            reload(state);
        }
    }
}

/// An output was removed: close whatever was open on it. (`validate` finds it
/// too; this is for the caller that knows.)
pub fn output_removed(state: &mut AbyssState, id: u64) {
    let on_it = state.switcher.output() == Some(id)
        || state.overview.output() == Some(id)
        || state.preview.output() == Some(id);
    if on_it {
        dismiss(state);
    }
}

/// What `output` draws this frame: the switcher, the overview or a hover
/// preview, whichever is open and on it. Decided from the state alone (every
/// window judged by [`thumbs::shown`] here), before any GPU work; the backend
/// that cannot lend the whole state to its renderer calls this first and
/// [`draw`] after.
pub fn plan(state: &AbyssState, output: &Output) -> Option<FramePlan> {
    let here = state.outputs.by_output(output).map(|e| e.id)?;
    frame(state).filter(|f| f.output == here).map(FramePlan)
}

/// The pieces of the state [`draw`] works on: a backend whose renderer is
/// borrowed out of the state lends these fields and nothing else.
pub struct Parts<'a> {
    pub thumbs: &'a mut thumbs::Thumbs,
    pub cache: &'a mut ec_abyss_render::overview::OverlayCache,
    pub handle: &'a LoopHandle<'static, AbyssState>,
}

/// A frame decided by [`plan`].
pub struct FramePlan(Frame);

/// The overlay's elements for `output` from a [`plan`]. Thumbnails are brought
/// up to date first, each only for a window the plan cleared for pixels; with
/// nothing open (`open` false) every texture is dropped. Level with the window
/// menu: after the windows, before annotations and trusted UI, never part of a
/// capture pass list.
pub fn draw(
    renderer: &mut GlesRenderer,
    parts: Parts<'_>,
    plan: Option<&FramePlan>,
    open: bool,
    output: &Output,
    output_loc: Point<i32, Logical>,
) -> Vec<AbyssRenderElement> {
    let Parts {
        thumbs,
        cache,
        handle,
    } = parts;
    if !open {
        // Nothing to draw and nothing to keep.
        if !thumbs.store.is_empty() || !cache.is_empty() {
            thumbs.release();
            cache.clear();
        }
        return Vec::new();
    }
    if let Some(FramePlan(f)) = plan {
        thumbs::refresh(renderer, thumbs, handle, &f.wanted, Instant::now());
    }
    ec_abyss_render::overview::overlay_elements(
        renderer,
        cache,
        &thumbs.store,
        plan.map(|p| &p.0.view),
        output,
        output_loc,
    )
}

/// [`plan`] and [`draw`] for a backend whose renderer does not borrow the
/// state.
pub fn overlay_elements(
    renderer: &mut GlesRenderer,
    state: &mut AbyssState,
    output: &Output,
    output_loc: Point<i32, Logical>,
) -> Vec<AbyssRenderElement> {
    let planned = plan(state, output);
    let open = any_open(state);
    draw(
        renderer,
        Parts {
            thumbs: &mut state.thumbs,
            cache: &mut state.overlay_cache,
            handle: &state.loop_handle,
        },
        planned.as_ref(),
        open,
        output,
        output_loc,
    )
}
