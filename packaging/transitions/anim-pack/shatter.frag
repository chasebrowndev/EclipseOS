// SPDX-License-Identifier: AGPL-3.0-only
// Glass shatter. The window is cut into shards, two triangles per ~90 px cell
// of a lattice that is bent by a smooth warp, so the cracks run irregular
// without storing any vertices. Each pixel is pulled back along the mean burst
// to find where its shard started; that cell's two shards are then inverted
// exactly (translate, spin, shrink) and owned by an analytic point-in-triangle
// test. Frame 1 already shows the cracks (shard edges, bright for ~80 ms, then
// faint). From the impact point the shards burst outward, nearest first, with
// gravity, spin and a slight shrink, a thin bright edge, then fade. Displacement
// stays under ~2 cells, inside the margin. Shaped from `progress` (linear curve).
vec2 h2(vec2 p) {
    vec3 q = fract(vec3(p.xyx) * vec3(0.1031, 0.1030, 0.0973));
    q += dot(q, q.yzx + 33.33);
    return fract((q.xx + q.yz) * q.zy);
}

// Smooth bend of the lattice, about 0.1 cell.
vec2 warp(vec2 x, float cell) {
    vec2 g = x / cell;
    return 0.09 * cell * sin(vec2(g.y * 2.9 + g.x * 1.3 + 1.0, g.x * 2.3 - g.y * 1.7 + 4.0));
}

// Signed distance from the edge a->b, positive on the left of it.
float edist(vec2 a, vec2 b, vec2 p) {
    vec2 e = b - a;
    return (e.x * (p.y - a.y) - e.y * (p.x - a.x)) * inversesqrt(dot(e, e));
}

// Cheap distance (octagon), good enough for timing and direction.
float dist(vec2 v) {
    v = abs(v);
    return max(v.x, v.y) + 0.4 * min(v.x, v.y);
}

// Burst progress of a shard `dl` from the impact.
float burst(float p, float dl, float reach) {
    return clamp((p - 0.1 - 0.2 * min(dl / reach, 1.0)) / 0.62, 0.0, 1.0);
}

void main() {
    float p = clamp(progress, 0.0, 1.0);
    vec2 qs = size / content.zw;                 // quad px
    vec2 L = (v_coords - content.xy) * qs;       // window px, top-left origin
    float cell = 90.0;
    vec2 I = size * (0.5 + (vec2(seed, fract(seed * 7.31)) - 0.5) * 0.3);   // impact
    float reach = length(size) * 0.5;
    // Pull the pixel back along the mean burst to where its shard started.
    vec2 H = L;
    for (int n = 0; n < 2; n++) {
        vec2 dv = H - I;
        float dl = dist(dv);
        float tt = burst(p, dl, reach);
        H = L - dv / (dl + 0.6 * cell) * cell * 0.9 * tt * (2.0 - tt) - vec2(0.0, 0.6 * cell * tt * tt);
    }
    // No shard started far outside the window.
    if (H.x < -cell || H.y < -cell || H.x > size.x + cell || H.y > size.y + cell) {
        gl_FragColor = vec4(0.0);
        return;
    }
    vec2 c = floor((H + warp(H, cell)) / cell);   // the lattice cell it started in
    vec2 cnG = (c + 0.5) * cell;
    vec2 cn = cnG - warp(cnG, cell);
    vec2 dv = cn - I;
    float dl = dist(dv);
    vec2 dn = dv / (dl + 0.6 * cell);
    float tt = burst(p, dl, reach);
    float mv = tt * (2.0 - tt);
    vec2 h0 = h2(c * 1.7 + 3.3);
    vec2 h1 = fract(h0.yx * 7.31 + 0.37);
    bool flip = h0.x > 0.5;

    float bestd = -1000.0;
    vec2 bestl = vec2(0.0);
    vec3 bestp = vec3(0.0);
    for (int k = 0; k < 2; k++) {
        bool k0 = k == 0;
        vec2 hs = k0 ? h0 : h1;
        // Triangle corners in cell units.
        vec2 A = flip ? (k0 ? vec2(0.0, 0.0) : vec2(1.0, 0.0)) : vec2(0.0, 0.0);
        vec2 B = k0 ? vec2(1.0, 0.0) : vec2(1.0, 1.0);
        vec2 C = flip ? vec2(0.0, 1.0) : (k0 ? vec2(1.0, 1.0) : vec2(0.0, 1.0));
        vec2 cenG = (c + (A + B + C) / 3.0) * cell;
        vec2 cen = cenG - warp(cenG, cell);
        vec2 off = (dn + (hs - 0.5) * 0.3) * cell * (0.7 + 0.4 * hs.x) * mv
                 + vec2(0.0, 0.6 * cell * tt * tt);
        float ang = (hs.y - 0.5) * 2.4 * tt;
        float sc = 1.0 - 0.22 * tt;
        vec2 rel = L - cen - off;
        float ca = cos(ang);
        float sa = sin(ang);
        vec2 loc = cen + vec2(ca * rel.x + sa * rel.y, -sa * rel.x + ca * rel.y) / sc;
        vec2 u = (loc + warp(loc, cell)) / cell - c;   // back in cell units
        float dm = min(edist(A, B, u), min(edist(B, C, u), edist(C, A, u))) * cell * sc;
        if (dm > bestd) {
            bestd = dm;
            bestl = loc;
            bestp = vec3(tt, hs.y, ang);
        }
    }
    if (bestd < -1.5) { gl_FragColor = vec4(0.0); return; }

    vec4 col = texture2D(tex, content.xy + bestl / size * content.zw);
    // Cracks: bright for ~80 ms, then faint; the edge stays lit in flight.
    float edge = 1.0 - smoothstep(0.2, 1.3, bestd);
    float cr = mix(0.12, 0.6, 1.0 - smoothstep(0.1, 0.17, p));
    cr = mix(cr, 0.4, smoothstep(0.0, 0.15, bestp.x));
    col.rgb += vec3(1.0, 0.92, 0.72) * edge * cr * 0.7 * col.a;
    col.rgb *= 1.0 + (bestp.y - 0.5) * 0.14 * bestp.x;
    float cov = smoothstep(-1.5, 0.0, bestd);
    float fade = 1.0 - smoothstep(0.55 + 0.2 * bestp.y, 0.98, p);
    gl_FragColor = col * cov * fade * alpha;
}
