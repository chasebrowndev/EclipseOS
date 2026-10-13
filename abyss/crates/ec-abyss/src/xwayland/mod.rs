// SPDX-License-Identifier: AGPL-3.0-only
//! XWayland: rootless X11 support (COMP-07).
//!
//! Lifecycle only; the window manager itself lives in [`xwm`] and the trust
//! boundary in [`security`]. One `X11Wm` at a time, owned by `AbyssState`
//! like every other child — no shared handles, no locks.

pub mod security;
pub mod xwm;

use smithay::{
    reexports::wayland_server::Client,
    utils::{Logical, Rectangle},
    wayland::xwayland_shell::{XWaylandShellHandler, XWaylandShellState},
    xwayland::{xwm::XwmId, X11Surface, X11Wm, XWayland, XWaylandEvent},
};

use crate::state::AbyssState;

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
    /// A managed window being mapped whose fullscreen/maximized state is
    /// already known: placement must not configure it (the first configure the
    /// client sees has to be the final rect). Set only across `place_new_window`.
    pub mapping: Option<smithay::desktop::Window>,
    /// Override-redirect windows whose geometry is still the creation-time
    /// one (no `ConfigureNotify` yet); see [`creation_geometry`].
    pub unrefreshed: Vec<X11Surface>,
    /// Managed windows that asked for fullscreen (or maximize) before they
    /// were mapped. There is no `Window` to act on yet, so the request is
    /// parked here and applied at map, the way `map_toplevel` replays the xdg
    /// pending state.
    pub wants_fullscreen: Vec<X11Surface>,
    pub wants_maximized: Vec<X11Surface>,
    /// Client-side scale currently advertised through XSETTINGS.
    pub advertised_scale: Option<i32>,
    /// The `client_scale` handed to smithay for the XWayland client: X pixels
    /// per logical pixel. `None` (read as 1.0) in compositor mode. Set once
    /// when the X server comes up; reload semantics are `restart`, so live
    /// windows never see it change under them.
    pub client_scale: Option<f64>,
}

/// X pixels per logical pixel for the X11 domain; 1.0 unless `scaling "client"`.
pub fn client_scale(state: &AbyssState) -> f64 {
    state.xwayland.client_scale.unwrap_or(1.0)
}

/// A rectangle in X space (physical pixels at `scale`) as abyss logical space.
///
/// smithay's `X11Surface` already converts at every boundary it owns
/// (`configure`, `configure_request`, `configure_notify`, `geometry`, size
/// hints), so abyss works in logical space throughout. The one place it does
/// not is a window's geometry at creation, which smithay divides by the
/// *truncated* scale; [`creation_geometry`] undoes that.
pub fn from_x11(rect: Rectangle<i32, Logical>, scale: f64) -> Rectangle<i32, Logical> {
    if scale == 1.0 {
        return rect;
    }
    Rectangle::new(
        (
            (rect.loc.x as f64 / scale).round() as i32,
            (rect.loc.y as f64 / scale).round() as i32,
        )
            .into(),
        (
            ((rect.size.w as f64 / scale).round() as i32).max(1),
            ((rect.size.h as f64 / scale).round() as i32).max(1),
        )
            .into(),
    )
}

/// The inverse of [`from_x11`].
#[cfg(test)]
pub fn to_x11(rect: Rectangle<i32, Logical>, scale: f64) -> Rectangle<i32, Logical> {
    if scale == 1.0 {
        return rect;
    }
    Rectangle::new(
        (
            (rect.loc.x as f64 * scale).round() as i32,
            (rect.loc.y as f64 * scale).round() as i32,
        )
            .into(),
        (
            ((rect.size.w as f64 * scale).round() as i32).max(1),
            ((rect.size.h as f64 * scale).round() as i32).max(1),
        )
            .into(),
    )
}

/// A window's geometry as smithay stored it at creation, before any
/// `ConfigureNotify` refreshed it: X pixels divided by `scale as i32`
/// (smithay `xwm/mod.rs:1455`), so only correct when `scale` is integral.
/// Redo the division with the real scale.
pub fn creation_geometry(raw: Rectangle<i32, Logical>, scale: f64) -> Rectangle<i32, Logical> {
    let truncated = (scale as i32).max(1) as f64;
    if scale == truncated {
        return raw;
    }
    let x = Rectangle::new(
        (
            (raw.loc.x as f64 * truncated).round() as i32,
            (raw.loc.y as f64 * truncated).round() as i32,
        )
            .into(),
        (
            (raw.size.w as f64 * truncated).round() as i32,
            (raw.size.h as f64 * truncated).round() as i32,
        )
            .into(),
    );
    from_x11(x, scale)
}

