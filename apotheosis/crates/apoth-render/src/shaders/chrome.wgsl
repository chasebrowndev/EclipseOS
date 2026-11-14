// SPDX-License-Identifier: AGPL-3.0-only
// Glass chrome pass (T-08 §3, T-07 §4 pass 7): rounded rects with a crisp rim and an
// analytic shadow, all from one signed-distance function. No blur, glow or gradient.
//
// Output is premultiplied RGBA; the pipeline blends `One, OneMinusSrcAlpha`, so a translucent
// fill leaves the window's alpha below 1 and the compositor's ext_background_effect blur
// shows through. `chrome::reference` mirrors these formulas on the CPU.

struct Uniforms {
    target_size: vec2<f32>,
    pad: vec2<f32>,
}

@group(0) @binding(0) var<uniform> u: Uniforms;

struct InstanceIn {
    // x, y, w, h in target pixels
    @location(0) rect: vec4<f32>,
    // corner radii: top-left, top-right, bottom-right, bottom-left
    @location(1) radii: vec4<f32>,
    // clip x0, y0, x1, y1
    @location(2) clip: vec4<f32>,
    // x = rim width, y = shadow spread, z = kind (0 rect, 1 shadow, 2 flat)
    @location(3) params: vec4<f32>,
    // premultiplied
    @location(4) fill: vec4<f32>,
    @location(5) rim: vec4<f32>,
}

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) @interpolate(flat) rect: vec4<f32>,
    @location(1) @interpolate(flat) radii: vec4<f32>,
    @location(2) @interpolate(flat) clip: vec4<f32>,
    @location(3) @interpolate(flat) params: vec4<f32>,
    @location(4) @interpolate(flat) fill: vec4<f32>,
    @location(5) @interpolate(flat) rim: vec4<f32>,
}

fn corner(vi: u32) -> vec2<f32> {
    return vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
}

@vertex
fn vs_chrome(@builtin(vertex_index) vi: u32, i: InstanceIn) -> VsOut {
    // kind: 0 rect (1 px antialias margin), 1 shadow (spread margin), 2 flat (none)
    var margin = 1.0;
    if (i.params.z > 1.5) {
        margin = 0.0;
    } else if (i.params.z > 0.5) {
        margin = i.params.y + 1.0;
    }
    let lo = i.rect.xy - vec2<f32>(margin);
    let size = i.rect.zw + vec2<f32>(2.0 * margin);
    let p = lo + corner(vi) * size;
    var o: VsOut;
    o.pos = vec4<f32>(
        p.x / u.target_size.x * 2.0 - 1.0,
        1.0 - p.y / u.target_size.y * 2.0,
        0.0,
        1.0,
    );
    o.rect = i.rect;
    o.radii = i.radii;
    o.clip = i.clip;
    o.params = i.params;
    o.fill = i.fill;
    o.rim = i.rim;
    return o;
}

// Signed distance (pixels, negative inside) to a rounded rect with per-corner radii.
fn sd_round(p: vec2<f32>, half: vec2<f32>, r: vec4<f32>) -> f32 {
    let right = p.x > 0.0;
    let bottom = p.y > 0.0;
    var rad: f32;
    if (right) {
        rad = select(r.y, r.z, bottom);
    } else {
        rad = select(r.x, r.w, bottom);
    }
    let q = abs(p) - half + vec2<f32>(rad);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2<f32>(0.0))) - rad;
}

@fragment
fn fs_chrome(i: VsOut) -> @location(0) vec4<f32> {
    let f = i.pos.xy;
    if (f.x < i.clip.x || f.y < i.clip.y || f.x >= i.clip.z || f.y >= i.clip.w) {
        return vec4<f32>(0.0);
    }
    if (i.params.z > 1.5) {
        // Flat: the fill inside the rect (glyph coverage rows from the text bridge).
        let inside = f.x >= i.rect.x && f.y >= i.rect.y
            && f.x < i.rect.x + i.rect.z && f.y < i.rect.y + i.rect.w;
        return select(vec4<f32>(0.0), i.fill, inside);
    }
    let half = i.rect.zw * 0.5;
    let d = sd_round(f - (i.rect.xy + half), half, i.radii);
    if (i.params.z > 0.5) {
        // Shadow: only outside the rect, falling off over `spread` pixels.
        let fall = 1.0 - smoothstep(0.0, i.params.y, d);
        let outside = clamp(d + 0.5, 0.0, 1.0);
        return i.fill * (fall * outside);
    }
    let cov = clamp(0.5 - d, 0.0, 1.0);
    var t = clamp(d + i.params.x + 0.5, 0.0, 1.0);
    if (i.params.x <= 0.0) {
        t = 0.0;
    }
    let rim_over = i.rim + i.fill * (1.0 - i.rim.a);
    return mix(i.fill, rim_over, t) * cov;
}
