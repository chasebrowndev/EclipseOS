// SPDX-License-Identifier: AGPL-3.0-only
//! Titlebars under `mode "de"`, shell side (COMP-05 §7.1, C-24, ADR 0090).
//!
//! Which windows have a bar, where it is, what the pointer is doing on it, and
//! what its buttons do. The drawing and the pure geometry are
//! `ec_abyss_render::titlebar`.
//!
//! **Geometry decision.** `Floating.rect` is the *outer* rect: the frame
//! (bar plus content) grown by the border on every side. A window without a bar
//! has `bar = 0` and the outer rect is what it always was, so `wm` and
//! `hybrid` are untouched. Everything that places a window (placement, snapping,
//! maximize, restore rects, clamping to the usable area) works on that outer
//! rect; the content rect is derived from it ([`content_of_outer`]) when
//! the client is configured and the window mapped. A mode flip does not touch
//! `rect`, so a flip round trip cannot make a window creep or shrink: only the
//! bar, and with it the content height, appears and goes.
//!
//! **Input.** A bar is the compositor's: no client is the pointer focus over
//! it ([`bar_at`] makes `surface_under` answer `None`). A press on it focuses
//! and raises the window and goes no further; a button acts on release over
//! the button it was pressed on. Every one of these paths sits behind the
//! session lock, the trusted-UI seat, the region selector, the trusted
//! indicators and the protected-surface gate (`input::pointer_button`), so a
//! bar is never a way past any of them.

use smithay::{
    backend::input::ButtonState,
    desktop::{Window, WindowSurface, WindowSurfaceType},
    utils::{Logical, Point, Rectangle},
};

pub use ec_abyss_render::titlebar::{
    bar_of, bar_rect, content_of_outer, outer_of_content, BarInfo, Button, Hit,
};
use ec_abyss_render::titlebar::{hit, set_bar};

use crate::state::AbyssState;

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

/// Two presses on a bar this close in time (ms) make a double-click. abyss has
/// no double-click setting to follow (no key in `docs/CONFIG.md`, and the
/// compositor has no toolkit settings to read it from), so this is the usual
/// desktop value.
pub const DOUBLE_CLICK_MS: u64 = 400;
/// ... and this close in space (logical px, per axis).
pub const DOUBLE_CLICK_SLOP: f64 = 6.0;
/// A press held on a bar and moved this far (logical px) becomes a window move.
pub const DRAG_THRESHOLD: f64 = 4.0;

/// Pairs presses into double-clicks. Generic over what was clicked so the
/// timing rules are testable without a window.
#[derive(Debug)]
pub struct ClickTracker<K> {
    last: Option<(K, u64, Point<f64, Logical>)>,
}

impl<K> Default for ClickTracker<K> {
    fn default() -> Self {
        ClickTracker { last: None }
    }
}

impl<K: PartialEq> ClickTracker<K> {
    /// A press on `key` at `now_ms` at `pos`. True when it completes a
    /// double-click (the pair is then used up: a third press starts over).
    pub fn press(&mut self, key: K, now_ms: u64, pos: Point<f64, Logical>) -> bool {
        let double = self.last.as_ref().is_some_and(|(k, t, p)| {
            *k == key
                && now_ms >= *t
                && now_ms - *t <= DOUBLE_CLICK_MS
                && (pos.x - p.x).abs() <= DOUBLE_CLICK_SLOP
                && (pos.y - p.y).abs() <= DOUBLE_CLICK_SLOP
        });
        self.last = if double { None } else { Some((key, now_ms, pos)) };
        double
    }

    pub fn clear(&mut self) {
        self.last = None;
    }

    pub fn is_of(&self, key: &K) -> bool {
        self.last.as_ref().is_some_and(|(k, ..)| k == key)
    }
}

/// What the pointer is doing on bars. Plain data owned by `AbyssState`.
#[derive(Debug, Default)]
pub struct BarUi {
    /// The bar (and button) under the pointer.
    hover: Option<(Window, Hit)>,
    /// The button a left press began on, until its release.
    press: Option<(Window, Button)>,
    /// Buttons whose press a bar swallowed, so their release is swallowed too.
    /// Fixed-size: the pointer path allocates nothing.
    swallowed: [u32; 4],
    /// A left press on the bar itself, until the pointer has moved far enough
    /// to be a window move (or the button comes up): the window and where it
    /// was pressed.
    drag: Option<(Window, Point<f64, Logical>)>,
    clicks: ClickTracker<Window>,
}

impl BarUi {
    pub fn forget(&mut self, window: &Window) {
        if self.hover.as_ref().is_some_and(|(w, _)| w == window) {
            self.hover = None;
        }
        if self.press.as_ref().is_some_and(|(w, _)| w == window) {
            self.press = None;
        }
        if self.drag.as_ref().is_some_and(|(w, _)| w == window) {
            self.drag = None;
        }
        if self.clicks.is_of(window) {
            self.clicks.clear();
        }
    }

    pub(super) fn swallow(&mut self, button: u32) {
        if let Some(slot) = self.swallowed.iter_mut().find(|s| **s == 0) {
            *slot = button;
        }
    }

    fn take_swallowed(&mut self, button: u32) -> bool {
        match self.swallowed.iter_mut().find(|s| **s == button) {
            Some(slot) => {
                *slot = 0;
                true
            }
            None => false,
        }
    }

