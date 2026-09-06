// SPDX-License-Identifier: AGPL-3.0-only
//! Nested backend: one output inside a host Wayland/X11 window.

use anyhow::{Context, Result};
use smithay::{
    backend::{
        renderer::{damage::OutputDamageTracker, gles::GlesRenderer},
        winit::{self, WinitEvent},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{generic::Generic, EventLoop, Interest, Mode as CalloopMode, PostAction},
        wayland_server::Display,
        winit::{dpi::LogicalSize, window::WindowAttributes},
    },
    utils::Transform,
    wayland::socket::ListeningSocketSource,
};

use crate::{
    config::Config,
    render::{collect_elements, send_frames},
    state::{client_state, HeliosState},
};

const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

pub fn run(config: Config) -> Result<()> {
    let mut event_loop: EventLoop<HeliosState> = EventLoop::try_new().context("calloop")?;
    let display: Display<HeliosState> = Display::new().context("wayland display")?;
    let socket = ListeningSocketSource::new_auto().context("bind wayland socket")?;
    let mut state = HeliosState::new(&display, event_loop.get_signal(), &socket, config);
    let dh = state.display_handle.clone();

    let attrs = WindowAttributes::default()
        .with_inner_size(LogicalSize::new(1280.0, 800.0))
        .with_title("helios")
        .with_visible(true);
    let (mut backend, winit_loop) =
        winit::init_from_attributes::<GlesRenderer>(attrs).map_err(|e| anyhow::anyhow!("winit init: {e}"))?;

    let size = backend.window_size();
    let mode = Mode {
        size,
        refresh: 60_000,
    };
    let output = Output::new(
        "winit".into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "helios".into(),
            model: "winit".into(),
        },
    );
    let _global = output.create_global::<HeliosState>(&dh);
    output.change_current_state(Some(mode), Some(Transform::Flipped180), None, Some((0, 0).into()));
    output.set_preferred(mode);
    state.space.map_output(&output, (0, 0));
    let mut damage_tracker = OutputDamageTracker::from_output(&output);

    let handle = event_loop.handle();
    handle
        .insert_source(socket, |stream, _, state| {
            if let Err(e) = state.display_handle.insert_client(stream, client_state()) {
                tracing::warn!(?e, "failed to insert client");
            }
        })
        .map_err(|e| anyhow::anyhow!("socket source: {e}"))?;
    handle
        .insert_source(
            Generic::new(display, Interest::READ, CalloopMode::Level),
            |_, display, state| {
                // SAFETY: the display is only touched from this callback on the loop thread.
                unsafe { display.get_mut().dispatch_clients(state) }?;
                Ok(PostAction::Continue)
            },
        )
        .map_err(|e| anyhow::anyhow!("display source: {e}"))?;

    std::env::set_var("WAYLAND_DISPLAY", &state.socket_name);
    tracing::info!(socket = %state.socket_name, "listening");

    let out = output.clone();
    handle
        .insert_source(winit_loop, move |event, _, state| match event {
            WinitEvent::Resized { size, .. } => {
                out.change_current_state(
                    Some(Mode {
                        size,
                        refresh: 60_000,
                    }),
                    None,
                    None,
                    None,
                );
                crate::shell::arrange(state);
            }
            WinitEvent::Input(ev) => state.process_input_event(ev),
            WinitEvent::CloseRequested => state.quit(),
            WinitEvent::Redraw => {
                let age = backend.buffer_age().unwrap_or(0);
                let rendered = {
                    let (renderer, mut fb) = match backend.bind() {
                        Ok(b) => b,
                        Err(e) => {
                            tracing::error!(?e, "bind");
                            return;
                        }
                    };
                    let elements = collect_elements(
                        renderer,
                        &state.space,
                        &mut state.borders,
                        &out,
                        &state.config,
                        state.focus.as_ref(),
                    );
                    match damage_tracker.render_output(renderer, &mut fb, age, &elements, CLEAR) {
                        Ok(r) => r.damage.map(|d| d.to_vec()),
                        Err(e) => {
                            tracing::error!(?e, "render");
                            return;
                        }
                    }
                };
                if let Some(damage) = rendered {
                    if let Err(e) = backend.submit(Some(&damage)) {
                        tracing::error!(?e, "submit");
                    }
                }
                let time = state.start_time.elapsed();
                send_frames(&state.space, &out, time);
                state.space.refresh();
                state.popups.cleanup();
                backend.window().request_redraw();
            }
            WinitEvent::Focus(_) => {}
        })
        .map_err(|e| anyhow::anyhow!("winit source: {e}"))?;

    event_loop
        .run(None, &mut state, |state| {
            let _ = state.display_handle.flush_clients();
        })
        .context("event loop")?;
    tracing::info!("helios exited cleanly");
    Ok(())
}
