// SPDX-License-Identifier: AGPL-3.0-only
//! `zwp_text_input_v3` (COMP-06 §1) and the state shared with
//! `zwp_input_method_v2`.
//!
//! This is abyss' own implementation rather than smithay's: smithay 0.7.0
//! drops every text-input request that arrives before an input-method client
//! has bound the seat and never replays it, so a text field that is enabled
//! before the IME connects stays dead forever. We keep the committed state and
//! hand it to the input method when it shows up.
//!
//! Surrounding text, preedit and commit strings are human input: they are
//! forwarded, never logged, and redacted out of `Debug`.

use std::fmt;

use smithay::{
    reexports::{
        wayland_protocols::wp::text_input::zv3::server::{
            zwp_text_input_manager_v3::{self, ZwpTextInputManagerV3},
            zwp_text_input_v3::{self, ChangeCause, ContentHint, ContentPurpose, ZwpTextInputV3},
        },
        wayland_server::{
            backend::{ClientId, ObjectId},
            protocol::wl_surface::WlSurface,
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
        },
    },
    utils::{Logical, Rectangle},
};

use crate::state::AbyssState;

/// State a text input accumulates between `commit` requests.
#[derive(Default, Clone)]
pub struct TextFieldState {
    enable: Option<bool>,
    surrounding_text: Option<(String, u32, u32)>,
    content_type: Option<(ContentHint, ContentPurpose)>,
    cursor_rectangle: Option<Rectangle<i32, Logical>>,
    text_change_cause: Option<ChangeCause>,
}

impl fmt::Debug for TextFieldState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never render the surrounding text itself: it is human input.
        f.debug_struct("TextFieldState")
            .field("enable", &self.enable)
            .field("has_surrounding_text", &self.surrounding_text.is_some())
            .field("content_type", &self.content_type)
            .field("cursor_rectangle", &self.cursor_rectangle)
            .field("text_change_cause", &self.text_change_cause)
            .finish()
    }
}

impl TextFieldState {
    fn merge(&mut self, other: TextFieldState) {
        if other.enable.is_some() {
            self.enable = other.enable;
        }
        if other.surrounding_text.is_some() {
            self.surrounding_text = other.surrounding_text;
        }
        if other.content_type.is_some() {
            self.content_type = other.content_type;
        }
        if other.cursor_rectangle.is_some() {
            self.cursor_rectangle = other.cursor_rectangle;
        }
        if other.text_change_cause.is_some() {
            self.text_change_cause = other.text_change_cause;
        }
    }

    fn is_empty(&self) -> bool {
        self.surrounding_text.is_none()
            && self.content_type.is_none()
            && self.cursor_rectangle.is_none()
            && self.text_change_cause.is_none()
    }
}

#[derive(Debug)]
struct TextInputInstance {
    object: ZwpTextInputV3,
    /// Number of `commit` requests seen, echoed back in `done`.
    serial: u32,
    pending: TextFieldState,
}

/// The seat-wide text-input / input-method bookkeeping.
///
/// Plain owned state on [`AbyssState`] — no interior mutability, no handle
/// graph.
#[derive(Debug, Default)]
pub struct ImeState {
    instances: Vec<TextInputInstance>,
    /// Surface holding keyboard focus, mirrored here so `enter`/`leave` can be
    /// emitted independently of whether an input method exists.
    focus: Option<WlSurface>,
    /// Text input that most recently committed `enable`.
    active: Option<ObjectId>,
    /// Committed field state not yet handed to an input method.
    pending_to_im: TextFieldState,
    /// An `activate` is owed to the input method.
    needs_activate: bool,
    /// The input method has been activated and not yet deactivated.
    activated: bool,
    pub(super) im: Option<super::input_method::InputMethodInstance>,
    pub(super) popup: Option<super::input_method::ImePopup>,
    pub(super) cursor_rectangle: Rectangle<i32, Logical>,
}

impl ImeState {
    fn instance_mut(&mut self, object: &ZwpTextInputV3) -> Option<&mut TextInputInstance> {
        self.instances.iter_mut().find(|i| &i.object == object)
    }

    /// The active text input for the focused client, if any.
    pub(super) fn active_text_input(&self) -> Option<(&ZwpTextInputV3, &WlSurface, u32)> {
        let active = self.active.as_ref()?;
        let surface = self.focus.as_ref().filter(|s| s.is_alive())?;
        let instance = self
            .instances
            .iter()
            .filter(|i| i.object.id().same_client_as(&surface.id()))
            .find(|i| &i.object.id() == active)?;
        Some((&instance.object, surface, instance.serial))
    }

    /// Serial of the active text input, or `default` when there is none.
    pub(super) fn active_serial_or(&self, default: u32) -> u32 {
        self.active_text_input().map(|(_, _, serial)| serial).unwrap_or(default)
    }

    pub(super) fn focus(&self) -> Option<&WlSurface> {
        self.focus.as_ref()
    }
}

impl AbyssState {
    /// Send `enter`/`leave` to follow keyboard focus.
    ///
    /// Unlike smithay this happens whether or not an input method is running:
    /// `zwp_text_input_v3.enter` is about focus, not about IME availability.
    pub fn ime_set_focus(&mut self, surface: Option<WlSurface>) {
        if self.ime.focus == surface {
            return;
        }

        if let Some(old) = self.ime.focus.take() {
            if old.is_alive() {
                for instance in self
                    .ime
                    .instances
                    .iter()
                    .filter(|i| i.object.id().same_client_as(&old.id()))
                {
                    instance.object.leave(&old);
                }
            }
            self.ime_deactivate();
        }

        self.ime.focus = surface;
        if let Some(new) = self.ime.focus.clone() {
            for instance in self
                .ime
                .instances
                .iter()
                .filter(|i| i.object.id().same_client_as(&new.id()))
            {
                instance.object.enter(&new);
            }
        }
    }

