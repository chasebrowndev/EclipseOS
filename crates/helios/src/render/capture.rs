// SPDX-License-Identifier: AGPL-3.0-only
//! Capture rendering with redaction (COMP-02 §7, §8).
//!
//! Capture never reuses the on-screen pass list. It builds its own, and a
//! surface classified sensitive is *excluded from that list* — a solid
//! placeholder of its geometry is drawn instead, so the pixels never reach the
//! target buffer at all. The decision is recorded in [`Redacted`] before any
//! GPU work, so tests can assert on it without reading pixels.
//!
//! Trusted UI (the capture indicator, the lock screen) is drawn by the
//! backends, never here, so it cannot be captured by construction.

use smithay::{
    backend::renderer::{
        damage::OutputDamageTracker,
        element::{
            solid::{SolidColorBuffer, SolidColorRenderElement},
            surface::WaylandSurfaceRenderElement,
            AsRenderElements, Kind,
        },
        gles::GlesRenderer,
        Bind, ExportMem, Offscreen,
    },
    desktop::{layer_map_for_output, Window},
    output::Output,
    reexports::{
        wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1,
        wayland_server::{protocol::wl_buffer::WlBuffer, Resource},
    },
    utils::{Buffer as BufferCoord, Logical, Physical, Point, Rectangle, Scale, Transform},
    wayland::shell::wlr_layer::Layer,
};

use crate::{render::HeliosRenderElement, state::HeliosState};

/// The pixel format captured frames are handed to clients in. `Xbgr8888` maps
/// straight onto the GL ES read path (`RGBA`/`UNSIGNED_BYTE`), so the readback
/// needs no channel swizzle on the CPU.
pub const FORMAT: smithay::backend::allocator::Fourcc = smithay::backend::allocator::Fourcc::Xbgr8888;
/// The matching `wl_shm` format advertised to clients.
pub const SHM_FORMAT: smithay::reexports::wayland_server::protocol::wl_shm::Format =
    smithay::reexports::wayland_server::protocol::wl_shm::Format::Xbgr8888;

/// Opaque black. A redacted surface must not leak its shape's contents, and an
/// alpha-blended placeholder would let whatever is underneath show through.
const PLACEHOLDER: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

/// One redaction decision, recorded when the pass list is built.
#[derive(Debug, Clone, PartialEq)]
pub struct Redacted {
    pub app_id: Option<String>,
    pub geometry: Rectangle<i32, Logical>,
}

/// A `copy` request waiting for a renderer. Holding this does not mean the
/// capture was authorised — nothing reaches this queue until [`super::super::
/// protocols::standard::screencopy`] has returned `Allow`.
pub struct Pending {
    pub frame: ZwlrScreencopyFrameV1,
    pub buffer: WlBuffer,
    pub output_id: u64,
    /// Region of the output to copy, output-local and physical.
    pub region: Rectangle<i32, Physical>,
    pub with_damage: bool,
}

/// Is this surface sensitive? Ratchet rule (COMP-02 §7): both inputs may only
/// *raise* the class, neither can lower it, so this is a plain `or`.
pub fn is_sensitive(in_set: bool, app_id: Option<&str>, redact: &[String]) -> bool {
    in_set || app_id.is_some_and(|id| redact.iter().any(|r| r == id))
}

/// Record a newly mapped window in the sensitive set when config says its
/// `app_id` is secret. Called on map, never on the commit path.
pub fn mark_sensitive(state: &mut HeliosState, window: &Window) {
    if state.config.capture.redact_app_id.is_empty() {
        return;
    }
    let Some(surface) = crate::shell::window_surface(window) else {
        return;
    };
    let app_id = crate::protocols::standard::data_device::app_id_of(&surface);
    if is_sensitive(false, app_id.as_deref(), &state.config.capture.redact_app_id) {
        tracing::info!(
            app_id = app_id.as_deref().unwrap_or("<none>"),
            "surface marked sensitive"
        );
        state.sensitive.insert(surface);
    }
}

fn phys(p: Point<i32, Logical>, scale: Scale<f64>) -> Point<i32, Physical> {
    p.to_f64().to_physical(scale).to_i32_round()
}

