// SPDX-License-Identifier: AGPL-3.0-only
//! Shared render-element collection for every backend (COMP-02 §4).
//!
//! Both the winit and DRM backends build their frame from [`collect_elements`];
//! the stacking order lives here once, not per backend.

pub mod anim;
pub mod blur;
pub mod capture;
pub mod cursor;
pub mod effects;
pub mod overscan;
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
    Rounded=effects::RoundedElement,
    Shader=smithay::backend::renderer::gles::element::PixelShaderElement,
    Blur=blur::BlurElement,
}

/// A window that wants a blurred backdrop: the window, the index in the element
/// list directly below its surfaces, and the region it blurs.
type BlurRequest = (Window, usize, Rectangle<i32, Physical>);

/// Four solid quads (top, bottom, left, right) per window.
type Border = [SolidColorBuffer; 4];

/// Border quads kept alive between frames, keyed by window.
#[derive(Default)]
pub struct BorderStore {
    borders: HashMap<Window, Border>,
    /// In-flight window moves (COMP-02 §9).
    pub anim: anim::AnimStore,
    /// One dim-inactive overlay quad per window, kept alive between frames.
    dims: HashMap<Window, SolidColorBuffer>,
    /// Rounded-corner texture program, compiled on the first frame that rounds.
    rounded: Option<smithay::backend::renderer::gles::GlesTexProgram>,
    /// Drop-shadow pixel program, compiled on the first frame that shadows.
    shadow: Option<smithay::backend::renderer::gles::GlesPixelProgram>,
    /// Blur chain, programs and per-window backdrops (COMP-02 §9).
    blur: blur::BlurStore,
}

