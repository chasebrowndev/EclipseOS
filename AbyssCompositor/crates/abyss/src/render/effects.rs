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

    vec2 half_size = win_rect.zw * 0.5;
    vec2 p = gl_FragCoord.xy - (win_rect.xy + half_size);
    float r = min(radius, min(half_size.x, half_size.y));
    vec2 q = abs(p) - half_size + r;
    float d = length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;

    gl_FragColor = color * (1.0 - smoothstep(-0.5, 0.5, d));
}
"#;

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

    // `step` drops everything under the window: the shadow is a ring, so a
    // translucent window is not darkened by its own shadow.
    float a = (1.0 - smoothstep(0.0, blur, d)) * step(0.0, d);
    gl_FragColor = shadow_color * (a * alpha);
}
"#;

pub fn compile_shadow(renderer: &mut GlesRenderer) -> Result<GlesPixelProgram, GlesError> {
    renderer.compile_custom_pixel_shader(
        SHADOW_SRC,
        &[
            UniformName::new("shadow_color", UniformType::_4f),
            UniformName::new("blur", UniformType::_1f),
            UniformName::new("radius", UniformType::_1f),
        ],
    )
}

/// Uniforms for one drop shadow over its area (the bordered rect grown by
/// `range`): `range` is how far it reaches and `radius` the corner radius it
/// hugs, both logical.
pub fn shadow_uniforms(range: f32, radius: f32) -> Vec<Uniform<'static>> {
    vec![
        // Premultiplied, so the colour carries its own alpha.
        Uniform::new("shadow_color", [0.0, 0.0, 0.0, SHADOW_ALPHA]),
        Uniform::new("blur", range),
        Uniform::new("radius", radius),
    ]
}

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

/// Opacity of the shadow directly against the window edge. Not configurable:
/// `shadow { range }` is the only knob COMP-13 §1.1 gives.
const SHADOW_ALPHA: f32 = 0.55;

#[cfg(test)]
mod tests {
    use super::*;

    fn win_rect(uniforms: &[Uniform<'static>]) -> String {
        format!("{:?}", uniforms[0])
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
