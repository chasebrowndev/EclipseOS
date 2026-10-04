// SPDX-License-Identifier: AGPL-3.0-only
//! Shared render-element collection for every backend (COMP-02 §4).
//!
//! Both the winit and DRM backends build their frame from [`collect_elements`];
//! the stacking order lives here once, not per backend.
//!
//! `capture.rs` is not here: it reads `AbyssState` and is TCB, so it stays in
//! `ec-abyss` (`ec-abyss/src/render/capture.rs`).

pub mod anim;
pub mod annotation;
pub mod blur;
pub mod cursor;
pub mod curve;
pub mod drop;
pub mod effects;
pub mod font;
pub mod hud;
pub mod hud_font;
pub mod output_overscan;
pub mod overscan;
pub mod palette;
pub mod sanitize;
pub mod select;
pub mod stats;
pub mod text;
pub mod userdata;

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
        compositor::{with_states, SurfaceAttributes},
        dmabuf::DmabufFeedback,
        shell::wlr_layer::{Anchor, ExclusiveZone, Layer},
    },
};

use smithay::desktop::space::SpaceElement;

use ec_abyss_config::{BlurMode, Config};

/// Top-left of the popup in global logical coordinates, or `None` if it is
/// gone or has no parent yet.
pub fn popup_location(
    popup: &smithay::wayland::input_method::PopupSurface,
) -> Option<smithay::utils::Point<i32, Logical>> {
    if !popup.alive() {
        return None;
    }
    let parent = popup.get_parent()?;
    let rect = popup.text_input_rectangle();
    let offset: smithay::utils::Point<i32, Logical> = (rect.loc.x, rect.loc.y + rect.size.h).into();
    Some(parent.location.loc + offset)
}

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
    // A tiled window's decorations (dim, border, shadow, glow, backdrop), cut to its tile.
    CroppedSolid=CropRenderElement<SolidColorRenderElement>,
    CroppedShader=CropRenderElement<PixelShaderElement>,
    CroppedBlur=CropRenderElement<blur::BlurElement>,
}

/// A surface that wants a blurred backdrop: what it belongs to, the index in
/// the element list directly below its surfaces, the region it blurs, the
/// mode it is drawn in (never `Off`), the logical corner radius its backdrop
/// is masked with, the window's glass bezel if it has one, and the layer's
/// input-region shape if it has one (Vol 1 §5.2), and the physical rect its
/// backdrop is cut to (a tiled window's tile, a layer's output), if any.
type BlurRequest = (
    blur::BlurKey,
    usize,
    Rectangle<i32, Physical>,
    BlurMode,
    i32,
    Option<Bezel>,
    Option<blur::Shape>,
    Option<Rectangle<i32, Physical>>,
);

/// A window's border drawn as a ring of glass instead of paint (COMP-02 §9).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Bezel {
    /// Ring width, physical px: the border size at the output scale.
    inner: i32,
    /// Nothing shows through the window itself, so only the ring is glass.
    opaque: bool,
    /// The hairline colour, premultiplied: the border colour, crossfading
    /// with focus like the painted border does.
    rim: [f32; 4],
}

/// The bezel width (physical px) a window's border becomes, or `None` when it
/// stays painted: only in `glass` mode, with the glass program available on
/// this output, a border to replace, and a window that is not maximized or
/// fullscreen (those have no border inset to fill).
fn bezel_width(mode: BlurMode, glass: bool, fills_area: bool, border: i32, upscale: f64) -> Option<i32> {
    (mode == BlurMode::Glass && glass && !fills_area && border > 0)
        .then(|| ((border as f64 * upscale).round() as i32).max(1))
}

/// One window's backdrop, if it gets one: region, logical mask radius and
/// the bezel's `(width, opaque)`. `rect` is the window's physical rect,
/// `corner` its physical corner radius and `opaque` its opaque regions (see
/// [`wants_backdrop`]). A bezel always asks — its ring is glass even over an
/// opaque window — for the window rect grown by the bezel, rounded to the
/// outer radius the painted border would have had; square stays square.
type WindowBackdrop = (Rectangle<i32, Physical>, i32, Option<(i32, bool)>);

#[allow(clippy::too_many_arguments)]
fn window_backdrop(
    mode: BlurMode,
    alpha: f32,
    rect: Rectangle<i32, Physical>,
    corner: i32,
    opaque: Vec<Rectangle<i32, Physical>>,
    rounding: i32,
    border: i32,
    bezel: Option<i32>,
) -> Option<WindowBackdrop> {
    let translucent = wants_backdrop(mode, alpha, rect, corner, opaque);
    match bezel {
        Some(inner) if !rect.is_empty() => {
            let radius = if rounding > 0 { rounding + border } else { 0 };
            Some((blur::grow(rect, inner), radius, Some((inner, !translucent))))
        }
        _ => translucent.then_some((rect, rounding, None)),
    }
}

/// Is `window` maximized or fullscreen, filling its area with no border inset?
fn fills_area(window: &Window) -> bool {
    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;
    if let Some(toplevel) = window.toplevel() {
        let states = toplevel.current_state().states;
        return states.contains(State::Maximized) || states.contains(State::Fullscreen);
    }
    window
        .x11_surface()
        .is_some_and(|x| x.is_maximized() || x.is_fullscreen())
}

/// Whether the glass program can draw on `output` this frame: compiled on
/// first use, and never on a rotated output (see [`effects::fb_y_mirrored`]).
fn glass_ready(renderer: &mut GlesRenderer, store: &mut BorderStore, output: &Output) -> bool {
    if effects::fb_y_mirrored(output.current_transform()).is_none() || output.current_mode().is_none() {
        return false;
    }
    if store.glass.is_none() {
        match effects::compile_glass(renderer) {
            Ok(program) => store.glass = Some(program),
            Err(err) => tracing::warn!(?err, "compiling the glass shader; painted borders"),
        }
    }
    store.glass.is_some()
}

