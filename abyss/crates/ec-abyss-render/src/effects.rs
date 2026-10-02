// SPDX-License-Identifier: AGPL-3.0-only
//! Optional per-window effects drawn with custom GL programs (COMP-02 §9).
//!
//! Rounding is a fragment-shader mask, not a geometry change: the window's own
//! surfaces are drawn by smithay as usual, with the default texture program
//! swapped for one that multiplies in a rounded-box coverage term. Because the
//! mask is computed from `gl_FragCoord`, every subsurface of a window is cut by
//! the same rectangle, so a window with subsurfaces rounds as one shape.
//!
//! Rounding a window means its texture is no longer fully opaque, so it can no
//! longer go to a scanout plane — that is inherent to the effect and is why
//! `rounding 0` keeps the plain path.

use smithay::backend::renderer::{
    element::{surface::WaylandSurfaceRenderElement, Element, Id, Kind, RenderElement, UnderlyingStorage},
    gles::{
        GlesError, GlesFrame, GlesPixelProgram, GlesRenderer, GlesTexProgram, Uniform, UniformName,
        UniformType,
    },
    utils::{CommitCounter, DamageSet, OpaqueRegions},
};
use smithay::utils::{Buffer as BufferCoords, Physical, Point, Rectangle, Scale, Transform};

/// Mirrors smithay's built-in `texture.frag`, with a rounded-box mask applied
/// to the final colour. `win_rect` is the window's rectangle in `gl_FragCoord`
/// space (framebuffer pixels, see [`rounding_uniforms`]).
const ROUNDED_TEX_SRC: &str = r#"#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif

#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform vec4 win_rect;
uniform float radius;

void main() {
    vec4 color = texture2D(tex, v_coords);

#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0) * alpha;
#else
    color = color * alpha;
#endif

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

#ifdef SHAPED
    float d = shape_sd(gl_FragCoord.xy);
#else
    vec2 half_size = win_rect.zw * 0.5;
    vec2 p = gl_FragCoord.xy - (win_rect.xy + half_size);
    float r = min(radius, min(half_size.x, half_size.y));
    vec2 q = abs(p) - half_size + r;
    float d = length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
#endif

    gl_FragColor = color * (1.0 - smoothstep(-0.5, 0.5, d));
}
"#;

/// The line every mask program declares its corner radius on; the shape
/// functions are spliced in right after it (see [`shaped_source`]).
const RADIUS_DECL: &str = "uniform float radius;\n";

/// A layer's glass shape (Vol 1 §5.2, [`crate::blur::Shape`]): up to
/// four boxes in `gl_FragCoord` space (`x y w h`, as [`rounding_uniforms`]
/// places `win_rect`), each rounded by `radius`, unioned in order by a
/// polynomial smooth-min of width `fillet`. `boxes` is how many are live
/// (2..=4). Mirrored line for line by [`crate::blur::shape_sd`].
const SHAPE_FNS: &str = r#"
#define SHAPED 1
uniform vec4 box0;
uniform vec4 box1;
uniform vec4 box2;
uniform vec4 box3;
uniform float boxes;
uniform float fillet;

