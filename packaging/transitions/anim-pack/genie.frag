// SPDX-License-Identifier: AGPL-3.0-only
// Classic genie. Everything is worked out in a frame where the target lies
// below the window (flipped when it is above), centred on the window, in px.
// Width and centre follow an S-curve across the window's rows, from full width
// at the far edge to chip size at the near edge.
// Phase 1 pinches that funnel; phase 2 slides the window's rows down it, the
// near edge leading, until they are a chip at the target. Each destination pixel
// maps back to a source row and column. Shaped from `progress` (linear curve).
void main() {
    float p = clamp(progress, 0.0, 1.0);
    p = direction > 0.0 ? 1.0 - p : p;
    float te = mix(p, p * p * (3.0 - 2.0 * p), 0.7);   // moves from frame 1
    float pinch = smoothstep(0.0, 1.0, clamp(te / 0.55, 0.0, 1.0));
    pinch = mix(clamp(te / 0.55, 0.0, 1.0), pinch, 0.6);
    float m = clamp((te - 0.12) / 0.88, 0.0, 1.0);
    float mn = smoothstep(0.0, 1.0, min(m * 1.5, 1.0));   // near edge leads
    float mf = m * m * (3.0 - 2.0 * m);                    // far edge trails

    vec2 cs = size;   // window px (`size` is the window, not the quad)
    vec2 qs = size / content.zw;   // quad px
    vec2 c = (content.xy + content.zw * 0.5) * qs;
    float u = clamp(cs.y / 1000.0, 0.6, 1.6);
    float W = cs.x;
    float H = cs.y;
    float wc = 44.0 * u;   // chip size at the target
    float hc = 30.0 * u;

    float d = travel.y < 0.0 ? -1.0 : 1.0;
    vec2 P = v_coords * qs - c;
    P.y *= d;
    float Tx = travel.x;
    float Ty = max(abs(travel.y), H * 0.5 + 40.0 * u);

    float y0 = -0.5 * H;
    float farY = mix(y0, Ty - 0.5 * hc, mf);
    float nearY = mix(0.5 * H, Ty + 0.5 * hc, mn);
    float span = nearY - farY;
    if (P.y < farY - 2.0 || P.y > nearY + 2.0) { gl_FragColor = vec4(0.0); return; }

    // Funnel across the window's own rows: v runs far edge -> near edge. The
    // near edge pinches first (phase 1); the far edge follows as it slides.
    float v = (P.y - farY) / max(span, 1.0);
    float S = v * v * (3.0 - 2.0 * v);
    float k = 1.0 - wc / W;
    float fnear = 1.0 - k * max(pinch, mn);
    float ffar = 1.0 - k * mf;
    float hw = 0.5 * W * mix(ffar, fnear, S);
    float cx = Tx * mix(mf, mix(0.5 * pinch, 1.0, mn), S);
    float uu = (P.x - cx) / (2.0 * hw) + 0.5;

    // Edges, about 1.5 px.
    float ex = min(P.x - cx + hw, cx + hw - P.x);
    float ey = min(P.y - farY, nearY - P.y);
    float cov = smoothstep(-0.75, 0.75, ex) * smoothstep(-0.75, 0.75, ey);
    if (cov <= 0.0) { gl_FragColor = vec4(0.0); return; }

    // Source position; 4 taps, spread by how hard the window is squeezed.
    vec2 sp = vec2(clamp(uu, 0.0, 1.0), clamp(v, 0.0, 1.0));
    float sy = d > 0.0 ? sp.y : 1.0 - sp.y;
    vec2 q = content.xy + vec2(sp.x, sy) * content.zw;
    vec2 sq = 0.35 * vec2(W / (2.0 * hw), H / max(span, 1.0)) / qs;
    sq = min(sq, vec2(0.02));
    vec4 col = texture2D(tex, q + vec2(sq.x, sq.y)) + texture2D(tex, q + vec2(-sq.x, sq.y))
             + texture2D(tex, q + vec2(sq.x, -sq.y)) + texture2D(tex, q + vec2(-sq.x, -sq.y));
    col *= 0.25;
    gl_FragColor = col * cov * (1.0 - smoothstep(0.9, 1.0, p)) * alpha;
}
