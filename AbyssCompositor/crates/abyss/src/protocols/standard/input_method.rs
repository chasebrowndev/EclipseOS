// SPDX-License-Identifier: AGPL-3.0-only
//! `zwp_input_method_v2` (COMP-06 §1): the IME side of text input.
//!
//! Abyss-owned rather than delegated to smithay, for the reason spelled out in
//! [`super::text_input`]: an input method that connects after a text field has
//! already enabled itself must still be told about that field.
//!
//! The compositor owns the input-method popup: it is positioned under the text
//! cursor rectangle the client reported and drawn above everything the shell
//! draws. IME content (preedit, commit strings) is human input — forwarded,
//! never logged.

use smithay::{
    input::keyboard::{
        GrabStartData as KeyboardGrabStartData, KeyboardGrab, KeyboardHandle, KeyboardInnerHandle,
        KeymapFile, ModifiersState,
    },
    reexports::{
        wayland_protocols_misc::zwp_input_method_v2::server::{
            zwp_input_method_keyboard_grab_v2::{self, ZwpInputMethodKeyboardGrabV2},
            zwp_input_method_manager_v2::{self, ZwpInputMethodManagerV2},
            zwp_input_method_v2::{self, ZwpInputMethodV2},
            zwp_input_popup_surface_v2::{self, ZwpInputPopupSurfaceV2},
        },
        wayland_server::{
            backend::ClientId,
            protocol::{wl_keyboard::KeymapFormat, wl_surface::WlSurface},
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
        },
    },
    utils::{Logical, Point, Rectangle, Serial, SERIAL_COUNTER},
    wayland::compositor,
};
use tracing::warn;

use crate::state::AbyssState;

/// Role given to a surface turned into an input-method popup.
pub const INPUT_POPUP_SURFACE_ROLE: &str = "zwp_input_popup_surface_v2";

/// The one input method bound to the seat.
#[derive(Debug)]
pub struct InputMethodInstance {
    pub(super) object: ZwpInputMethodV2,
    /// Number of `done` events sent; the client echoes it back on `commit`.
    serial: u32,
}

impl InputMethodInstance {
    pub(super) fn done(&mut self) {
        self.serial = self.serial.wrapping_add(1);
        self.object.done();
    }
}

/// Where the popup hangs off the focused surface.
#[derive(Debug, Clone)]
pub struct PopupParent {
    pub surface: WlSurface,
    pub location: Rectangle<i32, Logical>,
}

/// An `zwp_input_popup_surface_v2`.
#[derive(Debug)]
pub struct ImePopup {
    role: ZwpInputPopupSurfaceV2,
    surface: WlSurface,
    parent: Option<PopupParent>,
    rectangle: Rectangle<i32, Logical>,
}

impl ImePopup {
    pub fn alive(&self) -> bool {
        self.role.is_alive() && self.surface.alive()
    }

    pub fn wl_surface(&self) -> &WlSurface {
        &self.surface
    }
}

/// Top-left of the popup in global logical coordinates, or `None` if it is
/// gone or has no parent yet.
pub fn popup_location(popup: &ImePopup) -> Option<Point<i32, Logical>> {
    if !popup.alive() {
        return None;
    }
    let parent = popup.parent.as_ref()?;
    let rect = popup.rectangle;
    let offset: Point<i32, Logical> = (rect.loc.x, rect.loc.y + rect.size.h).into();
    Some(parent.location.loc + offset)
}

impl AbyssState {
    /// Geometry of the surface the IME is attached to, in global coordinates.
    fn parent_geometry(&self, parent: &WlSurface) -> Rectangle<i32, Logical> {
        self.space
            .elements()
            .find(|w| w.toplevel().map(|t| t.wl_surface() == parent).unwrap_or(false))
            .and_then(|w| self.space.element_geometry(w))
            .unwrap_or_default()
    }

    pub(super) fn ime_popup_set_parent(&mut self, parent: Option<WlSurface>) {
        let parent = parent.map(|surface| {
            let location = self.parent_geometry(&surface);
            PopupParent { surface, location }
        });
        if let Some(popup) = self.ime.popup.as_mut() {
            popup.parent = parent;
        }
    }

