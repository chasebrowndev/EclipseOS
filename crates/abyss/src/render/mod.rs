// SPDX-License-Identifier: AGPL-3.0-only
//! Shared render-element collection for every backend (COMP-02 §4).
//!
//! Both the winit and DRM backends build their frame from [`collect_elements`];
//! the stacking order lives here once, not per backend.

pub mod capture;
pub mod cursor;
pub mod stats;

use std::collections::HashMap;

use smithay::backend::renderer::element::{default_primary_scanout_output_compare, RenderElementStates};
use smithay::{
    backend::renderer::{
        element::{
            solid::{SolidColorBuffer, SolidColorRenderElement},
            surface::WaylandSurfaceRenderElement,
            AsRenderElements, Kind,
        },
        gles::GlesRenderer,
    },
    desktop::{
        layer_map_for_output,
        space::SpaceRenderElements,
        utils::{
            surface_presentation_feedback_flags_from_states, surface_primary_scanout_output,
            update_surface_primary_scanout_output, OutputPresentationFeedback,
        },
        Space, Window,
    },
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Physical, Point, Rectangle, Scale},
    wayland::{dmabuf::DmabufFeedback, shell::wlr_layer::Layer},
};

use crate::config::Config;

smithay::backend::renderer::element::render_elements! {
    pub AbyssRenderElement<=GlesRenderer>;
    Space=SpaceRenderElements<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>,
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
    Solid=SolidColorRenderElement,
    Texture=smithay::backend::renderer::element::texture::TextureRenderElement<smithay::backend::renderer::gles::GlesTexture>,
}

/// Four solid quads (top, bottom, left, right) per window.
type Border = [SolidColorBuffer; 4];

/// Border quads kept alive between frames, keyed by window.
#[derive(Default)]
pub struct BorderStore {
    borders: HashMap<Window, Border>,
    /// One dim-inactive overlay quad per window, kept alive between frames.
    dims: HashMap<Window, SolidColorBuffer>,
}

impl BorderStore {
    pub fn remove(&mut self, window: &Window) {
        self.borders.remove(window);
        self.dims.remove(window);
    }
}

fn phys(p: Point<i32, Logical>, scale: Scale<f64>) -> Point<i32, Physical> {
    p.to_f64().to_physical(scale).to_i32_round()
}

/// Collect one frame's elements, front to back.
///
/// Order (topmost first) follows COMP-02 §4: overlay layer surfaces, top layer
/// surfaces, toplevels, their borders, then bottom and background layers.
/// Trusted UI is prepended by the caller once COMP-10 lands.
pub fn collect_elements(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    borders: &mut BorderStore,
    output: &Output,
    config: &Config,
    focus: Option<&Window>,
    im_popup: Option<&smithay::wayland::input_method::PopupSurface>,
) -> Vec<AbyssRenderElement> {
    let scale = Scale::from(output.current_scale().fractional_scale());
    let output_loc = space.output_geometry(output).map(|g| g.loc).unwrap_or_default();
    let mut elements: Vec<AbyssRenderElement> = Vec::new();

    let layers = |elements: &mut Vec<AbyssRenderElement>, which: [Layer; 2], renderer: &mut GlesRenderer| {
        let map = layer_map_for_output(output);
        for layer in which {
            for surface in map.layers_on(layer).rev() {
                let Some(geo) = map.layer_geometry(surface) else {
                    continue;
                };
                elements.extend(
                    surface
                        .render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(
                            renderer,
                            // Layer geometry is already output-local.
                            phys(geo.loc, scale),
                            scale,
                            1.0,
                        )
                        .into_iter()
                        .map(AbyssRenderElement::Surface),
                );
            }
        }
    };

    // The input-method popup sits above everything the shell draws (COMP-06 §1).
    if let Some((popup, loc)) =
        im_popup.and_then(|p| crate::protocols::standard::input_method::popup_location(p).map(|l| (p, l)))
    {
        elements.extend(
            smithay::backend::renderer::element::surface::render_elements_from_surface_tree(
                renderer,
                popup.wl_surface(),
                phys(loc - output_loc, scale),
                scale,
                1.0,
                Kind::Unspecified,
            )
            .into_iter()
            .map(AbyssRenderElement::Surface),
        );
    }

    layers(&mut elements, [Layer::Overlay, Layer::Top], renderer);

    // With no per-window effect configured (the default) the whole space goes
    // through smithay's one call at alpha 1.0, so damage tracking and direct
    // scanout are exactly what they were before milestone 9b (COMP-02 §9).
    if config.decoration.any_window_effect() {
        elements.extend(window_elements(renderer, space, borders, output, config, focus));
    } else {
        borders.dims.clear();
        match smithay::desktop::space::space_render_elements(renderer, [space], output, 1.0) {
            Ok(space_elements) => elements.extend(space_elements.into_iter().map(AbyssRenderElement::Space)),
            Err(err) => tracing::warn!(?err, "collecting space elements"),
        }
    }

    elements.extend(border_elements(space, borders, output_loc, scale, config, focus));

    layers(&mut elements, [Layer::Bottom, Layer::Background], renderer);

    elements
}

