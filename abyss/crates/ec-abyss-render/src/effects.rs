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
use smithay::utils::{Buffer as BufferCoords, Physical, Point, Rectangle, Scale, Size, Transform};

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

#ifdef KEYED
// Tint-keyed translucency (ADR 0079): inside `key_rect` (gl_FragCoord space)
// a texel whose colour is within 1/255 per channel of key `i` is drawn at
// that key's opacity (`key{i}.w`); every other texel is untouched.
uniform vec4 key_rect;
uniform float keys;
uniform vec4 key0;
uniform vec4 key1;
uniform vec4 key2;
uniform vec4 key3;
uniform vec4 key_all; // 1.0 per key matched over the whole window

bool near_key(vec3 c, vec3 k, float tol) {
    return all(lessThanEqual(abs(c - k) * 255.0, vec3(tol)));
}

float key_opacity(vec4 texel) {
    vec2 f = gl_FragCoord.xy;
    bool inside = !(f.x < key_rect.x || f.y < key_rect.y || f.x >= key_rect.x + key_rect.z || f.y >= key_rect.y + key_rect.w);
    // The client's own anti-aliased edge (Firefox rounds its corners itself)
    // is a key colour at partial alpha, premultiplied. Match it on the
    // straight colour, with the tolerance widened by what 8-bit
    // premultiplication loses, so the edge fades like the pixels beside it
    // instead of staying opaque as a dark speck.
    float tol = 1.5;
    vec3 rgb = texel.rgb;
#if !defined(NO_ALPHA)
    if (texel.a < 0.2)
        return 1.0;
    if (texel.a < 0.99) {
        rgb = texel.rgb / texel.a;
        tol = 1.5 / texel.a;
    }
#endif
    if (keys > 0.5 && (inside || key_all.x > 0.5) && near_key(rgb, key0.rgb, tol))
        return key0.w;
    if (keys > 1.5 && (inside || key_all.y > 0.5) && near_key(rgb, key1.rgb, tol))
        return key1.w;
    if (keys > 2.5 && (inside || key_all.z > 0.5) && near_key(rgb, key2.rgb, tol))
        return key2.w;
    if (keys > 3.5 && (inside || key_all.w > 0.5) && near_key(rgb, key3.rgb, tol))
        return key3.w;
    return 1.0;
}
#endif

void main() {
    vec4 color = texture2D(tex, v_coords);
#ifdef KEYED
    float key_k = key_opacity(color);
#endif

#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0) * alpha;
#else
    color = color * alpha;
#endif
#ifdef KEYED
    color *= key_k;
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

/// The keyed program: the rounded-corner program with tint keying switched on
/// (`KEYED`). Surfaces of a window with a keyed strip, and of its popups, draw
/// through it; it is always handed every uniform, so nothing leaks from one
/// draw to the next. Cached by the caller.
pub fn compile_keyed(renderer: &mut GlesRenderer) -> Result<GlesTexProgram, GlesError> {
    renderer.compile_custom_texture_shader(
        ROUNDED_TEX_SRC.replacen("//_DEFINES_", "//_DEFINES_\n#define KEYED 1", 1),
        &[
            UniformName::new("win_rect", UniformType::_4f),
            UniformName::new("radius", UniformType::_1f),
            UniformName::new("key_rect", UniformType::_4f),
            UniformName::new("keys", UniformType::_1f),
            UniformName::new("key0", UniformType::_4f),
            UniformName::new("key1", UniformType::_4f),
            UniformName::new("key2", UniformType::_4f),
            UniformName::new("key3", UniformType::_4f),
            UniformName::new("key_all", UniformType::_4f),
        ],
    )
}

/// Keyed lens (ADR 0132), pass 1: the key test of [`ROUNDED_TEX_SRC`]'s KEYED
/// branch, run over the client's own pixels into a label map. The label is
/// stored in the blue channel as `label / 4`: 0 for no lens, `i + 1` for a
/// matching key `i` that is drawn as a lens. The first matching key decides,
/// as it does for the flat tint. Mirrored by [`crate::keyed_lens::label_of`].
const KEY_LABEL_SRC: &str = r#"#version 100

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

uniform vec4 key_rect;
uniform float keys;
uniform vec4 key0;
uniform vec4 key1;
uniform vec4 key2;
uniform vec4 key3;
uniform vec4 key_all;
uniform vec4 key_lens;

bool near_key(vec3 c, vec3 k, float tol) {
    return all(lessThanEqual(abs(c - k) * 255.0, vec3(tol)));
}

void main() {
    vec4 texel = texture2D(tex, v_coords);
    vec3 rgb = texel.rgb;
    float tol = 1.5;
    // An X-format buffer's alpha byte is undefined, so smithay's NO_ALPHA
    // variant must not read it (the keyed draw's own test does the same).
#if !defined(NO_ALPHA)
    if (texel.a < 0.2) {
        gl_FragColor = vec4(0.0, 0.0, 0.0, 1.0);
        return;
    }
    if (texel.a < 0.99) {
        rgb = texel.rgb / texel.a;
        tol = 1.5 / texel.a;
    }
#endif
    vec2 f = gl_FragCoord.xy;
    bool inside = !(f.x < key_rect.x || f.y < key_rect.y || f.x >= key_rect.x + key_rect.z || f.y >= key_rect.y + key_rect.w);
    float m = -1.0;
    if (keys > 0.5 && (inside || key_all.x > 0.5) && near_key(rgb, key0.rgb, tol)) {
        m = 0.0;
    } else if (keys > 1.5 && (inside || key_all.y > 0.5) && near_key(rgb, key1.rgb, tol)) {
        m = 1.0;
    } else if (keys > 2.5 && (inside || key_all.z > 0.5) && near_key(rgb, key2.rgb, tol)) {
        m = 2.0;
    } else if (keys > 3.5 && (inside || key_all.w > 0.5) && near_key(rgb, key3.rgb, tol)) {
        m = 3.0;
    }
    float lens = m < 0.5 ? key_lens.x : m < 1.5 ? key_lens.y : m < 2.5 ? key_lens.z : key_lens.w;
    float label = (m >= 0.0 && lens > 0.5) ? m + 1.0 : 0.0;
    gl_FragColor = vec4(0.0, 0.0, label / 4.0, 1.0);
}
"#;

/// Keyed lens (ADR 0132), pass 2: the distance from each lens texel to the
/// nearest texel of another label, by marching eight rays of integer steps out
/// to `dist_max` px. Writes the distance (over `dist_max`) to red and the label
/// (blue, as pass 1) with alpha 1, so the target needs no clearing. Nothing
/// beyond `dist_max` is measured: a texel that far from an edge is the plateau.
const KEY_DISTANCE_SRC: &str = r#"#version 100

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

uniform vec2 texel;
uniform float dist_max;

float label_at(vec2 uv) {
    if (uv.x < 0.0 || uv.y < 0.0 || uv.x > 1.0 || uv.y > 1.0) {
        return 0.0;
    }
    return floor(texture2D(tex, uv).b * 4.0 + 0.5);
}

void main() {
    float l0 = label_at(v_coords);
    float best = dist_max;
    if (l0 > 0.5) {
        for (int k = 0; k < 8; k++) {
            vec2 dir = k == 0 ? vec2(1.0, 0.0) : k == 1 ? vec2(1.0, 1.0)
                : k == 2 ? vec2(0.0, 1.0) : k == 3 ? vec2(-1.0, 1.0)
                : k == 4 ? vec2(-1.0, 0.0) : k == 5 ? vec2(-1.0, -1.0)
                : k == 6 ? vec2(0.0, -1.0) : vec2(1.0, -1.0);
            float step_len = (k == 1 || k == 3 || k == 5 || k == 7) ? 1.41421356 : 1.0;
            for (int s = 1; s <= 32; s++) {
                float fs = float(s) * step_len;
                if (fs >= best) {
                    break;
                }
                if (label_at(v_coords + dir * float(s) * texel) != l0) {
                    best = fs;
                    break;
                }
            }
        }
    }
    // The boundary lies half a texel beyond the last texel of the same label.
    float dist = max(best - 0.5, 0.0);
    gl_FragColor = vec4(clamp(dist / dist_max, 0.0, 1.0), 0.0, l0 / 4.0, 1.0);
}
"#;

/// The ray length of the keyed lens's distance field, physical px (ADR 0132).
pub const KEYED_LENS_DIST_MAX: f32 = 32.0;

/// Keyed lens (ADR 0132), pass 1's program. Cached by the caller.
pub fn compile_key_label(renderer: &mut GlesRenderer) -> Result<GlesTexProgram, GlesError> {
    renderer.compile_custom_texture_shader(
        KEY_LABEL_SRC,
        &[
            UniformName::new("key_rect", UniformType::_4f),
            UniformName::new("keys", UniformType::_1f),
            UniformName::new("key0", UniformType::_4f),
            UniformName::new("key1", UniformType::_4f),
            UniformName::new("key2", UniformType::_4f),
            UniformName::new("key3", UniformType::_4f),
            UniformName::new("key_all", UniformType::_4f),
            UniformName::new("key_lens", UniformType::_4f),
        ],
    )
}

/// Keyed lens (ADR 0132), pass 2's program. Cached by the caller.
pub fn compile_key_distance(renderer: &mut GlesRenderer) -> Result<GlesTexProgram, GlesError> {
    renderer.compile_custom_texture_shader(
        KEY_DISTANCE_SRC,
        &[
            UniformName::new("texel", UniformType::_2f),
            UniformName::new("dist_max", UniformType::_1f),
        ],
    )
}

/// Uniforms for [`compile_key_label`]: the key colours and flags exactly as
/// [`keyed_uniforms`] sets them, plus which keys are lenses. `key_rect` is the
/// strip in the label target's own `gl_FragCoord` space (`EVERYWHERE` for a
/// popup, where the whole target is the key region).
pub fn key_label_uniforms(key_rect: [f32; 4], keys: &crate::userdata::Keys) -> Vec<Uniform<'static>> {
    let key = |i: usize| -> [f32; 4] {
        if i < keys.n as usize {
            let [r, g, b] = keys.rgb[i];
            [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
        } else {
            [0.0; 4]
        }
    };
    let flag = |i: usize| (i < keys.n as usize && keys.lens[i]) as u8 as f32;
    let all = |i: usize| (i < keys.n as usize && keys.everywhere[i]) as u8 as f32;
    vec![
        Uniform::new("key_rect", key_rect),
        Uniform::new("keys", keys.n as f32),
        Uniform::new("key0", key(0)),
        Uniform::new("key1", key(1)),
        Uniform::new("key2", key(2)),
        Uniform::new("key3", key(3)),
        Uniform::new("key_all", [all(0), all(1), all(2), all(3)]),
        Uniform::new("key_lens", [flag(0), flag(1), flag(2), flag(3)]),
    ]
}

/// Uniforms for [`compile_key_distance`].
pub fn key_distance_uniforms(texel: [f32; 2], dist_max: f32) -> Vec<Uniform<'static>> {
    vec![Uniform::new("texel", texel), Uniform::new("dist_max", dist_max)]
}

/// The uniforms a keyed lens adds to a glass program's own (ADR 0132):
/// `round` is the window's rounded rectangle in `gl_FragCoord` space with
/// corner `radius` (its coverage cuts the lens at the window's corners), and
/// `key_alpha[i]` is key `i`'s opacity, which the lens's label selects.
pub fn keyed_lens_uniforms(
    round: [f32; 4],
    radius: f32,
    keys: &crate::userdata::Keys,
    dist_max: f32,
) -> Vec<Uniform<'static>> {
    let a = |i: usize| if i < keys.n as usize { keys.opacity[i] } else { 0.0 };
    let c = |i: usize| {
        if i < keys.n as usize {
            let [r, g, b] = keys.rgb[i];
            [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 0.0]
        } else {
            [0.0; 4]
        }
    };
    vec![
        Uniform::new("keyed_round", round),
        Uniform::new("keyed_radius", radius),
        Uniform::new("key_alpha", [a(0), a(1), a(2), a(3)]),
        Uniform::new("dist_max", dist_max),
        Uniform::new("lens_key0", c(0)),
        Uniform::new("lens_key1", c(1)),
        Uniform::new("lens_key2", c(2)),
        Uniform::new("lens_key3", c(3)),
    ]
}

/// The extra uniform names [`keyed_lens_uniforms`] sets, for the compile list.
const KEYED_LENS_UNIFORMS: [(&str, UniformType); 8] = [
    ("keyed_round", UniformType::_4f),
    ("keyed_radius", UniformType::_1f),
    ("key_alpha", UniformType::_4f),
    ("dist_max", UniformType::_1f),
    ("lens_key0", UniformType::_4f),
    ("lens_key1", UniformType::_4f),
    ("lens_key2", UniformType::_4f),
    ("lens_key3", UniformType::_4f),
];

/// The glass program with a keyed lens (ADR 0132, `KEYED_LENS`): the glass body
/// bent by the key mask's distance field instead of a rounded box, its colour
/// at the key's alpha. Its texture is packed (see `blur::keyed_lens`), so
/// `dual_tex` is set and the backdrop is the left half. Cached by the caller.
pub fn compile_keyed_lens(renderer: &mut GlesRenderer) -> Result<GlesTexProgram, GlesError> {
    let src = preset_source(&format!("{BACKDROP_HEAD}{GLASS_BODY}"), GlassPreset::Lens).replacen(
        "//_DEFINES_",
        "//_DEFINES_\n#define KEYED_LENS 1",
        1,
    );
    let names: Vec<_> = GLASS_UNIFORMS
        .iter()
        .chain(KEYED_LENS_UNIFORMS.iter())
        .map(|&(name, ty)| UniformName::new(name, ty))
        .collect();
    renderer.compile_custom_texture_shader(src, &names)
}

/// A rectangle covering any framebuffer: "no mask" / "everywhere".
const EVERYWHERE: [f32; 4] = [-1.0e6, -1.0e6, 2.0e6, 2.0e6];

pub fn fb_rect(rect: Rectangle<i32, Physical>, fb: FbMap) -> [f32; 4] {
    let rect = fb.gl_rect(rect);
    [
        rect.loc.x as f32,
        rect.loc.y as f32,
        rect.size.w as f32,
        rect.size.h as f32,
    ]
}

/// Uniforms for [`compile_keyed`]. `mask` is the window rectangle and corner
/// radius (physical, `rounding_uniforms`' convention), or `None` for no
/// rounding. `region` is where the keys apply, `None` for the whole surface.
pub fn keyed_uniforms(
    mask: Option<(Rectangle<i32, Physical>, f32)>,
    region: Option<Rectangle<i32, Physical>>,
    fb: FbMap,
    keys: &crate::userdata::Keys,
) -> Vec<Uniform<'static>> {
    let (win, radius) = match mask {
        Some((rect, radius)) => (fb_rect(rect, fb), radius),
        None => (EVERYWHERE, 0.0),
    };
    let key_rect = region.map_or(EVERYWHERE, |r| fb_rect(r, fb));
    let key = |i: usize| -> [f32; 4] {
        if i < keys.n as usize {
            let [r, g, b] = keys.rgb[i];
            [
                r as f32 / 255.0,
                g as f32 / 255.0,
                b as f32 / 255.0,
                keys.opacity[i],
            ]
        } else {
            [0.0; 4]
        }
    };
    vec![
        Uniform::new("win_rect", win),
        Uniform::new("radius", radius),
        Uniform::new("key_rect", key_rect),
        Uniform::new("keys", keys.n as f32),
        Uniform::new("key0", key(0)),
        Uniform::new("key1", key(1)),
        Uniform::new("key2", key(2)),
        Uniform::new("key3", key(3)),
        Uniform::new("key_all", {
            let a = |i: usize| (i < keys.n as usize && keys.everywhere[i]) as u8 as f32;
            [a(0), a(1), a(2), a(3)]
        }),
    ]
}

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

// A box's signed distance (x), and the centre of the corner arc f is nearest
// to (yz). Mirrors `blur::shape_sd`'s `box_sd`.
vec3 box_sd(vec2 f, vec4 b) {
    vec2 h = b.zw * 0.5;
    float r = max(min(radius, min(h.x, h.y)), 0.0);
    vec2 q = abs(f - (b.xy + h)) - h + r;
    float d = length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
    vec2 arc = min(max(f, b.xy + r), b.xy + b.zw - r);
    return vec3(d, arc);
}

// Mirrors `blur::join`: the smooth-min bite is scaled by the gap between the
// two arc centres, so a shared corner keeps its own radius.
vec3 join(vec3 a, vec3 b) {
    // `gap` is the arc centres' distance in radii, as `blur::join` takes it.
    vec3 m = a;
    if (b.x < a.x)
        m = b;
    if (fillet <= 0.0)
        return m;
    float h = max(fillet - abs(a.x - b.x), 0.0) / fillet;
    float gap = min(length(a.yz - b.yz) / max(radius, 1.0), 1.0);
    return vec3(m.x - gap * h * h * fillet * 0.25, m.yz);
}

// `box_sd` and the outward normal of the box's own distance (unnormalised sign
// grid, as the unshaped path's), for `shape_sdg`.
vec3 box_sdg(vec2 f, vec4 b, out vec2 g) {
    vec2 h = b.zw * 0.5;
    vec2 p = f - (b.xy + h);
    float r = max(min(radius, min(h.x, h.y)), 0.0);
    vec2 q = abs(p) - h + r;
    float d = length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
    vec2 arc = min(max(f, b.xy + r), b.xy + b.zw - r);
    vec2 s = vec2(p.x < 0.0 ? -1.0 : 1.0, p.y < 0.0 ? -1.0 : 1.0);
    g = ((q.x > 0.0 && q.y > 0.0) ? normalize(q) : (q.x > q.y ? vec2(1.0, 0.0) : vec2(0.0, 1.0))) * s;
    return vec3(d, arc);
}

// `join` with the gradient carried along: the smooth-min's bite moves the
// normal toward the other box's by `gap * h / 2` of the way (the derivative of
// the bite term; the arc-centre gap is held constant).
void join_g(inout vec3 a, inout vec2 ga, vec3 b, vec2 gb) {
    bool bm = b.x < a.x;
    vec3 m = bm ? b : a;
    vec2 gm = bm ? gb : ga;
    if (fillet > 0.0) {
        float h = max(fillet - abs(a.x - b.x), 0.0) / fillet;
        float gap = min(length(a.yz - b.yz) / max(radius, 1.0), 1.0);
        gm += (gap * h * 0.5) * ((bm ? ga : gb) - gm);
        m.x -= gap * h * h * fillet * 0.25;
    }
    a = m;
    ga = gm;
}

