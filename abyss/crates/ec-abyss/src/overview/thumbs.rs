// SPDX-License-Identifier: AGPL-3.0-only
//! Thumbnails for the switcher, the overview and hover previews (COMP-05
//! §7.1, C-24, ADR 0090): which window may be drawn, how, and when its texture
//! is redrawn.
//!
//! **Redaction first.** [`shown`] asks `render::capture::classify` -- the one
//! decision capture uses -- before anything else about a window is read, and
//! nothing touches the GPU for a window it did not clear:
//!
//! - `Protected` (COMP-19): no card at all ([`shown`] returns `None`).
//! - `Whole`, and `Nodes` whose reason is `NodesOutOfBounds` (the surface is
//!   not drawn even in capture): a [`Face::Hidden`] placeholder, no pixels and
//!   no title. Its texture, if one was held, is dropped.
//! - `Nodes`: the thumbnail, with an opaque cover over each rect.
//! - `Clear`: the thumbnail.
//!
//! The title is sanitised where it is drawn, by the titlebar's own
//! `display_title`. A hidden card carries no title, so there is none to draw.
//!
//! **Cost.** Nothing runs while nothing is open. While something is, a
//! window's texture is redrawn when its surface tree has committed since it
//! was last drawn, no more than ten times a second ([`MIN_INTERVAL`]), and
//! textures are dropped as soon as no card shows them.

use std::time::{Duration, Instant};

use smithay::{
    backend::renderer::gles::GlesRenderer,
    desktop::Window,
    reexports::{
        calloop::{
            timer::{TimeoutAction, Timer},
            LoopHandle, RegistrationToken,
        },
        wayland_server::protocol::wl_surface::WlSurface,
    },
    utils::{Logical, Physical, Point, Rectangle, Size},
    wayland::compositor::{with_surface_tree_downward, TraversalAction},
};

use ec_abyss_render::{
    overview::{cover_in_thumb, Body, CardView, Placed},
    thumb::{Job, ThumbStore},
};

use crate::{
    render::capture::{classify, Classified, RedactReason},
    state::AbyssState,
};

/// The most often one window's thumbnail is redrawn: ten times a second.
pub const MIN_INTERVAL: Duration = Duration::from_millis(100);

/// What a window may show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Face {
    /// Its pixels, with an opaque cover over each rect (global logical, in the
    /// space of [`Shown::global`]).
    Pixels { covers: Vec<Rectangle<i32, Logical>> },
    /// A blank placeholder: no pixels and no title.
    Hidden,
}

/// A window cleared to appear in a compositor-drawn copy.
#[derive(Debug, Clone)]
pub struct Shown {
    pub face: Face,
    /// The window's raw title, `None` for a hidden face or an untitled window.
    pub title: Option<String>,
    /// The region drawn, surface-local logical (the window's bounding box).
    pub local: Rectangle<i32, Logical>,
    /// The same region where the covers' coordinates put it.
    pub global: Rectangle<i32, Logical>,
}

impl Shown {
    pub fn size(&self) -> Size<i32, Logical> {
        self.local.size
    }
}

/// Decide what `window` may show. Called before any GPU work for it, every
/// frame it is drawn. `None` means no card at all.
pub fn shown(state: &AbyssState, window: &Window) -> Option<Shown> {
    // A window the space does not hold (another desktop, minimized) is judged
    // as if placed at the origin: only the rects relative to it matter here.
    let geo = state
        .space
        .element_geometry(window)
        .unwrap_or_else(|| Rectangle::new(Point::default(), window.geometry().size));
    let class = classify(state, window, geo);
    let bbox = window.bbox();
    let origin = geo.loc - window.geometry().loc;
    let global = Rectangle::new(origin + bbox.loc, bbox.size);
    let (face, title) = match class {
        Classified::Protected => return None,
        Classified::Whole { .. }
        | Classified::Nodes {
            reason: RedactReason::NodesOutOfBounds,
            ..
        } => (Face::Hidden, None),
        Classified::Nodes { rects, .. } => (Face::Pixels { covers: rects }, title_of(window)),
        Classified::Clear => (Face::Pixels { covers: Vec::new() }, title_of(window)),
    };
    Some(Shown {
        face,
        title,
        local: bbox,
        global,
    })
}