/// Per-window toplevel elements, front to back, with `decoration` opacity and
/// `dim-inactive` applied (COMP-02 §9).
///
/// This mirrors what `space_render_elements` does for toplevels, but one window
/// at a time so each can carry its own alpha. Any surface drawn at less than
/// full alpha cannot go to a scanout plane; the caller only takes this path when
/// an effect is actually configured.
fn window_elements(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    store: &mut BorderStore,
    output: &Output,
    config: &Config,
    focus: Option<&Window>,
) -> Vec<AbyssRenderElement> {
    let Some(output_geo) = space.output_geometry(output) else {
        return Vec::new();
    };
    let scale = Scale::from(output.current_scale().fractional_scale());
    let deco = &config.decoration;
    let live: Vec<Window> = space.elements().cloned().collect();
    store.dims.retain(|w, _| live.contains(w));

    let mut out = Vec::new();
    // `space.elements()` is bottom-to-top; frames are collected front-to-back.
    for window in live.into_iter().rev() {
        let Some(loc) = space.element_location(&window) else {
            continue;
        };
        let active = focus == Some(&window);
        let alpha = if active {
            deco.active_opacity
        } else {
            deco.inactive_opacity
        };

        // The dim overlay belongs above this window but below the ones in
        // front of it, so it is pushed just before the window's own surfaces.
        if !active && deco.dim_inactive > 0.0 {
            if let Some(geo) = space.element_geometry(&window) {
                let color = [0.0, 0.0, 0.0, deco.dim_inactive * alpha];
                let buffer = store
                    .dims
                    .entry(window.clone())
                    .or_insert_with(|| SolidColorBuffer::new(geo.size, color));
                buffer.update(geo.size, color);
                out.push(AbyssRenderElement::Solid(SolidColorRenderElement::from_buffer(
                    buffer,
                    phys(geo.loc - output_geo.loc, scale),
                    scale,
                    1.0,
                    Kind::Unspecified,
                )));
            }
        }

        let render_loc = loc - window.geometry().loc - output_geo.loc;
        out.extend(
            window
                .render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(
                    renderer,
                    phys(render_loc, scale),
                    scale,
                    alpha,
                )
                .into_iter()
                .map(AbyssRenderElement::Surface),
        );
    }
    out
}

fn border_elements(
    space: &Space<Window>,
    store: &mut BorderStore,
    output_loc: Point<i32, Logical>,
    scale: Scale<f64>,
    config: &Config,
    focus: Option<&Window>,
) -> Vec<AbyssRenderElement> {
    let width = config.general.border_size;
    if width <= 0 {
        store.borders.clear();
        return Vec::new();
    }
    let live: Vec<Window> = space.elements().cloned().collect();
    store.borders.retain(|w, _| live.contains(w));

    let mut out = Vec::new();
    for window in live {
        let Some(geo) = space.element_geometry(&window) else {
            continue;
        };
        let color = if focus == Some(&window) {
            config.general.col_active
        } else {
            config.general.col_inactive
        };
        // Outer rect: the tile, with the window inset by `width` on every side.
        let outer = Rectangle::new(
            // Window geometry is global; elements are output-local.
            (geo.loc.x - width - output_loc.x, geo.loc.y - width - output_loc.y).into(),
            (geo.size.w + 2 * width, geo.size.h + 2 * width).into(),
        );
        let quads = [
            // top, bottom, left, right
            Rectangle::new(outer.loc, (outer.size.w, width).into()),
            Rectangle::new(
                (outer.loc.x, outer.loc.y + outer.size.h - width).into(),
                (outer.size.w, width).into(),
            ),
            Rectangle::new(
                (outer.loc.x, outer.loc.y + width).into(),
                (width, (outer.size.h - 2 * width).max(0)).into(),
            ),
            Rectangle::new(
                (outer.loc.x + outer.size.w - width, outer.loc.y + width).into(),
                (width, (outer.size.h - 2 * width).max(0)).into(),
            ),
        ];
        let buffers = store
            .borders
            .entry(window)
            .or_insert_with(|| std::array::from_fn(|i| SolidColorBuffer::new(quads[i].size, color)));
        for (buffer, quad) in buffers.iter_mut().zip(quads) {
            buffer.update(quad.size, color);
            out.push(AbyssRenderElement::Solid(SolidColorRenderElement::from_buffer(
                buffer,
                phys(quad.loc, scale),
                scale,
                1.0,
                Kind::Unspecified,
            )));
        }
    }
    out
}

