// SPDX-License-Identifier: AGPL-3.0-only
//! The Alt-Tab window switcher under `mode "de"` (COMP-05 §7.1, C-24, ADR 0090).
//!
//! **Order.** Most recently used first, across every desktop of the focused
//! output (KDE's default), minimized windows last. Each window carries the
//! stamp of its own last focus, so windows of different desktops interleave
//! by true recency. Agent workspaces
//! (COMP-05 §3.2) do not exist yet and every workspace is a human one; when
//! they do they are excluded here. A window `thumbs::shown` withholds
//! entirely (protected) has no card and cannot be chosen.
//!
//! **Keys.** The first press of the chord opens it on the previous window
//! (Alt+Shift+Tab on the last). While the modifiers of the chord stay down,
//! Tab steps (Shift reverses), the arrow keys step, Escape cancels with
//! nothing changed. Releasing the modifiers, or Enter, activates the
//! selection: switch to its desktop, unminimize it, focus it. A switcher
//! opened with no modifier held (a hot corner) stays until Enter, a click or
//! Escape. It holds no keyboard grab: the input filter takes the keys, and
//! under a client keyboard grab the binds behave exactly as they always do.
//!
//! **Closing mid-switch.** A window that closes while it is open drops out of
//! the list. If it was selected the selection stays at the same position, so
//! it falls on the window that followed it (clamped at the end); no other
//! window is picked.

use smithay::{
    desktop::Window,
    input::keyboard::Keysym,
    utils::{IsAlive, Logical, Point, Rectangle},
};

use ec_abyss_render::overview::{switcher_layout, OverlayView, Placed, MARGIN};

use ec_abyss_config::input::Mods;

use crate::{shell, state::AbyssState};

use super::{thumbs, Entry, Frame};

type Rect = Rectangle<i32, Logical>;

const BTN_LEFT: u32 = 0x110;

#[derive(Debug, Clone)]
pub(crate) struct SwitcherOpen {
    output: u64,
    output_rect: Rect,
    order: Vec<Entry>,
    selected: usize,
    panel: Rect,
    placed: Vec<Placed>,
    /// The modifiers of the chord that opened it: releasing any of them
    /// activates the selection. Empty when opened without a chord.
    hold: Mods,
}

/// The open switcher. Plain data owned by `AbyssState`.
#[derive(Debug, Default)]
pub struct Switcher {
    pub(crate) open: Option<SwitcherOpen>,
}

impl Switcher {
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    pub fn output(&self) -> Option<u64> {
        self.open.as_ref().map(|o| o.output)
    }

    /// The windows listed, in order.
    pub fn windows(&self) -> Vec<Window> {
        self.open
            .as_ref()
            .map_or_else(Vec::new, |o| o.order.iter().map(|e| e.window.clone()).collect())
    }

    pub fn selected(&self) -> Option<usize> {
        self.open.as_ref().map(|o| o.selected)
    }

    pub fn selected_window(&self) -> Option<Window> {
        self.open
            .as_ref()
            .and_then(|o| o.order.get(o.selected))
            .map(|e| e.window.clone())
    }

    pub fn placed(&self) -> &[Placed] {
        self.open.as_ref().map_or(&[], |o| o.placed.as_slice())
    }

    pub fn panel(&self) -> Option<Rect> {
        self.open.as_ref().map(|o| o.panel)
    }

    /// Record the modifiers of the chord that opened it (the input filter
    /// knows them, the action does not).
    pub fn set_hold(&mut self, hold: Mods) {
        if let Some(o) = &mut self.open {
            o.hold = hold;
        }
    }

    /// Whether `held` -- the modifiers down after a key release -- ends the
    /// chord that opened it: a modifier of the chord is no longer down.
    pub fn releases(&self, held: Mods) -> bool {
        self.open.as_ref().is_some_and(|o| {
            o.hold != Mods::default()
                && ((o.hold.alt && !held.alt)
                    || (o.hold.ctrl && !held.ctrl)
                    || (o.hold.logo && !held.logo)
                    || (o.hold.shift && !held.shift && !o.hold.alt && !o.hold.ctrl && !o.hold.logo))
        })
    }
}

