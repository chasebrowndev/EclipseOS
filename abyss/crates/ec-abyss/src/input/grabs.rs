// SPDX-License-Identifier: AGPL-3.0-only
//! Interactive move and resize grabs (COMP-04 §3, COMP-05 §3).
//!
//! A client asks for these through `xdg_toplevel.move` / `xdg_toplevel.resize`.
//! While a grab is active the pointer focus is cleared, so the client sees a
//! `wl_pointer.leave` and stops reacting to the drag itself; every motion is
//! turned into a new window geometry instead. The grab ends when the button
//! that started it is released.
//!
//! The human can start the same grabs without the client's help: a `mousebind`
//! (modifier + button, `start_mouse_bind`) drags whatever window is under the
//! pointer, and the press is swallowed so the client never sees it.
//!
//! The same requests arrive with a `wl_touch.down` serial when a CSD client's
//! titlebar is dragged by finger. Those get a touch grab instead: motion of the
//! starting slot drives the window, and lifting that finger (or a cancel) ends
//! it.
//!
//! A touchpad drag gesture (`gesture "drag"`, COMP-04 §2 amended C-12) moves a
//! window without any grab: two fingers arrive as finger scroll and three or
//! four as a swipe, neither of which a pointer grab sees, so the input path
//! claims the gesture at its begin and drives [`GestureDrag`] from its deltas.

use smithay::{
    backend::input::ButtonState,
    desktop::Window,
    input::pointer::{
        AxisFrame, ButtonEvent, Focus, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
        GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
        GestureSwipeUpdateEvent, GrabStartData, MotionEvent, PointerGrab, PointerInnerHandle,
        RelativeMotionEvent,
    },
    input::touch::{
        DownEvent, GrabStartData as TouchGrabStartData, MotionEvent as TouchMotionEvent, OrientationEvent,
        ShapeEvent, TouchGrab, TouchInnerHandle, UpEvent,
    },
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::{protocol::wl_surface::WlSurface, Resource},
    },
    utils::{Logical, Point, Rectangle, Serial, Size, SERIAL_COUNTER},
};

use crate::config::Config;
use crate::input::MouseAction;
use crate::state::AbyssState;

/// The grab is only honoured if the serial really belongs to a press the
/// requesting surface received, and that press is still held. Anything else is
/// a client trying to grab the pointer without the human having asked for it.
pub fn start_data(
    state: &AbyssState,
    surface: &WlSurface,
    serial: Serial,
) -> Option<GrabStartData<AbyssState>> {
    let pointer = state.seat.get_pointer()?;
    if !pointer.has_grab(serial) {
        return None;
    }
    let start_data = pointer.grab_start_data()?;
    let (focus, _) = start_data.focus.as_ref()?;
    if !focus.id().same_client_as(&surface.id()) {
        return None;
    }
    Some(start_data)
}

/// The touch twin of [`start_data`]: the serial must be the live `wl_touch.down`
/// that opened the current touch sequence, and that down must have landed on a
/// surface of the requesting client. Anything else is refused.
pub fn touch_start_data(
    state: &AbyssState,
    surface: &WlSurface,
    serial: Serial,
) -> Option<TouchGrabStartData<AbyssState>> {
    let touch = state.seat.get_touch()?;
    if !touch.has_grab(serial) {
        return None;
    }
    let start_data = touch.grab_start_data()?;
    let (focus, _) = start_data.focus.as_ref()?;
    if !focus.id().same_client_as(&surface.id()) {
        return None;
    }
    Some(start_data)
}

/// Place `window` at `initial_location` shifted by `delta` (move grabs).
/// Under Radiant a tiled window is dragged over its placeholder instead of
/// being floated (`shell::drag_tile`); the window's centre aims the drop.
fn place_moved(
    data: &mut AbyssState,
    window: &Window,
    initial_location: Point<i32, Logical>,
    delta: Point<f64, Logical>,
    pointer: Point<f64, Logical>,
) {
    // Nothing behind the lock moves. The grab can outlive `engage_lock`
    // (ending it there would send a focus-restoring motion to the surface
    // behind the lock), so without this every motion would float a tiled
    // window out of its tree via `place_at`.
    if data.lock.locked {
        return;
    }
    let loc = initial_location.to_f64() + delta;
    if crate::shell::drag_tile(data, window, loc.to_i32_round()) {
        return;
    }
    // The size the layout gave the window, not the one its client has drawn so
    // far: a window just restored from maximize has not redrawn yet, and
    // pinning it at the old size would undo the restore.
    // Under `de` only; `wm` and `hybrid` pin the drawn size as they always did.
    let stored = if crate::shell::de::stacking(data) {
        crate::shell::stored_content(data, window)
    } else {
        None
    };
    let size = stored
        .or_else(|| data.space.element_geometry(window))
        .map(|g| g.size)
        .unwrap_or_default();
    let mut rect = Rectangle::new(loc.to_i32_round(), size);
    // Quick Tile (`mode "de"`): the screen-edge preview follows the pointer,
    // and the edges snap unless Shift is held (`shell::snap`).
    if data.config.mode.profile().quick_tile {
        crate::shell::quicktile::drag_preview(data, window, pointer);
        rect = crate::shell::snap::snap_content(data, window, rect, None);
    }
    crate::shell::place_at(data, &window.clone(), rect);
}

/// `rect` of a resize, with the dragged edges snapped (`mode "de"`).
fn snapped_resize(
    data: &AbyssState,
    window: &Window,
    edges: xdg_toplevel::ResizeEdge,
    rect: Rectangle<i32, Logical>,
) -> Rectangle<i32, Logical> {
    if data.config.mode.profile().quick_tile {
        crate::shell::snap::snap_content(data, window, rect, Some(edges))
    } else {
        rect
    }
}

