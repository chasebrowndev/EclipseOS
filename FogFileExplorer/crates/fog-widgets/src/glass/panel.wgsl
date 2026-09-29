// SPDX-License-Identifier: AGPL-3.0-only
//
// One glass panel (FOG §Visual design, "Floating glass"): a rounded SDF body
// of tint over an optional blurred backdrop, a top-lit sheen, a directional
// rim highlight, film grain and a soft shadow outside the edge.
//
// The viewport is the primitive's bounds (the panel plus its shadow margin);
// all geometry is in framebuffer pixels, so `position` needs no transform.
// Colours are linear and premultiplied.

struct Panel {
    // x, y, w, h of the panel body, framebuffer pixels.
    rect: vec4<f32>,
    tint: vec4<f32>,
    // rgb, strength.
    rim: vec4<f32>,
    shadow: vec4<f32>,
    // offset x, offset y, blur, rim width.
    shadow_geo: vec4<f32>,
    // radius, grain, opacity, mode (0 whole, 1 fill, 2 edge).
    params: vec4<f32>,
    // backdrop width, height, encode to sRGB, sheen.
    frame: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Panel;
@group(1) @binding(0) var backdrop: texture_2d<f32>;
@group(1) @binding(1) var smp: sampler;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

fn rounded(p: vec2<f32>, half: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - half + vec2<f32>(r);
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

// Pixel-stable hash, no texture.
fn grain(p: vec2<f32>) -> f32 {
    var q = fract(p * vec2<f32>(0.1031, 0.1030));
    q = q + dot(q, q.yx + 33.33);
    return fract((q.x + q.y) * q.x) - 0.5;
}

fn over(top: vec4<f32>, under: vec4<f32>) -> vec4<f32> {
    return top + under * (1.0 - top.a);
}

fn encode(c: vec4<f32>) -> vec4<f32> {
    if (u.frame.z < 0.5 || c.a <= 0.0) {
        return c;
    }
    let s = c.rgb / c.a;
    let e = select(1.055 * pow(s, vec3<f32>(1.0 / 2.4)) - 0.055, s * 12.92, s <= vec3<f32>(0.0031308));
    return vec4<f32>(e * c.a, c.a);
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let half = u.rect.zw * 0.5;
    let p = pos.xy - (u.rect.xy + half);
    let r = min(u.params.x, min(half.x, half.y));
    let d = rounded(p, half, r);
    let cover = clamp(0.5 - d, 0.0, 1.0);
    // Outward normal, for the light on the rim.
    let g = vec2<f32>(dpdx(d), dpdy(d));
    let n = g / max(length(g), 1e-4);
    let mode = u.params.w;
    let o = u.params.z;

    let b = textureSampleLevel(backdrop, smp, pos.xy / max(u.frame.xy, vec2<f32>(1.0)), 0.0);
    var body = u.tint;
    if (mode > 0.5) {
        body = over(u.tint, b);
    }

    // Sheen: light pooled at the top, gone by the middle.
    let t = clamp((p.y + half.y) / max(u.rect.w, 1.0), 0.0, 1.0);
    let sheen = u.frame.w * (1.0 - t) * (1.0 - t);
    body = over(vec4<f32>(sheen), body);

    // Grain, inside the body only.
    body = vec4<f32>(max(body.rgb + vec3<f32>(grain(pos.xy) * u.params.y * body.a), vec3<f32>(0.0)), body.a);

    // Rim: a thin inner ring, brightest where it faces the light (top left),
    // with a weaker counter-glint on the far edge: refraction, not a border.
    let w = u.shadow_geo.w;
    let ring = clamp((d + w + 0.5) / 1.0, 0.0, 1.0) * cover;
    let lit = dot(n, normalize(vec2<f32>(-0.45, -0.9)));
    let k = mix(0.28, 1.0, max(lit, 0.0)) + 0.5 * max(-lit, 0.0);
    let ra = u.rim.a * ring * k;
    body = over(vec4<f32>(u.rim.rgb * ra, ra), body);

    if (mode > 0.5 && mode < 1.5) {
        // Fill: the interior only; the blend constant carries the opacity.
        if (d > -0.5) {
            discard;
        }
        return encode(body);
    }

    // Shadow outside the body.
    let s = rounded(p - u.shadow_geo.xy, half, r);
    let blur = max(u.shadow_geo.z, 1.0);
    let fall = 1.0 - smoothstep(-blur * 0.35, blur, s);
    let shadow = u.shadow * (fall * fall) * (1.0 - cover);

    var edge = body * cover;
    if (mode > 1.5 && d <= -0.5) {
        // Edge pass of a filled panel: the fill already covered this.
        edge = vec4<f32>(0.0);
    }
    return encode(over(edge, shadow) * o);
}
