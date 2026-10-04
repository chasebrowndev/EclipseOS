// SPDX-License-Identifier: AGPL-3.0-only
// Window coordinates: 0..1 across the window, beyond it in the margin.
void main() {
    float t = clamp(1.0 - shown(), 0.0, 1.0);
    vec2 w = (v_coords - content.xy) / content.zw;
    vec2 lo = -content.xy / content.zw;
    vec2 hi = (vec2(1.0) - content.xy) / content.zw;
    float down = travel.y < 0.0 ? -1.0 : 1.0;
    // Flip so the funnel always points toward +y.
    if (down < 0.0) {
        w.y = 1.0 - w.y;
        float l = lo.y;
        lo.y = 1.0 - hi.y;
        hi.y = 1.0 - l;
    }
    vec2 target = clamp(vec2(0.5 + travel.x / size.x, 0.5 + abs(travel.y) / size.y),
                        lo + vec2(0.02), hi - vec2(0.02));
    float slide = smoothstep(0.3, 1.0, t);
    float top = mix(0.0, target.y, slide);
    float bottom = mix(1.0, target.y, slide);
    float bend = smoothstep(0.0, 0.6, t) * smoothstep(0.0, 1.0, clamp(w.y / max(target.y, 0.001), 0.0, 1.0));
    float left = mix(0.0, target.x - 0.02, bend);
    float right = mix(1.0, target.x + 0.02, bend);
    vec2 src = vec2((w.x - left) / max(right - left, 0.0001), (w.y - top) / max(bottom - top, 0.0001));
    if (src.x < 0.0 || src.x > 1.0 || src.y < 0.0 || src.y > 1.0) {
        gl_FragColor = vec4(0.0);
        return;
    }
    if (down < 0.0) {
        src.y = 1.0 - src.y;
    }
    vec4 c = texture2D(tex, content.xy + src * content.zw);
    gl_FragColor = c * (1.0 - smoothstep(0.85, 1.0, t)) * alpha;
}