/// `initial` with `edges` dragged by `delta`; the opposite edges stay put and
/// the size never drops below 1x1.
pub fn resized_rect(
    initial: Rectangle<i32, Logical>,
    edges: xdg_toplevel::ResizeEdge,
    delta: Point<i32, Logical>,
) -> Rectangle<i32, Logical> {
    let (mut x, mut y) = (initial.loc.x, initial.loc.y);
    let (mut w, mut h) = (initial.size.w, initial.size.h);
    use xdg_toplevel::ResizeEdge as E;
    let (top, bottom, left, right) = match edges {
        E::Top => (true, false, false, false),
        E::Bottom => (false, true, false, false),
        E::Left => (false, false, true, false),
        E::TopLeft => (true, false, true, false),
        E::BottomLeft => (false, true, true, false),
        E::Right => (false, false, false, true),
        E::TopRight => (true, false, false, true),
        E::BottomRight => (false, true, false, true),
        _ => (false, false, false, false),
    };
    if left {
        x += delta.x;
        w -= delta.x;
    }
    if right {
        w += delta.x;
    }
    if top {
        y += delta.y;
        h -= delta.y;
    }
    if bottom {
        h += delta.y;
    }
    Rectangle::new(Point::from((x, y)), Size::from((w.max(1), h.max(1))))
}

/// Drag the window by the pointer delta since the press.
pub struct MoveSurfaceGrab {
    pub start_data: GrabStartData<AbyssState>,
    pub window: Window,
    /// Window-geometry origin at the moment the grab started.
    pub initial_location: Point<i32, Logical>,
}

impl PointerGrab<AbyssState> for MoveSurfaceGrab {
    fn motion(
        &mut self,
        data: &mut AbyssState,
        handle: &mut PointerInnerHandle<'_, AbyssState>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        // The focus stays cleared for the whole grab: the client is being
        // dragged, not pointed at.
        handle.motion(data, None, event);
        let delta = event.location - self.start_data.location;
        place_moved(data, &self.window, self.initial_location, delta, event.location);
    }

    fn relative_motion(
        &mut self,
        data: &mut AbyssState,
        handle: &mut PointerInnerHandle<'_, AbyssState>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, None, event);
    }

    fn button(
        &mut self,
        data: &mut AbyssState,
        handle: &mut PointerInnerHandle<'_, AbyssState>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);
        if !handle.current_pressed().contains(&self.start_data.button) {
            finish_move(data, &self.window);
            handle.unset_grab(self, data, event.serial, event.time, true);
        }
    }

    fn axis(
        &mut self,
        data: &mut AbyssState,
        handle: &mut PointerInnerHandle<'_, AbyssState>,
        details: AxisFrame,
    ) {
        handle.axis(data, details);
    }

    fn frame(&mut self, data: &mut AbyssState, handle: &mut PointerInnerHandle<'_, AbyssState>) {
        handle.frame(data);
    }

    fn gesture_swipe_begin(
        &mut self,
        data: &mut AbyssState,
        handle: &mut PointerInnerHandle<'_, AbyssState>,
        event: &GestureSwipeBeginEvent,
    ) {
        handle.gesture_swipe_begin(data, event);
    }

    fn gesture_swipe_update(
        &mut self,
        data: &mut AbyssState,
        handle: &mut PointerInnerHandle<'_, AbyssState>,
        event: &GestureSwipeUpdateEvent,
    ) {
        handle.gesture_swipe_update(data, event);
    }

    fn gesture_swipe_end(
        &mut self,
        data: &mut AbyssState,
        handle: &mut PointerInnerHandle<'_, AbyssState>,
        event: &GestureSwipeEndEvent,
    ) {
        handle.gesture_swipe_end(data, event);
    }

    fn gesture_pinch_begin(
        &mut self,
        data: &mut AbyssState,
        handle: &mut PointerInnerHandle<'_, AbyssState>,
        event: &GesturePinchBeginEvent,
    ) {
        handle.gesture_pinch_begin(data, event);
    }

    fn gesture_pinch_update(
        &mut self,
        data: &mut AbyssState,
        handle: &mut PointerInnerHandle<'_, AbyssState>,
        event: &GesturePinchUpdateEvent,
    ) {
        handle.gesture_pinch_update(data, event);
    }

    fn gesture_pinch_end(
        &mut self,
        data: &mut AbyssState,
        handle: &mut PointerInnerHandle<'_, AbyssState>,
        event: &GesturePinchEndEvent,
    ) {
        handle.gesture_pinch_end(data, event);
    }

    fn gesture_hold_begin(
        &mut self,
        data: &mut AbyssState,
        handle: &mut PointerInnerHandle<'_, AbyssState>,
        event: &GestureHoldBeginEvent,
    ) {
        handle.gesture_hold_begin(data, event);
    }

    fn gesture_hold_end(
        &mut self,
        data: &mut AbyssState,
        handle: &mut PointerInnerHandle<'_, AbyssState>,
        event: &GestureHoldEndEvent,
    ) {
        handle.gesture_hold_end(data, event);
    }

    fn start_data(&self) -> &GrabStartData<AbyssState> {
        &self.start_data
    }

    fn unset(&mut self, data: &mut AbyssState) {
        // A grab that ends without a release (replaced, torn down) puts the
        // window back on its placeholder. After a release this is a no-op.
        crate::shell::cancel_tile_drag(data);
        data.pointer_grab_active = false;
    }
}

/// The shared end of a pointer or touch move: hand the window to the Radiant
/// drop (a no-op when no drop is in flight).
pub fn finish_move(state: &mut AbyssState, window: &Window) {
    // A screen-edge preview lands (`mode "de"`); a no-op without one.
    crate::shell::quicktile::drop_preview(state, window);
    crate::shell::drop_window(state, window);
    // A floating window released over another output moves there.
    crate::shell::land_on_output(state, window);
}

/// Drag one or two edges of the window; the opposite edges stay put.
pub struct ResizeSurfaceGrab {
    pub start_data: GrabStartData<AbyssState>,
    pub window: Window,
    pub edges: xdg_toplevel::ResizeEdge,
    /// Window geometry at the moment the grab started.
    pub initial_rect: Rectangle<i32, Logical>,
}