    #[cfg(test)]
    pub fn swallowed_is_clear(&self) -> bool {
        self.swallowed == [0; 4]
    }

    #[cfg(test)]
    pub fn hovered(&self) -> Option<(Window, Hit)> {
        self.hover.clone()
    }
}

/// The titlebar height `window` has now, or 0 for none. A bar is drawn under
/// `de` for a window that is not fullscreen, not an override-redirect or
/// unmanaged X11 window, and that uses server-side decoration:
///
/// - xdg: the compositor's decoration mode is `ServerSide`. A client that asked
///   for client-side (`protocols/standard/decoration.rs`), or never bound the
///   decoration protocol at all, draws its own.
/// - X11: Motif hints did not ask for no decorations (smithay's `is_decorated`
///   is true when they did).
///
/// Every workspace the shell has is a human one; agent workspaces (COMP-05
/// §3.2) are undecorated and will be excluded here when they exist.
pub fn bar_height(state: &AbyssState, window: &Window) -> i32 {
    if !state.config.mode.profile().titlebars || state.fullscreen.contains_key(window) {
        return 0;
    }
    if super::is_unmanaged_x11(state, window) {
        return 0;
    }
    let server_side = match window.underlying_surface() {
        WindowSurface::Wayland(t) => t.with_pending_state(|s| {
            s.decoration_mode
                == Some(smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode::ServerSide)
        }),
        WindowSurface::X11(x) => !x.is_decorated(),
    };
    if server_side {
        state.config.decoration.titlebar_height
    } else {
        0
    }
}

/// Write `window`'s bar into its user data for the renderer and the hit test:
/// `bar` is [`bar_height`] (0 clears it). Called by every layout pass.
pub(super) fn publish(state: &AbyssState, window: &Window, bar: i32, maximized: bool) {
    let info = (bar > 0).then(|| BarInfo {
        height: bar,
        hover: hover_button(&state.bar_ui, window),
        pressed: pressed_button(&state.bar_ui, window),
        maximized,
        shaded: state.winops.is_shaded(window),
    });
    set_bar(window, info);
}

fn hover_button(ui: &BarUi, window: &Window) -> Option<Button> {
    match &ui.hover {
        Some((w, Hit::Button(b))) if w == window => Some(*b),
        _ => None,
    }
}

/// The button of `window` that is held down and still under the pointer: moving
/// off it drops the pressed look (and the action, on release).
fn pressed_button(ui: &BarUi, window: &Window) -> Option<Button> {
    let (w, b) = ui.press.as_ref().filter(|(w, _)| w == window)?;
    (ui.hover == Some((w.clone(), Hit::Button(*b)))).then_some(*b)
}

/// The bar under `pos` (global logical), if it is the topmost thing there:
/// a window stacked above the bar hides it. No allocation.
pub fn bar_at(state: &AbyssState, pos: Point<f64, Logical>) -> Option<(Window, Hit)> {
    if !state.config.mode.profile().titlebars || super::top_layer_at(state, pos) {
        return None;
    }
    let top = super::winops::element_under(state, pos);
    for w in state.space.elements().rev() {
        if let Some(info) = bar_of(w) {
            if let Some(content) = state.space.element_geometry(w) {
                let bar = bar_rect(content, info.height);
                if let Some(h) = hit(bar.size.w, bar.size.h, pos - bar.loc.to_f64()) {
                    // The window's own popup over its bar is in front of it.
                    if let Some((t, loc)) = &top {
                        if *t == w
                            && w.surface_under(pos - loc.to_f64(), WindowSurfaceType::ALL)
                                .is_some()
                        {
                            return None;
                        }
                    }
                    return Some((w.clone(), h));
                }
            }
        }
        if top.as_ref().is_some_and(|(t, _)| *t == w) {
            break;
        }
    }
    None
}

/// Redraw `window`'s bar after its hover or press changed.
fn republish(state: &AbyssState, window: &Window) {
    if let Some(mut info) = bar_of(window) {
        info.hover = hover_button(&state.bar_ui, window);
        info.pressed = pressed_button(&state.bar_ui, window);
        set_bar(window, Some(info));
    }
}

/// The pointer moved to `pos`: update which bar button is hovered, redrawing
/// only on a change. Returns whether the pointer is over a bar.
pub fn motion(state: &mut AbyssState, pos: Point<f64, Logical>) -> bool {
    start_drag_if_moved(state, pos);
    let now = bar_at(state, pos);
    if now != state.bar_ui.hover {
        let old = std::mem::replace(&mut state.bar_ui.hover, now.clone());
        if let Some((w, _)) = old {
            republish(state, &w);
        }
        if let Some((w, _)) = &now {
            republish(state, w);
        }
        crate::backend::damage_all(state);
    }
    now.is_some()
}