/// Windows of output `id` in switching order: live windows by recency across
/// its desktops, then the minimized ones.
pub fn mru(state: &AbyssState, id: u64) -> Vec<Window> {
    let Some(entry) = state.outputs.get(id) else {
        return Vec::new();
    };
    let mut live: Vec<(u64, Window)> = Vec::new();
    for ws in &entry.workspaces {
        for w in ws.recent_live() {
            live.push((ws.focus_stamp_of(&w), w));
        }
    }
    // True per-window recency; never-focused windows (stamp 0) keep desktop
    // order, the sort being stable.
    live.sort_by_key(|e| std::cmp::Reverse(e.0));
    let order: Vec<usize> = (0..entry.workspaces.len()).collect();
    let mut out: Vec<Window> = Vec::new();
    let push = |w: Window, out: &mut Vec<Window>| {
        if !out.contains(&w) && !shell::is_unmanaged_x11(state, &w) {
            out.push(w);
        }
    };
    for (_, w) in live {
        push(w, &mut out);
    }
    for &i in &order {
        for m in entry.workspaces[i].minimized.iter().rev() {
            push(m.window.clone(), &mut out);
        }
    }
    out
}

fn relayout(o: &mut SwitcherOpen) {
    let area = Rectangle::new(
        o.output_rect.loc + Point::from((MARGIN, MARGIN)),
        (
            o.output_rect.size.w - 2 * MARGIN,
            o.output_rect.size.h - 2 * MARGIN,
        )
            .into(),
    );
    let sources: Vec<_> = o.order.iter().map(|e| e.size).collect();
    let (panel, placed) = switcher_layout(&sources, area);
    o.panel = panel;
    o.placed = placed;
}

/// Open the switcher on the focused output. `reverse` opens on the least
/// recently used window. False, and nothing opens, when the mode is not
/// `de`, the session is locked, a prompt or the region selector holds the
/// seat, a grab or drag is under way, or there is no window to switch to.
pub fn open(state: &mut AbyssState, reverse: bool) -> bool {
    if !super::allowed(state) || crate::input::grabs::drag_active(state) {
        return false;
    }
    let Some(id) = state.outputs.focused().map(|e| e.id) else {
        return false;
    };
    let Some(output_rect) = state
        .outputs
        .get(id)
        .and_then(|e| state.space.output_geometry(&e.output))
    else {
        return false;
    };
    let windows = mru(state, id);
    let order = super::entries(state, &windows);
    if order.is_empty() {
        return false;
    }
    // The first press goes to the window before the current one; with no
    // window focused (an empty desktop) nothing is current, so it goes to
    // the most recent.
    let current_leads = state.focus.as_ref() == order.first().map(|e| &e.window);
    let selected = match order.len() {
        1 => 0,
        n if reverse => n - 1,
        _ if current_leads => 1,
        _ => 0,
    };
    super::make_room(state);
    state.corners.disarm(&state.loop_handle);
    let mut o = SwitcherOpen {
        output: id,
        output_rect,
        order,
        selected,
        panel: Rectangle::default(),
        placed: Vec::new(),
        hold: Mods::default(),
    };
    relayout(&mut o);
    tracing::info!(windows = o.order.len(), "window switcher opened");
    state.switcher.open = Some(o);
    super::take_pointer(state);
    crate::backend::damage_all(state);
    true
}

/// The `switch-windows` bind: open the switcher, or step an open one.
pub fn step_or_open(state: &mut AbyssState, reverse: bool) {
    if state.switcher.is_open() {
        step(state, if reverse { -1 } else { 1 });
    } else {
        open(state, reverse);
    }
}

/// Move the selection `delta` places, wrapping.
pub fn step(state: &mut AbyssState, delta: i32) {
    let Some(o) = &mut state.switcher.open else {
        return;
    };
    let n = o.order.len() as i32;
    if n > 0 {
        o.selected = (o.selected as i32 + delta).rem_euclid(n) as usize;
        crate::backend::damage_all(state);
    }
}

/// Close it with nothing changed.
pub fn cancel(state: &mut AbyssState) {
    if state.switcher.is_open() {
        tracing::info!("window switcher cancelled");
        super::dismiss(state);
    }
}

