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
/// being floated (`shell::drag_tile`); `pointer` aims the drop.
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
    if crate::shell::drag_tile(data, window, loc.to_i32_round(), pointer) {
        return;
    }
    let size = data
        .space
        .element_geometry(window)
        .map(|g| g.size)
        .unwrap_or_default();
    crate::shell::place_at(data, &window.clone(), Rectangle::new(loc.to_i32_round(), size));
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
            finish_move(data, &self.window, handle.current_location());
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
pub fn finish_move(state: &mut AbyssState, window: &Window, pos: Point<f64, Logical>) {
    crate::shell::drop_window(state, window, pos);
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
    /// Where the dragging finger last was: `wl_touch.up` carries no position,
    /// and the drop lands where the finger left the glass.
    pub last: Point<f64, Logical>,
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
            finish_move(data, &self.window, self.last);
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
        self.last = event.location;
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

/// Begin an interactive move for `window` if `serial` really is a live press
/// (pointer button or touch down) on a surface of the requesting client.
pub fn start_move(state: &mut AbyssState, window: Window, surface: &WlSurface, serial: Serial) {
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
    let last = start_data.location;
    let grab = TouchMoveSurfaceGrab {
        start_data,
        window,
        initial_location,
        last,
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
    let Some(initial_location) = state.space.element_geometry(&window).map(|g| g.loc) else {
        return false;
    };
    let Some(pointer) = state.seat.get_pointer() else {
        return false;
    };
    let grab = MoveSurfaceGrab {
        start_data: GrabStartData {
            focus: None,
            button,
            location: state.pointer_location,
        },
        window,
        initial_location,
    };
    // Set after `set_grab`, which runs the previous grab's `unset`.
    pointer.set_grab(state, grab, SERIAL_COUNTER.next_serial(), Focus::Clear);
    state.pointer_grab_active = true;
    true
}

/// Compositor-initiated resize; the dragged corner is the one nearest the
/// pointer (see [`edges_for`]). Returns whether it was installed.
pub fn start_resize_at(state: &mut AbyssState, window: Window, button: u32) -> bool {
    let Some(initial_rect) = state.space.element_geometry(&window) else {
        return false;
    };
    let Some(pointer) = state.seat.get_pointer() else {
        return false;
    };
    let location = state.pointer_location;
    let grab = ResizeSurfaceGrab {
        start_data: GrabStartData {
            focus: None,
            button,
            location,
        },
        window,
        edges: edges_for(initial_rect, location),
        initial_rect,
    };
    pointer.set_grab(state, grab, SERIAL_COUNTER.next_serial(), Focus::Clear);
    state.pointer_grab_active = true;
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
/// override-redirect X11 windows) or the window is maximized/fullscreen.
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
    let Some(action) = state.config.mouse_bind_for(&mods, button) else {
        return false;
    };
    let Some((surface, _)) = state.surface_under(state.pointer_location) else {
        return false;
    };
    let Some(window) = crate::shell::window_for_surface(state, &root_surface(&surface)) else {
        return false;
    };
    if state.maximized.contains_key(&window)
        || state.fullscreen.contains_key(&window)
        || crate::shell::output_of_window(state, &window).is_none()
    {
        return false;
    }
    // Raise and focus the dragged window now rather than leaving it to
    // click-to-focus, which is frozen when the drag started on another output.
    crate::shell::focus::focus_window_raising(state, &window, true);
    match action {
        MouseAction::MoveWindow => start_move_at(state, window, button),
        MouseAction::ResizeWindow => start_resize_at(state, window, button),
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
    if state.pointer_grab_active
        || state.touch_grab_active
        || !state.popup_grabs.is_empty()
        || state.dnd_icon.is_some()
    {
        return true;
    }
    state.seat.get_pointer().is_some_and(|p| p.is_grabbed())
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