/// Spawn the X server and arm the event source (COMP-07 §1).
///
/// A `false` return is not fatal: abyss runs fine with no X11 domain at all,
/// which is exactly the isolation posture `xwayland { enable #false }` buys.
pub fn start(state: &mut AbyssState) -> bool {
    if !state.config.xwayland.enable {
        tracing::info!("xwayland disabled by config; no X11 trust domain will exist");
        return false;
    }

    // smithay spawns `Xwayland` from $PATH and its failure path panics inside
    // the child reaper ("wait() should either return Ok or panic") rather than
    // returning Err, which kills the compositor outright on a system where the
    // X server is simply not installed. Check first and degrade as designed.
    if !xwayland_on_path() {
        tracing::warn!("Xwayland not found in PATH; continuing without X11 support");
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
fn ready(state: &mut AbyssState, socket: std::os::unix::net::UnixStream, dpy: u32) {
    let Some(client) = state.xwayland.client.clone() else {
        tracing::error!("xwayland ready without a client; ignoring");
        return;
    };
    let wm = match X11Wm::start_wm::<AbyssState>(state.loop_handle.clone(), socket, client) {
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
    crate::session::import_display();

    apply_scale(state);
    tracing::info!(x11_display = format!(":{dpy}"), "xwayland ready");
}

/// Cursor scale / DPI hints for X11 clients (COMP-07 §4).
///
/// Default is client scaling (ADR 0078): the focused output's scale is handed
/// out and the client renders natively. `xwayland { scaling "compositor" }`
/// instead tells clients the world is 96 dpi at scale 1 and the compositor
/// upscales the result, which is blurry but sizes DPI-unaware apps correctly.
pub fn apply_scale(state: &mut AbyssState) {
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
    if state.xwayland.wm.is_none() {
        return;
    }
    if state.config.xwayland.scaling_client && state.xwayland.client_scale.is_none() {
        // Map X pixels 1:1 to physical pixels: smithay then reads X sizes and
        // positions as `x / scale` logical, and X buffers as that logical size.
        if let Some(client) = state.xwayland.client.clone() {
            use smithay::wayland::compositor::CompositorHandler;
            state.client_compositor_state(&client).set_client_scale(scale);
            state.xwayland.client_scale = Some(scale);
            // Outputs the X server already bound only see the new scale on
            // the next state change; without this its root stays logical-sized.
            let outputs: Vec<_> = state.space.outputs().cloned().collect();
            for output in outputs {
                output.change_current_state(None, None, None, None);
            }
        }
    }
    let Some(wm) = state.xwayland.wm.as_mut() else {
        return;
    };
    let cursor = cursor_size(std::env::var("XCURSOR_SIZE").ok().as_deref(), scale);
    let settings = [
        // XSETTINGS carries Xft.dpi multiplied by 1024.
        ("Xft/DPI".to_string(), Value::Integer(dpi * 1024)),
        (
            "Gdk/WindowScalingFactor".to_string(),
            Value::Integer(scale.round().max(1.0) as i32),
        ),
        ("Gdk/UnscaledDPI".to_string(), Value::Integer(96 * 1024)),
        ("Gtk/CursorThemeSize".to_string(), Value::Integer(cursor)),
    ];
    if let Err(e) = wm.set_xsettings(settings.into_iter()) {
        tracing::warn!(error = %e, "could not publish XSETTINGS");
        return;
    }
    state.xwayland.advertised_scale = Some(dpi);
    tracing::debug!(dpi, cursor, scale, "xwayland scaling updated");
    if let Some(dpy) = state.xwayland.display {
        publish_resource_manager(dpy, dpi, cursor);
    }
}

/// X cursor size in pixels: the session's `XCURSOR_SIZE` (24 if unset or
/// unparsable) times the client scale.
fn cursor_size(base: Option<&str>, scale: f64) -> i32 {
    let base = base
        .and_then(|v| v.trim().parse::<i32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(24);
    (f64::from(base) * scale).round().max(1.0) as i32
}

/// The `RESOURCE_MANAGER` text libXcursor and Chromium/CEF read.
fn resource_manager(dpi: i32, cursor: i32) -> String {
    format!("Xft.dpi:\t{dpi}\nXcursor.size:\t{cursor}\n")
}

/// Write `RESOURCE_MANAGER` on the X root. smithay keeps its X connection
/// private, so this opens a short-lived one, off the loop thread.
fn publish_resource_manager(dpy: u32, dpi: i32, cursor: i32) {
    let text = resource_manager(dpi, cursor);
    std::thread::spawn(move || {
        use x11rb::{
            connection::Connection,
            protocol::xproto::{AtomEnum, PropMode},
            wrapper::ConnectionExt,
        };
        let run = || -> Result<(), Box<dyn std::error::Error>> {
            let (conn, screen) = x11rb::connect(Some(&format!(":{dpy}")))?;
            let root = conn.setup().roots[screen].root;
            conn.change_property8(
                PropMode::REPLACE,
                root,
                AtomEnum::RESOURCE_MANAGER,
                AtomEnum::STRING,
                text.as_bytes(),
            )?
            .check()?;
            Ok(())
        };
        if let Err(e) = run() {
            tracing::warn!(error = %e, "could not set RESOURCE_MANAGER on the X root");
        }
    });
}

impl XWaylandShellHandler for AbyssState {
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
        // `place_new_window` names an X11 window as focused at map time, but
        // the keyboard cannot follow until the wl_surface exists, so
        // `focus_window` bailed. Finish the move now that it does.
        let pending = self.focus.clone().filter(|w| w.x11_surface() == Some(&surface));
        if let Some(window) = pending {
            if !self.lock.locked {
                crate::shell::focus_window(self, &window);
            }
        }
    }
}

smithay::delegate_xwayland_shell!(AbyssState);

/// Whether an executable `Xwayland` exists on `PATH`.
fn xwayland_on_path() -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| std::fs::metadata(dir.join("Xwayland")).is_ok_and(|m| m.is_file()))
}

#[cfg(test)]
mod scale_tests {
    use super::*;

    #[test]
    fn cursor_size_scales_the_session_base() {
        assert_eq!(cursor_size(None, 1.9), 46);
        assert_eq!(cursor_size(Some("32"), 2.0), 64);
        assert_eq!(cursor_size(Some("junk"), 1.0), 24);
        assert_eq!(cursor_size(Some("0"), 1.0), 24);
    }

    #[test]
    fn resource_manager_text() {
        assert_eq!(resource_manager(182, 46), "Xft.dpi:\t182\nXcursor.size:\t46\n");
    }
}