/// A left press on a bar that has moved [`DRAG_THRESHOLD`] becomes a window move
/// through the existing move grab (`input::grabs`), which restores a maximized
/// window under the pointer first, keeping the pointer at the same place along
/// the bar. The press was swallowed; its release must reach the grab to end it,
/// so it is taken off the swallow list.
fn start_drag_if_moved(state: &mut AbyssState, pos: Point<f64, Logical>) {
    let Some((_, origin)) = &state.bar_ui.drag else {
        return;
    };
    let (dx, dy) = (pos.x - origin.x, pos.y - origin.y);
    if dx * dx + dy * dy < DRAG_THRESHOLD * DRAG_THRESHOLD {
        return;
    }
    let Some((window, origin)) = state.bar_ui.drag.take() else {
        return;
    };
    if state.lock.locked
        || state.pointer_grab_active
        || crate::input::grabs::drag_active(state)
        || !state.bar_ui.take_swallowed(BTN_LEFT)
    {
        return;
    }
    if crate::input::grabs::start_move_from(state, window, BTN_LEFT, Some(origin)) {
        tracing::info!("titlebar drag started");
    } else {
        // Nothing to drag: the release is the bar's again.
        state.bar_ui.swallow(BTN_LEFT);
    }
}

/// A pointer button. True when a bar took it, in which case the seat must not
/// see it: no client gets a press that began on a bar, or its release.
pub fn button(state: &mut AbyssState, button: u32, button_state: ButtonState) -> bool {
    if !state.config.mode.profile().titlebars {
        return false;
    }
    let pos = state.pointer_location;
    if button_state == ButtonState::Pressed {
        let Some((window, hit)) = bar_at(state, pos) else {
            return false;
        };
        // A press outside a popup grab dismisses it, as any press would.
        if !state.popup_grabs.is_empty() {
            super::popup_grab_button_press(state, None);
        }
        // The window is the human's choice: focus and raise it. A bar is never
        // a client's surface, so nothing reaches one.
        // The menu is the human's alone and replaces any other grab: a right
        // press opens it where the pointer is. Focus and raise happen there.
        if button == BTN_RIGHT {
            state.bar_ui.swallow(button);
            super::winmenu::open_window_menu(state, &window, pos.to_i32_round());
            return true;
        }
        super::focus::focus_window_raising(state, &window, true);
        state.bar_ui.swallow(button);
        match (button, hit) {
            (BTN_LEFT, Hit::Button(b)) => {
                state.bar_ui.press = Some((window.clone(), b));
                state.bar_ui.clicks.clear();
                republish(state, &window);
                crate::backend::damage_all(state);
            }
            (BTN_LEFT, Hit::Bar) => {
                let now = state.start_time.elapsed().as_millis() as u64;
                if state.bar_ui.clicks.press(window.clone(), now, pos) {
                    // A double-click toggles maximize; the second press is
                    // not the start of a drag.
                    state.bar_ui.drag = None;
                    super::maximize::toggle_window(state, &window);
                } else {
                    state.bar_ui.drag = Some((window, pos));
                }
            }
            // A middle press on the bar is the bar's and does nothing.
            _ => {}
        }
        return true;
    }
    false
}

/// A pointer button release, called before every input gate so a press a bar
/// swallowed always has its release swallowed, whatever happened in between
/// (lock, prompt, selector, mode flip). True when it was. The action runs only
/// if nothing holds the pointer now.
pub fn release(state: &mut AbyssState, button: u32) -> bool {
    if !state.bar_ui.take_swallowed(button) {
        return false;
    }
    let pos = state.pointer_location;
    let blocked = state.lock.locked
        || crate::trusted_ui::holds_seat(state)
        || state.region_select.active()
        || !state.config.mode.profile().titlebars;
    if button == BTN_LEFT {
        state.bar_ui.drag = None;
        if let Some((window, b)) = state.bar_ui.press.take() {
            republish(state, &window);
            crate::backend::damage_all(state);
            // Acts only if the pointer is still on the button it began on.
            if !blocked && bar_at(state, pos) == Some((window.clone(), Hit::Button(b))) {
                act(state, &window, b);
            }
        }
    }
    true
}

/// What a button does, to `window` (not to whatever is focused).
fn act(state: &mut AbyssState, window: &Window, button: Button) {
    match button {
        Button::Minimize => super::minimize_window(state, window),
        Button::Maximize => super::maximize::toggle_window(state, window),
        Button::Close => super::close_window(state, window),
    }
}

/// The bar of `window` in global logical coordinates, if it has one now.
pub fn bar_geometry(state: &AbyssState, window: &Window) -> Option<Rectangle<i32, Logical>> {
    let info = bar_of(window)?;
    Some(bar_rect(state.space.element_geometry(window)?, info.height))
}

#[cfg(test)]
mod tests {
    use smithay::{
        backend::input::ButtonState,
        reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as Deco,
        utils::{Logical, Point, Rectangle},
    };

    use super::*;
    use crate::{
        config::Mode,
        shell::{
            self, arrange,
            focus::state_tests::{
                client::{Client, Toplevel},
                harness, Harness,
            },
            maximize, owned_windows, window_surface,
            workspace::Placement,
        },
    };

    fn window_of(h: &Harness, t: &Toplevel) -> Window {
        use wayland_client::Proxy;
        let id = t.surface.id().protocol_id();
        owned_windows(&h.state)
            .into_iter()
            .find(|w| {
                window_surface(w)
                    .is_some_and(|s| smithay::reexports::wayland_server::Resource::id(&s).protocol_id() == id)
            })
            .expect("placed")
    }

    fn flip(h: &mut Harness, mode: Mode) {
        let mut next = h.state.config.clone();
        next.mode = mode;
        crate::config::apply_loaded(&mut h.state, next);
    }

