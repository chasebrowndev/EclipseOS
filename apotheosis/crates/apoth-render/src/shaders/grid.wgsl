// SPDX-License-Identifier: AGPL-3.0-only
// Grid, glyph, rect and composite passes (T-07 §4 passes 3 and 5).
//
// Everything is premultiplied RGBA8 in non-sRGB formats; text blending is done here in the
// glyph fragment shader and mirrors `colour::blend_coverage` (the CPU reference).
//
// Pixel coordinates are integers: a glyph quad starts at cell * cell_size + offset, so grid
// text always sits on whole pixels at any cell metrics.

struct Uniforms {
    target_size: vec2<f32>,
    cell: vec2<u32>,
    origin: vec2<i32>,
    // x = row-ring offset, y = ring rows (0: no ring, rows are used as given).
    ring: vec2<u32>,
    // columns, rows
    grid: vec2<u32>,
    // 0 linear-corrected, 1 linear, 2 native
    mode: u32,
    // bit 0: glyphs blend against `cursor_dst` instead of the cell background
    flags: u32,
    cursor_dst: vec4<f32>,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var bg_tex: texture_2d<f32>;
@group(0) @binding(2) var mask_tex: texture_2d_array<f32>;
@group(0) @binding(3) var color_tex: texture_2d_array<f32>;
@group(0) @binding(4) var grid_tex: texture_2d<f32>;

fn to_ndc(px: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(
        px.x / u.target_size.x * 2.0 - 1.0,
        1.0 - px.y / u.target_size.y * 2.0,
        0.0,
        1.0,
    );
}

// Triangle-strip corner of a unit quad: (0,0) (1,0) (0,1) (1,1).
fn corner(vi: u32) -> vec2<f32> {
    return vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
}

fn ring_row(row: u32) -> u32 {
    if (u.ring.y > 0u) {
        return (row + u.ring.x) % u.ring.y;
    }
    return row;
}

// ---- grid-sized quad: backgrounds (grid pass) and composite --------------------------------

@vertex
fn vs_quad(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let size = vec2<f32>(f32(u.grid.x * u.cell.x), f32(u.grid.y * u.cell.y));
    return to_ndc(vec2<f32>(u.origin) + corner(vi) * size);
}

// One texel per cell; the background texture is a row ring like the grid texture, so the
// physical row of a band is the texture row.
@fragment
fn fs_bg(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let px = vec2<i32>(p.xy);
    let col = min(px.x / i32(u.cell.x), i32(u.grid.x) - 1);
    let row = min(px.y / i32(u.cell.y), i32(u.grid.y) - 1);
    return textureLoad(bg_tex, vec2<i32>(col, row), 0);
}

@fragment
fn fs_composite(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let g = vec2<i32>(p.xy) - u.origin;
    let ch = i32(u.cell.y);
    let band = g.y / ch;
    let within = g.y - band * ch;
    let prow = ring_row(u32(band));
    return textureLoad(grid_tex, vec2<i32>(g.x, i32(prow) * ch + within), 0);
}

// ---- rects: decorations and cursor shapes --------------------------------------------------

struct RectIn {
    // x, y, w, h in pixels
    @location(0) r: vec4<i32>,
    @location(1) color: vec4<f32>,
}

struct RectOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
}

@vertex
fn vs_rect(@builtin(vertex_index) vi: u32, i: RectIn) -> RectOut {
    var o: RectOut;
    let p = vec2<f32>(i.r.xy + u.origin) + corner(vi) * vec2<f32>(i.r.zw);
    o.pos = to_ndc(p);
    o.color = i.color;
    return o;
}

@fragment
fn fs_rect(i: RectOut) -> @location(0) vec4<f32> {
    return i.color;
}

// ---- glyphs ---------------------------------------------------------------------------------

struct GlyphIn {
    @location(0) cell: vec2<u32>,
    @location(1) offset: vec2<i32>,
    @location(2) atlas_xy: vec2<u32>,
    @location(3) size: vec2<u32>,
    // x = page, y = flags (bit 0: colour glyph)
    @location(4) page_flags: vec2<u32>,
    @location(5) color: vec4<f32>,
}

struct GlyphOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) @interpolate(flat) dest: vec2<i32>,
    @location(1) @interpolate(flat) atlas: vec4<u32>,
    @location(2) @interpolate(flat) color: vec4<f32>,
}

@vertex
fn vs_glyph(@builtin(vertex_index) vi: u32, i: GlyphIn) -> GlyphOut {
    let row = ring_row(i.cell.y);
    let dest = vec2<i32>(i32(i.cell.x * u.cell.x), i32(row * u.cell.y)) + i.offset + u.origin;
    let p = vec2<f32>(dest) + corner(vi) * vec2<f32>(i.size);
    var o: GlyphOut;
    o.pos = to_ndc(p);
    o.dest = dest;
    o.atlas = vec4<u32>(i.atlas_xy, i.page_flags);
    o.color = i.color;
    return o;
}

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    return select(
        pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4)),
        c / 12.92,
        c <= vec3<f32>(0.04045),
    );
}

fn to_encoded(l: vec3<f32>) -> vec3<f32> {
    let x = clamp(l, vec3<f32>(0.0), vec3<f32>(1.0));
    return select(
        1.055 * pow(x, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055),
        x * 12.92,
        x <= vec3<f32>(0.0031308),
    );
}

fn luma(l: vec3<f32>) -> f32 {
    return dot(l, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// Mirrors colour::blend_coverage: CORRECTION_EPSILON 0.001, CORRECTION_GAIN 0.5.
fn blend_text(fg: vec3<f32>, bg: vec3<f32>, cov: f32) -> vec3<f32> {
    if (u.mode == 2u) {
        return fg * cov + bg * (1.0 - cov);
    }
    let lf = to_linear(fg);
    let lb = to_linear(bg);
    var a = cov;
    if (u.mode == 0u) {
        let d = luma(lb) - luma(lf);
        if (abs(d) >= 0.001) {
            a = clamp(a + a * (1.0 - a) * d * 0.5, 0.0, 1.0);
        }
    }
    return to_encoded(lf * a + lb * (1.0 - a));
}

@fragment
fn fs_glyph(i: GlyphOut) -> @location(0) vec4<f32> {
    let px = vec2<i32>(i.pos.xy);
    let a = vec2<i32>(i.atlas.xy) + (px - i.dest);
    let page = i32(i.atlas.z);
    if ((i.atlas.w & 1u) != 0u) {
        return textureLoad(color_tex, a, page, 0);
    }
    let cov = textureLoad(mask_tex, a, page, 0).r;
    if (cov <= 0.0) {
        discard;
    }
    var dst: vec4<f32>;
    if ((u.flags & 1u) != 0u) {
        dst = u.cursor_dst;
    } else {
        let col = min(px.x / i32(u.cell.x), i32(u.grid.x) - 1);
        let row = min(px.y / i32(u.cell.y), i32(u.grid.y) - 1);
        dst = textureLoad(bg_tex, vec2<i32>(col, row), 0);
    }
    if (dst.a > 0.999) {
        // Opaque destination: the exact blend, written as an opaque pixel.
        return vec4<f32>(blend_text(i.color.rgb, dst.rgb, cov), 1.0);
    }
    // Translucent destination: plain premultiplied source-over (the blend state does it).
    return vec4<f32>(i.color.rgb * cov, cov);
}
