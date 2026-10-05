// SPDX-License-Identifier: AGPL-3.0-only
// Sci-fi teleport. The window resolves out of 3 px grains in noise-threshold
// order (with a slight top-down lean) over a ghost of itself that is there from
// the first frame. A grain brightens as it lands and is settled ~40 ms later.
// A few soft gold-white sparks glint where grains land, only inside the window.
// A soft 9-tap glow of the silhouette clears by mid-run, with a faint scan line
// and a shimmer that dies away. Close is the same run backwards.
float hash(vec2 p) {
    p = fract(p * vec2(123.34, 456.21) + seed * 17.0);
    p += dot(p, p + 45.32);
    return fract(p.x * p.y);
}

void main() {
    float s = clamp(shown(), 0.0, 1.0);
    vec2 cs = size;   // window px (`size` is the window, not the quad)
    vec2 w = (v_coords - content.xy) / content.zw;
    vec2 px = v_coords * size / content.zw;
    float u = max(cs.y / 1000.0, 0.5);

    // Shimmer: rows shift sideways a little, less and less.
    float k = (1.0 - s) * (1.0 - s);
    vec2 uv = v_coords + vec2(sin(px.y * 0.55 + s * 24.0) * 1.6 * k * content.z / size.x, 0.0);
    vec4 c = texture2D(tex, uv);

    // Each grain lands when s passes its threshold; d is how long ago, in s.
    vec2 g = floor(px / 3.0);
    float a = hash(g) * 0.72 + clamp(w.y, 0.0, 1.0) * 0.2;
    float d = s - a;
    float land = max(smoothstep(0.0, 0.06, d), 0.4 * sqrt(s));   // ghost fills from frame 1
    float settle = smoothstep(-0.01, 0.0, d) * (1.0 - smoothstep(0.0, 0.12, d));
    vec3 gold = vec3(1.0, 0.84, 0.52);
    vec4 o = c * land;
    o.rgb *= 1.0 + 0.5 * settle;   // a grain glows as it lands

    // Sparks: a quarter of the old density, soft dots on a 7 px grid.
    vec2 sg = floor(px / 7.0);
    vec2 sf = fract(px / 7.0) - 0.5;
    float sa = hash(sg + 31.7) * 0.72 + clamp(w.y, 0.0, 1.0) * 0.2;
    float sd = s - sa;
    float spark = smoothstep(-0.01, 0.0, sd) * (1.0 - smoothstep(0.0, 0.12, sd));
    spark *= step(0.78, hash(sg + 71.3));
    float dot2 = 1.0 - smoothstep(0.0, 0.5, length(sf));
    o.rgb += gold * (0.75 * spark * dot2 * dot2) * c.a;

    // Soft glow: 9 taps of the silhouette, strongest early, gone by mid-run.
    float r = 7.0 * u;
    vec2 e = vec2(r) / size * content.zw;
    float b = c.a * 0.25;
    b += texture2D(tex, v_coords + vec2(e.x, 0.0)).a * 0.125;
    b += texture2D(tex, v_coords - vec2(e.x, 0.0)).a * 0.125;
    b += texture2D(tex, v_coords + vec2(0.0, e.y)).a * 0.125;
    b += texture2D(tex, v_coords - vec2(0.0, e.y)).a * 0.125;
    b += texture2D(tex, v_coords + e).a * 0.0625;
    b += texture2D(tex, v_coords - e).a * 0.0625;
    b += texture2D(tex, v_coords + vec2(e.x, -e.y)).a * 0.0625;
    b += texture2D(tex, v_coords + vec2(-e.x, e.y)).a * 0.0625;
    float veil = smoothstep(0.0, 0.1, s) * (1.0 - smoothstep(0.2, 0.55, s));
    float ga = veil * (0.05 * b + 0.10 * 4.0 * b * (1.0 - b)) * (1.0 - o.a);
    o.rgb += gold * 0.8 * ga;
    o.a += ga;

    // Faint scan line sweeping down; gone before the end.
    float ys = s * 1.1 - 0.05;
    float line = exp(-pow((w.y - ys) * cs.y / (3.5 * u), 2.0)) * c.a;
    line *= (1.0 - smoothstep(0.7, 0.95, s)) * smoothstep(0.0, 0.08, s) * 0.28;
    o.rgb += gold * line;
    o.a = min(o.a + line * 0.5, 1.0);

    gl_FragColor = o * alpha;
}
