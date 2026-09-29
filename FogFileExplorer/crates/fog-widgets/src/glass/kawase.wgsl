// SPDX-License-Identifier: AGPL-3.0-only
//
// Dual-Kawase down and up samples, the compositor's blur
// (AbyssCompositor crates/abyss/src/render/blur.rs) ported to WGSL: same
// taps, same `halfpixel * offset` spacing, so a Fog panel blurs like a
// window does. Premultiplied alpha is kept, not forced to 1: a translucent
// window blurs into translucent glass.

struct Kawase {
    halfpixel: vec2<f32>,
    offset: f32,
    _pad: f32,
};

@group(0) @binding(0) var<uniform> k: Kawase;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var smp: sampler;

struct Out {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) i: u32) -> Out {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: Out;
    out.pos = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    return out;
}

fn tap(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(src, smp, uv, 0.0);
}

@fragment
fn down(in: Out) -> @location(0) vec4<f32> {
    let o = k.halfpixel * k.offset;
    var sum = tap(in.uv) * 4.0;
    sum += tap(in.uv - o);
    sum += tap(in.uv + o);
    sum += tap(in.uv + vec2<f32>(o.x, -o.y));
    sum += tap(in.uv - vec2<f32>(o.x, -o.y));
    return sum / 8.0;
}

@fragment
fn up(in: Out) -> @location(0) vec4<f32> {
    let o = k.halfpixel * k.offset;
    var sum = tap(in.uv + vec2<f32>(-o.x * 2.0, 0.0));
    sum += tap(in.uv + vec2<f32>(-o.x, o.y)) * 2.0;
    sum += tap(in.uv + vec2<f32>(0.0, o.y * 2.0));
    sum += tap(in.uv + vec2<f32>(o.x, o.y)) * 2.0;
    sum += tap(in.uv + vec2<f32>(o.x * 2.0, 0.0));
    sum += tap(in.uv + vec2<f32>(o.x, -o.y)) * 2.0;
    sum += tap(in.uv + vec2<f32>(0.0, -o.y * 2.0));
    sum += tap(in.uv + vec2<f32>(-o.x, -o.y)) * 2.0;
    return sum / 12.0;
}
