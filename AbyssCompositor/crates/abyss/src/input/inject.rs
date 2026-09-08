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
    backend::input::ButtonState,
    input::pointer::{ButtonEvent, RelativeMotionEvent},
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
    }
}