float box_sd(vec2 f, vec4 b) {
    vec2 h = b.zw * 0.5;
    float r = min(radius, min(h.x, h.y));
    vec2 q = abs(f - (b.xy + h)) - h + r;
    return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

float smin(float a, float b) {
    if (fillet <= 0.0)
        return min(a, b);
    float h = max(fillet - abs(a - b), 0.0) / fillet;
    return min(a, b) - h * h * fillet * 0.25;
}

float shape_sd(vec2 f) {
    float d = smin(box_sd(f, box0), box_sd(f, box1));
    if (boxes > 2.5)
        d = smin(d, box_sd(f, box2));
    if (boxes > 3.5)
        d = smin(d, box_sd(f, box3));
    return d;
}
"#;

/// The shaped variant of one of the mask programs: the shape functions and
/// `SHAPED` defined right after its `radius` uniform.
fn shaped_source(src: &str) -> String {
    src.replacen(RADIUS_DECL, &format!("{RADIUS_DECL}{SHAPE_FNS}"), 1)
}

/// The shape's own uniforms, in the order [`shape_uniforms`] sets them.
const SHAPE_UNIFORMS: [(&str, UniformType); 6] = [
    ("box0", UniformType::_4f),
    ("box1", UniformType::_4f),
    ("box2", UniformType::_4f),
    ("box3", UniformType::_4f),
    ("boxes", UniformType::_1f),
    ("fillet", UniformType::_1f),
];

fn with_shape(base: &[(&'static str, UniformType)]) -> Vec<UniformName<'static>> {
    base.iter()
        .chain(SHAPE_UNIFORMS.iter())
        .map(|&(name, ty)| UniformName::new(name, ty))
        .collect()
}

/// The shaped rounded-corner program (plain `blur` mode). Cached by the caller.
pub fn compile_rounded_shaped(renderer: &mut GlesRenderer) -> Result<GlesTexProgram, GlesError> {
    renderer.compile_custom_texture_shader(
        shaped_source(ROUNDED_TEX_SRC),
        &with_shape(&[("win_rect", UniformType::_4f), ("radius", UniformType::_1f)]),
    )
}

/// The shaped frost program. Cached by the caller.
pub fn compile_frost_shaped(renderer: &mut GlesRenderer) -> Result<GlesTexProgram, GlesError> {
    renderer.compile_custom_texture_shader(
        shaped_source(&format!("{BACKDROP_HEAD}{FROST_BODY}")),
        &with_shape(&[
            ("win_rect", UniformType::_4f),
            ("radius", UniformType::_1f),
            ("frost_tint", UniformType::_4f),
        ]),
    )
}

/// The shaped glass program. Cached by the caller.
pub fn compile_glass_shaped(renderer: &mut GlesRenderer) -> Result<GlesTexProgram, GlesError> {
    renderer.compile_custom_texture_shader(
        shaped_source(&format!("{BACKDROP_HEAD}{GLASS_BODY}")),
        &with_shape(&GLASS_UNIFORMS),
    )
}

/// The shape uniforms appended to a mask program's own: the boxes placed in
/// `gl_FragCoord` space exactly as [`rounding_uniforms`] places `win_rect`,
/// unused slots zeroed (`boxes` says how many are live).
pub fn shape_uniforms(shape: &crate::blur::Shape, fb_height: i32, mirrored: bool) -> [Uniform<'static>; 6] {
    let mut boxes = [[0.0f32; 4]; 4];
    for (slot, rect) in boxes.iter_mut().zip(shape.rects()) {
        let y = if mirrored {
            fb_height - rect.loc.y - rect.size.h
        } else {
            rect.loc.y
        };
        *slot = [
            rect.loc.x as f32,
            y as f32,
            rect.size.w as f32,
            rect.size.h as f32,
        ];
    }
    let [b0, b1, b2, b3] = boxes;
    [
        Uniform::new("box0", b0),
        Uniform::new("box1", b1),
        Uniform::new("box2", b2),
        Uniform::new("box3", b3),
        Uniform::new("boxes", shape.rects().len() as f32),
        Uniform::new("fillet", shape.fillet),
    ]
}

/// Compile the rounded-corner texture program. Cached by the caller; the
/// compile only happens on the first frame that actually rounds something.
pub fn compile_rounded(renderer: &mut GlesRenderer) -> Result<GlesTexProgram, GlesError> {
    renderer.compile_custom_texture_shader(
        ROUNDED_TEX_SRC,
        &[
            UniformName::new("win_rect", UniformType::_4f),
            UniformName::new("radius", UniformType::_1f),
        ],
    )
}

/// Shared head of the frost and glass backdrop programs: smithay's texture
/// interface (`tex`, `alpha`, `v_coords`, the debug `tint`), the rounded
/// rectangle of [`rounding_uniforms`], and a position hash both bodies use.
const BACKDROP_HEAD: &str = r#"#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif

#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform vec4 win_rect;
uniform float radius;

// Premultiply by `alpha` exactly as smithay's texture.frag does.
vec4 finish(vec4 color) {
#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0) * alpha;
#else
    color = color * alpha;
#endif
#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif
    return color;
}

// Hoskins' hash12: no `sin`, so it holds up at mediump and large coords.
float grain(vec2 p) {
    vec3 p3 = fract(vec3(p.xyx) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}
"#;

/// Frost: the rounded mask, a tint mixed in by its own alpha, and a fine
/// static grain hashed from the framebuffer position (COMP-02 §9).
const FROST_BODY: &str = r#"
uniform vec4 frost_tint;

void main() {
    vec4 color = texture2D(tex, v_coords);
    color.rgb = mix(color.rgb, frost_tint.rgb, frost_tint.a);
    color.rgb = clamp(color.rgb + (grain(floor(gl_FragCoord.xy)) - 0.5) * 0.035, 0.0, 1.0);
    color = finish(color);

#ifdef SHAPED
    float d = shape_sd(gl_FragCoord.xy);
#else
    vec2 half_size = win_rect.zw * 0.5;
    vec2 p = gl_FragCoord.xy - (win_rect.xy + half_size);
    float r = min(radius, min(half_size.x, half_size.y));
    vec2 q = abs(p) - half_size + r;
    float d = length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
#endif
    gl_FragColor = color * (1.0 - smoothstep(-0.5, 0.5, d));
}
"#;

/// Glass: a calm, saturated, dark frosted pane (COMP-02 §9). Only the
/// blurred backdrop (`tex`) is sampled. Inside the outer `bevel` it rolls off
/// by at most `strength` px toward the centre, a lens that is felt rather
/// than seen; the plateau is untouched. The colour is then saturated about
/// Rec.709 luma, its highlights compressed and smoked toward a near-neutral
/// `#0c0c0e`, and a crisp near-white hairline lit from the top-left (with a
/// weaker bottom-right lobe) is laid along the mask edge. A ±0.5/255 dither
/// removes 8-bit banding. Everything is in physical pixels with a y-down axis
/// (`y_sign` undoes the framebuffer mirroring of [`fb_y_mirrored`]), so the
/// texture offset is just the pixel offset times `px_uv`.
///
/// A window bezel (`inner` > 0) is the same glass laid as a ring of that width
/// around the window: the roll-off spans only the ring, so nothing under the
/// window is ever bent, a faint dark seam marks where glass meets content, and
/// the hairline takes the window's border colour (`rim_color`, premultiplied,
/// its alpha the line's strength). Over an opaque window nothing inside the
/// ring is drawn at all. Off the bezel `rim_color` is opaque white, which is
/// the neutral hairline unchanged.
const GLASS_BODY: &str = r#"
uniform vec2 px_uv;       // 1 / backdrop texture size
uniform float y_sign;     // -1 when the framebuffer is y-mirrored
uniform float strength;   // largest displacement, physical px, at the edge
uniform float bevel;      // roll-off band width, physical px
uniform float dispersion; // 0..1, off by default
uniform float rim;        // 0..1 hairline strength
uniform vec4 rim_color;   // premultiplied hairline colour; opaque white off the bezel
uniform float inner;      // bezel width, physical px; 0 off the bezel
uniform float opaque;     // 1 when the window inside the bezel is opaque

const vec2 LIGHT = vec2(-0.70710678, -0.70710678); // top-left, y down
const vec3 LUMA = vec3(0.2126, 0.7152, 0.0722);    // Rec.709
const float SATURATE = 1.7;
const float COMPRESS = 1.0;  // highlight knee: c / (1 + COMPRESS * luma)
const vec3 SMOKE = vec3(0.047, 0.047, 0.055);       // #0c0c0e
const float SMOKE_MIX = 0.28;
// Hairline alphas at rim 0.5: all-round base, top-left and bottom-right lobes.
const float LINE_BASE = 0.05;
const float LINE_TL = 0.13;
const float LINE_BR = 0.035;
// Bezel hairline, times the border colour's alpha: a crisp line all round,
// brightest top-left.
const float BEZEL_BASE = 0.4;
const float BEZEL_TL = 0.5;
const float BEZEL_BR = 0.1;
// Darkening of the 1 px seam where the bezel meets the window.
const float SEAM = 0.3;

vec4 blurred_at(vec2 uv) {
    return texture2D(tex, clamp(uv, px_uv * 0.5, vec2(1.0) - px_uv * 0.5));
}

#ifdef SHAPED
// Outward normal of the shape, y down, by central differences.
vec2 shape_grad(vec2 f) {
    vec2 g = vec2(shape_sd(f + vec2(0.5, 0.0)) - shape_sd(f - vec2(0.5, 0.0)),
                  shape_sd(f + vec2(0.0, 0.5)) - shape_sd(f - vec2(0.0, 0.5)));
    g.y *= y_sign;
    return length(g) > 0.01 ? normalize(g) : vec2(0.0, -1.0);
}
#endif

void main() {
#ifdef SHAPED
    float d = shape_sd(gl_FragCoord.xy);
#else
    vec2 half_size = win_rect.zw * 0.5;
    vec2 p = gl_FragCoord.xy - (win_rect.xy + half_size);
    p.y *= y_sign;
    float r = min(radius, min(half_size.x, half_size.y));
    vec2 q = abs(p) - half_size + r;
    float d = length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
#endif
    // Under an opaque window only the ring (and the window's own antialiased
    // edge) can show.
    if (opaque > 0.5 && d < -inner - 1.0) {
        gl_FragColor = vec4(0.0);
        return;
    }

#ifdef SHAPED
    vec2 grad = shape_grad(gl_FragCoord.xy);
#else
    // Outward normal of the rounded box, y down.
    vec2 s = vec2(p.x < 0.0 ? -1.0 : 1.0, p.y < 0.0 ? -1.0 : 1.0);
    vec2 grad;
    if (q.x > 0.0 && q.y > 0.0) {
        grad = normalize(q);
    } else if (q.x > q.y) {
        grad = vec2(1.0, 0.0);
    } else {
        grad = vec2(0.0, 1.0);
    }
    grad *= s;
#endif

    // Roll-off: largest at the edge, easing to zero (and zero slope) where
    // the bevel meets the plateau, so no line marks where the bend stops.
    // A bezel bends across its own width only.
    float band_w = inner > 0.0 ? inner : bevel;
    float u = 1.0 - clamp(-d / max(band_w, 1.0), 0.0, 1.0);
    vec2 bend = -grad * px_uv * (strength * u * u);
    vec4 color = blurred_at(v_coords + bend);
    if (dispersion > 0.0) {
        float spread = 0.05 * dispersion;
        color.r = blurred_at(v_coords + bend * (1.0 - spread)).r;
        color.b = blurred_at(v_coords + bend * (1.0 + spread)).b;
    }

    // Vibrancy: saturate, compress the highlights, smoke toward neutral.
    vec3 c = color.rgb;
    c = max(mix(vec3(dot(c, LUMA)), c, SATURATE), 0.0);
    c = c / (1.0 + COMPRESS * dot(c, LUMA));
    c = mix(c, SMOKE, SMOKE_MIX);

    // Hairline: a 1.25 px band inside the mask edge.
    float band = 1.0 - smoothstep(0.0, 1.25, -d);
    float tl = max(dot(grad, LIGHT), 0.0);
    float br = max(dot(grad, -LIGHT), 0.0);
    float lobe = inner > 0.0
        ? BEZEL_BASE + BEZEL_TL * tl + BEZEL_BR * br
        : LINE_BASE + LINE_TL * tl + LINE_BR * br;
    vec3 tint = rim_color.rgb / max(rim_color.a, 0.0001);
    c = mix(c, tint, clamp(band * lobe * rim_color.a * rim * 2.0, 0.0, 1.0));

    // Seam: a faint dark line just outside the window's own edge.
    if (inner > 0.0) {
        c *= 1.0 - SEAM * max(1.0 - abs(d + inner - 0.5), 0.0);
    }

    c += (grain(floor(gl_FragCoord.xy)) - 0.5) / 255.0;
    color = finish(vec4(clamp(c, 0.0, 1.0), color.a));

    gl_FragColor = color * (1.0 - smoothstep(-0.5, 0.5, d));
}
"#;

/// Compile the frost backdrop program (COMP-02 §9). Cached by the caller.
pub fn compile_frost(renderer: &mut GlesRenderer) -> Result<GlesTexProgram, GlesError> {
    renderer.compile_custom_texture_shader(
        format!("{BACKDROP_HEAD}{FROST_BODY}"),
        &[
            UniformName::new("win_rect", UniformType::_4f),
            UniformName::new("radius", UniformType::_1f),
            UniformName::new("frost_tint", UniformType::_4f),
        ],
    )
}

/// The glass program's uniforms, in the order [`glass_uniforms`] sets them.
const GLASS_UNIFORMS: [(&str, UniformType); 11] = [
    ("win_rect", UniformType::_4f),
    ("radius", UniformType::_1f),
    ("px_uv", UniformType::_2f),
    ("y_sign", UniformType::_1f),
    ("strength", UniformType::_1f),
    ("bevel", UniformType::_1f),
    ("dispersion", UniformType::_1f),
    ("rim", UniformType::_1f),
    ("rim_color", UniformType::_4f),
    ("inner", UniformType::_1f),
    ("opaque", UniformType::_1f),
];

/// Compile the glass backdrop program (COMP-02 §9). Cached by the caller.
pub fn compile_glass(renderer: &mut GlesRenderer) -> Result<GlesTexProgram, GlesError> {
    let names: Vec<_> = GLASS_UNIFORMS
        .iter()
        .map(|&(name, ty)| UniformName::new(name, ty))
        .collect();
    renderer.compile_custom_texture_shader(format!("{BACKDROP_HEAD}{GLASS_BODY}"), &names)
}

/// [`rounding_uniforms`] plus the frost tint (straight alpha, as configured).
pub fn frost_uniforms(
    rect: Rectangle<i32, Physical>,
    fb_height: i32,
    mirrored: bool,
    radius: f32,
    tint: [f32; 4],
) -> Vec<Uniform<'static>> {
    let mut u = rounding_uniforms(rect, fb_height, mirrored, radius);
    u.push(Uniform::new("frost_tint", tint));
    u
}

/// [`rounding_uniforms`] plus the glass parameters. `tex_size` is the
/// backdrop texture (the whole output, physical px); `strength` and `bevel`
/// are physical px. `bezel` is a window bezel's width (physical px), whether
/// the window inside it is opaque, and its premultiplied hairline colour;
/// `None` is plain glass with the neutral white hairline.
#[allow(clippy::too_many_arguments)]
pub fn glass_uniforms(
    rect: Rectangle<i32, Physical>,
    fb_height: i32,
    mirrored: bool,
    radius: f32,
    tex_size: (i32, i32),
    strength: f32,
    bevel: f32,
    glass: &ec_abyss_config::GlassBlur,
    bezel: Option<(i32, bool, [f32; 4])>,
) -> Vec<Uniform<'static>> {
    let (inner, opaque, rim_color) = bezel.map_or((0.0, 0.0, [1.0; 4]), |(inner, opaque, rim)| {
        (inner.max(0) as f32, if opaque { 1.0f32 } else { 0.0 }, rim)
    });
    let mut u = rounding_uniforms(rect, fb_height, mirrored, radius);
    u.extend([
        Uniform::new(
            "px_uv",
            [1.0 / tex_size.0.max(1) as f32, 1.0 / tex_size.1.max(1) as f32],
        ),
        Uniform::new("y_sign", if mirrored { -1.0f32 } else { 1.0 }),
        Uniform::new("strength", strength),
        Uniform::new("bevel", bevel),
        Uniform::new("dispersion", glass.dispersion),
        Uniform::new("rim", glass.rim),
        Uniform::new("rim_color", rim_color),
        Uniform::new("inner", inner),
        Uniform::new("opaque", opaque),
    ]);
    u
}

/// How far past the blurred region the glass program samples, physical px:
/// its displacement, with a margin for the widest dispersion (up to 5%
/// further). Zero for every other mode.
pub fn glass_reach(refraction: i32, scale: f64) -> i32 {
    (refraction.max(0) as f64 * scale * 1.05).ceil() as i32
}

/// How the output's physical space maps onto `gl_FragCoord` for a frame
/// rendered with `transform`, or `None` for a transform the mask does not
/// handle (a rotated output keeps square corners rather than drawing the mask
/// in the wrong place).
///
/// `Some(true)` means the vertical axis is mirrored: physical `y` lands at
/// `fb_height - y`. `GlesRenderer::render` builds its projection as
/// `flip180 * transform.matrix() * ortho`, where `ortho` maps physical `y = 0`
/// to the top of clip space and `flip180` negates it again. Under `Normal`
/// (DRM) that leaves physical `y = 0` at `gl_FragCoord.y = 0`, so the two
/// origins coincide; `Flipped180` (winit) cancels `flip180`, so physical
/// `y = 0` lands at `gl_FragCoord.y = fb_height` and the rect must be mirrored.
pub fn fb_y_mirrored(transform: Transform) -> Option<bool> {
    match transform {
        Transform::Normal => Some(false),
        Transform::Flipped180 => Some(true),
        _ => None,
    }
}

/// The uniform pair describing one window's rounded rectangle.
///
/// `rect` is in physical output pixels with a top-left origin (what the render
/// elements use). `mirrored` (from [`fb_y_mirrored`]) says whether the frame's
/// projection puts that origin at the bottom of the framebuffer (`false`, the
/// two already coincide) or at the top (`true`, so `rect` is mirrored against
/// `fb_height` to reach `gl_FragCoord` space).
pub fn rounding_uniforms(
    rect: Rectangle<i32, Physical>,
    fb_height: i32,
    mirrored: bool,
    radius: f32,
) -> Vec<Uniform<'static>> {
    let x = rect.loc.x as f32;
    let y = if mirrored {
        (fb_height - rect.loc.y - rect.size.h) as f32
    } else {
        rect.loc.y as f32
    };
    vec![
        Uniform::new("win_rect", [x, y, rect.size.w as f32, rect.size.h as f32]),
        Uniform::new("radius", radius),
    ]
}

/// A surface element drawn with the rounded-corner program bound.
///
/// Everything but `draw` delegates to the wrapped element, so damage tracking
/// is unchanged; `opaque_regions` is emptied because a rounded window is not
/// opaque at its corners, and `underlying_storage` returns `None` so the DRM
/// backend never promotes it to a plane and skips the mask.
#[derive(Debug)]
pub struct RoundedElement {
    inner: WaylandSurfaceRenderElement<GlesRenderer>,
    program: GlesTexProgram,
    uniforms: Vec<Uniform<'static>>,
}

impl RoundedElement {
    pub fn new(
        inner: WaylandSurfaceRenderElement<GlesRenderer>,
        program: GlesTexProgram,
        uniforms: Vec<Uniform<'static>>,
    ) -> Self {
        Self {
            inner,
            program,
            uniforms,
        }
    }
}

impl Element for RoundedElement {
    fn id(&self) -> &Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.inner.location(scale)
    }

    fn src(&self) -> Rectangle<f64, BufferCoords> {
        self.inner.src()
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }

    fn damage_since(&self, scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl RenderElement<GlesRenderer> for RoundedElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, BufferCoords>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        frame.override_default_tex_program(self.program.clone(), self.uniforms.clone());
        let res = RenderElement::<GlesRenderer>::draw(&self.inner, frame, src, dst, damage, opaque_regions);
        frame.clear_tex_program_override();
        res
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

/// The glow ring: a smoothstep falloff around the bordered rect, symmetric,
/// in the premultiplied `shadow_color` (COMP-02 §9).
const RING_SRC: &str = r#"
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif

uniform vec2 size;
uniform float alpha;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform vec4 shadow_color;
uniform float blur;
uniform float radius;

void main() {
    // The element is the window rect grown by `blur` on every side, so the
    // window itself is the inner rectangle inset by that much.
    vec2 p = v_coords * size - size * 0.5;
    vec2 half_inner = max(size * 0.5 - vec2(blur), vec2(0.0));
    float r = min(radius, min(half_inner.x, half_inner.y));
    vec2 q = abs(p) - half_inner + r;
    float d = length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;

    // `step` drops everything under the window: the glow is a ring, so a
    // translucent window is not tinted by its own glow.
    float a = (1.0 - smoothstep(0.0, blur, d)) * step(0.0, d);
    gl_FragColor = shadow_color * (a * alpha);
}
"#;

/// Compile the glow ring program (COMP-02 §9).
pub fn compile_ring(renderer: &mut GlesRenderer) -> Result<GlesPixelProgram, GlesError> {
    renderer.compile_custom_pixel_shader(
        RING_SRC,
        &[
            UniformName::new("shadow_color", UniformType::_4f),
            UniformName::new("blur", UniformType::_1f),
            UniformName::new("radius", UniformType::_1f),
        ],
    )
}

/// Uniforms for one glow ring over its area (the bordered rect grown by
/// `range`): `color` is its premultiplied colour against the window edge,
/// `range` how far it reaches and `radius` the corner radius it hugs, both
/// logical.
pub fn ring_uniforms(color: [f32; 4], range: f32, radius: f32) -> Vec<Uniform<'static>> {
    vec![
        // Premultiplied, so the colour carries its own alpha.
        Uniform::new("shadow_color", color),
        Uniform::new("blur", range),
        Uniform::new("radius", radius),
    ]
}

/// The drop shadow (COMP-02 §9): a tight contact layer and a wide ambient
/// layer, each a gaussian-blurred rounded box offset downward, so the pane
/// reads as lifted rather than outlined. Both are tapered to zero before the
/// element edge, and nothing is drawn under the (unshifted) window, so a
/// translucent window is not darkened by its own shadow. Worked in logical
/// px, y down; the element is the bordered rect grown by `range` on every
/// side and by `drop` more at the bottom.
const SHADOW_SRC: &str = r#"
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif

uniform vec2 size;
uniform float alpha;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform float range;
uniform float radius;
uniform float drop;
uniform float focus; // 0 unfocused .. 1 focused, crossfading with the border

float rounded_box(vec2 p, vec2 half_size, float r) {
    vec2 q = abs(p) - half_size + r;
    return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

// Winitzki's erf approximation; smooth, no tables.
float erf_approx(float x) {
    float x2 = x * x;
    float e = exp(-x2 * (1.2732395 + 0.147 * x2) / (1.0 + 0.147 * x2));
    return sign(x) * sqrt(max(1.0 - e, 0.0));
}

// Coverage of a rounded box blurred by a gaussian of `sigma`, at distance `d`.
float soft(float d, float sigma) {
    return 0.5 - 0.5 * erf_approx(d / (sigma * 1.41421356));
}

void main() {
    vec2 p = v_coords * size;
    vec2 centre = vec2(size.x * 0.5, (size.y - drop) * 0.5);
    vec2 half_size = max(centre - vec2(range), vec2(0.0));
    float r = min(radius, min(half_size.x, half_size.y));
    float d_in = rounded_box(p - centre, half_size, r);

    float contact_sigma = max(range * mix(0.07, 0.09, focus), 0.75);
    float ambient_sigma = max(range * mix(0.24, 0.33, focus), 1.0);
    float d_contact = rounded_box(p - centre - vec2(0.0, drop * 0.2), half_size, r);
    float d_ambient = rounded_box(p - centre - vec2(0.0, drop), half_size, r);
    float contact = soft(d_contact, contact_sigma) * mix(0.16, 0.22, focus);
    float ambient = soft(d_ambient, ambient_sigma) * mix(0.26, 0.42, focus);
    float a = 1.0 - (1.0 - contact) * (1.0 - ambient);

    // Zero at the element edge (`d_ambient` >= range all round it).
    a *= 1.0 - smoothstep(range * 0.5, range, d_ambient);
    a *= step(0.0, d_in);
    gl_FragColor = vec4(0.0, 0.0, 0.0, a * alpha);
}
"#;

/// Compile the drop-shadow program (COMP-02 §9).
pub fn compile_shadow(renderer: &mut GlesRenderer) -> Result<GlesPixelProgram, GlesError> {
    renderer.compile_custom_pixel_shader(
        SHADOW_SRC,
        &[
            UniformName::new("range", UniformType::_1f),
            UniformName::new("radius", UniformType::_1f),
            UniformName::new("drop", UniformType::_1f),
            UniformName::new("focus", UniformType::_1f),
        ],
    )
}

/// Uniforms for one drop shadow: `[range, radius, drop]` from
/// `shadow_geometry` (logical px) and the 0..1 `focus` weight.
pub fn shadow_uniforms([range, radius, drop]: [f32; 3], focus: f32) -> Vec<Uniform<'static>> {
    vec![
        Uniform::new("range", range),
        Uniform::new("radius", radius),
        Uniform::new("drop", drop),
        Uniform::new("focus", focus),
    ]
}

/// How far the shadow drops below the window, as a share of its `range`.
/// Not configurable: `shadow { range }` is the only knob COMP-13 §1.1 gives.
pub const SHADOW_DROP: f32 = 0.3;

const BORDER_SRC: &str = r#"
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif

uniform vec2 size;
uniform float alpha;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform vec4 color;
uniform float radius;
uniform float width;
uniform float scale;

float rounded_box(vec2 p, vec2 half_size, float r) {
    vec2 q = abs(p) - half_size + r;
    return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

void main() {
    // Worked in physical pixels, so the inner edge is the exact complement of
    // the window's own mask (same radius, same half-pixel smoothstep).
    vec2 half_out = size * 0.5 * scale;
    vec2 p = v_coords * size * scale - half_out;
    float w = width * scale;
    vec2 half_in = max(half_out - vec2(w), vec2(0.0));
    float r_in = min(radius * scale, min(half_in.x, half_in.y));
    float r_out = min(r_in + w, min(half_out.x, half_out.y));

    float outer = 1.0 - smoothstep(-0.5, 0.5, rounded_box(p, half_out, r_out));
    float inner = smoothstep(-0.5, 0.5, rounded_box(p, half_in, r_in));
    gl_FragColor = color * (outer * inner * alpha);
}
"#;

/// Compile the rounded border-ring program (COMP-02 §9).
pub fn compile_border(renderer: &mut GlesRenderer) -> Result<GlesPixelProgram, GlesError> {
    renderer.compile_custom_pixel_shader(
        BORDER_SRC,
        &[
            UniformName::new("color", UniformType::_4f),
            UniformName::new("radius", UniformType::_1f),
            UniformName::new("width", UniformType::_1f),
            UniformName::new("scale", UniformType::_1f),
        ],
    )
}

/// Uniforms for one border ring: `radius` is the window's own corner radius
/// (the ring's inner edge) and `width` the border size, both logical; the
/// outer radius is `radius + width`. `scale` is the output scale the window
/// mask multiplies its radius by, so both edges land on the same pixels.
pub fn border_uniforms(color: [f32; 4], radius: f32, width: f32, scale: f32) -> Vec<Uniform<'static>> {
    vec![
        // Premultiplied, like `SolidColorBuffer`.
        Uniform::new("color", color),
        Uniform::new("radius", radius),
        Uniform::new("width", width),
        Uniform::new("scale", scale),
    ]
}

/// How far the border glow reaches past the border, logical px. Not
/// configurable: `glow { strength }` scales intensity only (COMP-13 §1.1).
pub const GLOW_RANGE: i32 = 24;

#[cfg(test)]
mod tests {
    use super::*;

    fn win_rect(uniforms: &[Uniform<'static>]) -> String {
        format!("{:?}", uniforms[0])
    }

    #[test]
    fn glass_sets_exactly_the_uniforms_it_compiles() {
        // smithay rejects a uniform the program was not compiled with
        // (`UnknownUniform`), and leaves one it was compiled with but never
        // set at zero; the two lists must match name for name.
        let rect = Rectangle::new((0, 0).into(), (300, 200).into());
        let got = glass_uniforms(
            rect,
            1080,
            false,
            13.0,
            (1920, 1080),
            4.0,
            16.0,
            &ec_abyss_config::GlassBlur::default(),
            None,
        );
        let set: Vec<&str> = got.iter().map(|u| &*u.name).collect();
        let compiled: Vec<&str> = GLASS_UNIFORMS.iter().map(|&(name, _)| name).collect();
        assert_eq!(set, compiled);
        // A bezel sets the same list, not a longer one.
        let bezel = glass_uniforms(
            rect,
            1080,
            false,
            19.0,
            (1920, 1080),
            4.0,
            6.0,
            &ec_abyss_config::GlassBlur::default(),
            Some((6, true, [0.43, 0.34, 0.11, 0.45])),
        );
        let set: Vec<&str> = bezel.iter().map(|u| &*u.name).collect();
        assert_eq!(set, compiled);
    }

    /// The shaped programs splice the shape in exactly once; the plain ones
    /// never define `SHAPED`, so they preprocess to the single-box program.
    #[test]
    fn only_the_shaped_sources_define_the_shape() {
        let plain = [
            ROUNDED_TEX_SRC.to_string(),
            format!("{BACKDROP_HEAD}{FROST_BODY}"),
            format!("{BACKDROP_HEAD}{GLASS_BODY}"),
        ];
        for src in &plain {
            assert_eq!(src.matches(RADIUS_DECL).count(), 1);
            assert!(!src.contains("#define SHAPED"));
            let shaped = shaped_source(src);
            assert_eq!(shaped.matches("#define SHAPED").count(), 1);
            assert_eq!(shaped.matches("float shape_sd(").count(), 1);
            // Defined before any body uses it.
            assert!(shaped.find("#define SHAPED").unwrap() < shaped.find("#ifdef SHAPED").unwrap());
        }
    }

    #[test]
    fn shaped_glass_sets_exactly_the_uniforms_it_compiles() {
        let region = smithay::wayland::compositor::RegionAttributes {
            rects: vec![
                (
                    smithay::wayland::compositor::RectangleKind::Add,
                    Rectangle::new((0, 0).into(), (400, 40).into()),
                ),
                (
                    smithay::wayland::compositor::RectangleKind::Add,
                    Rectangle::new((100, 40).into(), (200, 200).into()),
                ),
            ],
        };
        let shape = crate::blur::Shape::from_region(
            Some(&region),
            (400, 300).into(),
            (0, 0).into(),
            Scale::from(1.0),
            20.0,
        )
        .unwrap();
        let rect = Rectangle::new((0, 0).into(), (400, 300).into());
        let mut got = glass_uniforms(
            rect,
            1080,
            true,
            20.0,
            (1920, 1080),
            4.0,
            16.0,
            &ec_abyss_config::GlassBlur::default(),
            None,
        );
        got.extend(shape_uniforms(&shape, 1080, true));
        let set: Vec<String> = got.iter().map(|u| u.name.to_string()).collect();
        let compiled: Vec<String> = with_shape(&GLASS_UNIFORMS)
            .iter()
            .map(|u| u.name.to_string())
            .collect();
        assert_eq!(set, compiled);
        // Boxes are mirrored into gl_FragCoord space like `win_rect`; the
        // unused slots are zero and `boxes` counts the live ones.
        let find = |name: &str| format!("{:?}", got.iter().find(|u| u.name == name).unwrap());
        assert_eq!(
            find("box1"),
            format!("{:?}", Uniform::new("box1", [100.0f32, 840.0, 200.0, 200.0]))
        );
        assert_eq!(find("box2"), format!("{:?}", Uniform::new("box2", [0.0f32; 4])));
        assert_eq!(find("boxes"), format!("{:?}", Uniform::new("boxes", 2.0f32)));
        // A different shape is a different look (it bumps the backdrop's commit).
        let other = crate::blur::Shape::from_region(
            Some(&region),
            (400, 100).into(),
            (0, 0).into(),
            Scale::from(1.0),
            20.0,
        )
        .unwrap();
        assert_ne!(
            format!("{:?}", shape_uniforms(&shape, 1080, true)),
            format!("{:?}", shape_uniforms(&other, 1080, true))
        );
    }

    #[test]
    fn plain_glass_keeps_the_white_hairline() {
        let rect = Rectangle::new((0, 0).into(), (300, 200).into());
        let got = glass_uniforms(
            rect,
            1080,
            false,
            13.0,
            (1920, 1080),
            4.0,
            16.0,
            &ec_abyss_config::GlassBlur::default(),
            None,
        );
        let find = |name: &str| format!("{:?}", got.iter().find(|u| u.name == name).unwrap());
        assert_eq!(
            find("rim_color"),
            format!("{:?}", Uniform::new("rim_color", [1.0f32; 4]))
        );
        assert_eq!(find("inner"), format!("{:?}", Uniform::new("inner", 0.0f32)));
        assert_eq!(find("opaque"), format!("{:?}", Uniform::new("opaque", 0.0f32)));
    }

    #[test]
    fn the_shader_sources_keep_one_grain() {
        // `grain` lives in the shared head; a second copy in a body would be
        // a redefinition and the program would fail to compile.
        for body in [FROST_BODY, GLASS_BODY] {
            let src = format!("{BACKDROP_HEAD}{body}");
            assert_eq!(src.matches("float grain(").count(), 1);
        }
    }

    #[test]
    fn normal_output_keeps_the_top_left_origin() {
        // DRM: physical y = 0 is gl_FragCoord.y = 0, so the rect is unchanged.
        let rect = Rectangle::new((10, 20).into(), (300, 200).into());
        let mirrored = fb_y_mirrored(Transform::Normal).unwrap();
        let got = rounding_uniforms(rect, 1080, mirrored, 13.0);
        let want = vec![Uniform::new("win_rect", [10.0f32, 20.0, 300.0, 200.0])];
        assert_eq!(win_rect(&got), win_rect(&want));
    }

    #[test]
    fn flipped180_output_mirrors_the_rect() {
        // winit: physical y = 0 is gl_FragCoord.y = fb_height.
        let rect = Rectangle::new((10, 20).into(), (300, 200).into());
        let mirrored = fb_y_mirrored(Transform::Flipped180).unwrap();
        let got = rounding_uniforms(rect, 1080, mirrored, 13.0);
        let want = vec![Uniform::new("win_rect", [10.0f32, 860.0, 300.0, 200.0])];
        assert_eq!(win_rect(&got), win_rect(&want));
    }

    #[test]
    fn rotated_outputs_are_not_masked() {
        for t in [
            Transform::_90,
            Transform::_180,
            Transform::_270,
            Transform::Flipped,
        ] {
            assert_eq!(fb_y_mirrored(t), None);
        }
    }
}