/// The corner radius a layer-shell surface's backdrop is masked with.
///
/// A layer-shell surface anchored to three-plus edges, or to both edges of an
/// axis, usually spans that axis corner-to-corner; fewer/adjacent anchors
/// (launcher, toasts, notification centre) get the same radius as a window.
///
/// "Usually" because a 3-edge/opposite-pair anchor alone isn't proof of a
/// flush slab: wlr-layer-shell's own `exclusive-zone` semantics carry that
/// distinction already. `ExclusiveZone::DontCare` (or no reservation at all)
/// means "extend it all the way to the edges it is anchored to" in the
/// protocol's own words — a genuine flush edge, so it stays square. A
/// *positive* exclusive zone instead asks the compositor to reserve a bounded
/// strip, which is what a taskbar does — hyperion anchors top+left+right for
/// layout (`size: (0, HEIGHT)` stretches it edge to edge) but reserves only
/// `HEIGHT` px and draws a floating rounded pill inset within that strip, not
/// a flush bar. No other layer client anchors 3+ edges with a positive
/// exclusive zone today, so this shape structurally identifies the bar
/// without a namespace/app-id check, and gets its own radius since its content
/// draws at a different radius than every other pane's.
fn layer_radius(surface: &smithay::desktop::LayerSurface, config: &Config) -> i32 {
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
        config.decoration.rounding
    }
}

/// Four solid quads (top, bottom, left, right) per window.
type Border = [SolidColorBuffer; 4];

/// Border quads kept alive between frames, keyed by output and window: one
/// window can be drawn on several outputs, each at its own geometry.
#[derive(Default)]
pub struct BorderStore {
    borders: HashMap<(Output, Window), Border>,
    /// In-flight window moves (COMP-02 §9).
    pub anim: anim::AnimStore,
    /// One dim-inactive overlay quad per window, kept alive between frames.
    dims: HashMap<(Output, Window), SolidColorBuffer>,
    /// Rounded-corner texture program, compiled on the first frame that rounds.
    rounded: Option<smithay::backend::renderer::gles::GlesTexProgram>,
    /// Frost and glass backdrop programs, compiled on the first frame that
    /// draws a backdrop in that mode.
    frost: Option<smithay::backend::renderer::gles::GlesTexProgram>,
    glass: Option<smithay::backend::renderer::gles::GlesTexProgram>,
    /// The same three (blur, frost, glass) masked to a layer's input-region
    /// shape (Vol 1 §5.2), compiled on the first frame that draws one.
    shaped: [Option<smithay::backend::renderer::gles::GlesTexProgram>; 3],
    /// Drop-shadow pixel program, compiled on the first frame that shadows.
    shadow: Option<smithay::backend::renderer::gles::GlesPixelProgram>,
    /// Glow ring pixel program, compiled on the first frame that glows.
    glow: Option<smithay::backend::renderer::gles::GlesPixelProgram>,
    /// Rounded border-ring program, compiled on the first frame that rounds.
    ring: Option<smithay::backend::renderer::gles::GlesPixelProgram>,
    /// One rounded border ring per window when rounding is on, kept alive
    /// between frames so its damage is tracked; the last-applied colour,
    /// radius, width and scale are kept to skip no-op uniform updates.
    rings: HashMap<(Output, Window), (PixelShaderElement, [f32; 7])>,
    /// One drop shadow per window when shadows are on, kept alive between
    /// frames for the same reason; the last-applied range, radius, drop and
    /// focus weight are kept to skip no-op uniform updates.
    shadows: HashMap<(Output, Window), (PixelShaderElement, [f32; 4])>,
    /// One border glow per window when glow is on, drawn by the ring
    /// program; the last-applied colour, range and radius are kept to skip
    /// no-op uniform updates.
    glows: HashMap<(Output, Window), (PixelShaderElement, [f32; 6])>,
    /// Blur chain, programs and per-window backdrops (COMP-02 §9), one store
    /// per output so one output's retain, clear or chain size never touches
    /// another's (BLUR-04).
    blur: HashMap<Output, blur::BlurStore>,
}

impl BorderStore {
    pub fn remove(&mut self, window: &Window) {
        self.borders.retain(|(_, w), _| w != window);
        self.rings.retain(|(_, w), _| w != window);
        self.shadows.retain(|(_, w), _| w != window);
        self.glows.retain(|(_, w), _| w != window);
        self.dims.retain(|(_, w), _| w != window);
    }
}

fn phys(p: Point<i32, Logical>, scale: Scale<f64>) -> Point<i32, Physical> {
    p.to_f64().to_physical(scale).to_i32_round()
}

/// The opaque regions of a surface tree's elements, output-local physical.
/// Each element reports them relative to its own location.
fn opaque_of(
    els: &[WaylandSurfaceRenderElement<GlesRenderer>],
    scale: Scale<f64>,
) -> Vec<Rectangle<i32, Physical>> {
    els.iter()
        .flat_map(|e| {
            let at = e.geometry(scale).loc;
            e.opaque_regions(scale)
                .into_iter()
                .map(move |r| Rectangle::new(r.loc + at, r.size))
        })
        .collect()
}

/// The window blur gate (COMP-02 §9): a window gets a backdrop when blur is
/// on and anything shows through it, either because it is drawn below full
/// alpha (a fade or an opacity rule) or because its opaque region leaves some
/// of `region` uncovered. `radius` is the physical corner radius it is masked
/// with (see [`blur::shows_through`]). An alpha-less buffer is wholly opaque
/// and never pays for a pass.
fn wants_backdrop(
    mode: BlurMode,
    alpha: f32,
    region: Rectangle<i32, Physical>,
    radius: i32,
    opaque: Vec<Rectangle<i32, Physical>>,
) -> bool {
    mode != BlurMode::Off
        && !region.is_empty()
        && (alpha < 1.0 || blur::shows_through(region, radius, opaque))
}