fn title_of(window: &Window) -> Option<String> {
    Some(ec_abyss_render::titlebar::title_of(window)).filter(|t| !t.is_empty())
}

/// A card for a window cleared by [`shown`], placed at `placed`.
pub fn card(
    shown: &Shown,
    id: u64,
    placed: Placed,
    minimized: bool,
    hover: bool,
    selected: bool,
) -> CardView {
    let body = match &shown.face {
        Face::Hidden => Body::Hidden,
        Face::Pixels { covers } => Body::Pixels {
            covers: covers
                .iter()
                .filter_map(|c| cover_in_thumb(shown.global, *c, placed.thumb))
                .collect(),
        },
    };
    CardView {
        id,
        frame: placed.frame,
        thumb: placed.thumb,
        title: shown.title.clone(),
        body,
        hover,
        selected,
        minimized,
    }
}

/// When a thumbnail is next drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    /// Draw it this frame.
    Now,
    /// Changed, but drawn too recently: try again after this long.
    Later(Duration),
    /// Nothing changed.
    Fresh,
}

/// The refresh rule. `last` is what the store holds for the window: the change
/// key and time of the last draw and its size; `commit` and `size` are the
/// window's now.
pub fn due(
    last: Option<(u64, Instant, Size<i32, Physical>)>,
    commit: u64,
    size: Size<i32, Physical>,
    now: Instant,
) -> Due {
    let Some((seen, at, drawn)) = last else {
        return Due::Now;
    };
    if drawn != size {
        return Due::Now;
    }
    if seen == commit {
        return Due::Fresh;
    }
    let since = now.saturating_duration_since(at);
    if since >= MIN_INTERVAL {
        Due::Now
    } else {
        Due::Later(MIN_INTERVAL - since)
    }
}

/// A key that changes whenever any surface of `surface`'s tree commits.
pub fn commit_key(surface: &WlSurface) -> u64 {
    use smithay::backend::renderer::utils::{CommitCounter, RendererSurfaceStateUserData};
    let mut sum = 0u64;
    with_surface_tree_downward(
        surface,
        (),
        |_, _, ()| TraversalAction::DoChildren(()),
        |_, states, ()| {
            if let Some(data) = states.data_map.get::<RendererSurfaceStateUserData>() {
                let n = data
                    .lock()
                    .ok()
                    .and_then(|s| s.current_commit().distance(Some(CommitCounter::default())))
                    .unwrap_or(0);
                sum = sum.wrapping_add(n as u64).wrapping_add(1);
            }
        },
        |_, _, ()| true,
    );
    sum
}

/// The thumbnails' store and the timer that wakes a frame for a rate-limited
/// redraw.
#[derive(Default)]
pub struct Thumbs {
    pub store: ThumbStore,
    retry: Option<RegistrationToken>,
}

impl std::fmt::Debug for Thumbs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Thumbs").field("store", &self.store).finish()
    }
}

impl Thumbs {
    /// Nothing shows a thumbnail: drop every texture.
    pub fn release(&mut self) {
        self.store.clear();
    }

    /// Whether a retry timer is pending.
    pub fn retry_pending(&self) -> bool {
        self.retry.is_some()
    }
}

/// A window to draw this frame, with the size its texture must have.
pub struct Wanted {
    pub window: Window,
    pub handle: u64,
    pub shown: Shown,
    pub size: Size<i32, Physical>,
}