impl ResizeSurfaceGrab {
    fn apply(&self, data: &mut AbyssState, location: Point<f64, Logical>) {
        let delta = (location - self.start_data.location).to_i32_round::<i32>();
        let rect = resized_rect(self.initial_rect, self.edges, delta);
        let rect = snapped_resize(data, &self.window, self.edges, rect);
        crate::shell::place_at(data, &self.window.clone(), rect);
    }
}

impl PointerGrab<AbyssState> for ResizeSurfaceGrab {
    fn motion(
        &mut self,
        data: &mut AbyssState,
        handle: &mut PointerInnerHandle<'_, AbyssState>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        handle.motion(data, None, event);
        self.apply(data, event.location);
    }

    fn relative_motion(
        &mut self,
        data: &mut AbyssState,
        handle: &mut PointerInnerHandle<'_, AbyssState>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, None, event);
    }

    fn button(
        &mut self,
        data: &mut AbyssState,
        handle: &mut PointerInnerHandle<'_, AbyssState>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);
        if event.state == ButtonState::Released && !handle.current_pressed().contains(&self.start_data.button)
        {
            handle.unset_grab(self, data, event.serial, event.time, true);
        }
    }

    fn axis(
        &mut self,
        data: &mut AbyssState,
        handle: &mut PointerInnerHandle<'_, AbyssState>,
        details: AxisFrame,
    ) {
        handle.axis(data, details);
    }

    fn frame(&mut self, data: &mut AbyssState, handle: &mut PointerInnerHandle<'_, AbyssState>) {
        handle.frame(data);
    }

    fn gesture_swipe_begin(
        &mut self,
        data: &mut AbyssState,
        handle: &mut PointerInnerHandle<'_, AbyssState>,
        event: &GestureSwipeBeginEvent,
    ) {
        handle.gesture_swipe_begin(data, event);
    }

    fn gesture_swipe_update(
        &mut self,
        data: &mut AbyssState,
        handle: &mut PointerInnerHandle<'_, AbyssState>,
        event: &GestureSwipeUpdateEvent,
    ) {
        handle.gesture_swipe_update(data, event);
    }

    fn gesture_swipe_end(
        &mut self,
        data: &mut AbyssState,
        handle: &mut PointerInnerHandle<'_, AbyssState>,
        event: &GestureSwipeEndEvent,
    ) {
        handle.gesture_swipe_end(data, event);
    }

    fn gesture_pinch_begin(
        &mut self,
        data: &mut AbyssState,
        handle: &mut PointerInnerHandle<'_, AbyssState>,
        event: &GesturePinchBeginEvent,
    ) {
        handle.gesture_pinch_begin(data, event);
    }

    fn gesture_pinch_update(
        &mut self,
        data: &mut AbyssState,
        handle: &mut PointerInnerHandle<'_, AbyssState>,
        event: &GesturePinchUpdateEvent,
    ) {
        handle.gesture_pinch_update(data, event);
    }

    fn gesture_pinch_end(
        &mut self,
        data: &mut AbyssState,
        handle: &mut PointerInnerHandle<'_, AbyssState>,
        event: &GesturePinchEndEvent,
    ) {
        handle.gesture_pinch_end(data, event);
    }

    fn gesture_hold_begin(
        &mut self,
        data: &mut AbyssState,
        handle: &mut PointerInnerHandle<'_, AbyssState>,
        event: &GestureHoldBeginEvent,
    ) {
        handle.gesture_hold_begin(data, event);
    }

    fn gesture_hold_end(
        &mut self,
        data: &mut AbyssState,
        handle: &mut PointerInnerHandle<'_, AbyssState>,
        event: &GestureHoldEndEvent,
    ) {
        handle.gesture_hold_end(data, event);
    }

    fn start_data(&self) -> &GrabStartData<AbyssState> {
        &self.start_data
    }

    fn unset(&mut self, data: &mut AbyssState) {
        data.pointer_grab_active = false;
        // The resize cursor goes back with the grab (`shell::edge`).
        crate::shell::edge::grab_ended(data);
    }
}

/// Touch twin of [`MoveSurfaceGrab`]: the finger that pressed drags the window.
///
/// Motion is not forwarded — the client is being dragged, not touched. Further
/// fingers, shape and orientation are swallowed for the same reason. `up` and
/// `frame` are forwarded so the client still sees every point it was sent a
/// `down` for released (smithay drops an `up` for a slot the client never saw).
pub struct TouchMoveSurfaceGrab {
    pub start_data: TouchGrabStartData<AbyssState>,
    pub window: Window,
    /// Window-geometry origin at the moment the grab started.
    pub initial_location: Point<i32, Logical>,
}

impl TouchGrab<AbyssState> for TouchMoveSurfaceGrab {
    fn down(
        &mut self,
        _data: &mut AbyssState,
        _handle: &mut TouchInnerHandle<'_, AbyssState>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        _event: &DownEvent,
        _seq: Serial,
    ) {
    }

    fn up(
        &mut self,
        data: &mut AbyssState,
        handle: &mut TouchInnerHandle<'_, AbyssState>,
        event: &UpEvent,
        seq: Serial,
    ) {
        handle.up(data, event, seq);
        if event.slot == self.start_data.slot {
            finish_move(data, &self.window);
            handle.unset_grab(self, data);
        }
    }

    fn motion(
        &mut self,
        data: &mut AbyssState,
        _handle: &mut TouchInnerHandle<'_, AbyssState>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &TouchMotionEvent,
        _seq: Serial,
    ) {
        if event.slot != self.start_data.slot {
            return;
        }
        let delta = event.location - self.start_data.location;
        place_moved(data, &self.window, self.initial_location, delta, event.location);
    }

    fn frame(&mut self, data: &mut AbyssState, handle: &mut TouchInnerHandle<'_, AbyssState>, seq: Serial) {
        handle.frame(data, seq);
    }

    fn cancel(&mut self, data: &mut AbyssState, handle: &mut TouchInnerHandle<'_, AbyssState>, seq: Serial) {
        handle.cancel(data, seq);
        handle.unset_grab(self, data);
    }

    fn shape(
        &mut self,
        _data: &mut AbyssState,
        _handle: &mut TouchInnerHandle<'_, AbyssState>,
        _event: &ShapeEvent,
        _seq: Serial,
    ) {
    }