/// Collect one frame's elements, front to back.
///
/// Order (topmost first) follows COMP-02 §4: overlay layer surfaces, top layer
/// surfaces, toplevels (each followed by its own border, glow, shadow and
/// blurred backdrop), then bottom and background layers.
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
    let output_geo = space.output_geometry(output).unwrap_or_default();
    let output_loc = output_geo.loc;
    let mut elements: Vec<AbyssRenderElement> = Vec::new();
    // (window, index in `elements` directly below its surfaces, region).
    // Filled front to back; `insert_blur` consumes it in reverse.
    let mut blur_requests: Vec<BlurRequest> = Vec::new();

    // Layers have no rules: they take the global mode.
    let layer_mode = config.decoration.blur.mode;
    let blur_layers = layer_mode != BlurMode::Off;
    // A layer surface bigger than its output (or hanging off it) is cut to it,
    // so it never draws onto a neighbouring output's area of the shared space.
    let output_rect = Rectangle::new(
        Point::from((0, 0)),
        output_geo.size.to_f64().to_physical(scale).to_i32_round(),
    );
    let layers = |elements: &mut Vec<AbyssRenderElement>,
                  requests: &mut Vec<BlurRequest>,
                  which: &[Layer],
                  renderer: &mut GlesRenderer| {
        let map = layer_map_for_output(output);
        for &layer in which {
            for surface in map.layers_on(layer).rev() {
                let Some(geo) = userdata::layer_geometry(&map, surface) else {
                    continue;
                };
                // Layer geometry is already output-local.
                let loc = phys(geo.loc, scale);
                let layer_rect = Rectangle::new(loc, geo.size.to_f64().to_physical(scale).to_i32_round());
                let crop = (!output_rect.contains_rect(layer_rect)).then_some(output_rect);
                let els = surface
                    .render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(renderer, loc, scale, 1.0);
                // Vol 1 §5.2 asks for blur behind layer-shell too. A layer
                // client declares its own translucency through the surface's
                // opaque region; anything it leaves uncovered is glass and
                // wants a backdrop. COMP-02 §9: opaque surfaces are skipped
                // entirely, so a bar that paints a solid ground costs nothing.
                // *(C-17)* Nor does a surface anchored to all four edges and
                // as big as the output: that is a scrim or a selection overlay
                // (slurp), not a sheet, and glass behind it would smear the
                // whole output it asks the human to read. A sized sheet merely
                // centred by those anchors still gets its glass.
                let scrim =
                    surface.cached_state().anchor.contains(Anchor::all()) && geo.size == output_geo.size;
                // Cropping can drop surfaces, so the splice index is counted on
                // what is actually pushed.
                let pushed =
                    |els: Vec<WaylandSurfaceRenderElement<GlesRenderer>>| -> Vec<AbyssRenderElement> {
                        match crop {
                            Some(crop) => els
                                .into_iter()
                                .filter_map(|e| CropRenderElement::from_element(e, scale, crop))
                                .map(AbyssRenderElement::Cropped)
                                .collect(),
                            None => els.into_iter().map(AbyssRenderElement::Surface).collect(),
                        }
                    };
                if blur_layers && !scrim {
                    let region = Rectangle::new(loc, geo.size.to_f64().to_physical(scale).to_i32_round());
                    // Vol 1 §5.2: a layer whose input region is 2..=4 boxes
                    // (a bar pill and the panel hanging off it) gets glass in
                    // that shape, joined by `bar.rounding` fillets; any other
                    // region keeps the single rounded box.
                    let fillet = (config.bar.rounding as f64 * scale.x.max(scale.y)) as f32;
                    let shape = with_states(surface.wl_surface(), |states| {
                        let mut attrs = states.cached_state.get::<SurfaceAttributes>();
                        blur::Shape::from_region(
                            attrs.current().input_region.as_ref(),
                            geo.size,
                            loc,
                            scale,
                            fillet,
                        )
                    });
                    let opaque = opaque_of(&els, scale);
                    let shows = match &shape {
                        Some(shape) => blur::shape_shows_through(shape, &opaque),
                        None => blur::shows_through(region, 0, opaque),
                    };
                    if shows {
                        let els = pushed(els);
                        requests.push((
                            blur::BlurKey::Layer(surface.clone()),
                            elements.len() + els.len(),
                            region,
                            layer_mode,
                            layer_radius(surface, config),
                            None,
                            shape,
                            crop,
                        ));
                        elements.extend(els);
                        continue;
                    }
                }
                elements.extend(pushed(els));
            }
        }
    };

    // The input-method popup sits above everything the shell draws (COMP-06 §1).
    if let Some((popup, loc)) = im_popup.and_then(|p| popup_location(p).map(|l| (p, l))) {
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
    if requests.is_empty() {
        // Nothing asked for a backdrop (blur `off`, or nothing translucent):
        // the chain must not sit in GPU memory for a frame that draws none.
        store.blur.remove(output);
        return;
    }
    let scale = Scale::from(output.current_scale().fractional_scale());
    let live: Vec<blur::BlurKey> = requests.iter().map(|(key, ..)| key.clone()).collect();
    store.blur.entry(output.clone()).or_default().retain(&live);

    // Same framebuffer-space mask as `window_elements`: needs the output's own
    // height and the sense of its vertical axis (COMP-02 §9).
    let fb_height = effects::fb_y_mirrored(output.current_transform())
        .zip(output.current_mode())
        .map(|(mirrored, mode)| (mode.size.h, mirrored));
    // The backdrop texture is the whole output in physical pixels.
    let fb_size = output
        .current_mode()
        .map(|mode| output.current_transform().transform_size(mode.size))
        .map(|size| (size.w, size.h))
        .unwrap_or((1, 1));
    let upscale = scale.x.max(scale.y);

    for (key, index, region, mode, radius, bezel, shape, crop) in requests.into_iter().rev() {
        // The final draw's program: plain `blur` only needs one to round
        // (a square backdrop draws with smithay's own); frost and glass
        // always do, radius 0 included. A shaped layer (Vol 1 §5.2) takes
        // the shaped variant of its mode's program, plain `blur` included.
        // A rotated output (`fb_height` None) keeps the plain square
        // backdrop in every mode.
        let scaled_radius = (radius as f64 * upscale) as f32;
        let program = fb_height.and_then(|(fb_height, mirrored)| {
            let (slot, compile, what): (_, fn(&mut GlesRenderer) -> _, _) = match (mode, shape.is_some()) {
                (BlurMode::Off, _) => return None,
                (BlurMode::Blur, false) if radius <= 0 => return None,
                (BlurMode::Blur, false) => (&mut store.rounded, effects::compile_rounded, "rounded-corner"),
                (BlurMode::Frost, false) => (&mut store.frost, effects::compile_frost, "frost"),
                (BlurMode::Glass, false) => (&mut store.glass, effects::compile_glass, "glass"),
                (BlurMode::Blur, true) => (
                    &mut store.shaped[0],
                    effects::compile_rounded_shaped,
                    "shaped rounded-corner",
                ),
                (BlurMode::Frost, true) => (
                    &mut store.shaped[1],
                    effects::compile_frost_shaped,
                    "shaped frost",
                ),
                (BlurMode::Glass, true) => (
                    &mut store.shaped[2],
                    effects::compile_glass_shaped,
                    "shaped glass",
                ),
            };
            if slot.is_none() {
                match compile(renderer) {
                    Ok(program) => *slot = Some(program),
                    Err(err) => {
                        tracing::warn!(?err, shader = what, "compiling a blur shader; plain backdrop")
                    }
                }
            }
            let program = slot.clone()?;
            let mut uniforms = match mode {
                BlurMode::Frost => {
                    effects::frost_uniforms(region, fb_height, mirrored, scaled_radius, cfg.frost.tint)
                }
                BlurMode::Glass => {
                    let refraction = (cfg.glass.refraction as f64 * upscale) as f32;
                    // A bezel lenses gently: at most 4 px, across the ring only.
                    let (strength, bevel) = match bezel {
                        Some(b) => (
                            refraction.min((BEZEL_REFRACTION * upscale) as f32),
                            b.inner as f32,
                        ),
                        None => (refraction, (cfg.glass.bevel as f64 * upscale) as f32),
                    };
                    effects::glass_uniforms(
                        region,
                        fb_height,
                        mirrored,
                        scaled_radius,
                        fb_size,
                        strength,
                        bevel,
                        &cfg.glass,
                        bezel.map(|b| (b.inner, b.opaque, b.rim)),
                    )
                }
                _ => effects::rounding_uniforms(region, fb_height, mirrored, scaled_radius),
            };
            // The boxes ride in the uniforms, so a new shape changes the
            // backdrop's `look` and bumps its commit.
            if let Some(shape) = &shape {
                uniforms.extend(effects::shape_uniforms(shape, fb_height, mirrored));
            }
            Some((program, uniforms))
        });
        // COMP-02 §3: refraction samples up to this far outside the region,
        // so damage that far out must invalidate the backdrop too.
        let glass = mode == BlurMode::Glass && program.is_some();
        let reach = if glass {
            effects::glass_reach(cfg.glass.refraction, upscale)
        } else {
            0
        };

        // Over an opaque window only the bezel's ring is ever seen.
        let ring = bezel.filter(|b| b.opaque && glass).map(|b| {
            (
                b.inner,
                (config.decoration.rounding.max(0) as f64 * upscale).ceil() as i32,
            )
        });
        let behind = &elements[index..];
        if let Some(element) = store.blur.entry(output.clone()).or_default().element(
            renderer,
            output,
            &key,
            region,
            behind,
            cfg,
            scale,
            program,
            reach,
            ring,
            shape.as_ref(),
            1.0,
        ) {
            let element = match crop {
                Some(crop) => {
                    CropRenderElement::from_element(element, scale, crop).map(AbyssRenderElement::CroppedBlur)
                }
                None => Some(AbyssRenderElement::Blur(element)),
            };
            if let Some(element) = element {
                elements.insert(index, element);
            }
        }
    }
}

