// SPDX-License-Identifier: AGPL-3.0-only
//! Interactive move and resize pointer grabs (COMP-04 §3, COMP-05 §3).
//!
//! A client asks for these through `xdg_toplevel.move` / `xdg_toplevel.resize`.
//! While a grab is active the pointer focus is cleared, so the client sees a
//! `wl_pointer.leave` and stops reacting to the drag itself; every motion is
//! turned into a new window geometry instead. The grab ends when the button
//! that started it is released.

use smithay::{
    backend::input::ButtonState,
    desktop::Window,
    input::pointer::{
        AxisFrame, ButtonEvent, Focus, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
        GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
        GestureSwipeUpdateEvent, GrabStartData, MotionEvent, PointerGrab, PointerInnerHandle,
        RelativeMotionEvent,
    },
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::{protocol::wl_surface::WlSurface, Resource},
    },
    utils::{Logical, Point, Rectangle, Serial, Size},
};

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
        let loc = self.initial_location.to_f64() + delta;
        let size = data
            .space
            .element_geometry(&self.window)
            .map(|g| g.size)
            .unwrap_or_default();
        crate::shell::place_at(
            data,
            &self.window.clone(),
            Rectangle::new(loc.to_i32_round(), size),
        );
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
        let (mut x, mut y) = (self.initial_rect.loc.x, self.initial_rect.loc.y);
        let (mut w, mut h) = (self.initial_rect.size.w, self.initial_rect.size.h);
        use xdg_toplevel::ResizeEdge as E;
        let (top, bottom, left, right) = match self.edges {
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
        let rect = Rectangle::new(Point::from((x, y)), Size::from((w.max(1), h.max(1))));
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

/// Begin an interactive move for `window` if `serial` really is a live press.
pub fn start_move(state: &mut AbyssState, window: Window, surface: &WlSurface, serial: Serial) {
    let Some(start_data) = start_data(state, surface, serial) else {
        return;
    };
    let Some(initial_location) = state.space.element_geometry(&window).map(|g| g.loc) else {
        return;
    };
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
}

/// Begin an interactive resize for `window` if `serial` really is a live press.
pub fn start_resize(
    state: &mut AbyssState,
    window: Window,
    surface: &WlSurface,
    serial: Serial,
    edges: xdg_toplevel::ResizeEdge,
) {
    let Some(start_data) = start_data(state, surface, serial) else {
        return;
    };
    let Some(initial_rect) = state.space.element_geometry(&window) else {
        return;
    };
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
}
