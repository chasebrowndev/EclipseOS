// SPDX-License-Identifier: AGPL-3.0-only
// Image placements (T-15 §6, T-07 §4 passes 2 and 4): one textured quad per placement.
// Textures hold premultiplied RGBA8; `tint` multiplies the sample (white texture + tint is
// the solid backdrop drawn under below-text images).

struct Uniforms {
    target_size: vec2<f32>,
    _pad: vec2<f32>,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct QuadIn {
    // x, y, w, h in target pixels
    @location(0) dst: vec4<f32>,
    // u0, v0, u1, v1
    @location(1) uv: vec4<f32>,
    // premultiplied
    @location(2) tint: vec4<f32>,
}

struct QuadOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) tint: vec4<f32>,
}

@vertex
fn vs_image(@builtin(vertex_index) vi: u32, q: QuadIn) -> QuadOut {
    let c = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    let px = q.dst.xy + c * q.dst.zw;
    var o: QuadOut;
    o.pos = vec4<f32>(px.x / u.target_size.x * 2.0 - 1.0, 1.0 - px.y / u.target_size.y * 2.0, 0.0, 1.0);
    o.uv = mix(q.uv.xy, q.uv.zw, c);
    o.tint = q.tint;
    return o;
}

@fragment
fn fs_image(i: QuadOut) -> @location(0) vec4<f32> {
    return textureSample(tex, samp, i.uv) * i.tint;
}
