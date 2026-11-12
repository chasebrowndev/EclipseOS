// SPDX-License-Identifier: AGPL-3.0-only
//! Hover previews for the taskbar (COMP-05 §7.1, C-24, ADR 0090): a thumbnail
//! card of a window, anchored near the chip the pointer is over.
//!
//! The socket methods `show_window_preview` and `hide_window_preview` drive
//! it. It holds no input: no keyboard, no pointer, no grab. It is dismissed by
//! `hide`, by the window closing, by focus moving to the window, by the lock,
//! by leaving `de`, by its output going, and by a 10 s idle timeout (a bar
//! that died with its preview up). Redaction is the switcher's: a protected
//! window gets no preview at all ([`show`] returns `Ok(false)`), a redacted one
//! a blank card with no title.

use std::time::Duration;

use smithay::{
    desktop::Window,
    reexports::calloop::{
        timer::{TimeoutAction, Timer},
        RegistrationToken,
    },
    utils::{IsAlive, Logical, Point, Rectangle},
};

use ec_abyss_render::overview::{preview_layout, OverlayView, Placed};

use crate::{shell, state::AbyssState};

use super::{thumbs, Frame};

type Rect = Rectangle<i32, Logical>;

/// How long a preview stays when nobody hides it.
pub const IDLE: Duration = Duration::from_secs(10);

#[derive(Debug)]
struct PreviewOpen {
    window: Window,
    handle: u64,
    output: u64,
    output_rect: Rect,
    anchor: Rect,
    placed: Placed,
    /// Who held focus when it opened: focus *moving to* the window ends it.
    focus_at_open: Option<Window>,
    timer: Option<RegistrationToken>,
    /// Which showing the idle timer belongs to.
    serial: u64,
}

/// The shown preview, if any. Plain data owned by `AbyssState`.
#[derive(Debug, Default)]
pub struct Preview {
    open: Option<PreviewOpen>,
    serial: u64,
}

impl Preview {
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    pub fn window(&self) -> Option<&Window> {
        self.open.as_ref().map(|o| &o.window)
    }

    pub fn output(&self) -> Option<u64> {
        self.open.as_ref().map(|o| o.output)
    }

    /// Where the card is drawn.
    pub fn placed(&self) -> Option<Placed> {
        self.open.as_ref().map(|o| o.placed)
    }

    pub fn anchor(&self) -> Option<Rect> {
        self.open.as_ref().map(|o| o.anchor)
    }

    /// The serial of the showing the idle timer would end.
    pub fn serial(&self) -> u64 {
        self.open.as_ref().map_or(0, |o| o.serial)
    }
}

/// Drop the preview without damage or pointer work: another overlay is
/// opening, or everything is closing.
pub(super) fn close_quietly(state: &mut AbyssState) {
    if let Some(o) = state.preview.open.take() {
        if let Some(t) = o.timer {
            state.loop_handle.remove(t);
        }
    }
}

/// Show `window`'s preview near `anchor` (global logical). `Ok(true)` when it
/// is up, `Ok(false)` when the window must have no card (protected). An error
/// names why nothing was shown: the mode is not `de`, the session is locked or
/// a prompt holds the seat, the window is gone or on no output, or the
/// switcher or overview is open.
pub fn show(state: &mut AbyssState, window: &Window, anchor: Rect) -> Result<bool, &'static str> {
    if !super::allowed(state) {
        return Err("previews need mode \"de\" and a live session");
    }
    if super::holds_keys(state) {
        return Err("the switcher or the overview is open");
    }
    if !window.alive() {
        return Err("no such window");
    }
    let id = shell::output_of_window(state, window).ok_or("window is not on an output")?;
    let Some(shown) = thumbs::shown(state, window) else {
        // Protected: no card, and nothing left of an earlier one.
        close_quietly(state);
        return Ok(false);
    };
    // The output the anchor is on, else the window's own.
    let (id, output_rect) = state
        .outputs
        .iter()
        .filter_map(|e| state.space.output_geometry(&e.output).map(|g| (e.id, g)))
        .find(|(_, g)| {
            g.to_f64().contains(Point::from((
                f64::from(anchor.loc.x + anchor.size.w / 2),
                f64::from(anchor.loc.y + anchor.size.h / 2),
            )))
        })
        .or_else(|| {
            state
                .outputs
                .get(id)
                .and_then(|e| state.space.output_geometry(&e.output))
                .map(|g| (id, g))
        })
        .ok_or("window's output has no geometry")?;
    let placed = preview_layout(shown.size(), anchor, output_rect);
    // Another showing replaces this one; the switcher, overview and menu are
    // already out of its way.
    state.winmenu.close();
    close_quietly(state);
    state.preview.serial += 1;
    let serial = state.preview.serial;
    let timer = state
        .loop_handle
        .insert_source(Timer::from_duration(IDLE), move |_, _, state| {
            idle_expired(state, serial);
            TimeoutAction::Drop
        })
        .ok();
    let handle = state.ipc.handle_for(window);
    state.preview.open = Some(PreviewOpen {
        window: window.clone(),
        handle,
        output: id,
        output_rect,
        anchor,
        placed,
        focus_at_open: state.focus.clone(),
        timer,
        serial,
    });
    crate::backend::damage_all(state);
    Ok(true)
}

/// Take the preview down. True when one was up.
pub fn hide(state: &mut AbyssState) -> bool {
    crate::contain::fault(crate::contain::Feature::Overview);
    let was = state.preview.is_open();
    close_quietly(state);
    if was {
        if !super::any_open(state) {
            state.thumbs.release();
            state.overlay_cache.clear();
        }
        crate::backend::damage_all(state);
    }
    was
}

/// The idle timer of showing `serial` fired.
pub fn idle_expired(state: &mut AbyssState, serial: u64) {
    if state.preview.open.as_ref().is_some_and(|o| o.serial == serial) {
        tracing::info!("window preview timed out");
        // The timer is the caller; it drops itself.
        if let Some(o) = &mut state.preview.open {
            o.timer = None;
        }
        hide(state);
    }
}

/// Drop the preview if what it stands on is gone: the window closed, focus
/// moved to it, its output went. (The lock and the mode are `super::validate`'s.)
pub(super) fn validate(state: &mut AbyssState) {
    let Some(o) = &state.preview.open else {
        return;
    };
    let focused_now = state.focus.as_ref() == Some(&o.window) && o.focus_at_open.as_ref() != Some(&o.window);
    let stale = !o.window.alive()
        || shell::output_of_window(state, &o.window).is_none()
        || state.outputs.get(o.output).is_none()
        || focused_now;
    if stale {
        hide(state);
    }
}

pub(super) fn frame(state: &AbyssState) -> Option<Frame> {
    let o = state.preview.open.as_ref()?;
    let scale = state
        .outputs
        .get(o.output)?
        .output
        .current_scale()
        .fractional_scale();
    // Decided afresh every frame, before any GPU work for this window.
    let shown = thumbs::shown(state, &o.window)?;
    let minimized = shell::is_minimized(state, &o.window);
    let card = thumbs::card(&shown, o.handle, o.placed, minimized, false, false);
    let wanted = vec![thumbs::Wanted {
        window: o.window.clone(),
        handle: o.handle,
        size: super::thumb_px(card.thumb.size, scale),
        shown,
    }];
    Some(Frame {
        output: o.output,
        view: OverlayView {
            output_rect: o.output_rect,
            dim: false,
            panel: None,
            strip: None,
            cards: vec![card],
            tiles: Vec::new(),
            filter: None,
            drag: None,
        },
        wanted,
    })
}