    /// Server-side decoration, as `protocols/standard/decoration.rs` answers a
    /// client that bound the protocol, then a layout pass.
    fn ssd(h: &mut Harness, w: &Window) {
        w.toplevel()
            .expect("xdg")
            .with_pending_state(|s| s.decoration_mode = Some(Deco::ServerSide));
        arrange(&mut h.state);
    }

    fn map(h: &mut Harness, c: &mut Client) -> (Toplevel, Window) {
        let t = c.create_toplevel(h);
        c.commit(h, &t.surface);
        c.attach(h, &t.surface);
        let w = window_of(h, &t);
        (t, w)
    }

    /// The client does what a well-behaved one does: draws the size it was last
    /// configured with. The test client otherwise keeps its 100x100 buffer.
    fn settle(h: &mut Harness, c: &mut Client, t: &Toplevel) {
        let (w, ht) = *c.configured_sizes(&t.toplevel).last().expect("configured");
        c.attach_sized(h, &t.surface, w, ht);
    }

    /// A decorated window mapped under `de`, drawn at the size it was given.
    fn de_window(h: &mut Harness, c: &mut Client) -> (Toplevel, Window) {
        flip(h, Mode::De);
        let (t, w) = map(h, c);
        ssd(h, &w);
        c.pump(h);
        settle(h, c, &t);
        (t, w)
    }

    fn floating_of(h: &Harness, w: &Window) -> (Rectangle<i32, Logical>, Placement) {
        h.state
            .outputs
            .iter()
            .flat_map(|e| e.workspaces.iter())
            .flat_map(|ws| ws.floating.iter())
            .find(|f| &f.window == w)
            .map(|f| (f.rect, f.placement))
            .expect("floating")
    }

    fn content(h: &Harness, w: &Window) -> Rectangle<i32, Logical> {
        h.state.space.element_geometry(w).expect("mapped")
    }

    fn bar(h: &Harness) -> i32 {
        h.state.config.decoration.titlebar_height
    }

    fn border(h: &Harness) -> i32 {
        h.state.config.general.border_size
    }

    fn usable(h: &Harness, id: u64) -> Rectangle<i32, Logical> {
        let output = h.state.outputs.get(id).expect("output").output.clone();
        smithay::desktop::layer_map_for_output(&output).arrange();
        shell::usable_area(&h.state, &output)
    }

    fn move_to(h: &mut Harness, p: Point<f64, Logical>) {
        h.state.pointer_moved(p, 0);
    }

    fn press(h: &mut Harness) {
        h.state.pointer_button(0x110, ButtonState::Pressed, 0);
    }

    fn release(h: &mut Harness) {
        h.state.pointer_button(0x110, ButtonState::Released, 0);
    }

    /// The centre of `b` on `w`'s bar.
    fn on_button(h: &Harness, w: &Window, b: Button) -> Point<f64, Logical> {
        let bar = bar_geometry(&h.state, w).expect("has a bar");
        let r = ec_abyss_render::titlebar::button_rect(bar.size.w, bar.size.h, b);
        Point::from((
            f64::from(bar.loc.x + r.loc.x) + f64::from(r.size.w) / 2.0,
            f64::from(bar.loc.y) + f64::from(bar.size.h) / 2.0,
        ))
    }

    fn on_bar(h: &Harness, w: &Window) -> Point<f64, Logical> {
        let bar = bar_geometry(&h.state, w).expect("has a bar");
        Point::from((f64::from(bar.loc.x) + 6.0, f64::from(bar.loc.y) + 6.0))
    }

    fn pointer_focus(
        h: &Harness,
    ) -> Option<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface> {
        h.state.seat.get_pointer().unwrap().current_focus()
    }

    // ---- which windows ---------------------------------------------------