/// Send frame callbacks to everything that was just drawn.
pub fn send_frames(space: &Space<Window>, output: &Output, time: std::time::Duration) {
    for window in space.elements() {
        window.send_frame(
            output,
            time,
            Some(std::time::Duration::ZERO),
            surface_primary_scanout_output,
        );
    }
    let mut map = layer_map_for_output(output);
    for layer in map.layers() {
        layer.send_frame(
            output,
            time,
            Some(std::time::Duration::ZERO),
            surface_primary_scanout_output,
        );
    }
    map.cleanup();
}

/// Per-surface dmabuf feedback for one render device (COMP-02 §5).
///
/// `render` is what every surface gets by default; `scanout` additionally
/// carries a `Scanout`-flagged tranche built from the formats the output's
/// primary plane accepts, and is handed only to surfaces that are plausible
/// direct-scanout candidates.
#[derive(Debug, Clone)]
pub struct SurfaceFeedback {
    pub render: DmabufFeedback,
    pub scanout: DmabufFeedback,
}

/// Record, per surface, which output actually presented it. Feeds both
/// `wp_presentation` (zero-copy flag) and frame-callback throttling.
pub fn update_primary_scanout(space: &Space<Window>, output: &Output, states: &RenderElementStates) {
    for window in space.elements() {
        window.with_surfaces(|surface, data| {
            update_surface_primary_scanout_output(
                surface,
                output,
                data,
                states,
                default_primary_scanout_output_compare,
            );
        });
    }
    let map = layer_map_for_output(output);
    for layer in map.layers() {
        layer.with_surfaces(|surface, data| {
            update_surface_primary_scanout_output(
                surface,
                output,
                data,
                states,
                default_primary_scanout_output_compare,
            );
        });
    }
}

/// Collect the `wp_presentation` feedback owed for the frame just composited
/// on `output`. Call after [`update_primary_scanout`].
pub fn presentation_feedback(
    space: &Space<Window>,
    output: &Output,
    states: &RenderElementStates,
) -> OutputPresentationFeedback {
    let mut feedback = OutputPresentationFeedback::new(output);
    for window in space.elements() {
        window.take_presentation_feedback(&mut feedback, surface_primary_scanout_output, |surface, _| {
            surface_presentation_feedback_flags_from_states(surface, states)
        });
    }
    let map = layer_map_for_output(output);
    for layer in map.layers() {
        layer.take_presentation_feedback(&mut feedback, surface_primary_scanout_output, |surface, _| {
            surface_presentation_feedback_flags_from_states(surface, states)
        });
    }
    feedback
}

/// The surface that could plausibly be scanned out directly on `output`: the
/// top-most window whose geometry covers the whole output. Returning `None`
/// means every surface keeps the plain render feedback.
pub fn scanout_candidate(space: &Space<Window>, output: &Output) -> Option<WlSurface> {
    let geo = space.output_geometry(output)?;
    let window = space.elements_for_output(output).last()?;
    let win_geo = space.element_geometry(window)?;
    if !win_geo.contains_rect(geo) {
        return None;
    }
    use smithay::wayland::seat::WaylandFocus;
    window.wl_surface().map(|s| s.into_owned())
}

/// Send dmabuf feedback for every surface on `output`, giving the scanout
/// tranche only to `candidate`.
pub fn send_dmabuf_feedback(
    space: &Space<Window>,
    output: &Output,
    feedback: &SurfaceFeedback,
    candidate: Option<&WlSurface>,
) {
    let select = |surface: &WlSurface, _: &_| {
        if Some(surface) == candidate {
            &feedback.scanout
        } else {
            &feedback.render
        }
    };
    for window in space.elements() {
        window.send_dmabuf_feedback(output, surface_primary_scanout_output, select);
    }
    let map = layer_map_for_output(output);
    for layer in map.layers() {
        layer.send_dmabuf_feedback(output, surface_primary_scanout_output, select);
    }
}