    /// Drop the active text input and tell the input method, if we told it to
    /// activate in the first place.
    pub(super) fn ime_deactivate(&mut self) {
        self.ime.active = None;
        self.ime.needs_activate = false;
        self.ime.pending_to_im = TextFieldState::default();
        if !self.ime.activated {
            return;
        }
        self.ime.activated = false;
        if let Some(im) = self.ime.im.as_mut() {
            im.object.deactivate();
            im.done();
        }
        self.ime_popup_set_parent(None);
    }

    /// Hand whatever the text field committed to the input method. A no-op
    /// while no input method is bound — the state is replayed once one is.
    pub(super) fn ime_flush(&mut self) {
        if self.ime.im.is_none() || self.ime.active.is_none() {
            return;
        }

        let activate = self.ime.needs_activate;
        let state = std::mem::take(&mut self.ime.pending_to_im);
        if !activate && state.is_empty() {
            return;
        }
        self.ime.needs_activate = false;

        if activate {
            self.ime.activated = true;
            if let Some(im) = self.ime.im.as_ref() {
                im.object.activate();
            }
            let parent = self.ime.focus.clone();
            self.ime_popup_set_parent(parent);
        }

        if let Some(im) = self.ime.im.as_ref() {
            if let Some((text, cursor, anchor)) = state.surrounding_text {
                im.object.surrounding_text(text, cursor, anchor);
            }
            if let Some(cause) = state.text_change_cause {
                im.object.text_change_cause(cause);
            }
            if let Some((hint, purpose)) = state.content_type {
                im.object.content_type(hint, purpose);
            }
        }

        if let Some(rect) = state.cursor_rectangle {
            self.ime_set_cursor_rectangle(rect);
        }

        if let Some(im) = self.ime.im.as_mut() {
            im.done();
        }
    }
}

/// Global for `zwp_text_input_manager_v3`.
#[derive(Debug)]
pub struct TextInputManagerState;

impl TextInputManagerState {
    pub fn new(display: &DisplayHandle) -> Self {
        display.create_global::<AbyssState, ZwpTextInputManagerV3, ()>(1, ());
        Self
    }
}

impl GlobalDispatch<ZwpTextInputManagerV3, ()> for AbyssState {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZwpTextInputManagerV3>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwpTextInputManagerV3, ()> for AbyssState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &ZwpTextInputManagerV3,
        request: zwp_text_input_manager_v3::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwp_text_input_manager_v3::Request::GetTextInput { id, seat: _ } => {
                let object = data_init.init(id, ());
                // A text input created while its client already holds focus
                // has to be told about that focus straight away.
                if let Some(focus) = state.ime.focus.clone() {
                    if focus.is_alive() && object.id().same_client_as(&focus.id()) {
                        object.enter(&focus);
                    }
                }
                state.ime.instances.push(TextInputInstance {
                    object,
                    serial: 0,
                    pending: TextFieldState::default(),
                });
            }
            zwp_text_input_manager_v3::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl Dispatch<ZwpTextInputV3, ()> for AbyssState {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZwpTextInputV3,
        request: zwp_text_input_v3::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // The serial is echoed back in `done` and must track the client's
        // commits even for requests we go on to discard.
        if matches!(request, zwp_text_input_v3::Request::Commit) {
            if let Some(instance) = state.ime.instance_mut(resource) {
                instance.serial = instance.serial.wrapping_add(1);
            }
        }

        match state.ime.focus.as_ref() {
            Some(focus) if focus.is_alive() && focus.id().same_client_as(&resource.id()) => {}
            // Requests from an unfocused client are not an error, just noise.
            _ => return,
        }

        let Some(instance) = state.ime.instance_mut(resource) else {
            return;
        };

        match request {
            zwp_text_input_v3::Request::Enable => instance.pending.enable = Some(true),
            zwp_text_input_v3::Request::Disable => instance.pending.enable = Some(false),
            zwp_text_input_v3::Request::SetSurroundingText { text, cursor, anchor } => {
                instance.pending.surrounding_text = Some((text, cursor as u32, anchor as u32));
            }
            zwp_text_input_v3::Request::SetTextChangeCause { cause } => {
                instance.pending.text_change_cause = cause.into_result().ok();
            }
            zwp_text_input_v3::Request::SetContentType { hint, purpose } => {
                if let (Ok(hint), Ok(purpose)) = (hint.into_result(), purpose.into_result()) {
                    instance.pending.content_type = Some((hint, purpose));
                }
            }
            zwp_text_input_v3::Request::SetCursorRectangle { x, y, width, height } => {
                instance.pending.cursor_rectangle =
                    Some(Rectangle::new((x, y).into(), (width, height).into()));
            }
            zwp_text_input_v3::Request::Commit => {
                let new_state = std::mem::take(&mut instance.pending);
                let id = resource.id();

                if state.ime.active.is_some() && state.ime.active.as_ref() != Some(&id) {
                    // Another text input of this client owns the IME.
                    return;
                }

                match new_state.enable {
                    Some(true) => {
                        state.ime.active = Some(id);
                        if !state.ime.activated {
                            state.ime.needs_activate = true;
                        }
                    }
                    Some(false) => {
                        state.ime_deactivate();
                        return;
                    }
                    None if state.ime.active.as_ref() != Some(&id) => return,
                    None => {}
                }

                state.ime.pending_to_im.merge(new_state);
                state.ime_flush();
            }
            zwp_text_input_v3::Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, object: &ZwpTextInputV3, _data: &()) {
        let id = object.id();
        state.ime.instances.retain(|i| i.object.id() != id);
        if state.ime.active.as_ref() == Some(&id) {
            state.ime_deactivate();
        }
    }
}
