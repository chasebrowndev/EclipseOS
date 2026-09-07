// SPDX-License-Identifier: AGPL-3.0-only
//! XWayland: rootless X11 support (COMP-07).
//!
//! Lifecycle only; the window manager itself lives in [`xwm`] and the trust
//! boundary in [`security`]. One `X11Wm` at a time, owned by `HeliosState`
//! like every other child — no shared handles, no locks.

pub mod security;
pub mod xwm;

use smithay::{
    reexports::wayland_server::Client,
    wayland::xwayland_shell::{XWaylandShellHandler, XWaylandShellState},
    xwayland::{xwm::XwmId, X11Surface, X11Wm, XWayland, XWaylandEvent},
};

use crate::state::HeliosState;

/// Everything the compositor owns on behalf of the X server.
#[derive(Default)]
pub struct XWaylandState {
    /// The window manager, once XWayland has signalled ready.
    pub wm: Option<X11Wm>,
    /// The Wayland client XWayland itself connects back as.
    pub client: Option<Client>,
    /// `:N`, once known.
    pub display: Option<u32>,
    /// Override-redirect surfaces: menus, tooltips, drag icons. Mapped for
    /// the human to see, excluded from the managed window set and (later)
    /// from the agent scene (COMP-07 §1).
    pub unmanaged: Vec<X11Surface>,
    /// Client-side scale currently advertised through XSETTINGS.
    pub advertised_scale: Option<i32>,
}

/// Spawn the X server and arm the event source (COMP-07 §1).
///
/// A `false` return is not fatal: helios runs fine with no X11 domain at all,
/// which is exactly the isolation posture `xwayland { enable #false }` buys.
pub fn start(state: &mut HeliosState) -> bool {
    if !state.config.xwayland.enable {
        tracing::info!("xwayland disabled by config; no X11 trust domain will exist");
        return false;
    }

    let res = XWayland::spawn(
        &state.display_handle,
        None,
        std::iter::empty::<(String, String)>(),
        true,
        std::process::Stdio::null(),
        std::process::Stdio::null(),
        |_| {},
    );
    let (xwayland, client) = match res {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(%e, "could not spawn Xwayland; continuing without X11 support");
            return false;
        }
    };

    let inserted = state
        .loop_handle
        .insert_source(xwayland, move |event, _, state| match event {
            XWaylandEvent::Ready {
                x11_socket,
                display_number,
            } => ready(state, x11_socket, display_number),
            XWaylandEvent::Error => {
                tracing::error!("Xwayland failed to start; X11 clients will not run");
                state.xwayland.client = None;
            }
        });
    if let Err(e) = inserted {
        tracing::warn!(%e, "could not register the Xwayland event source");
        return false;
    }

    state.xwayland.client = Some(client);
    true
}

/// XWayland is up: start the window manager and publish `DISPLAY`.
fn ready(state: &mut HeliosState, socket: std::os::unix::net::UnixStream, dpy: u32) {
    let Some(client) = state.xwayland.client.clone() else {
        tracing::error!("xwayland ready without a client; ignoring");
        return;
    };
    let wm = match X11Wm::start_wm::<HeliosState>(state.loop_handle.clone(), socket, client) {
        Ok(wm) => wm,
        Err(e) => {
            tracing::error!(error = %e, "could not start the X11 window manager");
            return;
        }
    };
    state.xwayland.wm = Some(wm);
    state.xwayland.display = Some(dpy);

    // Children spawned from binds must find the X server. Safe here: the
    // compositor core is single-threaded and no thread has been started yet
    // that reads the environment concurrently.
    std::env::set_var("DISPLAY", format!(":{dpy}"));

    apply_scale(state);
    tracing::info!(x11_display = format!(":{dpy}"), "xwayland ready");
}

/// Cursor scale / DPI hints for X11 clients (COMP-07 §4).
///
/// Default is compositor scaling: clients are told the world is 96 dpi at
/// scale 1 and the compositor upscales the result — blurry but always
/// correct. `xwayland { scaling "client" }` instead hands out the focused
/// output's scale and lets the client render natively.
pub fn apply_scale(state: &mut HeliosState) {
    use smithay::xwayland::xwm::settings::Value;

    let scale = if state.config.xwayland.scaling_client {
        crate::shell::focused_output(state)
            .map(|o| o.current_scale().fractional_scale())
            .unwrap_or(1.0)
    } else {
        1.0
    };
    let dpi = (96.0 * scale).round() as i32;
    if state.xwayland.advertised_scale == Some(dpi) {
        return;
    }
    let Some(wm) = state.xwayland.wm.as_mut() else {
        return;
    };
    let settings = [
        // XSETTINGS carries Xft.dpi multiplied by 1024.
        ("Xft/DPI".to_string(), Value::Integer(dpi * 1024)),
        (
            "Gdk/WindowScalingFactor".to_string(),
            Value::Integer(scale.round().max(1.0) as i32),
        ),
        ("Gdk/UnscaledDPI".to_string(), Value::Integer(96 * 1024)),
    ];
    if let Err(e) = wm.set_xsettings(settings.into_iter()) {
        tracing::warn!(error = %e, "could not publish XSETTINGS");
        return;
    }
    state.xwayland.advertised_scale = Some(dpi);
    tracing::debug!(dpi, scale, "xwayland scaling updated");
}

impl XWaylandShellHandler for HeliosState {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell_state
    }

    fn surface_associated(
        &mut self,
        _xwm: XwmId,
        _wl_surface: smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        surface: X11Surface,
    ) {
        tracing::debug!(
            class = surface.class(),
            "x11 surface associated with a wl_surface"
        );
    }
}

smithay::delegate_xwayland_shell!(HeliosState);