/// Build the capture pass list for one output, with redaction applied.
///
/// Returns the elements front-to-back and the redaction decisions taken. The
/// cursor, borders and trusted UI are deliberately absent: a capture target is
/// the shell's content, not the compositor's own chrome.
pub fn capture_elements(
    renderer: &mut GlesRenderer,
    state: &HeliosState,
    output: &Output,
) -> (Vec<HeliosRenderElement>, Vec<Redacted>) {
    let scale = Scale::from(output.current_scale().fractional_scale());
    let output_loc = state
        .space
        .output_geometry(output)
        .map(|g| g.loc)
        .unwrap_or_default();
    let mut elements: Vec<HeliosRenderElement> = Vec::new();
    let mut redacted: Vec<Redacted> = Vec::new();
    let redact_ids = &state.config.capture.redact_app_id;

    layer_elements(
        renderer,
        state,
        output,
        [Layer::Overlay, Layer::Top],
        scale,
        &mut elements,
        &mut redacted,
    );

    // Topmost first. `space.elements()` is bottom-to-top.
    for window in state.space.elements().rev() {
        let Some(geo) = state.space.element_geometry(window) else {
            continue;
        };
        let surface = crate::shell::window_surface(window);
        let app_id = surface
            .as_ref()
            .and_then(crate::protocols::standard::data_device::app_id_of);
        let in_set = surface.as_ref().is_some_and(|s| state.sensitive.contains(s));
        // Decided here, before any GPU work touches the surface.
        if is_sensitive(in_set, app_id.as_deref(), redact_ids) {
            redacted.push(Redacted {
                app_id,
                geometry: geo,
            });
            elements.push(placeholder(geo, output_loc, scale));
            continue;
        }
        let loc = geo.loc - output_loc - window.geometry().loc;
        elements.extend(
            window
                .render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(
                    renderer,
                    phys(loc, scale),
                    scale,
                    1.0,
                )
                .into_iter()
                .map(HeliosRenderElement::Surface),
        );
    }

    layer_elements(
        renderer,
        state,
        output,
        [Layer::Bottom, Layer::Background],
        scale,
        &mut elements,
        &mut redacted,
    );

    (elements, redacted)
}

#[allow(clippy::too_many_arguments)]
fn layer_elements(
    renderer: &mut GlesRenderer,
    state: &HeliosState,
    output: &Output,
    which: [Layer; 2],
    scale: Scale<f64>,
    elements: &mut Vec<HeliosRenderElement>,
    redacted: &mut Vec<Redacted>,
) {
    let map = layer_map_for_output(output);
    for layer in which {
        for surface in map.layers_on(layer).rev() {
            let Some(geo) = map.layer_geometry(surface) else {
                continue;
            };
            let wl = surface.wl_surface();
            if is_sensitive(
                state.sensitive.contains(wl),
                None,
                &state.config.capture.redact_app_id,
            ) {
                redacted.push(Redacted {
                    app_id: None,
                    geometry: geo,
                });
                elements.push(placeholder(geo, Point::default(), scale));
                continue;
            }
            elements.extend(
                surface
                    .render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(
                        renderer,
                        phys(geo.loc, scale),
                        scale,
                        1.0,
                    )
                    .into_iter()
                    .map(HeliosRenderElement::Surface),
            );
        }
    }
}

fn placeholder(
    geo: Rectangle<i32, Logical>,
    output_loc: Point<i32, Logical>,
    scale: Scale<f64>,
) -> HeliosRenderElement {
    let buffer = SolidColorBuffer::new(geo.size, PLACEHOLDER);
    HeliosRenderElement::Solid(SolidColorRenderElement::from_buffer(
        &buffer,
        phys(geo.loc - output_loc, scale),
        scale,
        1.0,
        Kind::Unspecified,
    ))
}

/// How long after the last captured frame the indicator stays lit. Capture
/// clients pull frames continuously; this only has to outlast one frame
/// interval so the indicator does not flicker.
pub const INDICATOR_LINGER: std::time::Duration = std::time::Duration::from_secs(2);

/// The compositor-drawn capture indicator (COMP-10 §3.6): a solid marker in
/// the top-right corner of every output, persistent for as long as capture is
/// happening. It is trusted UI, so it is drawn only by the backends into the
/// on-screen frame — never by [`capture_elements`], which is why a capture
/// client cannot see, capture or spoof it.
pub fn indicator(output: &Output, active: bool) -> Vec<HeliosRenderElement> {
    if !active {
        return Vec::new();
    }
    let Some(mode) = output.current_mode() else {
        return Vec::new();
    };
    let scale = Scale::from(output.current_scale().fractional_scale());
    let logical: smithay::utils::Size<i32, Logical> = mode.size.to_f64().to_logical(scale).to_i32_round();
    let size: smithay::utils::Size<i32, Logical> = (48, 8).into();
    let buffer = SolidColorBuffer::new(size, [1.0, 0.15, 0.15, 1.0]);
    let loc: Point<i32, Logical> = (logical.w - size.w - 8, 8).into();
    vec![HeliosRenderElement::Solid(SolidColorRenderElement::from_buffer(
        &buffer,
        phys(loc, scale),
        scale,
        1.0,
        Kind::Unspecified,
    ))]
}

