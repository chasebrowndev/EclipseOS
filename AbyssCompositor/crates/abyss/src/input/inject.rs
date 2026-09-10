// SPDX-License-Identifier: AGPL-3.0-only
//! Synthetic input (COMP-04 §6).
//!
//! A headless session has no input device, so events reach it through this
//! path alone: the conformance harness, the IPC layer and (later) agent seats
//! all call in here. The point of routing them through `AbyssState` rather
//! than through `PointerHandle` directly is that everything a real device
//! event triggers — idle activity, output focus, focus-follows-mouse, cursor
//! clamping, session-lock suppression — happens for an injected event too. A
//! test that drove the seat handle directly would be testing the seat, not the
//! compositor.

use smithay::{
    backend::input::{ButtonState, TouchSlot},
    input::{
        pointer::{ButtonEvent, RelativeMotionEvent},
        touch::{DownEvent, MotionEvent as TouchMotionEvent, UpEvent},
    },
    utils::{Logical, Point, SERIAL_COUNTER},
};

use crate::state::AbyssState;

impl AbyssState {
    /// Absolute pointer motion, in global compositor coordinates.
    pub fn inject_pointer_absolute(&mut self, location: Point<f64, Logical>, time: u32) {
        super::idle::on_activity(self);
        self.pointer_moved(location, time);
    }

    /// Relative pointer motion. Both the relative-pointer protocol event and
    /// the absolute one are sent, in that order — the same pair a libinput
    /// motion event produces.
    pub fn inject_pointer_relative(&mut self, delta: Point<f64, Logical>, time: u32) {
        super::idle::on_activity(self);
        let pointer = self.seat.get_pointer().unwrap();
        pointer.relative_motion(
            self,
            self.surface_under(self.pointer_location),
            &RelativeMotionEvent {
                delta,
                delta_unaccel: delta,
                utime: time as u64 * 1000,
            },
        );
        let location = self.pointer_location + delta;
        self.pointer_moved(location, time);
    }

    /// Pointer button press or release, by evdev button code.
    pub fn inject_pointer_button(&mut self, button: u32, pressed: bool, time: u32) {
        super::idle::on_activity(self);
        let serial = SERIAL_COUNTER.next_serial();
        let pointer = self.seat.get_pointer().unwrap();
        // Click-to-focus. Skipped under a grab (an active drag or popup grab
        // owns the focus) and under the lock screen, which sees no client.
        if pressed && !pointer.is_grabbed() && !self.lock.locked {
            if let Some(window) = self
                .space
                .element_under(self.pointer_location)
                .map(|(w, _)| w.clone())
            {
                self.space.raise_element(&window, true);
                self.focus = Some(window.clone());
                let target = window.toplevel().map(|t| t.wl_surface().clone());
                self.seat.get_keyboard().unwrap().set_focus(self, target, serial);
                crate::shell::arrange(self);
            }
        }
        pointer.button(
            self,
            &ButtonEvent {
                serial,
                time,
                button,
                state: if pressed {
                    ButtonState::Pressed
                } else {
                    ButtonState::Released
                },
            },
        );
        pointer.frame(self);
        // A press outside the grab dismisses it, and must be delivered after
        // the button: xdg-shell forbids `popup_done` preceding its cause.
        if pressed && !self.popup_grabs.is_empty() {
            let under = self.surface_under(self.pointer_location).map(|(s, _)| s);
            crate::shell::popup_grab_button_press(self, under.as_ref());
        }
    }

    /// A scroll frame. The `AxisFrame` is built by the caller, since only it
    /// knows the source, discrete steps and stop flags of the event.
    pub fn inject_pointer_axis(&mut self, frame: smithay::input::pointer::AxisFrame) {
        super::idle::on_activity(self);
        let pointer = self.seat.get_pointer().unwrap();
        pointer.axis(self, frame);
        pointer.frame(self);
    }