/// Which of `wanted` may be drawn and which must have no texture. A window
/// whose face is not `Pixels` is in the second list and never in the first:
/// this is the gate [`refresh`] draws through.
pub fn split(wanted: &[Wanted]) -> (Vec<&Wanted>, Vec<u64>) {
    let (draw, drop): (Vec<&Wanted>, Vec<&Wanted>) = wanted
        .iter()
        .partition(|w| matches!(w.shown.face, Face::Pixels { .. }));
    (draw, drop.into_iter().map(|w| w.handle).collect())
}

/// Bring `wanted`'s textures up to date: draw those that are due, leave the
/// rest, drop the textures of every window not listed (and of any whose face is
/// hidden). Arms one timer when a redraw was deferred by the rate limit.
pub fn refresh(
    renderer: &mut GlesRenderer,
    thumbs: &mut Thumbs,
    handle: &LoopHandle<'static, AbyssState>,
    wanted: &[Wanted],
    now: Instant,
) {
    let mut later: Option<Duration> = None;
    // Only a window `shown` cleared for pixels is ever drawn: a hidden face
    // has its texture dropped and goes no further.
    let (cleared, hidden) = split(wanted);
    for key in hidden {
        thumbs.store.remove(key);
    }
    for w in &cleared {
        let Some(surface) = crate::shell::window_surface(&w.window) else {
            thumbs.store.remove(w.handle);
            continue;
        };
        let commit = commit_key(&surface);
        match due(thumbs.store.state_of(w.handle), commit, w.size, now) {
            Due::Fresh => {}
            Due::Later(d) => later = Some(later.map_or(d, |l| l.min(d))),
            Due::Now => {
                thumbs.store.render(
                    renderer,
                    &Job {
                        key: w.handle,
                        surface: &surface,
                        src: w.shown.local,
                        size: w.size,
                        commit,
                        now,
                    },
                );
            }
        }
    }
    thumbs.store.retain(|k| cleared.iter().any(|w| w.handle == k));
    if let Some(d) = later {
        if thumbs.retry.is_none() {
            let armed = handle.insert_source(Timer::from_duration(d), |_, _, state| {
                state.thumbs.retry = None;
                crate::backend::damage_all(state);
                TimeoutAction::Drop
            });
            thumbs.retry = armed.ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sz(w: i32, h: i32) -> Size<i32, Physical> {
        Size::from((w, h))
    }

    #[test]
    fn a_window_never_drawn_is_drawn_now() {
        assert_eq!(due(None, 1, sz(10, 10), Instant::now()), Due::Now);
    }

    #[test]
    fn an_unchanged_window_is_left_alone_however_old() {
        let t = Instant::now();
        let last = Some((5, t, sz(10, 10)));
        assert_eq!(due(last, 5, sz(10, 10), t + Duration::from_secs(60)), Due::Fresh);
    }

    #[test]
    fn a_changed_window_is_drawn_at_most_ten_times_a_second() {
        let t = Instant::now();
        let last = Some((5, t, sz(10, 10)));
        assert_eq!(
            due(last, 6, sz(10, 10), t + Duration::from_millis(30)),
            Due::Later(Duration::from_millis(70))
        );
        assert_eq!(due(last, 6, sz(10, 10), t + Duration::from_millis(100)), Due::Now);
        // Simulate a client committing every millisecond for a second: the
        // thumbnail is drawn ten times, not a thousand.
        let mut held = Some((0u64, t, sz(10, 10)));
        let mut draws = 0;
        for ms in 1..=1000u64 {
            let now = t + Duration::from_millis(ms);
            if due(held, ms, sz(10, 10), now) == Due::Now {
                draws += 1;
                held = Some((ms, now, sz(10, 10)));
            }
        }
        assert_eq!(draws, 10);
    }

    #[test]
    fn a_new_size_redraws_at_once() {
        let t = Instant::now();
        assert_eq!(due(Some((5, t, sz(10, 10))), 5, sz(20, 20), t), Due::Now);
    }
}
