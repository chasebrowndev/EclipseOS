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
pub mod glass;
pub mod glyphs;
pub mod hud;
pub mod hud_font;
pub mod output_overscan;
pub mod overscan;
pub mod palette;
pub mod popup_shape;
pub mod sanitize;
pub mod select;
pub mod stats;
pub mod still;
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
        ContextId,
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
    utils::{IsAlive, Logical, Physical, Point, Rectangle, Scale, Size},
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
    // A window drawn at an animation scale (open, morph), and one cut to its
    // tile or output; see `anim::ScaledElement`. Only while it animates.
    Scaled=anim::ScaledElement,
    CroppedScaled=CropRenderElement<anim::ScaledElement>,
    // A closed window's last frame, played out as a ghost (`anim::Ghost`).
    ScaledTexture=anim::ScaledElement<smithay::backend::renderer::element::texture::TextureRenderElement<smithay::backend::renderer::gles::GlesTexture>>,
    // A window's offscreen copy drawn through an add-on transition shader
    // (ADR 0073); see `anim::shader`. Only while that run plays.
    Transition=anim::ShaderElement,
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
    /// The hairline colour, premultiplied white, dimmed while unfocused and
    /// crossfading with focus like the painted border does.
    rim: [f32; 4],
}

/// The bezel width (physical px) a window's border becomes, or `None` when it
/// stays painted: only in `glass` mode, with the glass program available on
/// this output, a border to replace, and a window that is not
/// fullscreen (those have no border inset to fill).
fn bezel_width(mode: BlurMode, glass: bool, fullscreen: bool, border: i32, upscale: f64) -> Option<i32> {
    (matches!(mode, BlurMode::Glass | BlurMode::Frost) && glass && !fullscreen && border > 0)
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

/// A layer surface's glass panes waiting for their backdrop (`eclipse_glass_v1`):
/// where they go in the element list (directly below the surface's own
/// elements), the pane list, and the surface's logical size and output-local
/// physical origin they are placed against.
struct PaneRequest {
    key: blur::BlurKey,
    index: usize,
    set: glass::PaneSet,
    size: Size<i32, Logical>,
    loc: Point<i32, Physical>,
    crop: Option<Rectangle<i32, Physical>>,
    /// A window's panes go directly above that window's own backdrop and bezel
    /// (same splice index, inserted after them); a layer's replace its glass.
    over: bool,
    /// The window's animation scale about its pivot (output-local physical),
    /// applied to every pane box; `None` at rest and for layers.
    scaled: Option<(Point<i32, Physical>, Scale<f64>)>,
    /// The window's animation alpha; 1.0 at rest and for layers.
    alpha: f32,
}

impl PaneRequest {
    /// Pane `pane`'s output-local physical box: placed against the surface
    /// origin, carried through the window's animation scale, then cut to the
    /// crop. `None` when it lies wholly outside the surface or the crop.
    fn rect(&self, pane: &glass::Pane, scale: Scale<f64>) -> Option<Rectangle<i32, Physical>> {
        let r = glass::pane_rect(pane, self.size, self.loc, scale)?;
        let r = match self.scaled {
            Some((origin, by)) => anim::track::scale_about(r, origin, by),
            None => r,
        };
        match self.crop {
            Some(crop) => r.intersection(crop),
            None => Some(r),
        }
    }

    /// Whether this set is spliced in before a backdrop request at `index`
    /// is. Both lists are consumed back to front; a set at or below the
    /// request's index goes first, except a window's, which at exactly the
    /// request's index must land above the backdrop it just got.
    fn before(&self, index: usize) -> bool {
        self.index > index || (self.index == index && !self.over)
    }
}

/// Whether a window with a backdrop draws its `eclipse_glass_v1` panes: only
/// in glass mode, on an output the glass program can draw to, and not when the
/// window is keyed-glass (ADR 0080), carries a translucent strip, or is
/// fullscreen — those keep their behaviour as it was.
fn window_takes_panes(mode: BlurMode, keyed: bool, strip: bool, fullscreen: bool, drawable: bool) -> bool {
    mode == BlurMode::Glass && !keyed && !strip && !fullscreen && drawable
}

/// The non-empty pane list `surface` has committed, if it has one.
fn committed_panes(surface: &WlSurface) -> Option<glass::PaneSet> {
    with_states(surface, |states| {
        let mut cached = states.cached_state.get::<glass::GlassState>();
        cached.current().panes.clone().filter(|p| !p.is_empty())
    })
}

/// The `ext_background_effect_v1` blur area `surface` has committed, or `None`
/// when it has no such object (the compositor then picks the backdrop itself).
/// `loc` is the surface origin, output-local physical; `fallback` clips the
/// region when the surface has no size yet.
fn explicit_blur(
    surface: &WlSurface,
    fallback: Size<i32, Logical>,
    loc: Point<i32, Physical>,
    scale: Scale<f64>,
) -> Option<blur::BlurArea> {
    use smithay::backend::renderer::utils::RendererSurfaceStateUserData;
    with_states(surface, |states| {
        let mut cached = states.cached_state.get::<blur::BackgroundEffectState>();
        let region = cached.current().blur.as_ref()?;
        let size = states
            .data_map
            .get::<RendererSurfaceStateUserData>()
            .and_then(|d| d.lock().unwrap().surface_size())
            .unwrap_or(fallback);
        Some(blur::blur_area(region, size, loc, scale))
    })
}

/// `area` drawn at an animation scale about `origin`. A shape is not carried
/// through the animation: it is drawn as its bounds, like a layer's input shape.
fn scale_area(area: blur::BlurArea, origin: Point<i32, Physical>, by: Scale<f64>) -> blur::BlurArea {
    match area.bounds() {
        None => blur::BlurArea::Nothing,
        Some(r) => blur::BlurArea::Rect(anim::track::scale_about(r, origin, by)),
    }
}

/// A window's backdrop when its client asked for exactly this `area`
/// (`ext_background_effect_v1`): the area's box, the logical mask `rounding`
/// and the shape if it has several boxes. Empty areas get none. The client has
/// declared where it is translucent, so the opaque region only vetoes it where
/// it covers the whole area. No bezel: the ring would not be where the client
/// asked for glass. [`explicit_bezel`] is the exception: an area that is the
/// whole window gets the compositor-chosen backdrop and its bezel instead.
fn explicit_window_backdrop(
    area: blur::BlurArea,
    alpha: f32,
    corner: i32,
    opaque: Vec<Rectangle<i32, Physical>>,
    rounding: i32,
) -> Option<(Rectangle<i32, Physical>, i32, Option<blur::Shape>)> {
    let region = area.bounds().filter(|r| !r.is_empty())?;
    let shows = alpha < 1.0
        || match &area {
            blur::BlurArea::Shape(shape) => blur::shape_shows_through(shape, &opaque),
            _ => blur::shows_through(region, corner, opaque),
        };
    shows.then(|| {
        let shape = match area {
            blur::BlurArea::Shape(s) => Some(s),
            _ => None,
        };
        (region, rounding, shape)
    })
}

/// Does a client's explicit `area` take the compositor-chosen backdrop, bezel
/// included? Only an unshaped box that covers the whole window `rect` (a pixel
/// of slack for the area's own rounding) does: the ring then sits where the
/// client asked for glass, and the window looks the same as one that asked for
/// nothing. A partial or shaped area keeps its own box and no bezel.
fn explicit_bezel(area: &blur::BlurArea, rect: Rectangle<i32, Physical>, bezel: Option<i32>) -> bool {
    bezel.is_some() && matches!(area, blur::BlurArea::Rect(r) if blur::grow(*r, 1).contains_rect(rect))
}

/// Is `window` fullscreen? Fullscreen windows are drawn without rounding.
fn is_fullscreen(window: &Window) -> bool {
    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;
    if let Some(toplevel) = window.toplevel() {
        return toplevel.current_state().states.contains(State::Fullscreen);
    }
    window.x11_surface().is_some_and(|x| x.is_fullscreen())
}

/// Whether `mode`'s backdrop program (glass or frost) can draw a bezel on
/// `output` this frame: compiled on first use, and never on a rotated output
/// (see [`effects::fb_y_mirrored`]).
fn bezel_ready(
    renderer: &mut GlesRenderer,
    store: &mut BorderStore,
    output: &Output,
    mode: BlurMode,
) -> bool {
    if effects::fb_y_mirrored(output.current_transform()).is_none() || output.current_mode().is_none() {
        return false;
    }
    let (slot, compile, what): (_, fn(&mut GlesRenderer) -> _, _) = match mode {
        BlurMode::Frost => (&mut store.frost, effects::compile_frost, "frost"),
        _ => (&mut store.glass, effects::compile_glass, "glass"),
    };
    if slot.is_none() {
        match compile(renderer) {
            Ok(program) => *slot = Some(program),
            Err(err) => tracing::warn!(?err, shader = what, "compiling a bezel shader; painted borders"),
        }
    }
    slot.is_some()
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

/// *(C-17)* A surface anchored to all four edges and as big as the output is
/// a scrim or a selection overlay (slurp), not a sheet: glass behind it would
/// smear the whole output it asks the human to read. A sized sheet merely
/// centred by those anchors still gets its glass.
fn is_scrim(
    surface: &smithay::desktop::LayerSurface,
    size: Size<i32, Logical>,
    output: Size<i32, Logical>,
) -> bool {
    surface.cached_state().anchor.contains(Anchor::all()) && size == output
}

/// Vol 1 §5.2: a layer whose input region is 2..=4 boxes (a bar pill and the
/// panel hanging off it) gets glass in that shape, joined by `bar.rounding`
/// fillets; any other region keeps the single rounded box (`None`). `loc` is
/// the surface's output-local physical origin.
fn layer_shape(
    surface: &smithay::desktop::LayerSurface,
    size: Size<i32, Logical>,
    loc: Point<i32, Physical>,
    scale: Scale<f64>,
    config: &Config,
) -> Option<blur::Shape> {
    let fillet = (config.bar.rounding as f64 * scale.x.max(scale.y)) as f32;
    with_states(surface.wl_surface(), |states| {
        let mut attrs = states.cached_state.get::<SurfaceAttributes>();
        blur::Shape::from_region(attrs.current().input_region.as_ref(), size, loc, scale, fillet)
    })
}

/// Whether a layer's glass shows anywhere its opaque region leaves uncovered.
fn layer_shows(
    region: Rectangle<i32, Physical>,
    shape: Option<&blur::Shape>,
    opaque: Vec<Rectangle<i32, Physical>>,
) -> bool {
    match shape {
        Some(shape) => blur::shape_shows_through(shape, &opaque),
        None => blur::shows_through(region, 0, opaque),
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
    /// Tint-keyed texture program (ADR 0079), compiled on the first frame a
    /// keyed strip draws; `Err` once it failed, so it is not retried per frame.
    keyed: Option<Result<smithay::backend::renderer::gles::GlesTexProgram, ()>>,
    /// Frost and glass backdrop programs, compiled on the first frame that
    /// draws a backdrop in that mode.
    frost: Option<smithay::backend::renderer::gles::GlesTexProgram>,
    glass: Option<smithay::backend::renderer::gles::GlesTexProgram>,
    /// The glass program failed to compile; layers with panes keep the
    /// whole-surface glass instead (and it is not retried per frame).
    glass_failed: bool,
    /// Glass pane elements built this frame, kept to reuse the allocation.
    pane_scratch: Vec<blur::BlurElement>,
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

/// One popup of a keyed window: its index, its physical rect (output-local)
/// its backdrop's logical corner radius and its surface elements.
type PopupGroup = (
    usize,
    Rectangle<i32, Physical>,
    i32,
    Vec<WaylandSurfaceRenderElement<GlesRenderer>>,
);

/// The logical corner radius for the glass behind a keyed popup: read off the
/// popup's own texture alpha ([`popup_shape`]), else the window's `fallback`
/// rounding for a texture that cannot be read back (an external-OES import).
fn popup_blur_radius(renderer: &mut GlesRenderer, popup: &smithay::desktop::PopupKind, fallback: i32) -> i32 {
    popup_shape::popup_radius(renderer, popup.wl_surface()).map_or(fallback, |r| r.round() as i32)
}

/// The keyed program and the output's framebuffer axis, if keying can draw on
/// `output` (a rotated output cannot place the key region and keeps the strip
/// opaque). Compiles on first use.
fn keyed_target(
    renderer: &mut GlesRenderer,
    store: &mut BorderStore,
    output: &Output,
) -> Option<(smithay::backend::renderer::gles::GlesTexProgram, i32, bool)> {
    let (mirrored, mode) = effects::fb_y_mirrored(output.current_transform()).zip(output.current_mode())?;
    let program = store.keyed.get_or_insert_with(|| {
        effects::compile_keyed(renderer)
            .map_err(|err| tracing::warn!(?err, "compiling the keyed-glass shader; strip stays opaque"))
    });
    Some((program.clone().ok()?, mode.size.h, mirrored))
}

fn phys(p: Point<i32, Logical>, scale: Scale<f64>) -> Point<i32, Physical> {
    p.to_f64().to_physical(scale).to_i32_round()
}

/// The `translucent-region` strip of a window: the top `top` logical px of its
/// physical `rect`, at the output `scale`, never taller than the window.
pub fn strip_rect(rect: Rectangle<i32, Physical>, top: i32, scale: Scale<f64>) -> Rectangle<i32, Physical> {
    let h = ((top.max(0) as f64 * scale.y).round() as i32).clamp(0, rect.size.h.max(0));
    Rectangle::new(rect.loc, (rect.size.w, h).into())
}

/// The blur region behind a window's strip when the window is otherwise
/// opaque: the strip grown down by the corner radius, so the backdrop's own
/// rounded bottom corners fall under the opaque page and not in the strip.
/// Never past the window. `None` for an empty strip.
pub(crate) fn strip_backdrop(
    strip: Rectangle<i32, Physical>,
    rect: Rectangle<i32, Physical>,
    corner: i32,
) -> Option<Rectangle<i32, Physical>> {
    let h = (strip.size.h + corner.max(0)).min(rect.size.h);
    let region = Rectangle::new(strip.loc, (strip.size.w, h).into());
    (!strip.is_empty() && !region.is_empty()).then_some(region)
}

/// Where a window's surface elements are cut so the strip and the rest can be
/// drawn at different alphas: the output-wide band down to the strip's bottom
/// edge, and the band below it, each cut to `crop` (the tile or output) too.
pub(crate) fn strip_crops(
    strip: Rectangle<i32, Physical>,
    out_rect: Rectangle<i32, Physical>,
    crop: Option<Rectangle<i32, Physical>>,
) -> [Rectangle<i32, Physical>; 2] {
    let edge = strip.loc.y + strip.size.h;
    let above = Rectangle::new(
        out_rect.loc,
        (out_rect.size.w, (edge - out_rect.loc.y).max(0)).into(),
    );
    let below = Rectangle::new(
        (out_rect.loc.x, edge).into(),
        (out_rect.size.w, (out_rect.loc.y + out_rect.size.h - edge).max(0)).into(),
    );
    [above, below].map(|band| match crop {
        Some(c) => band.intersection(c).unwrap_or_default(),
        None => band,
    })
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
    let mut pane_requests: Vec<PaneRequest> = Vec::new();

    // Layers have no rules: they take the global mode.
    let layer_mode = config.decoration.blur.mode;
    let blur_layers = layer_mode != BlurMode::Off;
    // A layer surface bigger than its output (or hanging off it) is cut to it,
    // so it never draws onto a neighbouring output's area of the shared space.
    let output_rect = Rectangle::new(
        Point::from((0, 0)),
        output_geo.size.to_f64().to_physical(scale).to_i32_round(),
    );
    // `eclipse_glass_v1` panes need the glass program; without it the layer
    // keeps its whole-surface glass.
    let panes_ok = layer_mode == BlurMode::Glass && ensure_glass(renderer, borders);
    let layers = |elements: &mut Vec<AbyssRenderElement>,
                  requests: &mut Vec<BlurRequest>,
                  pane_requests: &mut Vec<PaneRequest>,
                  which: &[Layer],
                  anim: &anim::AnimStore,
                  renderer: &mut GlesRenderer| {
        let map = layer_map_for_output(output);
        for &layer in which {
            // A layer surface that has just closed, played out in front of
            // whatever is left on its layer (`layer-close`).
            for ghost in anim.ghosts.iter().rev() {
                let anim::Ghost::Layer {
                    layer: on,
                    surfaces,
                    output: on_output,
                    geometry,
                    ..
                } = ghost
                else {
                    continue;
                };
                if *on != layer || on_output != output {
                    continue;
                }
                let t = anim.ghost_transform(ghost);
                let pivot = anim::track::pivot(&t, *geometry);
                snapshot_elements(
                    elements,
                    surfaces,
                    geometry.loc + t.loc(),
                    scale,
                    pivot.to_physical(scale).to_i32_round(),
                    Scale::from(t.scale),
                    None,
                    t.alpha,
                );
            }
            for surface in map.layers_on(layer).rev() {
                let Some(geo) = userdata::layer_geometry(&map, surface) else {
                    continue;
                };
                // `layer-open`: drawn displaced, scaled and faded while it
                // opens; the identity (and so exactly the plain path) otherwise.
                // Layer geometry is already output-local.
                let t = anim.layer_transform(output, surface);
                let pivot = anim::track::pivot(&t, geo);
                let drawn = t.map(geo, pivot);
                let scaled = t
                    .scaled()
                    .then(|| (pivot.to_physical(scale).to_i32_round(), Scale::from(t.scale)));
                let loc = phys(drawn.loc, scale);
                let layer_rect = Rectangle::new(loc, drawn.size.to_f64().to_physical(scale).to_i32_round());
                let crop = (!output_rect.contains_rect(layer_rect)).then_some(output_rect);
                let render_loc = match scaled {
                    // The scale is about the pivot; the surface is laid out
                    // at its target plus the offset.
                    Some(_) => phys(geo.loc + t.loc(), scale),
                    None => loc,
                };
                let els = surface.render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(
                    renderer, render_loc, scale, t.alpha,
                );
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
                let scrim = is_scrim(surface, geo.size, output_geo.size);
                // Cropping can drop surfaces, so the splice index is counted on
                // what is actually pushed.
                let pushed =
                    |els: Vec<WaylandSurfaceRenderElement<GlesRenderer>>| -> Vec<AbyssRenderElement> {
                        match (scaled, crop) {
                            (Some((origin, by)), crop) => els
                                .into_iter()
                                .filter_map(|e| {
                                    let e = anim::ScaledElement::new(e, origin, by, None);
                                    match crop {
                                        Some(crop) => CropRenderElement::from_element(e, scale, crop)
                                            .map(AbyssRenderElement::CroppedScaled),
                                        None => Some(AbyssRenderElement::Scaled(e)),
                                    }
                                })
                                .collect(),
                            (None, Some(crop)) => els
                                .into_iter()
                                .filter_map(|e| CropRenderElement::from_element(e, scale, crop))
                                .map(AbyssRenderElement::Cropped)
                                .collect(),
                            (None, None) => els.into_iter().map(AbyssRenderElement::Surface).collect(),
                        }
                    };
                // `ext_background_effect_v1`: where the client says its glass
                // is replaces the whole layer rect (and the input-region
                // shape). An empty region is no blur, a scrim included.
                let explicit =
                    explicit_blur(surface.wl_surface(), geo.size, render_loc, scale).map(
                        |area| match scaled {
                            Some((origin, by)) => scale_area(area, origin, by),
                            None => area,
                        },
                    );
                // `eclipse_glass_v1`: a surface with panes gets glass under
                // each pane and none under the whole surface. Only at rest (the
                // panes are laid out at the target) and where the glass program
                // can draw (an unrotated output).
                let panes = (panes_ok && t.is_identity())
                    .then(|| effects::fb_y_mirrored(output.current_transform()))
                    .flatten()
                    .and_then(|_| committed_panes(surface.wl_surface()));
                if let Some(set) = panes {
                    let els = pushed(els);
                    pane_requests.push(PaneRequest {
                        key: blur::BlurKey::Layer(surface.clone()),
                        index: elements.len() + els.len(),
                        set,
                        size: geo.size,
                        loc,
                        crop,
                        over: false,
                        scaled: None,
                        alpha: 1.0,
                    });
                    elements.extend(els);
                    continue;
                }
                let wants = match explicit {
                    Some(area) => area != blur::BlurArea::Nothing,
                    None => !scrim,
                };
                if blur_layers && wants {
                    // The input-region shape is laid out at the target; while
                    // the surface animates, the backdrop is its drawn box.
                    let shape = t
                        .is_identity()
                        .then(|| layer_shape(surface, geo.size, loc, scale, config))
                        .flatten();
                    let (region, shape) = match explicit {
                        Some(blur::BlurArea::Rect(r)) => (r, None),
                        Some(blur::BlurArea::Shape(s)) => (s.bounds().unwrap_or(layer_rect), Some(s)),
                        _ => (layer_rect, shape),
                    };
                    let opaque = match scaled {
                        Some((origin, by)) => opaque_of(&els, scale)
                            .into_iter()
                            .map(|r| anim::track::scale_about(r, origin, by))
                            .collect::<Vec<_>>(),
                        None => opaque_of(&els, scale),
                    };
                    if layer_shows(region, shape.as_ref(), opaque) {
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
    // The shell snapshots closing windows against this context; a renderer
    // rebuilt after a GPU reset hands out a new one.
    let context = smithay::backend::renderer::Renderer::context_id(renderer);
    if borders.anim.context.as_ref() != Some(&context) {
        borders.anim.context = Some(context);
    }
    borders.anim.sync(space, &config.animations, focus);
    // Compiles the add-on shaders this frame's runs need (once per style) so
    // every query below agrees on which windows a shader draws.
    borders.anim.prepare_shaders(renderer);
    borders
        .anim
        .sync_layers(output, &config.animations, output_geo.size);
    layers(
        &mut elements,
        &mut blur_requests,
        &mut pane_requests,
        above,
        &borders.anim,
        renderer,
    );

    // Toplevels go one window at a time so each one's border and shadow stack
    // directly under its own surfaces rather than under the whole window stack
    // (COMP-02 §9). With no effect configured every surface is still emitted
    // unwrapped at alpha 1.0, so damage tracking and direct scanout are what
    // `space_render_elements` gave.
    ghost_elements(renderer, borders, output, space, config, &mut elements);
    window_elements(
        renderer,
        space,
        borders,
        output,
        config,
        focus,
        &mut elements,
        &mut blur_requests,
        &mut pane_requests,
    );

    layers(
        &mut elements,
        &mut blur_requests,
        &mut pane_requests,
        below,
        &borders.anim,
        renderer,
    );

    insert_blur(
        renderer,
        output,
        borders,
        config,
        blur_requests,
        pane_requests,
        &mut elements,
    );

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
    panes: Vec<PaneRequest>,
    elements: &mut Vec<AbyssRenderElement>,
) {
    let cfg = &config.decoration.blur;
    if requests.is_empty() && panes.is_empty() {
        // Nothing asked for a backdrop (blur `off`, or nothing translucent):
        // the chain must not sit in GPU memory for a frame that draws none.
        store.blur.remove(output);
        return;
    }
    let scale = Scale::from(output.current_scale().fractional_scale());
    let live: Vec<blur::BlurKey> = requests
        .iter()
        .map(|(key, ..)| key.clone())
        .chain(panes.iter().map(|p| p.key.clone()))
        .collect();
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

    // Both lists are front to back, so both are consumed from the back: a pane
    // set goes in before any request whose index is not above its own.
    let mut panes = panes.into_iter().rev().peekable();
    for (key, index, region, mode, radius, bezel, shape, crop) in requests.into_iter().rev() {
        while let Some(req) = panes.next_if(|p| p.before(index)) {
            insert_panes(renderer, output, store, config, req, elements);
        }
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
                BlurMode::Frost => effects::frost_uniforms(
                    region,
                    fb_height,
                    mirrored,
                    scaled_radius,
                    cfg.frost.tint,
                    bezel.map(|b| (b.inner, b.opaque, b.rim)),
                ),
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
        let frost = mode == BlurMode::Frost && program.is_some();
        let reach = if glass {
            effects::glass_reach(cfg.glass.refraction, upscale)
        } else {
            0
        };

        // Over an opaque window only the bezel's ring is ever seen.
        let ring = bezel.filter(|b| b.opaque && (glass || frost)).map(|b| {
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
    for req in panes {
        insert_panes(renderer, output, store, config, req, elements);
    }
}

/// Make sure the glass program is compiled; false if it cannot be.
fn ensure_glass(renderer: &mut GlesRenderer, store: &mut BorderStore) -> bool {
    if store.glass.is_none() && !store.glass_failed {
        match effects::compile_glass(renderer) {
            Ok(program) => store.glass = Some(program),
            Err(err) => {
                store.glass_failed = true;
                tracing::warn!(
                    ?err,
                    "compiling the glass shader; layers keep whole-surface glass"
                );
            }
        }
    }
    store.glass.is_some()
}

/// Splice one layer surface's glass panes into the element list (`eclipse_glass_v1`).
///
/// The backdrop is blurred once over the union of the panes' boxes; the cache
/// is keyed on that union, so a pane resizing inside it re-blurs nothing. Then
/// the glass program draws once per pane, in array order (the last on top),
/// each with its own box, radius and look. A pane is the window bezel's glass
/// with its bend confined to its band, dispersion off, and no seam; tone and
/// state only move the rim and a small fill lift, scaled by the pane's alpha.
fn insert_panes(
    renderer: &mut GlesRenderer,
    output: &Output,
    store: &mut BorderStore,
    config: &Config,
    req: PaneRequest,
    elements: &mut Vec<AbyssRenderElement>,
) {
    let cfg = &config.decoration.blur;
    let scale = Scale::from(output.current_scale().fractional_scale());
    let upscale = scale.x.max(scale.y);
    let Some((fb_height, mirrored)) = effects::fb_y_mirrored(output.current_transform())
        .zip(output.current_mode())
        .map(|(mirrored, mode)| (mode.size.h, mirrored))
    else {
        return;
    };
    let fb_size = output
        .current_mode()
        .map(|mode| output.current_transform().transform_size(mode.size))
        .map(|size| (size.w, size.h))
        .unwrap_or((1, 1));
    let Some(program) = store.glass.clone() else {
        return;
    };
    let panes = req.set.as_slice();
    let rect_of = |p: &glass::Pane| req.rect(p, scale);
    // A scaled window's corner radii scale with it.
    let grown = req.scaled.map_or(1.0, |(_, by)| by.x.max(by.y));
    // Every pane's box, drawn or not: a hover must not move the cache key.
    let Some(union) = panes.iter().filter_map(rect_of).reduce(|a, b| a.merge(b)) else {
        return;
    };
    let strength = (cfg.glass.refraction as f64 * upscale).min(BEZEL_REFRACTION * upscale) as f32;
    let reach = effects::glass_reach(cfg.glass.refraction.min(BEZEL_REFRACTION as i32), upscale);
    let mut blurs = std::mem::take(&mut store.pane_scratch);
    blurs.clear();
    // A window's own backdrop sits at `req.index`; panes sample what is
    // behind it, not the already-smoked glass, or they re-smoke to a slab.
    let behind = &elements[req.index + usize::from(req.over)..];
    store.blur.entry(output.clone()).or_default().pane_elements(
        renderer,
        output,
        &req.key,
        union,
        behind,
        cfg,
        scale,
        reach,
        &program,
        panes.len(),
        req.alpha,
        |i| {
            let pane = &panes[i];
            let look = glass::look(pane)?;
            let rect = rect_of(pane)?;
            let mut tuned = cfg.glass.clone();
            tuned.dispersion = 0.0;
            tuned.rim = cfg.glass.rim * look.rim;
            Some((
                rect,
                effects::glass_uniforms_lifted(
                    rect,
                    fb_height,
                    mirrored,
                    (pane.radius as f64 * upscale * grown) as f32,
                    fb_size,
                    strength,
                    glass::band(pane, rect, upscale * grown),
                    &tuned,
                    None,
                    look.lift,
                ),
            ))
        },
        &mut blurs,
    );
    // Inserting each at the same index leaves the last pane in front.
    for element in blurs.drain(..) {
        let element = match req.crop {
            Some(crop) => {
                CropRenderElement::from_element(element, scale, crop).map(AbyssRenderElement::CroppedBlur)
            }
            None => Some(AbyssRenderElement::Blur(element)),
        };
        if let Some(element) = element {
            elements.insert(req.index, element);
        }
    }
    store.pane_scratch = blurs;
}

/// A bezel's largest refraction, logical px: felt at the window edge, never a
/// funhouse.
const BEZEL_REFRACTION: f64 = 4.0;

/// How much of the neutral hairline an unfocused window's bezel keeps.
const BEZEL_DIM: f32 = 0.6;

/// Windows that have left the space, still playing out (COMP-02 §9), front
/// to back, above the mapped windows: a closing window was usually on top.
///
/// Each ghost gets its surfaces (through the rounded-corner mask when
/// rounding is on), then the border and drop shadow its window had, in the
/// focus state it left in, all at the ghost's transform and alpha. No glow and
/// no backdrop: a ghost is gone within a few frames. The decorations are built
/// fresh each frame rather than kept, since a ghost moves every frame anyway.
/// With no ghost this does nothing.
fn ghost_elements(
    renderer: &mut GlesRenderer,
    store: &mut BorderStore,
    output: &Output,
    space: &Space<Window>,
    config: &Config,
    out: &mut Vec<AbyssRenderElement>,
) {
    if store.anim.ghosts.is_empty() {
        return;
    }
    let Some(output_geo) = space.output_geometry(output) else {
        return;
    };
    let scale = Scale::from(output.current_scale().fractional_scale());
    let rounding = effects::fb_y_mirrored(output.current_transform())
        .zip(output.current_mode())
        .filter(|_| config.decoration.rounding > 0)
        .map(|(mirrored, mode)| (mode.size.h, mirrored))
        .zip(store.rounded.clone());
    let radius = (config.decoration.rounding as f64 * scale.x.max(scale.y)) as f32;

    let now = store.anim.now();
    let context = store.anim.context.clone();
    let rounded = store.rounded.clone().filter(|_| config.decoration.rounding > 0);
    for index in (0..store.anim.ghosts.len()).rev() {
        let ghost = &store.anim.ghosts[index];
        if ghost.output() != output {
            continue;
        }
        let mut t = store.anim.ghost_transform(ghost);
        let at = match ghost {
            anim::Ghost::Live { window, from_loc, .. } => Rectangle::new(*from_loc, window.geometry().size),
            anim::Ghost::Snapshot { geometry, .. } => *geometry,
            // Drawn by the `layers` pass, at its layer's depth.
            anim::Ghost::Layer { .. } => continue,
        };
        // An add-on transition shader draws this ghost in place of the
        // built-in style: the window's copy through the shader, the border and
        // shadow fading with the run. If the copy cannot be made the built-in
        // track draws instead, from where it is.
        if let Some(run) = store.anim.ghost_shader(ghost).cloned() {
            let local = Rectangle::new(at.loc - output_geo.loc, at.size);
            let element = context.clone().and_then(|context| {
                transition_quad(
                    renderer,
                    &mut store.anim.shaders,
                    context,
                    now,
                    &run,
                    local,
                    scale,
                    rounded.as_ref().map(|p| (p, radius)),
                    |renderer, origin, mask| match ghost {
                        anim::Ghost::Live { window, .. } if window.alive() => {
                            live_content(renderer, window, origin, scale, 1.0, mask)
                        }
                        anim::Ghost::Snapshot { surfaces, .. } => {
                            let base = origin.to_f64().to_logical(scale).to_i32_round();
                            let mut content = Vec::new();
                            snapshot_elements(
                                &mut content,
                                surfaces,
                                base,
                                scale,
                                Point::default(),
                                Scale::from(1.0),
                                mask,
                                1.0,
                            );
                            content
                        }
                        _ => Vec::new(),
                    },
                )
            });
            if let Some(element) = element {
                // No border or shadow: a rim around the whole rectangle
                // would show the window the shader is burning away.
                out.push(AbyssRenderElement::Transition(element));
                continue;
            }
            t = anim::Transform::identity().compose(ghost.track().at(now).0);
        }
        let pivot = anim::track::pivot(&t, at);
        let drawn = t.map(at, pivot);
        if !drawn.overlaps(output_geo) {
            continue;
        }
        // Base opacity as the window was drawn: the rule, else the pair.
        let deco = &config.decoration;
        let opacity = ghost
            .window()
            .and_then(userdata::opacity_of)
            .unwrap_or(if ghost.active() {
                deco.active_opacity
            } else {
                deco.inactive_opacity
            });
        let alpha = opacity * t.alpha;
        let origin = (pivot - output_geo.loc.to_f64())
            .to_physical(scale)
            .to_i32_round();
        let by = Scale::from(t.scale);
        let mask = rounding.as_ref().map(|((fb_height, mirrored), program)| {
            let rect = Rectangle::new(
                phys(drawn.loc - output_geo.loc, scale),
                drawn.size.to_f64().to_physical(scale).to_i32_round(),
            );
            (
                program.clone(),
                effects::rounding_uniforms(rect, *fb_height, *mirrored, radius),
            )
        });
        match ghost {
            anim::Ghost::Live { window, .. } => {
                if !window.alive() {
                    continue;
                }
                let render_loc = at.loc + t.loc() - window.geometry().loc - output_geo.loc;
                let els = window.render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(
                    renderer,
                    phys(render_loc, scale),
                    scale,
                    alpha,
                );
                out.extend(els.into_iter().map(|s| {
                    AbyssRenderElement::Scaled(anim::ScaledElement::new(s, origin, by, mask.clone()))
                }));
            }
            anim::Ghost::Snapshot { surfaces, .. } => {
                snapshot_elements(
                    out,
                    surfaces,
                    at.loc + t.loc() - output_geo.loc,
                    scale,
                    origin,
                    by,
                    mask.clone(),
                    alpha,
                );
            }
            anim::Ghost::Layer { .. } => {}
        }
        // A live window's decor is drawn opaque whatever its opacity, so the
        // ghost's carries the animation's alpha alone.
        let decor = GhostDecor {
            output_loc: output_geo.loc,
            scale,
            rounded: rounding.is_some(),
            active: ghost.active(),
            alpha: t.alpha,
        };
        ghost_decor(store, config, &decor, drawn, out);
    }
}

/// A window's surface tree for the transition shader's offscreen copy:
/// `origin` is where its geometry's top-left sits inside the copy.
fn live_content(
    renderer: &mut GlesRenderer,
    window: &Window,
    origin: Point<i32, Physical>,
    scale: Scale<f64>,
    alpha: f32,
    mask: Option<anim::track::Mask>,
) -> Vec<AbyssRenderElement> {
    let loc = origin
        + phys(
            Point::from((-window.geometry().loc.x, -window.geometry().loc.y)),
            scale,
        );
    window
        .render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(renderer, loc, scale, alpha)
        .into_iter()
        .map(|s| {
            AbyssRenderElement::Scaled(anim::ScaledElement::new(
                s,
                Point::default(),
                Scale::from(1.0),
                mask.clone(),
            ))
        })
        .collect()
}

/// Draw a window through an add-on transition shader (ADR 0073): `content`
/// builds its elements with the geometry's top-left at the given point inside
/// the offscreen copy, which is the window grown by the run's margin. `at` is
/// the geometry as laid out, output-local logical; `rounded` is the corner
/// program and physical radius, applied to the copy so the shader sees the
/// window's shape. `None` means the copy could not be made.
#[allow(clippy::too_many_arguments)]
fn transition_quad(
    renderer: &mut GlesRenderer,
    shaders: &mut anim::ShaderRegistry,
    context: ContextId<smithay::backend::renderer::gles::GlesTexture>,
    now: std::time::Instant,
    run: &anim::ShaderRun,
    at: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    rounded: Option<(&smithay::backend::renderer::gles::GlesTexProgram, f32)>,
    content: impl FnOnce(
        &mut GlesRenderer,
        Point<i32, Physical>,
        Option<anim::track::Mask>,
    ) -> Vec<AbyssRenderElement>,
) -> Option<anim::ShaderElement> {
    let win: Size<i32, Physical> = at.size.to_f64().to_physical(scale).to_i32_round();
    let quad = run.quad(at.size, scale);
    let origin = quad.origin();
    // The copy is rendered upright, so its rounding mask is unmirrored.
    let mask = rounded.map(|(program, radius)| {
        let rect = Rectangle::new(origin, win);
        (
            program.clone(),
            effects::rounding_uniforms(rect, quad.size(win).h, false, radius),
        )
    });
    // Built only when the copy is drawn: a frozen ghost's is built once.
    shaders.element(
        renderer,
        context,
        run,
        now,
        win,
        quad,
        phys(at.loc, scale) - origin,
        scale,
        |renderer| content(renderer, origin, mask),
    )
}

/// A snapshot ghost's textures as elements: `base` is the geometry's top-left
/// as drawn, output-local logical; `origin` and `by` scale about the pivot.
#[allow(clippy::too_many_arguments)]
fn snapshot_elements(
    out: &mut Vec<AbyssRenderElement>,
    surfaces: &[anim::SnapshotSurface],
    base: Point<i32, Logical>,
    scale: Scale<f64>,
    origin: Point<i32, Physical>,
    by: Scale<f64>,
    mask: Option<(
        smithay::backend::renderer::gles::GlesTexProgram,
        Vec<smithay::backend::renderer::gles::Uniform<'static>>,
    )>,
    alpha: f32,
) {
    use smithay::backend::renderer::element::texture::TextureRenderElement;

    out.extend(surfaces.iter().map(|s| {
        let texture = TextureRenderElement::from_static_texture(
            s.id.clone(),
            s.context.clone(),
            (base + s.offset).to_f64().to_physical(scale),
            s.texture.clone(),
            s.buffer_scale,
            s.transform,
            Some(alpha),
            Some(s.src),
            Some(s.size),
            None,
            Kind::Unspecified,
        );
        AbyssRenderElement::ScaledTexture(anim::ScaledElement::new(texture, origin, by, mask.clone()))
    }));
}

/// Where and how one ghost's decor is drawn.
struct GhostDecor {
    output_loc: Point<i32, Logical>,
    scale: Scale<f64>,
    /// Whether the ghost's surfaces go through the rounded mask.
    rounded: bool,
    active: bool,
    alpha: f32,
}

/// A ghost's border and drop shadow (COMP-02 §9): the same shapes
/// `push_border` and `push_shadow` draw for a live window, around `geo`
/// (global, as drawn), at the decor's alpha. A program not compiled yet — no
/// live window has needed it — draws nothing rather than compiling mid-ghost.
fn ghost_decor(
    store: &BorderStore,
    config: &Config,
    decor: &GhostDecor,
    geo: Rectangle<i32, Logical>,
    out: &mut Vec<AbyssRenderElement>,
) {
    let GhostDecor {
        output_loc,
        scale,
        rounded,
        active,
        alpha,
    } = *decor;
    let width = config.general.border_size;
    if width > 0 {
        let color = if active {
            config.general.col_active
        } else {
            config.general.col_inactive
        };
        let outer = Rectangle::new(
            (geo.loc.x - width - output_loc.x, geo.loc.y - width - output_loc.y).into(),
            (geo.size.w + 2 * width, geo.size.h + 2 * width).into(),
        );
        match store.ring.as_ref().filter(|_| rounded) {
            Some(program) => out.push(AbyssRenderElement::Shader(PixelShaderElement::new(
                program.clone(),
                outer,
                None,
                alpha,
                effects::border_uniforms(
                    color,
                    config.decoration.rounding as f32,
                    width as f32,
                    scale.x.max(scale.y) as f32,
                ),
                Kind::Unspecified,
            ))),
            None => {
                let inner_h = (outer.size.h - 2 * width).max(0);
                let quads = [
                    Rectangle::new(outer.loc, (outer.size.w, width).into()),
                    Rectangle::new(
                        (outer.loc.x, outer.loc.y + outer.size.h - width).into(),
                        (outer.size.w, width).into(),
                    ),
                    Rectangle::new((outer.loc.x, outer.loc.y + width).into(), (width, inner_h).into()),
                    Rectangle::new(
                        (outer.loc.x + outer.size.w - width, outer.loc.y + width).into(),
                        (width, inner_h).into(),
                    ),
                ];
                for quad in quads {
                    out.push(AbyssRenderElement::Solid(SolidColorRenderElement::from_buffer(
                        &SolidColorBuffer::new(quad.size, color),
                        phys(quad.loc, scale),
                        scale,
                        alpha,
                        Kind::Unspecified,
                    )));
                }
            }
        }
    }
    let shadow = &config.decoration.shadow;
    if let Some(program) = store
        .shadow
        .as_ref()
        .filter(|_| shadow.enabled && shadow.range > 0)
    {
        let (area, geometry) =
            shadow_geometry(geo, output_loc, shadow.range, shadow_drop(shadow.range), config);
        out.push(AbyssRenderElement::Shader(PixelShaderElement::new(
            program.clone(),
            area,
            None,
            alpha,
            effects::shadow_uniforms(geometry, if active { 1.0 } else { 0.0 }),
            Kind::Unspecified,
        )));
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
    pane_requests: &mut Vec<PaneRequest>,
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
        |w| store.anim.transform(w).loc(),
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
    // Whether the bezel's program (glass, frost) can draw here, per mode.
    let mut bezel_ok: [Option<bool>; 2] = [None; 2];

    let keep_frames = anim::resolve(
        &config.animations,
        ec_abyss_config::animations::Event::WindowClose,
        store.anim.now(),
        store.anim.shed(),
    )
    .is_some();
    if !keep_frames {
        store.anim.forget_frames();
    }
    // `space.elements()` is bottom-to-top; frames are collected front-to-back.
    for window in live.into_iter().rev() {
        let Some(loc) = space.element_location(&window) else {
            continue;
        };
        // An X11 window with no wl_surface has nothing to show. An InputOnly
        // one never gets a surface: Wine's screen-sized cursor-clip window would
        // otherwise be drawn as a blurred backdrop and shadow over the game.
        if smithay::wayland::seat::WaylandFocus::wl_surface(&window).is_none() {
            continue;
        }
        // A fullscreen window is square: a rounded corner would only show the
        // border and the wallpaper behind it.
        // Only abyss's own fullscreen counts: it holds those windows floating,
        // untiled. A tiled window that merely carries the X11/xdg fullscreen
        // state (a client that set it itself) keeps its rounding.
        let rounding = rounding
            .clone()
            .filter(|_| !(is_fullscreen(&window) && userdata::tile_clip(&window).is_none()));
        // A tiled client that ignored its configure (Electron holds its own
        // min width) is cut back to its tile rather than drawn under its
        // neighbour. Only a window that actually overhangs is cropped, so one
        // that fits keeps its plain elements and its scanout path.
        let clip = userdata::tile_clip(&window).filter(|c| {
            let g = window.geometry().size;
            g.w > c.size.w || g.h > c.size.h
        });
        // Everything drawn for the window — surfaces, crop, mask, border,
        // shadow, backdrop — goes through its animation transform, scaling
        // about the centre of what it shows. The identity when idle, where
        // `map` is exactly the target rectangle.
        let t = store.anim.transform(&window);
        let target = space.element_geometry(&window).map(|mut geo| {
            if let Some(c) = clip {
                geo.size.w = geo.size.w.min(c.size.w);
                geo.size.h = geo.size.h.min(c.size.h);
            }
            geo
        });
        let pivot = target.map_or_else(|| loc.to_f64() + t.offset, |g| anim::track::pivot(&t, g));
        let geo = target.map(|g| t.map(g, pivot));
        // Decorations are cut to the whole tile (the window plus its border),
        // whether or not the client overhangs it: unlike a surface, a shadow or
        // backdrop would otherwise bleed into the neighbouring tile.
        let deco_crop = Some(
            userdata::tile_clip(&window)
                .map(|c| {
                    let width = config.general.border_size.max(0);
                    let c = t.map(c, pivot);
                    Rectangle::new(
                        phys(c.loc - output_geo.loc, scale)
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
        }) * t.alpha;
        // `windowrule "translucent-region …"`: the strip's physical rect, the
        // window's, and the strip's own alpha (times the window's). Only a
        // toplevel at rest has one; an animating window draws whole.
        let strip_def = userdata::strip_of(&window)
            .filter(|_| !t.scaled() && window.toplevel().is_some())
            .zip(geo);
        // A keyed strip needs the keyed program and a framebuffer axis; where
        // either is missing the window stays opaque rather than fading whole.
        let keyed = match strip_def.as_ref().and_then(|(s, _)| s.keyed().copied()) {
            Some(keys) => keyed_target(renderer, store, output)
                .map(|(program, h, mirrored)| (program, h, mirrored, keys)),
            None => None,
        };
        let strip_def = strip_def.filter(|(s, _)| s.keyed().is_none() || keyed.is_some());
        let strip = strip_def.map(|(s, geo)| {
            let rect = Rectangle::new(
                phys(geo.loc - output_geo.loc, scale),
                geo.size.to_f64().to_physical(scale).to_i32_round(),
            );
            let fade = match s.fill {
                userdata::StripFill::Fade(a) => alpha * a,
                userdata::StripFill::Keyed(_) => alpha,
            };
            // Blur goes behind the whole window when any key matches everywhere.
            let blur_rect = match s.keyed() {
                Some(k) if k.any_everywhere() => rect,
                _ => strip_rect(rect, s.top, scale),
            };
            (blur_rect, rect, fade)
        });
        let key_region = strip_def.map(|(s, geo)| {
            let rect = Rectangle::new(
                phys(geo.loc - output_geo.loc, scale),
                geo.size.to_f64().to_physical(scale).to_i32_round(),
            );
            strip_rect(rect, s.top, scale)
        });

        // The dim overlay belongs above this window but below the ones in
        // front of it, so it is pushed just before the window's own surfaces.
        if !active && deco.dim_inactive > 0.0 && store.anim.window_shader(&window).is_none() {
            if let Some(geo) = geo {
                let start = out.len();
                push_dim(
                    store,
                    output,
                    &window,
                    geo,
                    output_geo.loc,
                    scale,
                    deco.dim_inactive * alpha,
                    out,
                );
                crop_decor(out, start, scale, deco_crop);
            }
        }

        let render_loc = loc + t.loc() - window.geometry().loc - output_geo.loc;
        // Popups are split off a cropped window so a menu that opens past the
        // tile edge is not cut; smithay's `render_elements` puts them first.
        let mut popup_groups: Vec<PopupGroup> = Vec::new();
        let (popups, surfaces) = match (clip, window.toplevel()) {
            (clip, Some(toplevel)) if clip.is_some() || strip.is_some() => {
                let surface = toplevel.wl_surface();
                let mut popups = Vec::new();
                for (n, (popup, popup_offset)) in PopupManager::popups_for_surface(surface).enumerate() {
                    let offset = (window.geometry().loc + popup_offset - popup.geometry().loc)
                        .to_physical_precise_round(scale);
                    let els = render_elements_from_surface_tree(
                        renderer,
                        popup.wl_surface(),
                        phys(render_loc, scale) + offset,
                        scale,
                        alpha,
                        Kind::Unspecified,
                    );
                    if keyed.is_some() {
                        // Keyed over the whole surface, glass behind it.
                        let at = render_loc + window.geometry().loc + popup_offset;
                        let rect = Rectangle::new(
                            phys(at, scale),
                            popup.geometry().size.to_f64().to_physical(scale).to_i32_round(),
                        );
                        popup_groups.push((n, rect, popup_blur_radius(renderer, &popup, deco.rounding), els));
                    } else {
                        popups.extend(els);
                    }
                }
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
        // The strip's copy of the surface tree, at the strip's alpha. A keyed
        // strip is one draw through the keyed program instead.
        let strip_surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = match (strip, window.toplevel())
        {
            (Some((_, _, strip_alpha)), Some(toplevel)) if keyed.is_none() => {
                render_elements_from_surface_tree(
                    renderer,
                    toplevel.wl_surface(),
                    phys(render_loc, scale),
                    scale,
                    strip_alpha,
                    Kind::Unspecified,
                )
            }
            _ => Vec::new(),
        };
        let tile_crop = clip.map(|c| {
            let c = t.map(c, pivot);
            Rectangle::new(
                phys(c.loc - output_geo.loc, scale),
                c.size.to_f64().to_physical(scale).to_i32_round(),
            )
        });
        // Keep what was just imported for a `window-close` that follows a new
        // commit before the next frame (see `AnimStore::remember`).
        if keep_frames {
            if let (Some(context), Some(surface)) = (
                store.anim.context.as_ref(),
                smithay::wayland::seat::WaylandFocus::wl_surface(&window),
            ) {
                let frame = anim::snapshot(context, &surface, Point::from((0, 0)) - window.geometry().loc);
                store.anim.remember(&window, frame);
            }
        }
        // A window reaching past its output (any size, any placement) is cut
        // to it; one that fits keeps its plain elements and its scanout path.
        let mut bbox = window.bbox();
        bbox.loc += loc - window.geometry().loc;
        let bbox = t.map(bbox, pivot);
        let overhangs = !output_geo.contains_rect(bbox);
        let crop = match (tile_crop, overhangs) {
            (Some(tile), true) => Some(tile.intersection(out_rect).unwrap_or_default()),
            (Some(tile), false) => Some(tile),
            (None, true) => Some(out_rect),
            (None, false) => None,
        };

        // An add-on transition shader draws the open in place of the surfaces
        // (and any popups, which ride in the same copy). When the copy cannot
        // be made the plain surfaces below draw, at full alpha.
        let (popups, surfaces, shaded) = match store.anim.window_shader(&window).cloned() {
            Some(run) => {
                let at = Rectangle::new(loc - output_geo.loc, window.geometry().size);
                let now = store.anim.now();
                let element = store.anim.context.clone().and_then(|context| {
                    transition_quad(
                        renderer,
                        &mut store.anim.shaders,
                        context,
                        now,
                        &run,
                        at,
                        scale,
                        rounding
                            .as_ref()
                            .map(|(_, p)| (p, (deco.rounding as f64 * scale.x.max(scale.y)) as f32)),
                        |renderer, origin, mask| {
                            let base = userdata::opacity_of(&window).unwrap_or(if active {
                                deco.active_opacity
                            } else {
                                deco.inactive_opacity
                            });
                            live_content(renderer, &window, origin, scale, base, mask)
                        },
                    )
                });
                match element {
                    Some(element) => {
                        // Pushed after the popups of the plain path would be;
                        // here there are none, they are inside the copy.
                        out.push(AbyssRenderElement::Transition(element));
                        (Vec::new(), Vec::new(), true)
                    }
                    None => (popups, surfaces, false),
                }
            }
            None => (popups, surfaces, false),
        };

        // Blur follows what shows through the window, not a whole-window
        // alpha (COMP-02 §9): decided here, on the raw surfaces, before the
        // rounding and crop wrappers (which report no opaque region) consume
        // them. A matched `windowrule "blur …"` overrides the global mode.
        let global = config.decoration.blur.mode;
        let mode = userdata::blur_of(&window).map_or(global, |rule| rule.resolve(global));
        // No backdrop behind a window a shader is reshaping.
        let mode = if shaded { BlurMode::Off } else { mode };
        let corner = match rounding {
            Some(_) => (deco.rounding as f64 * scale.x.max(scale.y)).ceil() as i32,
            None => 0,
        };
        let border_size = config.general.border_size;
        let bezel =
            match border.is_some() && matches!(mode, BlurMode::Glass | BlurMode::Frost) && geo.is_some() {
                true => {
                    let slot = (mode == BlurMode::Frost) as usize;
                    let ready =
                        *bezel_ok[slot].get_or_insert_with(|| bezel_ready(renderer, store, output, mode));
                    bezel_width(mode, ready, is_fullscreen(&window), border_size, upscale)
                }
                false => None,
            };
        // A scaled window is drawn through `ScaledElement` about `origin`
        // (output-local physical); its opaque regions scale with it.
        let scaled = t.scaled().then(|| {
            (
                (pivot - output_geo.loc.to_f64())
                    .to_physical(scale)
                    .to_i32_round(),
                Scale::from(t.scale),
            )
        });
        // `ext_background_effect_v1`: the surface origin is `render_loc`, and
        // the area animates with the window.
        let explicit = smithay::wayland::seat::WaylandFocus::wl_surface(&window).and_then(|surface| {
            explicit_blur(&surface, window.bbox().size, phys(render_loc, scale), scale).map(|area| {
                match scaled {
                    Some((origin, by)) => scale_area(area, origin, by),
                    None => area,
                }
            })
        });
        // A `translucent-region` rule asks for glass behind the strip, so a
        // toolkit's empty blur region (GTK declares one) does not veto it.
        let explicit = explicit.filter(|a| strip.is_none() || *a != blur::BlurArea::Nothing);
        let backdrop = geo
            .filter(|_| mode != BlurMode::Off)
            .and_then(|geo| {
                let rect = Rectangle::new(
                    phys(geo.loc - output_geo.loc, scale),
                    geo.size.to_f64().to_physical(scale).to_i32_round(),
                );
                let opaque = match scaled {
                    Some((origin, by)) => opaque_of(&surfaces, scale)
                        .into_iter()
                        .map(|r| anim::track::scale_about(r, origin, by))
                        .collect(),
                    None => opaque_of(&surfaces, scale),
                };
                match explicit {
                    Some(area) if !explicit_bezel(&area, rect, bezel) => {
                        // The strip is translucent whatever the client's
                        // opaque region says.
                        let opaque = match strip {
                            Some((strip, _, _)) => Rectangle::subtract_rects_many(opaque, [strip]),
                            None => opaque,
                        };
                        explicit_window_backdrop(area, alpha, corner, opaque, deco.rounding)
                            .map(|(region, radius, shape)| (region, radius, None, shape))
                    }
                    _ => {
                        let backdrop = |opaque| {
                            window_backdrop(
                                mode,
                                alpha,
                                rect,
                                corner,
                                opaque,
                                deco.rounding,
                                border_size,
                                bezel,
                            )
                        };
                        // The client's opaque region must not hide the glass
                        // behind the strip. A bezel already blurs the whole
                        // bordered rect, so the strip only has to stop
                        // counting as opaque there; otherwise a window with
                        // nothing translucent of its own blurs just the strip.
                        match strip {
                            Some((strip, _, _)) if bezel.is_some() => {
                                backdrop(Rectangle::subtract_rects_many(opaque, [strip]))
                            }
                            Some((strip, _, _)) => backdrop(opaque.clone()).or_else(|| {
                                strip_backdrop(strip, rect, corner)
                                    .map(|region| (region, deco.rounding, None))
                            }),
                            None => backdrop(opaque),
                        }
                        .map(|(region, radius, bezel)| (region, radius, bezel, None))
                    }
                }
            })
            .map(|(region, radius, bezel, shape)| {
                let bezel = bezel.map(|(inner, opaque)| {
                    // The layer's neutral white hairline; focus only dims it,
                    // crossfading like the painted border does.
                    let rim = store.anim.border_color(&window, [1.0; 4], [BEZEL_DIM; 4]);
                    Bezel { inner, opaque, rim }
                });
                (region, radius, bezel, shape)
            });

        // Every surface of one window is masked by the same rectangle, so a
        // window with subsurfaces rounds as a single shape.
        //
        // Each band is the surfaces drawn within one crop. A window with a
        // translucent strip has two: the strip's copy at the strip's alpha
        // above the strip's bottom edge, the plain one below it. They are
        // disjoint, so the rounded mask (the whole window's) is shared.
        let bands = match strip {
            Some((strip, _, _)) if !shaded && keyed.is_none() => {
                let [above, below] = strip_crops(strip, out_rect, crop);
                vec![(strip_surfaces, Some(above)), (surfaces, Some(below))]
            }
            _ => vec![(surfaces, crop)],
        };
        // Keyed glass draws only when no transition shader reshapes the window;
        // a keyed window's popups otherwise draw plain.
        let keyed_on = keyed.filter(|_| !shaded);
        let mut popups = popups;
        if keyed_on.is_none() {
            for (_, _, _, els) in popup_groups.drain(..) {
                popups.extend(els);
            }
        }
        match (scaled, &rounding, geo) {
            // Tint-keyed strip (ADR 0079): the popups, then the whole surface
            // tree, each through the keyed program; only key-coloured pixels
            // inside the region turn translucent. Idle windows only.
            (None, _, Some(geo)) if keyed_on.is_some() => {
                let Some((program, fb_h, mirrored, keys)) = keyed_on.clone() else {
                    continue;
                };
                let rect = Rectangle::new(
                    phys(geo.loc - output_geo.loc, scale),
                    geo.size.to_f64().to_physical(scale).to_i32_round(),
                );
                let radius = deco.rounding as f64 * scale.x.max(scale.y);
                let mask = rounding.as_ref().map(|_| (rect, radius as f32));
                let region = key_region;
                let popup_crop = overhangs.then_some(out_rect);
                let push_keyed = |out: &mut Vec<AbyssRenderElement>,
                                  els: Vec<WaylandSurfaceRenderElement<GlesRenderer>>,
                                  uniforms: Vec<smithay::backend::renderer::gles::Uniform<'static>>,
                                  crop: Option<Rectangle<i32, Physical>>| {
                    for s in els {
                        let el = effects::RoundedElement::new(s, program.clone(), uniforms.clone());
                        match crop {
                            Some(crop) => out.extend(
                                CropRenderElement::from_element(el, scale, crop)
                                    .map(AbyssRenderElement::CroppedRounded),
                            ),
                            None => out.push(AbyssRenderElement::Rounded(el)),
                        }
                    }
                };
                for (n, prect, pradius, els) in popup_groups.drain(..) {
                    push_keyed(
                        out,
                        els,
                        effects::keyed_uniforms(None, None, fb_h, mirrored, &keys),
                        popup_crop,
                    );
                    if mode != BlurMode::Off {
                        blurred.push((
                            blur::BlurKey::Popup(window.clone(), n),
                            out.len(),
                            prect,
                            mode,
                            pradius,
                            None,
                            None,
                            popup_crop,
                        ));
                    }
                }
                push_popups(out, popups, scale, popup_crop);
                for (surfaces, crop) in bands {
                    push_keyed(
                        out,
                        surfaces,
                        effects::keyed_uniforms(mask, region, fb_h, mirrored, &keys),
                        crop,
                    );
                }
            }
            // Animating at a scale: the same arms through `ScaledElement`,
            // which carries the mask itself. Idle windows never get here.
            (Some((origin, by)), rounding, _) => {
                let mask = rounding
                    .as_ref()
                    .zip(geo)
                    .map(|(((fb_height, mirrored), program), geo)| {
                        let rect = Rectangle::new(
                            phys(geo.loc - output_geo.loc, scale),
                            geo.size.to_f64().to_physical(scale).to_i32_round(),
                        );
                        let radius = deco.rounding as f64 * scale.x.max(scale.y);
                        (
                            program.clone(),
                            effects::rounding_uniforms(rect, *fb_height, *mirrored, radius as f32),
                        )
                    });
                let popup_crop = overhangs.then_some(out_rect);
                for popup in popups {
                    let popup = anim::ScaledElement::new(popup, origin, by, None);
                    match popup_crop {
                        Some(crop) => out.extend(
                            CropRenderElement::from_element(popup, scale, crop)
                                .map(AbyssRenderElement::CroppedScaled),
                        ),
                        None => out.push(AbyssRenderElement::Scaled(popup)),
                    }
                }
                for surface in bands.into_iter().flat_map(|(surfaces, _)| surfaces) {
                    let surface = anim::ScaledElement::new(surface, origin, by, mask.clone());
                    match crop {
                        Some(crop) => out.extend(
                            CropRenderElement::from_element(surface, scale, crop)
                                .map(AbyssRenderElement::CroppedScaled),
                        ),
                        None => out.push(AbyssRenderElement::Scaled(surface)),
                    }
                }
            }
            (None, Some(((fb_height, mirrored), program)), Some(geo)) => {
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
                for (surfaces, crop) in bands {
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
            }
            _ => {
                push_popups(out, popups, scale, overhangs.then_some(out_rect));
                for (surfaces, crop) in bands {
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
        }

        let Some(geo) = geo else {
            continue;
        };
        // A shader open draws the window alone: no border, glow or shadow
        // around the rectangle it is reshaping.
        if shaded {
            store.rings.remove(&(output.clone(), window.clone()));
            store.borders.remove(&(output.clone(), window.clone()));
            continue;
        }
        // Border, glow, then shadow, all directly under this window's
        // surfaces. The ring's inner edge is the exact complement of the
        // window's rounded mask, so it never covers window content from below
        // either; glow and shadow draw only outside the bordered rect. A glass
        // bezel replaces the painted ring.
        // In glass and frost the bezel is the frame: a window whose bezel is
        // not drawn yet (no backdrop on its first frames) gets no painted ring
        // either, unless the bezel shader failed to compile.
        let glassy = matches!(mode, BlurMode::Glass | BlurMode::Frost)
            && bezel_ok[(mode == BlurMode::Frost) as usize] != Some(false);
        let bezeled = glassy || backdrop.as_ref().is_some_and(|(_, _, b, _)| b.is_some());
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
            // Follows the border's focus crossfade, so glow fades with focus.
            let (active, inactive) = glow_colors(config);
            let color = store.anim.border_color(&window, active, inactive);
            push_glow(
                store,
                program,
                output,
                &window,
                geo,
                output_geo.loc,
                config,
                color,
                out,
            );
        }
        if let Some(program) = &shadow {
            // Focus deepens the shadow, following the border's crossfade.
            let focus = store.anim.border_color(&window, [1.0, 0.0, 0.0, 0.0], [0.0; 4])[0];
            push_shadow(
                store,
                program,
                output,
                &window,
                geo,
                output_geo.loc,
                config,
                focus,
                out,
            );
        }
        crop_decor(out, start, scale, deco_crop);

        // An opaque window gets no backdrop pass at all (the gate above)
        // unless its border is a glass bezel. The backdrop goes below the
        // window's own border and shadow, so neither is smeared into it; it
        // covers the window rect only, which the ring does not overlap — or,
        // for a bezel, the bordered rect, which the shadow does not overlap.
        if let Some((region, radius, bezel, shape)) = backdrop {
            // `eclipse_glass_v1` on a window: glass under each pane, directly
            // above the backdrop and bezel just below. Only where the window
            // is glass already (it has a backdrop, in glass mode); a keyed,
            // strip or fullscreen window is left as it was. The panes ride the
            // window's animation: its scale about the pivot and its alpha.
            let panes = window_takes_panes(
                mode,
                keyed_on.is_some(),
                strip.is_some(),
                is_fullscreen(&window),
                effects::fb_y_mirrored(output.current_transform()).is_some(),
            )
            .then(|| smithay::wayland::seat::WaylandFocus::wl_surface(&window))
            .flatten()
            .and_then(|surface| {
                let set = committed_panes(&surface)?;
                ensure_glass(renderer, store).then_some(())?;
                Some((surface, set))
            });
            if let Some((surface, set)) = panes {
                use smithay::backend::renderer::utils::RendererSurfaceStateUserData;
                let size = with_states(&surface, |states| {
                    states
                        .data_map
                        .get::<RendererSurfaceStateUserData>()
                        .and_then(|d| d.lock().unwrap().surface_size())
                })
                .unwrap_or_else(|| window.bbox().size);
                pane_requests.push(PaneRequest {
                    key: blur::BlurKey::Panes(window.clone()),
                    index: out.len(),
                    set,
                    size,
                    // The root surface's origin: `render_loc` already carries
                    // the window position, its animation offset and the xdg
                    // window-geometry offset.
                    loc: phys(render_loc, scale),
                    crop: deco_crop,
                    over: true,
                    scaled,
                    alpha: t.alpha,
                });
            }
            blurred.push((
                blur::BlurKey::Window(window.clone()),
                out.len(),
                region,
                mode,
                radius,
                bezel,
                shape,
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
/// One window's dim-inactive overlay at `opacity`, over `geo` (global). The
/// quad is kept per window so its Id, and with it damage, is stable.
#[allow(clippy::too_many_arguments)]
fn push_dim(
    store: &mut BorderStore,
    output: &Output,
    window: &Window,
    geo: Rectangle<i32, Logical>,
    output_loc: Point<i32, Logical>,
    scale: Scale<f64>,
    opacity: f32,
    out: &mut Vec<AbyssRenderElement>,
) {
    let color = [0.0, 0.0, 0.0, opacity];
    let buffer = store
        .dims
        .entry((output.clone(), window.clone()))
        .or_insert_with(|| SolidColorBuffer::new(geo.size, color));
    buffer.update(geo.size, color);
    out.push(AbyssRenderElement::Solid(SolidColorRenderElement::from_buffer(
        buffer,
        phys(geo.loc - output_loc, scale),
        scale,
        1.0,
        Kind::Unspecified,
    )));
}

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
    focus: f32,
    out: &mut Vec<AbyssRenderElement>,
) {
    let range = config.decoration.shadow.range;
    let (area, geometry) = shadow_geometry(geo, output_loc, range, shadow_drop(range), config);
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

/// A window's glow colour when focused and when not, premultiplied, with a
/// state whose glow is off as transparent (COMP-02 §9). Config colours are
/// straight alpha; premultiplying before the focus crossfade keeps the fade
/// to transparent linear.
fn glow_colors(config: &Config) -> ([f32; 4], [f32; 4]) {
    let glow = &config.decoration.glow;
    let k = glow.strength as f32 / 100.0;
    let on = |yes: bool, [r, g, b, a]: [f32; 4]| match yes {
        true => [r * a * k, g * a * k, b * a * k, a * k],
        false => [0.0; 4],
    };
    (
        on(glow.active, config.general.col_active),
        on(glow.inactive, config.general.col_inactive),
    )
}

/// One window's border glow: a ring in `color` (see [`glow_colors`]),
/// reaching `GLOW_RANGE` past the border (COMP-02 §9). Stored and updated like
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
    color: [f32; 4],
    out: &mut Vec<AbyssRenderElement>,
) {
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
    let fade = store.anim.transform(window).alpha;
    let color = store
        .anim
        .border_color(window, config.general.col_active, config.general.col_inactive)
        .map(|c| c * fade);
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
    // A translucent strip is composited by us, like any other alpha.
    if userdata::strip_of(window).is_some() {
        return None;
    }
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

    fn pane_set(boxes: &[(i32, i32, i32, i32)]) -> glass::PaneSet {
        let bytes: Vec<u8> = boxes
            .iter()
            .flat_map(|&(x, y, w, h)| {
                [x as u32, y as u32, w as u32, h as u32, 8, 0, 0, 0, 255]
                    .into_iter()
                    .flat_map(|v| v.to_ne_bytes())
            })
            .collect();
        glass::parse_panes(&bytes).unwrap()
    }

    fn window_request(
        index: usize,
        over: bool,
        loc: Point<i32, Physical>,
        scaled: Option<(Point<i32, Physical>, Scale<f64>)>,
    ) -> PaneRequest {
        PaneRequest {
            key: blur::BlurKey::Annotation(0),
            index,
            set: pane_set(&[(10, 20, 100, 30)]),
            size: (400, 300).into(),
            loc,
            crop: None,
            over,
            scaled,
            alpha: 1.0,
        }
    }

    /// Panes are surface-local to the root surface, whose origin is the window
    /// position minus the xdg geometry offset: a cell at (10, 20) under a
    /// client with a (12, 8) shadow margin sits 12 and 8 px left and up of
    /// where it would on the window rect, at the output scale.
    #[test]
    fn window_panes_follow_the_geometry_offset() {
        let scale = Scale::from(2.0);
        let window_at = Point::<i32, Logical>::from((100, 50));
        let geometry = Point::<i32, Logical>::from((12, 8));
        let origin = phys(window_at - geometry, scale);
        let req = window_request(5, true, origin, None);
        let pane = &req.set.as_slice()[0];
        let r = req.rect(pane, scale).unwrap();
        assert_eq!(
            (r.loc.x, r.loc.y, r.size.w, r.size.h),
            ((88 + 10) * 2, (42 + 20) * 2, 200, 60)
        );
        // Without the offset the same pane lands elsewhere.
        let flat = window_request(5, true, phys(window_at, scale), None);
        assert_ne!(flat.rect(pane, scale).unwrap().loc, r.loc);
    }

    /// An animating window carries its panes with it: scaled about the pivot.
    #[test]
    fn window_panes_ride_the_animation_scale() {
        let scale = Scale::from(1.0);
        let pivot = Point::<i32, Physical>::from((200, 150));
        let req = window_request(0, true, (0, 0).into(), Some((pivot, Scale::from(0.5))));
        let r = req.rect(&req.set.as_slice()[0], scale).unwrap();
        // (10, 20) is (-190, -130) from the pivot, halved to (-95, -65).
        assert_eq!((r.loc.x, r.loc.y, r.size.w, r.size.h), (105, 85, 50, 15));
    }

    /// At one splice index a layer's panes go in before its backdrop (they
    /// replace it), a window's after (they sit above the window's backdrop and
    /// bezel); anything lower in the list always goes in first.
    #[test]
    fn window_panes_land_above_their_backdrop() {
        let at = |index, over| window_request(index, over, (0, 0).into(), None);
        assert!(at(7, false).before(7));
        assert!(!at(7, true).before(7));
        assert!(at(8, true).before(7));
        assert!(!at(6, true).before(7));
        assert!(!at(6, false).before(7));
    }

    /// Panes change only a glass-mode window that nothing else claims.
    #[test]
    fn only_plain_glass_windows_take_panes() {
        assert!(window_takes_panes(BlurMode::Glass, false, false, false, true));
        for mode in [BlurMode::Off, BlurMode::Frost] {
            assert!(!window_takes_panes(mode, false, false, false, true));
        }
        assert!(!window_takes_panes(BlurMode::Glass, true, false, false, true));
        assert!(!window_takes_panes(BlurMode::Glass, false, true, false, true));
        assert!(!window_takes_panes(BlurMode::Glass, false, false, true, true));
        assert!(!window_takes_panes(BlurMode::Glass, false, false, false, false));
    }

    fn config(rounding: i32, border: i32, range: i32) -> Config {
        let mut cfg = Config::default();
        cfg.decoration.rounding = rounding;
        cfg.general.border_size = border;
        cfg.decoration.shadow.range = range;
        cfg
    }

    /// `ext_background_effect_v1` on a window: the client's box is the backdrop
    /// (not the window rect), an empty region has none, and only a fully
    /// opaque-covered area is vetoed.
    #[test]
    fn a_window_blurs_where_its_client_asked() {
        let r = |x, y, w, h| Rectangle::<i32, Physical>::new((x, y).into(), (w, h).into());
        let area = blur::BlurArea::Rect(r(10, 10, 100, 40));
        assert_eq!(
            explicit_window_backdrop(area, 1.0, 0, vec![], 12),
            Some((r(10, 10, 100, 40), 12, None))
        );
        assert_eq!(
            explicit_window_backdrop(blur::BlurArea::Nothing, 0.5, 0, vec![], 12),
            None
        );
        let opaque = vec![r(0, 0, 300, 300)];
        assert_eq!(explicit_window_backdrop(area, 1.0, 0, opaque.clone(), 12), None);
        assert!(
            explicit_window_backdrop(area, 0.9, 0, opaque, 12).is_some(),
            "a fade shows through"
        );
    }

    /// An explicit area that is the whole window takes the bezel path (one
    /// glass look for every window); a partial or shaped one keeps its box.
    #[test]
    fn a_whole_window_area_takes_the_bezel() {
        let r = |x, y, w, h| Rectangle::<i32, Physical>::new((x, y).into(), (w, h).into());
        let window = r(10, 10, 400, 300);
        let whole = blur::BlurArea::Rect(r(10, 10, 400, 300));
        assert!(explicit_bezel(&whole, window, Some(2)));
        // A pixel short from the area's own rounding still covers it.
        assert!(explicit_bezel(
            &blur::BlurArea::Rect(r(10, 11, 400, 299)),
            window,
            Some(2)
        ));
        // A CSD surface larger than the window geometry covers it too.
        assert!(explicit_bezel(
            &blur::BlurArea::Rect(r(0, 0, 500, 400)),
            window,
            Some(2)
        ));
        // No bezel on this output (not glass, no border, fullscreen): unchanged.
        assert!(!explicit_bezel(&whole, window, None));
        // A strip or nothing keeps its own box.
        assert!(!explicit_bezel(
            &blur::BlurArea::Rect(r(10, 10, 400, 40)),
            window,
            Some(2)
        ));
        assert!(!explicit_bezel(&blur::BlurArea::Nothing, window, Some(2)));
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
        assert_eq!(shadow_drop(20), 0, "an even halo, not an offset shadow");
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
    fn the_strip_is_measured_from_the_top_and_scales() {
        // A 300x200 logical window at (100, 50).
        let rect = r(100, 50, 300, 200);
        assert_eq!(strip_rect(rect, 40, Scale::from(1.0)), r(100, 50, 300, 40));
        // At scale 2 the window is 600x400 physical and the strip doubles.
        let rect2 = r(200, 100, 600, 400);
        assert_eq!(strip_rect(rect2, 40, Scale::from(2.0)), r(200, 100, 600, 80));
        // Fractional scale rounds; a strip taller than the window is the window.
        assert_eq!(strip_rect(rect, 40, Scale::from(1.5)).size.h, 60);
        assert_eq!(strip_rect(rect, 999, Scale::from(1.0)), rect);
        assert!(strip_rect(rect, 0, Scale::from(1.0)).is_empty());
    }

    #[test]
    fn the_strip_blurs_a_region_that_hides_the_bottom_corners() {
        let rect = r(100, 50, 300, 200);
        let strip = strip_rect(rect, 40, Scale::from(1.0));
        // Grown by the corner radius, so the rounded bottom is under the page.
        assert_eq!(strip_backdrop(strip, rect, 12), Some(r(100, 50, 300, 52)));
        // Never past the window.
        let tall = strip_rect(rect, 195, Scale::from(1.0));
        assert_eq!(strip_backdrop(tall, rect, 12), Some(rect));
        assert_eq!(
            strip_backdrop(strip_rect(rect, 0, Scale::from(1.0)), rect, 12),
            None
        );
    }

    #[test]
    fn the_strip_splits_the_surfaces_into_two_disjoint_bands() {
        let out = r(0, 0, 1000, 800);
        let strip = r(100, 50, 300, 40);
        let [above, below] = strip_crops(strip, out, None);
        assert_eq!(above, r(0, 0, 1000, 90));
        assert_eq!(below, r(0, 90, 1000, 710));
        // A tile crop cuts both.
        let tile = r(100, 50, 300, 200);
        let [above, below] = strip_crops(strip, out, Some(tile));
        assert_eq!(above, r(100, 50, 300, 40));
        assert_eq!(below, r(100, 90, 300, 160));
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
        // Fullscreen, no border, no glass program, other modes.
        assert_eq!(bezel_width(BlurMode::Glass, true, true, 6, 1.0), None);
        assert_eq!(bezel_width(BlurMode::Glass, true, false, 0, 1.0), None);
        assert_eq!(bezel_width(BlurMode::Glass, false, false, 6, 1.0), None);
        for mode in [BlurMode::Off, BlurMode::Blur] {
            assert_eq!(bezel_width(mode, true, false, 6, 1.0), None);
        }
        assert_eq!(bezel_width(BlurMode::Frost, true, false, 6, 1.0), Some(6));
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