    /// A touch point coming down, in global compositor coordinates.
    ///
    /// Touch has no cursor, so there is nothing to clamp and no
    /// focus-follows-motion: the surface under the point at down time owns the
    /// whole sequence (smithay's `DefaultGrab` holds it until the last point
    /// lifts). Down does raise and focus its window, the same way a pointer
    /// click does — a tap is how a touch user picks a window.
    pub fn inject_touch_down(&mut self, slot: u32, location: Point<f64, Logical>, time: u32) {
        super::idle::on_activity(self);
        let serial = SERIAL_COUNTER.next_serial();
        let touch = match self.seat.get_touch() {
            Some(touch) => touch,
            None => return,
        };
        // Fail closed under the lock screen: no surface behind it sees input.
        let focus = if self.lock.locked {
            None
        } else {
            self.surface_under(location)
        };
        if !self.lock.locked && !touch.is_grabbed() {
            if let Some(window) = self.space.element_under(location).map(|(w, _)| w.clone()) {
                self.space.raise_element(&window, true);
                self.focus = Some(window.clone());
                let target = window.toplevel().map(|t| t.wl_surface().clone());
                self.seat.get_keyboard().unwrap().set_focus(self, target, serial);
                crate::shell::arrange(self);
            }
        }
        if let Some((surface, _)) = &focus {
            if let Some(client) = smithay::reexports::wayland_server::Resource::client(surface) {
                self.touch_points.retain(|p| p.slot != slot);
                self.touch_points.push(crate::state::TouchPoint {
                    slot,
                    surface: surface.clone(),
                    client,
                });
            }
        }
        touch.down(
            self,
            focus,
            &DownEvent {
                slot: TouchSlot::from(Some(slot)),
                location,
                serial,
                time,
            },
        );
        touch.frame(self);
    }

    /// A touch point moving. The focus handed to smithay here is only used to
    /// find drag-and-drop targets; the point keeps the surface it came down on.
    pub fn inject_touch_motion(&mut self, slot: u32, location: Point<f64, Logical>, time: u32) {
        super::idle::on_activity(self);
        let touch = match self.seat.get_touch() {
            Some(touch) => touch,
            None => return,
        };
        let focus = if self.lock.locked {
            None
        } else {
            self.surface_under(location)
        };
        touch.motion(
            self,
            focus,
            &TouchMotionEvent {
                slot: TouchSlot::from(Some(slot)),
                location,
                time,
            },
        );
        touch.frame(self);
    }

    /// A touch point lifting.
    pub fn inject_touch_up(&mut self, slot: u32, time: u32) {
        super::idle::on_activity(self);
        self.touch_points.retain(|p| p.slot != slot);
        let touch = match self.seat.get_touch() {
            Some(touch) => touch,
            None => return,
        };
        touch.up(
            self,
            &UpEvent {
                slot: TouchSlot::from(Some(slot)),
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        touch.frame(self);
    }

    /// Release every touch point that came down on `surface`.
    ///
    /// Called when a surface is destroyed: the client must still get an `up`
    /// for each id it saw come down, or it goes on believing the point is held.
    /// `wl_touch.cancel` is not a substitute — it carries no id, and a client
    /// that does not listen for it (wlcs does not) never learns anything.
    ///
    /// smithay cannot do this for us. It routes touch events through the focus
    /// surface (`for_each_focused_touch` matches `wl_touch` instances by
    /// client), and by the time `CompositorHandler::destroyed` runs the surface
    /// is already dead and has no client, so the event is silently dropped. So
    /// the `up` goes straight to the client's `wl_touch` here, and smithay's own
    /// slot bookkeeping is then cleared through the normal `up` path (which
    /// delivers nothing, for the same reason).
    pub(crate) fn release_touch_on(
        &mut self,
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        time: u32,
    ) {
        let points: Vec<crate::state::TouchPoint> = self
            .touch_points
            .iter()
            .filter(|p| &p.surface == surface)
            .cloned()
            .collect();
        for point in points {
            if let Some(touch) = client_touch(&self.display_handle, &point.client) {
                let serial = SERIAL_COUNTER.next_serial();
                touch.up(serial.into(), time, point.slot as i32);
                touch.frame();
            }
            self.inject_touch_up(point.slot, time);
        }
    }
}

/// The client's `wl_touch`, if it has one.
///
/// wayland-server exposes no way to enumerate a client's objects, only
/// `object_from_protocol_id`, so the client's id space is walked. Client object
/// ids are handed out from 2 upwards and a Wayland client that has a seat has a
/// handful of objects, not thousands; this runs once per touch point whose
/// surface was destroyed under it, never on the input path.
fn client_touch(
    dh: &smithay::reexports::wayland_server::DisplayHandle,
    client: &smithay::reexports::wayland_server::Client,
) -> Option<smithay::reexports::wayland_server::protocol::wl_touch::WlTouch> {
    (2..=MAX_CLIENT_OBJECT_ID)
        .find_map(|id| client.object_from_protocol_id(dh, id).ok())
        .filter(smithay::reexports::wayland_server::Resource::is_alive)
}

/// How far to walk a client's object id space looking for its `wl_touch`.
const MAX_CLIENT_OBJECT_ID: u32 = 1024;
