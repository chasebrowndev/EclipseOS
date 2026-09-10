// SPDX-License-Identifier: AGPL-3.0-only
//! `xdg_activation_v1` (COMP-06 §1, gated per COMP-05 §5).
//!
//! Activation is a focus change requested by one client on behalf of another, so
//! it is exactly the kind of ambient authority this compositor does not grant.
//! COMP-05 §5: honoured only when the requesting principal may focus the target;
//! otherwise the window is marked urgent and the human decides.
//!
//! In Phase 1 the only principal is the human seat, so "may focus the target"
//! reduces to: the token was minted by the surface that currently holds human
//! focus, from a real input serial, recently. A token minted by a background
//! client, or replayed later, cannot steal focus.

use std::time::Duration;

use smithay::{
    delegate_xdg_activation,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    wayland::xdg_activation::{
        XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
    },
};

use crate::state::AbyssState;

/// A token older than this is stale regardless of who minted it: the human has
/// had time to move on, and honouring it would be a focus steal after the fact.
const TOKEN_LIFETIME: Duration = Duration::from_secs(5);

impl AbyssState {
    /// COMP-05 §5: may the principal behind this token focus anything right now?
    fn activation_permitted(&self, data: &XdgActivationTokenData) -> bool {
        if data.timestamp.elapsed() > TOKEN_LIFETIME {
            return false;
        }
        // A token with no input serial was not minted from a human action.
        if data.serial.is_none() {
            return false;
        }
        let Some(requester) = data.surface.as_ref() else {
            return false;
        };
        let focused = self.focus.as_ref().and_then(crate::shell::window_surface);
        focused.as_ref() == Some(requester)
    }
}

impl XdgActivationHandler for AbyssState {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.activation_state
    }

    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        let permitted = self.activation_permitted(&token_data);
        self.activation_state.remove_token(&token);

        let Some(window) = crate::shell::window_for_surface(self, &surface) else {
            return;
        };
        if permitted {
            crate::shell::focus_window(self, &window);
        } else {
            tracing::debug!("activation refused; marking urgent");
            crate::shell::mark_urgent(self, &window);
        }
    }
}

delegate_xdg_activation!(AbyssState);
