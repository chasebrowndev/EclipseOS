// SPDX-License-Identifier: AGPL-3.0-only
//! `wp_fractional_scale_v1` and `wp_viewporter` (COMP-06 §2).
//!
//! Clients that speak fractional scale are told the exact scale of the output
//! they are on, so they can render at 1.5x rather than at 2x and be downscaled.
//! `wp_viewporter` is its required companion: a client rendering at a
//! fractional scale needs the viewport to describe the resulting non-integer
//! surface size.

use smithay::{
    delegate_fractional_scale, delegate_viewporter,
    desktop::Window,
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    wayland::{
        compositor::{send_surface_state, with_states},
        fractional_scale::{with_fractional_scale, FractionalScaleHandler},
    },
};

use crate::state::HeliosState;

impl FractionalScaleHandler for HeliosState {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        // The surface has no buffer yet, so it is on no output; answer with the
        // focused output's scale and correct it on the first commit.
        if let Some(output) = self.outputs.focused().map(|e| e.output.clone()) {
            send_scale(&surface, &output);
        }
    }
}

/// Tell a surface the scale and transform of the output it lives on.
pub fn send_scale(surface: &WlSurface, output: &Output) {
    let scale = output.current_scale();
    with_states(surface, |states| {
        with_fractional_scale(states, |fractional| {
            fractional.set_preferred_scale(scale.fractional_scale());
        });
        send_surface_state(surface, states, scale.integer_scale(), output.current_transform());
    });
}

/// Push the current scale of `output` to every surface of `window`.
pub fn update_window_scale(window: &Window, output: &Output) {
    if let Some(surface) = window.toplevel().map(|t| t.wl_surface().clone()) {
        send_scale(&surface, output);
    }
}

delegate_fractional_scale!(HeliosState);
delegate_viewporter!(HeliosState);