// The shape's distance and its gradient from one evaluation of each box (the
// glass shader used four more evaluations for a central difference).
float shape_sdg(vec2 f, out vec2 g) {
    vec2 g0;
    vec2 g1;
    vec3 a = box_sdg(f, box0, g0);
    vec3 b = box_sdg(f, box1, g1);
    join_g(a, g0, b, g1);
    if (boxes > 2.5) {
        vec2 g2;
        vec3 c = box_sdg(f, box2, g2);
        join_g(a, g0, c, g2);
    }
    if (boxes > 3.5) {
        vec2 g3;
        vec3 e = box_sdg(f, box3, g3);
        join_g(a, g0, e, g3);
    }
    g = g0;
    return a.x;
}

float shape_sd(vec2 f) {
    vec3 d = join(box_sd(f, box0), box_sd(f, box1));
    if (boxes > 2.5)
        d = join(d, box_sd(f, box2));
    if (boxes > 3.5)
        d = join(d, box_sd(f, box3));
    return d.x;
}
"#;

/// The shaped variant of one of the mask programs: the shape functions and
/// `SHAPED` defined right after its `radius` uniform.
fn shaped_source(src: &str) -> String {
    src.replacen(RADIUS_DECL, &format!("{RADIUS_DECL}{SHAPE_FNS}"), 1)
}

/// The glass material presets (liquid-glass plan): `Lens` for small shaped
/// controls, `Regular` for panes and window frames, `Frost` for large
/// translucent bodies. Each is its own compiled program (a `#define`, not a
/// uniform branch, so a variant carries only its own math). Until the optics
/// steps land all three are today's look, bit for bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlassPreset {
    Lens = 0,
    Regular = 1,
    Frost = 2,
}

impl GlassPreset {
    /// Every preset, in program-slot order.
    pub const ALL: [GlassPreset; 3] = [GlassPreset::Lens, GlassPreset::Regular, GlassPreset::Frost];

    /// Slot in a `[_; 3]` of programs.
    pub fn index(self) -> usize {
        self as usize
    }

    /// The macro a variant is compiled with.
    fn define(self) -> &'static str {
        match self {
            GlassPreset::Lens => "GLASS_LENS",
            GlassPreset::Regular => "GLASS_REGULAR",
            GlassPreset::Frost => "GLASS_FROST",
        }
    }

    /// Which preset a glass backdrop gets: shaped controls (bar pills) are
    /// lenses, a window frame's bezel is regular glass, anything else (a large
    /// translucent window body) is frost.
    /// A single-rectangle layer sheet in glass mode (no input-region shape, no
    /// bezel), by its shorter side in logical px: a capsule-thin one (the bar
    /// pill) is a lens, anything bigger (launcher, toasts) is regular glass.
    pub fn of_sheet(short_logical: f32) -> Self {
        if short_logical <= SHEET_PILL_MAX_SHORT {
            GlassPreset::Lens
        } else {
            GlassPreset::Regular
        }
    }

    pub fn of(shaped: bool, bezel: bool) -> Self {
        if shaped {
            GlassPreset::Lens
        } else if bezel {
            GlassPreset::Regular
        } else {
            GlassPreset::Frost
        }
    }
}

/// Largest shorter side, logical px, of a layer sheet that still counts as a
/// control pill (the bar is 40): see [`GlassPreset::of_sheet`].
pub const SHEET_PILL_MAX_SHORT: f32 = 48.0;

/// `src` with the preset's `#define` right after the `radius` uniform (where the
/// shaped variant's `SHAPED` also goes).
fn preset_source(src: &str, preset: GlassPreset) -> String {
    let src = src.replacen("// SHADE_CONSTS", &shade_consts(), 1);
    src.replacen(
        RADIUS_DECL,
        &format!("{RADIUS_DECL}\n#define {} 1\n", preset.define()),
        1,
    )
}

