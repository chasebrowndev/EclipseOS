// SPDX-License-Identifier: AGPL-3.0-only
// Burning paper. A domain-warped fbm front climbs from the bottom edge; ahead
// of it the window chars, along it runs white-hot -> orange -> deep red, behind
// it the window is gone. Sparks are born at the front and rise into the top
// margin. Open is the same burn played backwards. Shaped from `progress`
// (curve is linear in the kdl) so the front moves on the very first frame.
float hash(vec2 p) {
    p = fract(p * vec2(123.34, 456.21) + seed * 17.0);
    p += dot(p, p + 45.32);
    return fract(p.x * p.y);
}

float noise(vec2 p) {
    vec2 i = floor(p);
    vec2 f = fract(p);
    f = f * f * (3.0 - 2.0 * f);
    return mix(mix(hash(i), hash(i + vec2(1.0, 0.0)), f.x),
               mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), f.x), f.y);
}

// Sparks live in a frame that rides the front: yrel is px ahead of it. A
// spark of burn-time age a sits at yrel = a * rise speed, so a pixel's age
// picks its birth-time bin, and the bin plus the column pick the particle.
vec4 sparks(float x, float yrel, float t, float u) {
    vec4 acc = vec4(0.0);
    for (int i = 0; i < 2; i++) {
        float fi = float(i);
        float life = 0.30 + 0.16 * fi;
        float vr = (190.0 - 70.0 * fi) * u / life;
        float cell = (30.0 - 8.0 * fi) * u;
        float dt = 0.011;
        float a = yrel / vr;
        float tb = t - a;
        if (a <= 0.0 || a >= life || tb < 0.0) continue;
        float kb = floor(tb / dt);
        float fr = tb / dt - kb;
        float cx = floor(x / cell);
        vec2 h = vec2(hash(vec2(cx, kb) + fi * 37.7), hash(vec2(kb, cx) + 11.3 + fi * 5.1));
        if (h.x > 0.13 + 0.04 * fi) continue;
        float q = a / life;
        float xc = (cx + 0.5) * cell + (h.y - 0.5) * 0.35 * cell + sin(a * 22.0 + h.y * 40.0) * 5.0 * u * q;
        float r = max((3.2 + 2.4 * h.y) * (1.0 - 0.4 * fi) * u, 1.5);
        float d = abs(x - xc) / r;
        float body = (1.0 - smoothstep(0.35, 1.0, d)) * pow(1.0 - fr, 2.0);
        float flick = 0.65 + 0.35 * sin(t * 90.0 + h.y * 60.0);
        float al = min(body * pow(1.0 - q, 1.1) * flick * 1.5, 1.0);
        vec3 col = mix(vec3(1.0, 0.92, 0.62), vec3(1.0, 0.42, 0.08), smoothstep(0.0, 0.35, q));
        col = mix(col, vec3(0.65, 0.1, 0.03), smoothstep(0.5, 1.0, q));
        acc.rgb = acc.rgb * (1.0 - al) + col * al;
        acc.a = acc.a + al * (1.0 - acc.a);
    }
    return acc;
}

void main() {
    // Burn amount 0..1; the run's shaping lives here.
    float p = clamp(progress, 0.0, 1.0);
    float t = direction > 0.0 ? 1.0 - (1.0 - (1.0 - p) * (1.0 - p)) : mix(p, p * p * (3.0 - 2.0 * p), 0.4);
    vec2 cs = size;   // window px (`size` is the window, not the quad)
    float u = cs.y / 1000.0;   // sizes below are for a 1000 px tall window
    vec2 w = (v_coords - content.xy) / content.zw;
    vec2 px = w * cs;
    float gp = (1.0 - w.y) * cs.y;

    float bw = 28.0 * u;   // glow band
    float ch = 120.0 * u;   // char reach ahead of the front
    float amp = 220.0 * u;
    float tilt = (hash(vec2(3.1, 7.7)) - 0.5) * 0.30 * cs.y;
    float spread = 0.3 * amp + 0.5 * abs(tilt);   // how far f strays from gp
    float lo = -spread - bw * 0.9 - ch * 0.12;
    float hi = cs.y + spread + 10.0 * u;
    float F = mix(lo, hi, t);
    float bound = 0.5 * amp + 0.5 * abs(tilt) + 12.0 * u;
    // Far behind the front nothing is left; far ahead only the window remains.
    if (F - gp > bound) { gl_FragColor = vec4(0.0); return; }
    if (gp - F > bound + ch + 220.0 * u) { gl_FragColor = texture2D(tex, v_coords) * alpha; return; }

    // Warped fbm decides where the paper catches.
    vec2 q = px / (230.0 * u);
    q += (noise(q * 1.6 + 3.1) - 0.5) * vec2(1.6, 1.0);
    float n = noise(q) * 0.50 + noise(q * 2.3 + 5.0) * 0.30 + noise(q * 5.1 + 2.0) * 0.20;
    float f = gp + (n - 0.5) * amp + (w.x - 0.5) * tilt;

    float rough = (noise(px / (9.0 * u) + 1.7) - 0.5) * 9.0 * u;
    float e = F - f + rough;   // > 0: burnt away
    float act = smoothstep(0.0, 0.012, t);

    vec4 c = texture2D(tex, v_coords);
    float keep = 1.0 - smoothstep(-0.75, 0.75, e);
    float charr = pow(smoothstep(-ch, 0.0, e), 1.6) * act;
    vec4 base = c * keep;
    base.rgb *= mix(vec3(1.0), vec3(0.34, 0.20, 0.14), charr * 0.85);

    // Heat: peaks at the edge, spills a few px past it.
    float h = e < 0.0 ? 1.0 + e / bw : 1.0 - e / (7.0 * u);
    h = clamp(h, 0.0, 1.0);
    h *= 0.88 + 0.24 * sin(px.x / (17.0 * u) + n * 11.0 + t * 45.0);
    h = clamp(pow(h, 1.6), 0.0, 1.0) * act;
    vec3 hc = mix(vec3(0.38, 0.03, 0.01), vec3(1.0, 0.40, 0.06), smoothstep(0.0, 0.55, h));
    hc = mix(hc, vec3(1.0, 0.95, 0.80), smoothstep(0.72, 1.0, h));
    float ab = smoothstep(0.02, 0.30, h) * 0.96 * c.a;
    vec4 o = vec4(base.rgb * (1.0 - ab) + hc * ab, base.a * (1.0 - ab) + ab);

    // Sparks over everything, fading toward the quad's top edge.
    float yrel = f - F;
    vec4 sp = sparks(px.x, yrel, t, u);
    float topfade = smoothstep(0.0, 40.0 * u, v_coords.y * size.y / content.w);
    sp *= topfade * smoothstep(-6.0 * u, 14.0 * u, gp) * smoothstep(-20.0 * u, 10.0 * u, px.x) * smoothstep(cs.x + 20.0 * u, cs.x - 10.0 * u, px.x) * (1.0 - smoothstep(0.85, 1.0, t));
    o.rgb = o.rgb * (1.0 - sp.a) + sp.rgb;
    o.a = o.a + sp.a * (1.0 - o.a);
    gl_FragColor = o * alpha;
}