    #[test]
    fn only_a_server_side_window_under_de_has_a_bar() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (_t, w) = map(&mut h, &mut c);
        // No decoration protocol: the client draws its own.
        flip(&mut h, Mode::De);
        assert_eq!(bar_height(&h.state, &w), 0, "never negotiated: client-side");
        w.toplevel()
            .unwrap()
            .with_pending_state(|s| s.decoration_mode = Some(Deco::ClientSide));
        assert_eq!(bar_height(&h.state, &w), 0, "explicit client-side");
        ssd(&mut h, &w);
        assert_eq!(bar_height(&h.state, &w), bar(&h));
        assert!(bar_of(&w).is_some(), "published for the renderer");
        // Fullscreen has none.
        h.state.fullscreen.insert(w.clone(), None);
        assert_eq!(bar_height(&h.state, &w), 0);
        h.state.fullscreen.clear();
        // Neither wm nor hybrid.
        for mode in [Mode::Hybrid, Mode::Wm] {
            flip(&mut h, mode);
            assert_eq!(bar_height(&h.state, &w), 0, "{mode:?} draws no bar");
            assert!(bar_of(&w).is_none(), "{mode:?}: nothing published");
        }
    }

    // ---- geometry --------------------------------------------------------

    #[test]
    fn the_client_is_configured_with_the_content_rect_under_the_bar() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (t, w) = de_window(&mut h, &mut c);
        let (rect, _) = floating_of(&h, &w);
        let (b, bar) = (border(&h), bar(&h));
        let want = content_of_outer(rect, b, bar);
        assert_eq!(content(&h, &w), want, "mapped at the content rect");
        assert_eq!(
            c.configured_sizes(&t.toplevel).last(),
            Some(&(want.size.w, want.size.h))
        );
        // Frame = bar + content + border: nothing is lost.
        assert_eq!(want.size.h + bar + 2 * b, rect.size.h);
        assert_eq!(outer_of_content(want, b, bar), rect);
        let geo = bar_geometry(&h.state, &w).expect("bar");
        assert_eq!(geo.loc.y + geo.size.h, want.loc.y, "the bar sits on the content");
        assert_eq!(geo.size.w, want.size.w);
    }

    #[test]
    fn get_windows_reports_the_content_rect() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (_t, w) = de_window(&mut h, &mut c);
        let want = content(&h, &w);
        let reply = crate::ipc::methods::dispatch(&mut h.state, 0, "get_windows", &serde_json::json!({}))
            .unwrap_or_else(|_| panic!("get_windows failed"));
        let rect = &reply.as_array().expect("array")[0]["rect"];
        assert_eq!(
            (
                rect["x"].as_i64(),
                rect["y"].as_i64(),
                rect["w"].as_i64(),
                rect["h"].as_i64()
            ),
            (
                Some(i64::from(want.loc.x)),
                Some(i64::from(want.loc.y)),
                Some(i64::from(want.size.w)),
                Some(i64::from(want.size.h))
            ),
            "ec-ctl windows keeps the content rect, bar not included"
        );
    }

    #[test]
    fn maximize_fills_the_usable_area_with_the_frame_and_restore_gives_it_back() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (t, w) = de_window(&mut h, &mut c);
        let id = h.a;
        let (before, _) = floating_of(&h, &w);
        let before_content = content(&h, &w);
        let bar = bar(&h);

        assert!(maximize::set(&mut h.state, &w, Some((true, true))));
        c.pump(&mut h);
        let u = usable(&h, id);
        let (rect, p) = floating_of(&h, &w);
        assert!(p.is_fully_maximized());
        assert_eq!(rect, u, "the frame fills the usable area");
        let body = Rectangle::new(
            Point::from((u.loc.x, u.loc.y + bar)),
            (u.size.w, u.size.h - bar).into(),
        );
        assert_eq!(
            c.configured_sizes(&t.toplevel).last(),
            Some(&(body.size.w, body.size.h))
        );
        settle(&mut h, &mut c, &t);
        assert_eq!(content(&h, &w), body, "the content is shorter by the bar");
        assert_eq!(
            bar_geometry(&h.state, &w).unwrap().loc,
            u.loc,
            "bar flush with the top"
        );

        assert!(maximize::set(&mut h.state, &w, None));
        c.pump(&mut h);
        settle(&mut h, &mut c, &t);
        assert_eq!(floating_of(&h, &w).0, before, "restore rect is the frame it had");
        assert_eq!(content(&h, &w), before_content);
    }

    /// Where the window is and the size its client was last told.
    fn told(h: &Harness, c: &Client, t: &Toplevel, w: &Window) -> Rectangle<i32, Logical> {
        let (cw, ch) = *c.configured_sizes(&t.toplevel).last().expect("configured");
        Rectangle::new(content(h, w).loc, (cw, ch).into())
    }

    #[test]
    fn a_mode_flip_round_trip_neither_creeps_nor_shrinks() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        // A tile in hybrid, decorated server-side.
        let (t, w) = map(&mut h, &mut c);
        ssd(&mut h, &w);
        c.pump(&mut h);
        let tile = told(&h, &c, &t, &w);
        assert_eq!(bar_height(&h.state, &w), 0);
        let mut seen = None;
        for _ in 0..4 {
            flip(&mut h, Mode::De);
            c.pump(&mut h);
            let (rect, _) = floating_of(&h, &w);
            // The frame is the tile's outer rect every time.
            if let Some(prev) = seen {
                assert_eq!(rect, prev, "the frame is stable across flips");
            }
            seen = Some(rect);
            let under_bar = content_of_outer(rect, border(&h), bar(&h));
            assert_eq!(
                told(&h, &c, &t, &w),
                under_bar,
                "de: the content sits under the bar"
            );
            assert_eq!(
                under_bar.size.h + bar(&h),
                tile.size.h,
                "and is shorter by exactly the bar"
            );
            flip(&mut h, Mode::Hybrid);
            c.pump(&mut h);
            assert_eq!(
                told(&h, &c, &t, &w),
                tile,
                "back in hybrid it is exactly the tile"
            );
        }
    }

    #[test]
    fn a_floating_window_is_not_shifted_by_the_flip() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (t, w) = map(&mut h, &mut c);
        ssd(&mut h, &w);
        crate::shell::focus::focus_window(&mut h.state, &w);
        shell::toggle_floating(&mut h.state);
        c.pump(&mut h);
        let floated = told(&h, &c, &t, &w);
        for _ in 0..3 {
            flip(&mut h, Mode::De);
            c.pump(&mut h);
            let (rect, _) = floating_of(&h, &w);
            flip(&mut h, Mode::Hybrid);
            c.pump(&mut h);
            assert_eq!(
                told(&h, &c, &t, &w),
                floated,
                "same content rect in hybrid after every round trip"
            );
            flip(&mut h, Mode::De);
            assert_eq!(floating_of(&h, &w).0, rect, "and the same frame in de");
            flip(&mut h, Mode::Hybrid);
        }
    }

    #[test]
    fn placing_a_content_rect_puts_the_frame_around_it_and_keeps_the_bar_reachable() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (t, w) = de_window(&mut h, &mut c);
        let id = h.a;
        let u = usable(&h, id);
        // The X11 configure path and an interactive move both end in `place_at`.
        let want = Rectangle::new(Point::from((u.loc.x + 200, u.loc.y + 150)), (400, 300).into());
        shell::place_at(&mut h.state, &w, want);
        c.pump(&mut h);
        assert_eq!(
            told(&h, &c, &t, &w),
            want,
            "the content lands exactly where asked"
        );
        let geo = bar_geometry(&h.state, &w).unwrap();
        assert_eq!(geo.loc.y, want.loc.y - bar(&h));

        // Asked to sit at the very top: the bar, not the content, stops at the edge.
        let top = Rectangle::new(Point::from((u.loc.x + 200, u.loc.y)), (400, 300).into());
        shell::place_at(&mut h.state, &w, top);
        c.pump(&mut h);
        let geo = bar_geometry(&h.state, &w).unwrap();
        assert!(geo.loc.y >= u.loc.y, "the titlebar is inside the usable area");
        assert_eq!(content(&h, &w).loc.y, u.loc.y + bar(&h) + border(&h));
    }

    #[test]
    fn a_new_window_opens_with_its_frame_inside_the_area() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        flip(&mut h, Mode::De);
        let t = c.create_toplevel(&mut h);
        c.commit(&mut h, &t.surface);
        let w = window_of(&h, &t);
        // The decoration answer arrives with the first configure in practice.
        ssd(&mut h, &w);
        c.attach(&mut h, &t.surface);
        let id = h.a;
        let u = usable(&h, id);
        let geo = bar_geometry(&h.state, &w).expect("bar");
        assert!(geo.loc.y >= u.loc.y);
        assert!(floating_of(&h, &w).0.loc.y >= u.loc.y);
    }

    #[test]
    fn a_popup_is_placed_against_the_content_not_the_bar() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (t, w) = de_window(&mut h, &mut c);
        let id = h.a;
        let u = usable(&h, id);
        // Content flush under the bar at the top of the area.
        let want = Rectangle::new(
            Point::from((u.loc.x + 100, u.loc.y + bar(&h) + 3)),
            (300, 200).into(),
        );
        shell::place_at(&mut h.state, &w, want);
        // A popup 10 px above the content's origin: inside the output when the
        // origin is the content's, so it is not slid. Against the frame's origin
        // it would be above the output and slid down.
        let (surface, _popup) = c.create_popup(&mut h, &t, (0, -10));
        let root = window_surface(&w).unwrap();
        let geo = smithay::desktop::PopupManager::popups_for_surface(&root)
            .find_map(|(p, _)| match p {
                smithay::desktop::PopupKind::Xdg(x) => Some(x.with_pending_state(|s| s.geometry)),
                _ => None,
            })
            .expect("popup");
        let _ = surface;
        assert_eq!(geo.loc.y, -10, "positioned from the content origin");
        assert_eq!(content(&h, &w).loc, want.loc);
    }

    // ---- review findings -------------------------------------------------

    #[test]
    fn a_top_layer_surface_over_a_button_gets_the_click_not_the_window() {
        use smithay::reexports::wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::KeyboardInteractivity as Ki;
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (t, w) = de_window(&mut h, &mut c);
        let (_ls, _layer) = c.map_layer(&mut h, Ki::None);
        let id = h.a;
        let output = h.state.outputs.get(id).unwrap().output.clone();
        let lg = {
            let map = smithay::desktop::layer_map_for_output(&output);
            let l = map.layers().next().expect("layer").clone();
            shell::layer_geometry(&map, &l).expect("geometry")
        };
        let centre: Point<f64, Logical> = Point::from((
            f64::from(lg.loc.x) + f64::from(lg.size.w) / 2.0,
            f64::from(lg.loc.y) + f64::from(lg.size.h) / 2.0,
        ));
        // Move the window so its close button sits under the layer's centre.
        let at = on_button(&h, &w, Button::Close);
        let c0 = content(&h, &w);
        let want = Rectangle::new(
            Point::from((
                c0.loc.x + (centre.x - at.x) as i32,
                c0.loc.y + (centre.y - at.y) as i32,
            )),
            c0.size,
        );
        shell::place_at(&mut h.state, &w, want);
        c.pump(&mut h);
        let p = on_button(&h, &w, Button::Close);
        assert!(shell::top_layer_at(&h.state, p), "the layer is over the button");
        assert!(bar_at(&h.state, p).is_none(), "so the bar is not hit");
        move_to(&mut h, p);
        assert_eq!(bar_of(&w).unwrap().hover, None, "no hover under a layer");
        press(&mut h);
        release(&mut h);
        c.pump(&mut h);
        assert!(!c.closed(&t.toplevel), "the window under the layer is not closed");
    }

    #[test]
    fn a_swallowed_release_is_consumed_even_when_a_gate_took_it_first() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (t, w) = de_window(&mut h, &mut c);
        let p = on_button(&h, &w, Button::Close);
        move_to(&mut h, p);
        press(&mut h);
        // The press began on a bar; the mode flips before the release.
        flip(&mut h, Mode::Hybrid);
        release(&mut h);
        c.pump(&mut h);
        assert!(!c.closed(&t.toplevel), "no action off a de bar");
        assert_eq!(
            h.state.bar_ui.swallowed, [0; 4],
            "nothing left to eat a later release"
        );
        // Back under de: a press on the client is delivered and so is its release.
        flip(&mut h, Mode::De);
        assert!(!super::release(&mut h.state, BTN_LEFT));
    }

    #[test]
    fn a_popup_over_its_own_bar_is_in_front_of_it() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (t, w) = de_window(&mut h, &mut c);
        let id = h.a;
        let u = usable(&h, id);
        let want = Rectangle::new(
            Point::from((u.loc.x + 100, u.loc.y + bar(&h) + 40)),
            (300, 200).into(),
        );
        shell::place_at(&mut h.state, &w, want);
        c.pump(&mut h);
        let (surface, _popup) = c.create_popup(&mut h, &t, (0, -10));
        c.attach_sized(&mut h, &surface, 20, 20);
        let p = Point::from((f64::from(want.loc.x) + 5.0, f64::from(want.loc.y) - 5.0));
        assert!(
            bar_at(&h.state, p).is_none(),
            "the popup, not the bar, is at the point"
        );
        assert!(shell::surface_under(&h.state, p).is_some());
        // Above the popup the bar is still there.
        let q = Point::from((f64::from(want.loc.x) + 5.0, f64::from(want.loc.y) - 20.0));
        assert!(bar_at(&h.state, q).is_some());
    }

    #[test]
    fn an_x11_request_gets_the_bar_added_and_the_motif_hint_is_read_right() {
        // No Xwayland in the unit harness: the two decisions an X11 window
        // makes are pure. Named content (a request, a rule) grows by the bar;
        // a default frame does not; the bar is held to the area.
        let s = |w, h| smithay::utils::Size::<i32, Logical>::from((w, h));
        assert_eq!(shell::framed_size(s(400, 300), true, 30, 1000), s(400, 330));
        assert_eq!(shell::framed_size(s(400, 300), false, 30, 1000), s(400, 300));
        assert_eq!(shell::framed_size(s(400, 300), true, 0, 1000), s(400, 300));
        assert_eq!(shell::framed_size(s(400, 990), true, 30, 1000), s(400, 1000));
        // smithay's `is_decorated` is true for a client that asked for no
        // decorations (client-side): such a window gets no bar. Exercised
        // end to end only by the manual Xwayland run.
    }

    // ---- hit testing -----------------------------------------------------

    #[test]
    fn the_bar_is_hit_above_the_content_and_no_surface_is_under_it() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (_t, w) = de_window(&mut h, &mut c);
        let p = on_bar(&h, &w);
        assert!(matches!(bar_at(&h.state, p), Some((ref x, Hit::Bar)) if *x == w));
        assert!(
            shell::surface_under(&h.state, p).is_none(),
            "no client under a bar"
        );
        let close = on_button(&h, &w, Button::Close);
        assert!(matches!(
            bar_at(&h.state, close),
            Some((_, Hit::Button(Button::Close)))
        ));
        let c0 = content(&h, &w);
        let inside = Point::from((f64::from(c0.loc.x) + 5.0, f64::from(c0.loc.y) + 5.0));
        assert!(bar_at(&h.state, inside).is_none());
        assert!(
            shell::surface_under(&h.state, inside).is_some(),
            "the content still hits"
        );
        // Just above the bar is not the bar.
        let above = Point::from((p.x, f64::from(bar_geometry(&h.state, &w).unwrap().loc.y) - 1.0));
        assert!(bar_at(&h.state, above).is_none());
    }

    #[test]
    fn a_window_stacked_over_a_bar_hides_it() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (_a, a) = de_window(&mut h, &mut c);
        let (_b, b) = map(&mut h, &mut c);
        ssd(&mut h, &b);
        // Put b's content over a's bar, b on top.
        let ga = bar_geometry(&h.state, &a).unwrap();
        let over = Rectangle::new(
            Point::from((ga.loc.x - 20, ga.loc.y - 10)),
            (ga.size.w + 40, 200).into(),
        );
        shell::place_at(&mut h.state, &b, over);
        crate::shell::focus::focus_window(&mut h.state, &b);
        let p = Point::from((f64::from(ga.loc.x) + 10.0, f64::from(ga.loc.y) + 5.0));
        let hit = bar_at(&h.state, p);
        assert!(
            !matches!(hit, Some((ref x, _)) if *x == a),
            "a's bar is under b and cannot be clicked"
        );
    }

    #[test]
    fn wm_and_hybrid_have_no_bar_to_hit() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (_t, w) = de_window(&mut h, &mut c);
        let p = on_bar(&h, &w);
        for mode in [Mode::Hybrid, Mode::Wm] {
            flip(&mut h, mode);
            assert!(bar_at(&h.state, p).is_none());
            assert!(bar_geometry(&h.state, &w).is_none());
        }
    }

    // ---- buttons ---------------------------------------------------------

    #[test]
    fn a_press_on_the_bar_focuses_and_raises_and_no_client_hears_it() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (_a, a) = de_window(&mut h, &mut c);
        let (_b, b) = map(&mut h, &mut c);
        ssd(&mut h, &b);
        let u = usable(&h, h.a);
        shell::place_at(
            &mut h.state,
            &b,
            Rectangle::new(
                Point::from((u.loc.x + u.size.w - 340, u.loc.y + u.size.h - 260)),
                (300, 200).into(),
            ),
        );
        crate::shell::focus::focus_window(&mut h.state, &b);
        assert_eq!(h.state.focus.as_ref(), Some(&b));
        let p = on_bar(&h, &a);
        move_to(&mut h, p);
        assert!(
            pointer_focus(&h).is_none(),
            "the pointer is over a bar: no client focus"
        );
        press(&mut h);
        assert_eq!(
            h.state.focus.as_ref(),
            Some(&a),
            "a click on the bar focuses its window"
        );
        assert!(pointer_focus(&h).is_none());
        let before = h.state.bar_ui.hovered();
        release(&mut h);
        assert_eq!(before.map(|(w, _)| w), Some(a.clone()));
        assert!(
            !shell::is_minimized(&h.state, &a),
            "a plain bar click does nothing else"
        );
    }

    #[test]
    fn minimize_acts_on_release_over_the_same_button() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (_t, w) = de_window(&mut h, &mut c);
        let p = on_button(&h, &w, Button::Minimize);
        move_to(&mut h, p);
        assert_eq!(
            bar_of(&w).unwrap().hover,
            Some(Button::Minimize),
            "hover is drawn"
        );
        press(&mut h);
        assert_eq!(bar_of(&w).unwrap().pressed, Some(Button::Minimize));
        assert!(!shell::is_minimized(&h.state, &w), "not on press");
        release(&mut h);
        assert!(shell::is_minimized(&h.state, &w));
    }

    #[test]
    fn maximize_button_toggles_and_the_icon_state_follows() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (_t, w) = de_window(&mut h, &mut c);
        let p = on_button(&h, &w, Button::Maximize);
        move_to(&mut h, p);
        press(&mut h);
        release(&mut h);
        assert!(maximize::is_maximized(&h.state, &w));
        assert!(bar_of(&w).unwrap().maximized, "the glyph becomes restore");
        // The bar moved to the top: click its maximize button again.
        let p = on_button(&h, &w, Button::Maximize);
        move_to(&mut h, p);
        press(&mut h);
        release(&mut h);
        assert!(!maximize::is_maximized(&h.state, &w));
    }

    #[test]
    fn pressing_a_button_and_dragging_off_cancels() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (t, w) = de_window(&mut h, &mut c);
        let p = on_button(&h, &w, Button::Close);
        move_to(&mut h, p);
        press(&mut h);
        // Off the button, onto the bar, and release there.
        let off = on_bar(&h, &w);
        move_to(&mut h, off);
        assert_eq!(
            bar_of(&w).unwrap().pressed,
            None,
            "the press look goes with the pointer"
        );
        release(&mut h);
        c.pump(&mut h);
        assert!(!c.closed(&t.toplevel), "released elsewhere: nothing happens");
        // Release on another button cancels too.
        move_to(&mut h, p);
        press(&mut h);
        let other = on_button(&h, &w, Button::Minimize);
        move_to(&mut h, other);
        release(&mut h);
        assert!(!shell::is_minimized(&h.state, &w));
        // Off the window entirely.
        move_to(&mut h, p);
        press(&mut h);
        move_to(&mut h, Point::from((1.0, 1.0)));
        release(&mut h);
        c.pump(&mut h);
        assert!(!c.closed(&t.toplevel));
    }

    #[test]
    fn close_acts_on_the_bars_window_while_another_is_focused() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (a, aw) = de_window(&mut h, &mut c);
        let (b, bw) = map(&mut h, &mut c);
        ssd(&mut h, &bw);
        // Move b away from a so a's bar is reachable, then focus b.
        let u = usable(&h, h.a);
        shell::place_at(
            &mut h.state,
            &bw,
            Rectangle::new(
                Point::from((u.loc.x + u.size.w - 340, u.loc.y + u.size.h - 260)),
                (300, 200).into(),
            ),
        );
        crate::shell::focus::focus_window(&mut h.state, &bw);
        let p = on_button(&h, &aw, Button::Close);
        move_to(&mut h, p);
        press(&mut h);
        release(&mut h);
        c.pump(&mut h);
        assert!(c.closed(&a.toplevel), "the bar's own window was asked to close");
        assert!(!c.closed(&b.toplevel), "not the focused one");
    }

    #[test]
    fn buttons_do_nothing_under_wm_and_hybrid() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (t, w) = de_window(&mut h, &mut c);
        let p = on_button(&h, &w, Button::Close);
        flip(&mut h, Mode::Hybrid);
        move_to(&mut h, p);
        press(&mut h);
        release(&mut h);
        c.pump(&mut h);
        assert!(!c.closed(&t.toplevel));
        assert!(!shell::is_minimized(&h.state, &w));
    }

    #[test]
    fn a_bar_press_is_not_a_way_past_the_session_lock() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (t, w) = de_window(&mut h, &mut c);
        let p = on_button(&h, &w, Button::Close);
        move_to(&mut h, p);
        h.state.lock.locked = true;
        press(&mut h);
        release(&mut h);
        c.pump(&mut h);
        assert!(!c.closed(&t.toplevel), "the lock owns the pointer");
        assert!(h.state.bar_ui.press.is_none());
        h.state.lock.locked = false;
    }
}
