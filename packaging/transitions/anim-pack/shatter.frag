// SPDX-License-Identifier: AGPL-3.0-only
float hash(vec2 p) {
    p = fract(p * vec2(234.34, 435.35) + seed * 13.0);
    p += dot(p, p + 34.23);
    return fract(p.x * p.y);
}

void main() {
    float t = clamp(1.0 - shown(), 0.0, 1.0);
    vec2 quad = size / content.zw;
    vec2 px = v_coords * quad;
    // The whole pane drops into the bottom margin as it breaks.
    px.y -= t * t * content.y * quad.y;
    float cell = 48.0;
    vec2 id = floor(px / cell);
    vec2 centre = (id + 0.5) * cell;
    float h = hash(id);
    float lt = clamp((t - h * 0.4) / 0.6, 0.0, 1.0);
    float a = (h - 0.5) * lt * 3.0;
    vec2 q = px - centre;
    q = mat2(cos(a), -sin(a), sin(a), cos(a)) * q / max(1.0 - lt, 0.001);
    if (abs(q.x) > cell * 0.5 || abs(q.y) > cell * 0.5) {
        gl_FragColor = vec4(0.0);
        return;
    }
    gl_FragColor = texture2D(tex, (centre + q) / quad) * (1.0 - lt) * alpha;
}
