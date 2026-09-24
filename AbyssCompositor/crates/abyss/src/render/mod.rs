// SPDX-License-Identifier: AGPL-3.0-only
//! Shared render-element collection for every backend (COMP-02 §4).
//!
//! Both the winit and DRM backends build their frame from [`collect_elements`];
//! the stacking order lives here once, not per backend.

pub mod anim;
pub mod annotation;
pub mod blur;
pub mod capture;
pub mod cursor;
pub mod curve;
pub mod effects;
pub mod font;
pub mod overscan;
pub mod sanitize;
pub mod select;
pub mod stats;
pub mod text;

use std::collections::HashMap;

use smithay::backend::renderer::element::{
    default_primary_scanout_output_compare, Element, RenderElementStates,
};
use smithay::{
    backend::renderer::{
        element::{
            solid::{SolidColorBuffer, SolidColorRenderElement},
            surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
            utils::CropRenderElement,
            AsRenderElements, Kind,
        },
        gles::{element::PixelShaderElement, GlesRenderer},
    },
    desktop::{
        layer_map_for_output,
        space::SpaceRenderElements,
        utils::{
            surface_presentation_feedback_flags_from_states, surface_primary_scanout_output,
            update_surface_primary_scanout_output, OutputPresentationFeedback,
        },
        PopupManager, Space, Window,
    },
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Physical, Point, Rectangle, Scale},
    wayland::{
        dmabuf::DmabufFeedback,
        shell::wlr_layer::{Anchor, ExclusiveZone, Layer},
    },
};

use crate::config::Config;

smithay::backend::renderer::element::render_elements! {
    pub AbyssRenderElement<=GlesRenderer>;
    Space=SpaceRenderElements<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>,
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
    Solid=SolidColorRenderElement,
    Texture=smithay::backend::renderer::element::texture::TextureRenderElement<smithay::backend::renderer::gles::GlesTexture>,
    // Memory-backed, so `DrmCompositor` can scan it out on a plane (a texture
    // element has no underlying storage and never can). Used by the cursor.
    Memory=smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement<GlesRenderer>,
    Rounded=effects::RoundedElement,
    Shader=smithay::backend::renderer::gles::element::PixelShaderElement,
    Blur=blur::BlurElement,
    // A tiled window that overhangs its tile, cut back to it (see `window_elements`).
    Cropped=CropRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>,
    CroppedRounded=CropRenderElement<effects::RoundedElement>,
}

/// A surface that wants a blurred backdrop: what it belongs to, the index in
/// the element list directly below its surfaces, and the region it blurs.
type BlurRequest = (blur::BlurKey, usize, Rectangle<i32, Physical>);

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
    /// Rounded border-ring program, compiled on the first frame that rounds.
    ring: Option<smithay::backend::renderer::gles::GlesPixelProgram>,
    /// One rounded border ring per window when rounding is on, kept alive
    /// between frames so its damage is tracked; the last-applied colour,
    /// radius, width and scale are kept to skip no-op uniform updates.
    rings: HashMap<Window, (PixelShaderElement, [f32; 7])>,
    /// One drop shadow per window when shadows are on, kept alive between
    /// frames for the same reason; the last-applied range and radius are kept
    /// to skip no-op uniform updates.
    shadows: HashMap<Window, (PixelShaderElement, [f32; 2])>,
    /// Blur chain, programs and per-window backdrops (COMP-02 §9).
    blur: blur::BlurStore,
}

impl BorderStore {
    pub fn remove(&mut self, window: &Window) {
        self.borders.remove(window);
        self.rings.remove(window);
        self.shadows.remove(window);
        self.dims.remove(window);
    }
}

fn phys(p: Point<i32, Logical>, scale: Scale<f64>) -> Point<i32, Physical> {
    p.to_f64().to_physical(scale).to_i32_round()
}