/// Prepare a cell pane's uniforms: mark the pane as a cell, with the contact
/// shadow's reach in physical px and alpha (see the cell branch in the shader).
pub fn mark_cell(uniforms: &mut [Uniform<'static>], shadow_px: f32, shadow_alpha: f32) {
    use smithay::backend::renderer::gles::UniformValue;
    for u in uniforms.iter_mut() {
        match u.name.as_ref() {
            "opaque" => u.value = UniformValue::_1f(2.0),
            "inner" => u.value = UniformValue::_1f(shadow_px),
            "bar" => u.value = UniformValue::_1f(shadow_alpha),
            _ => {}
        }
    }
}

/// The glass program of one preset (unshaped). Cached by the caller.
pub fn compile_glass_preset(
    renderer: &mut GlesRenderer,
    preset: GlassPreset,
) -> Result<GlesTexProgram, GlesError> {
    let names: Vec<_> = GLASS_UNIFORMS
        .iter()
        .map(|&(name, ty)| UniformName::new(name, ty))
        .collect();
    renderer.compile_custom_texture_shader(
        preset_source(&format!("{BACKDROP_HEAD}{GLASS_BODY}"), preset),
        &names,
    )
}

/// The shaped glass program of one preset. Cached by the caller.
pub fn compile_glass_shaped_preset(
    renderer: &mut GlesRenderer,
    preset: GlassPreset,
) -> Result<GlesTexProgram, GlesError> {
    renderer.compile_custom_texture_shader(
        shaped_source(&preset_source(&format!("{BACKDROP_HEAD}{GLASS_BODY}"), preset)),
        &with_shape(&GLASS_UNIFORMS),
    )
}

macro_rules! preset_compilers {
    ($($plain:ident, $shaped:ident => $preset:expr;)*) => {$(
        /// `compile_glass_preset` for one preset, as a plain function pointer.
        pub fn $plain(renderer: &mut GlesRenderer) -> Result<GlesTexProgram, GlesError> {
            compile_glass_preset(renderer, $preset)
        }
        /// `compile_glass_shaped_preset` for one preset, as a plain function pointer.
        pub fn $shaped(renderer: &mut GlesRenderer) -> Result<GlesTexProgram, GlesError> {
            compile_glass_shaped_preset(renderer, $preset)
        }
    )*};
}

preset_compilers! {
    compile_glass_lens, compile_glass_shaped_lens => GlassPreset::Lens;
    compile_glass_regular, compile_glass_shaped_regular => GlassPreset::Regular;
    compile_glass_frost, compile_glass_shaped_frost => GlassPreset::Frost;
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
        &with_shape(&FROST_UNIFORMS),
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
pub fn shape_uniforms(shape: &crate::blur::Shape, fb: FbMap) -> [Uniform<'static>; 6] {
    let mut boxes = [[0.0f32; 4]; 4];
    let map = fb;
    for (slot, rect) in boxes.iter_mut().zip(shape.rects()) {
        let rect = map.gl_rect(*rect);
        *slot = [
            rect.loc.x as f32,
            rect.loc.y as f32,
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
#ifdef JOINED
// A joined element's adaptive read samples the member the fragment is nearest
// to (ADR 0130), so `g_own_box` is set per fragment by `shape_sdg`.
vec4 g_own_box;
#define OWN_BOX g_own_box
#else
#define OWN_BOX win_rect
#endif

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

// Adaptive glass (ADR 0092). Mirrors `adaptive_amount` below.
// The lengths below were tuned while `px_uv` was 1/output instead of 1/crop
// and so read ~0.37x as far as written (800 px window, 2880 px output); they
// carry that factor so the adaptive tint reads the same patch as before.
const float ADAPT_SPAN = 0.19;
const float ADAPT_STEP = 18.0;     // cross arm cap, physical px
const float ADAPT_INSET = 9.0;     // taps stay this far inside the rect, physical px
const float ADAPT_MIN_STEP = 2.2;  // below this one tap is enough
const float ADAPT_TEXT_DARK = 1.0;
const float ADAPT_TEXT_LIGHT = 0.01;
const float ADAPT_C_OK = 7.0;
const float ADAPT_C_MIN = 2.0;
const float ADAPT_REACH = 1.75;
const float ADAPT_MAX = 0.95;
#if defined(GLASS_LENS) || defined(GLASS_REGULAR)
// iOS light glass is a bright, near-opaque slab with dark text: lens and regular
// let the adaptive tint reach nearly all the way (frost keeps 0.8).
const float ADAPT_MAX_L = 0.95;
#else
const float ADAPT_MAX_L = 0.8;     // light glass keeps some backdrop: it must not read as a white slab
#endif
const float ADAPT_EASE = 1.5;      // exponent of the response curve
const float ADAPT_GHOST = 0.22;    // share of the backdrop's own deviation kept through the correction
const float ADAPT_GRAIN = 0.03;    // faint frost, scaled by the correction
const float ADAPT_WORST = 0.75;    // weight of the worst tap over the mean
#define ADAPT_ARMS 4               // taps around the centre: the cross (the diagonals were 4 more)
uniform vec2 px_uv;         // 1 physical px in the sampled texture's UV (1 / crop size; half that in x when packed)
uniform float dual_tex;       // 1 when `tex` is [blurred | sharp] side by side (chromatic aberration), else 0
#ifdef KEYED_LENS
// A keyed lens's texture is three wide (ADR 0132): the blurred backdrop, the
// lightly blurred one, then the key mask, a third of the texture each.
vec2 blur_hi() { return vec2(1.0 / 3.0, 1.0); }
vec2 clamp_blur(vec2 uv) { return clamp(uv, px_uv * 0.5, blur_hi() - px_uv * 0.5); }
vec2 clamp_sharp(vec2 uv) { return clamp(uv + vec2(1.0 / 3.0, 0.0), vec2(1.0 / 3.0, 0.0) + px_uv * 0.5, vec2(2.0 / 3.0, 1.0) - px_uv * 0.5); }
#else
// The part of the texture the blurred backdrop lives in: all of it, or its left half when packed.
vec2 blur_hi() { return vec2(dual_tex > 0.5 ? 0.5 : 1.0, 1.0); }
vec2 clamp_blur(vec2 uv) { return clamp(uv, px_uv * 0.5, blur_hi() - px_uv * 0.5); }
// The same uv in the lightly blurred half (packed only): shifted right by half the texture.
vec2 clamp_sharp(vec2 uv) { return clamp(uv + vec2(0.5, 0.0), vec2(0.5, 0.0) + px_uv * 0.5, vec2(1.0) - px_uv * 0.5); }
#endif
uniform vec4 phys_map;  // mat2, column-major: gl_FragCoord offset -> render-space offset
// A gl_FragCoord offset (or gradient) in the render-space frame the glass is laid out in.
vec2 to_phys(vec2 g) { return mat2(phys_map.xy, phys_map.zw) * g; }
uniform float scheme_light; // 0 dark (the original look, exactly), 1 light (ADR 0084)
uniform float adapt;        // effective adaptive strength; 0 off
uniform float adapt_layers; // glass layers over this backdrop: 1 window, 2 pane on a window; 0 unset = 1
const vec3 LUMA = vec3(0.2126, 0.7152, 0.0722);    // Rec.709
const vec3 SURFACE_D = vec3(0.047, 0.047, 0.055);  // #0c0c0e
const vec3 SURFACE_L = vec3(0.992, 0.989, 0.980);  // paper white

// Luma (display-encoded) of the blurred backdrop around this fragment that
// the text has to survive: nine taps (centre, cross, diagonals) from `tex`,
// the blur chain's own output. The mean is pulled `ADAPT_WORST` of the way to
// the tap the text contrasts with least (darkest for ink, brightest for
// white), so a busy backdrop (dark blocks under a light surface) is judged by
// its worst patch, not its average. The arms are at most `ADAPT_STEP` long (on
// a small surface, `ADAPT_SPAN` of its half-size) and each tap is held inside
// the surface's rect, so a shape's drawer and pill each read the backdrop
// behind themselves. Under `ADAPT_MIN_STEP` one tap does, blended in
// continuously (see `w` below). Mirrors `adaptive_luma` below.
float g_dev;  // centre minus local mean luma of the last backdrop_luma()
float backdrop_luma() {
    vec2 hs = abs(to_phys(OWN_BOX.zw)) * 0.5;
    vec2 step = min(vec2(ADAPT_STEP), hs * ADAPT_SPAN);
    // Taps stay `ADAPT_INSET` inside the rect (at least half its size): the
    // blur chain is low-res and ragged at an output edge, and a worst-patch
    // read turns those blocks into pale rectangles.
    vec2 lim = max(hs - vec2(ADAPT_INSET), hs * 0.5);
    vec2 p0 = to_phys(gl_FragCoord.xy - (OWN_BOX.xy + OWN_BOX.zw * 0.5));
    vec2 pc = clamp(p0, -lim, lim);
    float c = dot(texture2D(tex, clamp_blur(v_coords + (pc - p0) * px_uv)).rgb, LUMA);
    g_dev = 0.0;
    // Under `ADAPT_MIN_STEP` the backdrop is flat at the arms' scale and the
    // centre tap does; the multi-tap read fades in over [MIN/4, 4 MIN] of the
    // arm length, so a surface resizing across the threshold does not pop.
    // Mirrors `adaptive_tap_weight`.
    float w = smoothstep(ADAPT_MIN_STEP * 0.25, ADAPT_MIN_STEP * 4.0, max(step.x, step.y));
    if (w <= 0.0) {
        return c;
    }
    float sum = c;
    float lo = c;
    float hi = c;
    for (int i = 0; i < ADAPT_ARMS; i++) {
        vec2 d = i == 0 ? vec2(1.0, 0.0) : i == 1 ? vec2(-1.0, 0.0) : i == 2 ? vec2(0.0, 1.0)
            : i == 3 ? vec2(0.0, -1.0) : i == 4 ? vec2(0.7, 0.7) : i == 5 ? vec2(-0.7, 0.7)
            : i == 6 ? vec2(0.7, -0.7) : vec2(-0.7, -0.7);
        vec2 q = clamp(pc + d * step, -lim, lim);
        vec2 uv = clamp_blur(v_coords + (q - p0) * px_uv);
        float l = dot(texture2D(tex, uv).rgb, LUMA);
        sum += l;
        lo = min(lo, l);
        hi = max(hi, l);
    }
    float mean = sum / float(ADAPT_ARMS + 1);
    g_dev = (c - mean) * w;
    return mix(c, mix(mean, mix(hi, lo, scheme_light), ADAPT_WORST), w);
}

// How far to pull the tint toward the scheme's surface colour.
float adapt_amount(float luma, float light, float strength) {
    float lb = clamp(luma, 0.0, 1.0);
    lb *= lb;
    float lt = mix(ADAPT_TEXT_DARK, ADAPT_TEXT_LIGHT, light);
    float contrast = (max(lb, lt) + 0.05) / (min(lb, lt) + 0.05);
    float need = clamp((ADAPT_C_OK - contrast) / (ADAPT_C_OK - ADAPT_C_MIN), 0.0, 1.0);
    // Convex ease; each glass layer stacked on the backdrop multiplies the
    // exponent, so a pane over a window's glass compounds, not adds.
    float a = need * strength * ADAPT_REACH;
    float x = max(1.0 - a, 0.0);
    // x^1.5 for a window's one layer without the general `pow`.
    float xe = adapt_layers > 1.5 ? pow(x, ADAPT_EASE * adapt_layers) : x * sqrt(x);
    return min(1.0 - xe, mix(ADAPT_MAX, ADAPT_MAX_L, light));
}

vec3 adapt_tint(vec3 c) {
    if (adapt <= 0.0) {
        return c;
    }
    float a = adapt_amount(backdrop_luma(), scheme_light, adapt);
    vec3 t = mix(c, mix(SURFACE_D, SURFACE_L, scheme_light), a);
    // Keep a ghost of the backdrop and a little frost where the correction is
    // heavy, so the glass does not read as a flat slab. Mirrors `adaptive_ghost`.
    t += vec3(g_dev * ADAPT_GHOST * a);
    t += (grain(floor(gl_FragCoord.xy) + 17.0) - 0.5) * ADAPT_GRAIN * a;
    return t;
}
"#;

/// Frost: the rounded mask, a tint mixed in by its own alpha, and a fine
/// static grain hashed from the framebuffer position (COMP-02 §9).
/// The joined variant of the glass body (ADR 0130): up to 16 member boxes of
/// one element, each with its own corner radius, fillet to the member it was
/// reached from, bevel, strength and look. Members are folded in attachment
/// order with `join_member`, the smooth-min of `join_g` at each member's own
/// fillet and radius; `glass::joined_distance` is the CPU mirror. The body reads
/// the NEAREST member's look (least raw distance), so a member's rim, lift and
/// bevel stay its own and the seam, where the nearest member changes, shows no
/// step: the fold is continuous, and only the per-member values switch there.
const JOINED_FNS: &str = r#"
uniform vec4 jbox[16];  // x y w h, gl_FragCoord space
uniform vec4 jdat[16];  // fillet to its parent, corner radius, bevel, strength (physical px)
uniform vec4 jlook[16]; // lift, shade, smoke, rim
uniform float jcount;

float strength;   // the nearest member's, set by shape_sdg
float bevel;
float lift;
float shade;
float smoke;
float rim;
float g_s_eff;    // the nearest member's effective size, harmonic mean of its sides

// `join_g` at one member's own fillet and corner radius.
void join_member(inout vec3 a, inout vec2 ga, vec3 b, vec2 gb, float fillet_k, float radius_k) {
    bool bm = b.x < a.x;
    vec3 m = bm ? b : a;
    vec2 gm = bm ? gb : ga;
    if (fillet_k > 0.0) {
        float h = max(fillet_k - abs(a.x - b.x), 0.0) / fillet_k;
        float gap = min(length(a.yz - b.yz) / max(radius_k, 1.0), 1.0);
        gm += (gap * h * 0.5) * ((bm ? ga : gb) - gm);
        m.x -= gap * h * h * fillet_k * 0.25;
    }
    a = m;
    ga = gm;
}

// The joined distance and its gradient at f; sets the nearest member's look,
// size and bevel into the globals the body reads. Indices are loop indices
// (fragment uniform arrays allow nothing else).
float shape_sdg(vec2 f, out vec2 g) {
    vec3 a = vec3(0.0);
    vec2 ga = vec2(0.0);
    float best = 1e9;
    vec4 sel_box = vec4(0.0);
    vec4 sel_dat = vec4(0.0);
    vec4 sel_look = vec4(0.0);
    for (int i = 0; i < 16; i++) {
        if (float(i) >= jcount)
            break;
        vec4 b = jbox[i];
        vec4 dat = jdat[i];
        vec2 h = b.zw * 0.5;
        float r = max(min(dat.y, min(h.x, h.y)), 0.0);
        vec2 p = f - (b.xy + h);
        vec2 q = abs(p) - h + r;
        float d = length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
        vec2 arc = min(max(f, b.xy + r), b.xy + b.zw - r);
        vec2 s = vec2(p.x < 0.0 ? -1.0 : 1.0, p.y < 0.0 ? -1.0 : 1.0);
        vec2 gi = ((q.x > 0.0 && q.y > 0.0) ? normalize(q) : (q.x > q.y ? vec2(1.0, 0.0) : vec2(0.0, 1.0))) * s;
        vec3 c = vec3(d, arc);
        if (i == 0) {
            a = c;
            ga = gi;
        } else {
            join_member(a, ga, c, gi, dat.x, dat.y);
        }
        if (d < best) {
            best = d;
            sel_box = b;
            sel_dat = dat;
            sel_look = jlook[i];
        }
    }
    strength = sel_dat.w;
    bevel = sel_dat.z;
    lift = sel_look.x;
    shade = sel_look.y;
    smoke = sel_look.z;
    rim = sel_look.w;
    g_s_eff = 2.0 * sel_box.z * sel_box.w / max(sel_box.z + sel_box.w, 1.0);
    g_own_box = sel_box;
    g = ga;
    return a.x;
}
"#;

const FROST_BODY: &str = r#"
uniform vec4 frost_tint;
uniform vec4 rim_color;   // premultiplied bezel hairline colour
uniform float inner;      // bezel width, physical px; 0 off the bezel
uniform float opaque;     // 1 when the window inside the bezel is opaque
uniform float bar;        // titlebar depth from the region's top, physical px; 0 none

const vec2 LIGHT = vec2(-0.70710678, -0.70710678); // top-left, y down
// The glass program's neutral hairline, so a bezel reads the same in both.
const float LINE_BASE = 0.1;
const float LINE_TL = 0.26;
const float LINE_BR = 0.07;

void main() {
    vec4 color = texture2D(tex, v_coords);

    // `p` and `half_size` are read by the opaque-window test below in both
    // variants, so they sit outside the shaped split.
    // Physical space from here on: the size is the rect's, swapped on a quarter turn.
    vec2 half_size = abs(to_phys(win_rect.zw)) * 0.5;
    vec2 p = to_phys(gl_FragCoord.xy - (win_rect.xy + win_rect.zw * 0.5));
#ifdef SHAPED
    float d = shape_sd(gl_FragCoord.xy);
#else
    float r = min(radius, min(half_size.x, half_size.y));
    vec2 q = abs(p) - half_size + r;
    float d = length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
#endif
    // Outside the mask the result is zero: skip the taps below.
    if (d >= 0.5) {
        gl_FragColor = vec4(0.0);
        return;
    }
    // Under an opaque window only the ring can show.
    if (opaque > 0.5 && d < -inner - 1.0 && p.y + half_size.y >= bar) {
        gl_FragColor = vec4(0.0);
        return;
    }
    color.rgb = mix(color.rgb, frost_tint.rgb, frost_tint.a);
    color.rgb = adapt_tint(color.rgb);
    color.rgb = clamp(color.rgb + (grain(floor(gl_FragCoord.xy)) - 0.5) * 0.035, 0.0, 1.0);
#ifndef SHAPED
    // Bezel hairline: a 1.25 px band inside the mask edge, lit top-left.
    if (inner > 0.0) {
        vec2 s = vec2(p.x < 0.0 ? -1.0 : 1.0, p.y < 0.0 ? -1.0 : 1.0);
        vec2 grad = (q.x > 0.0 && q.y > 0.0) ? normalize(q) : (q.x > q.y ? vec2(1.0, 0.0) : vec2(0.0, 1.0));
        grad *= s;
        float band = 1.0 - smoothstep(0.0, 1.25, -d);
        float lobe = LINE_BASE + LINE_TL * max(dot(grad, LIGHT), 0.0) + LINE_BR * max(dot(grad, -LIGHT), 0.0);
        vec3 tint = rim_color.rgb / max(rim_color.a, 0.0001);
        color.rgb = mix(color.rgb, tint, clamp(band * lobe * rim_color.a, 0.0, 1.0));
    }
#endif
    color = finish(color);
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
/// (`to_phys` undoes the framebuffer orientation of [`FbMap`]), so the
/// texture offset is just the pixel offset times `px_uv`.
///
/// A window bezel (`inner` > 0) is the same glass laid as a ring of that width
/// around the window: the roll-off spans only the ring, so nothing under the
/// window is ever bent, a faint dark seam marks where glass meets content, and
/// the hairline is the same neutral one a layer wears (`rim_color`, white
/// premultiplied, its alpha dimming it while the window is unfocused). Over an
/// opaque window nothing inside the ring is drawn at all. Off the bezel
/// `rim_color` is opaque white.
const GLASS_BODY: &str = r#"
#ifndef JOINED
uniform float strength;   // largest displacement, physical px, at the edge
uniform float bevel;      // roll-off band width, physical px
#endif
uniform float dispersion; // chromatic fringe at the edge, physical px; 0 off (needs a packed `tex`)
#ifndef JOINED
uniform float rim;        // 0..1 hairline strength
#endif
uniform vec4 rim_color;   // premultiplied hairline colour; opaque white off the bezel
uniform float inner;      // bezel width, physical px; 0 off the bezel
uniform float opaque;     // 1 when the window inside the bezel is opaque
uniform float bar;        // titlebar depth from the region's top, physical px; 0 none
#ifndef JOINED
uniform float lift;       // 0..1 mix toward white; 0 (unset) on windows
uniform float shade;      // 0..1 mix toward ink (light only: a pane's depth step); 0 elsewhere
#endif
uniform float shade_ref;  // physical px of the reference control size (40 logical); 0 (unset) = 40
#ifndef JOINED
uniform float smoke;      // 0..1 weight of the smoke and adaptive tint; 1 normal, less on stacked glass
#endif
uniform vec4 body_tint;   // a trusted prompt's fixed palette tint (straight rgb, alpha = weight); 0 elsewhere
uniform float sharp_damp; // 0..1 share of the bevel's lightly blurred half held back; 0 (unset) everywhere but a trusted prompt

const vec2 LIGHT = vec2(-0.70710678, -0.70710678); // top-left, y down
const float WINDOW_RIM_K = 0.85;   // floor of the size fade for a translucent window's rim
const float SATURATE = 1.7;
const float COMPRESS = 1.0;  // highlight knee: c / (1 + COMPRESS * luma)
const vec3 SMOKE = vec3(0.047, 0.047, 0.055);       // #0c0c0e
const float SMOKE_MIX = 0.28;
// Hairline alphas at rim 0.5: all-round base, top-left and bottom-right lobes.
const float LINE_BASE = 0.05;
const float LINE_TL = 0.13;
const float LINE_BR = 0.035;
// Lens and regular only: edge darkening and the lit edge gradient at the rim, a
// clearer interior (less smoke and adaptive tint) and a faint fill.
const float EDGE_DARK = 0.26;
const float EDGE_DARK_L = 0.12;
const float LINE_IN = 0.34;
const float RIM_SAT = 1.5;          // rim colour: saturation of the sharp backdrop
const float RIM_GAIN = 1.7;         // and its brightening, then clamped
const float RIM_SHARP = 1.05;       // weight (before the light lobes) in the dark scheme: soft, subtle
const float RIM_SHARP_L = 1.0;      // and in the light one: a crisp edge
const float RIM_WHITE_L = 0.45;     // light scheme: share of white mixed into the rim colour
const float OUT_BLUR_IN = 4.0;      // a displaced sample this deep inside the edge starts to go blurred, physical px
const float OUT_BLUR_OUT = 2.0;     // and is fully blurred this far outside it
const float RIM_CRISP_PX = 3.0;     // light scheme: width of the crisp edge, physical px
// A cell nested in a card pane (GLASS_LENS panes with `opaque` 2): fill colour,
// depth cues and rim. See the branch in `main`.
const vec3 CELL_BASE_D = vec3(1.0);
const vec3 CELL_BASE_L = vec3(0.16, 0.13, 0.09);
const float CELL_GRAD = 0.7;        // the gradient's share of the fill alpha at `shade` 1
const float CELL_EDGE_DARK = 0.30;  // Fresnel edge darkening
const float CELL_EDGE_A = 0.9;      // and the fill's thickening at the rim
const float CELL_RIM_GAIN = 3.0;
const float CELL_GRAD_L = 0.25;
const float CELL_RIM_GAIN_L = 0.45;
const float CELL_EDGE_A_L = 0.25;    // a cell's hairline is crisper than a pane's
const float CELL_INNER = 0.10;      // the faint inner highlight
// SHADE_CONSTS
const float LENS_SMOKE = 0.35;      // share of the smoke the interior keeps
const float LENS_ADAPT = 0.75;      // share of the adaptive tint it keeps
const vec3 LENS_FILL_D = vec3(0.17, 0.17, 0.18);
const vec3 LENS_FILL_L = vec3(1.0, 0.995, 0.985);
const float LENS_FILL_AMT_D = 0.10;
const float LENS_FILL_AMT_L = 0.75;
// Darkening of the 1 px seam where the bezel meets the window.
const float SEAM = 0.1;
// The light twins: bright frosted glass with the dark ink drawn on it. Each is
// `mix(dark, light, scheme_light)`, which is the dark constant bit for bit at 0.
const float SATURATE_L = 1.6;
const float COMPRESS_L = 0.0;                       // no highlight knee
const vec3 SMOKE_L = vec3(0.992, 0.989, 0.980);     // paper white
const float SMOKE_MIX_L = 0.12;   // a light veil; the window tint does the whitening (ADR 0084)
const float SEAM_L = 0.08;
// A white rim on near-white frost is no edge. In light a white-rimmed shape
// (a window's bezel, a layer) gets an ink hairline on its outermost pixel, and
// its white stays as the specular on the lit top-left lobe just inside.
const float INK_EDGE_L = 0.45;
const float SPEC_L = 0.85;
const vec3 LIFT_L = vec3(0.102, 0.086, 0.063);      // ink #1a1610: a depth step darkens light glass

// GLASS_LENS / GLASS_REGULAR / GLASS_FROST name the preset this program was
// compiled for (liquid-glass step 0: scaffolding, all three are today's look).
vec4 blurred_at(vec2 uv) {
    return texture2D(tex, clamp_blur(uv));
}

// The lightly blurred twin of `blurred_at` (packed textures only).
vec4 sharp_at(vec2 uv) {
    return texture2D(tex, clamp_sharp(uv));
}

#ifdef KEYED_LENS
// Keyed lens (ADR 0132). The last third of the packed texture is the key mask:
// red is the distance to the nearest other label over `dist_max` (1 on the
// plateau), blue the label / 4 (0: not lens). `keyed_round` is the window's
// rounded rectangle, which cuts the lens at its corners.
uniform vec4 keyed_round;
uniform float keyed_radius;
uniform vec4 key_alpha;
uniform float dist_max;
// The keys' colours (rgb; w unused): the lens composites the key over the bent
// backdrop exactly as the flat key does, so its hue survives (ADR 0132).
uniform vec4 lens_key0;
uniform vec4 lens_key1;
uniform vec4 lens_key2;
uniform vec4 lens_key3;

vec2 mask_uv(vec2 uv) {
    return clamp(uv + vec2(2.0 / 3.0, 0.0), vec2(2.0 / 3.0, 0.0) + px_uv * 0.5, vec2(1.0) - px_uv * 0.5);
}

float mask_label(vec2 uv) {
    return floor(texture2D(tex, mask_uv(uv)).b * 4.0 + 0.5);
}

float mask_dist(vec2 uv) {
    return texture2D(tex, mask_uv(uv)).r * dist_max;
}

float rounded_sd(vec2 f) {
    vec2 h = keyed_round.zw * 0.5;
    float rr = max(min(keyed_radius, min(h.x, h.y)), 0.0);
    vec2 qq = abs(f - (keyed_round.xy + h)) - h + rr;
    return length(max(qq, vec2(0.0))) + min(max(qq.x, qq.y), 0.0) - rr;
}
#endif

#if defined(SHAPED) || defined(JOINED)
// Outward normal of the shape, y down, from the analytic gradient.
vec2 shape_normal(vec2 g) {
    g = to_phys(g);
    return length(g) > 0.01 ? normalize(g) : vec2(0.0, -1.0);
}
#endif

void main() {
    // `p` and `half_size` are read by the opaque-window test below in both
    // variants, so they sit outside the shaped split.
    // Physical space from here on: the size is the rect's, swapped on a quarter turn.
    vec2 half_size = abs(to_phys(win_rect.zw)) * 0.5;
    vec2 p = to_phys(gl_FragCoord.xy - (win_rect.xy + win_rect.zw * 0.5));
#if defined(SHAPED) || defined(JOINED)
    vec2 sg;
    float d = shape_sdg(gl_FragCoord.xy, sg);
#else
    float r = min(radius, min(half_size.x, half_size.y));
    vec2 q = abs(p) - half_size + r;
    float d = length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
#endif

#ifdef KEYED_LENS
    // The keyed region's own edge (ADR 0132): inside a lens the distance to the
    // nearest other label, outside none. Its outward normal is the negative
    // gradient of that distance (which grows inward); the gradient is taken
    // over one texel either side in the mask's third of the texture.
    float klab = mask_label(v_coords);
    d = klab < 0.5 ? 1.0 : -mask_dist(v_coords);
    vec2 kg = vec2(
        mask_dist(v_coords + vec2(px_uv.x, 0.0)) - mask_dist(v_coords - vec2(px_uv.x, 0.0)),
        mask_dist(v_coords + vec2(0.0, px_uv.y)) - mask_dist(v_coords - vec2(0.0, px_uv.y)));
    float kl = length(kg);
    vec2 kgrad = kl > 1e-4 ? -kg / kl : vec2(0.0, -1.0);
#endif

#if defined(GLASS_LENS) && !defined(SHAPED) && !defined(JOINED)
    // A CELL (a control nested in a card pane): a fill on what is already drawn,
    // no backdrop sample at all. `opaque` is 2 for a cell (never on a pane
    // otherwise), `lift` its fill alpha, `shade` its gradient strength, `rim`
    // and `rim_color` the hairline, `inner` the contact shadow's reach (px) and
    // `bar` its alpha. Premultiplied output.
    if (opaque > 1.5) {
        float edge = -d;
        float bw = clamp(bevel, 2.0, 6.0);
        vec2 cs = vec2(p.x < 0.0 ? -1.0 : 1.0, p.y < 0.0 ? -1.0 : 1.0);
        vec2 cg;
        if (q.x > 0.0 && q.y > 0.0) {
            cg = normalize(q);
        } else if (q.x > q.y) {
            cg = vec2(1.0, 0.0);
        } else {
            cg = vec2(0.0, 1.0);
        }
        cg *= cs;
        float ctl = max(dot(cg, LIGHT), 0.0);
        float cbr = max(dot(cg, -LIGHT), 0.0);
        float cin = 1.0 - smoothstep(-0.5, 0.5, d);
        vec4 outc = vec4(0.0);
        if (d < 0.5) {
            vec3 base = mix(CELL_BASE_D, CELL_BASE_L, scheme_light);
            float a = lift;
            // 1: a vertical gradient, the top clearer in dark and fainter in light.
            float gs = (0.5 - clamp((p.y + half_size.y) / max(2.0 * half_size.y, 1.0), 0.0, 1.0)) * 2.0;
            a *= 1.0 + mix(CELL_GRAD, CELL_GRAD_L, scheme_light) * shade * gs * mix(1.0, -1.0, scheme_light);
            float u = 1.0 - clamp(edge / bw, 0.0, 1.0);
            // 2: Fresnel-style: the fill darkens and thickens toward the rim.
            base = mix(base, vec3(0.0), CELL_EDGE_DARK * u * u * mix(1.0, 0.1, scheme_light));
            a *= 1.0 + mix(CELL_EDGE_A, CELL_EDGE_A_L, scheme_light) * u * u;
            vec3 col = base;
            // 3: one lit edge: a crisp two-lobe hairline and a faint inner highlight.
            float line = 1.0 - smoothstep(0.0, 1.25, edge);
            float lobe = LINE_BASE + LINE_TL * ctl + LINE_BR * cbr;
            vec3 ctint = rim_color.rgb / max(rim_color.a, 0.0001);
            float la = clamp(line * lobe * rim_color.a * rim * 2.0 * mix(CELL_RIM_GAIN, CELL_RIM_GAIN_L, scheme_light), 0.0, 1.0);
            float hl = (1.0 - smoothstep(0.0, 1.4, abs(edge - bw))) * (0.3 + ctl + 0.5 * cbr)
                * CELL_INNER * min(rim, 3.0) * (1.0 - scheme_light * 0.5);
            col = mix(col, vec3(1.0), clamp(hl, 0.0, 1.0));
            a = max(a, hl);
            col = mix(col, ctint, la);
            a = max(a, la);
            outc = vec4(col * a, a) * cin;
        }
        // 4: a small soft contact shadow on the card, 1.5 px down, inside the margin.
        if (bar > 0.0 && inner > 0.5 && d > -1.0) {
            vec2 sp = p - vec2(0.0, 1.5);
            vec2 sq = abs(sp) - half_size + r;
            float sd = length(max(sq, vec2(0.0))) + min(max(sq.x, sq.y), 0.0) - r;
            float sa = bar * (1.0 - smoothstep(0.0, inner, max(sd, 0.0)));
            sa *= sa * (1.0 - cin);
            outc += vec4(vec3(0.02, 0.015, 0.01) * sa, sa) * (1.0 - outc.a);
        }
        gl_FragColor = outc;
        return;
    }
#endif
    // Outside the mask the result is zero: skip the taps below.
    if (d >= 0.5) {
        gl_FragColor = vec4(0.0);
        return;
    }
    // Under an opaque window only the ring (and the window's own antialiased
    // edge) can show.
    if (opaque > 0.5 && d < -inner - 1.0 && p.y + half_size.y >= bar) {
        gl_FragColor = vec4(0.0);
        return;
    }

#if defined(SHAPED) || defined(JOINED)
    vec2 grad = shape_normal(sg);
#elif defined(KEYED_LENS)
    vec2 grad = kgrad;
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

    // A bezel bends across its own width only.
#if defined(GLASS_LENS) || defined(GLASS_REGULAR)
    // A translucent window (its bezel is not `opaque`) shows its glass through
    // the body, so its lit rim and edge bend run over the full bevel, like any
    // other lens; only an opaque window's glass is the bare ring.
    float band_w = (inner > 0.0 && opaque > 0.5) ? inner : bevel;
#else
    float band_w = inner > 0.0 ? inner : bevel;
#endif
#if defined(GLASS_LENS) || defined(GLASS_REGULAR)
    // Lens optics. `t` runs 0 at the edge to 1 where the bevel meets the flat
    // plateau, which is only the lightly-bent blurred result (one tap, none of
    // the lens math). In the bevel the surface is a convex height profile
    // h(t) = 1 - (1 - t)^p, so the slope, and with it the bend, is steepest at
    // the rim and eases to zero at the plateau: `strength * (1 - t)^(p - 1)`
    // (p = 3.5) physical px. The bend points OUT of the shape, so the rim shows
    // content from beyond the edge, however far the shape's own edge is. The
    // rim reads the lightly blurred half of the packed `tex`, crossfading to
    // the plateau's blur by `u`; each colour channel is bent a little
    // differently (red by 1 - k, blue by 1 + k, `dispersion` = k), so the
    // fringe is as wide as the local bend is.
    float t = -d / max(band_w, 1.0);
    vec4 color = blurred_at(v_coords);
    float lens_u = 0.0;  // 0 on the plateau, up to 1 at the rim: where the edge shading runs
    vec3 rim_sh = color.rgb;  // the sharp backdrop at the rim, for the rim's own colour
    if (t < 1.0) {
        float u = 1.0 - max(t, 0.0);
        lens_u = u;
        float bend = strength * u * u * sqrt(u);
        vec2 b = grad * px_uv * bend;
        float k = dispersion;
        if (dual_tex > 0.5) {
            vec3 sh = vec3(sharp_at(v_coords + b * (1.0 - k)).r,
                           sharp_at(v_coords + b).g,
                           sharp_at(v_coords + b * (1.0 + k)).b);
            // Where the bent sample lands outside the shape (or in the outer
            // part of the bevel) the lightly blurred half is still readable
            // text of whatever lies beside the pane: show the plateau's blur
            // there instead, so the rim keeps its colour and bend but never
            // legible content. One extra tap, only for those fragments.
            float out_w = smoothstep(-OUT_BLUR_IN, OUT_BLUR_OUT, d + bend);
            if (out_w > 0.0) {
                sh = mix(sh, blurred_at(v_coords + b).rgb, out_w);
            }
            // `sharp_damp` (a trusted prompt only) holds back that share of the
            // lightly blurred half: the band stays a lens, not a window onto it.
            vec3 shown = mix(color.rgb, sh, 1.0 - sharp_damp);
            color.rgb = mix(color.rgb, shown, u);
            rim_sh = shown;
        } else {
            color.r = blurred_at(v_coords + b * (1.0 - k)).r;
            color.g = blurred_at(v_coords + b).g;
            color.b = blurred_at(v_coords + b * (1.0 + k)).b;
        }
    }
#else
    // Roll-off: largest at the edge, easing to zero (and zero slope) where
    // the bevel meets the plateau, so no line marks where the bend stops.
    float u = 1.0 - clamp(-d / max(band_w, 1.0), 0.0, 1.0);
    vec2 bend = -grad * px_uv * (strength * u * u);
    vec4 color = blurred_at(v_coords + bend);
    // Chromatic aberration: every channel moves from the blurred result toward
    // the lightly blurred half of the packed texture by the bevel falloff `u`
    // (so the plateau, where u is 0, stays pure blur), and red and blue are read
    // `dispersion * u` real pixels either side of the bent sample along the edge
    // normal: strongest at the outer edge. Green stays on the bent sample and
    // takes the same sharpening, or a sharp backdrop would turn the edge into a
    // green band (red and blue sharp against a blurred green).
    if (dispersion > 0.0 && dual_tex > 0.5) {
        float w = u;
        vec2 fr = -grad * px_uv * (dispersion * w);
        vec2 uv0 = v_coords + bend;
        vec2 uvr = uv0 + fr;
        vec2 uvb = uv0 - fr;
        color.r = mix(blurred_at(uvr).r, sharp_at(uvr).r, w);
        color.g = mix(color.g, sharp_at(uv0).g, w);
        color.b = mix(blurred_at(uvb).b, sharp_at(uvb).b, w);
    }
#endif

    // Vibrancy: saturate, compress the highlights, smoke toward neutral.
    vec3 c = color.rgb;
    c = max(mix(vec3(dot(c, LUMA)), c, mix(SATURATE, SATURATE_L, scheme_light)), 0.0);
    c = c / (1.0 + mix(COMPRESS, COMPRESS_L, scheme_light) * dot(c, LUMA));
#if defined(GLASS_LENS)
    float smoke_w = smoke * LENS_SMOKE;
    float adapt_w = smoke * LENS_ADAPT;
#else
    float smoke_w = smoke;
    float adapt_w = smoke;
#endif
    c = mix(c, mix(SMOKE, SMOKE_L, scheme_light), mix(SMOKE_MIX, SMOKE_MIX_L, scheme_light) * smoke_w);
#if defined(GLASS_LENS)
    c = mix(c, mix(LENS_FILL_D, LENS_FILL_L, scheme_light), mix(LENS_FILL_AMT_D, LENS_FILL_AMT_L, scheme_light) * smoke);
#endif
    if (smoke > 0.0) {
        c = mix(c, adapt_tint(c), adapt_w);
    }
    c = mix(c, vec3(1.0), lift);
    c = mix(c, LIFT_L, shade);
    c = mix(c, body_tint.rgb, body_tint.a);

#if defined(GLASS_LENS) || defined(GLASS_REGULAR)
    // Thickness, inside the bevel only (the plateau never gets here): the edge
    // darkens as the glass thickens toward the rim, and a faint highlight line
    // sits near the inner end of the band, lit like the hairline. No taps: both
    // are functions of the distance already computed. `rim` 0 turns them off too.
    if (lens_u > 0.0) {
        float rs = min(rim * 2.0, 1.0);
        // Edge shading follows ONE continuous rule of the pane's effective size
        // `s_eff` (harmonic mean of its two sides, so a long skinny bar and a
        // short fat card both read by what they are): the band grows
        // sublinearly with size, its strength falls sublinearly, and neither has
        // a threshold. Small controls (about 40 px) keep the original look.
        float s_eff = 2.0 * win_rect.z * win_rect.w / max(win_rect.z + win_rect.w, 1.0);
#ifdef KEYED_LENS
        // The lens region is a whole strip or window, not the keyed surface:
        // a surface is a small control, so its edge keeps the small-control look.
        s_eff = shade_ref > 0.0 ? shade_ref : SHADE_REF;
#endif
#ifdef JOINED
        // A joined member reads by its own size: the nearest member's (`shape_sdg`).
        s_eff = g_s_eff;
#endif
#ifdef SHAPED
        // A joined shape reads by its smallest box, not its bounding box: a
        // drawer opening under a bar must not change how wide the bar's own edge is.
        s_eff = min(s_eff, 2.0 * box0.z * box0.w / max(box0.z + box0.w, 1.0));
        s_eff = min(s_eff, 2.0 * box1.z * box1.w / max(box1.z + box1.w, 1.0));
        if (boxes > 2.5) {
            s_eff = min(s_eff, 2.0 * box2.z * box2.w / max(box2.z + box2.w, 1.0));
        }
        if (boxes > 3.5) {
            s_eff = min(s_eff, 2.0 * box3.z * box3.w / max(box3.z + box3.w, 1.0));
        }
#endif
        float sref = shade_ref > 0.0 ? shade_ref : SHADE_REF;
        float xs = max(s_eff / sref, 0.05);
        float sw = max(min(band_w, max(SHADE_BASE * sref * pow(xs, SHADE_P), SHADE_MIN * sref / SHADE_REF)), 1.0);
        float sk = min(pow(xs, -SHADE_Q), 1.0);
        // A translucent window's rim stays legible however large the window is
        // (the size rule would fade it to a third): it is what ties the window to
        // the bar and the launcher.
        if (inner > 0.0 && opaque < 0.5) {
            sk = max(sk, WINDOW_RIM_K);
        }
        float su = 1.0 - clamp(-d / sw, 0.0, 1.0);
        c *= 1.0 - mix(EDGE_DARK, EDGE_DARK_L, scheme_light) * sk * su * su * rs;
        // One lit edge: a soft gradient that falls off over the outer half of
        // the band from the hairline (drawn below), lit like it. No separate line.
        float fall = 1.0 - clamp(-d / (0.5 * sw), 0.0, 1.0);
        float lit = LINE_IN * (0.35 + max(dot(grad, LIGHT), 0.0) + 0.5 * max(dot(grad, -LIGHT), 0.0));
        vec3 ltint = rim_color.rgb / max(rim_color.a, 0.0001);
        c = mix(c, ltint, clamp(fall * fall * lit * rim_color.a * rs * sk, 0.0, 1.0));
        // The rim carries the backdrop's own colour: the sharp backdrop, brighter and
        // more saturated, lightens the band (never darkens it), by the same lit
        // falloff. Strong over light or colourful backdrops, nearly nothing over dark.
        // Dark scheme: a soft band (the outer half of the bevel), subtle. Light
        // scheme: a crisp ~3 px edge, brighter and biased to white (Apple's light
        // glass shows a clearly visible rim even over a dark backdrop).
        vec3 rb = mix(vec3(dot(rim_sh, LUMA)), rim_sh, RIM_SAT) * RIM_GAIN;
        rb = mix(rb, vec3(0.95), scheme_light * RIM_WHITE_L);
        float crisp = 1.0 - smoothstep(0.0, RIM_CRISP_PX, -d);
        float dirl = clamp(0.35 + max(dot(grad, LIGHT), 0.0) + 0.5 * max(dot(grad, -LIGHT), 0.0), 0.0, 1.2);
        float rw = mix(RIM_SHARP * fall * fall, RIM_SHARP_L * crisp, scheme_light) * dirl * rs;
        c = mix(c, max(c, clamp(rb, 0.0, 1.0)), clamp(rw, 0.0, 1.0));
    }
#endif

    // Hairline: a 1.25 px band inside the mask edge.
    if (-d < 2.25) {
        float band = 1.0 - smoothstep(0.0, 1.25, -d);
        float tl = max(dot(grad, LIGHT), 0.0);
        float br = max(dot(grad, -LIGHT), 0.0);
        float lobe = LINE_BASE + LINE_TL * tl + LINE_BR * br;
        vec3 tint = rim_color.rgb / max(rim_color.a, 0.0001);
        c = mix(c, tint, clamp(band * lobe * rim_color.a * rim * 2.0, 0.0, 1.0));
        // Light only: `white_rim` is 0 on dark (and on a light pane, whose rim is
        // already ink), so these two mixes leave dark bit for bit. The spec band
        // sits out to 2.25 px, past the rim's own 1.25, hence the wider test.
        float white_rim = scheme_light * step(0.5, dot(tint, LUMA));
        float spec = (1.0 - smoothstep(0.0, 1.0, abs(-d - 1.75) - 0.5)) * tl;
        c = mix(c, vec3(1.0), white_rim * SPEC_L * rim_color.a * spec);
        c = mix(c, LIFT_L, white_rim * INK_EDGE_L * rim_color.a * (1.0 - smoothstep(0.0, 1.1, -d)));
    }

    // Seam: a faint dark line just outside the window's own edge.
    if (inner > 0.0) {
        c *= 1.0 - mix(SEAM, SEAM_L, scheme_light) * max(1.0 - abs(d + inner - 0.5), 0.0);
    }

    c += (grain(floor(gl_FragCoord.xy)) - 0.5) / 255.0;
#ifdef KEYED_LENS
    // The flat key over the lensed backdrop (ADR 0079 composite, ADR 0132): the
    // key's own colour at its alpha over the bent, dispersed backdrop `color`.
    // None of the glass's tint, smoke, lift or body colour is applied, so the
    // lens differs from the flat key by the bend, the dispersion and the rim
    // alone. Inside the key only; the result is opaque there, so the window's
    // hole shows exactly this. Cut at the window's rounded corners, as the flat
    // key is.
    float ka = klab < 1.5 ? key_alpha.x : klab < 2.5 ? key_alpha.y : klab < 3.5 ? key_alpha.z : key_alpha.w;
    vec3 kcol = klab < 1.5 ? lens_key0.rgb : klab < 2.5 ? lens_key1.rgb : klab < 3.5 ? lens_key2.rgb : lens_key3.rgb;
    vec3 kc = kcol * ka + clamp(color.rgb, 0.0, 1.0) * (1.0 - ka);
    // The rim: the hairline and its lit lobe, the lens's one tint (ADR 0132).
    float hair = 1.0 - smoothstep(0.0, 1.25, -d);
    float tl = max(dot(kgrad, LIGHT), 0.0);
    float br = max(dot(kgrad, -LIGHT), 0.0);
    vec3 rtint = rim_color.rgb / max(rim_color.a, 0.0001);
    kc = mix(kc, rtint, clamp(hair * (LINE_BASE + LINE_TL * tl + LINE_BR * br) * rim_color.a * rim * 2.0, 0.0, 1.0));
    float kcov = (1.0 - smoothstep(-0.5, 0.5, d)) * (1.0 - smoothstep(-0.5, 0.5, rounded_sd(gl_FragCoord.xy)));
    gl_FragColor = finish(vec4(clamp(kc, 0.0, 1.0) * kcov, kcov));
#else
    color = finish(vec4(clamp(c, 0.0, 1.0), color.a));

    gl_FragColor = color * (1.0 - smoothstep(-0.5, 0.5, d));
#endif
}
"#;

/// Largest share of a glass shape's shorter side its bevel may span.
const GLASS_BEVEL_FRACTION: f32 = 0.3;
/// The same for a lens or regular preset.
const LENS_BEVEL_FRACTION: f32 = 0.4;
/// Largest rim bend of a lens or a bar-less regular shape, as a share of its
/// shorter side: a 30 px cell bends by under 5 px, a 130 px pill keeps the full
/// configured strength.
pub const LENS_STRENGTH_FRACTION: f32 = 0.16;
/// The cap for a lens glass pane that is not a nested cell (the bar's pill, a
/// card): a 40 px pill bends its rim by up to 16 px, like the launcher card's
/// 28, not by the 6 px that [`LENS_STRENGTH_FRACTION`] left it (the bend is
/// already scaled down with size by `glass::lens_scale`).
pub const LENS_STRENGTH_FRACTION_PANE: f32 = 0.4;

/// The bevel band of a lens whose shorter side is `short` physical px:
/// `bevel`, capped at [`LENS_BEVEL_FRACTION`] of `short`.
pub fn lens_bevel(bevel: f32, short: f32) -> f32 {
    bevel.min(short.max(1.0) * LENS_BEVEL_FRACTION)
}

/// The rim bend of a lens shape whose shorter side is `short` physical px:
/// `strength`, capped at [`LENS_STRENGTH_FRACTION`] of `short`.
pub fn lens_strength(strength: f32, short: f32) -> f32 {
    strength.min(short.max(1.0) * LENS_STRENGTH_FRACTION)
}
/// Share of the dispersion setting that becomes the per-channel bend scale `k`
/// (red `1 - k`, blue `1 + k`).
pub const DISPERSION_K: f32 = 0.12;
/// A window frame's (regular preset) rim displacement per unit of the top-level
/// `refraction` setting: its default 1.5 px reads as 7.5.
const REGULAR_GAIN: f32 = 5.0;

/// The frost program's uniforms, in the order [`frost_uniforms`] sets them.
const FROST_UNIFORMS: [(&str, UniformType); 12] = [
    ("win_rect", UniformType::_4f),
    ("radius", UniformType::_1f),
    ("frost_tint", UniformType::_4f),
    ("phys_map", UniformType::_4f),
    ("rim_color", UniformType::_4f),
    ("inner", UniformType::_1f),
    ("opaque", UniformType::_1f),
    ("bar", UniformType::_1f),
    ("px_uv", UniformType::_2f),
    ("scheme_light", UniformType::_1f),
    ("adapt", UniformType::_1f),
    ("adapt_layers", UniformType::_1f),
];

/// Compile the frost backdrop program (COMP-02 §9). Cached by the caller.
pub fn compile_frost(renderer: &mut GlesRenderer) -> Result<GlesTexProgram, GlesError> {
    renderer.compile_custom_texture_shader(
        format!("{BACKDROP_HEAD}{FROST_BODY}"),
        &FROST_UNIFORMS.map(|(name, ty)| UniformName::new(name, ty)),
    )
}

/// The annotation panel's backdrop (ADR 0071): the blur saturated to 140%
/// about Rec.709 luma and cut to the panel's rounded rectangle. No tint and
/// no grain: the tint is the panel texture's own fill, drawn over this, so
/// the dumps (which cannot run a shader) and the screen agree on it.
const HUD_BODY: &str = r#"
const vec3 HUD_LUMA = vec3(0.2126, 0.7152, 0.0722);
const float HUD_SATURATE = 1.4;

void main() {
    vec4 color = texture2D(tex, v_coords);
    float l = dot(color.rgb, HUD_LUMA);
    color.rgb = clamp(mix(vec3(l), color.rgb, HUD_SATURATE), 0.0, 1.0);
    color = finish(color);

    vec2 half_size = win_rect.zw * 0.5;
    vec2 p = gl_FragCoord.xy - (win_rect.xy + half_size);
    float r = min(radius, min(half_size.x, half_size.y));
    vec2 q = abs(p) - half_size + r;
    float d = length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
    gl_FragColor = color * (1.0 - smoothstep(-0.5, 0.5, d));
}
"#;

/// Compile the annotation panel's backdrop program. Uniforms are
/// [`rounding_uniforms`]'. Cached by the caller.
pub fn compile_hud_glass(renderer: &mut GlesRenderer) -> Result<GlesTexProgram, GlesError> {
    renderer.compile_custom_texture_shader(
        format!("{BACKDROP_HEAD}{HUD_BODY}"),
        &[
            UniformName::new("win_rect", UniformType::_4f),
            UniformName::new("radius", UniformType::_1f),
        ],
    )
}

/// Uniforms the joined program takes per member instead of per element: the
/// bevel and strength, the look's rim, lift, shade and smoke.
const JOINED_PER_MEMBER: [&str; 6] = ["strength", "bevel", "rim", "lift", "shade", "smoke"];

/// The glass program's uniforms, in the order [`glass_uniforms`] sets them.
const GLASS_UNIFORMS: [(&str, UniformType); 22] = [
    ("win_rect", UniformType::_4f),
    ("radius", UniformType::_1f),
    ("px_uv", UniformType::_2f),
    ("dual_tex", UniformType::_1f),
    ("phys_map", UniformType::_4f),
    ("strength", UniformType::_1f),
    ("bevel", UniformType::_1f),
    ("dispersion", UniformType::_1f),
    ("rim", UniformType::_1f),
    ("rim_color", UniformType::_4f),
    ("inner", UniformType::_1f),
    ("opaque", UniformType::_1f),
    ("bar", UniformType::_1f),
    ("lift", UniformType::_1f),
    ("shade", UniformType::_1f),
    ("smoke", UniformType::_1f),
    ("scheme_light", UniformType::_1f),
    ("adapt", UniformType::_1f),
    ("adapt_layers", UniformType::_1f),
    ("shade_ref", UniformType::_1f),
    ("body_tint", UniformType::_4f),
    ("sharp_damp", UniformType::_1f),
];

/// The joined program's uniforms: the element's shared glass uniforms (the
/// [`GLASS_UNIFORMS`] less [`JOINED_PER_MEMBER`]), then `jcount` and the 16
/// member arrays element by element. Every name is set on every frame.
fn joined_names() -> Vec<UniformName<'static>> {
    let mut names: Vec<UniformName<'static>> = GLASS_UNIFORMS
        .iter()
        .filter(|(name, _)| !JOINED_PER_MEMBER.contains(name))
        .map(|&(name, ty)| UniformName::new(name, ty))
        .collect();
    names.push(UniformName::new("jcount", UniformType::_1f));
    for array in ["jbox", "jdat", "jlook"] {
        for k in 0..crate::glass::MAX_SHAPE_PANES {
            names.push(UniformName::new(format!("{array}[{k}]"), UniformType::_4f));
        }
    }
    names
}

/// One member of a joined element, as the joined program reads it.
#[derive(Clone, Copy, Debug)]
pub struct JoinedMember {
    /// The member's shape box, output-local physical.
    pub rect: Rectangle<i32, Physical>,
    /// Its corner radius, physical px.
    pub radius: f32,
    /// The smooth-min width joining it to the member it was reached from
    /// (physical px; 0 for the first member).
    pub fillet: f32,
    /// Its bevel and strength, as [`glass_uniforms_capped_into`] would set them
    /// for it alone.
    pub bevel: f32,
    pub strength: f32,
    /// Its look, as the single-pane path sets them.
    pub lift: f32,
    pub shade: f32,
    pub smoke: f32,
    pub rim: f32,
}

/// The values a joined element shares between its members.
#[derive(Clone, Copy, Debug)]
pub struct JoinedCommon {
    pub dispersion: f32,
    pub rim_color: [f32; 4],
    pub scheme_light: f32,
    pub adapt: f32,
    pub layers: f32,
    pub shade_ref: f32,
}

/// Set the joined program's uniforms, in [`joined_names`] order (the order is
/// not required; smithay binds by name). `bbox` is the elements' bounding box
/// of member shape boxes (`win_rect`); members past the 16th are not sent (the
/// caller keeps shapes to the cap).
pub fn joined_uniforms_into(
    out: &mut Vec<Uniform<'static>>,
    fb: FbMap,
    bbox: Rectangle<i32, Physical>,
    members: &[JoinedMember],
    common: &JoinedCommon,
    tex_size: (i32, i32),
) {
    let n = members.len().min(crate::glass::MAX_SHAPE_PANES);
    let radius = members.first().map_or(0.0, |m| m.radius);
    rounding_uniforms_into(out, bbox, fb, radius);
    out.extend([
        Uniform::new("px_uv", px_uv_for(tex_size)),
        Uniform::new("dual_tex", 0.0f32),
        Uniform::new("phys_map", fb.phys_map()),
        Uniform::new("dispersion", common.dispersion),
        Uniform::new("rim_color", common.rim_color),
        Uniform::new("inner", 0.0f32),
        Uniform::new("opaque", 0.0f32),
        Uniform::new("bar", 0.0f32),
        Uniform::new("scheme_light", common.scheme_light),
        Uniform::new("adapt", common.adapt),
        Uniform::new("adapt_layers", common.layers),
        Uniform::new("shade_ref", common.shade_ref),
        Uniform::new("body_tint", [0.0f32; 4]),
        Uniform::new("sharp_damp", 0.0f32),
        Uniform::new("jcount", n as f32),
    ]);
    let mut boxes = [[0.0f32; 4]; crate::glass::MAX_SHAPE_PANES];
    let mut dat = [[0.0f32; 4]; crate::glass::MAX_SHAPE_PANES];
    let mut look = [[0.0f32; 4]; crate::glass::MAX_SHAPE_PANES];
    for (k, m) in members.iter().take(n).enumerate() {
        let r = fb.gl_rect(m.rect);
        boxes[k] = [r.loc.x as f32, r.loc.y as f32, r.size.w as f32, r.size.h as f32];
        dat[k] = [m.fillet, m.radius, m.bevel, m.strength];
        look[k] = [m.lift, m.shade, m.smoke, m.rim];
    }
    out.extend(
        boxes
            .iter()
            .enumerate()
            .map(|(k, v)| Uniform::new(format!("jbox[{k}]"), *v)),
    );
    out.extend(
        dat.iter()
            .enumerate()
            .map(|(k, v)| Uniform::new(format!("jdat[{k}]"), *v)),
    );
    out.extend(
        look.iter()
            .enumerate()
            .map(|(k, v)| Uniform::new(format!("jlook[{k}]"), *v)),
    );
}

/// The joined source: the lens body with `JOINED` defined right after the
/// version line, so the body takes its per-member branches.
fn joined_source(src: &str) -> String {
    src.replacen("#version 100\n", "#version 100\n#define JOINED 1\n", 1)
}

/// The joined glass program (ADR 0130), lens preset: a joined element is a pane
/// group, and panes are lenses. Cached by the caller.
pub fn compile_glass_joined(renderer: &mut GlesRenderer) -> Result<GlesTexProgram, GlesError> {
    renderer.compile_custom_texture_shader(
        joined_source(&preset_source(
            &format!("{BACKDROP_HEAD}{JOINED_FNS}{GLASS_BODY}"),
            GlassPreset::Lens,
        )),
        &joined_names(),
    )
}

/// Compile the glass backdrop program (COMP-02 §9). Cached by the caller.
pub fn compile_glass(renderer: &mut GlesRenderer) -> Result<GlesTexProgram, GlesError> {
    let names: Vec<_> = GLASS_UNIFORMS
        .iter()
        .map(|&(name, ty)| UniformName::new(name, ty))
        .collect();
    renderer.compile_custom_texture_shader(format!("{BACKDROP_HEAD}{GLASS_BODY}"), &names)
}

/// `px_uv` for a texture that spans `size` physical px: one physical pixel in
/// that texture's normalised UV space. The glass/frost element samples the
/// blur result, which covers the chain's crop (not the whole output) at any
/// resolution, so `size` is the crop, never the framebuffer.
pub fn px_uv_for(size: (i32, i32)) -> [f32; 2] {
    [1.0 / size.0.max(1) as f32, 1.0 / size.1.max(1) as f32]
}

/// Largest chromatic fringe at `dispersion` 1, in logical px (scaled to
/// physical by the output scale). Red and blue sit this far either side of the
/// bent sample at the outer edge of the bevel; tuned by eye (docs/GLASS_PROFILING.md).
pub const DISPERSION_PX: f32 = 8.0;

/// The glass program's `dispersion` uniform for a configured `dispersion`
/// (0..1): the fringe at the edge in physical px at output scale `upscale`.
pub fn fringe_px(dispersion: f32, upscale: f64) -> f32 {
    (dispersion.clamp(0.0, 1.0) as f64 * DISPERSION_PX as f64 * upscale) as f32
}

/// Logical px of the reference control size the edge shading is calibrated to
/// (a bar pill, a lens button): at this size the shading is the original look.
pub const SHADE_REF: f32 = 40.0;
/// The shading band at the reference size, as a share of it (the old lens
/// bevel cap, 0.4 of the shorter side).
pub const SHADE_BASE: f32 = 0.4;
/// How fast the band grows with the effective size: `(s / ref)^SHADE_P`.
pub const SHADE_P: f32 = 0.4;
/// How fast the shading's strength falls with it: `(s / ref)^-SHADE_Q`.
pub const SHADE_Q: f32 = 0.4;
/// Narrowest the band goes, logical px.
pub const SHADE_MIN: f32 = 2.0;

fn shade_consts() -> String {
    format!(
        "const float SHADE_REF = {SHADE_REF:?};\nconst float SHADE_BASE = {SHADE_BASE:?};\nconst float SHADE_P = {SHADE_P:?};\nconst float SHADE_Q = {SHADE_Q:?};\nconst float SHADE_MIN = {SHADE_MIN:?};\n"
    )
}

/// The effective size of a `w` x `h` pane for edge shading: the harmonic mean
/// of its sides (a square is its side, a long skinny box about twice its
/// short side). Any unit.
pub fn shade_size(w: f32, h: f32) -> f32 {
    2.0 * w * h / (w + h).max(1.0)
}

/// CPU mirror of the shader's shading band, in the pane's px: `band` is the
/// pane's bevel (what bounds the shading), `scale` the output scale.
pub fn shade_band(w: f32, h: f32, band: f32, scale: f32) -> f32 {
    let sref = SHADE_REF * scale;
    let xs = (shade_size(w, h) / sref).max(0.05);
    band.min((SHADE_BASE * sref * xs.powf(SHADE_P)).max(SHADE_MIN * scale))
        .max(1.0)
}

/// CPU mirror of the shader's strength factor, 1.0 up to the reference size.
pub fn shade_strength(w: f32, h: f32, scale: f32) -> f32 {
    let xs = (shade_size(w, h) / (SHADE_REF * scale)).max(0.05);
    xs.powf(-SHADE_Q).min(1.0)
}

/// Set the `shade_ref` uniform: the reference size in physical px
/// (`SHADE_REF * output scale`), if `uniforms` has one.
pub fn set_shade_ref(uniforms: &mut [Uniform<'static>], scale: f64) {
    for u in uniforms.iter_mut().filter(|u| u.name == "shade_ref") {
        u.value = smithay::backend::renderer::gles::UniformValue::_1f(SHADE_REF * scale as f32);
    }
}

/// Set the `body_tint` uniform (a trusted prompt's palette tint; straight rgb
/// and alpha), if `uniforms` has one.
pub fn set_body_tint(uniforms: &mut [Uniform<'static>], tint: [f32; 4]) {
    for u in uniforms.iter_mut().filter(|u| u.name == "body_tint") {
        u.value = smithay::backend::renderer::gles::UniformValue::_4f(tint[0], tint[1], tint[2], tint[3]);
    }
}

/// Set the `sharp_damp` uniform (the share of the bevel's lightly blurred half
/// held back; a trusted prompt's), if `uniforms` has one.
pub fn set_sharp_damp(uniforms: &mut [Uniform<'static>], damp: f32) {
    for u in uniforms.iter_mut().filter(|u| u.name == "sharp_damp") {
        u.value = smithay::backend::renderer::gles::UniformValue::_1f(damp);
    }
}

/// Mark the `packed` uniform (if `uniforms` has one): `tex` is the blurred
/// result with the lightly blurred level beside it, which the chromatic
/// fringe reads.
pub fn set_packed(uniforms: &mut [Uniform<'static>], packed: bool) {
    for u in uniforms.iter_mut().filter(|u| u.name == "dual_tex") {
        u.value = smithay::backend::renderer::gles::UniformValue::_1f(if packed { 1.0 } else { 0.0 });
    }
}

/// Point the `px_uv` uniform (if `uniforms` has one) at a texture spanning
/// `size` physical px. The builders cannot know the crop: it is only fixed
/// once the backdrop is refreshed, so the blur store applies it here.
pub fn set_px_uv(uniforms: &mut [Uniform<'static>], size: (i32, i32)) {
    let [x, y] = px_uv_for(size);
    for u in uniforms.iter_mut().filter(|u| u.name == "px_uv") {
        u.value = smithay::backend::renderer::gles::UniformValue::_2f(x, y);
    }
}

/// [`rounding_uniforms`] plus the frost tint (straight alpha, as configured)
/// and a window bezel's `(width, opaque, premultiplied hairline colour)`;
/// `None` is plain frost, no hairline. `tex_size` is a placeholder for the
/// size the backdrop texture spans (the blur store re-points `px_uv` at the
/// chain's crop, see [`set_px_uv`]),
/// `light` is `Appearance::glass_light()` and `adapt` the effective adaptive
/// strength ([`ec_abyss_config::AdaptiveBlur::effective`]).
#[allow(clippy::too_many_arguments)]
pub fn frost_uniforms_into(
    out: &mut Vec<Uniform<'static>>,
    rect: Rectangle<i32, Physical>,
    fb: FbMap,
    radius: f32,
    tint: [f32; 4],
    bezel: Option<(i32, bool, [f32; 4], i32)>,
    tex_size: (i32, i32),
    light: f32,
    adapt: f32,
) {
    let (inner, opaque, rim_color, bar) =
        bezel.map_or((0.0, 0.0, [0.0; 4], 0.0), |(inner, opaque, rim, bar)| {
            (
                inner.max(0) as f32,
                if opaque { 1.0f32 } else { 0.0 },
                rim,
                bar.max(0) as f32,
            )
        });
    rounding_uniforms_into(out, rect, fb, radius);
    out.extend([
        Uniform::new("frost_tint", tint),
        Uniform::new("phys_map", fb.phys_map()),
        Uniform::new("rim_color", rim_color),
        Uniform::new("inner", inner),
        Uniform::new("opaque", opaque),
        Uniform::new("bar", bar),
        Uniform::new("px_uv", px_uv_for(tex_size)),
        Uniform::new("scheme_light", light),
        Uniform::new("adapt", adapt),
    ]);
}

/// [`frost_uniforms_into`] into a fresh vector.
#[allow(clippy::too_many_arguments)]
pub fn frost_uniforms(
    rect: Rectangle<i32, Physical>,
    fb: FbMap,
    radius: f32,
    tint: [f32; 4],
    bezel: Option<(i32, bool, [f32; 4], i32)>,
    tex_size: (i32, i32),
    light: f32,
    adapt: f32,
) -> Vec<Uniform<'static>> {
    let mut u = Vec::with_capacity(11);
    frost_uniforms_into(&mut u, rect, fb, radius, tint, bezel, tex_size, light, adapt);
    u
}

/// [`rounding_uniforms`] plus the glass parameters. `tex_size` is the
/// size the backdrop texture spans, physical px (a placeholder: the blur store
/// re-points `px_uv` at the chain's crop, see [`set_px_uv`]); `strength` and `bevel`
/// are physical px. `bezel` is a window bezel's width (physical px), whether
/// the window inside it is opaque, and its premultiplied hairline colour;
/// `None` is plain glass with the neutral white hairline. `light` is
/// `Appearance::glass_light()`: 0.0 draws the original dark glass. `adapt` is
/// the effective adaptive strength (0.0 leaves the tint fixed).
#[allow(clippy::too_many_arguments)]
pub fn glass_uniforms_into(
    out: &mut Vec<Uniform<'static>>,
    preset: GlassPreset,
    rect: Rectangle<i32, Physical>,
    fb: FbMap,
    radius: f32,
    tex_size: (i32, i32),
    strength: f32,
    bevel: f32,
    glass: &ec_abyss_config::GlassBlur,
    bezel: Option<(i32, bool, [f32; 4], i32)>,
    light: f32,
    adapt: f32,
) {
    glass_uniforms_lifted_into(
        out, preset, rect, fb, radius, tex_size, strength, bevel, glass, bezel, 0.0, 0.0, 1.0, light, adapt,
        1.0,
    )
}

/// [`glass_uniforms_into`] into a fresh vector.
#[allow(clippy::too_many_arguments)]
pub fn glass_uniforms(
    rect: Rectangle<i32, Physical>,
    fb: FbMap,
    radius: f32,
    tex_size: (i32, i32),
    strength: f32,
    bevel: f32,
    glass: &ec_abyss_config::GlassBlur,
    bezel: Option<(i32, bool, [f32; 4], i32)>,
    light: f32,
    adapt: f32,
) -> Vec<Uniform<'static>> {
    let mut u = Vec::with_capacity(20);
    glass_uniforms_into(
        &mut u,
        GlassPreset::Frost,
        rect,
        fb,
        radius,
        tex_size,
        strength,
        bevel,
        glass,
        bezel,
        light,
        adapt,
    );
    u
}

/// [`glass_uniforms`] with a fill `lift` (a mix toward white, 0.0..=1.0) and a
/// `shade` (a mix toward ink, light only): a glass pane's hover/press/focus
/// fill (`eclipse_glass_v1`). `layers` is how many glass layers stack on the
/// sampled backdrop for adaptive glass: 1 a window, 2 a pane on a window.
/// `smoke` scales the smoke and adaptive tint (1.0 normal; stacked glass less).
#[allow(clippy::too_many_arguments)]
pub fn glass_uniforms_lifted_into(
    out: &mut Vec<Uniform<'static>>,
    preset: GlassPreset,
    rect: Rectangle<i32, Physical>,
    fb: FbMap,
    radius: f32,
    tex_size: (i32, i32),
    strength: f32,
    bevel: f32,
    glass: &ec_abyss_config::GlassBlur,
    bezel: Option<(i32, bool, [f32; 4], i32)>,
    lift: f32,
    shade: f32,
    smoke: f32,
    light: f32,
    adapt: f32,
    layers: f32,
) {
    glass_uniforms_capped_into(
        out,
        preset,
        rect,
        fb,
        radius,
        tex_size,
        strength,
        bevel,
        glass,
        bezel,
        lift,
        shade,
        smoke,
        light,
        adapt,
        layers,
        LENS_STRENGTH_FRACTION,
    );
}

/// The effective `(strength, bevel)` of a glass element of `rect` (its shape box):
/// the configured values are ceilings, bounded by the element's size and its
/// bezel (ADR 0092 bevel rules). Shared by [`glass_uniforms_capped_into`] and the
/// joined builder, so a joined member bends exactly as it would alone.
pub fn effective_bend(
    preset: GlassPreset,
    rect: Rectangle<i32, Physical>,
    strength: f32,
    bevel: f32,
    inner: f32,
    opaque: f32,
    strength_fraction: f32,
) -> (f32, f32) {
    if inner > 0.0 && opaque < 0.5 && bevel > 0.0 && preset != GlassPreset::Frost {
        // A translucent window's bevel runs into its body, bounded by its size.
        let short = rect.size.w.min(rect.size.h).max(1) as f32;
        (strength, lens_bevel(bevel, short))
    } else if inner > 0.0 || bevel <= 0.0 {
        (strength, bevel)
    } else if preset != GlassPreset::Frost {
        // The band is capped at LENS_BEVEL_FRACTION of the shorter side and
        // the bend at LENS_STRENGTH_FRACTION of it, so a small widget bends
        // gently and shows what is directly behind it, not its neighbours.
        let short = rect.size.w.min(rect.size.h).max(1) as f32;
        (strength.min(short * strength_fraction), lens_bevel(bevel, short))
    } else {
        let short = rect.size.w.min(rect.size.h).max(1) as f32;
        let capped = bevel.min(short * GLASS_BEVEL_FRACTION);
        (strength * capped / bevel, capped)
    }
}

/// [`glass_uniforms_lifted_into`] with the lens bend cap as a share of the
/// shape's shorter side. The pane path passes [`LENS_STRENGTH_FRACTION_PANE`]:
/// the bar is one thin pill whose rim must bend as visibly as the launcher's.
#[allow(clippy::too_many_arguments)]
pub fn glass_uniforms_capped_into(
    out: &mut Vec<Uniform<'static>>,
    preset: GlassPreset,
    rect: Rectangle<i32, Physical>,
    fb: FbMap,
    radius: f32,
    tex_size: (i32, i32),
    strength: f32,
    bevel: f32,
    glass: &ec_abyss_config::GlassBlur,
    bezel: Option<(i32, bool, [f32; 4], i32)>,
    lift: f32,
    shade: f32,
    smoke: f32,
    light: f32,
    adapt: f32,
    layers: f32,
    strength_fraction: f32,
) {
    let (inner, opaque, rim_color, bar) =
        bezel.map_or((0.0, 0.0, [1.0; 4], 0.0), |(inner, opaque, rim, bar)| {
            (
                inner.max(0) as f32,
                if opaque { 1.0f32 } else { 0.0 },
                rim,
                bar.max(0) as f32,
            )
        });
    // The configured bevel/strength are ceilings: a thin shape (a bar) gets a
    // bevel of at most GLASS_BEVEL_FRACTION of its shorter side, and the bend
    // shrinks with it, so it keeps a clear centre and the same slope.
    let (strength, bevel) = effective_bend(preset, rect, strength, bevel, inner, opaque, strength_fraction);
    rounding_uniforms_into(out, rect, fb, radius);
    out.extend([
        Uniform::new("px_uv", px_uv_for(tex_size)),
        Uniform::new("dual_tex", 0.0f32),
        Uniform::new("phys_map", fb.phys_map()),
        Uniform::new("strength", strength),
        Uniform::new("bevel", bevel),
        Uniform::new("dispersion", glass.dispersion),
        Uniform::new("rim", glass.rim),
        Uniform::new("rim_color", rim_color),
        Uniform::new("inner", inner),
        Uniform::new("opaque", opaque),
        Uniform::new("bar", bar),
        Uniform::new("lift", lift),
        Uniform::new("shade", shade),
        Uniform::new("smoke", smoke),
        Uniform::new("scheme_light", light),
        Uniform::new("adapt", adapt),
        Uniform::new("adapt_layers", layers),
        Uniform::new("shade_ref", 0.0f32),
        Uniform::new("body_tint", [0.0f32; 4]),
        Uniform::new("sharp_damp", 0.0f32),
    ]);
}

/// [`glass_uniforms_lifted_into`] into a fresh vector.
#[allow(clippy::too_many_arguments)]
pub fn glass_uniforms_lifted(
    rect: Rectangle<i32, Physical>,
    fb: FbMap,
    radius: f32,
    tex_size: (i32, i32),
    strength: f32,
    bevel: f32,
    glass: &ec_abyss_config::GlassBlur,
    bezel: Option<(i32, bool, [f32; 4], i32)>,
    lift: f32,
    shade: f32,
    smoke: f32,
    light: f32,
    adapt: f32,
    layers: f32,
) -> Vec<Uniform<'static>> {
    let mut u = Vec::with_capacity(20);
    glass_uniforms_lifted_into(
        &mut u,
        GlassPreset::Frost,
        rect,
        fb,
        radius,
        tex_size,
        strength,
        bevel,
        glass,
        bezel,
        lift,
        shade,
        smoke,
        light,
        adapt,
        layers,
    );
    u
}

/// Adaptive glass (ADR 0092): contrast of the scheme's text on a backdrop at
/// or above which the tint is left alone, and below which it starts to move.
pub const ADAPT_CONTRAST_OK: f32 = 7.0;
/// Contrast at which the shift is at its largest.
pub const ADAPT_CONTRAST_MIN: f32 = 2.0;
/// Relative luminance of the text: white on a dark scheme, ink on a light one.
const ADAPT_TEXT_DARK: f32 = 1.0;
const ADAPT_TEXT_LIGHT: f32 = 0.01;
/// How much of the tint `strength` 1.0 (or less, times this) may replace, and
/// the most it ever does.
const ADAPT_REACH: f32 = 1.75;
const ADAPT_MAX: f32 = 0.95;
/// The cap on light glass: it keeps some backdrop instead of a white slab.
const ADAPT_MAX_L: f32 = 0.8;
/// Lens and regular light glass is a near-opaque slab (iOS light): the shader's
/// `ADAPT_MAX_L` for those presets.
const ADAPT_MAX_L_SLAB: f32 = 0.95;

/// The light-scheme cap of `preset` (0.95 lens and regular, 0.8 frost).
pub fn adaptive_cap_light(preset: GlassPreset) -> f32 {
    if preset == GlassPreset::Frost {
        ADAPT_MAX_L
    } else {
        ADAPT_MAX_L_SLAB
    }
}
/// Exponent of the response curve: 1 - (1 - a)^EASE steepens small shifts.
const ADAPT_EASE: f32 = 1.5;
/// Share of the backdrop's own luma deviation (centre minus local mean) kept
/// through the correction, scaled by it: a dark band stays a faint ghost
/// under the glass instead of vanishing into a flat slab.
pub const ADAPT_GHOST: f32 = 0.22;
/// Weight of the worst tap over the mean in [`adaptive_luma`].
const ADAPT_WORST: f32 = 0.75;
/// The scheme's surface colour the tint moves toward (the glass smoke).
pub const ADAPT_SURFACE_DARK: [f32; 3] = [0.047, 0.047, 0.055];
pub const ADAPT_SURFACE_LIGHT: [f32; 3] = [0.992, 0.989, 0.980];

/// The luma adaptive glass judges: the mean of the sampled backdrop pulled
/// toward the tap the scheme's text contrasts with least (`hi` for dark, `lo`
/// for light). Mirrors the tail of `backdrop_luma` in the shader.
/// Arm length (physical px) under which the backdrop is read with one tap.
pub const ADAPT_MIN_STEP: f32 = 2.2;

/// Weight of the multi-tap read at arm length `step`: 0 below a quarter of
/// [`ADAPT_MIN_STEP`], 1 from four times it up, smooth between (the shader's `w`).
pub fn adaptive_tap_weight(step: f32) -> f32 {
    let t = ((step - ADAPT_MIN_STEP * 0.25) / (ADAPT_MIN_STEP * 3.75)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// [`adaptive_luma`] faded in from the centre tap `c` by `weight`.
pub fn adaptive_luma_blended(c: f32, mean: f32, lo: f32, hi: f32, light: f32, weight: f32) -> f32 {
    c + (adaptive_luma(mean, lo, hi, light) - c) * weight
}

pub fn adaptive_luma(mean: f32, lo: f32, hi: f32, light: f32) -> f32 {
    let worst = hi + (lo - hi) * light;
    mean + (worst - mean) * ADAPT_WORST
}

/// How far, 0.0..=0.95, adaptive glass pulls the tint toward the scheme's
/// surface colour. `luma` is the display-encoded mean luma of the blurred
/// backdrop, `light` is `Appearance::glass_light()` (0 dark, 1 light) and
/// `strength` the configured strength. 0 when the strength is 0 or the text
/// already contrasts with the backdrop. Mirrors `adapt_amount` in the shader.
pub fn adaptive_amount(luma: f32, light: f32, strength: f32) -> f32 {
    adaptive_amount_for(GlassPreset::Frost, luma, light, strength)
}

/// [`adaptive_amount`] under `preset`'s light-scheme cap.
pub fn adaptive_amount_for(preset: GlassPreset, luma: f32, light: f32, strength: f32) -> f32 {
    let lb = luma.clamp(0.0, 1.0).powi(2);
    let lt = ADAPT_TEXT_DARK + (ADAPT_TEXT_LIGHT - ADAPT_TEXT_DARK) * light;
    let contrast = (lb.max(lt) + 0.05) / (lb.min(lt) + 0.05);
    let need = ((ADAPT_CONTRAST_OK - contrast) / (ADAPT_CONTRAST_OK - ADAPT_CONTRAST_MIN)).clamp(0.0, 1.0);
    adaptive_stacked_for(
        adaptive_cap_light(preset),
        need * strength.max(0.0) * ADAPT_REACH,
        1.0,
        light,
    )
}

/// The response curve for a raw shift `a` under `layers` glass layers (1 a
/// window, 2 a pane on a window's glass): each layer multiplies the exponent.
/// `light` is the scheme (light glass has the lower cap).
pub fn adaptive_stacked(a: f32, layers: f32, light: f32) -> f32 {
    adaptive_stacked_for(ADAPT_MAX_L, a, layers, light)
}

/// [`adaptive_stacked`] with the light-scheme cap `cap_light`.
pub fn adaptive_stacked_for(cap_light: f32, a: f32, layers: f32, light: f32) -> f32 {
    (1.0 - (1.0 - a).max(0.0).powf(ADAPT_EASE * layers.max(1.0)))
        .min(ADAPT_MAX + (cap_light - ADAPT_MAX) * light)
}

/// The luma added back to a corrected pixel: `dev` (its centre-minus-mean
/// luma) kept at [`ADAPT_GHOST`] of its size, scaled by the `amount`.
pub fn adaptive_ghost(dev: f32, amount: f32) -> f32 {
    dev * ADAPT_GHOST * amount
}

/// `tint` moved toward the scheme's surface colour by [`adaptive_amount`].
pub fn adaptive_tint(tint: [f32; 3], luma: f32, light: f32, strength: f32) -> [f32; 3] {
    let a = adaptive_amount(luma, light, strength);
    let mut out = [0.0; 3];
    for i in 0..3 {
        let surface = ADAPT_SURFACE_DARK[i] + (ADAPT_SURFACE_LIGHT[i] - ADAPT_SURFACE_DARK[i]) * light;
        out[i] = tint[i] + (surface - tint[i]) * a;
    }
    out
}

/// How far past the blurred region the glass program samples, physical px:
/// its displacement (with a 5% margin, kept so a build without dispersion keeps
/// its region), plus the widest chromatic fringe, [`fringe_px`] at `dispersion`.
/// Zero for every other mode.
/// What a preset's glass program is fed, in physical px at output scale
/// `upscale`: the rim displacement `strength`, the bevel band cap, the
/// dispersion uniform (px of fringe for frost, the per-channel bend scale `k`
/// for lens and regular) and the rim strength.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PresetLook {
    pub strength: f32,
    pub bevel: f32,
    pub dispersion: f32,
    pub rim: f32,
}

/// [`PresetLook`] of `preset` under `glass`.
pub fn preset_look(preset: GlassPreset, glass: &ec_abyss_config::GlassBlur, upscale: f64) -> PresetLook {
    match preset {
        GlassPreset::Frost => PresetLook {
            strength: (glass.refraction as f64 * upscale) as f32,
            bevel: (glass.bevel as f64 * upscale) as f32,
            dispersion: fringe_px(glass.dispersion, upscale),
            rim: glass.rim,
        },
        GlassPreset::Regular => PresetLook {
            strength: (glass.refraction * REGULAR_GAIN) * upscale as f32,
            bevel: (glass.bevel as f64 * upscale) as f32,
            dispersion: glass.dispersion.clamp(0.0, 1.0) * DISPERSION_K,
            rim: glass.rim,
        },
        GlassPreset::Lens => PresetLook {
            strength: glass.lens.refraction * upscale as f32,
            bevel: (glass.lens.bevel as f64 * upscale) as f32,
            dispersion: glass.lens.dispersion.clamp(0.0, 1.0) * DISPERSION_K,
            rim: glass.lens.rim,
        },
    }
}

/// How far past its region a member of `preset` samples, physical px: its own
/// largest displacement (plus its own dispersion), so a lens never inflates a
/// frost member's reach and the reach does not depend on the size of the union
/// of its tier's crops.
pub fn preset_reach(preset: GlassPreset, glass: &ec_abyss_config::GlassBlur, upscale: f64) -> i32 {
    preset_reach_for(preset, glass, upscale, f32::MAX)
}

/// [`preset_reach`] for members whose shorter side is at most `short` physical
/// px (a lens's bend is capped by it, see [`lens_strength`]).
pub fn preset_reach_for(
    preset: GlassPreset,
    glass: &ec_abyss_config::GlassBlur,
    upscale: f64,
    short: f32,
) -> i32 {
    match preset {
        GlassPreset::Frost => glass_reach(glass.refraction, glass.dispersion, upscale),
        _ => {
            let look = preset_look(preset, glass, upscale);
            let strength = if preset == GlassPreset::Lens {
                lens_strength(look.strength, short)
            } else {
                look.strength
            };
            (strength.max(0.0) * (1.0 + look.dispersion) * 1.05).ceil() as i32 + 1
        }
    }
}

pub fn glass_reach(refraction: f32, dispersion: f32, scale: f64) -> i32 {
    (refraction.max(0.0) as f64 * scale * 1.05 + fringe_px(dispersion, scale) as f64).ceil() as i32
}

/// The map for glass panes (`eclipse_glass_v1`, COMP-02 §9) on `output`, for
/// every transform: panes are drawn on quarter-turned outputs too.
pub fn pane_fb_map(output: &smithay::output::Output) -> Option<FbMap> {
    let mode = output.current_mode()?;
    Some(FbMap::new(output.current_transform(), mode.size))
}

/// The map for windows, bezels, rounding and the trusted prompt and annotation
/// glass: `None` on a rotated output, which keeps the painted border and square
/// corners (VOL1 C-15). See [`windowed_transform`].
pub fn output_fb_map(output: &smithay::output::Output) -> Option<FbMap> {
    let mode = output.current_mode()?;
    windowed_transform(output.current_transform()).then(|| FbMap::new(output.current_transform(), mode.size))
}

/// Whether windows and bezels draw their masks under `transform`: the two
/// transforms they are verified on. A rotated output keeps the painted border
/// (VOL1 C-15) and square corners.
pub fn windowed_transform(transform: Transform) -> bool {
    matches!(transform, Transform::Normal | Transform::Flipped180)
}

/// The render-space to `gl_FragCoord` map for one frame, for all eight
/// transforms (COMP-02 §9). `fb` is the framebuffer (`mode.size`); `transform`
/// is the render transform, i.e. the output's `current_transform()` inverted,
/// which is what `GlesRenderer::render` is handed.
///
/// Render space is what render elements are laid out in: top-left origin,
/// `transform.transform_size(fb)` big. `GlesRenderer::render` projects it as
/// `flip180 * transform.matrix() * ortho`, with the ortho built over the render
/// size and the viewport over `fb`. Because the render size is `fb` with its
/// axes swapped for a quarter turn, every edge maps to an integer edge, with
/// no scaling: see [`FbMap::gl_edge`]. A rotated output is therefore a
/// rectangle in `gl_FragCoord` space like any other, and its rounding mask,
/// shape boxes and blur crop are the same rectangle as they are unrotated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FbMap {
    pub fb: Size<i32, Physical>,
    pub transform: Transform,
}

impl FbMap {
    /// The map for an output in `output_transform` whose mode is `mode`.
    pub fn new(output_transform: Transform, mode: Size<i32, Physical>) -> Self {
        Self {
            fb: mode,
            transform: output_transform.invert(),
        }
    }

    /// The transform that turns framebuffer texels into upright render space:
    /// the output's own transform (the inverse of the render one). A texture
    /// blitted under it (`render_texture_from_to`) lands with
    /// `transform_size(fb)` extent, so a quarter turn's result is upright.
    pub fn orient(&self) -> Transform {
        self.transform.invert()
    }

    /// The render-space size (what elements lay out in).
    pub fn render_size(&self) -> Size<i32, Physical> {
        self.transform.transform_size(self.fb)
    }

    /// Where the render-space edge point `(x, y)` lands in `gl_FragCoord`
    /// pixels (origin bottom-left, as GL). `Normal` leaves `(x, y)` alone,
    /// which is why DRM (`Normal`) needs no mirror; `Flipped180` (winit) is
    /// `y` mirrored against `fb.h`. Quarter turns swap the axes and mirror
    /// whichever of them the turn points down.
    pub fn gl_edge(&self, x: i32, y: i32) -> (i32, i32) {
        let (w, h) = (self.fb.w, self.fb.h);
        match self.transform {
            Transform::Normal => (x, y),
            Transform::_180 => (w - x, h - y),
            Transform::Flipped => (w - x, y),
            Transform::Flipped180 => (x, h - y),
            Transform::_90 => (w - y, x),
            Transform::_270 => (y, h - x),
            Transform::Flipped90 => (w - y, h - x),
            Transform::Flipped270 => (y, x),
        }
    }

    /// The render-space rectangle `rect` as a `gl_FragCoord` rectangle: the
    /// lower-left corner and the size, from its two mapped corners.
    pub fn gl_rect(&self, rect: Rectangle<i32, Physical>) -> Rectangle<i32, Physical> {
        let (ax, ay) = self.gl_edge(rect.loc.x, rect.loc.y);
        let (bx, by) = self.gl_edge(rect.loc.x + rect.size.w, rect.loc.y + rect.size.h);
        Rectangle::new(
            (ax.min(bx), ay.min(by)).into(),
            ((ax - bx).abs(), (ay - by).abs()).into(),
        )
    }

    /// The inverse of [`FbMap::gl_edge`]: the render-space point at
    /// `gl_FragCoord` edge `(x, y)`.
    pub fn phys_edge(&self, x: i32, y: i32) -> (i32, i32) {
        let (w, h) = (self.fb.w, self.fb.h);
        match self.transform {
            Transform::Normal => (x, y),
            Transform::_180 => (w - x, h - y),
            Transform::Flipped => (w - x, y),
            Transform::Flipped180 => (x, h - y),
            Transform::_90 => (y, w - x),
            Transform::_270 => (h - y, x),
            Transform::Flipped90 => (h - y, w - x),
            Transform::Flipped270 => (y, x),
        }
    }

    /// The render-space rectangle whose `gl_FragCoord` image is `rect`.
    pub fn phys_rect(&self, rect: Rectangle<i32, Physical>) -> Rectangle<i32, Physical> {
        let (ax, ay) = self.phys_edge(rect.loc.x, rect.loc.y);
        let (bx, by) = self.phys_edge(rect.loc.x + rect.size.w, rect.loc.y + rect.size.h);
        Rectangle::new(
            (ax.min(bx), ay.min(by)).into(),
            ((ax - bx).abs(), (ay - by).abs()).into(),
        )
    }

    /// The matrix that turns a `gl_FragCoord` offset into a render-space
    /// offset, as a column-major `mat2` (`[c0.x, c0.y, c1.x, c1.y]`, the
    /// layout `mat2(vec4)` reads). Every map here is a signed axis swap, which
    /// is orthogonal, so this matrix is also the inverse of the render-to-gl
    /// one. The shaders apply it as `p = mat2(m.xy, m.zw) * p`; for
    /// `Flipped180` it is `diag(1, -1)`, the old `y_sign`.
    pub fn phys_map(&self) -> [f32; 4] {
        let (ox, oy) = self.gl_edge(0, 0);
        let (ex, ey) = self.gl_edge(1, 0);
        let (fx, fy) = self.gl_edge(0, 1);
        // Columns of the render-to-gl Jacobian; its transpose maps back.
        let (jx0, jy0) = ((ex - ox) as f32, (ey - oy) as f32);
        let (jx1, jy1) = ((fx - ox) as f32, (fy - oy) as f32);
        [jx0, jx1, jy0, jy1]
    }
}

/// The uniform pair describing one window's rounded rectangle.
///
/// `rect` is in physical output pixels with a top-left origin (what the render
/// elements use); `fb` maps it to `gl_FragCoord` space.
pub fn rounding_uniforms_into(
    out: &mut Vec<Uniform<'static>>,
    rect: Rectangle<i32, Physical>,
    fb: FbMap,
    radius: f32,
) {
    let rect = fb.gl_rect(rect);
    out.extend([
        Uniform::new(
            "win_rect",
            [
                rect.loc.x as f32,
                rect.loc.y as f32,
                rect.size.w as f32,
                rect.size.h as f32,
            ],
        ),
        Uniform::new("radius", radius),
    ]);
}

/// [`rounding_uniforms_into`] into a fresh vector (for callers off the per-frame path).
pub fn rounding_uniforms(rect: Rectangle<i32, Physical>, fb: FbMap, radius: f32) -> Vec<Uniform<'static>> {
    let mut u = Vec::with_capacity(2);
    rounding_uniforms_into(&mut u, rect, fb, radius);
    u
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

    // A squared falloff reads as light spreading rather than a band, and the
    // glow eases in from the window edge (as the shadow does) so it never
    // starts at full strength against the rim. Nothing is drawn under the
    // window: the glow is a ring, so a translucent window is not tinted by
    // its own glow.
    float fall = 1.0 - smoothstep(0.0, blur, d);
    float a = fall * fall * smoothstep(0.0, blur * 0.3, d);
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
uniform vec3 shadow_rgb; // black on dark; a warm brown on light (ADR 0084)

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
    float ambient_sigma = max(range * mix(0.40, 0.50, focus), 1.0);
    float d_contact = rounded_box(p - centre - vec2(0.0, drop * 0.2), half_size, r);
    float d_ambient = rounded_box(p - centre - vec2(0.0, drop), half_size, r);
    // The boxes sit `drop` lower than the window, so a window's rounded
    // bottom corners lie inside them: coverage there would be full strength,
    // a dark wedge filling the cutout. Measure from the box edge outward only
    // (coverage caps at one half, its value at the edge), so the corner is as
    // dark as the straight edge beside it.
    float contact = soft(max(d_contact, 0.0), contact_sigma) * mix(0.05, 0.08, focus);
    float ambient = soft(max(d_ambient, 0.0), ambient_sigma) * mix(0.22, 0.32, focus);
    float a = 1.0 - (1.0 - contact) * (1.0 - ambient);

    // Zero at the element edge (`d_ambient` >= range all round it).
    a *= 1.0 - smoothstep(range * 0.5, range, d_ambient);
    // Nothing under the window; a 2 px ease keeps the cut off the window's
    // edge antialiased. The wide ambient sigma, not a fade, is what keeps the
    // shadow from reading as a dark band hugging each rounded corner.
    a *= smoothstep(0.0, 2.0, d_in);
    gl_FragColor = vec4(shadow_rgb * (a * alpha), a * alpha);
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
            UniformName::new("shadow_rgb", UniformType::_3f),
        ],
    )
}

/// Uniforms for one drop shadow: `[range, radius, drop]` from
/// `shadow_geometry` (logical px), the 0..1 `focus` weight and the shadow's
/// straight colour (`Appearance::shadow_rgb`; black on dark).
pub fn shadow_uniforms([range, radius, drop]: [f32; 3], focus: f32, rgb: [f32; 3]) -> Vec<Uniform<'static>> {
    vec![
        Uniform::new("range", range),
        Uniform::new("radius", radius),
        Uniform::new("drop", drop),
        Uniform::new("focus", focus),
        Uniform::new("shadow_rgb", (rgb[0], rgb[1], rgb[2])),
    ]
}

/// How far the shadow drops below the window, as a share of its `range`. Zero:
/// an offset shadow's body lies under the window's rounded corners and fills
/// their cutouts, so depth is the glass rim plus an even halo instead.
/// Not configurable: `shadow { range }` is the only knob COMP-13 §1.1 gives.
pub const SHADOW_DROP: f32 = 0.0;

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

/// The Quick Tile drop preview (`mode "de"`, COMP-05 §3.1): one rounded glass
/// panel over the target zone. A translucent neutral fill and a crisp 1 px
/// white rim inside the edge, with a hairline of shade just inside that so the
/// rim keeps its edge over a bright backdrop. No outward spread: nothing draws
/// past the zone, so it cannot read as a glow. Premultiplied uniforms.
const PREVIEW_SRC: &str = r#"
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

uniform vec4 fill;
uniform vec4 rim;
uniform vec4 shade;
uniform float radius;
uniform float scale;

float rounded_box(vec2 p, vec2 half_size, float r) {
    vec2 q = abs(p) - half_size + r;
    return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

void main() {
    vec2 half_out = size * 0.5 * scale;
    vec2 p = v_coords * size * scale - half_out;
    float r = min(radius * scale, min(half_out.x, half_out.y));
    float d = rounded_box(p, half_out, r);
    float body = 1.0 - smoothstep(-0.5, 0.5, d);
    // Band widths in physical px: the rim is the outermost line, the shade the next.
    float line = max(scale, 1.0);
    float rim_a = body * (1.0 - smoothstep(-line - 0.5, -line + 0.5, d));
    float shade_a = body * (1.0 - rim_a) * (1.0 - smoothstep(-2.0 * line - 0.5, -2.0 * line + 0.5, d));
    vec4 c = fill * body;
    c = rim * rim_a + c * (1.0 - rim.a * rim_a);
    c = shade * shade_a + c * (1.0 - shade.a * shade_a);
    gl_FragColor = c * alpha;
}
"#;

/// Compile the Quick Tile preview program.
pub fn compile_preview(renderer: &mut GlesRenderer) -> Result<GlesPixelProgram, GlesError> {
    renderer.compile_custom_pixel_shader(
        PREVIEW_SRC,
        &[
            UniformName::new("fill", UniformType::_4f),
            UniformName::new("rim", UniformType::_4f),
            UniformName::new("shade", UniformType::_4f),
            UniformName::new("radius", UniformType::_1f),
            UniformName::new("scale", UniformType::_1f),
        ],
    )
}

/// Uniforms for the preview: `fill`, `rim` and `shade` are straight RGBA (they
/// are premultiplied here), `radius` is logical, `scale` the output scale.
pub fn preview_uniforms(
    fill: [f32; 4],
    rim: [f32; 4],
    shade: [f32; 4],
    radius: f32,
    scale: f32,
) -> Vec<Uniform<'static>> {
    let pm = |c: [f32; 4]| [c[0] * c[3], c[1] * c[3], c[2] * c[3], c[3]];
    vec![
        Uniform::new("fill", pm(fill)),
        Uniform::new("rim", pm(rim)),
        Uniform::new("shade", pm(shade)),
        Uniform::new("radius", radius),
        Uniform::new("scale", scale),
    ]
}

/// How far the border glow reaches past the border, logical px. Not
/// configurable: `glow { strength }` scales intensity only (COMP-13 §1.1).
pub const GLOW_RANGE: i32 = 24;

#[cfg(test)]
mod tests {
    use super::*;

    fn fb_norm() -> FbMap {
        FbMap::new(Transform::Normal, (1920, 1080).into())
    }
    fn fb_flip() -> FbMap {
        FbMap::new(Transform::Flipped180, (1920, 1080).into())
    }

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
            fb_norm(),
            13.0,
            (1920, 1080),
            4.0,
            16.0,
            &ec_abyss_config::GlassBlur::default(),
            None,
            0.0,
            0.0,
        );
        let set: Vec<&str> = got.iter().map(|u| &*u.name).collect();
        let compiled: Vec<&str> = GLASS_UNIFORMS.iter().map(|&(name, _)| name).collect();
        assert_eq!(set, compiled);
        // A bezel sets the same list, not a longer one.
        let bezel = glass_uniforms(
            rect,
            fb_norm(),
            19.0,
            (1920, 1080),
            4.0,
            6.0,
            &ec_abyss_config::GlassBlur::default(),
            Some((6, true, [0.43, 0.34, 0.11, 0.45], 30)),
            0.0,
            0.0,
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
    fn joined_glass_sets_exactly_the_uniforms_it_compiles() {
        // The joined program's compile list and its packed uniforms must match
        // name for name (smithay: an unknown name errors, an unset one is zero).
        let members = [
            JoinedMember {
                rect: Rectangle::new((40, 40).into(), (300, 40).into()),
                radius: 12.0,
                fillet: 0.0,
                bevel: 20.0,
                strength: 9.0,
                lift: 0.0,
                shade: 0.0,
                smoke: 1.0,
                rim: 0.5,
            },
            JoinedMember {
                rect: Rectangle::new((290, 80).into(), (50, 160).into()),
                radius: 12.0,
                fillet: 16.0,
                bevel: 14.0,
                strength: 7.0,
                lift: 0.1,
                shade: 0.2,
                smoke: 0.9,
                rim: 0.4,
            },
        ];
        let common = JoinedCommon {
            dispersion: 1.0,
            rim_color: [1.0; 4],
            scheme_light: 0.0,
            adapt: 0.5,
            layers: 2.0,
            shade_ref: 40.0,
        };
        let mut out = Vec::new();
        joined_uniforms_into(
            &mut out,
            fb_norm(),
            Rectangle::new((40, 40).into(), (300, 120).into()),
            &members,
            &common,
            (1920, 1080),
        );
        let mut set: Vec<String> = out.iter().map(|u| u.name.to_string()).collect();
        let mut compiled: Vec<String> = joined_names().iter().map(|u| u.name.to_string()).collect();
        set.sort();
        compiled.sort();
        assert_eq!(set, compiled);
        assert_eq!(compiled.len(), 16 + 1 + 3 * crate::glass::MAX_SHAPE_PANES);
        // The per-member names are the joined program's, not the element's.
        for name in JOINED_PER_MEMBER {
            assert!(!compiled.iter().any(|n| n == name), "{name} is per member");
        }
    }

    #[test]
    fn only_the_joined_source_defines_joined() {
        let body = format!("{BACKDROP_HEAD}{JOINED_FNS}{GLASS_BODY}");
        let joined = joined_source(&preset_source(&body, GlassPreset::Lens));
        assert!(joined.starts_with("#version 100\n#define JOINED 1\n"));
        assert_eq!(joined.matches("float shape_sdg(").count(), 1);
        assert_eq!(joined.matches("#define OWN_BOX g_own_box").count(), 1);
        let plain = preset_source(&format!("{BACKDROP_HEAD}{GLASS_BODY}"), GlassPreset::Lens);
        assert!(!plain.contains("#define JOINED"));
        assert!(plain.contains("#define OWN_BOX win_rect"));
        // The unjoined body still declares the per-member uniforms.
        // ... inside an `#ifndef JOINED`, and the joined one declares plain globals.
        assert!(plain.contains("#ifndef JOINED\nuniform float strength;"));
        assert!(joined.contains("\nfloat strength;"));
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
            fb_flip(),
            20.0,
            (1920, 1080),
            4.0,
            16.0,
            &ec_abyss_config::GlassBlur::default(),
            None,
            0.0,
            0.0,
        );
        got.extend(shape_uniforms(&shape, fb_flip()));
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
            format!("{:?}", shape_uniforms(&shape, fb_flip())),
            format!("{:?}", shape_uniforms(&other, fb_flip()))
        );
    }

    #[test]
    fn plain_glass_keeps_the_white_hairline() {
        let rect = Rectangle::new((0, 0).into(), (300, 200).into());
        let got = glass_uniforms(
            rect,
            fb_norm(),
            13.0,
            (1920, 1080),
            4.0,
            16.0,
            &ec_abyss_config::GlassBlur::default(),
            None,
            0.0,
            0.0,
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
        let fb = FbMap::new(Transform::Normal, (1920, 1080).into());
        let got = rounding_uniforms(rect, fb, 13.0);
        let want = vec![Uniform::new("win_rect", [10.0f32, 20.0, 300.0, 200.0])];
        assert_eq!(win_rect(&got), win_rect(&want));
    }

    #[test]
    fn flipped180_output_mirrors_the_rect() {
        // winit: physical y = 0 is gl_FragCoord.y = fb_height.
        let rect = Rectangle::new((10, 20).into(), (300, 200).into());
        let fb = FbMap::new(Transform::Flipped180, (1920, 1080).into());
        let got = rounding_uniforms(rect, fb, 13.0);
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
            assert!(!windowed_transform(t), "{t:?}");
        }
    }

    const ALL_RENDER_TRANSFORMS: [Transform; 8] = [
        Transform::Normal,
        Transform::_90,
        Transform::_180,
        Transform::_270,
        Transform::Flipped,
        Transform::Flipped90,
        Transform::Flipped180,
        Transform::Flipped270,
    ];

    /// `GlesRenderer::render`'s projection, replicated from smithay 0.7.0
    /// (`renderer/gles/mod.rs`): the ortho over the render size, then
    /// `flip180 * transform.matrix()`, then the viewport over `fb`. Matrices
    /// are indexed `[column][row]`, as cgmath's are.
    fn projected_gl(t: Transform, fb: (i32, i32), x: i32, y: i32) -> (f32, f32) {
        let render = t.transform_size(Size::<i32, Physical>::from(fb));
        let (rw, rh) = (render.w as f32, render.h as f32);
        let mut ortho = [[0.0f32; 3]; 3];
        ortho[0][0] = 2.0 / rw;
        ortho[1][1] = -2.0 / rh;
        ortho[2][0] = -(1.0f32.copysign(ortho[0][0]));
        ortho[2][1] = -(1.0f32.copysign(ortho[1][1]));
        let tm = t.matrix();
        let mut transform = [[0.0f32; 3]; 3];
        for (c, col) in transform.iter_mut().enumerate() {
            for (r, v) in col.iter_mut().enumerate() {
                *v = tm[c][r];
            }
        }
        let flip180 = [[1.0f32, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0]];
        let mul = |m: &[[f32; 3]; 3], v: [f32; 3]| {
            let mut out = [0.0f32; 3];
            for (r, o) in out.iter_mut().enumerate() {
                *o = (0..3).map(|c| m[c][r] * v[c]).sum();
            }
            out
        };
        let v = mul(&flip180, mul(&transform, mul(&ortho, [x as f32, y as f32, 1.0])));
        ((v[0] + 1.0) / 2.0 * fb.0 as f32, (v[1] + 1.0) / 2.0 * fb.1 as f32)
    }

    #[test]
    fn fb_map_matches_smithay_projection_for_every_transform() {
        // The output's mode, and a few points on the render-space edges and inside.
        let fb = (1920, 1080);
        for t in ALL_RENDER_TRANSFORMS {
            let map = FbMap {
                fb: fb.into(),
                transform: t,
            };
            let rs = t.transform_size(Size::<i32, Physical>::from(fb));
            for (x, y) in [
                (0, 0),
                (rs.w, 0),
                (0, rs.h),
                (rs.w, rs.h),
                (300, 200),
                (17, rs.h - 3),
            ] {
                let (gx, gy) = projected_gl(t, fb, x, y);
                let (mx, my) = map.gl_edge(x, y);
                assert!(
                    (gx - mx as f32).abs() < 1e-2 && (gy - my as f32).abs() < 1e-2,
                    "{t:?} ({x}, {y}): projection ({gx}, {gy}), map ({mx}, {my})"
                );
            }
        }
    }

    #[test]
    fn fb_map_rect_is_the_projected_rect_for_every_transform() {
        let fb = (1920, 1080);
        let rect = Rectangle::new((40, 25).into(), (360, 140).into());
        for t in ALL_RENDER_TRANSFORMS {
            let got = FbMap {
                fb: fb.into(),
                transform: t,
            }
            .gl_rect(rect);
            // The four projected corners span exactly the mapped rectangle.
            let corners = [
                projected_gl(t, fb, rect.loc.x, rect.loc.y),
                projected_gl(t, fb, rect.loc.x + rect.size.w, rect.loc.y),
                projected_gl(t, fb, rect.loc.x, rect.loc.y + rect.size.h),
                projected_gl(t, fb, rect.loc.x + rect.size.w, rect.loc.y + rect.size.h),
            ];
            let lo_x = corners.iter().map(|c| c.0).fold(f32::MAX, f32::min);
            let lo_y = corners.iter().map(|c| c.1).fold(f32::MAX, f32::min);
            let hi_x = corners.iter().map(|c| c.0).fold(f32::MIN, f32::max);
            let hi_y = corners.iter().map(|c| c.1).fold(f32::MIN, f32::max);
            assert_eq!(
                (got.loc.x, got.loc.y, got.size.w, got.size.h),
                (
                    lo_x.round() as i32,
                    lo_y.round() as i32,
                    (hi_x - lo_x).round() as i32,
                    (hi_y - lo_y).round() as i32,
                ),
                "{t:?}"
            );
        }
    }

    #[test]
    fn fb_map_phys_map_undoes_the_projection_for_every_transform() {
        // A one-pixel step in render space, seen in gl space, and back through
        // the matrix, is the same step.
        let fb = (1920, 1080);
        for t in ALL_RENDER_TRANSFORMS {
            let m = FbMap {
                fb: fb.into(),
                transform: t,
            }
            .phys_map();
            let (x, y) = (300, 200);
            let (ox, oy) = projected_gl(t, fb, x, y);
            for (dx, dy) in [(1, 0), (0, 1)] {
                let (nx, ny) = projected_gl(t, fb, x + dx, y + dy);
                let (gx, gy) = (nx - ox, ny - oy);
                let back = (m[0] * gx + m[2] * gy, m[1] * gx + m[3] * gy);
                assert!(
                    (back.0 - dx as f32).abs() < 1e-3 && (back.1 - dy as f32).abs() < 1e-3,
                    "{t:?} step ({dx}, {dy}): {back:?}"
                );
            }
        }
    }

    #[test]
    fn fb_map_phys_edge_inverts_gl_edge_for_every_transform() {
        let map_fb = (1920, 1080);
        for t in ALL_RENDER_TRANSFORMS {
            let map = FbMap {
                fb: map_fb.into(),
                transform: t,
            };
            let rs = map.render_size();
            for (x, y) in [(0, 0), (rs.w, rs.h), (17, rs.h - 3), (rs.w, 0)] {
                let (gx, gy) = map.gl_edge(x, y);
                assert_eq!(map.phys_edge(gx, gy), (x, y), "{t:?} ({x}, {y})");
            }
            let rect = Rectangle::new((40, 25).into(), (360, 140).into());
            let gl = map.gl_rect(rect);
            assert_eq!(map.phys_rect(gl), rect, "{t:?}");
        }
    }

    #[test]
    fn fb_map_flip_only_transforms_keep_the_mirror_matrix() {
        let fb = (1920, 1080);
        let normal = FbMap {
            fb: fb.into(),
            transform: Transform::Normal,
        };
        let flipped = FbMap {
            fb: fb.into(),
            transform: Transform::Flipped180,
        };
        assert_eq!(normal.phys_map(), [1.0, 0.0, 0.0, 1.0]);
        // `y_sign` of -1: the mirrored y axis.
        assert_eq!(flipped.phys_map(), [1.0, 0.0, 0.0, -1.0]);
        assert_eq!(normal.gl_edge(10, 20), (10, 20));
        assert_eq!(flipped.gl_edge(10, 20), (10, 1060));
    }

    #[test]
    fn output_transform_is_inverted_for_the_render_transform() {
        // A `_90` output is rendered with `_270`, and its render size is the
        // mode's axes swapped (portrait for a landscape panel).
        let map = FbMap::new(Transform::_90, Size::from((1920, 1080)));
        assert_eq!(map.transform, Transform::_270);
        assert_eq!(map.render_size(), Size::<i32, Physical>::from((1080, 1920)));
        let normal = FbMap::new(Transform::Normal, Size::from((1920, 1080)));
        assert_eq!(normal.render_size(), Size::<i32, Physical>::from((1920, 1080)));
    }

    #[test]
    fn adaptive_tint_is_off_at_zero_strength_and_over_a_contrasting_backdrop() {
        let t = [0.2, 0.3, 0.4];
        for light in [0.0, 1.0] {
            assert_eq!(adaptive_tint(t, 0.0, light, 0.0), t);
            assert_eq!(adaptive_tint(t, 1.0, light, 0.0), t);
        }
        // Dark scheme over a dark backdrop, light scheme over a bright one.
        assert_eq!(adaptive_tint(t, 0.1, 0.0, 1.0), t);
        assert_eq!(adaptive_tint(t, 1.0, 1.0, 1.0), t);
    }

    #[test]
    fn adaptive_tint_darkens_dark_glass_over_bright_and_lightens_light_over_dark() {
        let t = [0.2, 0.3, 0.4];
        let d = adaptive_tint(t, 1.0, 0.0, 0.5);
        assert!(d.iter().zip(t).all(|(a, b)| *a < b), "{d:?}");
        let l = adaptive_tint(t, 0.0, 1.0, 0.5);
        assert!(l.iter().zip(t).all(|(a, b)| *a > b), "{l:?}");
        // Stronger moves further, and brighter moves further.
        assert!(adaptive_amount(1.0, 0.0, 0.4) > adaptive_amount(1.0, 0.0, 0.2));
        assert!(adaptive_amount(1.0, 0.0, 0.3) > adaptive_amount(0.5, 0.0, 0.3));
        assert!(adaptive_amount(1.0, 0.0, 1.0) <= ADAPT_MAX);
    }

    #[test]
    fn adaptive_luma_judges_a_busy_backdrop_by_its_worst_patch() {
        // Light glass (ink) over a mostly bright backdrop with a dark patch.
        let l = adaptive_luma(0.7, 0.1, 0.9, 1.0);
        assert!(l < 0.7 && l > 0.1, "{l}");
        assert!(adaptive_amount(l, 1.0, 1.0) > adaptive_amount(0.7, 1.0, 1.0));
        // Dark glass (white) over a mostly dark backdrop with a bright patch.
        assert!(adaptive_luma(0.2, 0.1, 0.9, 0.0) > 0.2);
        // A flat backdrop is unchanged.
        assert_eq!(adaptive_luma(0.5, 0.5, 0.5, 1.0), 0.5);
    }

    #[test]
    fn a_pane_on_window_glass_shifts_further_than_the_window() {
        for a in [0.1, 0.3, 0.5] {
            assert!(adaptive_stacked(a, 2.0, 0.0) > adaptive_stacked(a, 1.0, 0.0));
        }
        assert!(adaptive_stacked(5.0, 2.0, 1.0) <= ADAPT_MAX_L);
        assert_eq!(adaptive_stacked(0.0, 2.0, 1.0), 0.0);
    }

    #[test]
    fn adaptive_ghost_keeps_a_small_share_of_the_deviation_and_none_when_uncorrected() {
        assert_eq!(adaptive_ghost(-0.4, 0.0), 0.0);
        let g = adaptive_ghost(-0.4, 0.8);
        assert!(g < 0.0 && g.abs() < 0.4 * 0.25, "{g}");
        assert!(adaptive_ghost(-0.4, 0.8).abs() > adaptive_ghost(-0.4, 0.4).abs());
    }

    /// The preset variants differ from each other in exactly one `#define`, and
    /// the shaped ones add the shape functions on top.
    #[test]
    fn preset_variants_differ_only_in_their_define() {
        let base = format!("{BACKDROP_HEAD}{GLASS_BODY}");
        let sources: Vec<String> = GlassPreset::ALL
            .iter()
            .map(|p| preset_source(&base, *p))
            .collect();
        for (p, src) in GlassPreset::ALL.iter().zip(&sources) {
            assert_eq!(src.matches("#define GLASS_").count(), 1, "{p:?}");
            assert!(src.contains(&format!("#define {} 1", p.define())));
            // Dropping the define gives back the plain program.
            let plain = src.replace(&format!("\n#define {} 1\n", p.define()), "");
            assert_eq!(
                plain,
                base.replacen("// SHADE_CONSTS", &shade_consts(), 1),
                "{p:?}"
            );
        }
        let shaped = shaped_source(&sources[0]);
        assert!(shaped.find("#define SHAPED").unwrap() < shaped.find("#ifdef SHAPED").unwrap());
        assert_eq!(shaped.matches("#define GLASS_LENS").count(), 1);
        // Slots and selection.
        assert_eq!(GlassPreset::ALL.map(GlassPreset::index), [0, 1, 2]);
        assert_eq!(GlassPreset::of(true, false), GlassPreset::Lens);
        assert_eq!(GlassPreset::of(true, true), GlassPreset::Lens);
        assert_eq!(GlassPreset::of(false, true), GlassPreset::Regular);
        assert_eq!(GlassPreset::of(false, false), GlassPreset::Frost);
        // A layer sheet: the 40 px bar is a lens, a launcher or toast regular.
        assert_eq!(GlassPreset::of_sheet(40.0), GlassPreset::Lens);
        assert_eq!(GlassPreset::of_sheet(48.0), GlassPreset::Lens);
        assert_eq!(GlassPreset::of_sheet(72.0), GlassPreset::Regular);
        assert_eq!(GlassPreset::of_sheet(370.0), GlassPreset::Regular);
    }

    /// The CPU mirror's light cap agrees with the shader's per-preset cap.
    #[test]
    fn adaptive_caps_match_the_shader() {
        for preset in GlassPreset::ALL {
            assert_eq!(preset != GlassPreset::Frost, adaptive_cap_light(preset) > 0.9);
        }
        assert!(BACKDROP_HEAD.contains("const float ADAPT_MAX_L = 0.95;"));
        assert!(BACKDROP_HEAD.contains("const float ADAPT_MAX_L = 0.8;"));
        // Light, a dark backdrop: the slab reaches further than frost.
        let slab = adaptive_amount_for(GlassPreset::Lens, 0.0, 1.0, 1.0);
        let frost = adaptive_amount_for(GlassPreset::Frost, 0.0, 1.0, 1.0);
        assert!(
            (slab - 0.95).abs() < 1e-4 && (frost - 0.8).abs() < 1e-4,
            "{slab} {frost}"
        );
        assert_eq!(adaptive_amount(0.0, 1.0, 1.0), frost);
    }

    /// A small lens bends by a share of its shorter side, a big one keeps the
    /// configured strength, and the reach follows the capped bend.
    #[test]
    fn small_lenses_bend_gently() {
        assert!((lens_strength(28.0, 30.0) - 4.8).abs() < 1e-4);
        assert_eq!(lens_strength(28.0, 200.0), 28.0);
        assert!(lens_strength(28.0, 0.0) <= LENS_STRENGTH_FRACTION);
        let glass = ec_abyss_config::GlassBlur::default();
        let small = preset_reach_for(GlassPreset::Lens, &glass, 1.0, 30.0);
        let big = preset_reach_for(GlassPreset::Lens, &glass, 1.0, 400.0);
        assert!(small < big);
        assert_eq!(big, preset_reach(GlassPreset::Lens, &glass, 1.0));
        // The bend never reaches farther than the member is thick.
        assert!(small as f32 <= 30.0 * LENS_STRENGTH_FRACTION * 1.3 + 3.0);
    }

    /// A member's reach is its own largest displacement (plus its own
    /// dispersion), in physical px: it scales with the output scale, not with
    /// any union of crops, and a lens never inflates a frost member's.
    #[test]
    fn reach_covers_each_members_own_displacement() {
        let glass = ec_abyss_config::GlassBlur::default();
        for scale in [1.0, 1.5, 2.0] {
            let lens = preset_look(GlassPreset::Lens, &glass, scale);
            let reach = preset_reach(GlassPreset::Lens, &glass, scale);
            // The farthest any channel is bent (blue, by 1 + k) is inside it.
            assert!(
                reach as f32 >= lens.strength * (1.0 + lens.dispersion),
                "lens at {scale}"
            );
            let reg = preset_look(GlassPreset::Regular, &glass, scale);
            assert!(preset_reach(GlassPreset::Regular, &glass, scale) as f32 >= reg.strength);
            let frost = preset_reach(GlassPreset::Frost, &glass, scale);
            assert_eq!(frost, glass_reach(glass.refraction, glass.dispersion, scale));
        }
        // A lens member's reach does not change a frost member's, whatever the
        // lens setting.
        let mut big = glass.clone();
        big.lens.refraction = 90.0;
        assert_eq!(
            preset_reach(GlassPreset::Frost, &big, 1.0),
            preset_reach(GlassPreset::Frost, &glass, 1.0)
        );
        assert!(preset_reach(GlassPreset::Lens, &big, 1.0) > preset_reach(GlassPreset::Lens, &glass, 1.0));
        // Linear in the output scale (physical px), not in anything else.
        let one = preset_look(GlassPreset::Lens, &glass, 1.0).strength;
        let two = preset_look(GlassPreset::Lens, &glass, 2.0).strength;
        assert!((two - 2.0 * one).abs() < 1e-3);
    }

    /// The one-tap to multi-tap switch is continuous: the weight is 0 at a quarter
    /// of the threshold, 1 at four times it, and a surface resized one
    /// physical px at a time (arm length `ADAPT_SPAN / 2` of its size) never
    /// moves the tint by more than a few percent per px, whatever the backdrop's
    /// spread. The tint amount is itself continuous in that luma (and in `smoke`,
    /// `adapt`, the rim band and the lens plateau, which all fade to the value
    /// they skip).
    #[test]
    fn adaptive_one_tap_to_multi_tap_is_continuous() {
        assert_eq!(adaptive_tap_weight(0.0), 0.0);
        assert_eq!(adaptive_tap_weight(ADAPT_MIN_STEP * 0.25), 0.0);
        assert_eq!(adaptive_tap_weight(ADAPT_MIN_STEP * 4.0), 1.0);
        assert_eq!(adaptive_tap_weight(18.0), 1.0);
        for (light, c, mean, lo, hi) in [(1.0, 0.9, 0.5, 0.1, 0.9), (0.0, 0.1, 0.5, 0.1, 0.9)] {
            let arm = |size: f32| (size * 0.5 * 0.19).min(18.0);
            let tint = |size: f32| {
                let l = adaptive_luma_blended(c, mean, lo, hi, light, adaptive_tap_weight(arm(size)));
                adaptive_amount(l, light, 0.5)
            };
            let mut worst = 0.0f32;
            let mut size = 0.0f32;
            while size < 100.0 {
                worst = worst.max((tint(size + 1.0) - tint(size)).abs());
                size += 0.25;
            }
            assert!(worst < 0.09, "tint step per px {worst} (light {light})");
        }
    }

    // ---- edge shading: one continuous rule of the effective size -----------

    /// The band the pane's bevel allows (`lens_bevel` of the 40 px logical cap)
    /// and what shading it gets, for a `w` x `h` pane at `scale`.
    fn shaded(w: f32, h: f32, scale: f32) -> (f32, f32) {
        let band = lens_bevel(40.0 * scale, w.min(h));
        (shade_band(w, h, band, scale), shade_strength(w, h, scale))
    }

    #[test]
    fn the_shading_is_continuous_and_monotonic() {
        let (mut pb, mut ps) = shaded(50.0, 50.0, 1.0);
        for n in 51..4000 {
            let (b, k) = shaded(n as f32, n as f32, 1.0);
            assert!(b >= pb - 1e-4, "band shrank at {n}: {pb} -> {b}");
            assert!(k <= ps + 1e-4, "strength grew at {n}: {ps} -> {k}");
            assert!((b - pb).abs() / pb <= 0.02, "band jumps {pb} -> {b} at {n}");
            assert!((k - ps).abs() / ps <= 0.02, "strength jumps {ps} -> {k} at {n}");
            (pb, ps) = (b, k);
        }
        // Below that the band is the bevel's own 0.4 of the side (linear: a
        // px of size is at most 1/s of it), never a step.
        let mut prev = shaded(20.0, 20.0, 1.0).0;
        for n in 21..50 {
            let b = shaded(n as f32, n as f32, 1.0).0;
            assert!((b - prev).abs() / prev <= 1.0 / (n - 1) as f32 + 1e-3, "{n}");
            prev = b;
        }
    }

    #[test]
    fn skinny_and_fat_panes_read_alike_and_the_axes_are_symmetric() {
        for (w, h) in [(2000.0, 60.0), (300.0, 700.0), (1900.0, 1000.0), (781.0, 205.0)] {
            assert_eq!(shaded(w, h, 1.0), shaded(h, w, 1.0), "{w}x{h}");
        }
        let near = |a: f32, b: f32| (a - b).abs() / a.max(b) <= 0.15;
        let (bar, bar_k) = shaded(2000.0, 60.0, 1.0);
        let (small, small_k) = shaded(120.0, 60.0, 1.0);
        assert!(
            near(bar, small) && near(bar_k, small_k),
            "{bar} {small} {bar_k} {small_k}"
        );
        let (sq, sq_k) = shaded(900.0, 900.0, 1.0);
        let (card, card_k) = shaded(1900.0, 1000.0, 1.0);
        assert!(
            near(sq, card) && near(sq_k, card_k),
            "{sq} {card} {sq_k} {card_k}"
        );
    }

    #[test]
    fn small_controls_keep_the_original_look_and_big_ones_never_get_wider_than_it() {
        // At the reference size (a 40 px pill) the band is the old 0.4 of the
        // side and the strength the old full one, exactly.
        let (b, k) = shaded(40.0, 40.0, 1.0);
        assert!((b - 16.0).abs() < 1e-3 && (k - 1.0).abs() < 1e-6, "{b} {k}");
        // Below it nothing changes at all (the bevel is already the smaller).
        for n in 8..40 {
            let (b, k) = shaded(n as f32, n as f32, 1.0);
            assert!((b - 0.4 * n as f32).abs() < 1e-3 && k == 1.0, "{n}");
        }
        // Above it the band is never wider than the old one nor the strength
        // stronger; the table in docs/GLASS_PROFILING.md has the numbers.
        for n in 40..4000 {
            let old = lens_bevel(40.0, n as f32);
            let (b, k) = shaded(n as f32, n as f32, 1.0);
            assert!(b <= old + 1e-3 && k <= 1.0, "{n}");
        }
    }

    #[test]
    fn the_shading_scales_with_the_output() {
        for (w, h) in [(40.0, 40.0), (300.0, 700.0), (2000.0, 60.0), (900.0, 900.0)] {
            let (b1, k1) = shaded(w, h, 1.0);
            let (b2, k2) = shaded(2.0 * w, 2.0 * h, 2.0);
            assert!((b2 - 2.0 * b1).abs() < 1e-2, "{w}x{h}: {b1} {b2}");
            assert!((k2 - k1).abs() < 1e-5);
        }
    }

    #[test]
    fn the_shader_carries_the_mirrored_constants() {
        let c = shade_consts();
        for (name, v) in [
            ("SHADE_REF", SHADE_REF),
            ("SHADE_BASE", SHADE_BASE),
            ("SHADE_P", SHADE_P),
            ("SHADE_Q", SHADE_Q),
            ("SHADE_MIN", SHADE_MIN),
        ] {
            assert!(c.contains(&format!("const float {name} = {v:?};")), "{name}");
        }
        assert!(GLASS_BODY.contains("// SHADE_CONSTS"));
    }
}