/// Service every queued capture. Called once per composite, from the backend
/// that owns the renderer — the only place a `GlesRenderer` is reachable.
pub fn service(state: &mut HeliosState, renderer: &mut GlesRenderer) {
    if state.captures.is_empty() {
        return;
    }
    let pending: Vec<Pending> = std::mem::take(&mut state.captures);
    for p in pending {
        if !p.frame.is_alive() {
            continue;
        }
        // Re-check the gate at service time: the session may have locked
        // between the `copy` request and this composite.
        if state.lock.locked {
            tracing::warn!("screencopy denied at service time (session locked)");
            p.frame.failed();
            continue;
        }
        match copy_one(state, renderer, &p) {
            Ok(()) => {
                if p.with_damage {
                    p.frame
                        .damage(0, 0, p.region.size.w.max(0) as u32, p.region.size.h.max(0) as u32);
                }
                let now = std::time::Duration::from(state.clock.now());
                let secs = now.as_secs();
                p.frame.ready(
                    (secs >> 32) as u32,
                    (secs & 0xFFFF_FFFF) as u32,
                    now.subsec_nanos(),
                );
                state.capture_seen = Some(std::time::Instant::now());
            }
            Err(e) => {
                tracing::warn!(error = %e, "screencopy failed");
                p.frame.failed();
            }
        }
    }
}

fn copy_one(state: &HeliosState, renderer: &mut GlesRenderer, p: &Pending) -> anyhow::Result<()> {
    let Some(entry) = state.outputs.get(p.output_id) else {
        anyhow::bail!("output gone");
    };
    let output = entry.output.clone();
    let mode = output
        .current_mode()
        .ok_or_else(|| anyhow::anyhow!("output has no mode"))?;

    let (elements, redacted) = capture_elements(renderer, state, &output);
    if !redacted.is_empty() {
        tracing::info!(count = redacted.len(), "redacted surfaces excluded from capture");
    }

    // Render through the output's own transform so a capture is oriented the way
    // the user sees the screen, not the way the GPU happens to store it.
    let fb_size = output.current_transform().transform_size(mode.size);
    let mut target: smithay::backend::renderer::gles::GlesTexture = Offscreen::create_buffer(
        renderer,
        FORMAT,
        fb_size.to_logical(1).to_buffer(1, Transform::Normal),
    )?;
    {
        let mut fb = Bind::bind(renderer, &mut target)?;
        let mut tracker = OutputDamageTracker::from_output(&output);
        tracker
            .render_output(renderer, &mut fb, 0, &elements, PLACEHOLDER)
            .map_err(|e| anyhow::anyhow!("render capture: {e:?}"))?;
        let region: Rectangle<i32, BufferCoord> = Rectangle::new(
            (p.region.loc.x, p.region.loc.y).into(),
            (p.region.size.w, p.region.size.h).into(),
        );
        let mapping = ExportMem::copy_framebuffer(renderer, &fb, region, FORMAT)?;
        let pixels = ExportMem::map_texture(renderer, &mapping)?;
        write_shm(&p.buffer, p.region, pixels)?;
    }
    Ok(())
}

/// Copy the readback into the client's shm buffer. The buffer was validated
/// when `copy` arrived; re-validate here because the client may have resized
/// the pool in between.
fn write_shm(buffer: &WlBuffer, region: Rectangle<i32, Physical>, pixels: &[u8]) -> anyhow::Result<()> {
    let w = region.size.w.max(0) as usize;
    let h = region.size.h.max(0) as usize;
    let src_stride = w * 4;
    smithay::wayland::shm::with_buffer_contents_mut(buffer, |ptr, len, data| {
        if data.format != SHM_FORMAT || data.width as usize != w || data.height as usize != h {
            anyhow::bail!("buffer no longer matches the advertised frame");
        }
        let dst_stride = data.stride.max(0) as usize;
        let offset = data.offset.max(0) as usize;
        if dst_stride < src_stride || offset + dst_stride * h > len || pixels.len() < src_stride * h {
            anyhow::bail!("buffer too small");
        }
        for y in 0..h {
            // SAFETY: bounds checked immediately above; `ptr` is valid for `len`
            // bytes for the duration of this closure.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    pixels.as_ptr().add(y * src_stride),
                    ptr.add(offset + y * dst_stride),
                    src_stride,
                );
            }
        }
        Ok(())
    })??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_sensitive;

    #[test]
    fn nothing_is_sensitive_by_default() {
        assert!(!is_sensitive(false, Some("firefox"), &[]));
        assert!(!is_sensitive(false, None, &[]));
    }

    #[test]
    fn config_raises() {
        let redact = vec!["bitwarden".to_string()];
        assert!(is_sensitive(false, Some("bitwarden"), &redact));
        assert!(!is_sensitive(false, Some("firefox"), &redact));
    }

    #[test]
    fn the_set_raises_and_config_cannot_lower_it() {
        // Already in the sensitive set: no config value, and no missing
        // app_id, can bring it back down. Ratchet rule.
        assert!(is_sensitive(true, None, &[]));
        assert!(is_sensitive(true, Some("firefox"), &["bitwarden".to_string()]));
    }
}
