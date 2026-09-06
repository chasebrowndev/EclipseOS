// SPDX-License-Identifier: AGPL-3.0-only
//! Shared render-element collection for every backend (COMP-02 §4).
//!
//! Both the winit and DRM backends build their frame from [`collect_elements`];
//! the stacking order lives here once, not per backend.

use std::collections::HashMap;

use smithay::{
    backend::renderer::{
        element::{
            solid::{SolidColorBuffer, SolidColorRenderElement},
            surface::WaylandSurfaceRenderElement,
            AsRenderElements, Kind,
        },
        gles::GlesRenderer,
    },
    desktop::{layer_map_for_output, space::SpaceRenderElements, Space, Window},
    output::Output,
    utils::{Logical, Physical, Point, Rectangle, Scale},
    wayland::shell::wlr_layer::Layer,
};

use crate::config::Config;

smithay::backend::renderer::element::render_elements! {
    pub HeliosRenderElement<=GlesRenderer>;
    Space=SpaceRenderElements<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>,
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
    Solid=SolidColorRenderElement,
}

/// Four solid quads (top, bottom, left, right) per window.
type Border = [SolidColorBuffer; 4];

/// Border quads kept alive between frames, keyed by window.
#[derive(Default)]
pub struct BorderStore {
    borders: HashMap<Window, Border>,
}

impl BorderStore {
    pub fn remove(&mut self, window: &Window) {
        self.borders.remove(window);
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
) -> Vec<HeliosRenderElement> {
    let scale = Scale::from(output.current_scale().fractional_scale());
    let output_loc = space.output_geometry(output).map(|g| g.loc).unwrap_or_default();
    let mut elements: Vec<HeliosRenderElement> = Vec::new();

    let layers = |elements: &mut Vec<HeliosRenderElement>, which: [Layer; 2], renderer: &mut GlesRenderer| {
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
                            phys(geo.loc + output_loc, scale),
                            scale,
                            1.0,
                        )
                        .into_iter()
                        .map(HeliosRenderElement::Surface),
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
                phys(loc + output_loc, scale),
                scale,
                1.0,
                Kind::Unspecified,
            )
            .into_iter()
            .map(HeliosRenderElement::Surface),
        );
    }

    layers(&mut elements, [Layer::Overlay, Layer::Top], renderer);

    match smithay::desktop::space::space_render_elements(renderer, [space], output, 1.0) {
        Ok(space_elements) => elements.extend(space_elements.into_iter().map(HeliosRenderElement::Space)),
        Err(err) => tracing::warn!(?err, "collecting space elements"),
    }

    elements.extend(border_elements(space, borders, output_loc, scale, config, focus));

    layers(&mut elements, [Layer::Bottom, Layer::Background], renderer);

    elements
}

fn border_elements(
    space: &Space<Window>,
    store: &mut BorderStore,
    output_loc: Point<i32, Logical>,
    scale: Scale<f64>,
    config: &Config,
    focus: Option<&Window>,
) -> Vec<HeliosRenderElement> {
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
            (geo.loc.x - width + output_loc.x, geo.loc.y - width + output_loc.y).into(),
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
            out.push(HeliosRenderElement::Solid(SolidColorRenderElement::from_buffer(
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
        window.send_frame(output, time, Some(std::time::Duration::ZERO), |_, _| {
            Some(output.clone())
        });
    }
    let mut map = layer_map_for_output(output);
    for layer in map.layers() {
        layer.send_frame(output, time, Some(std::time::Duration::ZERO), |_, _| {
            Some(output.clone())
        });
    }
    map.cleanup();
}