    fn orientation(
        &mut self,
        _data: &mut AbyssState,
        _handle: &mut TouchInnerHandle<'_, AbyssState>,
        _event: &OrientationEvent,
        _seq: Serial,
    ) {
    }

    fn start_data(&self) -> &TouchGrabStartData<AbyssState> {
        &self.start_data
    }

    fn unset(&mut self, data: &mut AbyssState) {
        // Cancelled, or ended without its finger lifting: back to the
        // placeholder. After `up` this is a no-op.
        crate::shell::cancel_tile_drag(data);
        data.touch_grab_active = false;
    }
}

/// Touch twin of [`ResizeSurfaceGrab`]; same forwarding rules as
/// [`TouchMoveSurfaceGrab`].
pub struct TouchResizeSurfaceGrab {
    pub start_data: TouchGrabStartData<AbyssState>,
    pub window: Window,
    pub edges: xdg_toplevel::ResizeEdge,
    /// Window geometry at the moment the grab started.
    pub initial_rect: Rectangle<i32, Logical>,
}

impl TouchGrab<AbyssState> for TouchResizeSurfaceGrab {
    fn down(
        &mut self,
        _data: &mut AbyssState,
        _handle: &mut TouchInnerHandle<'_, AbyssState>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        _event: &DownEvent,
        _seq: Serial,
    ) {
    }

    fn up(
        &mut self,
        data: &mut AbyssState,
        handle: &mut TouchInnerHandle<'_, AbyssState>,
        event: &UpEvent,
        seq: Serial,
    ) {
        handle.up(data, event, seq);
        if event.slot == self.start_data.slot {
            handle.unset_grab(self, data);
        }
    }

    fn motion(
        &mut self,
        data: &mut AbyssState,
        _handle: &mut TouchInnerHandle<'_, AbyssState>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &TouchMotionEvent,
        _seq: Serial,
    ) {
        if event.slot != self.start_data.slot {
            return;
        }
        let delta = (event.location - self.start_data.location).to_i32_round::<i32>();
        let rect = resized_rect(self.initial_rect, self.edges, delta);
        let rect = snapped_resize(data, &self.window, self.edges, rect);
        crate::shell::place_at(data, &self.window.clone(), rect);
    }

    fn frame(&mut self, data: &mut AbyssState, handle: &mut TouchInnerHandle<'_, AbyssState>, seq: Serial) {
        handle.frame(data, seq);
    }

    fn cancel(&mut self, data: &mut AbyssState, handle: &mut TouchInnerHandle<'_, AbyssState>, seq: Serial) {
        handle.cancel(data, seq);
        handle.unset_grab(self, data);
    }

    fn shape(
        &mut self,
        _data: &mut AbyssState,
        _handle: &mut TouchInnerHandle<'_, AbyssState>,
        _event: &ShapeEvent,
        _seq: Serial,
    ) {
    }

    fn orientation(
        &mut self,
        _data: &mut AbyssState,
        _handle: &mut TouchInnerHandle<'_, AbyssState>,
        _event: &OrientationEvent,
        _seq: Serial,
    ) {
    }

    fn start_data(&self) -> &TouchGrabStartData<AbyssState> {
        &self.start_data
    }

    fn unset(&mut self, data: &mut AbyssState) {
        data.touch_grab_active = false;
    }
}

/// A client-requested move or resize of a maximized or Quick Tiled window (`mode "de"`): when
/// the press is live, the window stops being maximized first and comes back
/// under the pointer, so the grab starts from the restored rectangle.
fn release_maximize(state: &mut AbyssState, window: &Window, surface: &WlSurface, serial: Serial) {
    if crate::shell::maximize::is_placed(state, window)
        && (start_data(state, surface, serial).is_some()
            || touch_start_data(state, surface, serial).is_some())
    {
        crate::shell::maximize::release_for_drag(state, window);
    }
}

/// Begin an interactive move for `window` if `serial` really is a live press
/// (pointer button or touch down) on a surface of the requesting client.
pub fn start_move(state: &mut AbyssState, window: Window, surface: &WlSurface, serial: Serial) {
    release_maximize(state, &window, surface, serial);
    let Some(initial_location) = state.space.element_geometry(&window).map(|g| g.loc) else {
        return;
    };
    if let Some(start_data) = start_data(state, surface, serial) {
        let Some(pointer) = state.seat.get_pointer() else {
            return;
        };
        let grab = MoveSurfaceGrab {
            start_data,
            window,
            initial_location,
        };
        // Set after `set_grab`: it runs the *previous* grab's `unset`, which
        // clears the flag.
        pointer.set_grab(state, grab, serial, Focus::Clear);
        state.pointer_grab_active = true;
        return;
    }
    let Some(start_data) = touch_start_data(state, surface, serial) else {
        return;
    };
    let Some(touch) = state.seat.get_touch() else {
        return;
    };
    let grab = TouchMoveSurfaceGrab {
        start_data,
        window,
        initial_location,
    };
    touch.set_grab(state, grab, serial);
    state.touch_grab_active = true;
}

/// Begin an interactive resize for `window` if `serial` really is a live press
/// (pointer button or touch down) on a surface of the requesting client.
pub fn start_resize(
    state: &mut AbyssState,
    window: Window,
    surface: &WlSurface,
    serial: Serial,
    edges: xdg_toplevel::ResizeEdge,
) {
    // A shaded window has no content to size (`shell::winops`).
    if !crate::shell::winops::can_resize(state, &window) {
        return;
    }
    release_maximize(state, &window, surface, serial);
    let Some(initial_rect) = state.space.element_geometry(&window) else {
        return;
    };
    if let Some(start_data) = start_data(state, surface, serial) {
        let Some(pointer) = state.seat.get_pointer() else {
            return;
        };
        let grab = ResizeSurfaceGrab {
            start_data,
            window,
            edges,
            initial_rect,
        };
        pointer.set_grab(state, grab, serial, Focus::Clear);
        state.pointer_grab_active = true;
        // After `set_grab`, whose focus clear resets the cursor.
        crate::shell::edge::show_cursor(state, edges);
        return;
    }
    let Some(start_data) = touch_start_data(state, surface, serial) else {
        return;
    };
    let Some(touch) = state.seat.get_touch() else {
        return;
    };
    let grab = TouchResizeSurfaceGrab {
        start_data,
        window,
        edges,
        initial_rect,
    };
    touch.set_grab(state, grab, serial);
    state.touch_grab_active = true;
}