/// A bezel's largest refraction, logical px: felt at the window edge, never a
/// funhouse.
const BEZEL_REFRACTION: f64 = 4.0;

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
    // Only windows that reach this output get any decoration work: one that
    // sits on another output must not paint its backdrop, shadow, glow, border
    // or dim here.
    let live = windows_on_output(
        space,
        output_geo,
        // A window not yet laid out has no owner and is drawn wherever it lands.
        |w| userdata::owner_output(w).is_none_or(|o| &o == output),
        |w| store.anim.offset(w),
    );
    // The output's own rectangle, output-local physical: nothing of a window
    // (surface tree, popups, decorations) is ever drawn outside it.
    let out_rect = Rectangle::new(
        Point::from((0, 0)),
        output_geo.size.to_f64().to_physical(scale).to_i32_round(),
    );
    if deco.dim_inactive > 0.0 {
        store.dims.retain(|(o, w), _| o != output || live.contains(w));
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
    let shadow = shadow_program(renderer, store, output, config, &live);
    let glow = glow_program(renderer, store, output, config, &live);
    let upscale = scale.x.max(scale.y);
    // Compiled (and checked) only once some window actually wants a bezel.
    let mut glass: Option<bool> = None;

    // `space.elements()` is bottom-to-top; frames are collected front-to-back.
    for window in live.into_iter().rev() {
        let Some(loc) = space.element_location(&window) else {
            continue;
        };
        // A tiled client that ignored its configure (Electron holds its own
        // min width) is cut back to its tile rather than drawn under its
        // neighbour. Only a window that actually overhangs is cropped, so one
        // that fits keeps its plain elements and its scanout path.
        let clip = userdata::tile_clip(&window).filter(|c| {
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
        // Decorations are cut to the whole tile (the window plus its border),
        // whether or not the client overhangs it: unlike a surface, a shadow or
        // backdrop would otherwise bleed into the neighbouring tile.
        let deco_crop = Some(
            userdata::tile_clip(&window)
                .map(|c| {
                    let width = config.general.border_size.max(0);
                    Rectangle::new(
                        phys(c.loc + store.anim.offset(&window) - output_geo.loc, scale)
                            - Point::from((width, width))
                                .to_f64()
                                .to_physical(scale)
                                .to_i32_round(),
                        (c.size + (2 * width, 2 * width).into())
                            .to_f64()
                            .to_physical(scale)
                            .to_i32_round(),
                    )
                })
                .map_or(Some(out_rect), |tile| tile.intersection(out_rect))
                // Wholly outside: an empty rect crops everything away.
                .unwrap_or_default(),
        );
        let active = focus == Some(&window);
        // A matched `windowrule "opacity …"` overrides the global pair.
        let alpha = userdata::opacity_of(&window).unwrap_or(if active {
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
                    .entry((output.clone(), window.clone()))
                    .or_insert_with(|| SolidColorBuffer::new(geo.size, color));
                buffer.update(geo.size, color);
                let start = out.len();
                out.push(AbyssRenderElement::Solid(SolidColorRenderElement::from_buffer(
                    buffer,
                    phys(geo.loc - output_geo.loc, scale),
                    scale,
                    1.0,
                    Kind::Unspecified,
                )));
                crop_decor(out, start, scale, deco_crop);
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
        let tile_crop = clip.map(|c| {
            Rectangle::new(
                phys(c.loc + store.anim.offset(&window) - output_geo.loc, scale),
                c.size.to_f64().to_physical(scale).to_i32_round(),
            )
        });
        // A window reaching past its output (any size, any placement) is cut
        // to it; one that fits keeps its plain elements and its scanout path.
        let mut bbox = window.bbox();
        bbox.loc += loc + store.anim.offset(&window) - window.geometry().loc;
        let overhangs = !output_geo.contains_rect(bbox);
        let crop = match (tile_crop, overhangs) {
            (Some(tile), true) => Some(tile.intersection(out_rect).unwrap_or_default()),
            (Some(tile), false) => Some(tile),
            (None, true) => Some(out_rect),
            (None, false) => None,
        };

        // Blur follows what shows through the window, not a whole-window
        // alpha (COMP-02 §9): decided here, on the raw surfaces, before the
        // rounding and crop wrappers (which report no opaque region) consume
        // them. A matched `windowrule "blur …"` overrides the global mode.
        let global = config.decoration.blur.mode;
        let mode = userdata::blur_of(&window).map_or(global, |rule| rule.resolve(global));
        let corner = match rounding {
            Some(_) => (deco.rounding as f64 * scale.x.max(scale.y)).ceil() as i32,
            None => 0,
        };
        let border_size = config.general.border_size;
        let bezel = match border.is_some() && mode == BlurMode::Glass && geo.is_some() {
            true => {
                let ready = *glass.get_or_insert_with(|| glass_ready(renderer, store, output));
                bezel_width(mode, ready, fills_area(&window), border_size, upscale)
            }
            false => None,
        };
        let backdrop = geo
            .filter(|_| mode != BlurMode::Off)
            .and_then(|geo| {
                let rect = Rectangle::new(
                    phys(geo.loc - output_geo.loc, scale),
                    geo.size.to_f64().to_physical(scale).to_i32_round(),
                );
                window_backdrop(
                    mode,
                    alpha,
                    rect,
                    corner,
                    opaque_of(&surfaces, scale),
                    deco.rounding,
                    border_size,
                    bezel,
                )
            })
            .map(|(region, radius, bezel)| {
                let bezel = bezel.map(|(inner, opaque)| {
                    // Config colours are straight alpha; premultiply before
                    // the focus crossfade so it stays linear.
                    let pm = |[r, g, b, a]: [f32; 4]| [r * a, g * a, b * a, a];
                    let rim = store.anim.border_color(
                        &window,
                        pm(config.general.col_active),
                        pm(config.general.col_inactive),
                    );
                    Bezel { inner, opaque, rim }
                });
                (region, radius, bezel)
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
                push_popups(out, popups, scale, overhangs.then_some(out_rect));
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
                push_popups(out, popups, scale, overhangs.then_some(out_rect));
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
        // Border, glow, then shadow, all directly under this window's
        // surfaces. The ring's inner edge is the exact complement of the
        // window's rounded mask, so it never covers window content from below
        // either; glow and shadow draw only outside the bordered rect. A glass
        // bezel replaces the painted ring.
        let bezeled = backdrop.as_ref().is_some_and(|(_, _, b)| b.is_some());
        let start = out.len();
        match &border {
            Some(_) if bezeled => {
                store.rings.remove(&(output.clone(), window.clone()));
                store.borders.remove(&(output.clone(), window.clone()));
            }
            Some(frame) => push_border(store, frame, output, &window, geo, config, out),
            None => {}
        }
        if let Some(program) = &glow {
            push_glow(store, program, output, &window, geo, output_geo.loc, config, out);
        }
        if let Some(program) = &shadow {
            push_shadow(store, program, output, &window, geo, output_geo.loc, config, out);
        }
        crop_decor(out, start, scale, deco_crop);

        // An opaque window gets no backdrop pass at all (the gate above)
        // unless its border is a glass bezel. The backdrop goes below the
        // window's own border and shadow, so neither is smeared into it; it
        // covers the window rect only, which the ring does not overlap — or,
        // for a bezel, the bordered rect, which the shadow does not overlap.
        if let Some((region, radius, bezel)) = backdrop {
            blurred.push((
                blur::BlurKey::Window(window.clone()),
                out.len(),
                region,
                mode,
                radius,
                bezel,
                None,
                deco_crop,
            ));
        }
    }
}

/// Windows that `owned` says belong on this output and whose geometry (moved
/// by their animation `offset`) reaches `output_geo`, bottom to top. Everything per-output about a window starts
/// from this list.
fn windows_on_output<E: SpaceElement + PartialEq + Clone>(
    space: &Space<E>,
    output_geo: Rectangle<i32, Logical>,
    owned: impl Fn(&E) -> bool,
    offset: impl Fn(&E) -> Point<i32, Logical>,
) -> Vec<E> {
    space
        .elements()
        .filter(|w| owned(w))
        .filter(|w| {
            space.element_geometry(w).is_some_and(|mut geo| {
                geo.loc += offset(w);
                geo.overlaps(output_geo)
            })
        })
        .cloned()
        .collect()
}

/// A cropped window's popups, cut to the output only when they might leave it.
fn push_popups(
    out: &mut Vec<AbyssRenderElement>,
    popups: Vec<WaylandSurfaceRenderElement<GlesRenderer>>,
    scale: Scale<f64>,
    crop: Option<Rectangle<i32, Physical>>,
) {
    match crop {
        Some(crop) => out.extend(
            popups
                .into_iter()
                .filter_map(|p| CropRenderElement::from_element(p, scale, crop))
                .map(AbyssRenderElement::Cropped),
        ),
        None => out.extend(popups.into_iter().map(AbyssRenderElement::Surface)),
    }
}

/// Cut the decoration elements pushed since `start` to `crop` (a tile),
/// dropping any that fall wholly outside it. `None` leaves them be.
fn crop_decor(
    out: &mut Vec<AbyssRenderElement>,
    start: usize,
    scale: Scale<f64>,
    crop: Option<Rectangle<i32, Physical>>,
) {
    let Some(crop) = crop else {
        return;
    };
    let tail: Vec<AbyssRenderElement> = out.drain(start..).collect();
    out.extend(tail.into_iter().filter_map(|e| match e {
        AbyssRenderElement::Solid(s) => {
            CropRenderElement::from_element(s, scale, crop).map(AbyssRenderElement::CroppedSolid)
        }
        AbyssRenderElement::Shader(s) => {
            CropRenderElement::from_element(s, scale, crop).map(AbyssRenderElement::CroppedShader)
        }
        other => Some(other),
    }));
}

/// The drop-shadow program when shadows are on, compiled on first use, with
/// the stored shadows of windows that are gone dropped (COMP-02 §9); `None`
/// draws no shadows.
fn shadow_program(
    renderer: &mut GlesRenderer,
    store: &mut BorderStore,
    output: &Output,
    config: &Config,
    live: &[Window],
) -> Option<smithay::backend::renderer::gles::GlesPixelProgram> {
    let shadow = &config.decoration.shadow;
    if !shadow.enabled || shadow.range <= 0 {
        store.shadows.clear();
        return None;
    }
    store.shadows.retain(|(o, w), _| o != output || live.contains(w));
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

/// The glow ring program when border glow is on, with the stored glows of
/// windows that are gone dropped (COMP-02 §9); `None` draws no glow.
fn glow_program(
    renderer: &mut GlesRenderer,
    store: &mut BorderStore,
    output: &Output,
    config: &Config,
    live: &[Window],
) -> Option<smithay::backend::renderer::gles::GlesPixelProgram> {
    if !config.decoration.glow.on() {
        store.glows.clear();
        return None;
    }
    store.glows.retain(|(o, w), _| o != output || live.contains(w));
    if store.glow.is_none() {
        match effects::compile_ring(renderer) {
            Ok(program) => store.glow = Some(program),
            Err(err) => {
                tracing::warn!(?err, "compiling the glow shader; glow disabled");
                return None;
            }
        }
    }
    store.glow.clone()
}

/// Where one window's drop shadow or glow goes and what it is shaded with: its
/// output-local area, grown from the bordered rect by `range` and by `drop`
/// more at the bottom, and its `[range, radius, drop]` uniforms. `geo` is
/// global with the animation offset applied (COMP-02 §9). Glow passes
/// `drop = 0`.
fn shadow_geometry(
    geo: Rectangle<i32, Logical>,
    output_loc: Point<i32, Logical>,
    range: i32,
    drop: i32,
    config: &Config,
) -> (Rectangle<i32, Logical>, [f32; 3]) {
    // The shadow sits outside the border, so it grows from the bordered rect.
    let inset = config.general.border_size;
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
        (
            geo.size.w + 2 * (inset + range),
            geo.size.h + 2 * (inset + range) + drop,
        )
            .into(),
    );
    (area, [range as f32, radius as f32, drop as f32])
}

/// How far a shadow of `range` drops below its window, logical px.
fn shadow_drop(range: i32) -> i32 {
    (range as f32 * effects::SHADOW_DROP).round() as i32
}

/// One window's drop shadow. The stored element is reused so its Id, and with
/// it damage tracking, is stable across frames: `resize` only bumps its commit
/// when the area actually changes, and the uniforms are only replaced when
/// range, radius or the focus weight do. Focus deepens the shadow, following
/// the border's crossfade; the area is sized for the deepest case, so a
/// focus change never resizes it.
#[allow(clippy::too_many_arguments)]
fn push_shadow(
    store: &mut BorderStore,
    program: &smithay::backend::renderer::gles::GlesPixelProgram,
    output: &Output,
    window: &Window,
    geo: Rectangle<i32, Logical>,
    output_loc: Point<i32, Logical>,
    config: &Config,
    out: &mut Vec<AbyssRenderElement>,
) {
    let range = config.decoration.shadow.range;
    let (area, geometry) = shadow_geometry(geo, output_loc, range, shadow_drop(range), config);
    let focus = store.anim.border_color(window, [1.0, 0.0, 0.0, 0.0], [0.0; 4])[0];
    let params = [geometry[0], geometry[1], geometry[2], focus];
    let uniforms = || effects::shadow_uniforms(geometry, focus);
    let (element, applied) = store
        .shadows
        .entry((output.clone(), window.clone()))
        .or_insert_with(|| {
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

/// One window's border glow: a ring in the window's border colour, reaching `GLOW_RANGE` past the border (COMP-02 §9). It follows the
/// border's focus crossfade, and a state with glow off crossfades to
/// transparent, so glow fades in or out with focus. Stored and updated like
/// `push_shadow`.
#[allow(clippy::too_many_arguments)]
fn push_glow(
    store: &mut BorderStore,
    program: &smithay::backend::renderer::gles::GlesPixelProgram,
    output: &Output,
    window: &Window,
    geo: Rectangle<i32, Logical>,
    output_loc: Point<i32, Logical>,
    config: &Config,
    out: &mut Vec<AbyssRenderElement>,
) {
    let glow = &config.decoration.glow;
    // Config colours are straight alpha; the shader wants premultiplied, and
    // premultiplying before the crossfade keeps the fade to transparent linear.
    let k = glow.strength as f32 / 100.0;
    let on = |yes: bool, [r, g, b, a]: [f32; 4]| match yes {
        true => [r * a * k, g * a * k, b * a * k, a * k],
        false => [0.0; 4],
    };
    let color = store.anim.border_color(
        window,
        on(glow.active, config.general.col_active),
        on(glow.inactive, config.general.col_inactive),
    );
    let (area, [range, radius, _]) = shadow_geometry(geo, output_loc, effects::GLOW_RANGE, 0, config);
    let params = [color[0], color[1], color[2], color[3], range, radius];
    let uniforms = || effects::ring_uniforms(color, range, radius);
    let (element, applied) = store
        .glows
        .entry((output.clone(), window.clone()))
        .or_insert_with(|| {
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
    store.borders.retain(|(o, w), _| o != output || live.contains(w));
    store.rings.retain(|(o, w), _| o != output || live.contains(w));

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
        store.borders.retain(|(o, _), _| o != output);
    } else {
        store.rings.retain(|(o, _), _| o != output);
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
    output: &Output,
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
        let (element, applied) = store
            .rings
            .entry((output.clone(), window.clone()))
            .or_insert_with(|| {
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
        .entry((output.clone(), window.clone()))
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
    if config.decoration.any_window_effect() || config.decoration.blur.mode != BlurMode::Off {
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
        let (area, params) = shadow_geometry(geo, (40, 0).into(), 20, 6, &config(13, 6, 20));
        // Grown by range + border on every side, and by the drop at the bottom.
        assert_eq!(area, Rectangle::new((34, 24).into(), (352, 258).into()));
        assert_eq!(params, [20.0, 19.0, 6.0]);
        let (_, params) = shadow_geometry(geo, (40, 0).into(), 20, 6, &config(0, 6, 20));
        assert_eq!(params, [20.0, 0.0, 6.0], "square windows keep a square shadow");
        let (area, params) = shadow_geometry(geo, (40, 0).into(), 24, 0, &config(13, 6, 20));
        assert_eq!(
            area,
            Rectangle::new((30, 20).into(), (360, 260).into()),
            "glow does not drop"
        );
        assert_eq!(params[2], 0.0);
        assert_eq!(shadow_drop(20), 6);
    }

    #[test]
    fn shadow_is_only_rebuilt_when_its_inputs_change() {
        let geo = Rectangle::new((100, 50).into(), (300, 200).into());
        let at = |geo, output: (i32, i32), cfg: &Config| {
            let range = cfg.decoration.shadow.range;
            shadow_geometry(geo, output.into(), range, shadow_drop(range), cfg)
        };
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
            ("drop", shadow_geometry(geo, (0, 0).into(), 20, 7, &base)),
        ] {
            assert_ne!(got, same, "{what} must update the shadow");
        }
    }

    fn r(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Physical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    #[test]
    fn an_opaque_window_at_full_alpha_gets_no_backdrop() {
        let region = r(100, 50, 300, 200);
        assert!(!wants_backdrop(BlurMode::Glass, 1.0, region, 0, vec![region]));
        // GTK leaves its rounded corners out; the mask hides them anyway.
        let cross = vec![r(113, 50, 274, 200), r(100, 63, 300, 174)];
        assert!(!wants_backdrop(BlurMode::Glass, 1.0, region, 13, cross));
    }

    #[test]
    fn a_translucent_window_gets_one() {
        let region = r(100, 50, 300, 200);
        // Faded (or an opacity rule): the explicit alpha gate, whatever smithay
        // reports for the opaque region.
        assert!(wants_backdrop(BlurMode::Blur, 0.9, region, 0, vec![region]));
        // An ARGB client that declares no opaque region.
        assert!(wants_backdrop(BlurMode::Glass, 1.0, region, 13, vec![]));
        // A partly opaque one, e.g. a transparent sidebar.
        assert!(wants_backdrop(
            BlurMode::Frost,
            1.0,
            region,
            13,
            vec![r(180, 50, 220, 200)]
        ));
    }

    #[test]
    fn an_opaque_glass_window_with_a_border_gets_a_bezel() {
        let rect = r(100, 50, 300, 200);
        let bezel = bezel_width(BlurMode::Glass, true, false, 6, 1.0);
        assert_eq!(bezel, Some(6));
        let got = window_backdrop(BlurMode::Glass, 1.0, rect, 9, vec![rect], 9, 6, bezel);
        // One request: the bordered rect, the outer radius, opaque inside.
        assert_eq!(got, Some((r(94, 44, 312, 212), 15, Some((6, true)))));
        // A translucent one keeps its glass inside the ring.
        let got = window_backdrop(BlurMode::Glass, 0.8, rect, 9, vec![rect], 9, 6, bezel);
        assert_eq!(got, Some((r(94, 44, 312, 212), 15, Some((6, false)))));
        // Square windows keep a square bezel; HiDPI scales the width.
        let got = window_backdrop(BlurMode::Glass, 1.0, rect, 0, vec![rect], 0, 6, bezel);
        assert_eq!(got.map(|g| g.1), Some(0));
        assert_eq!(bezel_width(BlurMode::Glass, true, false, 6, 1.5), Some(9));
    }

    #[test]
    fn no_bezel_where_the_border_stays_painted() {
        // Maximized/fullscreen, no border, no glass program, other modes.
        assert_eq!(bezel_width(BlurMode::Glass, true, true, 6, 1.0), None);
        assert_eq!(bezel_width(BlurMode::Glass, true, false, 0, 1.0), None);
        assert_eq!(bezel_width(BlurMode::Glass, false, false, 6, 1.0), None);
        for mode in [BlurMode::Off, BlurMode::Blur, BlurMode::Frost] {
            assert_eq!(bezel_width(mode, true, false, 6, 1.0), None);
        }
        // Without a bezel the window rect is unchanged, and opaque asks nothing.
        let rect = r(100, 50, 300, 200);
        assert_eq!(
            window_backdrop(BlurMode::Glass, 1.0, rect, 9, vec![rect], 9, 6, None),
            None
        );
        assert_eq!(
            window_backdrop(BlurMode::Frost, 0.8, rect, 9, vec![rect], 9, 6, None),
            Some((rect, 9, None))
        );
    }

    /// A stand-in toplevel: `Window` needs a live client, the cull does not.
    #[derive(Clone, PartialEq, Debug)]
    struct Fake(u32);
    impl smithay::utils::IsAlive for Fake {
        fn alive(&self) -> bool {
            true
        }
    }
    impl SpaceElement for Fake {
        fn bbox(&self) -> Rectangle<i32, Logical> {
            Rectangle::new((0, 0).into(), (300, 200).into())
        }
        fn is_in_input_region(&self, _: &Point<f64, Logical>) -> bool {
            true
        }
        fn set_activate(&self, _: bool) {}
        fn output_enter(&self, _: &Output, _: Rectangle<i32, Logical>) {}
        fn output_leave(&self, _: &Output) {}
    }

    fn virtual_output(name: &str) -> Output {
        use smithay::output::{Mode, PhysicalProperties, Scale, Subpixel};
        use smithay::utils::Transform;
        let mode = Mode {
            size: (1920, 1080).into(),
            refresh: 60_000,
        };
        let output = Output::new(
            name.to_string(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "abyss".into(),
                model: "virtual".into(),
            },
        );
        output.change_current_state(
            Some(mode),
            Some(Transform::Normal),
            Some(Scale::Integer(1)),
            Some((0, 0).into()),
        );
        output
    }

    #[test]
    fn a_window_on_another_output_gets_no_elements_here() {
        let a = virtual_output("A");
        let b = virtual_output("B");
        let mut space: Space<Fake> = Space::default();
        space.map_output(&a, (0, 0));
        space.map_output(&b, (1920, 300));
        let a_geo = space.output_geometry(&a).unwrap();
        let b_geo = space.output_geometry(&b).unwrap();
        let none = |_: &Fake| Point::from((0, 0));

        space.map_element(Fake(1), (2100, 400), false);
        space.map_element(Fake(2), (1820, 400), false);
        // Fake 1 belongs to B, fake 2 to A.
        let owned_by = |name: &'static str| move |w: &Fake| (w.0 == 1) == (name == "B");
        // Wholly on B.
        let only_one = |w: Fake| move |x: &Fake| *x == w;
        let on = |geo, name, w: Fake| {
            windows_on_output(
                &space,
                geo,
                |x: &Fake| owned_by(name)(x) && only_one(w.clone())(x),
                none,
            )
        };
        assert!(on(a_geo, "A", Fake(1)).is_empty());
        assert_eq!(on(b_geo, "B", Fake(1)), vec![Fake(1)]);

        // Owned by A but hanging 100 px into B's rectangle (300 wide at x=1820,
        // y=400): B draws none of it, A still does.
        assert!(on(b_geo, "B", Fake(2)).is_empty());
        assert_eq!(on(a_geo, "A", Fake(2)), vec![Fake(2)]);
        // The crop rect that bounds it on A is A's own, so nothing lands on B.
        let overhang = space.element_geometry(&Fake(2)).unwrap();
        assert!(!a_geo.contains_rect(overhang) && a_geo.intersection(overhang).is_some());

        // An animation offset can carry an owned window onto this output.
        let slide = |_: &Fake| Point::from((-400, 0));
        assert_eq!(
            windows_on_output(&space, a_geo, |w: &Fake| w.0 == 1, slide).len(),
            1
        );
    }

    #[test]
    fn blur_off_never_asks() {
        let region = r(100, 50, 300, 200);
        assert!(!wants_backdrop(BlurMode::Off, 0.5, region, 0, vec![]));
    }
}
