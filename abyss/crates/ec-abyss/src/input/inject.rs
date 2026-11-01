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
        pointer::ButtonEvent,
        touch::{DownEvent, MotionEvent as TouchMotionEvent, UpEvent},
    },
    utils::{Logical, Point, SERIAL_COUNTER},
};

use crate::state::AbyssState;

use smithay::input::keyboard::{Keycode, Keysym};

/// A key, and the Shift that must be held to get its keysym.
#[derive(Clone, Copy)]
struct KeySeq {
    shift: Option<Keycode>,
    key: Keycode,
}

/// Find the key that types `sym` in the seat's active layout, bare or shifted.
fn find_key(state: &mut AbyssState, sym: u32) -> Option<KeySeq> {
    let kbd = state.seat.get_keyboard()?;
    kbd.with_xkb_state(state, |ctx| {
        let xkb = ctx.xkb().lock().ok()?;
        // SAFETY: the keymap reference does not outlive the guard.
        let keymap = unsafe { xkb.keymap() };
        let active = xkb.active_layout().0;
        let target = Keysym::new(sym);
        let shift = Keysym::new(0xffe1);
        let mut shift_key = None;
        let mut best: Option<(Keycode, u32)> = None;
        for raw in keymap.min_keycode().raw()..=keymap.max_keycode().raw() {
            let code = Keycode::new(raw);
            let layouts = keymap.num_layouts_for_key(code);
            if layouts == 0 {
                continue;
            }
            let layout = active.min(layouts - 1);
            for level in 0..keymap.num_levels_for_key(code, layout).min(2) {
                let syms = keymap.key_get_syms_by_level(code, layout, level);
                if level == 0 && shift_key.is_none() && syms == [shift] {
                    shift_key = Some(code);
                }
                if syms.contains(&target) && best.is_none_or(|(_, l)| level < l) {
                    best = Some((code, level));
                }
            }
        }
        let (key, level) = best?;
        match level {
            0 => Some(KeySeq { shift: None, key }),
            _ => Some(KeySeq {
                shift: Some(shift_key?),
                key,
            }),
        }
    })
}

impl AbyssState {
    /// Absolute pointer motion, in global compositor coordinates.
    pub fn inject_pointer_absolute(&mut self, location: Point<f64, Logical>, time: u32) {
        if self.trusted_ui.active() {
            return;
        }
        super::idle::on_activity(self);
        self.pointer_moved(location, time);
    }

    /// Relative pointer motion. Both the relative-pointer protocol event and
    /// the absolute one are sent, in that order — the same pair a libinput
    /// motion event produces.
    pub fn inject_pointer_relative(&mut self, delta: Point<f64, Logical>, time: u32) {
        if self.trusted_ui.active() {
            return;
        }
        super::idle::on_activity(self);
        self.relative_motion(delta, delta, time as u64 * 1000);
        let location = self.pointer_location + delta;
        self.pointer_moved(location, time);
    }