/// Collect one frame's elements, front to back.
///
/// Order (topmost first) follows COMP-02 §4: overlay layer surfaces, top layer
/// surfaces, toplevels (each followed by its own border, shadow and blurred
/// backdrop), then bottom and background layers.
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
    // Filled front to back; `insert_blur` consumes it in reverse.
    let mut blur_requests: Vec<BlurRequest> = Vec::new();

    let blur_layers = config.decoration.blur.enabled;
    let layers = |elements: &mut Vec<AbyssRenderElement>,
                  requests: &mut Vec<BlurRequest>,
                  which: &[Layer],
                  renderer: &mut GlesRenderer| {
        let map = layer_map_for_output(output);
        for &layer in which {
            for surface in map.layers_on(layer).rev() {
                let Some(geo) = crate::shell::layer_geometry(&map, surface) else {
                    continue;
                };
                // Layer geometry is already output-local.
                let loc = phys(geo.loc, scale);
                let els = surface
                    .render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(renderer, loc, scale, 1.0);
                // Vol 1 §5.2 asks for blur behind layer-shell too. A layer
                // client declares its own translucency through the surface's
                // opaque region; anything it leaves uncovered is glass and
                // wants a backdrop. COMP-02 §9: opaque surfaces are skipped
                // entirely, so a bar that paints a solid ground costs nothing.
                if blur_layers {
                    let region = Rectangle::new(loc, geo.size.to_f64().to_physical(scale).to_i32_round());
                    let opaque: Vec<_> = els
                        .iter()
                        .flat_map(|e| {
                            let at = e.geometry(scale).loc;
                            e.opaque_regions(scale)
                                .into_iter()
                                .map(move |r| Rectangle::new(r.loc + at, r.size))
                        })
                        .collect();
                    if blur::shows_through(region, opaque) {
                        requests.push((
                            blur::BlurKey::Layer(surface.clone()),
                            elements.len() + els.len(),
                            region,
                        ));
                    }
                }
                elements.extend(els.into_iter().map(AbyssRenderElement::Surface));
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
    layers(&mut elements, &mut blur_requests, above, renderer);

    // Toplevels go one window at a time so each one's border and shadow stack
    // directly under its own surfaces rather than under the whole window stack
    // (COMP-02 §9). With no effect configured every surface is still emitted
    // unwrapped at alpha 1.0, so damage tracking and direct scanout are what
    // `space_render_elements` gave.
    borders.anim.sync(space, config, focus);
    window_elements(
        renderer,
        space,
        borders,
        output,
        config,
        focus,
        &mut elements,
        &mut blur_requests,
    );

    layers(&mut elements, &mut blur_requests, below, renderer);

    insert_blur(renderer, output, borders, config, blur_requests, &mut elements);

    elements
}

/// Build and splice in one blurred backdrop per translucent surface (COMP-02 §9).
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
    let deco = &config.decoration;
    let scale = Scale::from(output.current_scale().fractional_scale());
    let live: Vec<blur::BlurKey> = requests.iter().map(|(key, _, _)| key.clone()).collect();
    store.blur.retain(&live);

    // Same framebuffer-space mask as `window_elements`: needs the output's own
    // height and the sense of its vertical axis (COMP-02 §9).
    let fb_height = effects::fb_y_mirrored(output.current_transform())
        .zip(output.current_mode())
        .map(|(mirrored, mode)| (mode.size.h, mirrored));

    for (key, index, region) in requests.into_iter().rev() {
        // A toplevel reuses the radius its own corners are drawn with. A
        // layer-shell surface anchored to three-plus edges, or to both edges
        // of an axis, usually spans that axis corner-to-corner; fewer/adjacent
        // anchors (launcher, toasts, notification centre) get the same radius
        // as a window.
        //
        // "Usually" because a 3-edge/opposite-pair anchor alone isn't proof of
        // a flush slab: wlr-layer-shell's own `exclusive-zone` semantics carry
        // that distinction already. `ExclusiveZone::DontCare` (or no
        // reservation at all) means "extend it all the way to the edges it is
        // anchored to" in the protocol's own words — a genuine flush edge, so
        // it stays square. A *positive* exclusive zone instead asks the
        // compositor to reserve a bounded strip, which is what a taskbar does
        // — hyperion anchors top+left+right for layout (`size: (0, HEIGHT)`
        // stretches it edge to edge) but reserves only `HEIGHT` px and draws a
        // floating rounded pill inset within that strip, not a flush bar. No
        // other layer client anchors 3+ edges with a positive exclusive zone
        // today, so this shape structurally identifies the bar without a
        // namespace/app-id check, and gets its own radius since its content
        // draws at a different radius than every other pane's.
        let radius = match &key {
            blur::BlurKey::Window(_) => deco.rounding,
            blur::BlurKey::Layer(surface) => {
                let state = surface.cached_state();
                let anchor = state.anchor;
                let opposite_pair = (anchor.contains(Anchor::LEFT) && anchor.contains(Anchor::RIGHT))
                    || (anchor.contains(Anchor::TOP) && anchor.contains(Anchor::BOTTOM));
                let edges = [Anchor::LEFT, Anchor::RIGHT, Anchor::TOP, Anchor::BOTTOM]
                    .into_iter()
                    .filter(|edge| anchor.contains(*edge))
                    .count();
                let bounded_strip = matches!(state.exclusive_zone, ExclusiveZone::Exclusive(z) if z > 0);
                if edges >= 3 || opposite_pair {
                    if bounded_strip {
                        config.bar.rounding as i32
                    } else {
                        0
                    }
                } else {
                    deco.rounding
                }
            }
        };

        let rounding = (radius > 0)
            .then_some(())
            .and(fb_height)
            .and_then(|(fb_height, mirrored)| {
                if store.rounded.is_none() {
                    match effects::compile_rounded(renderer) {
                        Ok(program) => store.rounded = Some(program),
                        Err(err) => {
                            tracing::warn!(
                                ?err,
                                "compiling the rounded-corner shader; blur rounding disabled"
                            )
                        }
                    }
                }
                store.rounded.clone().map(|program| {
                    let scaled_radius = radius as f64 * scale.x.max(scale.y);
                    let uniforms =
                        effects::rounding_uniforms(region, fb_height, mirrored, scaled_radius as f32);
                    (program, uniforms)
                })
            });

        let behind = &elements[index..];
        if let Some(element) = store
            .blur
            .element(renderer, output, &key, region, behind, cfg, scale, rounding)
        {
            elements.insert(index, AbyssRenderElement::Blur(element));
        }
    }
}

/// Per-window toplevel elements, front to back, with `decoration` opacity and
/// `dim-inactive` applied (COMP-02 §9), pushed straight onto `out`.
///
/// Each window contributes, topmost first: its dim overlay, its surfaces, its
/// border, its drop shadow, and — spliced in afterwards by `insert_blur` at the
/// index recorded in `blurred` — its blurred backdrop. A window's decorations
/// therefore stack directly under its own surfaces and above every window
/// further back, and its backdrop samples only what lies behind the window,
/// never its own border or shadow.
///
/// Any surface drawn at less than full alpha cannot go to a scanout plane; with
/// no effect configured every surface goes out unwrapped at alpha 1.0.
#[allow(clippy::too_many_arguments)]
fn window_elements(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    store: &mut BorderStore,
    output: &Output,
    config: &Config,
    focus: Option<&Window>,
    out: &mut Vec<AbyssRenderElement>,
    blurred: &mut Vec<BlurRequest>,
) {
    let Some(output_geo) = space.output_geometry(output) else {
        return;
    };
    let scale = Scale::from(output.current_scale().fractional_scale());
    let deco = &config.decoration;
    let live: Vec<Window> = space.elements().cloned().collect();
    if deco.dim_inactive > 0.0 {
        store.dims.retain(|w, _| live.contains(w));
    } else {
        store.dims.clear();
    }

    // Rounding masks in framebuffer (`gl_FragCoord`) space, so it needs the
    // output's own height and the sense of its vertical axis under the output
    // transform; see `effects::fb_y_mirrored` (COMP-02 §9).
    let fb_height = effects::fb_y_mirrored(output.current_transform())
        .zip(output.current_mode())
        .filter(|_| deco.rounding > 0)
        .map(|(mirrored, mode)| (mode.size.h, mirrored));
    if fb_height.is_some() && store.rounded.is_none() {
        match effects::compile_rounded(renderer) {
            Ok(program) => store.rounded = Some(program),
            Err(err) => tracing::warn!(?err, "compiling the rounded-corner shader; rounding disabled"),
        }
    }
    let rounding = fb_height.zip(store.rounded.clone());
    let border = border_frame(renderer, store, output_geo.loc, scale, output, config, &live);
    let shadow = shadow_program(renderer, store, config, &live);

    // `space.elements()` is bottom-to-top; frames are collected front-to-back.
    for window in live.into_iter().rev() {
        let Some(loc) = space.element_location(&window) else {
            continue;
        };
        // A tiled client that ignored its configure (Electron holds its own
        // min width) is cut back to its tile rather than drawn under its
        // neighbour. Only a window that actually overhangs is cropped, so one
        // that fits keeps its plain elements and its scanout path.
        let clip = crate::shell::tile_clip(&window).filter(|c| {
            let g = window.geometry().size;
            g.w > c.size.w || g.h > c.size.h
        });
        let geo = space.element_geometry(&window).map(|mut geo| {
            geo.loc += store.anim.offset(&window);
            if let Some(c) = clip {
                geo.size.w = geo.size.w.min(c.size.w);
                geo.size.h = geo.size.h.min(c.size.h);
            }
            geo
        });
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
            if let Some(geo) = geo {
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
        // Popups are split off a cropped window so a menu that opens past the
        // tile edge is not cut; smithay's `render_elements` puts them first.
        let (popups, surfaces) = match (clip, window.toplevel()) {
            (Some(_), Some(toplevel)) => {
                let surface = toplevel.wl_surface();
                let popups = PopupManager::popups_for_surface(surface)
                    .flat_map(|(popup, popup_offset)| {
                        let offset = (window.geometry().loc + popup_offset - popup.geometry().loc)
                            .to_physical_precise_round(scale);
                        render_elements_from_surface_tree(
                            renderer,
                            popup.wl_surface(),
                            phys(render_loc, scale) + offset,
                            scale,
                            alpha,
                            Kind::Unspecified,
                        )
                    })
                    .collect();
                let own = render_elements_from_surface_tree(
                    renderer,
                    surface,
                    phys(render_loc, scale),
                    scale,
                    alpha,
                    Kind::Unspecified,
                );
                (popups, own)
            }
            _ => (
                Vec::new(),
                window.render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(
                    renderer,
                    phys(render_loc, scale),
                    scale,
                    alpha,
                ),
            ),
        };
        let crop = clip.map(|c| {
            Rectangle::new(
                phys(c.loc + store.anim.offset(&window) - output_geo.loc, scale),
                c.size.to_f64().to_physical(scale).to_i32_round(),
            )
        });

        // Every surface of one window is masked by the same rectangle, so a
        // window with subsurfaces rounds as a single shape.
        match (&rounding, geo) {
            (Some(((fb_height, mirrored), program)), Some(geo)) => {
                let rect = Rectangle::new(
                    phys(geo.loc - output_geo.loc, scale),
                    geo.size.to_f64().to_physical(scale).to_i32_round(),
                );
                let radius = deco.rounding as f64 * scale.x.max(scale.y);
                let uniforms = effects::rounding_uniforms(rect, *fb_height, *mirrored, radius as f32);
                let round =
                    |surface| effects::RoundedElement::new(surface, program.clone(), uniforms.clone());
                // Unmasked: the mask follows the tile, and would cut the popup too.
                out.extend(popups.into_iter().map(AbyssRenderElement::Surface));
                match crop {
                    Some(crop) => out.extend(
                        surfaces
                            .into_iter()
                            .filter_map(|s| CropRenderElement::from_element(round(s), scale, crop))
                            .map(AbyssRenderElement::CroppedRounded),
                    ),
                    None => out.extend(
                        surfaces
                            .into_iter()
                            .map(|s| AbyssRenderElement::Rounded(round(s))),
                    ),
                }
            }
            _ => {
                out.extend(popups.into_iter().map(AbyssRenderElement::Surface));
                match crop {
                    Some(crop) => out.extend(
                        surfaces
                            .into_iter()
                            .filter_map(|s| CropRenderElement::from_element(s, scale, crop))
                            .map(AbyssRenderElement::Cropped),
                    ),
                    None => out.extend(surfaces.into_iter().map(AbyssRenderElement::Surface)),
                }
            }
        }

        let Some(geo) = geo else {
            continue;
        };
        // Border then shadow, both directly under this window's surfaces. The
        // ring's inner edge is the exact complement of the window's rounded
        // mask, so it never covers window content from below either.
        if let Some(frame) = &border {
            push_border(store, frame, &window, geo, config, out);
        }
        if let Some(program) = &shadow {
            push_shadow(store, program, &window, geo, output_geo.loc, config, out);
        }

        // Blur samples what shows through the window's alpha, so a window drawn
        // at full opacity gets no backdrop pass at all. A matched `windowrule
        // "blur …"` overrides the global default, but never bypasses the
        // alpha gate above: an opaque window is never blurred. The backdrop
        // goes below the window's own border and shadow, so neither is
        // smeared into it; it covers the window rect only, which the ring
        // does not overlap.
        let blur_wanted =
            alpha < 1.0 && crate::shell::rules::blur_of(&window).unwrap_or(config.decoration.blur.enabled);
        if blur_wanted {
            blurred.push((
                blur::BlurKey::Window(window.clone()),
                out.len(),
                Rectangle::new(
                    phys(geo.loc - output_geo.loc, scale),
                    geo.size.to_f64().to_physical(scale).to_i32_round(),
                ),
            ));
        }
    }
}

/// The drop-shadow program when shadows are on, compiled on first use, with
/// the stored shadows of windows that are gone dropped (COMP-02 §9); `None`
/// draws no shadows.
fn shadow_program(
    renderer: &mut GlesRenderer,
    store: &mut BorderStore,
    config: &Config,
    live: &[Window],
) -> Option<smithay::backend::renderer::gles::GlesPixelProgram> {
    let shadow = &config.decoration.shadow;
    if !shadow.enabled || shadow.range <= 0 {
        store.shadows.clear();
        return None;
    }
    store.shadows.retain(|w, _| live.contains(w));
    if store.shadow.is_none() {
        match effects::compile_shadow(renderer) {
            Ok(program) => store.shadow = Some(program),
            Err(err) => {
                tracing::warn!(?err, "compiling the shadow shader; shadows disabled");
                return None;
            }
        }
    }
    store.shadow.clone()
}

/// Where one window's drop shadow goes and what it is shaded with: its
/// output-local area, grown from the bordered rect by `range`, and its
/// `[range, radius]` uniforms. `geo` is global with the animation offset
/// applied (COMP-02 §9).
fn shadow_geometry(
    geo: Rectangle<i32, Logical>,
    output_loc: Point<i32, Logical>,
    config: &Config,
) -> (Rectangle<i32, Logical>, [f32; 2]) {
    // The shadow sits outside the border, so it grows from the bordered rect.
    let inset = config.general.border_size;
    let range = config.decoration.shadow.range;
    // It hugs the border ring's outer edge, whose radius is the window's plus
    // the border width (see `push_border`); square stays square.
    let radius = match config.decoration.rounding {
        r if r > 0 => r + inset.max(0),
        _ => 0,
    };
    let area = Rectangle::new(
        (
            geo.loc.x - output_loc.x - inset - range,
            geo.loc.y - output_loc.y - inset - range,
        )
            .into(),
        (geo.size.w + 2 * (inset + range), geo.size.h + 2 * (inset + range)).into(),
    );
    (area, [range as f32, radius as f32])
}

/// One window's drop shadow. The stored element is reused so its Id, and with
/// it damage tracking, is stable across frames: `resize` only bumps its commit
/// when the area actually changes, and the uniforms are only replaced when
/// range or radius do.
fn push_shadow(
    store: &mut BorderStore,
    program: &smithay::backend::renderer::gles::GlesPixelProgram,
    window: &Window,
    geo: Rectangle<i32, Logical>,
    output_loc: Point<i32, Logical>,
    config: &Config,
    out: &mut Vec<AbyssRenderElement>,
) {
    let (area, params) = shadow_geometry(geo, output_loc, config);
    let uniforms = || effects::shadow_uniforms(params[0], params[1]);
    let (element, applied) = store.shadows.entry(window.clone()).or_insert_with(|| {
        (
            PixelShaderElement::new(program.clone(), area, None, 1.0, uniforms(), Kind::Unspecified),
            params,
        )
    });
    element.resize(area, None);
    if *applied != params {
        element.update_uniforms(uniforms());
        *applied = params;
    }
    out.push(AbyssRenderElement::Shader(element.clone()));
}

/// Per-frame border state shared by every window on one output.
struct BorderFrame {
    width: i32,
    radius: i32,
    output_loc: Point<i32, Logical>,
    scale: Scale<f64>,
    /// The ring program when the window is rounded; `None` draws four quads.
    ring: Option<smithay::backend::renderer::gles::GlesPixelProgram>,
}

/// Set up this frame's borders and drop the stored ones of windows that are
/// gone; `None` when `border-size` is off (COMP-02 §9).
fn border_frame(
    renderer: &mut GlesRenderer,
    store: &mut BorderStore,
    output_loc: Point<i32, Logical>,
    scale: Scale<f64>,
    output: &Output,
    config: &Config,
    live: &[Window],
) -> Option<BorderFrame> {
    let width = config.general.border_size;
    if width <= 0 {
        store.borders.clear();
        store.rings.clear();
        return None;
    }
    store.borders.retain(|w, _| live.contains(w));
    store.rings.retain(|w, _| live.contains(w));

    // The ring only exists where `window_elements` actually rounds the window:
    // same radius, and the same fallback to square corners on an output whose
    // transform the mask does not handle (`effects::fb_y_mirrored`), so the
    // border and the window it frames never disagree.
    let radius = config.decoration.rounding;
    let rounds = radius > 0
        && effects::fb_y_mirrored(output.current_transform()).is_some()
        && output.current_mode().is_some();
    if rounds && store.ring.is_none() {
        match effects::compile_border(renderer) {
            Ok(program) => store.ring = Some(program),
            Err(err) => tracing::warn!(?err, "compiling the border-ring shader; square borders"),
        }
    }
    let ring = store.ring.clone().filter(|_| rounds);
    if ring.is_some() {
        store.borders.clear();
    } else {
        store.rings.clear();
    }
    Some(BorderFrame {
        width,
        radius,
        output_loc,
        scale,
        ring,
    })
}

/// One window's border: a single ring whose inner edge follows the window's
/// rounded corners, or four solid quads (COMP-02 §9). `geo` is global with the
/// animation offset applied. The stored element is reused so its Id, and with
/// it damage tracking, is stable across frames.
fn push_border(
    store: &mut BorderStore,
    frame: &BorderFrame,
    window: &Window,
    geo: Rectangle<i32, Logical>,
    config: &Config,
    out: &mut Vec<AbyssRenderElement>,
) {
    let BorderFrame {
        width,
        radius,
        output_loc,
        scale,
        ref ring,
    } = *frame;
    // The `border` animation crossfades this on focus change; with the
    // animation off it is the focused/unfocused colour outright.
    let color = store
        .anim
        .border_color(window, config.general.col_active, config.general.col_inactive);
    // Outer rect: the tile, with the window inset by `width` on every side.
    let outer = Rectangle::new(
        // Window geometry is global; elements are output-local.
        (geo.loc.x - width - output_loc.x, geo.loc.y - width - output_loc.y).into(),
        (geo.size.w + 2 * width, geo.size.h + 2 * width).into(),
    );
    if let Some(program) = ring {
        let [r, g, b, a] = color;
        let params = [
            r,
            g,
            b,
            a,
            radius as f32,
            width as f32,
            scale.x.max(scale.y) as f32,
        ];
        let uniforms = || effects::border_uniforms(color, params[4], params[5], params[6]);
        let (element, applied) = store.rings.entry(window.clone()).or_insert_with(|| {
            (
                PixelShaderElement::new(program.clone(), outer, None, 1.0, uniforms(), Kind::Unspecified),
                params,
            )
        });
        element.resize(outer, None);
        if *applied != params {
            element.update_uniforms(uniforms());
            *applied = params;
        }
        out.push(AbyssRenderElement::Shader(element.clone()));
        return;
    }
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
        .entry(window.clone())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn config(rounding: i32, border: i32, range: i32) -> Config {
        let mut cfg = Config::default();
        cfg.decoration.rounding = rounding;
        cfg.general.border_size = border;
        cfg.decoration.shadow.range = range;
        cfg
    }

    #[test]
    fn shadow_hugs_the_bordered_rect() {
        let geo = Rectangle::new((100, 50).into(), (300, 200).into());
        let (area, params) = shadow_geometry(geo, (40, 0).into(), &config(13, 6, 20));
        assert_eq!(area, Rectangle::new((34, 24).into(), (352, 252).into()));
        assert_eq!(params, [20.0, 19.0]);
        let (_, params) = shadow_geometry(geo, (40, 0).into(), &config(0, 6, 20));
        assert_eq!(params, [20.0, 0.0], "square windows keep a square shadow");
    }

    #[test]
    fn shadow_is_only_rebuilt_when_its_inputs_change() {
        let geo = Rectangle::new((100, 50).into(), (300, 200).into());
        let at = |geo, output: (i32, i32), cfg: &Config| shadow_geometry(geo, output.into(), cfg);
        let base = config(13, 6, 20);
        let same = at(geo, (0, 0), &base);
        // A static window: nothing changes, so `resize` and `update_uniforms`
        // are both no-ops and the stored element keeps its commit.
        assert_eq!(at(geo, (0, 0), &config(13, 6, 20)), same);

        let moved = Rectangle::new((101, 50).into(), (300, 200).into());
        let resized = Rectangle::new((100, 50).into(), (300, 201).into());
        for (what, got) in [
            ("move", at(moved, (0, 0), &base)),
            ("resize", at(resized, (0, 0), &base)),
            ("output", at(geo, (0, 1), &base)),
            ("rounding", at(geo, (0, 0), &config(8, 6, 20))),
            ("border", at(geo, (0, 0), &config(13, 3, 20))),
            ("range", at(geo, (0, 0), &config(13, 6, 40))),
        ] {
            assert_ne!(got, same, "{what} must update the shadow");
        }
    }
}