/// Which edges a mouse-bound resize drags, chosen by the quadrant of the window
/// the pointer is in (Hyprland's behaviour): the nearest corner moves, the
/// opposite one stays put. A pointer exactly on a midline counts as right or
/// bottom.
pub fn edges_for(rect: Rectangle<i32, Logical>, pos: Point<f64, Logical>) -> xdg_toplevel::ResizeEdge {
    use xdg_toplevel::ResizeEdge as E;
    let cx = rect.loc.x as f64 + rect.size.w as f64 / 2.0;
    let cy = rect.loc.y as f64 + rect.size.h as f64 / 2.0;
    match (pos.x < cx, pos.y < cy) {
        (true, true) => E::TopLeft,
        (false, true) => E::TopRight,
        (true, false) => E::BottomLeft,
        (false, false) => E::BottomRight,
    }
}

/// Begin a compositor-initiated move: the human pressed a `mousebind`, so
/// there is no client serial to vet. The grab starts where the pointer is now.
/// Returns whether it was installed.
pub fn start_move_at(state: &mut AbyssState, window: Window, button: u32) -> bool {
    start_move_from(state, window, button, None)
}

/// [`start_move_at`] measuring the drag from `origin` (where the press was)
/// instead of from where the pointer is now, so a titlebar drag that began a few
/// pixels back carries the window the whole way. A maximized window is restored
/// under the pointer first, so its drag starts from the pointer, not `origin`.
pub fn start_move_from(
    state: &mut AbyssState,
    window: Window,
    button: u32,
    origin: Option<Point<f64, Logical>>,
) -> bool {
    let was_maximized = crate::shell::maximize::is_placed(state, &window);
    crate::shell::maximize::release_for_drag(state, &window);
    let Some(initial_location) = state.space.element_geometry(&window).map(|g| g.loc) else {
        return false;
    };
    let Some(pointer) = state.seat.get_pointer() else {
        return false;
    };
    let location = match origin {
        Some(o) if !was_maximized => o,
        _ => state.pointer_location,
    };
    let grab = MoveSurfaceGrab {
        start_data: GrabStartData {
            focus: None,
            button,
            location,
        },
        window,
        initial_location,
    };
    // Set after `set_grab`, which runs the previous grab's `unset`.
    pointer.set_grab(state, grab, SERIAL_COUNTER.next_serial(), Focus::Clear);
    state.pointer_grab_active = true;
    true
}

/// Height of the strip along the top of a floating `mode "de"` window that a
/// press can start a move from, logical px. A fixed constant, not a config key.
pub(crate) const DRAG_STRIP_HEIGHT: i32 = 32;

/// Whether `pos` lies in the top [`DRAG_STRIP_HEIGHT`] rows of `content`.
pub(crate) fn in_drag_strip(content: Rectangle<i32, Logical>, pos: Point<f64, Logical>) -> bool {
    let height = DRAG_STRIP_HEIGHT.min(content.size.h);
    let x = pos.x >= content.loc.x as f64 && pos.x < (content.loc.x + content.size.w) as f64;
    let y = pos.y >= content.loc.y as f64 && pos.y < (content.loc.y + height) as f64;
    x && y
}

/// A left press on the top strip of a window, waiting to see whether it
/// becomes a move. The press itself went to the seat (the client and click to
/// focus see it); only motion past the titlebar drag threshold starts the grab.
#[derive(Debug)]
pub struct StripDrag {
    pub window: Window,
    pub origin: Point<f64, Logical>,
}

/// The floating window whose top strip is under the pointer, when that strip
/// may start a move: `mode "de"`, a Wayland toplevel with no compositor
/// titlebar of its own (a client-side-decorated window; server-side ones drag
/// by their bar), not fullscreen, and not while the screen is locked.
fn strip_window_under_pointer(state: &AbyssState) -> Option<Window> {
    if !crate::shell::de::stacking(state) || state.lock.locked || state.pointer_grab_active {
        return None;
    }
    let window = grabbable_window_under_pointer(state)?;
    if window.toplevel().is_none() || crate::shell::titlebar::bar_height(state, &window) > 0 {
        return None;
    }
    let id = crate::shell::output_of_window(state, &window)?;
    let entry = state.outputs.get(id)?;
    if !entry.workspaces[entry.active]
        .floating
        .iter()
        .any(|f| f.window == window)
    {
        return None;
    }
    let content = state.space.element_geometry(&window)?;
    in_drag_strip(content, state.pointer_location).then_some(window)
}

/// A left press (not taken by a titlebar, edge, or mousebind): arm a strip drag
/// if the press is on a floating window's top strip. Never swallows the press.
pub fn arm_strip_drag(state: &mut AbyssState) {
    state.strip_drag = strip_window_under_pointer(state).map(|window| StripDrag {
        window,
        origin: state.pointer_location,
    });
}

/// Pointer motion: once an armed strip press has moved past the titlebar drag
/// threshold, start the same move grab a titlebar drag uses.
pub fn strip_motion(state: &mut AbyssState) {
    let Some(origin) = state.strip_drag.as_ref().map(|d| d.origin) else {
        return;
    };
    let p = state.pointer_location;
    let (dx, dy) = (p.x - origin.x, p.y - origin.y);
    let threshold = crate::shell::titlebar::DRAG_THRESHOLD;
    if dx * dx + dy * dy < threshold * threshold {
        return;
    }
    let Some(StripDrag { window, origin }) = state.strip_drag.take() else {
        return;
    };
    if state.lock.locked || drag_active_but_click(state) {
        return;
    }
    start_move_from(state, window, super::BTN_LEFT, Some(origin));
}