impl BorderStore {
    pub fn remove(&mut self, window: &Window) {
        self.borders.remove(window);
        self.dims.remove(window);
        self.blur.forget(window);
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
///
/// `fullscreen` says a fullscreen toplevel owns this output, which drops the
/// `Top` layer — the bar — below the window stack so the surface actually covers
/// it. `Overlay` stays on top: it is the layer reserved for things that outrank a
/// fullscreen window, such as a lock screen.
#[allow(clippy::too_many_arguments)]
pub fn collect_elements(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    borders: &mut BorderStore,
    output: &Output,
    config: &Config,
    focus: Option<&Window>,
    im_popup: Option<&smithay::wayland::input_method::PopupSurface>,
    fullscreen: bool,
) -> Vec<AbyssRenderElement> {
    let scale = Scale::from(output.current_scale().fractional_scale());
    let output_loc = space.output_geometry(output).map(|g| g.loc).unwrap_or_default();
    let mut elements: Vec<AbyssRenderElement> = Vec::new();
    // (window, index in `elements` directly below its surfaces, region).
    let mut blur_requests: Vec<BlurRequest> = Vec::new();

    let layers = |elements: &mut Vec<AbyssRenderElement>, which: &[Layer], renderer: &mut GlesRenderer| {
        let map = layer_map_for_output(output);
        for &layer in which {
            for surface in map.layers_on(layer).rev() {
                let Some(geo) = crate::shell::layer_geometry(&map, surface) else {
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

    let (above, below): (&[Layer], &[Layer]) = if fullscreen {
        (&[Layer::Overlay], &[Layer::Top, Layer::Bottom, Layer::Background])
    } else {
        (&[Layer::Overlay, Layer::Top], &[Layer::Bottom, Layer::Background])
    };
    layers(&mut elements, above, renderer);

    // With no per-window effect configured (the default) the whole space goes
    // through smithay's one call at alpha 1.0, so damage tracking and direct
    // scanout are exactly what they were before milestone 9b (COMP-02 §9).
    borders.anim.sync(space, config, focus);
    if borders.anim.running()
        || config.decoration.any_window_effect()
        || borders.anim.fading()
        || crate::shell::rules::any_opacity_override(space.elements())
    {
        let base = elements.len();
        let (window_els, mut blurred) = window_elements(renderer, space, borders, output, config, focus);
        elements.extend(window_els);
        for req in &mut blurred {
            req.1 += base;
        }
        blur_requests = blurred;
    } else {
        borders.dims.clear();
        match smithay::desktop::space::space_render_elements(renderer, [space], output, 1.0) {
            Ok(space_elements) => elements.extend(space_elements.into_iter().map(AbyssRenderElement::Space)),
            Err(err) => tracing::warn!(?err, "collecting space elements"),
        }
    }

    elements.extend(border_elements(space, borders, output_loc, scale, config));
    elements.extend(shadow_elements(renderer, space, borders, output_loc, config));

    layers(&mut elements, below, renderer);

    insert_blur(renderer, output, borders, config, blur_requests, &mut elements);

    elements
}

/// Build and splice in one blurred backdrop per translucent window (COMP-02 §9).
///
/// Requests arrive in the order the windows were drawn (front to back); they are
/// consumed back to front so each insertion leaves the earlier indices — and the
/// backdrop of every window further back, already spliced in — intact.
fn insert_blur(
    renderer: &mut GlesRenderer,
    output: &Output,
    store: &mut BorderStore,
    config: &Config,
    requests: Vec<BlurRequest>,
    elements: &mut Vec<AbyssRenderElement>,
) {
    let cfg = &config.decoration.blur;
    if !cfg.enabled {
        // Switching blur off must not leave the chain sitting in GPU memory.
        store.blur.clear();
        return;
    }
    let scale = Scale::from(output.current_scale().fractional_scale());
    for (window, index, region) in requests.into_iter().rev() {
        let behind = &elements[index..];
        if let Some(element) = store
            .blur
            .element(renderer, output, &window, region, behind, cfg, scale)
        {
            elements.insert(index, AbyssRenderElement::Blur(element));
        }
    }
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
) -> (Vec<AbyssRenderElement>, Vec<BlurRequest>) {
    let Some(output_geo) = space.output_geometry(output) else {
        return (Vec::new(), Vec::new());
    };
    let scale = Scale::from(output.current_scale().fractional_scale());
    let deco = &config.decoration;
    let live: Vec<Window> = space.elements().cloned().collect();
    store.dims.retain(|w, _| live.contains(w));

    // Rounding masks in framebuffer space, so it needs the output's own height
    // and the sense of its vertical axis. `Normal` puts the physical origin at
    // the top-left and GL's at the bottom-left; `Flipped180` (what the winit
    // backend uses) already mirrors vertically, so the two coincide. A rotated
    // output keeps square corners rather than drawing the mask in the wrong
    // place (COMP-02 §9).
    let fb_height = match (
        deco.rounding > 0,
        output.current_transform(),
        output.current_mode(),
    ) {
        (true, smithay::utils::Transform::Normal, Some(mode)) => Some((mode.size.h, false)),
        (true, smithay::utils::Transform::Flipped180, Some(mode)) => Some((mode.size.h, true)),
        _ => None,
    };
    if fb_height.is_some() && store.rounded.is_none() {
        match effects::compile_rounded(renderer) {
            Ok(program) => store.rounded = Some(program),
            Err(err) => tracing::warn!(?err, "compiling the rounded-corner shader; rounding disabled"),
        }
    }
    let rounding = fb_height.zip(store.rounded.clone());

    let mut out = Vec::new();
    let mut blurred: Vec<BlurRequest> = Vec::new();
    // `space.elements()` is bottom-to-top; frames are collected front-to-back.
    for window in live.into_iter().rev() {
        let Some(loc) = space.element_location(&window) else {
            continue;
        };
        let active = focus == Some(&window);
        // A matched `windowrule "opacity …"` overrides the global pair.
        let alpha = crate::shell::rules::opacity_of(&window).unwrap_or(if active {
            deco.active_opacity
        } else {
            deco.inactive_opacity
        }) * store.anim.fade(&window);

        // The dim overlay belongs above this window but below the ones in
        // front of it, so it is pushed just before the window's own surfaces.
        if !active && deco.dim_inactive > 0.0 {
            if let Some(mut geo) = space.element_geometry(&window) {
                geo.loc += store.anim.offset(&window);
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

        let render_loc = loc + store.anim.offset(&window) - window.geometry().loc - output_geo.loc;
        let surfaces = window.render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(
            renderer,
            phys(render_loc, scale),
            scale,
            alpha,
        );

        // Every surface of one window is masked by the same rectangle, so a
        // window with subsurfaces rounds as a single shape.
        match (&rounding, space.element_geometry(&window)) {
            (Some(((fb_height, flip_y), program)), Some(mut geo)) => {
                geo.loc += store.anim.offset(&window);
                let rect = Rectangle::new(
                    phys(geo.loc - output_geo.loc, scale),
                    geo.size.to_f64().to_physical(scale).to_i32_round(),
                );
                let radius = deco.rounding as f64 * scale.x.max(scale.y);
                let uniforms = effects::rounding_uniforms(rect, *fb_height, *flip_y, radius as f32);
                out.extend(surfaces.into_iter().map(|surface| {
                    AbyssRenderElement::Rounded(effects::RoundedElement::new(
                        surface,
                        program.clone(),
                        uniforms.clone(),
                    ))
                }));
            }
            _ => out.extend(surfaces.into_iter().map(AbyssRenderElement::Surface)),
        }

        // Blur samples what shows through the window's alpha, so a window drawn
        // at full opacity gets no backdrop pass at all.
        if config.decoration.blur.enabled && alpha < 1.0 {
            if let Some(mut geo) = space.element_geometry(&window) {
                geo.loc += store.anim.offset(&window);
                blurred.push((
                    window.clone(),
                    out.len(),
                    Rectangle::new(
                        phys(geo.loc - output_geo.loc, scale),
                        geo.size.to_f64().to_physical(scale).to_i32_round(),
                    ),
                ));
            }
        }
    }
    (out, blurred)
}

/// One drop shadow behind each window, below the borders (COMP-02 §9).
fn shadow_elements(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    store: &mut BorderStore,
    output_loc: Point<i32, Logical>,
    config: &Config,
) -> Vec<AbyssRenderElement> {
    let shadow = &config.decoration.shadow;
    if !shadow.enabled || shadow.range <= 0 {
        return Vec::new();
    }
    if store.shadow.is_none() {
        match effects::compile_shadow(renderer) {
            Ok(program) => store.shadow = Some(program),
            Err(err) => {
                tracing::warn!(?err, "compiling the shadow shader; shadows disabled");
                return Vec::new();
            }
        }
    }
    let Some(program) = store.shadow.clone() else {
        return Vec::new();
    };

    // The shadow sits outside the border, so it grows from the bordered rect.
    let inset = config.general.border_size;
    let range = shadow.range;
    let mut out = Vec::new();
    for window in space.elements() {
        let Some(mut geo) = space.element_geometry(window) else {
            continue;
        };
        geo.loc += store.anim.offset(window);
        let area = Rectangle::new(
            (
                geo.loc.x - output_loc.x - inset - range,
                geo.loc.y - output_loc.y - inset - range,
            )
                .into(),
            (geo.size.w + 2 * (inset + range), geo.size.h + 2 * (inset + range)).into(),
        );
        out.push(AbyssRenderElement::Shader(effects::shadow_element(
            program.clone(),
            area,
            range as f32,
            config.decoration.rounding as f32,
        )));
    }
    out
}

fn border_elements(
    space: &Space<Window>,
    store: &mut BorderStore,
    output_loc: Point<i32, Logical>,
    scale: Scale<f64>,
    config: &Config,
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
        let Some(mut geo) = space.element_geometry(&window) else {
            continue;
        };
        geo.loc += store.anim.offset(&window);
        // The `border` animation crossfades this on focus change; with the
        // animation off it is the focused/unfocused colour outright.
        let color = store
            .anim
            .border_color(&window, config.general.col_active, config.general.col_inactive);
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
///
/// A configured window effect rules scanout out entirely: opacity, rounding,
/// dim and blur all mean we composite the window's pixels ourselves, and a
/// buffer handed straight to a plane never passes through any of them.
pub fn scanout_candidate(space: &Space<Window>, output: &Output, config: &Config) -> Option<WlSurface> {
    if config.decoration.any_window_effect() || config.decoration.blur.enabled {
        return None;
    }
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