/// Close it and activate the selection: its desktop, unminimized, focused.
pub fn commit(state: &mut AbyssState) {
    let Some(window) = state.switcher.selected_window() else {
        return;
    };
    super::dismiss(state);
    if window.alive() {
        super::activate(state, &window);
    }
}

pub(super) fn key(state: &mut AbyssState, sym: Keysym, shift: bool) {
    match sym {
        Keysym::Tab => step(state, if shift { -1 } else { 1 }),
        Keysym::ISO_Left_Tab | Keysym::Left | Keysym::Up => step(state, -1),
        Keysym::Right | Keysym::Down => step(state, 1),
        Keysym::Escape => cancel(state),
        Keysym::Return | Keysym::KP_Enter | Keysym::space => commit(state),
        _ => {}
    }
}

pub(super) fn motion(state: &mut AbyssState, pos: Point<f64, Logical>) {
    let Some(o) = &mut state.switcher.open else {
        return;
    };
    if let Some(k) = o.placed.iter().position(|p| p.frame.to_f64().contains(pos)) {
        if o.selected != k {
            o.selected = k;
            crate::backend::damage_all(state);
        }
    }
}

pub(super) fn button(state: &mut AbyssState, button: u32, pressed: bool) {
    if !pressed {
        return;
    }
    let pos = state.pointer_location;
    let Some(o) = &mut state.switcher.open else {
        return;
    };
    // The release of this press goes nowhere.
    let hit = o.placed.iter().position(|p| p.frame.to_f64().contains(pos));
    state.bar_ui.swallow(button);
    match (button, hit) {
        (BTN_LEFT, Some(k)) => {
            if let Some(o) = &mut state.switcher.open {
                o.selected = k;
            }
            commit(state);
        }
        (_, None) => cancel(state),
        _ => {}
    }
}

/// Drop windows that are gone, keeping the selection where it was.
pub(super) fn validate(state: &mut AbyssState) {
    let Some(mut o) = state.switcher.open.take() else {
        return;
    };
    let before = o.order.len();
    let mut kept = 0;
    let mut selected = o.selected;
    let mut i = 0;
    o.order.retain(|e| {
        // A window that became protected has no card and cannot be chosen.
        let keep = e.window.alive()
            && shell::output_of_window(state, &e.window).is_some()
            && thumbs::shown(state, &e.window).is_some();
        if !keep && i < o.selected {
            selected -= 1;
        }
        if keep {
            kept += 1;
        }
        i += 1;
        keep
    });
    if kept == before {
        state.switcher.open = Some(o);
        return;
    }
    if o.order.is_empty() {
        super::dismiss(state);
        return;
    }
    o.selected = selected.min(o.order.len() - 1);
    relayout(&mut o);
    state.switcher.open = Some(o);
    crate::backend::damage_all(state);
}

pub(super) fn frame(state: &AbyssState) -> Option<Frame> {
    let o = state.switcher.open.as_ref()?;
    let scale = state
        .outputs
        .get(o.output)?
        .output
        .current_scale()
        .fractional_scale();
    let mut cards = Vec::with_capacity(o.order.len());
    let mut wanted = Vec::with_capacity(o.order.len());
    for (k, e) in o.order.iter().enumerate() {
        // Decided afresh every frame, before any GPU work for this window.
        let Some(shown) = thumbs::shown(state, &e.window) else {
            continue;
        };
        let minimized = shell::is_minimized(state, &e.window);
        let card = thumbs::card(&shown, e.handle, o.placed[k], minimized, false, k == o.selected);
        wanted.push(thumbs::Wanted {
            window: e.window.clone(),
            handle: e.handle,
            size: super::thumb_px(card.thumb.size, scale),
            shown,
        });
        cards.push(card);
    }
    Some(Frame {
        output: o.output,
        view: OverlayView {
            output_rect: o.output_rect,
            dim: false,
            panel: Some(o.panel),
            strip: None,
            cards,
            tiles: Vec::new(),
            filter: None,
            drag: None,
        },
        wanted,
    })
}