/// A button release ends an armed strip press without a move.
pub fn strip_release(state: &mut AbyssState, button: u32) {
    if button == super::BTN_LEFT {
        state.strip_drag = None;
    }
}

/// Compositor-initiated resize; the dragged corner is the one nearest the
/// pointer (see [`edges_for`]). Returns whether it was installed.
pub fn start_resize_at(state: &mut AbyssState, window: Window, button: u32) -> bool {
    if !crate::shell::winops::can_resize(state, &window) {
        return false;
    }
    crate::shell::maximize::release_for_drag(state, &window);
    let Some(initial_rect) = state.space.element_geometry(&window) else {
        return false;
    };
    let Some(pointer) = state.seat.get_pointer() else {
        return false;
    };
    let location = state.pointer_location;
    let edges = edges_for(initial_rect, location);
    let grab = ResizeSurfaceGrab {
        start_data: GrabStartData {
            focus: None,
            button,
            location,
        },
        window,
        edges,
        initial_rect,
    };
    pointer.set_grab(state, grab, SERIAL_COUNTER.next_serial(), Focus::Clear);
    state.pointer_grab_active = true;
    crate::shell::edge::show_cursor(state, edges);
    tracing::info!(?edges, "resize from the window band started");
    true
}

/// Begin a resize of `edges` from the resize band of a decorated window under
/// `mode "de"` (`shell::edge`): the human pressed `button` in the band, so
/// there is no client serial to vet. Returns whether the grab was installed.
pub fn start_resize_edges(
    state: &mut AbyssState,
    window: Window,
    button: u32,
    edges: xdg_toplevel::ResizeEdge,
) -> bool {
    if state.lock.locked
        || drag_active(state)
        || state.fullscreen.contains_key(&window)
        || !crate::shell::winops::can_resize(state, &window)
    {
        return false;
    }
    let Some(initial_rect) = state.space.element_geometry(&window) else {
        return false;
    };
    let Some(pointer) = state.seat.get_pointer() else {
        return false;
    };
    let grab = ResizeSurfaceGrab {
        start_data: GrabStartData {
            focus: None,
            button,
            location: state.pointer_location,
        },
        window,
        edges,
        initial_rect,
    };
    pointer.set_grab(state, grab, SERIAL_COUNTER.next_serial(), Focus::Clear);
    state.pointer_grab_active = true;
    crate::shell::edge::show_cursor(state, edges);
    true
}

/// The root of a surface tree: a subsurface resolves to the surface it hangs
/// off, so a press on a subsurface counts as a press on its window.
fn root_surface(surface: &WlSurface) -> WlSurface {
    let mut root = surface.clone();
    while let Some(parent) = smithay::wayland::compositor::get_parent(&root) {
        root = parent;
    }
    root
}

/// A `mousebind` press (COMP-04 §5, ADR 0057): if `button` went down with
/// exactly the bound modifiers over a managed toplevel, start the bound
/// move/resize on it and return `true`.
///
/// Called *before* the press is handed to the seat. The grab clears pointer
/// focus, so the press lands in the grab and the client never sees it — nor the
/// matching release. Declines (returns `false`, press proceeds normally) when a
/// drag is already in flight, the session is locked, nothing bound matches, the
/// surface under the pointer is not a toplevel (layer surfaces, popups,
/// override-redirect X11 windows) or the window is fullscreen.
///
/// The lookup allocates nothing (a refcount bump per subsurface hop at most);
/// focusing an X11 window on a hit is the press path's, not the motion path's.
pub fn start_mouse_bind(state: &mut AbyssState, button: u32) -> bool {
    if state.lock.locked || drag_active(state) {
        return false;
    }
    let Some(keyboard) = state.seat.get_keyboard() else {
        return false;
    };
    let mods = keyboard.modifier_state();
    let Some(action) = state.config.mouse_bind_for(&super::held_mods(&mods), button) else {
        return false;
    };
    let Some(window) = grabbable_window_under_pointer(state) else {
        return false;
    };
    // Raise and focus the dragged window now rather than leaving it to
    // click-to-focus, which is frozen when the drag started on another output.
    crate::shell::focus::focus_window_raising(state, &window, true);
    match action {
        MouseAction::MoveWindow => start_move_at(state, window, button),
        MouseAction::ResizeWindow => start_resize_at(state, window, button),
    }
}

/// The managed toplevel under the pointer, if the human may drag it: not a
/// layer surface, popup or override-redirect X11 window, not fullscreen, and on an output.
fn grabbable_window_under_pointer(state: &AbyssState) -> Option<Window> {
    let (surface, _) = state.surface_under(state.pointer_location)?;
    let window = crate::shell::window_for_surface(state, &root_surface(&surface))?;
    if state.fullscreen.contains_key(&window) || crate::shell::output_of_window(state, &window).is_none() {
        return None;
    }
    Some(window)
}

/// A touchpad drag gesture moving a window, from the gesture's begin to its
/// end. Only the accumulated delta changes per motion; nothing allocates.
#[derive(Debug)]
pub struct GestureDrag {
    /// 2 for a finger scroll, 3 or 4 for a swipe: which event stream owns it.
    pub fingers: u32,
    window: Window,
    initial_location: Point<i32, Logical>,
    delta: Point<f64, Logical>,
}

/// Whether a `fingers`-finger touchpad gesture beginning now is a window drag
/// rather than the app's: bound with exactly the held modifiers, not under the
/// lock, not while a region selection or another drag holds the input.
pub fn drag_gesture_claims(
    config: &Config,
    fingers: u32,
    mods: &smithay::input::keyboard::ModifiersState,
    locked: bool,
    busy: bool,
) -> bool {
    !locked && !busy && config.drag_gesture_bound(fingers, &super::held_mods(mods))
}