    pub(super) fn ime_set_cursor_rectangle(&mut self, rectangle: Rectangle<i32, Logical>) {
        self.ime.cursor_rectangle = rectangle;
        if let Some(popup) = self.ime.popup.as_mut() {
            popup.rectangle = rectangle;
            popup.role.text_input_rectangle(
                rectangle.loc.x,
                rectangle.loc.y,
                rectangle.size.w,
                rectangle.size.h,
            );
        }
    }
}

/// Global for `zwp_input_method_manager_v2`.
#[derive(Debug)]
pub struct InputMethodManagerState;

impl InputMethodManagerState {
    pub fn new(display: &DisplayHandle) -> Self {
        display.create_global::<AbyssState, ZwpInputMethodManagerV2, ()>(1, ());
        Self
    }
}

impl GlobalDispatch<ZwpInputMethodManagerV2, ()> for AbyssState {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZwpInputMethodManagerV2>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwpInputMethodManagerV2, ()> for AbyssState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &ZwpInputMethodManagerV2,
        request: zwp_input_method_manager_v2::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwp_input_method_manager_v2::Request::GetInputMethod { seat: _, input_method } => {
                let object = data_init.init(input_method, ());
                if state.ime.im.is_some() {
                    // Only one input method per seat.
                    object.unavailable();
                    return;
                }
                state.ime.im = Some(InputMethodInstance { object, serial: 0 });
                // Whatever the focused text field committed before the input
                // method existed is replayed now.
                state.ime_flush();
            }
            zwp_input_method_manager_v2::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl Dispatch<ZwpInputMethodV2, ()> for AbyssState {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZwpInputMethodV2,
        request: zwp_input_method_v2::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let is_current = state.ime.im.as_ref().map(|im| &im.object) == Some(resource);
        if !is_current {
            return;
        }

        match request {
            zwp_input_method_v2::Request::CommitString { text } => {
                if let Some((ti, _, _)) = state.ime.active_text_input() {
                    ti.commit_string(Some(text));
                }
            }
            zwp_input_method_v2::Request::SetPreeditString {
                text,
                cursor_begin,
                cursor_end,
            } => {
                if let Some((ti, _, _)) = state.ime.active_text_input() {
                    ti.preedit_string(Some(text), cursor_begin, cursor_end);
                }
            }
            zwp_input_method_v2::Request::DeleteSurroundingText {
                before_length,
                after_length,
            } => {
                if let Some((ti, _, _)) = state.ime.active_text_input() {
                    ti.delete_surrounding_text(before_length, after_length);
                }
            }
            zwp_input_method_v2::Request::Commit { serial } => {
                let current = state.ime.im.as_ref().map(|im| im.serial).unwrap_or(0);
                if let Some((ti, _, ti_serial)) = state.ime.active_text_input() {
                    // A stale serial means the state the IME worked from is
                    // gone: discard it by answering with a serial that cannot
                    // match.
                    ti.done(if serial == current { ti_serial } else { 0 });
                }
            }
            zwp_input_method_v2::Request::GetInputPopupSurface { id, surface } => {
                if compositor::give_role(&surface, INPUT_POPUP_SURFACE_ROLE).is_err()
                    && compositor::get_role(&surface) != Some(INPUT_POPUP_SURFACE_ROLE)
                {
                    // The protocol requires an error here but defines no enum.
                    resource.post_error(0u32, "Surface already has a role.");
                    return;
                }
                let role = data_init.init(id, ());
                let parent = state.ime.focus().cloned().map(|surface| {
                    let location = state.parent_geometry(&surface);
                    PopupParent { surface, location }
                });
                state.ime.popup = Some(ImePopup {
                    role,
                    surface,
                    parent,
                    rectangle: state.ime.cursor_rectangle,
                });
            }
            zwp_input_method_v2::Request::GrabKeyboard { keyboard } => {
                let grab = data_init.init(keyboard, ());
                let Some(kbd) = state.seat.get_keyboard() else {
                    return;
                };
                send_grab_keymap(state, &kbd, &grab);
                state.ime.grab = Some(grab.clone());
                kbd.set_grab(
                    state,
                    ImeKeyboardGrab {
                        grab,
                        start_data: KeyboardGrabStartData { focus: None },
                    },
                    SERIAL_COUNTER.next_serial(),
                );
            }
            zwp_input_method_v2::Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, object: &ZwpInputMethodV2, _data: &()) {
        if state.ime.im.as_ref().map(|im| &im.object) != Some(object) {
            return;
        }
        state.ime.im = None;
        // The input method is gone; there is nobody left to deactivate.
        state.ime.activated = false;
        state.ime.needs_activate = state.ime.active.is_some();
    }
}

/// Send the current keymap, repeat info and modifiers to a fresh grab.
fn send_grab_keymap(
    state: &mut AbyssState,
    keyboard: &KeyboardHandle<AbyssState>,
    grab: &ZwpInputMethodKeyboardGrabV2,
) {
    let mods = keyboard.modifier_state().serialized;
    let res = keyboard.with_xkb_state(state, |ctx| {
        // SAFETY: the keymap reference does not outlive this closure.
        let keymap_file = KeymapFile::new(unsafe { ctx.xkb().lock().unwrap().keymap() });
        keymap_file.with_fd(false, |fd, size| {
            grab.keymap(KeymapFormat::XkbV1, fd, size as u32);
        })
    });
    if let Err(err) = res {
        warn!(?err, "failed to send keymap to the input method");
        return;
    }
    grab.modifiers(
        SERIAL_COUNTER.next_serial().0,
        mods.depressed,
        mods.latched,
        mods.locked,
        mods.layout_effective,
    );
}

/// Keyboard grab held by the input method: keys go to the IME, not the app.
#[derive(Debug)]
struct ImeKeyboardGrab {
    grab: ZwpInputMethodKeyboardGrabV2,
    start_data: KeyboardGrabStartData<AbyssState>,
}

impl KeyboardGrab<AbyssState> for ImeKeyboardGrab {
    fn input(
        &mut self,
        data: &mut AbyssState,
        _handle: &mut KeyboardInnerHandle<'_, AbyssState>,
        keycode: smithay::backend::input::Keycode,
        key_state: smithay::backend::input::KeyState,
        modifiers: Option<ModifiersState>,
        serial: Serial,
        time: u32,
    ) {
        let serial = data.ime.active_serial_or(serial.0);
        self.grab.key(serial, time, keycode.raw() - 8, key_state.into());
        if let Some(serialized) = modifiers.map(|m| m.serialized) {
            self.grab.modifiers(
                serial,
                serialized.depressed,
                serialized.latched,
                serialized.locked,
                serialized.layout_effective,
            );
        }
    }