    /// Pointer button press or release, by evdev button code.
    pub fn inject_pointer_button(&mut self, button: u32, pressed: bool, time: u32) {
        // A trusted prompt owns the seat; scripted input cannot answer or
        // steer around it (COMP-10 §3.10).
        if self.trusted_ui.active() {
            return;
        }
        super::idle::on_activity(self);
        // Injected buttons reach a protected surface only if their origin may
        // (COMP-19 §3), and never a commit slot's rectangle.
        if crate::protocols::protected::gate::button(self, button, pressed)
            != crate::protocols::protected::gate::Button::Pass
        {
            return;
        }
        let serial = SERIAL_COUNTER.next_serial();
        let pointer = self.seat.get_pointer().unwrap();
        // Click-to-focus. Skipped under a grab (an active drag or popup grab
        // owns the focus) and under the lock screen, which sees no client.
        if pressed && !pointer.is_grabbed() && !self.lock.locked {
            // The same entry point real input uses (ADR 0042): scripted input
            // must exercise the focus rules, not a copy of them, or a focus
            // bug passes WLCS and still ships.
            let pos = self.pointer_location;
            let action =
                crate::shell::focus::decide_pointer_focus(&crate::shell::focus::click_focus_ctx(self, pos));
            crate::shell::focus::apply_focus(self, action, crate::shell::focus::FocusCause::Click);
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
        // A trusted prompt owns the seat; scripted input cannot answer or
        // steer around it (COMP-10 §3.10).
        if self.trusted_ui.active() {
            return;
        }
        super::idle::on_activity(self);
        if crate::protocols::protected::gate::axis(self) {
            return;
        }
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
        // A trusted prompt owns the seat; scripted input cannot answer or
        // steer around it (COMP-10 §3.10).
        if self.trusted_ui.active() {
            return;
        }
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
            let under = self.surface_under(location);
            crate::protocols::protected::gate::filter_under(
                self,
                under,
                location,
                crate::protocols::protected::gate::Mode::Event,
            )
        };
        if !self.lock.locked && !touch.is_grabbed() {
            // As above: one focus path, shared with the pointer. The context is
            // built at the touch point, not at `pointer_location` — touch has
            // no cursor.
            let action = crate::shell::focus::decide_pointer_focus(&crate::shell::focus::click_focus_ctx(
                self, location,
            ));
            crate::shell::focus::apply_focus(self, action, crate::shell::focus::FocusCause::Touch);
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
        // A trusted prompt owns the seat; scripted input cannot answer or
        // steer around it (COMP-10 §3.10).
        if self.trusted_ui.active() {
            return;
        }
        super::idle::on_activity(self);
        let touch = match self.seat.get_touch() {
            Some(touch) => touch,
            None => return,
        };
        let focus = if self.lock.locked {
            None
        } else {
            let under = self.surface_under(location);
            crate::protocols::protected::gate::filter_under(
                self,
                under,
                location,
                crate::protocols::protected::gate::Mode::Event,
            )
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

    /// Scripted click (COMP-13 §2.2): move to `location`, then press and
    /// release `button` `count` times, through the same entry points a device
    /// uses so focus rules, grabs and gating all apply.
    pub fn inject_click(&mut self, location: Point<f64, Logical>, button: u32, count: u32) {
        let time = self.start_time.elapsed().as_millis() as u32;
        self.inject_pointer_absolute(location, time);
        for _ in 0..count {
            self.pointer_button(button, ButtonState::Pressed, time);
            self.pointer_button(button, ButtonState::Released, time);
        }
    }

    /// Scripted scroll: move to `location`, then one wheel frame of `dx`/`dy`
    /// detents (positive is down/right).
    pub fn inject_scroll(&mut self, location: Point<f64, Logical>, dx: f64, dy: f64) {
        use smithay::backend::input::{Axis, AxisSource};
        let time = self.start_time.elapsed().as_millis() as u32;
        self.inject_pointer_absolute(location, time);
        let mut frame = smithay::input::pointer::AxisFrame::new(time).source(AxisSource::Wheel);
        for (axis, d) in [(Axis::Horizontal, dx), (Axis::Vertical, dy)] {
            if d != 0.0 {
                frame = frame.value(axis, d * 15.0).v120(axis, (d * 120.0) as i32);
            }
        }
        self.inject_pointer_axis(frame);
    }

    /// Scripted text: each character as the key (and Shift) that types it in
    /// the active layout. Every character is resolved before any key is sent,
    /// so a string that cannot be typed types nothing. The text is never
    /// logged. Returns how many characters were typed.
    pub fn inject_text(&mut self, text: &str) -> Result<usize, &'static str> {
        let mut seqs = Vec::with_capacity(text.len());
        for c in text.chars() {
            let sym = match c {
                '\n' => 0xff0d,
                '\t' => 0xff09,
                c if c.is_control() => return Err("character cannot be typed"),
                c => Keysym::from_char(c).raw(),
            };
            seqs.push(find_key(self, sym).ok_or("character cannot be typed")?);
        }
        let n = seqs.len();
        for seq in seqs {
            self.send_seq(seq, &[]);
        }
        Ok(n)
    }

    /// Scripted key by keysym name (`Return`, `Escape`, `a`), with optional
    /// held modifiers (`ctrl`, `alt`, `shift`, `logo`).
    pub fn inject_key(&mut self, name: &str, mods: &[&str]) -> Result<(), &'static str> {
        let sym = smithay::input::keyboard::xkb::keysym_from_name(
            name,
            smithay::input::keyboard::xkb::KEYSYM_NO_FLAGS,
        )
        .raw();
        if sym == 0 {
            return Err("unknown key name");
        }
        let mut held = Vec::with_capacity(mods.len());
        for m in mods {
            let msym = match *m {
                "ctrl" => 0xffe3,
                "alt" => 0xffe9,
                "shift" => 0xffe1,
                "logo" | "super" => 0xffeb,
                _ => return Err("unknown modifier"),
            };
            held.push(find_key(self, msym).ok_or("modifier has no key")?.key);
        }
        let seq = find_key(self, sym).ok_or("key is not on the active layout")?;
        self.send_seq(seq, &held);
        Ok(())
    }

    fn send_seq(&mut self, seq: KeySeq, held: &[Keycode]) {
        use smithay::backend::input::KeyState;
        let time = self.start_time.elapsed().as_millis() as u32;
        super::idle::on_activity(self);
        for code in held.iter().copied().chain(seq.shift) {
            self.keyboard_key(code, KeyState::Pressed, time);
        }
        self.keyboard_key(seq.key, KeyState::Pressed, time);
        self.keyboard_key(seq.key, KeyState::Released, time);
        for code in seq.shift.into_iter().chain(held.iter().rev().copied()) {
            self.keyboard_key(code, KeyState::Released, time);
        }
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