/// Claim a touchpad gesture beginning now as a window drag if it is one (see
/// [`drag_gesture_claims`]) and a draggable window is under the pointer.
/// Returns whether it was claimed; if not, the gesture is the app's.
pub fn start_drag_gesture(state: &mut AbyssState, fingers: u32) -> bool {
    let Some(keyboard) = state.seat.get_keyboard() else {
        return false;
    };
    let mods = keyboard.modifier_state();
    let busy = state.region_select.active() || state.gesture_drag.is_some() || drag_active(state);
    if !drag_gesture_claims(&state.config, fingers, &mods, state.lock.locked, busy) {
        return false;
    }
    let Some(window) = grabbable_window_under_pointer(state) else {
        return false;
    };
    let Some(initial_location) = state.space.element_geometry(&window).map(|g| g.loc) else {
        return false;
    };
    crate::shell::focus::focus_window_raising(state, &window, true);
    state.gesture_drag = Some(GestureDrag {
        fingers,
        window,
        initial_location,
        delta: Point::default(),
    });
    true
}

/// Start a gesture drag of `window` as `start_drag_gesture` would, minus the
/// modifier and hit-test claim (tests).
#[cfg(test)]
pub(crate) fn begin_gesture_drag_for_test(state: &mut AbyssState, window: &Window) {
    let initial_location = state.space.element_geometry(window).unwrap().loc;
    state.gesture_drag = Some(GestureDrag {
        fingers: 2,
        window: window.clone(),
        initial_location,
        delta: Point::default(),
    });
}

/// Move the dragged window by `delta` more, through the same path as a
/// pointer move (Radiant tile drag, or a floating placement).
pub fn update_drag_gesture(state: &mut AbyssState, delta: Point<f64, Logical>) {
    let Some(drag) = state.gesture_drag.as_mut() else {
        return;
    };
    drag.delta += delta;
    let (window, initial_location, delta) = (drag.window.clone(), drag.initial_location, drag.delta);
    // A swipe does not move the pointer: the point the edge preview follows is
    // where it would be had it carried the window.
    let pointer = state.pointer_location + delta;
    place_moved(state, &window, initial_location, delta, pointer);
}

/// The fingers lifted: land the window where the guides showed, or put a
/// cancelled drag's tile back on its placeholder.
pub fn end_drag_gesture(state: &mut AbyssState, cancelled: bool) {
    let Some(drag) = state.gesture_drag.take() else {
        return;
    };
    if cancelled {
        crate::shell::cancel_tile_drag(state);
    } else {
        finish_move(state, &drag.window);
    }
}

/// The single definition of "a drag is in flight" (COMP-05 §5).
///
/// Focus must not follow the mouse across an output boundary mid-drag, and a
/// drag is more than this crate's own interactive move/resize: a client can
/// hold the pointer itself (`wl_data_device.start_drag`, or any grab it
/// installed), and a popup grab is a held-pointer interaction too.
///
/// Smithay `=0.7.0` does not expose "is a DnD grab active" as its own
/// predicate — the DnD grab is just a `PointerGrab` it installs on the seat
/// (`wayland/selection/data_device/dnd_grab.rs`), so it is observable only as
/// `PointerHandle::is_grabbed`. The repo's own `ClientDndGrabHandler` tracking
/// (`dnd_icon`, set in `started`, cleared in `dropped`) is checked too, since
/// a touch-initiated DnD never touches the pointer grab at all.
///
/// A shell-owned touch move/resize (`touch_grab_active`) is a drag too. It is
/// a plain flag for the same reason as `pointer_grab_active`: smithay holds the
/// touch mutex across a touch grab's callbacks, and this predicate is reached
/// from inside them (`place_at` → focus refresh). `TouchHandle::is_grabbed` is
/// deliberately not consulted — it is true for every plain finger press.
///
/// Order matters: the lock-free reads come first, because
/// `PointerHandle::is_grabbed` takes the pointer's internal mutex — which
/// smithay holds across a grab's own callbacks. Checking
/// `pointer_grab_active` first short-circuits the one re-entrant case.
pub fn drag_active(state: &AbyssState) -> bool {
    drag_active_but_click(state) || state.seat.get_pointer().is_some_and(|p| p.is_grabbed())
}

