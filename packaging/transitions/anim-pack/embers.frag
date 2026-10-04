// SPDX-License-Identifier: AGPL-3.0-only
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

void main() {
    float s = clamp(shown(), 0.0, 1.0);
    vec2 px = v_coords * size / content.zw;
    float n = noise(px / 28.0) * 0.7 + noise(px / 7.0) * 0.3;
    float front = (1.0 - s) * 1.1;
    float keep = smoothstep(front - 0.05, front, n);
    float rim = (smoothstep(front - 0.14, front - 0.04, n) - keep) * clamp(s * 6.0, 0.0, 1.0);
    vec4 c = texture2D(tex, v_coords);
    vec3 gold = vec3(1.0, 0.72, 0.32);
    gl_FragColor = (c * keep + vec4(gold, 1.0) * rim * c.a) * alpha;
}