    fn set_focus(
        &mut self,
        data: &mut AbyssState,
        handle: &mut KeyboardInnerHandle<'_, AbyssState>,
        focus: Option<WlSurface>,
        serial: Serial,
    ) {
        handle.set_focus(data, focus, serial)
    }

    fn start_data(&self) -> &KeyboardGrabStartData<AbyssState> {
        &self.start_data
    }

    fn unset(&mut self, _data: &mut AbyssState) {}
}

impl Dispatch<ZwpInputMethodKeyboardGrabV2, ()> for AbyssState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ZwpInputMethodKeyboardGrabV2,
        request: zwp_input_method_keyboard_grab_v2::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwp_input_method_keyboard_grab_v2::Request::Release => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        object: &ZwpInputMethodKeyboardGrabV2,
        _data: &(),
    ) {
        if state.ime.grab.as_ref() != Some(object) {
            return;
        }
        state.ime.grab = None;
        if let Some(keyboard) = state.seat.get_keyboard() {
            keyboard.unset_grab(state);
        }
    }
}

impl Dispatch<ZwpInputPopupSurfaceV2, ()> for AbyssState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ZwpInputPopupSurfaceV2,
        request: zwp_input_popup_surface_v2::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwp_input_popup_surface_v2::Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, object: &ZwpInputPopupSurfaceV2, _data: &()) {
        if state.ime.popup.as_ref().map(|p| &p.role) == Some(object) {
            state.ime.popup = None;
        }
    }
}