/// [`drag_active`] without `PointerHandle::is_grabbed`, for the click-to-focus
/// decision. A button press installs smithay's implicit click grab before
/// click-to-focus runs, so `is_grabbed` is true for every plain click; read
/// there, it made the press its own "drag" and froze focus whenever the
/// keyboard's owner was on no output — an on-demand layer surface holding it.
pub fn drag_active_but_click(state: &AbyssState) -> bool {
    state.pointer_grab_active
        || state.touch_grab_active
        || state.gesture_drag.is_some()
        || !state.popup_grabs.is_empty()
        || state.dnd_icon.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use xdg_toplevel::ResizeEdge as E;

    fn r(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((x, y)), Size::from((w, h)))
    }

    #[test]
    fn resize_moves_only_the_dragged_edges() {
        let init = r(100, 100, 400, 300);
        let d = Point::from((50, 30));
        assert_eq!(resized_rect(init, E::BottomRight, d), r(100, 100, 450, 330));
        assert_eq!(resized_rect(init, E::TopLeft, d), r(150, 130, 350, 270));
        assert_eq!(resized_rect(init, E::Right, d), r(100, 100, 450, 300));
        assert_eq!(resized_rect(init, E::Top, d), r(100, 130, 400, 270));
        assert_eq!(resized_rect(init, E::None, d), init);
    }

    #[test]
    fn drag_strip_is_the_top_rows_of_the_content_only() {
        let content = r(100, 100, 300, 200);
        let at = |x: f64, y: f64| in_drag_strip(content, Point::from((x, y)));
        assert!(at(100.0, 100.0), "the first row, first column");
        assert!(
            at(399.0, 100.0 + DRAG_STRIP_HEIGHT as f64 - 1.0),
            "the last strip row"
        );
        assert!(
            !at(200.0, 100.0 + DRAG_STRIP_HEIGHT as f64),
            "one row below the strip"
        );
        assert!(!at(200.0, 99.0), "above the content");
        assert!(!at(400.0, 150.0), "right of the content");
        assert!(!at(99.0, 150.0), "left of the content");
        // A window shorter than the strip is all strip, never past its bottom.
        let short = r(0, 0, 50, 10);
        assert!(in_drag_strip(short, Point::from((5.0, 9.0))));
        assert!(!in_drag_strip(short, Point::from((5.0, 10.0))));
    }

    #[test]
    fn resize_never_collapses_below_one_pixel() {
        let got = resized_rect(r(0, 0, 10, 10), E::BottomRight, Point::from((-50, -50)));
        assert_eq!(got.size, Size::from((1, 1)));
    }

    #[test]
    fn edges_for_picks_the_nearest_corner() {
        let rect = r(100, 200, 400, 300); // centre (300, 350)
        let at = |x: f64, y: f64| edges_for(rect, Point::from((x, y)));
        assert_eq!(at(110.0, 210.0), E::TopLeft);
        assert_eq!(at(490.0, 210.0), E::TopRight);
        assert_eq!(at(110.0, 490.0), E::BottomLeft);
        assert_eq!(at(490.0, 490.0), E::BottomRight);
        // The midlines belong to the right and bottom halves.
        assert_eq!(at(300.0, 350.0), E::BottomRight);
        assert_eq!(at(299.9, 349.9), E::TopLeft);
    }

    #[test]
    fn a_mouse_resize_keeps_the_corner_opposite_the_pointer_fixed() {
        let rect = r(100, 200, 400, 300);
        let start = Point::from((110.0, 210.0));
        let edges = edges_for(rect, start);
        // Dragging the top-left corner right/down shrinks from that corner.
        assert_eq!(
            resized_rect(rect, edges, Point::from((20, 10))),
            r(120, 210, 380, 290)
        );
    }

    fn hold_alt(s: &mut AbyssState) {
        let kbd = s.seat.get_keyboard().expect("keyboard capability");
        let mods = smithay::input::keyboard::ModifiersState {
            alt: true,
            ..Default::default()
        };
        kbd.set_modifier_state(mods);
    }

    #[test]
    fn a_drag_gesture_is_claimed_only_with_its_exact_modifiers_and_nothing_in_the_way() {
        use smithay::input::keyboard::ModifiersState;
        let cfg = Config::default();
        let sup = ModifiersState {
            logo: true,
            ..Default::default()
        };
        assert!(drag_gesture_claims(&cfg, 2, &sup, false, false));
        // Unbound finger count, no modifier, extra modifier, wrong modifier.
        assert!(!drag_gesture_claims(&cfg, 3, &sup, false, false));
        assert!(!drag_gesture_claims(
            &cfg,
            2,
            &ModifiersState::default(),
            false,
            false
        ));
        let sup_shift = ModifiersState { shift: true, ..sup };
        assert!(!drag_gesture_claims(&cfg, 2, &sup_shift, false, false));
        let alt = ModifiersState {
            alt: true,
            ..Default::default()
        };
        assert!(!drag_gesture_claims(&cfg, 2, &alt, false, false));
        // The lock, and a drag or selection already holding the input.
        assert!(!drag_gesture_claims(&cfg, 2, &sup, true, false));
        assert!(!drag_gesture_claims(&cfg, 2, &sup, false, true));
    }

    /// With Super held but no window under the pointer the gesture is the
    /// app's, and nothing is left claimed.
    #[test]
    fn a_drag_gesture_over_no_window_is_not_claimed() {
        let mut h = crate::shell::focus::state_tests::harness();
        let s = &mut h.state;
        let kbd = s.seat.get_keyboard().expect("keyboard capability");
        kbd.set_modifier_state(smithay::input::keyboard::ModifiersState {
            logo: true,
            ..Default::default()
        });
        assert!(!start_drag_gesture(s, 2));
        assert!(s.gesture_drag.is_none());
        assert!(!drag_active(s));
    }

    /// The harness has no client, so there is never a window to grab; every
    /// path must decline and leave the press for the seat.
    #[test]
    fn a_mousebind_press_with_no_window_under_the_pointer_starts_nothing() {
        let mut h = crate::shell::focus::state_tests::harness();
        let s = &mut h.state;
        hold_alt(s);
        assert!(!start_mouse_bind(s, 0x110));
        assert!(!start_mouse_bind(s, 0x111));
        assert!(!s.pointer_grab_active);
        assert!(!s.seat.get_pointer().unwrap().is_grabbed());
    }

    #[test]
    fn a_press_without_the_bound_modifiers_or_button_starts_nothing() {
        let mut h = crate::shell::focus::state_tests::harness();
        let s = &mut h.state;
        // No modifier held.
        assert!(!start_mouse_bind(s, 0x110));
        hold_alt(s);
        // Bound modifier, unbound button.
        assert!(!start_mouse_bind(s, 0x112));
        assert!(!s.pointer_grab_active);
    }

    #[test]
    fn a_mousebind_press_is_declined_while_a_drag_is_in_flight() {
        let mut h = crate::shell::focus::state_tests::harness();
        let s = &mut h.state;
        hold_alt(s);
        s.pointer_grab_active = true;
        assert!(!start_mouse_bind(s, 0x110));
    }

    #[test]
    fn a_touch_grab_counts_as_a_drag_and_a_bare_press_does_not() {
        let mut h = crate::shell::focus::state_tests::harness();
        let s = &mut h.state;
        let at = s.pointer_location;
        // A plain finger press holds smithay's touch grab but is not a drag.
        s.inject_touch_down(0, at, 0);
        assert!(!drag_active(s));
        s.inject_touch_up(0, 1);
        s.touch_grab_active = true;
        assert!(drag_active(s));
    }

    #[test]
    fn a_touch_serial_that_is_not_live_starts_nothing() {
        let mut h = crate::shell::focus::state_tests::harness();
        let s = &mut h.state;
        let at = s.pointer_location;
        s.inject_touch_down(0, at, 0);
        let touch = s.seat.get_touch().expect("touch capability");
        // Nothing is under the point, so there is no focus for a client to
        // claim, and a made-up serial is not the one the press holds.
        let bogus = smithay::utils::SERIAL_COUNTER.next_serial();
        assert!(!touch.has_grab(bogus));
        assert!(touch.grab_start_data().and_then(|d| d.focus).is_none());
        assert!(!s.touch_grab_active);
    }
}
